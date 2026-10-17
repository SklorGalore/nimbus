//! The regions of a merge result, kept aligned with the merge's inputs
//! while the result is edited (M9 tasks 7 and 8).
//!
//! A merge view shows the inputs beside an editable result. The result
//! starts as what [`Merge::write`] produced, so every chunk of the merge
//! has a known line range in it. From then on the result is a normal
//! document: the user takes a side for a change, or types. Rather than
//! diffing the result against anything, the view keeps each region's
//! result range up to date through the edits it sees:
//!
//! - an edit inside one region changes that region's length and marks it
//!   edited by hand;
//! - an edit spanning several regions joins them into one;
//! - every region after an edit moves by the lines it added or removed;
//! - taking a side replaces a region's result lines and marks which side.
//!
//! Each step is O(log regions) to find and O(regions) at worst to shift,
//! with no per-line memory, so typing in the result of a merge of 10 MB
//! files costs what typing anywhere costs. The only text read is the
//! lines of a take, copied once.

use std::ops::Range;

use crate::Text;
use crate::live_diff::EditSpan;
use crate::merge::{Chunk, Kind, Merge, Pick, lines_into};

/// How a region stands in the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// As the merge wrote it (for a conflict: markers, or nothing yet).
    Pending,
    /// The user took this side (or both, in this order).
    Taken(Pick),
    /// The user typed in it.
    Edited,
}

/// The tag a merge view's rows carry: what the merge said, and what has
/// been done about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionTag {
    /// The merge's verdict for the stretch.
    pub kind: Kind,
    /// What the user did with it.
    pub resolution: Resolution,
}

/// One stretch of the merge: its lines in each input and in the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// The merge's verdict.
    pub kind: Kind,
    /// What has been done with it.
    pub resolution: Resolution,
    /// Lines of the base.
    pub base: Range<usize>,
    /// Lines of ours.
    pub ours: Range<usize>,
    /// Lines of theirs.
    pub theirs: Range<usize>,
    /// Lines of the result, kept current through edits.
    pub result: Range<usize>,
}

impl Region {
    /// Whether this is a change the user may still act on.
    #[must_use]
    pub fn is_change(&self) -> bool {
        self.kind != Kind::Same || self.resolution != Resolution::Pending
    }

    /// Whether this is a conflict nobody has settled.
    #[must_use]
    pub fn is_open_conflict(&self) -> bool {
        self.kind == Kind::Conflict && self.resolution == Resolution::Pending
    }

    /// The tag for the rows: `None` for an untouched equal run.
    #[must_use]
    pub fn tag(&self) -> Option<RegionTag> {
        self.is_change().then_some(RegionTag {
            kind: self.kind,
            resolution: self.resolution,
        })
    }
}

/// The regions of one merge result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Regions {
    regions: Vec<Region>,
    /// Lines in the result.
    result_lines: usize,
}

impl Regions {
    /// Regions for `merge`, whose result was written chunk by chunk into
    /// the line ranges `result` (from [`Merge::write_tracked`]).
    #[must_use]
    pub fn new(merge: &Merge, result: &[Range<usize>]) -> Self {
        let regions = merge
            .chunks
            .iter()
            .zip(result)
            .map(|(c, r)| Region {
                kind: c.kind,
                resolution: Resolution::Pending,
                base: c.base.clone(),
                ours: c.ours.clone(),
                theirs: c.theirs.clone(),
                result: r.clone(),
            })
            .collect();
        Self {
            regions,
            result_lines: result.last().map_or(0, |r| r.end),
        }
    }

    /// Regions for a two-way merge: `merge` was made with the left text
    /// as both base and ours, and the result starts as a copy of the left
    /// text, so each region's result lines are its `ours` lines.
    #[must_use]
    pub fn from_ours(merge: &Merge) -> Self {
        let ranges: Vec<Range<usize>> = merge.chunks.iter().map(|c| c.ours.clone()).collect();
        Self::new(merge, &ranges)
    }

