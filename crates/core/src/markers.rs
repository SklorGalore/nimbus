//! Conflict markers in a file, read back into three texts (M9 task 9).
//!
//! A merge left with conflicts (ours, git's, anyone's) is a text with
//! blocks like
//!
//! ```text
//! <<<<<<< ours
//! our lines
//! ||||||| base          (only in diff3 style)
//! the base's lines
//! =======
//! their lines
//! >>>>>>> theirs
//! ```
//!
//! Splitting such a file gives the three texts a three-way merge needs:
//! the common text goes into all three, each block's parts into their own.
//! Merging them again finds the same conflicts (both sides changed the
//! same stretch of the base, or, without a base part, both added there),
//! so the merge view can open on a conflicted file as if it had made the
//! merge itself. Nothing is lost: the blocks are kept as marker blocks in
//! the result until each is resolved.

use std::fmt;

/// The three texts split out of a file with conflict markers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Split {
    /// The common text with each block's base part (empty parts when the
    /// markers had no `|||||||` section).
    pub base: String,
    /// The common text with each block's first part.
    pub ours: String,
    /// The common text with each block's second part.
    pub theirs: String,
    /// The labels after the first `<<<<<<<` and `>>>>>>>`, as found.
    pub labels: (String, String),
    /// How many blocks there were.
    pub conflicts: usize,
}

/// Why a text's markers could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerError {
    /// A `<<<<<<<` block was still open at the end of the text, or a
    /// marker came out of order; the line (from 1) says where.
    Unterminated(usize),
}

impl fmt::Display for MarkerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unterminated(line) => write!(f, "conflict markers out of order at line {line}"),
        }
    }
}

impl std::error::Error for MarkerError {}

/// Whether `text` holds a conflict block start: a line beginning with
/// `<<<<<<< `. Looks at the whole text; callers wanting a quick check
/// pass its start.
#[must_use]
pub fn has_markers(text: &str) -> bool {
    text.lines()
        .any(|l| l.starts_with("<<<<<<< ") || l == "<<<<<<<")
}

/// Split `text` at its conflict markers. `Ok(None)` when it has none.
///
/// # Errors
///
/// [`MarkerError::Unterminated`] when the markers do not pair up.
pub fn split(text: &str) -> Result<Option<Split>, MarkerError> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Part {
        Common,
        Ours,
        Base,
        Theirs,
    }
    let mut out = Split::default();
    let mut part = Part::Common;
    let mut first_labels = true;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let body = line.strip_suffix('\n').unwrap_or(line);
        let body = body.strip_suffix('\r').unwrap_or(body);
        let marker = |m: &str| body == m || body.starts_with(&format!("{m} "));
        if marker("<<<<<<<") {
            if part != Part::Common {
                return Err(MarkerError::Unterminated(i + 1));
            }
            part = Part::Ours;
            out.conflicts += 1;
            if first_labels {
                body.get(8..).unwrap_or("").clone_into(&mut out.labels.0);
            }
            continue;
        }
        if marker("|||||||") && part == Part::Ours {
            part = Part::Base;
            continue;
        }
        if marker("=======") && matches!(part, Part::Ours | Part::Base) {
            part = Part::Theirs;
            continue;
        }
        if marker(">>>>>>>") && part == Part::Theirs {
            part = Part::Common;
            if first_labels {
                body.get(8..).unwrap_or("").clone_into(&mut out.labels.1);
                first_labels = false;
            }
            continue;
        }
        match part {
            Part::Common => {
                out.base.push_str(line);
                out.ours.push_str(line);
                out.theirs.push_str(line);
            }
            Part::Ours => out.ours.push_str(line),
            Part::Base => out.base.push_str(line),
            Part::Theirs => out.theirs.push_str(line),
        }
    }
    if part != Part::Common {
        return Err(MarkerError::Unterminated(text.lines().count().max(1)));
    }
    Ok((out.conflicts > 0).then_some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_two_way_block() {
        let s = split("a\n<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> feature\nc\n")
            .unwrap()
            .unwrap();
        assert_eq!(s.ours, "a\nx\nc\n");
        assert_eq!(s.theirs, "a\ny\nc\n");
        assert_eq!(s.base, "a\nc\n");
        assert_eq!(s.labels, ("HEAD".to_owned(), "feature".to_owned()));
        assert_eq!(s.conflicts, 1);
    }

    #[test]
    fn splits_a_diff3_block() {
        let s = split("<<<<<<< ours\nx\n||||||| base\nb\n=======\ny\n>>>>>>> theirs\n")
            .unwrap()
            .unwrap();
        assert_eq!(
            (s.base.as_str(), s.ours.as_str(), s.theirs.as_str()),
            ("b\n", "x\n", "y\n")
        );
    }

    #[test]
    fn no_markers_and_bad_markers() {
        assert_eq!(split("a\nb\n").unwrap(), None);
        assert!(!has_markers("a\n=======\nb\n"));
        assert!(has_markers("<<<<<<< x\n"));
        assert_eq!(split("<<<<<<< a\nx\n"), Err(MarkerError::Unterminated(2)));
        assert!(split("a\n>>>>>>> b\n").unwrap().is_none());
    }

    #[test]
    fn merging_the_split_finds_the_conflicts_again() {
        use crate::merge::{Kind, Merge, Pick};
        use crate::{Buffer, RopeBuffer, Snapshot};
        use std::sync::Arc;
        let text = "a\n<<<<<<< ours\nx\n=======\ny\n>>>>>>> theirs\nc\n";
        let parts = split(text).unwrap().unwrap();
        let snap = |t: &str| -> Arc<dyn Snapshot> { RopeBuffer::from_text(t).snapshot().into() };
        let (base, ours, theirs) = (snap(&parts.base), snap(&parts.ours), snap(&parts.theirs));
        let merge = Merge::new(&base, &ours, &theirs).unwrap();
        assert_eq!(merge.conflicts(), 1);
        assert_eq!(merge.chunks[1].kind, Kind::Conflict);
        let mut out = String::new();
        merge.write(&*base, &*ours, &*theirs, |_| Pick::Markers, &mut out);
        assert_eq!(out, text);
    }
}
