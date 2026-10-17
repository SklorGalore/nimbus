//! Replace in files, for files that are not open in a tab.
//!
//! Open tabs get their replacements as one undoable edit in the app; this
//! module handles the files on disk. A worker re-reads each file, checks
//! that every ticked range still holds the matched text (the file may have
//! changed since the search), splices in the replacements from the end of
//! the file back so earlier offsets stay valid, and writes the result
//! through the same temp-file-and-rename path as saving a document.
//!
//! The old contents of each written file are kept in memory for "Undo
//! Replace in Files", up to [`UNDO_BUDGET`] in all; past it the largest
//! files are written without a way back, and the summary names them. Undo
//! checks each file's size and modification time first, as the file
//! watcher does, so a file edited since is left alone.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use nimbus_core::MappedBuffer;
use regex::bytes::Regex;

use crate::io;

/// How much old content "Undo Replace in Files" keeps, in bytes
/// (DESIGN.md §9).
pub const UNDO_BUDGET: usize = 64 * 1024 * 1024;

/// Text looked at around a match when checking it still matches, so `\b`,
/// `^` and `$` see what they saw during the search.
const CONTEXT: usize = 256;

/// What identifies a version of a file without reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    fn from(meta: &std::fs::Metadata) -> Self {
        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
        }
    }

    fn of(path: &Path) -> Option<Self> {
        std::fs::metadata(path).ok().map(|m| Self::from(&m))
    }
}

/// The pattern and replacement a Replace All applies.
#[derive(Debug, Clone)]
pub struct Replacement {
    /// The compiled pattern the hits came from.
    pub re: Regex,
    /// `$1`-style references in the template expand (regex mode).
    pub expand: bool,
    /// The replacement text, or template.
    pub template: String,
}

impl Replacement {
    /// The text that replaces `range` in `hay`, or `None` when `hay` no
    /// longer matches there. The pattern runs again on the line's text up
    /// to the match, with what precedes it, so assertions see the same
    /// context as in the search.
    #[must_use]
    pub fn at(&self, hay: &[u8], range: Range<usize>) -> Option<Vec<u8>> {
        if range.end > hay.len() || range.start > range.end {
            return None;
        }
        // The line's end: an anchor must not see the next line.
        let line_end =
            memchr::memchr(b'\n', &hay[range.start..]).map_or(hay.len(), |i| range.start + i);
        let caps = self.re.captures_at(&hay[..line_end], range.start)?;
        if caps.get(0).map(|m| m.range()) != Some(range) {
            return None;
        }
        let mut out = Vec::new();
        if self.expand {
            caps.expand(self.template.as_bytes(), &mut out);
        } else {
            out.extend_from_slice(self.template.as_bytes());
        }
        Some(out)
    }

    /// [`Replacement::at`] for a range of a text that is not all in
    /// memory: `read` gives the bytes of any range, and only some context
    /// around the match is read.
    pub fn at_in(
        &self,
        len: usize,
        range: Range<usize>,
        read: impl FnOnce(Range<usize>) -> Option<Vec<u8>>,
    ) -> Option<Vec<u8>> {
        let start = range.start.saturating_sub(CONTEXT);
        let end = range.end.saturating_add(CONTEXT).min(len);
        let window = read(start..end)?;
        self.at(&window, range.start - start..range.end - start)
    }
}

/// `bytes` with `edits` (sorted, non-overlapping ranges in `bytes`)
/// replaced.
#[must_use]
pub fn apply(bytes: &[u8], edits: &[(Range<usize>, Vec<u8>)]) -> Vec<u8> {
    let grown: usize = edits.iter().map(|(_, t)| t.len()).sum();
    let mut out = Vec::with_capacity(bytes.len() + grown);
    let mut pos = 0;
    for (range, text) in edits {
        out.extend_from_slice(&bytes[pos..range.start]);
        out.extend_from_slice(text);
        pos = range.end;
    }
    out.extend_from_slice(&bytes[pos..]);
    out
}

