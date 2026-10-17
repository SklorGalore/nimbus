//! Line operations: duplicate, move, delete, join, insert a line, select
//! a line (M11).
//!
//! Each operation is a pure function of the text and the selections: it
//! returns a [`Batch`], the replacements for one
//! [`crate::Buffer::replace_sorted`] call plus the selections afterwards,
//! the shape [`crate::multi`] established in M8. Working out every
//! replacement first and applying them in one pass keeps the operation a
//! single undo step, keeps every cursor's offsets valid (the batch applies
//! from the end of the text back), and costs the mapped buffer one piece
//! rebuild instead of one per cursor.
//!
//! Nothing here reads more of the text than the lines the selections
//! touch. A line is found with [`Text::line_span`] and the lines of a
//! selection by walking `next` from the first, so a mapped file whose line
//! numbers are not all counted yet still works. Text is read with
//! [`Text::slice`], which borrows from a rope and copies from a mapped
//! file; the caller bounds how much by refusing selections over
//! [`Batch::check_size`]'s limit (64 MB in the app), so the worst case is
//! bounded by the limit, not by the file.

use std::borrow::Cow;
use std::ops::Range;

use crate::long_line::NEAR;
use crate::{Selection, Selections, Text};

/// The result of an operation: replacements for one
/// [`crate::Buffer::replace_sorted`] call (sorted, not overlapping, ranges
/// in the text before any of them), and the selections afterwards.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Batch<'a> {
    /// The replacements, in text order.
    pub items: Vec<(Range<usize>, Cow<'a, str>)>,
    /// The selections once the items are applied, as offsets into the new
    /// text. `None` means: move the current selections through the edits
    /// ([`Selections::map_through`]), which suits operations that only
    /// insert or delete around the cursors.
    pub after: Option<Selections>,
}

/// Why an operation did not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineError {
    /// The selected lines hold more bytes than the operation's limit.
    TooLarge,
}

impl std::fmt::Display for LineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => f.write_str("selection too large for this operation"),
        }
    }
}

impl std::error::Error for LineError {}

/// The lines touched by one or more selections, as bytes: from the start
/// of the first line to the end of the last line's content, and where the
/// line after starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    /// Start of the first line.
    pub start: usize,
    /// End of the last line's content, before its line ending.
    pub end: usize,
    /// Start of the line after the block; `None` after the last line.
    pub next: Option<usize>,
}

impl Block {
    /// The lines `range` touches. A range ending at the very start of a
    /// line (a selection made by Select Line) does not include that line.
    #[must_use]
    pub fn of(t: &dyn Text, range: Range<usize>) -> Self {
        let first = t.line_span(range.start);
        let mut last = first;
        while let Some(n) = last.next {
            if n >= range.end || (n == range.end && range.start < range.end) {
                break;
            }
            last = t.line_span(n);
        }
        Self {
            start: first.start,
            end: last.end,
            next: last.next,
        }
    }

    /// The bytes of the lines, line endings between them included.
    #[must_use]
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    /// Whether the block holds no text at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.end == self.start
    }
}

/// The blocks the selections touch, in order, each with the selections in
/// it (as indices into `sels`). Selections on the same lines share a
/// block; with `merge_adjacent`, blocks on consecutive lines are joined
/// too, for operations that would otherwise move one block into another.
#[must_use]
pub fn blocks(t: &dyn Text, sels: &Selections, merge_adjacent: bool) -> Vec<(Block, Vec<usize>)> {
    let mut out: Vec<(Block, Vec<usize>)> = Vec::new();
    for (i, s) in sels.iter().enumerate() {
        let b = Block::of(t, s.range());
        if let Some((last, members)) = out.last_mut() {
            let touches = b.start <= last.end || (merge_adjacent && Some(b.start) == last.next);
            if touches {
                if b.end > last.end {
                    last.end = b.end;
                    last.next = b.next;
                }
                members.push(i);
                continue;
            }
        }
        out.push((b, vec![i]));
    }
    out
}

