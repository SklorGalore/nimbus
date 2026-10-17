//! Three-way merge (M9 task 4).
//!
//! Given a common ancestor (the **base**) and two texts derived from it
//! (**ours** and **theirs**), a two-way diff can only say "these differ";
//! the base turns that into "who changed what". The merge is two diffs,
//! base to ours and base to theirs, walked together over the base's
//! lines:
//!
//! - a stretch of the base that neither diff touches is the same in all
//!   three texts ([`Kind::Same`]);
//! - a stretch only one side changed takes that side ([`Kind::Ours`],
//!   [`Kind::Theirs`]);
//! - a stretch both sides changed identically is taken once
//!   ([`Kind::Both`]);
//! - a stretch both sides changed differently is a [`Kind::Conflict`],
//!   for a person to settle.
//!
//! Two changes are "the same stretch" when their base ranges overlap *or
//! touch*: an insertion at the very line another side changed cannot be
//! ordered safely, so it is a conflict rather than a guess. This is what
//! git's merge does too.
//!
//! Every [`Chunk`] carries its line range in all three texts, so a view can
//! lay the three columns side by side and a result can be assembled by
//! picking, per chunk, which text to copy. Like the rest of the diff, it
//! works on interned line tokens and is pure: no I/O, no threads.
//!
//! Merging texts past [`DIFF_LIMIT`](crate::diff::DIFF_LIMIT) in layers
//! (trimming the common start and end first, as [`crate::compare`] does)
//! is left for when such a merge is needed; the merge views use this
//! directly.

use std::ops::Range;
use std::sync::Arc;

use crate::diff::{DiffError, Hunk, Interner, Lines};
use crate::{Snapshot, Text};

/// What a chunk of the merge is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Unchanged on both sides: all three ranges hold the same lines.
    Same,
    /// Changed on our side only; theirs still holds the base's lines.
    Ours,
    /// Changed on their side only; ours still holds the base's lines.
    Theirs,
    /// Both sides made the same change: ours and theirs hold the same
    /// lines, the base holds what they replaced.
    Both,
    /// Both sides changed the same stretch differently.
    Conflict,
}

/// A stretch of the merge, as line ranges in each text. Chunks tile all
/// three texts in order; neighbours are never both [`Kind::Same`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// What kind of stretch it is.
    pub kind: Kind,
    /// Lines of the base.
    pub base: Range<usize>,
    /// Lines of ours.
    pub ours: Range<usize>,
    /// Lines of theirs.
    pub theirs: Range<usize>,
}

/// Which text a conflict chunk takes when the merge result is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// Our lines.
    Ours,
    /// Their lines.
    Theirs,
    /// Our lines, then theirs.
    OursThenTheirs,
    /// Their lines, then ours.
    TheirsThenOurs,
    /// The base's lines: neither change.
    Base,
    /// Leave the conflict for later, as git does: our lines between
    /// `<<<<<<<` and `=======`, theirs up to `>>>>>>>`. Saving a merge
    /// with such blocks keeps it readable by every other tool.
    Markers,
}

/// The conflict marker lines [`Pick::Markers`] writes and the merge view
/// reads back (M9 task 9).
pub const MARKER_OURS: &str = "<<<<<<< ours";
/// Separates our lines from theirs in a conflict block.
pub const MARKER_MIDDLE: &str = "=======";
/// Ends a conflict block.
pub const MARKER_THEIRS: &str = ">>>>>>> theirs";

/// A three-way merge: the chunks and the line counts of the three texts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Merge {
    /// The chunks, in order.
    pub chunks: Vec<Chunk>,
    /// Lines in base, ours and theirs.
    pub lines: [usize; 3],
}

impl Merge {
    /// Merge `ours` and `theirs`, both derived from `base`.
    ///
    /// # Errors
    ///
    /// [`DiffError::TooManyLines`] from the interner.
    pub fn new(
        base: &Arc<dyn Snapshot>,
        ours: &Arc<dyn Snapshot>,
        theirs: &Arc<dyn Snapshot>,
    ) -> Result<Self, DiffError> {
        let mut interner = Interner::new();
        let b = interner.intern(base)?;
        let o = interner.intern(ours)?;
        let t = interner.intern(theirs)?;
        let to_ours = interner.diff(&b, &o);
        let to_theirs = interner.diff(&b, &t);
        Ok(Self::from_hunks(&to_ours, &to_theirs, &b, &o, &t))
    }

