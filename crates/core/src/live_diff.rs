//! A line diff kept current while one of its texts is edited (M9 task 3).
//!
//! Re-diffing two files after every keystroke would cost time proportional
//! to their size, which breaks the typing budget on anything large. But an
//! edit changes only the lines it touches: every hunk before them is still
//! right, and every hunk after them is still right once its line numbers
//! are shifted by the lines the edit added or removed. So a [`LiveDiff`]
//! keeps the tokens of both texts, and an edit does two things:
//!
//! 1. [`edit`](LiveDiff::edit) replaces the tokens of the lines the edit
//!    touched with freshly interned ones, shifts the hunks after them, and
//!    works out the **region** to re-diff: the touched lines, widened to
//!    swallow any hunk they touch, plus a little context, on both sides.
//!    The region's edges are equal lines (nothing outside a hunk differs),
//!    so a diff of the region alone splices in exactly.
//! 2. [`rediff`](LiveDiff::rediff) diffs that region and splices the new
//!    hunks in. It is a separate step so the caller can decide where to
//!    run it: in place when the region is small (it nearly always is), or
//!    later, from a worker, when a huge single hunk (a file rewritten)
//!    makes the region the whole file. Between the two steps the hunks
//!    are still a correct description of everything outside the region.
//!
//! This is the pattern M5 uses for reparsing after an edit: incremental
//! on the main thread within a budget, a worker past it.
//!
//! The live diff holds a token per line of each text, so it is for texts
//! within [`DIFF_LIMIT`]; past that the app recompares in layers
//! ([`crate::compare`]) after a pause, as the find bar re-searches.

use std::ops::Range;
use std::sync::Arc;

use crate::diff::{DIFF_LIMIT, DiffError, Hunk, Interner, Lines};
use crate::{Snapshot, Text};

/// Which of the two texts an edit happened in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The first text (left, or the saved file).
    Old,
    /// The second text (right, or the live document).
    New,
}

impl Side {
    fn index(self) -> usize {
        match self {
            Self::Old => 0,
            Self::New => 1,
        }
    }

    fn other(self) -> Self {
        match self {
            Self::Old => Self::New,
            Self::New => Self::Old,
        }
    }
}

/// Which lines of the text before an edit the edit touched, in the diff's
/// line numbering. The caller works these out from the document before
/// applying the edit, with [`Text::byte_to_line`] on the edit's start and
/// removed end; this crate cannot, since the removed text may be gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditSpan {
    /// Where the edit starts, in bytes (the same before and after).
    pub start: usize,
    /// The line holding `start`, before the edit.
    pub first_line: usize,
    /// How many line breaks the removed text held: the edit touched lines
    /// `first_line..=first_line + removed_lines` before it.
    pub removed_lines: usize,
    /// How many bytes were inserted at `start`.
    pub inserted: usize,
}

impl EditSpan {
    /// The span of `edit`, already applied, read from the text `after` it:
    /// the text before the edit is the same up to `start`, so the first
    /// line is the same, and the removed text's line breaks are counted in
    /// the edit record. `None` when that cannot be done: the removed text
    /// is held as pieces of a mapped file rather than bytes, or the start
    /// lies past the part of a mapped file whose lines are counted. The
    /// caller then falls back to a full diff.
    #[must_use]
    pub fn after(after: &dyn Text, edit: &crate::Edit) -> Option<Self> {
        let removed = edit.removed.as_bytes()?;
        if edit.start > after.indexed_bytes() {
            return None;
        }
        Some(Self {
            start: edit.start,
            first_line: after.byte_to_line(edit.start),
            removed_lines: memchr::memchr_iter(b'\n', removed).count(),
            inserted: edit.inserted.len(),
        })
    }
}

/// A stretch of both texts to re-diff, from [`LiveDiff::edit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// Lines of the old text (current numbering).
    pub old: Range<usize>,
    /// Lines of the new text (current numbering).
    pub new: Range<usize>,
    /// The hunks the region took out of [`LiveDiff::hunks`] when it was
    /// made: what the view can keep showing, roughly placed, until the
    /// re-diff. Their line numbers are from before the edit.
    pub stale: Vec<Hunk>,
    /// Where in the hunk list the re-diff's hunks go.
    at: usize,
}