impl Batch<'_> {
    /// The batch with its text copied out of the buffer it was read from,
    /// so the buffer can be edited with it. The copies are bounded by the
    /// operation's limit ([`Batch::check_size`]).
    #[must_use]
    pub fn into_owned(self) -> Batch<'static> {
        Batch {
            items: self
                .items
                .into_iter()
                .map(|(r, t)| (r, Cow::Owned(t.into_owned())))
                .collect(),
            after: self.after,
        }
    }

    /// Refuse the operation when the selections' lines hold more than
    /// `limit` bytes together: the app passes 64 MB, the M8 read limit, so a
    /// line operation on a huge file's selection stays within the typing
    /// budget or says why it did not run.
    ///
    /// # Errors
    ///
    /// [`LineError::TooLarge`] over the limit.
    pub fn check_size(t: &dyn Text, sels: &Selections, limit: usize) -> Result<(), LineError> {
        let total: usize = blocks(t, sels, false).iter().map(|(b, _)| b.len()).sum();
        if total > limit {
            Err(LineError::TooLarge)
        } else {
            Ok(())
        }
    }
}

/// The leading whitespace of the line starting at `start`, read from at
/// most [`NEAR`] bytes of it.
pub(crate) fn indentation(t: &dyn Text, start: usize, end: usize) -> Cow<'_, str> {
    let head = t.slice(start..end.min(start + NEAR));
    let n = head.len() - head.trim_start_matches([' ', '\t']).len();
    match head {
        Cow::Borrowed(s) => Cow::Borrowed(&s[..n]),
        Cow::Owned(mut s) => {
            s.truncate(n);
            Cow::Owned(s)
        }
    }
}

/// Duplicate: a selection's text is inserted again after it; a bare
/// cursor's lines are inserted again below them. The cursors end up on the
/// copy, so pressing again makes another.
#[must_use]
pub fn duplicate<'t>(t: &'t dyn Text, sels: &Selections, ending: &str) -> Batch<'t> {
    let mut items: Vec<(Range<usize>, Cow<'t, str>)> = Vec::new();
    // Lines are duplicated once, however many bare cursors sit on them.
    let mut done_block: Option<Block> = None;
    for s in sels.iter() {
        let r = s.range();
        if r.is_empty() {
            let b = Block::of(t, r);
            if done_block == Some(b) {
                continue;
            }
            done_block = Some(b);
            let mut copy = t.slice(b.start..b.end).into_owned();
            copy.push_str(ending);
            items.push((b.start..b.start, Cow::Owned(copy)));
        } else {
            items.push((r.end..r.end, t.slice(r.clone())));
        }
    }
    items.sort_by_key(|(r, _)| r.start);
    // Each cursor moves past what was inserted before it, which is the
    // copy of its own lines or of its own text.
    let mut after = Vec::new();
    let mut grown = 0usize;
    let mut counted: Option<Block> = None;
    for s in sels.iter() {
        let r = s.range();
        if r.is_empty() {
            let b = Block::of(t, r.clone());
            if counted != Some(b) {
                counted = Some(b);
                grown += b.len() + ending.len();
            }
            after.push(Selection::cursor(r.start + grown));
        } else {
            let start = r.end + grown;
            grown += r.len();
            after.push(directed(start..r.end + grown, *s));
        }
    }
    Batch {
        items,
        after: Some(Selections::from_vec(after, sels.primary_index())),
    }
}

/// `range` as a selection pointing the way `like` does.
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

/// Move the selected lines one line up (`down == false`) or down. The
/// selections move with their lines. A block already at the edge of the
/// text stays.
#[must_use]
pub fn move_lines<'t>(t: &'t dyn Text, sels: &Selections, down: bool) -> Batch<'t> {
    let groups = blocks(t, sels, true);
    let mut items: Vec<(Range<usize>, Cow<'t, str>)> = Vec::new();
    // By how much each selection's offsets move: the swapped line and the
    // line ending between.
    let mut moved: Vec<isize> = vec![0; sels.count()];
    for (b, members) in &groups {
        let block_text = t.slice(b.start..b.end);
        if down {
            let Some(next) = b.next else { continue };
            let line = t.line_span(next);
            let sep = t.slice(b.end..next);
            let mut text =
                String::with_capacity(line.end - line.start + sep.len() + block_text.len());
            text.push_str(&t.slice(line.start..line.end));
            text.push_str(&sep);
            text.push_str(&block_text);
            let by = (line.end - line.start + sep.len()).cast_signed();
            for &i in members {
                moved[i] = by;
            }
            items.push((b.start..line.end, Cow::Owned(text)));
        } else {
            if b.start == 0 {
                continue;
            }
            let prev = t.line_span(b.start - 1);
            let sep = t.slice(prev.end..b.start);
            let mut text =
                String::with_capacity(prev.end - prev.start + sep.len() + block_text.len());
            text.push_str(&block_text);
            text.push_str(&sep);
            text.push_str(&t.slice(prev.start..prev.end));
            let by = -(prev.end - prev.start + sep.len()).cast_signed();
            for &i in members {
                moved[i] = by;
            }
            items.push((prev.start..b.end, Cow::Owned(text)));
        }
    }
    // Every replacement keeps the length, so nothing else shifts.
    let after = sels
        .iter()
        .zip(&moved)
        .map(|(s, &by)| Selection {
            anchor: s.anchor.saturating_add_signed(by),
            head: s.head.saturating_add_signed(by),
        })
        .collect();
    Batch {
        items,
        after: Some(Selections::from_vec(after, sels.primary_index())),
    }
}

