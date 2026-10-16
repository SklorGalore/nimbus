//! Cursor movement as pure functions of the text and a byte offset.
//!
//! Left and right move by *grapheme*, so `e` plus a combining accent, an
//! emoji with a skin tone, or a flag is one step, and a `\r\n` line ending is
//! one step too. Up and down move by line and aim at a *preferred column*
//! that survives passing through shorter lines (the "sticky column").
//!
//! The functions take [`Text`], not a buffer, so they work on snapshots too.

use unicode_segmentation::GraphemeCursor;

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
    fn page_moves_clamp_then_reach_the_end() {
        let t = RopeBuffer::from_text("abc\nabc\nabc");
        let v = vertical(&t, 1, None, 10, 4);
        assert_eq!(v.pos, 9);
        assert_eq!(vertical(&t, v.pos, Some(v.preferred_col), 10, 4).pos, 11);
    }
}
