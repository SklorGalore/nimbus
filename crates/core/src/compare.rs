//! Comparing two texts of any size (M9 task 2a).
//!
//! [`diff`](crate::diff) gives an exact line diff, but it holds a token per
//! line and an entry per distinct line, so for a 4 GB log it would want
//! gigabytes. This module gets a correct diff at any size by working in
//! layers, each exact or labelled:
//!
//! 1. **Trim the common start and end.** The two texts are compared byte
//!    by byte from both ends, straight from their storage, with no memory
//!    beyond a position. Only the middle is diffed. This alone covers the
//!    usual huge-file cases exactly: a log that grew, a big file with a
//!    few edits, a file changed on disk.
//! 2. **Full diff of the middle** when it is within [`DIFF_LIMIT`] on both
//!    sides, exactly as for any file.
//! 3. **Windows** when it is not: both middles are walked in windows of a
//!    bounded size, each window pair diffed on its own, and the result
//!    kept up to the last change that does not touch the window's end,
//!    where the walk resumes. A window pair with no common line is
//!    widened a few times, then reported as one changed block. The hunks
//!    always turn the old text into the new one, but may not be the
//!    smallest set that does, so [`Comparison::exact`] is `false` and the
//!    diff bar can say so.
//!
//! With [`Options::split`] (M18), texts within the limit have their lines
//! over [`crate::split::SPLIT_AT`] cut into pieces before the diff, so a
//! minified file shows its real changes. The hunks are then in *units*
//! (lines and pieces) and [`Comparison::old_splits`] and
//! [`Comparison::new_splits`] map units back to lines. Past the limit
//! nothing is cut: finding the long lines would mean reading the whole of
//! a huge file once more.
//!
//! Everything here is pure and reads through [`Text::chunk_at`], so it
//! runs on a worker over snapshots (DESIGN.md §4.1).

use std::ops::Range;
use std::sync::Arc;

use crate::diff::{DIFF_LIMIT, DiffError, Hunk, Interner, Normalize};
use crate::split::Splits;
use crate::{Snapshot, Text};

/// How a comparison is done; the defaults are for real files, tests use
/// small values to exercise every layer on a few bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Most bytes per side, after trimming, for a full diff.
    pub limit: usize,
    /// Bytes per side per window past the limit. Memory for the windowed
    /// diff is proportional to this, times the widening below.
    pub window: usize,
    /// What counts as an equal line.
    pub normalize: Normalize,
    /// Cut long lines into pieces (texts within `limit` only).
    pub split: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            limit: DIFF_LIMIT,
            window: 32 << 20,
            normalize: Normalize::default(),
            split: false,
        }
    }
}

/// How many times a window is doubled when it finds no common line
/// before the pair is reported as one changed block.
const WIDENINGS: u32 = 3;

/// The result of comparing two texts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Comparison {
    /// The changes, in line ranges of each whole text, sorted and apart.
    pub hunks: Vec<Hunk>,
    /// Lines in the old text (by the diff's convention: no empty line
    /// after a final newline).
    pub old_lines: usize,
    /// Lines in the new text.
    pub new_lines: usize,
    /// The hunks are a smallest set (layers 1 and 2). `false` after the
    /// windowed layer, whose hunks are correct but may be coarser.
    pub exact: bool,
    /// How many lines, both sides together, went through the diff
    /// algorithm: what trimming saved, for the debug overlay and tests.
    pub lines_diffed: usize,
    /// The old text's cut lines. When either side has any, the hunks and
    /// the line counts above are in units, not lines.
    pub old_splits: Splits,
    /// The new text's cut lines.
    pub new_splits: Splits,
}