/// One file on disk to change, with the ranges to replace (in file
/// order, as the search found them).
#[derive(Debug, Clone)]
pub struct Job {
    /// The file.
    pub path: PathBuf,
    /// Ticked hits, as byte ranges.
    pub ranges: Vec<Range<usize>>,
}

/// How one file's replace went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Written, with this many replacements.
    Replaced(usize),
    /// Every ticked range had changed since the search: left alone.
    Changed,
    /// Could not be read or written.
    Failed(String),
}

/// A written file's old content, for undo.
#[derive(Debug, Clone)]
pub struct Saved {
    /// The file.
    pub path: PathBuf,
    /// Its bytes before the replace.
    pub old: Vec<u8>,
    /// The file as written, to tell a change since.
    pub stamp: Stamp,
}

/// What a Replace All did to the files on disk.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Each file and how it went, in job order.
    pub files: Vec<(PathBuf, Outcome)>,
    /// Old contents kept for undo.
    pub saved: Vec<Saved>,
    /// Written files whose old content did not fit the undo budget.
    pub unkept: Vec<PathBuf>,
}

impl Report {
    /// Replacements made, files written.
    #[must_use]
    pub fn totals(&self) -> (usize, usize) {
        self.files
            .iter()
            .filter_map(|(_, o)| match o {
                Outcome::Replaced(n) => Some(*n),
                _ => None,
            })
            .fold((0, 0), |(r, f), n| (r + n, f + 1))
    }
}

/// Replace in one file: read it, check each range, write the result.
fn replace_one(rep: &Replacement, job: &Job) -> (Outcome, Option<Vec<u8>>, Option<Stamp>) {
    let bytes = match std::fs::read(&job.path) {
        Ok(b) => b,
        Err(e) => return (Outcome::Failed(format!("cannot read: {e}")), None, None),
    };
    let edits: Vec<(Range<usize>, Vec<u8>)> = job
        .ranges
        .iter()
        .filter_map(|r| rep.at(&bytes, r.clone()).map(|t| (r.clone(), t)))
        .collect();
    if edits.is_empty() {
        return (Outcome::Changed, None, None);
    }
    let new = MappedBuffer::from_bytes(apply(&bytes, &edits));
    match io::save(&job.path, &new, false) {
        Ok(meta) => (
            Outcome::Replaced(edits.len()),
            Some(bytes),
            Some(Stamp::from(&meta)),
        ),
        Err(e) => (Outcome::Failed(e.to_string()), None, None),
    }
}

/// Run every job, one file at a time, keeping old contents within the
/// budget.
#[must_use]
pub fn run(rep: &Replacement, jobs: &[Job]) -> Report {
    let mut report = Report::default();
    let mut kept = 0usize;
    for job in jobs {
        let (outcome, old, stamp) = replace_one(rep, job);
        if let (Some(old), Some(stamp)) = (old, stamp) {
            if kept + old.len() <= UNDO_BUDGET {
                kept += old.len();
                report.saved.push(Saved {
                    path: job.path.clone(),
                    old,
                    stamp,
                });
            } else {
                report.unkept.push(job.path.clone());
            }
        }
        report.files.push((job.path.clone(), outcome));
    }
    report
}

/// Run the jobs on a worker thread and hand the report to `send`.
///
/// # Errors
///
/// Returns the error if the thread could not start; nothing is written
/// then.
pub fn spawn(
    rep: Replacement,
    jobs: Vec<Job>,
    send: impl FnOnce(Report) + Send + 'static,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("nimbus-replace".to_owned())
        .spawn(move || send(run(&rep, &jobs)))
        .map(drop)
}

