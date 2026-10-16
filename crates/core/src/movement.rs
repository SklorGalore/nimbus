//! Cursor movement as pure functions of the text and a byte offset.
//!
//! Left and right move by *grapheme*, so `e` plus a combining accent, an
//! emoji with a skin tone, or a flag is one step, and a `\r\n` line ending is
//! one step too. Up and down move by line and aim at a *preferred column*
//! that survives passing through shorter lines (the "sticky column").
//!
//! The functions take [`Text`], not a buffer, so they work on snapshots too.

use std::ops::Range;

use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

use crate::Text;
use crate::column;

/// Byte offset where the content of line `l` ends, before its line ending.
fn content_end(t: &dyn Text, l: usize) -> usize {
    t.line_to_byte(l) + t.line(l).len()
}

/// Snap any offset into the text, onto a char boundary, and out of the
/// middle of a `\r\n`.
#[must_use]
pub fn clamp(t: &dyn Text, pos: usize) -> usize {
    let pos = pos.min(t.len_bytes());
    let l = t.byte_to_line(pos);
    let start = t.line_to_byte(l);
    let line = t.line(l);
    start + line.floor_char_boundary((pos - start).min(line.len()))
}

/// The next cursor stop to the right of `pos`.
#[must_use]
pub fn next_grapheme(t: &dyn Text, pos: usize) -> usize {
    let l = t.byte_to_line(pos);
    let start = t.line_to_byte(l);
    let line = t.line(l);
    let off = pos.saturating_sub(start);
    if off >= line.len() {
        // At the end of the content: step over the whole line ending. On
        // the last line this is the end of the text.
        return t.line_to_byte(l + 1);
    }
    // GraphemeCursor panics off a char boundary, so floor first.
    let mut c = GraphemeCursor::new(line.floor_char_boundary(off), line.len(), true);
    match c.next_boundary(&line, 0) {
        Ok(Some(b)) => start + b,
        Ok(None) | Err(_) => start + line.len(),
    }
}

/// The next cursor stop to the left of `pos`.
#[must_use]
pub fn prev_grapheme(t: &dyn Text, pos: usize) -> usize {
    let l = t.byte_to_line(pos);
    let start = t.line_to_byte(l);
    if pos <= start {
        return if l == 0 { 0 } else { content_end(t, l - 1) };
    }
    let line = t.line(l);
    let off = pos - start;
    if off > line.len() {
        // Between the \r and \n of a line ending.
        return start + line.len();
    }
    let mut c = GraphemeCursor::new(line.floor_char_boundary(off), line.len(), true);
    match c.prev_boundary(&line, 0) {
        Ok(Some(b)) => start + b,
        Ok(None) | Err(_) => start,
    }
}

/// Start of the line containing `pos`.
#[must_use]
pub fn line_start(t: &dyn Text, pos: usize) -> usize {
    t.line_to_byte(t.byte_to_line(pos))
}

/// End of the content of the line containing `pos`, before its line ending.
#[must_use]
pub fn line_end(t: &dyn Text, pos: usize) -> usize {
    content_end(t, t.byte_to_line(pos))
}

/// Character classes for word movement. Code-editor style: `.`, `(` and
/// friends split words, unlike the Unicode word rules for prose, which keep
/// `bar.baz` together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Space,
    Punct,
}

fn class(grapheme: &str) -> Class {
    match grapheme.chars().next() {
        Some(c) if c.is_alphanumeric() || c == '_' => Class::Word,
        Some(c) if c.is_whitespace() => Class::Space,
        _ => Class::Punct,
    }
}

/// Runs of graphemes of one class, as `(start, end, class)` byte offsets.
/// A plain iterator: word movement runs per keystroke and must not allocate.
fn segments(line: &str) -> impl Iterator<Item = (usize, usize, Class)> + '_ {
    let mut graphemes = line.grapheme_indices(true).peekable();
    std::iter::from_fn(move || {
        let (start, g) = graphemes.next()?;
        let c = class(g);
        let mut end = start + g.len();
        while let Some(&(i, next)) = graphemes.peek() {
            if class(next) != c {
                break;
            }
            end = i + next.len();
            graphemes.next();
        }
        Some((start, end, c))
    })
}

/// Start of the previous word, as Alt+Left (macOS) or Ctrl+Left does. At
/// the start of a line, moves to the end of the previous line.
#[must_use]
pub fn word_left(t: &dyn Text, pos: usize) -> usize {
    let l = t.byte_to_line(pos);
    let start = t.line_to_byte(l);
    if pos <= start {
        return prev_grapheme(t, pos);
    }
    let line = t.line(l);
    let off = (pos - start).min(line.len());
    segments(&line)
        .take_while(|&(i, _, _)| i < off)
        .filter(|&(_, _, c)| c != Class::Space)
        .last()
        .map_or(start, |(i, _, _)| start + i)
}