    /// The regions, in order; they tile the result.
    #[must_use]
    pub fn all(&self) -> &[Region] {
        &self.regions
    }

    /// Lines in the result.
    #[must_use]
    pub fn result_lines(&self) -> usize {
        self.result_lines
    }

    /// How many conflicts are still open.
    #[must_use]
    pub fn open_conflicts(&self) -> usize {
        self.regions.iter().filter(|r| r.is_open_conflict()).count()
    }

    /// The region holding result line `line`.
    #[must_use]
    pub fn at_result_line(&self, line: usize) -> Option<usize> {
        if self.regions.is_empty() {
            return None;
        }
        // The last region starting at or before `line`; empty regions at
        // the same start are skipped in favour of one that holds it.
        let mut i = self.regions.partition_point(|r| r.result.start <= line);
        i = i.saturating_sub(1);
        while i + 1 < self.regions.len()
            && self.regions[i].result.is_empty()
            && self.regions[i + 1].result.start <= line
        {
            i += 1;
        }
        Some(i)
    }

    /// Follow an edit to the result, already applied, whose text is now
    /// `after`: `span` as the document reports it. Regions the edit
    /// touched join into one, marked edited; the rest move.
    pub fn apply_edit(&mut self, after: &dyn Text, span: EditSpan) {
        // The new line count comes from the text itself: newline arithmetic
        // alone misses a final line without a newline appearing or going.
        let new_total = diff_lines(after);
        let delta = new_total.cast_signed() - self.result_lines.cast_signed();
        // An edit at the very end names the line after the last newline;
        // the lookup below maps that to the region ending there.
        let first_line = span.first_line;
        // The lines touched run from the one holding the start to the one
        // holding the byte after the removed text. When the new text ends
        // at a line start (it ends with a newline, or nothing was inserted
        // there), that following line is whole and untouched.
        let join = span.start + span.inserted;
        let joins_at_line_start = join == 0 || after.byte_at(join - 1) == Some(b'\n');
        let mut last_line = span.first_line + span.removed_lines;
        if joins_at_line_start && last_line > first_line {
            last_line -= 1;
        }
        self.result_lines = new_total;
        if self.regions.is_empty() {
            // Nothing to merge (every text was empty): what is typed is a
            // region of its own, from no input.
            self.regions.push(Region {
                kind: Kind::Same,
                resolution: Resolution::Edited,
                base: 0..0,
                ours: 0..0,
                theirs: 0..0,
                result: 0..new_total,
            });
            return;
        }
        let Some(first) = self.at_result_line(first_line) else {
            return;
        };
        let Some(last) = self.at_result_line(last_line) else {
            return;
        };
        let last = last.max(first);
        // Join the touched regions into the first; a changed stretch that
        // swallows an equal run keeps the stronger verdict.
        let joined: Region = {
            let mut r = self.regions[first].clone();
            for other in &self.regions[first + 1..=last] {
                r.base.end = other.base.end;
                r.ours.end = other.ours.end;
                r.theirs.end = other.theirs.end;
                r.result.end = other.result.end;
                r.kind = stronger(r.kind, other.kind);
            }
            r.resolution = Resolution::Edited;
            r.result.end = r
                .result
                .end
                .saturating_add_signed(delta)
                .max(r.result.start);
            r
        };
        self.regions.splice(first..=last, [joined]);
        for r in &mut self.regions[first + 1..] {
            r.result.start = r.result.start.saturating_add_signed(delta);
            r.result.end = r.result.end.saturating_add_signed(delta);
        }
    }