impl Region {
    /// How many lines the re-diff will look at, both sides together: the
    /// caller's budget.
    #[must_use]
    pub fn lines(&self) -> usize {
        self.old.len() + self.new.len()
    }
}

/// Equal lines included on each side of a region, so a hunk that slides
/// into them lands where a fresh diff would put it.
const CONTEXT: usize = 3;

/// A diff of two texts that follows edits to either.
///
/// Invariant: [`hunks`](LiveDiff::hunks) is always sorted, apart and in
/// current line numbers. A pending region's lines simply have no hunks in
/// the list until [`rediff`](LiveDiff::rediff) puts them back.
#[derive(Debug)]
pub struct LiveDiff {
    interner: Interner,
    sides: [Lines; 2],
    hunks: Vec<Hunk>,
    /// A region handed out by [`edit`](LiveDiff::edit) and not yet
    /// re-diffed; a second edit widens it rather than losing it.
    pending: Option<Region>,
}

impl LiveDiff {
    /// Diff `old` against `new` in full. Both must be within
    /// [`DIFF_LIMIT`]; this does not trim or window.
    ///
    /// # Errors
    ///
    /// [`DiffError::TooManyLines`] from the interner, or
    /// [`DiffError::TooLarge`] past the limit.
    pub fn new(old: &Arc<dyn Snapshot>, new: &Arc<dyn Snapshot>) -> Result<Self, DiffError> {
        if old.len_bytes() > DIFF_LIMIT || new.len_bytes() > DIFF_LIMIT {
            return Err(DiffError::TooLarge);
        }
        let mut interner = Interner::new();
        let a = interner.intern(old)?;
        let b = interner.intern(new)?;
        let hunks = interner.diff(&a, &b);
        Ok(Self {
            interner,
            sides: [a, b],
            hunks,
            pending: None,
        })
    }

    /// The current hunks, sorted and apart. Exact outside a pending
    /// region; inside one there are none until the re-diff.
    #[must_use]
    pub fn hunks(&self) -> &[Hunk] {
        &self.hunks
    }

    /// Lines in a side's text.
    #[must_use]
    pub fn lines(&self, side: Side) -> usize {
        self.sides[side.index()].len()
    }

    /// How many text versions the interner keeps alive (one per edit
    /// followed); see [`Interner::pinned_texts`].
    #[must_use]
    pub fn pinned_texts(&self) -> usize {
        self.interner.pinned_texts()
    }

    /// The region waiting for [`rediff`](LiveDiff::rediff), if any.
    #[must_use]
    pub fn pending(&self) -> Option<&Region> {
        self.pending.as_ref()
    }