/// End of the next word, as Alt+Right (macOS) or Ctrl+Right does. At the
/// end of a line, moves to the start of the next line.
#[must_use]
pub fn word_right(t: &dyn Text, pos: usize) -> usize {
    let l = t.byte_to_line(pos);
    let start = t.line_to_byte(l);
    let line = t.line(l);
    let off = pos.saturating_sub(start);
    if off >= line.len() {
        return next_grapheme(t, pos);
    }
    segments(&line)
        .find(|&(_, end, c)| end > off && c != Class::Space)
        .map_or(start + line.len(), |(_, end, _)| start + end)
}

/// The word, run of spaces, or run of punctuation under `pos`, for a
/// double-click. At the end of a line it is the run before the position.
#[must_use]
pub fn word_at(t: &dyn Text, pos: usize) -> Range<usize> {
    let l = t.byte_to_line(pos);
    let start = t.line_to_byte(l);
    let line = t.line(l);
    let off = pos.saturating_sub(start).min(line.len());
    let mut before = None;
    for (i, end, _) in segments(&line) {
        if i <= off && off < end {
            return start + i..start + end;
        }
        if end == off {
            before = Some(start + i..start + end);
        }
    }
    before.unwrap_or(pos..pos)
}

/// The whole line containing `pos`, including its line ending, for a
/// triple-click.
#[must_use]
pub fn line_range(t: &dyn Text, pos: usize) -> Range<usize> {
    let l = t.byte_to_line(pos);
    t.line_to_byte(l)..t.line_to_byte(l + 1)
}

/// Home: the first non-blank character of the line, or column 0 if the
/// cursor is already there (so pressing Home twice toggles).
#[must_use]
pub fn smart_home(t: &dyn Text, pos: usize) -> usize {
    let start = line_start(t, pos);
    let line = t.line(t.byte_to_line(pos));
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let first = start + indent;
    if pos == first { start } else { first }
}

/// Display column of `pos` on its line.
#[must_use]
pub fn column_of(t: &dyn Text, pos: usize, tab_width: usize) -> usize {
    let l = t.byte_to_line(pos);
    column::col_at(&t.line(l), pos - t.line_to_byte(l), tab_width)
}

/// The result of a vertical move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vertical {
    /// The new cursor offset.
    pub pos: usize,
    /// The column to keep aiming for on the next vertical move.
    pub preferred_col: usize,
}