    /// The merge from the two diffs and the three token sequences (all
    /// from one interner).
    #[must_use]
    pub fn from_hunks(
        to_ours: &[Hunk],
        to_theirs: &[Hunk],
        base: &Lines,
        ours: &Lines,
        theirs: &Lines,
    ) -> Self {
        let lines = [base.len(), ours.len(), theirs.len()];
        let mut chunks = Vec::with_capacity(to_ours.len() + to_theirs.len() + 1);
        let (mut i, mut j) = (0, 0);
        // Where the next chunk starts in each text.
        let (mut b_pos, mut o_pos, mut t_pos) = (0, 0, 0);
        loop {
            let next_o = to_ours.get(i).map(|h| h.old.start);
            let next_t = to_theirs.get(j).map(|h| h.old.start);
            let Some(start) = next_o.into_iter().chain(next_t).min() else {
                break;
            };
            if start > b_pos {
                let n = start - b_pos;
                chunks.push(Chunk {
                    kind: Kind::Same,
                    base: b_pos..start,
                    ours: o_pos..o_pos + n,
                    theirs: t_pos..t_pos + n,
                });
                o_pos += n;
                t_pos += n;
            }
            // Gather every hunk, from either side, that overlaps or touches
            // the region so far; each one can extend it.
            let mut end = start;
            let (mut used_o, mut used_t) = (0usize, 0usize);
            let (mut o_delta, mut t_delta) = (0isize, 0isize);
            loop {
                let mut grew = false;
                if let Some(h) = to_ours.get(i + used_o)
                    && h.old.start <= end
                {
                    end = end.max(h.old.end);
                    o_delta += h.new.len().cast_signed() - h.old.len().cast_signed();
                    used_o += 1;
                    grew = true;
                }
                if let Some(h) = to_theirs.get(j + used_t)
                    && h.old.start <= end
                {
                    end = end.max(h.old.end);
                    t_delta += h.new.len().cast_signed() - h.old.len().cast_signed();
                    used_t += 1;
                    grew = true;
                }
                if !grew {
                    break;
                }
            }
            let base_len = end - start;
            let o_end = (o_pos + base_len).saturating_add_signed(o_delta);
            let t_end = (t_pos + base_len).saturating_add_signed(t_delta);
            let kind = match (used_o > 0, used_t > 0) {
                (true, false) => Kind::Ours,
                (false, true) => Kind::Theirs,
                _ if ours.tokens[o_pos..o_end] == theirs.tokens[t_pos..t_end] => Kind::Both,
                _ => Kind::Conflict,
            };
            chunks.push(Chunk {
                kind,
                base: start..end,
                ours: o_pos..o_end,
                theirs: t_pos..t_end,
            });
            i += used_o;
            j += used_t;
            b_pos = end;
            o_pos = o_end;
            t_pos = t_end;
        }
        if b_pos < lines[0] || o_pos < lines[1] || t_pos < lines[2] {
            chunks.push(Chunk {
                kind: Kind::Same,
                base: b_pos..lines[0],
                ours: o_pos..lines[1],
                theirs: t_pos..lines[2],
            });
        }
        Self { chunks, lines }
    }

    /// How many chunks are conflicts.
    #[must_use]
    pub fn conflicts(&self) -> usize {
        self.chunks
            .iter()
            .filter(|c| c.kind == Kind::Conflict)
            .count()
    }

    /// Write the merged text: each chunk copied from the text its kind
    /// names, and each conflict from what `pick` says for it (called with
    /// the conflict's index among conflicts). Appends to `out`.
    pub fn write(
        &self,
        base: &dyn Text,
        ours: &dyn Text,
        theirs: &dyn Text,
        pick: impl FnMut(usize) -> Pick,
        out: &mut String,
    ) {
        self.write_tracked(base, ours, theirs, pick, out, &mut Vec::new());
    }