/// Write the old contents back, for files unchanged since the replace.
/// Returns the files restored and, for the rest, why not.
#[must_use]
pub fn undo(saved: &[Saved]) -> (Vec<PathBuf>, Vec<(PathBuf, String)>) {
    let mut restored = Vec::new();
    let mut failed = Vec::new();
    for s in saved {
        if Stamp::of(&s.path) != Some(s.stamp) {
            failed.push((s.path.clone(), "changed since".to_owned()));
            continue;
        }
        let old = MappedBuffer::from_bytes(s.old.clone());
        match io::save(&s.path, &old, false) {
            Ok(_) => restored.push(s.path.clone()),
            Err(e) => failed.push((s.path.clone(), e.to_string())),
        }
    }
    (restored, failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nimbus_core::search::Query;

    fn rep(pattern: &str, template: &str, regex: bool) -> Replacement {
        let re = Query {
            pattern: pattern.into(),
            regex,
            case_sensitive: true,
            whole_word: false,
        }
        .compile()
        .unwrap()
        .unwrap();
        Replacement {
            re,
            expand: regex,
            template: template.into(),
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nimbus-replace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn replacements_check_the_text_and_expand_captures() {
        let r = rep(r"(\w+)=(\w+)", "$2=$1", true);
        assert_eq!(
            r.at(b"key=value\nx", 0..9).as_deref(),
            Some(&b"value=key"[..])
        );
        assert_eq!(r.at(b"key-value\nx", 0..9), None, "changed since");
        assert_eq!(r.at(b"key=value", 0..20), None, "past the end");
        let lit = rep("a", "$1", false);
        assert_eq!(lit.at(b"a", 0..1).as_deref(), Some(&b"$1"[..]));
        // Anchors see the line, not the next one.
        let end = rep(r"a$", "b", true);
        assert_eq!(end.at(b"a\nz", 0..1).as_deref(), Some(&b"b"[..]));
        let word = rep(r"\bfoo", "bar", true);
        let text = b"xfoo foo";
        assert_eq!(word.at(text, 1..4), None, "no boundary before it");
        assert!(word.at(text, 5..8).is_some());
        let got = word.at_in(text.len(), 5..8, |r| Some(text[r].to_vec()));
        assert_eq!(got.as_deref(), Some(&b"bar"[..]));
    }

    #[test]
    fn apply_splices_edits_in_order() {
        let out = apply(
            b"a foo b foo c",
            &[(2..5, b"x".to_vec()), (8..11, b"yz".to_vec())],
        );
        assert_eq!(out, b"a x b yz c");
    }

    #[test]
    fn runs_jobs_and_undoes_unchanged_files() {
        let a = temp("a.txt");
        let b = temp("b.txt");
        std::fs::write(&a, "foo and foo\n").unwrap();
        std::fs::write(&b, "moved foo\n").unwrap();
        let jobs = vec![
            Job {
                path: a.clone(),
                ranges: vec![0..3, 8..11],
            },
            Job {
                path: b.clone(),
                ranges: std::iter::once(0..3).collect(), // changed since the search
            },
            Job {
                path: temp("missing.txt"),
                ranges: std::iter::once(0..3).collect(),
            },
        ];
        let report = run(&rep("foo", "bar", false), &jobs);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "bar and bar\n");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "moved foo\n");
        assert_eq!(report.files[0].1, Outcome::Replaced(2));
        assert_eq!(report.files[1].1, Outcome::Changed);
        assert!(matches!(report.files[2].1, Outcome::Failed(_)));
        assert_eq!(report.totals(), (2, 1));
        assert_eq!(report.saved.len(), 1);
        let (restored, failed) = undo(&report.saved);
        assert_eq!(restored, vec![a.clone()]);
        assert!(failed.is_empty());
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "foo and foo\n");
        // Changed after the replace: left alone.
        let report = run(&rep("foo", "qux", false), &jobs[..1]);
        std::fs::write(&a, "edited meanwhile\n").unwrap();
        let (restored, failed) = undo(&report.saved);
        assert!(restored.is_empty());
        assert_eq!(failed[0].1, "changed since");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "edited meanwhile\n");
    }
}
