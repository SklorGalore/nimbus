//! Patches for staging, unstaging and discarding part of a file (M28).
//!
//! git stages a whole file with `git add`. To stage only some of a file's
//! changes, a client writes a *patch* holding just those changes and asks
//! git to apply it to the index (`git apply --cached`); to discard some
//! changes, it applies the opposite patch to the working tree. This module
//! writes those patches; [`super::Repo::apply`] hands them to git.
//!
//! **What a unified diff says.** A patch is a list of *hunks*. Each starts
//! with a header, `@@ -12,3 +12,4 @@`: in the old text, 3 lines from line
//! 12 are covered; in the new text, 4 lines from line 12. The lines that
//! follow start with a space (*context*: the line stays), `-` (removed) or
//! `+` (added). git finds the place by the line numbers and checks that the
//! context and removed lines are really there, so a patch made against
//! other text is refused instead of applied in the wrong place. Our hunks
//! carry no context beyond the change itself (git's `--unidiff-zero`):
//! the line numbers alone place them, and the text they would check is
//! exactly the text the window showed.
//!
//! **Picking lines.** Between the two texts there are changes: lines only
//! the old text has (removed) and lines only the new one has (added). The
//! user picks some of them. A patch applied *forward* to the old text (to
//! stage onto the index) does the picked changes and leaves the rest: an
//! unpicked removed line becomes context (it stays), an unpicked added line
//! is left out. A patch applied to the new text *in reverse* (to unstage
//! from the index, or to discard from the working tree) undoes the picked
//! changes: a picked added line is removed, a picked removed line comes
//! back, and an unpicked added line is context. Other clients build their
//! line staging the same way.
//!
//! **Line endings.** A line keeps its ending, `\n` or `\r\n`, in the patch,
//! so a CRLF file stays CRLF. A text that does not end with a newline has
//! a last line without one, which a patch marks with `\ No newline at end
//! of file`; when picked lines would follow such a line, it is replaced by
//! the same line with a newline, as git's own diffs do.

use std::fmt::Write as _;
use std::ops::Range;
use std::sync::Arc;

use nimbus_core::{Buffer as _, Hunk, Interner, RopeBuffer, Snapshot};

/// The lines picked, by line number from 0: removed lines of the old text
/// and added lines of the new text. Lines that are not part of a change
/// are ignored, so a pick may cover equal lines too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pick {
    /// Lines of the old text.
    pub old: Vec<Range<usize>>,
    /// Lines of the new text.
    pub new: Vec<Range<usize>>,
}

impl Pick {
    /// Every line of both texts: the whole change.
    #[must_use]
    pub fn all() -> Self {
        Self {
            old: vec![0..usize::MAX],
            new: vec![0..usize::MAX],
        }
    }

    fn has(ranges: &[Range<usize>], line: usize) -> bool {
        ranges.iter().any(|r| r.contains(&line))
    }
}

/// Which text the patch is applied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Apply {
    /// To the old text, doing the picked changes: staging.
    Forward,
    /// To the new text, undoing the picked changes: unstaging and
    /// discarding.
    Reverse,
}

/// Why no patch was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchError {
    /// None of the picked lines is part of a change.
    Nothing,
    /// The path cannot go in a patch (empty, or with a newline in it).
    BadPath,
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nothing => f.write_str("no changed lines are selected"),
            Self::BadPath => f.write_str("this file's name cannot go in a patch"),
        }
    }
}

impl std::error::Error for PatchError {}

/// A text split into lines, each with its ending; a text that ends with a
/// newline has no empty line after it (the convention of
/// [`nimbus_core::diff`]).
#[must_use]
pub fn lines(text: &[u8]) -> Vec<&[u8]> {
    text.split_inclusive(|&b| b == b'\n').collect()
}

