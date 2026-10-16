//! Cursors and selections.
//!
//! A selection has an *anchor*, where it started, and a *head*, where the
//! cursor is now. When they are equal the selection is just a cursor. Keeping
//! both, instead of a sorted start and end, preserves direction: Shift+Left
//! from a selection made rightwards shrinks it rather than growing it.

use std::ops::Range;

/// One selection, as byte offsets into the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Selection {
    /// Where the selection started; stays put while extending.
    pub anchor: usize,
    /// Where the cursor is; moves while extending.
    pub head: usize,
}

impl Selection {
    /// A bare cursor at `pos`.
    #[must_use]
    pub fn cursor(pos: usize) -> Self {
        Self {
            anchor: pos,
            head: pos,
        }
    }

    /// Whether anchor and head coincide.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// The selected bytes, smallest offset first.
    #[must_use]
    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    /// Move the head to `pos`; collapse onto it unless `extend` is set.
    #[must_use]
    pub fn moved_to(self, pos: usize, extend: bool) -> Self {
        if extend {
            Self {
                anchor: self.anchor,
                head: pos,
            }
        } else {
            Self::cursor(pos)
        }
    }
}

/// Which side of an edit a position touching it ends up on, for
/// [`Selections::map_through`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bias {
    /// After the inserted text: a cursor where text was typed moves past it.
    After,
    /// Before the inserted text: the start of a selection stays where the
    /// inserted text begins.
    Before,
}

/// All selections in a document: one per cursor.
///
/// Always at least one. They are kept sorted by position and never
/// overlap, which is what lets an edit at every cursor be applied in one
/// pass from the end of the text back to the start ([`Selections::normalize`]
/// restores this after any change). One of them is the *primary*: the one
/// the view follows, the input method attaches to, and the find bar starts
/// from. A newly added cursor becomes the primary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selections {
    items: Vec<Selection>,
    /// Index of the primary in `items`.
    primary: usize,
}

impl Default for Selections {
    fn default() -> Self {
        Self::single(Selection::default())
    }
}

/// Whether `b`, which starts at or after `a`, should merge into it: they
/// overlap, or touch where either is a bare cursor. Two selections that
/// only touch stay apart, so each keeps its own text.
fn overlaps(a: Selection, b: Selection) -> bool {
    let (ra, rb) = (a.range(), b.range());
    rb.start < ra.end || (rb.start == ra.end && (a.is_empty() || b.is_empty()))
}

/// `range` as a selection pointing the way `like` points: forwards (head
/// at the end) unless `like` was made backwards.
fn directed(range: Range<usize>, like: Selection) -> Selection {
    if like.head < like.anchor {
        Selection {
            anchor: range.end,
            head: range.start,
        }
    } else {
        Selection {
            anchor: range.start,
            head: range.end,
        }
    }
}

impl Selections {
    /// Exactly one selection.
    #[must_use]
    pub fn single(sel: Selection) -> Self {
        Self {
            items: vec![sel],
            primary: 0,
        }
    }

    /// Selections from a list in any order, with `items[primary]` as the
    /// primary; sorted and merged. An empty list gives a cursor at 0.
    #[must_use]
    pub fn from_vec(items: Vec<Selection>, primary: usize) -> Self {
        if items.is_empty() {
            return Self::default();
        }
        let primary = primary.min(items.len() - 1);
        let mut sels = Self { items, primary };
        sels.normalize();
        sels
    }

    /// The main selection: the one the view follows and the IME attaches to.
    #[must_use]
    pub fn primary(&self) -> Selection {
        // `items` is never empty and `primary` is always in range.
        self.items.get(self.primary).copied().unwrap_or_default()
    }

    /// Where the primary is in [`Selections::as_slice`].
    #[must_use]
    pub fn primary_index(&self) -> usize {
        self.primary
    }

    /// How many selections there are: at least one.
    #[must_use]
    pub fn count(&self) -> usize {
        self.items.len()
    }