    /// Record an edit to `side`, whose text is now `after`. Replaces the
    /// touched lines' tokens, shifts the hunks after them, takes the hunks
    /// they meet out of the list, and returns the region to pass to
    /// [`rediff`](LiveDiff::rediff).
    ///
    /// # Errors
    ///
    /// [`DiffError::TooManyLines`] from the interner.
    pub fn edit(
        &mut self,
        side: Side,
        after: &Arc<dyn Snapshot>,
        span: EditSpan,
    ) -> Result<Region, DiffError> {
        let s = side.index();
        let before_len = self.sides[s].len();
        // Tokens the edit touched, before: the lines from the first to the
        // one holding the removed end. Clamped: the line after a final
        // newline has no token.
        let first = span.first_line.min(before_len);
        let last = (span.first_line + span.removed_lines + 1).min(before_len);
        // The same lines after: from the first line's start to the end of
        // the line holding the inserted end.
        let from = after.line_to_byte(first);
        let to = line_end_at(&**after, span.start + span.inserted);
        let fresh = self.interner.intern_range(after, from..to)?;
        let added = fresh.len();
        let removed = last - first;
        self.sides[s].tokens.splice(first..last, fresh.tokens);
        let delta = len_delta(added, removed);

        // Hunks before the touched lines stay; hunks after move by
        // `delta`; hunks meeting them (touching counts: a hunk ending
        // where the edit starts may grow into it) come out, and the
        // region covers them.
        let mut lo = first;
        let mut hi = first + added;
        let n = self.hunks.len();
        let mut take_from = n;
        let mut take_to = n;
        for (i, h) in self.hunks.iter_mut().enumerate() {
            let r = range_of(h, side);
            if r.end < first {
                continue;
            }
            if r.start > last {
                if take_from == n {
                    take_from = i;
                    take_to = i;
                }
                set_range(h, side, shifted(r, delta));
                continue;
            }
            if take_from == n {
                take_from = i;
            }
            take_to = i + 1;
            lo = lo.min(r.start);
            // Its far end in post-edit numbers: inside the touched span it
            // can reach at most the span's new end; past it, it moved.
            let end_after = if r.end <= last {
                r.end.min(first + added)
            } else {
                r.end.saturating_add_signed(delta)
            };
            hi = hi.max(end_after);
        }
        // Context, stopped by the text's ends, absorbing any hunk it
        // reaches: a region's edge must be an equal line.
        lo = lo.saturating_sub(CONTEXT);
        hi = (hi + CONTEXT).min(self.sides[s].len());
        while take_from > 0 && range_of(&self.hunks[take_from - 1], side).end >= lo {
            take_from -= 1;
            lo = lo.min(range_of(&self.hunks[take_from], side).start);
        }
        while take_to < n && range_of(&self.hunks[take_to], side).start <= hi {
            hi = hi.max(range_of(&self.hunks[take_to], side).end);
            take_to += 1;
        }
        let mut stale: Vec<Hunk> = self.hunks.drain(take_from..take_to).collect();
        let mut at = take_from;

        // A region still pending from an earlier edit joins this one: its
        // lines on this side move with the edit, the hunks between the
        // two come out as well, and the stale hunks are pooled.
        if let Some(earlier) = self.pending.take() {
            let moved = |r: Range<usize>| -> Range<usize> {
                if r.start >= first {
                    shifted(r, delta)
                } else if r.end > first {
                    r.start..r.end.saturating_add_signed(delta).max(r.start)
                } else {
                    r
                }
            };
            let e_range = match side {
                Side::Old => moved(earlier.old),
                Side::New => moved(earlier.new),
            };
            lo = lo.min(e_range.start);
            hi = hi.max(e_range.end).min(self.sides[s].len());
            let e_at = if earlier.at <= take_from {
                earlier.at
            } else {
                earlier
                    .at
                    .saturating_sub(take_to - take_from)
                    .max(take_from)
            };
            let (from, to) = (e_at.min(at), e_at.max(at));
            stale.extend(self.hunks.drain(from..to));
            at = from;
            stale.extend(earlier.stale);
            // The join may have put the edge inside a hunk again.
            while at > 0 && range_of(&self.hunks[at - 1], side).end >= lo {
                at -= 1;
                let h = self.hunks.remove(at);
                lo = lo.min(range_of(&h, side).start);
                stale.push(h);
            }
            while at < self.hunks.len() && range_of(&self.hunks[at], side).start <= hi {
                let h = self.hunks.remove(at);
                hi = hi.max(range_of(&h, side).end);
                stale.push(h);
            }
        }

        let mut region = self.region_from(side, lo..hi, at);
        region.stale = stale;
        self.pending = Some(region.clone());
        Ok(region)
    }

    /// The other side's lines aligned with `range` on `side`, with no
    /// hunks left inside it, going into the hunk list at `at`. Outside
    /// hunks, lines pair one to one, so the start is found by counting
    /// from the start of the texts through the hunks before `at`, and the
    /// end by counting back from their ends through the hunks after it.
    fn region_from(&self, side: Side, range: Range<usize>, at: usize) -> Region {
        let other = side.other();
        let (side_len, other_len) = (
            self.sides[side.index()].len(),
            self.sides[other.index()].len(),
        );
        let mut before = 0isize;
        for h in &self.hunks[..at] {
            before += len_delta(range_of(h, other).len(), range_of(h, side).len());
        }
        let other_start = range.start.saturating_add_signed(before).min(other_len);
        let mut after = 0isize;
        for h in &self.hunks[at..] {
            after += len_delta(range_of(h, other).len(), range_of(h, side).len());
        }
        let tail = side_len.saturating_sub(range.end);
        let other_end = (other_len.cast_signed() - tail.cast_signed() - after)
            .clamp(other_start.cast_signed(), other_len.cast_signed())
            .cast_unsigned();
        let (old, new) = match side {
            Side::Old => (range, other_start..other_end),
            Side::New => (other_start..other_end, range),
        };
        Region {
            old,
            new,
            stale: Vec::new(),
            at,
        }
    }