/// Move `lines` lines up (negative) or down (positive).
///
/// `preferred` is the sticky column from the previous vertical move, or
/// `None` after any other kind of move. Moving up from the first line goes
/// to the start of the text and moving down from the last line goes to the
/// end, as most editors do; both reset the sticky column.
#[must_use]
pub fn vertical(
    t: &dyn Text,
    pos: usize,
    preferred: Option<usize>,
    lines: isize,
    tab_width: usize,
) -> Vertical {
    let l = t.byte_to_line(pos);
    let col = preferred.unwrap_or_else(|| column_of(t, pos, tab_width));
    let last = t.len_lines().saturating_sub(1);
    let target = l.saturating_add_signed(lines).min(last);
    if target == l && lines != 0 {
        let pos = if lines < 0 { 0 } else { t.len_bytes() };
        return Vertical {
            pos,
            preferred_col: column_of(t, pos, tab_width),
        };
    }
    let line = t.line(target);
    Vertical {
        pos: t.line_to_byte(target) + column::byte_at_col(&line, col, tab_width),
        preferred_col: col,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RopeBuffer;

    fn stops_right(s: &str) -> Vec<usize> {
        let t = RopeBuffer::from_text(s);
        let mut out = vec![0];
        let mut p = 0;
        loop {
            let n = next_grapheme(&t, p);
            if n == p {
                return out;
            }
            out.push(n);
            p = n;
        }
    }

    fn stops_left(s: &str) -> Vec<usize> {
        let t = RopeBuffer::from_text(s);
        let mut p = t.len_bytes();
        let mut out = vec![p];
        loop {
            let n = prev_grapheme(&t, p);
            if n == p {
                return out;
            }
            out.push(n);
            p = n;
        }
    }

    fn both(s: &str, expected: &[usize]) {
        assert_eq!(stops_right(s), expected, "right through {s:?}");
        let mut rev = expected.to_vec();
        rev.reverse();
        assert_eq!(stops_left(s), rev, "left through {s:?}");
    }

    #[test]
    fn graphemes_are_single_steps() {
        both("e\u{301}x", &[0, 3, 4]);
        both("a👨‍👩‍👧b", &[0, 1, 19, 20]);
        both("👍🏽", &[0, 8]);
        both("🇫🇷🇩🇪", &[0, 8, 16]);
    }

    #[test]
    fn line_endings_are_single_steps() {
        both("ab\r\ncd", &[0, 1, 2, 4, 5, 6]);
        both("ab\ncd", &[0, 1, 2, 3, 4, 5]);
        both("\n\n", &[0, 1, 2]);
        both("\r\n\r\n", &[0, 2, 4]);
    }

    #[test]
    fn last_line_and_empty_text() {
        let t = RopeBuffer::from_text("ab");
        assert_eq!(next_grapheme(&t, 2), 2);
        let t = RopeBuffer::from_text("ab\n");
        assert_eq!(next_grapheme(&t, 2), 3);
        let e = RopeBuffer::default();
        assert_eq!(next_grapheme(&e, 0), 0);
        assert_eq!(prev_grapheme(&e, 0), 0);
        assert_eq!(line_end(&e, 0), 0);
        assert_eq!(vertical(&e, 0, None, 1, 4).pos, 0);
        assert_eq!(vertical(&e, 0, None, -1, 4).pos, 0);
    }

    #[test]
    fn line_bounds_exclude_crlf() {
        let t = RopeBuffer::from_text("ab\r\ncd");
        assert_eq!(line_end(&t, 0), 2);
        assert_eq!(line_start(&t, 5), 4);
        assert_eq!(line_end(&t, 4), 6);
        assert_eq!(prev_grapheme(&t, 4), 2);
    }

    #[test]
    fn clamp_snaps_onto_valid_positions() {
        let t = RopeBuffer::from_text("é\r\nx");
        assert_eq!(clamp(&t, 1), 0);
        assert_eq!(clamp(&t, 3), 2);
        assert_eq!(clamp(&t, 99), 5);
    }

    #[test]
    fn sticky_column_survives_short_lines() {
        let t = RopeBuffer::from_text("abcdef\nab\nabcdef");
        let v = vertical(&t, 5, None, 1, 4);
        assert_eq!(
            v,
            Vertical {
                pos: 9,
                preferred_col: 5
            }
        );
        assert_eq!(vertical(&t, v.pos, Some(5), 1, 4).pos, 15);
    }

    #[test]
    fn sticky_column_through_wide_chars_and_tabs() {
        let t = RopeBuffer::from_text("abcd\n日本\nabcd");
        let v = vertical(&t, 3, None, 1, 4);
        assert_eq!(v.pos, 8, "column 3 inside 本 rounds left");
        assert_eq!(vertical(&t, v.pos, Some(3), 1, 4).pos, 15);

        let t = RopeBuffer::from_text("abcdef\n\tx\nabcdef");
        assert_eq!(vertical(&t, 5, None, 1, 4).pos, 9);
        assert_eq!(vertical(&t, 2, None, 1, 4).pos, 7);
        assert_eq!(vertical(&t, 3, None, 1, 4).pos, 8);
    }

    #[test]
    fn vertical_never_lands_inside_crlf() {
        let t = RopeBuffer::from_text("abc\r\nde\r\nfgh");
        assert_eq!(vertical(&t, 3, None, 1, 4).pos, 7);
    }

    #[test]
    fn moving_past_the_ends_goes_to_text_bounds() {
        let t = RopeBuffer::from_text("abc\ndef");
        assert_eq!(
            vertical(&t, 2, None, -1, 4),
            Vertical {
                pos: 0,
                preferred_col: 0
            }
        );
        assert_eq!(
            vertical(&t, 5, None, 1, 4),
            Vertical {
                pos: 7,
                preferred_col: 3
            }
        );
    }

    #[test]
    fn word_movement_skips_punctuation_and_spaces() {
        // Runs: let | ␠ | foo_bar | ␠ | = | ␠ | baz | ( | 1 | );
        let t = RopeBuffer::from_text("let foo_bar = baz(1);\nnext");
        assert_eq!(word_right(&t, 0), 3);
        assert_eq!(word_right(&t, 3), 11);
        assert_eq!(word_right(&t, 11), 13, "punctuation is a stop");
        assert_eq!(word_right(&t, 13), 17);
        assert_eq!(word_right(&t, 17), 18);
        assert_eq!(word_right(&t, 19), 21);
        assert_eq!(word_right(&t, 21), 22, "line end: next line");
        assert_eq!(word_left(&t, 22), 21, "line start: previous line end");
        assert_eq!(word_left(&t, 21), 19);
        assert_eq!(word_left(&t, 14), 12);
        assert_eq!(word_left(&t, 2), 0);
    }

    #[test]
    fn word_at_picks_the_segment_under_the_cursor() {
        let t = RopeBuffer::from_text("foo  bar.baz");
        assert_eq!(word_at(&t, 1), 0..3);
        assert_eq!(word_at(&t, 3), 3..5, "spaces select as a run");
        assert_eq!(word_at(&t, 8), 8..9);
        assert_eq!(word_at(&t, 12), 9..12, "end of line takes the word before");
    }

    #[test]
    fn line_range_includes_the_line_ending() {
        let t = RopeBuffer::from_text("ab\r\ncd");
        assert_eq!(line_range(&t, 1), 0..4);
        assert_eq!(line_range(&t, 5), 4..6);
    }

    #[test]
    fn smart_home_toggles_between_indent_and_column_zero() {
        let t = RopeBuffer::from_text("  \tfoo");
        assert_eq!(smart_home(&t, 6), 3);
        assert_eq!(smart_home(&t, 3), 0);
        assert_eq!(smart_home(&t, 0), 3);
    }

    #[test]
    fn page_moves_clamp_then_reach_the_end() {
        let t = RopeBuffer::from_text("abc\nabc\nabc");
        let v = vertical(&t, 1, None, 10, 4);
        assert_eq!(v.pos, 9);
        assert_eq!(vertical(&t, v.pos, Some(v.preferred_col), 10, 4).pos, 11);
    }
}