/// Delete the selected lines whole, line endings included. Each cursor
/// lands on the line that takes its place, keeping its column where it
/// can.
#[must_use]
pub fn delete_lines<'t>(t: &'t dyn Text, sels: &Selections) -> Batch<'t> {
    let groups = blocks(t, sels, false);
    let mut items: Vec<(Range<usize>, Cow<'t, str>)> = Vec::new();
    let mut after = Vec::new();
    let mut removed = 0usize;
    for (b, members) in &groups {
        // Delete through the line ending; on the last line, through the
        // ending before it instead, so no empty line is left behind.
        let range = match b.next {
            Some(n) => b.start..n,
            None if b.start > 0 => t.line_span(b.start - 1).end..b.end,
            None => b.start..b.end,
        };
        // The line that moves up into the gap, for keeping columns.
        let landing_len = b.next.map_or(0, |n| {
            let span = t.line_span(n);
            span.end - span.start
        });
        for &i in members {
            let s = sels.as_slice()[i];
            let line_start = t.line_span(s.head).start;
            let col = (s.head - line_start).min(landing_len);
            let at = range.start - removed + if b.next.is_some() { col } else { 0 };
            after.push(Selection::cursor(at));
        }
        removed += range.len();
        items.push((range, Cow::Borrowed("")));
    }
    Batch {
        items,
        after: Some(Selections::from_vec(after, sels.primary_index())),
    }
}

/// Join each selection's lines into one: a cursor joins its line with the
/// next; a selection spanning lines joins them all. The line break and the
/// whitespace around it become one space, or nothing when either side is
/// empty.
#[must_use]
pub fn join_lines<'t>(t: &'t dyn Text, sels: &Selections) -> Batch<'t> {
    let mut items: Vec<(Range<usize>, Cow<'t, str>)> = Vec::new();
    let mut after = Vec::new();
    // How much the text before the current point has shrunk.
    let mut delta = 0usize;
    for s in sels.iter() {
        let r = s.range();
        let b = Block::of(t, r.clone());
        let mut span = t.line_span(b.start);
        // A cursor or one-line selection joins with the line below.
        let last_end = if span.end == b.end {
            b.next.map_or(b.end, |n| t.line_span(n).end)
        } else {
            b.end
        };
        let delta_before = delta;
        let mut first_join = None;
        let mut last_new_end = None;
        while span.end < last_end {
            let Some(next) = span.next else { break };
            let next_span = t.line_span(next);
            // Only the ends of the two lines matter, so only they are read.
            let tail = t.slice(span.end.saturating_sub(NEAR).max(span.start)..span.end);
            let trimmed_tail = tail.trim_end_matches([' ', '\t']).len();
            let from = span.end - (tail.len() - trimmed_tail);
            let head = t.slice(next_span.start..next_span.end.min(next_span.start + NEAR));
            let lead = head.len() - head.trim_start_matches([' ', '\t']).len();
            let to = next_span.start + lead;
            let left_empty = from == span.start;
            let right_empty = to == next_span.end;
            let glue = if left_empty || right_empty { "" } else { " " };
            if items.last().is_none_or(|(prev, _)| prev.end <= from) {
                first_join.get_or_insert(from - delta);
                delta += to - from - glue.len();
                last_new_end = Some(from - delta_before + glue.len());
                items.push((from..to, Cow::Borrowed(glue)));
            }
            span = next_span;
        }
        if r.is_empty() {
            // The cursor goes to the join, before the space.
            after.push(Selection::cursor(
                first_join.unwrap_or_else(|| r.start.saturating_sub(delta)),
            ));
        } else {
            let start =
                first_join.map_or(r.start - delta_before, |j| j.min(r.start - delta_before));
            let end = (r.end - delta).max(last_new_end.unwrap_or(0)).max(start);
            after.push(directed(start..end, *s));
        }
    }
    Batch {
        items,
        after: Some(Selections::from_vec(after, sels.primary_index())),
    }
}