/// Compare `old` with `new`, in layers, within `options`.
///
/// # Errors
///
/// [`DiffError::TooManyLines`] when a side has more distinct lines than
/// the diff can number.
pub fn compare(
    old: &Arc<dyn Snapshot>,
    new: &Arc<dyn Snapshot>,
    options: Options,
) -> Result<Comparison, DiffError> {
    let (old_len, new_len) = (old.len_bytes(), new.len_bytes());
    let trim = trim(&**old, &**new);
    let old_mid = trim.prefix_bytes..old_len - trim.suffix_bytes;
    let new_mid = trim.prefix_bytes..new_len - trim.suffix_bytes;
    let suffix_lines_old = count_lines(&**old, old_mid.end..old_len);
    let suffix_lines_new = count_lines(&**new, new_mid.end..new_len);
    debug_assert_eq!(suffix_lines_old, suffix_lines_new);

    let mut result = Comparison {
        exact: true,
        ..Comparison::default()
    };
    let split = options.split && old_len <= options.limit && new_len <= options.limit;
    if split {
        result.old_splits = Splits::scan(&**old);
        result.new_splits = Splits::scan(&**new);
    }
    let split = split && !(result.old_splits.is_empty() && result.new_splits.is_empty());
    if split {
        // The common start is the same bytes on both sides, so the same
        // cuts: its unit count is the same on both.
        let prefix_units = result.old_splits.unit_of_line(trim.prefix_lines);
        let mut interner = Interner::with_options(options.normalize, true);
        let a = interner.intern_range(old, old_mid.clone())?;
        let b = interner.intern_range(new, new_mid.clone())?;
        result.lines_diffed = a.len() + b.len();
        result.hunks = interner.diff(&a, &b);
        shift(&mut result.hunks, prefix_units, prefix_units);
        result.old_lines = result
            .old_splits
            .units(trim.prefix_lines + count_lines(&**old, old_mid.start..old_len));
        result.new_lines = result
            .new_splits
            .units(trim.prefix_lines + count_lines(&**new, new_mid.start..new_len));
        return Ok(result);
    }
    let (mid_old_lines, mid_new_lines) = if old_mid.is_empty() && new_mid.is_empty() {
        (0, 0)
    } else if old_mid.len() <= options.limit && new_mid.len() <= options.limit {
        let mut interner = Interner::with_options(options.normalize, false);
        let a = interner.intern_range(old, old_mid.clone())?;
        let b = interner.intern_range(new, new_mid.clone())?;
        result.lines_diffed = a.len() + b.len();
        result.hunks = interner.diff(&a, &b);
        shift(&mut result.hunks, trim.prefix_lines, trim.prefix_lines);
        (a.len(), b.len())
    } else {
        result.exact = false;
        windowed(
            old,
            old_mid,
            new,
            new_mid,
            trim.prefix_lines,
            options,
            &mut result,
        )?
    };
    result.old_lines = trim.prefix_lines + mid_old_lines + suffix_lines_old;
    result.new_lines = trim.prefix_lines + mid_new_lines + suffix_lines_new;
    Ok(result)
}

/// The common start and end of two texts, in bytes and in whole lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Trim {
    /// Bytes of common start, cut back to the end of a line (after its
    /// `\n`), so the middle begins at a line start on both sides.
    pub prefix_bytes: usize,
    /// Lines in that common start.
    pub prefix_lines: usize,
    /// Bytes of common end, cut forward to a line start, so the middle
    /// ends after a `\n` (or at the end of the text) on both sides.
    pub suffix_bytes: usize,
}

/// Find the common start and end of `a` and `b` at line granularity. Reads
/// the common parts once each, from storage, holding nothing.
#[must_use]
pub fn trim(a: &dyn Text, b: &dyn Text) -> Trim {
    let (la, lb) = (a.len_bytes(), b.len_bytes());
    let max = la.min(lb);

    // Common start in bytes, then back to the last newline in it.
    let mut common = 0;
    let mut prefix_lines = 0;
    let mut prefix_bytes = 0;
    while common < max {
        let (ca, sa) = a.chunk_at(common);
        let (cb, sb) = b.chunk_at(common);
        let (Some(ca), Some(cb)) = (ca.get(common - sa..), cb.get(common - sb..)) else {
            break;
        };
        let k = ca.len().min(cb.len()).min(max - common);
        if k == 0 {
            break;
        }
        let same = mismatch(&ca[..k], &cb[..k]);
        // Newlines inside the equal run end common lines.
        for i in memchr::memchr_iter(b'\n', &ca[..same]) {
            prefix_lines += 1;
            prefix_bytes = common + i + 1;
        }
        common += same;
        if same < k {
            break;
        }
    }

    // Common end in bytes, not reaching into the common start, then
    // forward to the first line start in it.
    let max_suffix = max - prefix_bytes;
    let mut common = 0;
    while common < max_suffix {
        let (pa, pb) = (la - common, lb - common);
        let (ca, sa) = a.chunk_at(pa - 1);
        let (cb, sb) = b.chunk_at(pb - 1);
        let (Some(ca), Some(cb)) = (ca.get(..pa - sa), cb.get(..pb - sb)) else {
            break;
        };
        let k = ca.len().min(cb.len()).min(max_suffix - common);
        if k == 0 {
            break;
        }
        let (ta, tb) = (&ca[ca.len() - k..], &cb[cb.len() - k..]);
        let same = mismatch_rev(ta, tb);
        common += same;
        if same < k {
            break;
        }
    }
    // `common` bytes at the end agree. The suffix lines start after the
    // first newline *inside* that stretch: the line holding its first byte
    // may differ earlier on, and a line start just before it in `a` is no
    // line start in `b`, whose byte there is outside the common part.
    let suffix_bytes = if common == max_suffix && la == lb {
        // Identical after the common start, which ended at a line start in
        // both: the whole rest is common lines, newline or not.
        common
    } else {
        first_newline_within(a, la - common..la).map_or(0, |nl| la - (nl + 1))
    };
    Trim {
        prefix_bytes,
        prefix_lines,
        suffix_bytes,
    }
}