/// The changes between two texts, line by line and exact (no options), as
/// the compare view finds them. Bytes that are not UTF-8 are compared as
/// their replacement character, which keeps every line where it is.
#[must_use]
pub fn hunks(old: &[u8], new: &[u8]) -> Vec<Hunk> {
    let snap = |b: &[u8]| -> Arc<dyn Snapshot> {
        RopeBuffer::from_text(&String::from_utf8_lossy(b))
            .snapshot()
            .into()
    };
    let mut interner = Interner::new();
    match (interner.intern(&snap(old)), interner.intern(&snap(new))) {
        (Ok(a), Ok(b)) => interner.diff(&a, &b),
        // Over four billion distinct lines: treat it all as one change.
        _ => vec![Hunk {
            old: 0..lines(old).len(),
            new: 0..lines(new).len(),
        }],
    }
}

/// One line of a hunk being written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Context,
    Remove,
    Add,
}

/// A patch for `path` that makes the picked changes between `old` and
/// `new` (see the module notes for what `apply` changes). `create`: the
/// file does not exist on the side the patch applies to, so the patch
/// creates it with this mode (`100644`, or `100755` for an executable).
///
/// # Errors
///
/// [`PatchError::Nothing`] when no picked line is part of a change, and
/// [`PatchError::BadPath`] for a path a patch cannot hold.
pub fn build(
    path: &str,
    (old, new): (&[u8], &[u8]),
    pick: &Pick,
    apply: Apply,
    create: Option<&str>,
) -> Result<Vec<u8>, PatchError> {
    if path.is_empty() || path.contains(['\n', '\r', '\0']) {
        return Err(PatchError::BadPath);
    }
    let (a, b) = (lines(old), lines(new));
    let mut body = Vec::new();
    // How many lines the patched text has gained so far, for the headers'
    // new-side line numbers.
    let mut delta: isize = 0;
    let mut ops: Vec<(Op, &[u8])> = Vec::new();
    for h in hunks(old, new) {
        ops.clear();
        let removed = |l: usize| Pick::has(&pick.old, l);
        let added = |l: usize| Pick::has(&pick.new, l);
        let picked = h.old.clone().any(removed) || h.new.clone().any(added);
        if !picked {
            continue;
        }
        // The text the patch applies to, over this hunk: its lines, then
        // the lines the other side brings in.
        let (target, start) = match apply {
            Apply::Forward => {
                for l in h.old.clone() {
                    let op = if removed(l) { Op::Remove } else { Op::Context };
                    ops.push((op, a[l]));
                }
                for l in h.new.clone().filter(|&l| added(l)) {
                    ops.push((Op::Add, b[l]));
                }
                (h.old.clone(), h.old.start)
            }
            Apply::Reverse => {
                for l in h.new.clone() {
                    let op = if added(l) { Op::Remove } else { Op::Context };
                    ops.push((op, b[l]));
                }
                for l in h.old.clone().filter(|&l| removed(l)) {
                    ops.push((Op::Add, a[l]));
                }
                (h.new.clone(), h.new.start)
            }
        };
        write_hunk(&mut body, &ops, (start, target.len()), &mut delta);
    }
    if body.is_empty() {
        return Err(PatchError::Nothing);
    }
    let mut out = String::new();
    let _ = writeln!(
        out,
        "diff --git {} {}",
        prefixed("a/", path),
        prefixed("b/", path)
    );
    match create {
        Some(mode) => {
            let _ = writeln!(out, "new file mode {mode}");
            let _ = writeln!(out, "--- /dev/null");
        }
        None => {
            let _ = writeln!(out, "--- {}", prefixed("a/", path));
        }
    }
    let _ = writeln!(out, "+++ {}", prefixed("b/", path));
    let mut patch = out.into_bytes();
    patch.extend_from_slice(&body);
    Ok(patch)
}