/// Open a new line below (`above == false`) or above each cursor's line,
/// indented like it, without breaking the line the cursor is on. The
/// cursors move to the new lines.
#[must_use]
pub fn insert_line<'t>(t: &'t dyn Text, sels: &Selections, ending: &str, above: bool) -> Batch<'t> {
    let mut items: Vec<(Range<usize>, Cow<'t, str>)> = Vec::new();
    let mut after = Vec::new();
    let mut last_line: Option<usize> = None;
    let mut grown = 0usize;
    for s in sels.iter() {
        let span = t.line_span(s.head);
        let indent = indentation(t, span.start, span.end);
        if last_line == Some(span.start) {
            // A second cursor on the line shares the new line.
            if let Some(prev) = after.last() {
                after.push(*prev);
            }
            continue;
        }
        last_line = Some(span.start);
        let (at, text) = if above {
            (span.start, format!("{indent}{ending}"))
        } else {
            (span.end, format!("{ending}{indent}"))
        };
        let cursor = at + grown + if above { indent.len() } else { text.len() };
        grown += text.len();
        after.push(Selection::cursor(cursor));
        items.push((at..at, Cow::Owned(text)));
    }
    Batch {
        items,
        after: Some(Selections::from_vec(after, sels.primary_index())),
    }
}