    /// All selections, sorted by position.
    #[must_use]
    pub fn as_slice(&self) -> &[Selection] {
        &self.items
    }

    /// All selections, sorted by position.
    pub fn iter(&self) -> impl Iterator<Item = &Selection> {
        self.items.iter()
    }

    /// Make `sel` the only selection.
    pub fn set_single(&mut self, sel: Selection) {
        self.items.clear();
        self.items.push(sel);
        self.primary = 0;
    }

    /// Keep only the primary (Escape with several cursors).
    pub fn collapse_to_primary(&mut self) {
        let p = self.primary();
        self.set_single(p);
    }

    /// Replace the primary with `sel`, keeping the others (dragging out a
    /// selection that was just added). Merges if it now overlaps another.
    pub fn replace_primary(&mut self, sel: Selection) {
        if let Some(p) = self.items.get_mut(self.primary) {
            *p = sel;
        }
        self.normalize();
    }

    /// Add `sel` as a new selection and make it the primary. If it
    /// overlaps one already there, the two merge.
    pub fn push_primary(&mut self, sel: Selection) {
        self.items.push(sel);
        self.primary = self.items.len() - 1;
        self.normalize();
    }

    /// Remove the selection at `index`, unless it is the only one. The
    /// primary moves to the selection before it, if it was the one removed.
    /// Returns whether one was removed.
    pub fn remove(&mut self, index: usize) -> bool {
        if self.items.len() <= 1 || index >= self.items.len() {
            return false;
        }
        self.items.remove(index);
        if self.primary > index || self.primary == self.items.len() {
            self.primary -= 1;
        }
        true
    }

    /// Change every selection with `f`, then sort and merge. For moving
    /// every cursor at once; `f` is told each selection's index, which
    /// [`Selections::primary_index`] can be compared with.
    pub fn transform(&mut self, mut f: impl FnMut(usize, Selection) -> Selection) {
        for (i, sel) in self.items.iter_mut().enumerate() {
            *sel = f(i, *sel);
        }
        self.normalize();
    }

    /// Sort the selections and merge any that overlap, keeping track of
    /// the primary. A merged selection covers both; it points the way the
    /// primary did if the primary is one of them, otherwise the way the
    /// earlier one did. Allocates nothing.
    pub fn normalize(&mut self) {
        if self.items.len() <= 1 {
            self.primary = 0;
            return;
        }
        let p = self.primary();
        self.items
            .sort_unstable_by_key(|s| (s.range().start, s.range().end, s.head));
        // Equal selections are interchangeable, so the first equal to the
        // old primary is it.
        let mut pi = self.items.iter().position(|s| *s == p).unwrap_or(0);
        let mut w = 0;
        for r in 1..self.items.len() {
            let (a, b) = (self.items[w], self.items[r]);
            if overlaps(a, b) {
                let (ra, rb) = (a.range(), b.range());
                let merged = ra.start..ra.end.max(rb.end);
                let like = if r == pi && !b.is_empty() { b } else { a };
                self.items[w] = directed(merged, like);
                if r == pi {
                    pi = w;
                }
            } else {
                w += 1;
                self.items[w] = b;
                if r == pi {
                    pi = w;
                }
            }
        }
        self.items.truncate(w + 1);
        self.primary = pi;
    }

    /// Move every selection through `edits`, the result of an edit at
    /// every cursor ([`crate::Buffer::replace_sorted`]: in descending
    /// order, offsets in the text before any of them). One pass over both.
    ///
    /// A position before an edit stays; one after it shifts by how much
    /// the edit grew the text; one touching or inside it goes to the end of
    /// the new text with [`Bias::After`], or its start with
    /// [`Bias::Before`]. `start` is the bias for each selection's start and
    /// `end` for its end: typing uses `After` for both, so the cursor ends
    /// up after what was typed; indenting uses `Before` for the start, so a
    /// selection from the start of a line keeps the indent inside it.
    /// Sorts and merges afterwards.
    pub fn map_through(&mut self, edits: &[crate::Edit], start: Bias, end: Bias) {
        let mut mapper = Mapper {
            edits,
            next: 0,
            shift: 0,
        };
        for sel in &mut self.items {
            let r = sel.range();
            let (s, e) = if r.is_empty() {
                let at = mapper.map(r.start, Side::End, end);
                (at, at)
            } else {
                (
                    mapper.map(r.start, Side::Start, start),
                    mapper.map(r.end, Side::End, end),
                )
            };
            *sel = directed(s..e, *sel);
        }
        self.normalize();
    }
}