    /// The text that taking `pick` for region `i` puts in the result, into
    /// `out`, and the result lines it replaces. The caller makes the edit
    /// and then calls [`took`](Regions::took).
    pub fn take_text(
        &self,
        i: usize,
        pick: Pick,
        (base, ours, theirs): (&dyn Text, &dyn Text, &dyn Text),
        out: &mut String,
    ) -> Option<Range<usize>> {
        let r = self.regions.get(i)?;
        out.clear();
        match pick {
            Pick::Ours => lines_into(ours, r.ours.clone(), out),
            Pick::Theirs => lines_into(theirs, r.theirs.clone(), out),
            Pick::OursThenTheirs => {
                lines_into(ours, r.ours.clone(), out);
                lines_into(theirs, r.theirs.clone(), out);
            }
            Pick::TheirsThenOurs => {
                lines_into(theirs, r.theirs.clone(), out);
                lines_into(ours, r.ours.clone(), out);
            }
            Pick::Base => lines_into(base, r.base.clone(), out),
            Pick::Markers => {
                let m = Merge {
                    chunks: vec![Chunk {
                        kind: Kind::Conflict,
                        base: r.base.clone(),
                        ours: r.ours.clone(),
                        theirs: r.theirs.clone(),
                    }],
                    lines: [0; 3],
                };
                m.write(base, ours, theirs, |_| Pick::Markers, out);
            }
        }
        Some(r.result.clone())
    }

    /// Region `i` was replaced by a take of `pick` (through
    /// [`apply_edit`](Regions::apply_edit), which marked it edited):
    /// record the pick instead.
    pub fn took(&mut self, i: usize, pick: Pick) {
        if let Some(r) = self.regions.get_mut(i) {
            r.resolution = Resolution::Taken(pick);
        }
    }

    /// The segments for [`crate::align::Rows::from_segments`], with the
    /// columns ours, result, theirs (the base is not shown).
    pub fn segments(&self) -> impl Iterator<Item = (Option<RegionTag>, [Range<usize>; 3])> + '_ {
        self.regions.iter().map(|r| {
            (
                r.tag(),
                [r.ours.clone(), r.result.clone(), r.theirs.clone()],
            )
        })
    }
}

/// Of two verdicts joined by an edit, the one the user must still mind.
fn stronger(a: Kind, b: Kind) -> Kind {
    let rank = |k: Kind| match k {
        Kind::Same => 0,
        Kind::Both => 1,
        Kind::Ours | Kind::Theirs => 2,
        Kind::Conflict => 3,
    };
    if rank(b) > rank(a) { b } else { a }
}