/// Write one hunk: `ops` cover `count` lines of the target from line
/// `start` (from 0). A line without a newline that something follows on
/// the patched side gets one, as a removal and an addition.
fn write_hunk(
    out: &mut Vec<u8>,
    ops: &[(Op, &[u8])],
    (start, count): (usize, usize),
    delta: &mut isize,
) {
    // The patched side's lines, in order: context and additions.
    let kept: Vec<usize> = (0..ops.len()).filter(|&i| ops[i].0 != Op::Remove).collect();
    let mut fixed: Vec<(Op, std::borrow::Cow<'_, [u8]>)> = Vec::with_capacity(ops.len() + 1);
    for (i, &(op, line)) in ops.iter().enumerate() {
        let followed = kept.last().is_some_and(|&last| last > i);
        if op != Op::Remove && followed && !line.ends_with(b"\n") {
            let mut with = line.to_vec();
            with.push(b'\n');
            if op == Op::Context {
                fixed.push((Op::Remove, line.into()));
            }
            fixed.push((Op::Add, with.into()));
        } else {
            fixed.push((op, line.into()));
        }
    }
    let new_count = fixed.iter().filter(|(op, _)| *op != Op::Remove).count();
    // With no context, an empty side's number is the line *after which*
    // the change goes; otherwise it is the first line, counting from 1.
    let number = |start: usize, n: usize| if n == 0 { start } else { start + 1 };
    let new_start = start.saturating_add_signed(*delta);
    let header = format!(
        "@@ -{},{count} +{},{new_count} @@\n",
        number(start, count),
        number(new_start, new_count)
    );
    out.extend_from_slice(header.as_bytes());
    for (op, line) in &fixed {
        out.push(match op {
            Op::Context => b' ',
            Op::Remove => b'-',
            Op::Add => b'+',
        });
        out.extend_from_slice(line);
        if !line.ends_with(b"\n") {
            out.extend_from_slice(b"\n\\ No newline at end of file\n");
        }
    }
    let grew = isize::try_from(new_count).unwrap_or(0) - isize::try_from(count).unwrap_or(0);
    *delta += grew;
}

/// `prefix` and `path` for a `---` or `+++` line, in git's quoting when the
/// path needs it.
fn prefixed(prefix: &str, path: &str) -> String {
    quote(&format!("{prefix}{path}"))
}

/// A path as git writes it in a patch: as it is, or in double quotes with
/// C escapes when it holds a quote, a backslash, a control character or a
/// byte outside ASCII (git's `core.quotePath`, which `git apply` reads).
fn quote(path: &str) -> String {
    let plain = path
        .bytes()
        .all(|b| b.is_ascii() && !b.is_ascii_control() && b != b'"' && b != b'\\');
    if plain {
        return path.to_owned();
    }
    let mut out = String::from("\"");
    for b in path.bytes() {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\t' => out.push_str("\\t"),
            b if b.is_ascii() && !b.is_ascii_control() => out.push(char::from(b)),
            b => {
                let _ = write!(out, "\\{b:03o}");
            }
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(patch: &[u8]) -> String {
        String::from_utf8_lossy(patch).into_owned()
    }

    /// Apply a patch's hunks to `target` in memory, the way git does with
    /// `--unidiff-zero`, checking every context and removed line.
    pub(crate) fn apply_in_memory(target: &[u8], patch: &[u8]) -> Vec<u8> {
        let lines_of = lines(target);
        let mut out: Vec<u8> = Vec::new();
        let mut at = 0;
        let body = text(patch);
        let mut rows = body.split_inclusive('\n').peekable();
        while let Some(row) = rows.next() {
            let Some(head) = row.strip_prefix("@@ -") else {
                continue;
            };
            let (old, _) = head.split_once(' ').unwrap();
            let (s, n) = old.split_once(',').unwrap();
            let (s, n): (usize, usize) = (s.parse().unwrap(), n.parse().unwrap());
            let first = if n == 0 { s } else { s - 1 };
            for l in &lines_of[at..first] {
                out.extend_from_slice(l);
            }
            at = first;
            while let Some(next) = rows.peek() {
                if next.starts_with("@@") {
                    break;
                }
                let row = rows.next().unwrap();
                let mut line = row[1..].as_bytes().to_vec();
                if rows.peek().is_some_and(|r| r.starts_with('\\')) {
                    rows.next();
                    line.pop();
                }
                match &row[..1] {
                    " " => {
                        assert_eq!(lines_of[at], &line[..], "context");
                        out.extend_from_slice(&line);
                        at += 1;
                    }
                    "-" => {
                        assert_eq!(lines_of[at], &line[..], "removed");
                        at += 1;
                    }
                    "+" => out.extend_from_slice(&line),
                    _ => panic!("bad row {row:?}"),
                }
            }
        }
        for l in &lines_of[at..] {
            out.extend_from_slice(l);
        }
        out
    }

    #[test]
    fn staging_everything_gives_the_new_text() {
        let old = b"a\nb\nc\nd\n";
        let new = b"a\nB\nc\nd\ne\n";
        let p = build("f.txt", (old, new), &Pick::all(), Apply::Forward, None).unwrap();
        assert!(text(&p).starts_with("diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n"));
        assert_eq!(apply_in_memory(old, &p), new);
        let r = build("f.txt", (old, new), &Pick::all(), Apply::Reverse, None).unwrap();
        assert_eq!(apply_in_memory(new, &r), old);
    }

    #[test]
    fn unpicked_lines_stay_as_they_were() {
        let old = b"one\ntwo\nthree\n";
        let new = b"ONE\nTWO\nthree\nfour\n";
        // Only the second line's change and the added fourth line.
        let pick = Pick {
            old: vec![1..2],
            new: vec![1..2, 3..4],
        };
        let p = build("f", (old, new), &pick, Apply::Forward, None).unwrap();
        assert_eq!(apply_in_memory(old, &p), b"one\nTWO\nthree\nfour\n");
        let r = build("f", (old, new), &pick, Apply::Reverse, None).unwrap();
        assert_eq!(apply_in_memory(new, &r), b"ONE\ntwo\nthree\n");
    }

    #[test]
    fn nothing_picked_is_an_error() {
        let pick = Pick {
            old: vec![5..6],
            new: Vec::new(),
        };
        let e = build("f", (b"a\n", b"b\n"), &pick, Apply::Forward, None);
        assert_eq!(e, Err(PatchError::Nothing));
        assert_eq!(
            build("a\nb", (b"a\n", b"b\n"), &Pick::all(), Apply::Forward, None),
            Err(PatchError::BadPath)
        );
    }

    #[test]
    fn a_missing_final_newline_is_marked_and_mended() {
        let old = b"x";
        let new = b"x\ny\n";
        // Only the added y: x must gain its newline for y to follow it.
        let pick = Pick {
            old: Vec::new(),
            new: vec![1..2],
        };
        let p = build("f", (old, new), &pick, Apply::Forward, None).unwrap();
        assert!(
            text(&p).contains("\\ No newline at end of file"),
            "{}",
            text(&p)
        );
        assert_eq!(apply_in_memory(old, &p), b"x\ny\n");
        let all = build("f", (old, new), &Pick::all(), Apply::Forward, None).unwrap();
        assert_eq!(apply_in_memory(old, &all), new);
        let back = build("f", (old, new), &Pick::all(), Apply::Reverse, None).unwrap();
        assert_eq!(apply_in_memory(new, &back), old);
    }

    #[test]
    fn crlf_lines_keep_their_endings() {
        let old = b"a\r\nb\r\n";
        let new = b"a\r\nB\r\nc\r\n";
        let p = build("f", (old, new), &Pick::all(), Apply::Forward, None).unwrap();
        assert!(text(&p).contains("+B\r\n"));
        assert_eq!(apply_in_memory(old, &p), new);
    }

    #[test]
    fn a_new_file_patch_creates_it() {
        let p = build(
            "dir/n.txt",
            (b"", b"1\n2\n"),
            &Pick::all(),
            Apply::Forward,
            Some("100644"),
        )
        .unwrap();
        assert!(text(&p).starts_with(
            "diff --git a/dir/n.txt b/dir/n.txt\nnew file mode 100644\n--- /dev/null\n+++ b/dir/n.txt\n@@ -0,0 +1,2 @@\n"
        ));
    }

    #[test]
    fn odd_paths_are_quoted() {
        assert_eq!(quote("a b.txt"), "a b.txt");
        assert_eq!(quote("é.txt"), "\"\\303\\251.txt\"");
        assert_eq!(quote("q\"t"), "\"q\\\"t\"");
    }
}