    /// [`write`](Merge::write), also recording each chunk's line range in
    /// the result into `ranges` (one per chunk, in order), for a merge
    /// view that keeps the result aligned with its inputs.
    pub fn write_tracked(
        &self,
        base: &dyn Text,
        ours: &dyn Text,
        theirs: &dyn Text,
        mut pick: impl FnMut(usize) -> Pick,
        out: &mut String,
        ranges: &mut Vec<Range<usize>>,
    ) {
        let mut conflict = 0;
        let mut line = 0;
        ranges.clear();
        for c in &self.chunks {
            let before = out.len();
            match c.kind {
                Kind::Same => lines_into(base, c.base.clone(), out),
                Kind::Ours | Kind::Both => lines_into(ours, c.ours.clone(), out),
                Kind::Theirs => lines_into(theirs, c.theirs.clone(), out),
                Kind::Conflict => {
                    let p = pick(conflict);
                    conflict += 1;
                    match p {
                        Pick::Ours => lines_into(ours, c.ours.clone(), out),
                        Pick::Theirs => lines_into(theirs, c.theirs.clone(), out),
                        Pick::OursThenTheirs => {
                            lines_into(ours, c.ours.clone(), out);
                            lines_into(theirs, c.theirs.clone(), out);
                        }
                        Pick::TheirsThenOurs => {
                            lines_into(theirs, c.theirs.clone(), out);
                            lines_into(ours, c.ours.clone(), out);
                        }
                        Pick::Base => lines_into(base, c.base.clone(), out),
                        Pick::Markers => {
                            marker_line(out, MARKER_OURS);
                            lines_into(ours, c.ours.clone(), out);
                            marker_line(out, MARKER_MIDDLE);
                            lines_into(theirs, c.theirs.clone(), out);
                            marker_line(out, MARKER_THEIRS);
                        }
                    }
                }
            }
            let added = lines_in(&out[before..]);
            ranges.push(line..line + added);
            line += added;
        }
    }
}

/// Append a marker on a line of its own: a text that does not end with a
/// newline gets one first, so the marker never joins the line before it.
fn marker_line(out: &mut String, marker: &str) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(marker);
    out.push('\n');
}

/// Lines in `text` by the diff's convention: one per newline, plus one for
/// a final stretch without one.
#[must_use]
pub fn lines_in(text: &str) -> usize {
    let n = text.matches('\n').count();
    if text.is_empty() || text.ends_with('\n') {
        n
    } else {
        n + 1
    }
}