    /// Diff `region` and put the result into the hunks. `region` must be
    /// the latest one from [`edit`](LiveDiff::edit) (or
    /// [`pending`](LiveDiff::pending)); any other is ignored.
    pub fn rediff(&mut self, region: &Region) {
        let Some(pending) = &self.pending else {
            return;
        };
        if pending.old != region.old || pending.new != region.new || pending.at != region.at {
            return;
        }
        self.pending = None;
        let old = &self.sides[0].tokens[region.old.clone()];
        let new = &self.sides[1].tokens[region.new.clone()];
        let mut fresh = self.interner.diff_tokens(old, new);
        for h in &mut fresh {
            h.old = h.old.start + region.old.start..h.old.end + region.old.start;
            h.new = h.new.start + region.new.start..h.new.end + region.new.start;
        }
        self.hunks.splice(region.at..region.at, fresh);
    }

    /// [`edit`](LiveDiff::edit) then [`rediff`](LiveDiff::rediff) at once,
    /// for callers without a budget to keep.
    ///
    /// # Errors
    ///
    /// As [`edit`](LiveDiff::edit).
    pub fn edit_now(
        &mut self,
        side: Side,
        after: &Arc<dyn Snapshot>,
        span: EditSpan,
    ) -> Result<Region, DiffError> {
        let region = self.edit(side, after, span)?;
        self.rediff(&region);
        Ok(region)
    }
}

fn range_of(h: &Hunk, side: Side) -> Range<usize> {
    match side {
        Side::Old => h.old.clone(),
        Side::New => h.new.clone(),
    }
}

fn set_range(h: &mut Hunk, side: Side, r: Range<usize>) {
    match side {
        Side::Old => h.old = r,
        Side::New => h.new = r,
    }
}

fn shifted(r: Range<usize>, delta: isize) -> Range<usize> {
    let start = r.start.saturating_add_signed(delta);
    let end = r.end.saturating_add_signed(delta).max(start);
    start..end
}

/// `a - b` as a signed count of lines. Line counts fit in `isize`: they
/// are bounded by the text's length in bytes.
fn len_delta(a: usize, b: usize) -> isize {
    a.cast_signed() - b.cast_signed()
}