/// Lines of `t` by the diff's convention: no empty line after a final
/// newline.
fn diff_lines(t: &dyn Text) -> usize {
    let n = t.len_lines();
    let len = t.len_bytes();
    if n > 0 && (len == 0 || t.byte_at(len - 1) == Some(b'\n')) {
        n - 1
    } else {
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Buffer, RopeBuffer, Snapshot};
    use std::sync::Arc;

    fn snap(text: &str) -> Arc<dyn Snapshot> {
        RopeBuffer::from_text(text).snapshot().into()
    }

    /// A merge of the three texts, its regions, and the result as a rope
    /// to edit: a copy of ours for a two-way merge (`pick` `None`), else
    /// written with `pick` for conflicts.
    fn setup(
        base: &str,
        ours: &str,
        theirs: &str,
        pick: Option<Pick>,
    ) -> (Regions, RopeBuffer, Merge) {
        let (base, ours_t, theirs) = (snap(base), snap(ours), snap(theirs));
        let m = Merge::new(&base, &ours_t, &theirs).unwrap();
        let Some(pick) = pick else {
            return (Regions::from_ours(&m), RopeBuffer::from_text(ours), m);
        };
        let mut out = String::new();
        let mut ranges = Vec::new();
        m.write_tracked(&*base, &*ours_t, &*theirs, |_| pick, &mut out, &mut ranges);
        (Regions::new(&m, &ranges), RopeBuffer::from_text(&out), m)
    }

    fn span(before: &str, start: usize, removed: usize, inserted: &str) -> EditSpan {
        EditSpan {
            start,
            first_line: before[..start].matches('\n').count(),
            removed_lines: before[start..start + removed].matches('\n').count(),
            inserted: inserted.len(),
        }
    }

    fn text(b: &RopeBuffer) -> String {
        b.slice(0..b.len_bytes()).into_owned()
    }

    #[test]
    fn two_way_regions_tile_the_result() {
        // Two-way: base is ours, so every difference is "theirs".
        let (r, result, _) = setup("a\nb\nc\n", "a\nb\nc\n", "a\nB\nc\nd\n", None);
        assert_eq!(text(&result), "a\nb\nc\n");
        let kinds: Vec<_> = r.all().iter().map(|x| (x.kind, x.result.clone())).collect();
        assert_eq!(
            kinds,
            vec![
                (Kind::Same, 0..1),
                (Kind::Theirs, 1..2),
                (Kind::Same, 2..3),
                (Kind::Theirs, 3..3)
            ]
        );
        assert_eq!(r.at_result_line(1), Some(1));
        assert_eq!(r.at_result_line(2), Some(2));
    }

    #[test]
    fn taking_a_side_replaces_the_region_and_shifts_the_rest() {
        let (mut r, mut result, _) = setup("a\nb\nc\n", "a\nb\nc\n", "a\nB1\nB2\nc\n", None);
        let (base, ours, theirs) = (snap("a\nb\nc\n"), snap("a\nb\nc\n"), snap("a\nB1\nB2\nc\n"));
        let mut out = String::new();
        let lines = r
            .take_text(1, Pick::Theirs, (&*base, &*ours, &*theirs), &mut out)
            .unwrap();
        assert_eq!(lines, 1..2);
        assert_eq!(out, "B1\nB2\n");
        let before = text(&result);
        let (start, end) = (result.line_to_byte(1), result.line_to_byte(2));
        result.replace(start..end, &out).unwrap();
        r.apply_edit(&result, span(&before, start, end - start, &out));
        r.took(1, Pick::Theirs);
        assert_eq!(text(&result), "a\nB1\nB2\nc\n");
        assert_eq!(r.all()[1].result, 1..3);
        assert_eq!(r.all()[1].resolution, Resolution::Taken(Pick::Theirs));
        assert_eq!(r.all()[2].result, 3..4, "the equal run after moved down");
        assert_eq!(r.result_lines(), 4);
    }

    #[test]
    fn typing_inside_a_region_marks_it_edited_and_across_regions_joins_them() {
        let (mut r, mut result, _) = setup("a\nb\nc\nd\n", "a\nb\nc\nd\n", "a\nB\nc\nD\n", None);
        // Type at the end of line 1 ("b"): one region, longer by a line.
        let before = text(&result);
        result.replace(3..3, "x\ny").unwrap();
        r.apply_edit(&result, span(&before, 3, 0, "x\ny"));
        assert_eq!(r.all()[1].result, 1..3);
        assert_eq!(r.all()[1].resolution, Resolution::Edited);
        assert_eq!(r.all()[2].result, 3..4);
        assert_eq!(r.all()[3].result, 4..5);
        // Delete from line 0 into line 3: regions 0..=2 become one.
        let before = text(&result);
        let (start, end) = (1, result.line_to_byte(3) + 1);
        result.replace(start..end, "").unwrap();
        r.apply_edit(&result, span(&before, start, end - start, ""));
        assert_eq!(r.all().len(), 2, "{:?}", r.all());
        assert_eq!(r.all()[0].kind, Kind::Theirs);
        assert_eq!(r.all()[0].result, 0..1);
        assert_eq!(r.all()[1].result, 1..2);
        assert_eq!(r.result_lines(), 2);
    }

    #[test]
    fn conflicts_start_as_markers_and_count() {
        let (r, result, _) = setup("a\nb\nc\n", "a\nX\nc\n", "a\nY\nc\n", Some(Pick::Markers));
        assert_eq!(
            text(&result),
            "a\n<<<<<<< ours\nX\n=======\nY\n>>>>>>> theirs\nc\n"
        );
        assert_eq!(r.open_conflicts(), 1);
        assert_eq!(r.all()[1].result, 1..6);
        assert_eq!(r.at_result_line(4), Some(1));
    }
}