/// Select each selection's lines whole, line endings included; a selection
/// that already covers whole lines grows by the line below.
#[must_use]
pub fn select_lines(t: &dyn Text, sels: &Selections) -> Selections {
    let mut out = Vec::new();
    for s in sels.iter() {
        let r = s.range();
        let b = Block::of(t, r.clone());
        let end = b.next.unwrap_or(b.end);
        let whole = !r.is_empty() && r.start == b.start && r.end == end;
        let end = if whole {
            b.next
                .map_or(end, |n| t.line_span(n).next.unwrap_or(t.len_bytes()))
        } else {
            end
        };
        out.push(Selection {
            anchor: b.start,
            head: end,
        });
    }
    Selections::from_vec(out, sels.primary_index())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RopeBuffer, multi};

    fn sels(at: &[(usize, usize)]) -> Selections {
        Selections::from_vec(
            at.iter()
                .map(|&(a, h)| Selection { anchor: a, head: h })
                .collect(),
            at.len() - 1,
        )
    }

    fn cursors(at: &[usize]) -> Selections {
        let v: Vec<(usize, usize)> = at.iter().map(|&p| (p, p)).collect();
        sels(&v)
    }

    /// Apply `batch` to `text`; returns the new text and the selections.
    fn apply(text: &str, before: &Selections, batch: Batch<'_>) -> (String, Selections) {
        let mut b = RopeBuffer::from_text(text);
        let mut items = batch.items;
        let edits = multi::apply_each(&mut b, &mut items).unwrap();
        let after = batch.after.unwrap_or_else(|| {
            let mut s = before.clone();
            s.map_through(&edits, crate::Bias::After, crate::Bias::After);
            s
        });
        (b.slice(0..b.len_bytes()).into_owned(), after)
    }

    fn heads(s: &Selections) -> Vec<usize> {
        s.iter().map(|s| s.head).collect()
    }

    #[test]
    fn blocks_cover_touched_lines_and_merge() {
        let t = RopeBuffer::from_text("aa\nbb\ncc\ndd");
        assert_eq!(
            Block::of(&t, 1..1),
            Block {
                start: 0,
                end: 2,
                next: Some(3)
            }
        );
        assert_eq!(
            Block::of(&t, 1..4),
            Block {
                start: 0,
                end: 5,
                next: Some(6)
            }
        );
        // A selection ending at a line start leaves that line out.
        assert_eq!(
            Block::of(&t, 0..3),
            Block {
                start: 0,
                end: 2,
                next: Some(3)
            }
        );
        assert_eq!(
            Block::of(&t, 10..11),
            Block {
                start: 9,
                end: 11,
                next: None
            }
        );
        let g = blocks(&t, &cursors(&[0, 1, 4, 9]), false);
        assert_eq!(g.len(), 3);
        assert_eq!(g[0].1, vec![0, 1]);
        let g = blocks(&t, &cursors(&[0, 4, 9]), true);
        assert_eq!(g.len(), 2, "adjacent lines merge: {g:?}");
        assert_eq!(
            g[0].0,
            Block {
                start: 0,
                end: 5,
                next: Some(6)
            }
        );
    }

    #[test]
    fn size_limit_counts_the_selected_lines() {
        let t = RopeBuffer::from_text("short\nlonger line\nx");
        assert_eq!(Batch::check_size(&t, &cursors(&[0]), 5), Ok(()));
        assert_eq!(
            Batch::check_size(&t, &cursors(&[7]), 5),
            Err(LineError::TooLarge)
        );
        assert_eq!(
            LineError::TooLarge.to_string(),
            "selection too large for this operation"
        );
    }

    #[test]
    fn duplicate_lines_and_selections_with_one_and_three_cursors() {
        let text = "one\ntwo\nthree";
        let (out, s) = apply(
            text,
            &cursors(&[1]),
            duplicate(&RopeBuffer::from_text(text), &cursors(&[1]), "\n"),
        );
        assert_eq!(out, "one\none\ntwo\nthree");
        assert_eq!(heads(&s), [5], "on the copy, same column");
        let before = cursors(&[1, 5, 9]);
        let (out, s) = apply(
            text,
            &before,
            duplicate(&RopeBuffer::from_text(text), &before, "\n"),
        );
        assert_eq!(out, "one\none\ntwo\ntwo\nthree\nthree");
        assert_eq!(heads(&s), [5, 13, 23]);
        // A selection duplicates its text; the copy is selected.
        let before = sels(&[(0, 3)]);
        let (out, s) = apply(
            text,
            &before,
            duplicate(&RopeBuffer::from_text(text), &before, "\n"),
        );
        assert_eq!(out, "oneone\ntwo\nthree");
        assert_eq!(s.primary(), Selection { anchor: 3, head: 6 });
        // Two cursors on one line duplicate it once; last line without
        // a line ending.
        let before = cursors(&[8, 10]);
        let (out, s) = apply(
            text,
            &before,
            duplicate(&RopeBuffer::from_text(text), &before, "\n"),
        );
        assert_eq!(out, "one\ntwo\nthree\nthree");
        assert_eq!(heads(&s), [14, 16]);
    }

    #[test]
    fn move_lines_up_and_down_with_one_and_three_cursors() {
        let text = "a\nbb\nccc\ndddd";
        let t = RopeBuffer::from_text(text);
        let before = cursors(&[3]);
        let (out, s) = apply(text, &before, move_lines(&t, &before, true));
        assert_eq!(out, "a\nccc\nbb\ndddd");
        assert_eq!(heads(&s), [7]);
        let (out, s) = apply(text, &before, move_lines(&t, &before, false));
        assert_eq!(out, "bb\na\nccc\ndddd");
        assert_eq!(heads(&s), [1]);
        // Three cursors on three lines: the first two are adjacent and move
        // as one block; the last cannot move down.
        let before = cursors(&[0, 3, 10]);
        let (out, s) = apply(text, &before, move_lines(&t, &before, true));
        assert_eq!(out, "ccc\na\nbb\ndddd");
        assert_eq!(heads(&s), [4, 7, 10]);
        // At the top nothing moves.
        let before = cursors(&[0]);
        let b = move_lines(&t, &before, false);
        assert!(b.items.is_empty());
        // A selection across lines moves them all and stays selected.
        let before = sels(&[(2, 6)]);
        let (out, s) = apply(text, &before, move_lines(&t, &before, true));
        assert_eq!(out, "a\ndddd\nbb\nccc");
        assert_eq!(
            s.primary(),
            Selection {
                anchor: 7,
                head: 11
            }
        );
        // CRLF endings travel with their lines.
        let text = "a\r\nb\r\nc";
        let t = RopeBuffer::from_text(text);
        let before = cursors(&[0]);
        let (out, _) = apply(text, &before, move_lines(&t, &before, true));
        assert_eq!(out, "b\r\na\r\nc");
    }

    #[test]
    fn delete_lines_with_one_and_three_cursors() {
        let text = "one\ntwo\nthree\nfour";
        let t = RopeBuffer::from_text(text);
        let before = cursors(&[5]);
        let (out, s) = apply(text, &before, delete_lines(&t, &before));
        assert_eq!(out, "one\nthree\nfour");
        assert_eq!(heads(&s), [5], "same column on the line that moved up");
        let before = cursors(&[1, 6, 15]);
        let (out, s) = apply(text, &before, delete_lines(&t, &before));
        assert_eq!(out, "three");
        assert_eq!(heads(&s), [1, 2, 5]);
        // Deleting the last line takes the line ending before it.
        let before = cursors(&[16]);
        let (out, s) = apply(text, &before, delete_lines(&t, &before));
        assert_eq!(out, "one\ntwo\nthree");
        assert_eq!(heads(&s), [13]);
    }

    #[test]
    fn join_lines_with_one_and_three_cursors() {
        let text = "a  \n   b\nc\n\nd\ne";
        let t = RopeBuffer::from_text(text);
        let before = cursors(&[0]);
        let (out, s) = apply(text, &before, join_lines(&t, &before));
        assert_eq!(out, "a b\nc\n\nd\ne");
        assert_eq!(heads(&s), [1], "at the join, before the space");
        // Three cursors: a, c and the empty line; joining with an empty
        // line adds no space.
        let before = cursors(&[0, 9, 11]);
        let (out, s) = apply(text, &before, join_lines(&t, &before));
        assert_eq!(out, "a b\ncd\ne");
        assert_eq!(
            heads(&s),
            [1, 5],
            "the last two joins meet, so their cursors merge"
        );
        // A selection over three lines joins them all and stays selected.
        let before = sels(&[(9, 13)]);
        let (out, s) = apply(text, &before, join_lines(&t, &before));
        assert_eq!(out, "a  \n   b\ncd\ne");
        assert_eq!(
            s.primary(),
            Selection {
                anchor: 9,
                head: 11
            }
        );
    }

    #[test]
    fn insert_line_below_and_above_keeps_indentation() {
        let text = "  one\n  two";
        let t = RopeBuffer::from_text(text);
        let before = cursors(&[3]);
        let (out, s) = apply(text, &before, insert_line(&t, &before, "\n", false));
        assert_eq!(out, "  one\n  \n  two");
        assert_eq!(heads(&s), [8]);
        let (out, s) = apply(text, &before, insert_line(&t, &before, "\n", true));
        assert_eq!(out, "  \n  one\n  two");
        assert_eq!(heads(&s), [2]);
        // Three cursors, two on one line: that line gets one new line.
        let before = cursors(&[2, 4, 8]);
        let (out, s) = apply(text, &before, insert_line(&t, &before, "\n", false));
        assert_eq!(out, "  one\n  \n  two\n  ");
        assert_eq!(heads(&s), [8, 17]);
    }

    #[test]
    fn select_line_takes_the_line_and_grows_on_repeat() {
        let t = RopeBuffer::from_text("aa\nbb\ncc");
        let s = select_lines(&t, &cursors(&[1]));
        assert_eq!(s.primary(), Selection { anchor: 0, head: 3 });
        let s = select_lines(&t, &s);
        assert_eq!(s.primary(), Selection { anchor: 0, head: 6 });
        let s = select_lines(&t, &s);
        assert_eq!(s.primary(), Selection { anchor: 0, head: 8 });
        let s = select_lines(&t, &s);
        assert_eq!(
            s.primary(),
            Selection { anchor: 0, head: 8 },
            "stops at the end"
        );
        let s = select_lines(&t, &cursors(&[1, 4, 7]));
        assert_eq!(s.count(), 3);
        assert_eq!(s.as_slice()[2], Selection { anchor: 6, head: 8 });
    }
}