/// The end of the line holding `at`: after its `\n`, or the end of the
/// text.
fn line_end_at(t: &dyn Text, at: usize) -> usize {
    let len = t.len_bytes();
    let mut pos = at.min(len);
    while pos < len {
        let (chunk, base) = t.chunk_at(pos);
        let Some(bytes) = chunk.get(pos - base..) else {
            return len;
        };
        if bytes.is_empty() {
            return len;
        }
        if let Some(i) = memchr::memchr(b'\n', bytes) {
            return pos + i + 1;
        }
        pos += bytes.len();
    }
    len
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Buffer, RopeBuffer};

    fn snap(text: &str) -> Arc<dyn Snapshot> {
        RopeBuffer::from_text(text).snapshot().into()
    }

    /// The span of an edit at `start` replacing `removed` with `inserted`
    /// in `before`, as the app would compute it from the document.
    fn span(before: &str, start: usize, removed: usize, inserted: &str) -> EditSpan {
        EditSpan {
            start,
            first_line: before[..start].matches('\n').count(),
            removed_lines: before[start..start + removed].matches('\n').count(),
            inserted: inserted.len(),
        }
    }

    fn edited(before: &str, start: usize, removed: usize, inserted: &str) -> String {
        let mut s = before.to_owned();
        s.replace_range(start..start + removed, inserted);
        s
    }

    #[test]
    fn typing_inside_a_line_rediffs_that_line_only() {
        let old = "a\nb\nc\nd\ne\n";
        let mut live = LiveDiff::new(&snap(old), &snap(old)).unwrap();
        assert!(live.hunks().is_empty());
        let new = edited(old, 4, 0, "X");
        let region = live
            .edit_now(Side::New, &snap(&new), span(old, 4, 0, "X"))
            .unwrap();
        assert_eq!(
            live.hunks(),
            &[Hunk {
                old: 2..3,
                new: 2..3
            }]
        );
        assert!(region.lines() <= 2 * (1 + 2 * CONTEXT), "{region:?}");
        assert_eq!(live.lines(Side::New), 5);
    }

    #[test]
    fn inserting_lines_shifts_later_hunks() {
        let old = "a\nb\nc\nd\ne\n";
        let new0 = "a\nb\nc\nd\nE\n";
        let mut live = LiveDiff::new(&snap(old), &snap(new0)).unwrap();
        assert_eq!(
            live.hunks(),
            &[Hunk {
                old: 4..5,
                new: 4..5
            }]
        );
        let new1 = edited(new0, 2, 0, "x\ny\n");
        let region = live
            .edit(Side::New, &snap(&new1), span(new0, 2, 0, "x\ny\n"))
            .unwrap();
        // Before the re-diff the later hunk has moved, and the region
        // took it (it is within the context) out of the list.
        assert!(live.hunks().is_empty());
        assert_eq!(
            region.stale,
            vec![Hunk {
                old: 4..5,
                new: 6..7
            }]
        );
        live.rediff(&region);
        assert_eq!(
            live.hunks(),
            &[
                Hunk {
                    old: 1..1,
                    new: 1..3
                },
                Hunk {
                    old: 4..5,
                    new: 6..7
                }
            ]
        );
    }

    #[test]
    fn editing_the_old_side_works_too() {
        let old = "a\nb\nc\n";
        let new = "a\nB\nc\n";
        let mut live = LiveDiff::new(&snap(old), &snap(new)).unwrap();
        let old1 = edited(old, 2, 1, "B");
        live.edit_now(Side::Old, &snap(&old1), span(old, 2, 1, "B"))
            .unwrap();
        assert!(live.hunks().is_empty(), "{:?}", live.hunks());
    }

    #[test]
    fn deleting_across_lines_and_the_final_newline() {
        let old = "a\nb\nc";
        let new = "a\nb\nc";
        let mut live = LiveDiff::new(&snap(old), &snap(new)).unwrap();
        // Join the first two lines.
        let new1 = edited(new, 1, 1, "");
        live.edit_now(Side::New, &snap(&new1), span(new, 1, 1, ""))
            .unwrap();
        assert_eq!(live.lines(Side::New), 2);
        assert_eq!(
            live.hunks(),
            &[Hunk {
                old: 0..2,
                new: 0..1
            }]
        );
        // Add a newline at the very end: no new line by the convention.
        let new2 = edited(&new1, new1.len(), 0, "\n");
        live.edit_now(Side::New, &snap(&new2), span(&new1, new1.len(), 0, "\n"))
            .unwrap();
        assert_eq!(live.lines(Side::New), 2);
        assert_eq!(
            live.hunks(),
            &[Hunk {
                old: 0..3,
                new: 0..2
            }]
        );
    }

    #[test]
    fn two_edits_before_a_rediff_merge_their_regions() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let mut live = LiveDiff::new(&snap(old), &snap(old)).unwrap();
        let n1 = edited(old, 0, 1, "A");
        let r1 = live
            .edit(Side::New, &snap(&n1), span(old, 0, 1, "A"))
            .unwrap();
        let n2 = edited(&n1, 14, 1, "H");
        let r2 = live
            .edit(Side::New, &snap(&n2), span(&n1, 14, 1, "H"))
            .unwrap();
        assert!(
            r2.new.start <= r1.new.start && r2.new.end >= 8,
            "{r1:?} {r2:?}"
        );
        live.rediff(&r1); // stale: ignored
        assert!(live.pending().is_some());
        live.rediff(&r2);
        assert_eq!(
            live.hunks(),
            &[
                Hunk {
                    old: 0..1,
                    new: 0..1
                },
                Hunk {
                    old: 7..8,
                    new: 7..8
                }
            ]
        );
    }
}
