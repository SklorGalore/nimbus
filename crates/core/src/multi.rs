//! Editing at several cursors at once (M8).
//!
//! An operation such as typing a character is worked out at each cursor
//! separately: a range to replace and its new text. Applying those one by
//! one would move every later cursor's offsets after each edit. Instead
//! [`apply_each`] applies them all in one [`Buffer::replace_sorted`], which
//! works from the end of the text back to the start, so no offset moves
//! before it is used. The result is one list of edits that undo puts back
//! in one pass, that [`crate::report_sorted`] tells the syntax tree about,
//! and that [`crate::Selections::map_through`] moves the cursors through.

use std::borrow::Cow;
use std::ops::Range;

use crate::buffer::{Buffer, Edit, EditError};

/// Apply `items`, one replacement per cursor in any order, as a single
/// batch. Returns the edits in [`Buffer::replace_sorted`] order (last in
/// the text first, offsets in the text before any of them).
///
/// Before applying, the items are sorted, ones that change nothing are
/// dropped, and deletions that overlap are merged into one: two cursors
/// deleting words backwards can reach the same start. `items` is left
/// sorted and merged, so a caller can reuse the vector.
///
/// # Errors
///
/// [`EditError::Unsorted`] if two items that insert text overlap, which
/// would mean the cursors themselves overlapped; and the
/// [`Buffer::replace_sorted`] errors. On error nothing has changed.
pub fn apply_each(
    buffer: &mut dyn Buffer,
    items: &mut Vec<(Range<usize>, Cow<'_, str>)>,
) -> Result<Vec<Edit>, EditError> {
    items.retain(|(r, t)| !(r.is_empty() && t.is_empty()));
    items.sort_by_key(|(r, _)| (r.start, r.end));
    let mut w = 0;
    for i in 1..items.len() {
        let (next, prev) = (&items[i], &items[w]);
        if next.0.start < prev.0.end {
            if !next.1.is_empty() || !prev.1.is_empty() {
                return Err(EditError::Unsorted { index: i });
            }
            let end = prev.0.end.max(next.0.end);
            items[w].0.end = end;
        } else {
            w += 1;
            items.swap(w, i);
        }
    }
    if !items.is_empty() {
        items.truncate(w + 1);
    }
    let refs: Vec<(Range<usize>, &str)> = items.iter().map(|(r, t)| (r.clone(), &**t)).collect();
    buffer.replace_sorted(&refs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RopeBuffer, Text};

    fn text(b: &RopeBuffer) -> String {
        b.slice(0..b.len_bytes()).into_owned()
    }

    #[test]
    fn typing_at_three_cursors_is_one_descending_batch() {
        let mut b = RopeBuffer::from_text("ab\ncd\nef");
        let mut items: Vec<(Range<usize>, Cow<'_, str>)> = [6, 0, 3]
            .into_iter()
            .map(|p| (p..p, Cow::Borrowed("X")))
            .collect();
        let edits = apply_each(&mut b, &mut items).unwrap();
        assert_eq!(text(&b), "Xab\nXcd\nXef");
        let starts: Vec<usize> = edits.iter().map(|e| e.start).collect();
        assert_eq!(starts, [6, 3, 0], "last first, offsets before the batch");
    }

    #[test]
    fn overlapping_deletions_merge_and_no_ops_drop() {
        let mut b = RopeBuffer::from_text("one two three");
        let mut items = vec![
            (4..8, Cow::Borrowed("")),
            (6..9, Cow::Borrowed("")),
            (12..12, Cow::Borrowed("")),
        ];
        let edits = apply_each(&mut b, &mut items).unwrap();
        assert_eq!(text(&b), "one hree");
        assert_eq!(edits.len(), 1);
        assert_eq!(items.len(), 1);

        // Overlapping inserts are a caller bug: refused, nothing changed.
        let mut items = vec![(0..3, Cow::Borrowed("a")), (2..4, Cow::Borrowed("b"))];
        assert!(matches!(
            apply_each(&mut b, &mut items),
            Err(EditError::Unsorted { .. })
        ));
        assert_eq!(text(&b), "one hree");

        let mut items: Vec<(Range<usize>, Cow<'_, str>)> = Vec::new();
        assert_eq!(apply_each(&mut b, &mut items).unwrap(), Vec::new());
    }
}