/// Append lines `lines` of `t` to `out`, line endings included.
pub fn lines_into(t: &dyn Text, lines: Range<usize>, out: &mut String) {
    if lines.is_empty() {
        return;
    }
    t.slice_into(t.line_to_byte(lines.start)..t.line_to_byte(lines.end), out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Buffer, RopeBuffer};

    fn snap(text: &str) -> Arc<dyn Snapshot> {
        RopeBuffer::from_text(text).snapshot().into()
    }

    fn merge(base: &str, ours: &str, theirs: &str) -> (Merge, String) {
        let (b, o, t) = (snap(base), snap(ours), snap(theirs));
        let m = Merge::new(&b, &o, &t).unwrap();
        let mut out = String::new();
        m.write(&*b, &*o, &*t, |_| Pick::OursThenTheirs, &mut out);
        (m, out)
    }

    fn kinds(m: &Merge) -> Vec<Kind> {
        m.chunks.iter().map(|c| c.kind).collect()
    }

    #[test]
    fn separate_changes_both_apply() {
        let (m, out) = merge("a\nb\nc\nd\ne\n", "A\nb\nc\nd\ne\n", "a\nb\nc\nd\nE\n");
        assert_eq!(kinds(&m), [Kind::Ours, Kind::Same, Kind::Theirs]);
        assert_eq!(out, "A\nb\nc\nd\nE\n");
        assert_eq!(m.conflicts(), 0);
    }

    #[test]
    fn identical_changes_are_taken_once() {
        let (m, out) = merge("a\nb\nc\n", "a\nX\nc\n", "a\nX\nc\n");
        assert_eq!(kinds(&m), [Kind::Same, Kind::Both, Kind::Same]);
        assert_eq!(out, "a\nX\nc\n");
    }

    #[test]
    fn different_changes_to_one_line_conflict() {
        let (m, out) = merge("a\nb\nc\n", "a\nX\nc\n", "a\nY\nc\n");
        assert_eq!(kinds(&m), [Kind::Same, Kind::Conflict, Kind::Same]);
        assert_eq!(m.conflicts(), 1);
        assert_eq!(out, "a\nX\nY\nc\n");
        assert_eq!(
            m.chunks[1],
            Chunk {
                kind: Kind::Conflict,
                base: 1..2,
                ours: 1..2,
                theirs: 1..2
            }
        );
    }

    #[test]
    fn touching_changes_conflict_and_adjacent_insertions_too() {
        // Ours changes line 1, theirs changes line 2: they touch.
        let (m, _) = merge("a\nb\nc\nd\n", "a\nB\nc\nd\n", "a\nb\nC\nd\n");
        assert_eq!(kinds(&m), [Kind::Same, Kind::Conflict, Kind::Same]);
        // Both insert at the same place.
        let (m, _) = merge("a\nb\n", "a\nx\nb\n", "a\ny\nb\n");
        assert_eq!(kinds(&m), [Kind::Same, Kind::Conflict, Kind::Same]);
        // Changes one line apart are separate.
        let (m, out) = merge("a\nb\nc\nd\n", "A\nb\nc\nd\n", "a\nb\nC\nd\n");
        assert_eq!(
            kinds(&m),
            [Kind::Ours, Kind::Same, Kind::Theirs, Kind::Same]
        );
        assert_eq!(out, "A\nb\nC\nd\n");
    }

    #[test]
    fn one_side_unchanged_gives_the_other() {
        let (m, out) = merge("a\nb\n", "a\nb\n", "x\ny\nz");
        assert_eq!(out, "x\ny\nz");
        assert!(
            kinds(&m)
                .iter()
                .all(|k| matches!(k, Kind::Theirs | Kind::Same))
        );
        let (_, out) = merge("", "new\n", "");
        assert_eq!(out, "new\n");
    }

    #[test]
    fn markers_and_tracked_ranges() {
        let (b, o, t) = (snap("a\nb\nc\n"), snap("a\nX\nc\n"), snap("a\nY\nc\n"));
        let m = Merge::new(&b, &o, &t).unwrap();
        let mut out = String::new();
        let mut ranges = Vec::new();
        m.write_tracked(&*b, &*o, &*t, |_| Pick::Markers, &mut out, &mut ranges);
        assert_eq!(out, "a\n<<<<<<< ours\nX\n=======\nY\n>>>>>>> theirs\nc\n");
        assert_eq!(ranges, vec![0..1, 1..6, 6..7]);
        // A side without a final newline still gets its marker on its own
        // line.
        let (b, o, t) = (snap("a"), snap("x"), snap("y"));
        let m = Merge::new(&b, &o, &t).unwrap();
        let mut out = String::new();
        m.write(&*b, &*o, &*t, |_| Pick::Markers, &mut out);
        assert_eq!(out, "<<<<<<< ours\nx\n=======\ny\n>>>>>>> theirs\n");
        assert_eq!(lines_in("a\nb"), 2);
        assert_eq!(lines_in("a\nb\n"), 2);
        assert_eq!(lines_in(""), 0);
    }

    #[test]
    fn picks_choose_the_conflict_text() {
        let (b, o, t) = (snap("a\nb\nc\n"), snap("a\nX\nc\n"), snap("a\nY\nc\n"));
        let m = Merge::new(&b, &o, &t).unwrap();
        for (pick, want) in [
            (Pick::Ours, "a\nX\nc\n"),
            (Pick::Theirs, "a\nY\nc\n"),
            (Pick::TheirsThenOurs, "a\nY\nX\nc\n"),
            (Pick::Base, "a\nb\nc\n"),
        ] {
            let mut out = String::new();
            m.write(&*b, &*o, &*t, |_| pick, &mut out);
            assert_eq!(out, want, "{pick:?}");
        }
    }
}