/// Which end of a selection a position is, for [`Mapper::map`]. The two
/// differ only where an edit starts or ends exactly at the position: the
/// end of a selection is not moved by an edit that starts there (that edit
/// belongs to what follows), and the start of one is not moved by an edit
/// that ends there.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Start,
    End,
}

/// Maps positions, in increasing order, through descending edits.
struct Mapper<'a> {
    edits: &'a [crate::Edit],
    /// How many edits, counted from the last (the first in the text), lie
    /// wholly before the positions seen so far.
    next: usize,
    /// How much those edits grew the text.
    shift: isize,
}

impl Mapper<'_> {
    /// The `i`-th edit in text order.
    fn edit(&self, i: usize) -> Option<&crate::Edit> {
        let n = self.edits.len();
        if i < n {
            self.edits.get(n - 1 - i)
        } else {
            None
        }
    }

    fn map(&mut self, pos: usize, side: Side, bias: Bias) -> usize {
        while let Some(e) = self.edit(self.next) {
            let end = e.old_range().end;
            let before = match side {
                Side::Start => end < pos || (end == pos && e.start < pos),
                Side::End => end < pos,
            };
            if !before {
                break;
            }
            self.shift += e.growth();
            self.next += 1;
        }
        let shifted = |p: usize, shift: isize| p.saturating_add_signed(shift);
        let Some(e) = self.edit(self.next) else {
            return shifted(pos, self.shift);
        };
        let covers = match side {
            Side::Start => e.start <= pos,
            Side::End => e.start < pos || (e.start == pos && e.removed.is_empty()),
        };
        if !covers {
            return shifted(pos, self.shift);
        }
        let start = shifted(e.start, self.shift);
        match bias {
            Bias::After => start + e.inserted.len(),
            Bias::Before => start,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_is_ordered_regardless_of_direction() {
        let s = Selection { anchor: 9, head: 3 };
        assert_eq!(s.range(), 3..9);
        assert!(!s.is_empty());
    }

    #[test]
    fn moving_collapses_unless_extending() {
        let s = Selection { anchor: 2, head: 5 };
        assert_eq!(s.moved_to(7, false), Selection::cursor(7));
        assert_eq!(s.moved_to(1, true), Selection { anchor: 2, head: 1 });
    }

    fn sel(anchor: usize, head: usize) -> Selection {
        Selection { anchor, head }
    }

    #[test]
    fn selections_always_have_a_primary() {
        let mut sels = Selections::default();
        assert_eq!(sels.primary(), Selection::cursor(0));
        sels.set_single(Selection::cursor(4));
        assert_eq!(sels.primary().head, 4);
        assert_eq!(sels.count(), 1);
        assert!(!sels.remove(0), "the last one stays");
        assert_eq!(Selections::from_vec(Vec::new(), 3), Selections::default());
    }

    #[test]
    fn selections_sort_and_the_newest_is_primary() {
        let mut sels = Selections::single(Selection::cursor(10));
        sels.push_primary(Selection::cursor(2));
        sels.push_primary(sel(20, 25));
        assert_eq!(
            sels.as_slice(),
            &[Selection::cursor(2), Selection::cursor(10), sel(20, 25)]
        );
        assert_eq!(sels.primary(), sel(20, 25));
        assert!(sels.remove(2));
        assert_eq!(sels.primary(), Selection::cursor(10), "the one before");
        sels.collapse_to_primary();
        assert_eq!(sels.as_slice(), &[Selection::cursor(10)]);
    }

    #[test]
    fn overlapping_selections_merge_and_touching_ones_stay_apart() {
        // Overlapping: merged, pointing the way the primary did.
        let s = Selections::from_vec(vec![sel(0, 5), sel(8, 3)], 1);
        assert_eq!(s.as_slice(), &[sel(8, 0)]);
        assert_eq!(s.primary_index(), 0);
        // Not the primary: the earlier one's direction.
        let s = Selections::from_vec(vec![sel(0, 5), sel(8, 3), Selection::cursor(20)], 2);
        assert_eq!(s.as_slice(), &[sel(0, 8), Selection::cursor(20)]);
        assert_eq!(s.primary(), Selection::cursor(20));
        // Touching selections stay apart; a cursor touching one merges.
        let s = Selections::from_vec(vec![sel(0, 4), sel(4, 9)], 0);
        assert_eq!(s.count(), 2);
        let s = Selections::from_vec(vec![sel(0, 4), Selection::cursor(4)], 1);
        assert_eq!(s.as_slice(), &[sel(0, 4)]);
        assert_eq!(s.primary_index(), 0);
        // Two cursors at one place are one.
        let s = Selections::from_vec(vec![Selection::cursor(3); 3], 2);
        assert_eq!(s.as_slice(), &[Selection::cursor(3)]);
    }

    fn edit(start: usize, removed: &str, inserted: &str) -> crate::Edit {
        crate::Edit {
            start,
            removed: crate::Content::from(removed),
            inserted: crate::Content::from(inserted),
        }
    }

    #[test]
    fn mapping_through_typing_moves_each_cursor_past_its_text() {
        // "ab|cd|ef" with cursors at 2 and 4; "X" typed at both. Edits are
        // last-first with offsets in the old text.
        let mut s = Selections::from_vec(vec![Selection::cursor(2), Selection::cursor(4)], 1);
        let edits = [edit(4, "", "X"), edit(2, "", "X")];
        s.map_through(&edits, Bias::After, Bias::After);
        assert_eq!(s.as_slice(), &[Selection::cursor(3), Selection::cursor(6)]);
        assert_eq!(s.primary_index(), 1);
    }

    #[test]
    fn mapping_through_replaced_selections_collapses_after_the_new_text() {
        // Two touching selections "abc" and "de" in "abcdef", both replaced.
        let mut s = Selections::from_vec(vec![sel(0, 3), sel(3, 5)], 0);
        let edits = [edit(3, "de", "Y"), edit(0, "abc", "XX")];
        s.map_through(&edits, Bias::After, Bias::After);
        assert_eq!(s.as_slice(), &[Selection::cursor(2), Selection::cursor(3)]);
    }

    #[test]
    fn mapping_through_backspace_and_indent() {
        // Backspace at 3 and 7: each removes the byte before.
        let mut s = Selections::from_vec(vec![Selection::cursor(3), Selection::cursor(7)], 0);
        let edits = [edit(6, "x", ""), edit(2, "x", "")];
        s.map_through(&edits, Bias::After, Bias::After);
        assert_eq!(s.as_slice(), &[Selection::cursor(2), Selection::cursor(5)]);

        // Indent: a tab at the start of lines 0 and 4; a selection from 0
        // to 6 keeps the first tab inside it, a cursor at 9 moves by two.
        let mut s = Selections::from_vec(vec![sel(0, 6), Selection::cursor(9)], 0);
        let edits = [edit(4, "", "\t"), edit(0, "", "\t")];
        s.map_through(&edits, Bias::Before, Bias::After);
        assert_eq!(s.as_slice(), &[sel(0, 8), Selection::cursor(11)]);
    }

    #[test]
    fn positions_away_from_the_edits_only_shift() {
        let mut s = Selections::from_vec(vec![sel(1, 2), sel(10, 12)], 0);
        let edits = [edit(5, "abc", "")];
        s.map_through(&edits, Bias::After, Bias::After);
        assert_eq!(s.as_slice(), &[sel(1, 2), sel(7, 9)]);
    }
}