/// Length of the equal prefix of two equal-length slices.
fn mismatch(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// Length of the equal suffix of two equal-length slices.
fn mismatch_rev(a: &[u8], b: &[u8]) -> usize {
    a.iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count()
}

/// The first line start at or after `range.start` within `range`: the
/// byte after the first `\n` in it, or `range.start` itself when it is a
/// line start (the start of the text or just after a `\n`).
fn first_line_start_within(t: &dyn Text, range: Range<usize>) -> Option<usize> {
    if range.start == 0 || t.byte_at(range.start - 1) == Some(b'\n') {
        return Some(range.start);
    }
    first_newline_within(t, range).map(|nl| nl + 1)
}

/// Where the first `\n` in `range` is.
fn first_newline_within(t: &dyn Text, range: Range<usize>) -> Option<usize> {
    let mut pos = range.start;
    while pos < range.end {
        let (chunk, base) = t.chunk_at(pos);
        let bytes = chunk.get(pos - base..)?;
        let bytes = &bytes[..bytes.len().min(range.end - pos)];
        if bytes.is_empty() {
            return None;
        }
        if let Some(i) = memchr::memchr(b'\n', bytes) {
            return Some(pos + i);
        }
        pos += bytes.len();
    }
    None
}

/// Lines in `range` of `t` by the diff's convention: one per `\n`, plus
/// one for a final stretch without one. `range` starts at a line start.
fn count_lines(t: &dyn Text, range: Range<usize>) -> usize {
    let mut lines = 0;
    let mut last_newline_at_end = true;
    let mut pos = range.start;
    while pos < range.end {
        let (chunk, base) = t.chunk_at(pos);
        let Some(bytes) = chunk.get(pos - base..) else {
            break;
        };
        let bytes = &bytes[..bytes.len().min(range.end - pos)];
        if bytes.is_empty() {
            break;
        }
        lines += memchr::memchr_iter(b'\n', bytes).count();
        last_newline_at_end = bytes.last() == Some(&b'\n');
        pos += bytes.len();
    }
    if !range.is_empty() && !last_newline_at_end {
        lines += 1;
    }
    lines
}

fn shift(hunks: &mut [Hunk], old_by: usize, new_by: usize) {
    for h in hunks {
        h.old = h.old.start + old_by..h.old.end + old_by;
        h.new = h.new.start + new_by..h.new.end + new_by;
    }
}

/// The end of the window starting at `start` and at most `size` bytes
/// long, extended to a line end (after a `\n`) or `limit`.
fn window_end(t: &dyn Text, start: usize, size: usize, limit: usize) -> usize {
    let want = start.saturating_add(size).min(limit);
    if want >= limit {
        return limit;
    }
    // `want` may be mid-line: go on to the next line start.
    first_line_start_within(t, want..limit).unwrap_or(limit)
}

/// Layer 3: diff `old_mid` against `new_mid` window by window, appending
/// hunks (in whole-text line numbers) to `out`. Returns the line counts of
/// the two middles.
fn windowed(
    old: &Arc<dyn Snapshot>,
    old_mid: Range<usize>,
    new: &Arc<dyn Snapshot>,
    new_mid: Range<usize>,
    prefix_lines: usize,
    options: Options,
    out: &mut Comparison,
) -> Result<(usize, usize), DiffError> {
    let mut old_pos = old_mid.start;
    let mut new_pos = new_mid.start;
    let mut old_line = prefix_lines;
    let mut new_line = prefix_lines;
    let window = options.window.max(1);
    while old_pos < old_mid.end || new_pos < new_mid.end {
        let mut size = window;
        let mut widenings = 0;
        loop {
            let old_end = window_end(&**old, old_pos, size, old_mid.end);
            let new_end = window_end(&**new, new_pos, size, new_mid.end);
            let mut interner = Interner::with_options(options.normalize, false);
            let a = interner.intern_range(old, old_pos..old_end)?;
            let b = interner.intern_range(new, new_pos..new_end)?;
            out.lines_diffed += a.len() + b.len();
            let mut hunks = interner.diff(&a, &b);
            let old_done = old_end == old_mid.end;
            let new_done = new_end == new_mid.end;
            // A hunk touching a window's end may be an artefact of the cut:
            // the lines after the window might have matched it. Keep what
            // lies before it and resume there, unless both windows are the
            // last ones, where the diff is final.
            let suspect = hunks.last().is_some_and(|h| {
                (!old_done && h.old.end == a.len()) || (!new_done && h.new.end == b.len())
            });
            let (resume_old, resume_new) = match hunks.pop() {
                Some(last) if suspect => (last.old.start, last.new.start),
                Some(last) => {
                    hunks.push(last);
                    (a.len(), b.len())
                }
                None => (a.len(), b.len()),
            };
            let progressed = resume_old > 0 || resume_new > 0 || (old_done && new_done);
            if !progressed && widenings < WIDENINGS {
                size *= 2;
                widenings += 1;
                continue;
            }
            if progressed {
                shift(&mut hunks, old_line, new_line);
                out.hunks.extend(hunks);
                old_pos = line_offset(&**old, old_pos, resume_old);
                new_pos = line_offset(&**new, new_pos, resume_new);
                old_line += resume_old;
                new_line += resume_new;
            } else {
                // Nothing in common within the widened windows: one block.
                out.hunks.push(Hunk {
                    old: old_line..old_line + a.len(),
                    new: new_line..new_line + b.len(),
                });
                old_pos = old_end;
                new_pos = new_end;
                old_line += a.len();
                new_line += b.len();
            }
            break;
        }
    }
    merge_touching(&mut out.hunks);
    Ok((old_line - prefix_lines, new_line - prefix_lines))
}

/// The byte where line `lines` after `start` begins (`start` is a line
/// start).
fn line_offset(t: &dyn Text, start: usize, lines: usize) -> usize {
    let mut pos = start;
    let mut left = lines;
    let len = t.len_bytes();
    while left > 0 && pos < len {
        let (chunk, base) = t.chunk_at(pos);
        let Some(bytes) = chunk.get(pos - base..) else {
            break;
        };
        if bytes.is_empty() {
            break;
        }
        let mut advanced = false;
        for i in memchr::memchr_iter(b'\n', bytes) {
            left -= 1;
            if left == 0 {
                pos += i + 1;
                advanced = true;
                break;
            }
        }
        if !advanced {
            pos += bytes.len();
        }
    }
    pos
}

/// Windows can leave two hunks that touch (one ending where the next
/// begins on both sides); views expect them merged.
fn merge_touching(hunks: &mut Vec<Hunk>) {
    let mut merged: Vec<Hunk> = Vec::with_capacity(hunks.len());
    for h in hunks.drain(..) {
        if let Some(last) = merged.last_mut()
            && last.old.end == h.old.start
            && last.new.end == h.new.start
        {
            last.old.end = h.old.end;
            last.new.end = h.new.end;
        } else {
            merged.push(h);
        }
    }
    *hunks = merged;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Buffer, RopeBuffer};

    fn snap(text: &str) -> Arc<dyn Snapshot> {
        RopeBuffer::from_text(text).snapshot().into()
    }

    /// `NIMBUS_DIFF_FILES=OLD:NEW cargo test --release -p nimbus-core -- --ignored --nocapture compare_bench`
    ///
    /// Times the layered comparison of two files with the real options
    /// (M9 task 2a budgets: 10 MB pairs, 100 MB pairs, the 1 GB and 4 GB
    /// logs grown or edited).
    #[test]
    #[ignore = "benchmark: needs NIMBUS_DIFF_FILES and a release build"]
    fn compare_bench() {
        let Ok(spec) = std::env::var("NIMBUS_DIFF_FILES") else {
            return;
        };
        let (a, b) = spec.split_once(':').expect("OLD:NEW");
        let t = std::time::Instant::now();
        let old =
            RopeBuffer::from_reader(std::fs::File::open(a).expect("open OLD")).expect("read OLD");
        let new =
            RopeBuffer::from_reader(std::fs::File::open(b).expect("open NEW")).expect("read NEW");
        let loaded = t.elapsed();
        let (os, ns): (Arc<dyn Snapshot>, Arc<dyn Snapshot>) =
            (old.snapshot().into(), new.snapshot().into());
        let t = std::time::Instant::now();
        let c = compare(&os, &ns, Options::default()).expect("compare");
        println!(
            "{spec}: {} MB + {} MB loaded in {loaded:?}; compared in {:?}: {} hunks, {} + {} lines, {} lines diffed, exact {}",
            os.len_bytes() >> 20,
            ns.len_bytes() >> 20,
            t.elapsed(),
            c.hunks.len(),
            c.old_lines,
            c.new_lines,
            c.lines_diffed,
            c.exact
        );
    }

    #[test]
    fn trims_whole_lines_only() {
        let t = trim(&*snap("ab\ncd\nef\n"), &*snap("ab\ncX\nef\n"));
        assert_eq!(t.prefix_bytes, 3);
        assert_eq!(t.prefix_lines, 1);
        assert_eq!(t.suffix_bytes, 3);
        let t = trim(&*snap("abc"), &*snap("abd"));
        assert_eq!((t.prefix_bytes, t.prefix_lines, t.suffix_bytes), (0, 0, 0));
        let t = trim(&*snap("abc"), &*snap("abc"));
        assert_eq!((t.prefix_bytes, t.prefix_lines, t.suffix_bytes), (0, 0, 3));
        let t = trim(&*snap("a\nb\n"), &*snap("a\nb\n"));
        assert_eq!((t.prefix_bytes, t.prefix_lines, t.suffix_bytes), (4, 2, 0));
        // The common tail "a\n" starts mid-line in the second text: no
        // suffix lines, even though it starts a line in the first.
        let t = trim(&*snap("}\na\n"), &*snap("\ta\n"));
        assert_eq!((t.prefix_bytes, t.prefix_lines, t.suffix_bytes), (0, 0, 0));
    }

    #[test]
    fn trimmed_middle_is_all_that_is_diffed() {
        let old: String = (0..1000).fold(String::new(), |mut s, i| {
            use std::fmt::Write as _;
            let _ = writeln!(s, "line {i}");
            s
        });
        let new = old.replacen("line 500\n", "changed\n", 1);
        let c = compare(&snap(&old), &snap(&new), Options::default()).unwrap();
        assert_eq!(
            c.hunks,
            vec![Hunk {
                old: 500..501,
                new: 500..501
            }]
        );
        assert_eq!(c.old_lines, 1000);
        assert_eq!(c.new_lines, 1000);
        assert!(c.exact);
        assert_eq!(c.lines_diffed, 2);
    }

    #[test]
    fn windows_find_scattered_edits() {
        let old: String = (0..2000).fold(String::new(), |mut s, i| {
            use std::fmt::Write as _;
            let _ = writeln!(s, "line {i}");
            s
        });
        let new = old
            .replacen("line 100\n", "", 1)
            .replacen("line 900\n", "line 900\nextra\n", 1)
            .replacen("line 1500\n", "other\n", 1);
        let small = Options {
            limit: 64,
            window: 512,
            ..Options::default()
        };
        let c = compare(&snap(&old), &snap(&new), small).unwrap();
        assert!(!c.exact);
        assert_eq!(
            c.hunks,
            vec![
                Hunk {
                    old: 100..101,
                    new: 100..100
                },
                Hunk {
                    old: 901..901,
                    new: 900..901
                },
                Hunk {
                    old: 1500..1501,
                    new: 1500..1501
                },
            ]
        );
    }

    #[test]
    fn unrelated_windows_become_one_block() {
        let old: String = (0..300).fold(String::new(), |mut s, i| {
            use std::fmt::Write as _;
            let _ = writeln!(s, "a {i}");
            s
        });
        let new: String = (0..300).fold(String::new(), |mut s, i| {
            use std::fmt::Write as _;
            let _ = writeln!(s, "b {i}");
            s
        });
        let c = compare(
            &snap(&old),
            &snap(&new),
            Options {
                limit: 16,
                window: 64,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(
            c.hunks,
            vec![Hunk {
                old: 0..300,
                new: 0..300
            }]
        );
    }
}
