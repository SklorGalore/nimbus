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

/// All selections in a document. Always at least one; M2 to M7 use exactly
/// one, and M8 adds more for multi-cursor editing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selections {
    items: Vec<Selection>,
}

impl Default for Selections {
    fn default() -> Self {
        Self::single(Selection::default())
    }
}

impl Selections {
    /// Exactly one selection.
    #[must_use]
    pub fn single(sel: Selection) -> Self {
        Self { items: vec![sel] }
    }

    /// The main selection: the one the view follows and the IME attaches to.
    #[must_use]
    pub fn primary(&self) -> Selection {
        // `items` is never empty: every constructor puts one in.
        self.items.first().copied().unwrap_or_default()
    }

    /// Replace the main selection.
    pub fn set_primary(&mut self, sel: Selection) {
        if let Some(first) = self.items.first_mut() {
            *first = sel;
        } else {
            self.items.push(sel);
        }
    }

    /// All selections.
    pub fn iter(&self) -> impl Iterator<Item = &Selection> {
        self.items.iter()
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

    #[test]
    fn selections_always_have_a_primary() {
        let mut sels = Selections::default();
        assert_eq!(sels.primary(), Selection::cursor(0));
        sels.set_primary(Selection::cursor(4));
        assert_eq!(sels.primary().head, 4);
        assert_eq!(sels.iter().count(), 1);
    }
}
