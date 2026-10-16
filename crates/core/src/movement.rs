//! Cursor movement as pure functions of the text and a byte offset.
//!
//! Left and right move by *grapheme*, so `e` plus a combining accent, an
//! emoji with a skin tone, or a flag is one step, and a `\r\n` line ending is
//! one step too. Up and down move by line and aim at a *preferred column*
//! that survives passing through shorter lines (the "sticky column").
//!
//! The functions take [`Text`], not a buffer, so they work on snapshots too.
//!
//! In a mapped file with bytes that are not UTF-8, the text these functions
//! read shows each invalid byte as the control character SUB
//! ([`crate::SUBSTITUTE`]), one byte for one byte. A control character is
//! always a grapheme of its own, so the cursor steps over an invalid byte
//! by itself and every stop is a unit boundary of the raw bytes.

use std::ops::Range;

use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

use crate::long_line;
use crate::wrap::{self, RowPos, WrapView};
use crate::{LineSpan, Text};

/// The last character boundary at or before `b`.
fn floor_boundary(t: &dyn Text, mut b: usize) -> usize {
    while b > 0 && !t.is_char_boundary(b) {
        b -= 1;
    }
    b
}

/// Snap any offset into the text, onto a char boundary, and out of the
/// middle of a `\r\n`.
#[must_use]
pub fn clamp(t: &dyn Text, pos: usize) -> usize {
    let pos = pos.min(t.len_bytes());
    let span = t.line_span(pos);
    floor_boundary(t, pos.min(span.end)).max(span.start)
}

/// The next cursor stop to the right of `pos`.
#[must_use]
pub fn next_grapheme(t: &dyn Text, pos: usize) -> usize {
    let span = t.line_span(pos);
    if pos >= span.end {
        // At the end of the content: step over the whole line ending. On
        // the last line this is the end of the text.
        return span.next.unwrap_or(t.len_bytes());
    }
    let (line, base) = long_line::around(t, span, pos);
    let off = pos - base;
    // GraphemeCursor panics off a char boundary, so floor first.
    let mut c = GraphemeCursor::new(line.floor_char_boundary(off), line.len(), true);
    match c.next_boundary(&line, 0) {
        Ok(Some(b)) => base + b,
        Ok(None) | Err(_) => base + line.len(),
    }
}

/// The next cursor stop to the left of `pos`.
#[must_use]
pub fn prev_grapheme(t: &dyn Text, pos: usize) -> usize {
    let mut span = t.line_span(pos);
    if pos <= span.start {
        if span.start == 0 {
            return 0;
        }
        let prev = t.line_span(span.start - 1);
        if prev.end < span.start {
            // Step over the line ending to the end of the line above.
            return prev.end;
        }
        // A pseudo-line break (a mapped file cuts very long lines it has
        // not indexed yet): there is no line ending, so step back one
        // character within the piece before it.
        span = prev;
    }
    if pos > span.end {
        // Between the \r and \n of a line ending.
        return span.end;
    }
    let (line, base) = long_line::around(t, span, pos);
    let off = pos - base;
    let mut c = GraphemeCursor::new(line.floor_char_boundary(off), line.len(), true);
    match c.prev_boundary(&line, 0) {
        Ok(Some(b)) => base + b,
        Ok(None) | Err(_) => base,
    }
}

/// Start of the line containing `pos`.
#[must_use]
pub fn line_start(t: &dyn Text, pos: usize) -> usize {
    t.line_span(pos).start
}

/// End of the content of the line containing `pos`, before its line ending.
#[must_use]
pub fn line_end(t: &dyn Text, pos: usize) -> usize {
    t.line_span(pos).end
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
    let span = t.line_span(pos);
    if pos <= span.start {
        return prev_grapheme(t, pos);
    }
    let (line, base) = long_line::around(t, span, pos);
    let off = (pos - base).min(line.len());
    segments(&line)
        .take_while(|&(i, _, _)| i < off)
        .filter(|&(_, _, c)| c != Class::Space)
        .last()
        .map_or(base, |(i, _, _)| base + i)
}

/// End of the next word, as Alt+Right (macOS) or Ctrl+Right does. At the
/// end of a line, moves to the start of the next line.
#[must_use]
pub fn word_right(t: &dyn Text, pos: usize) -> usize {
    let span = t.line_span(pos);
    if pos >= span.end {
        return next_grapheme(t, pos);
    }
    let (line, base) = long_line::around(t, span, pos);
    let off = pos.saturating_sub(base);
    segments(&line)
        .find(|&(_, end, c)| end > off && c != Class::Space)
        .map_or(base + line.len(), |(_, end, _)| base + end)
}

/// The word, run of spaces, or run of punctuation under `pos`, for a
/// double-click. At the end of a line it is the run before the position.
#[must_use]
pub fn word_at(t: &dyn Text, pos: usize) -> Range<usize> {
    let span = t.line_span(pos);
    let (line, base) = long_line::around(t, span, pos);
    let off = pos.saturating_sub(base).min(line.len());
    let mut before = None;
    for (i, end, _) in segments(&line) {
        if i <= off && off < end {
            return base + i..base + end;
        }
        if end == off {
            before = Some(base + i..base + end);
        }
    }
    before.unwrap_or(pos..pos)
}

/// The whole line containing `pos`, including its line ending, for a
/// triple-click.
#[must_use]
pub fn line_range(t: &dyn Text, pos: usize) -> Range<usize> {
    let span = t.line_span(pos);
    span.start..span.next.unwrap_or(t.len_bytes())
}

/// Home: the first non-blank character of the line, or column 0 if the
/// cursor is already there (so pressing Home twice toggles).
#[must_use]
pub fn smart_home(t: &dyn Text, pos: usize) -> usize {
    let span = t.line_span(pos);
    // Indentation is short; only the start of a long line is read.
    let head = t.slice(span.start..span.end.min(span.start + long_line::NEAR));
    let indent = head.len() - head.trim_start_matches([' ', '\t']).len();
    let first = span.start + indent;
    if pos == first { span.start } else { first }
}

/// Display column of `pos` on its line (block-approximate on a long line).
#[must_use]
pub fn column_of(t: &dyn Text, pos: usize, tab_width: usize) -> usize {
    long_line::col_at(t, t.line_span(pos), pos, tab_width)
}

/// The result of a vertical move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vertical {
    /// The new cursor offset.
    pub pos: usize,
    /// The column to keep aiming for on the next vertical move.
    pub preferred_col: usize,
}

/// The line `lines` lines from `span` (negative is up), or `None` past the
/// first or last line.
fn step_lines(t: &dyn Text, mut span: LineSpan, lines: isize) -> Option<LineSpan> {
    for _ in 0..lines.unsigned_abs() {
        span = if lines > 0 {
            t.line_span(span.next?)
        } else if span.start == 0 {
            return None;
        } else {
            t.line_span(span.start - 1)
        };
    }
    Some(span)
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
    let col = preferred.unwrap_or_else(|| column_of(t, pos, tab_width));
    let span = t.line_span(pos);
    // Past the first or last line: a partial move (Page Down near the end)
    // lands on that line; from the first or last line itself, go to the
    // start or end of the text.
    let target = if let Some(target) = step_lines(t, span, lines) {
        target
    } else {
        let reached = if lines < 0 { 0 } else { t.len_bytes() };
        let end_span = t.line_span(reached);
        if end_span.start == span.start {
            return Vertical {
                pos: reached,
                preferred_col: column_of(t, reached, tab_width),
            };
        }
        end_span
    };
    Vertical {
        pos: long_line::byte_at_col(t, target, col, tab_width),
        preferred_col: col,
    }
}

/// Move `rows` visual rows up (negative) or down, for when soft wrap is on.
///
/// `preferred` is the sticky column *within the row*, kept across shorter
/// rows. As with [`vertical`], moving past the first or last row goes to the
/// start or end of the text. With wrap off (`w` not wrapping) this behaves
/// exactly like [`vertical`].
pub fn vertical_rows(
    t: &dyn Text,
    w: &mut WrapView<'_>,
    pos: usize,
    preferred: Option<usize>,
    rows: isize,
) -> Vertical {
    let tab = w.tab_width();
    let span = t.line_span(pos);
    // A long line is never wrapped: one row, with block-approximate columns.
    let (row, vcol) = if long_line::is_long(span) {
        (
            0,
            preferred.unwrap_or_else(|| long_line::col_at(t, span, pos, tab)),
        )
    } else {
        let line = t.slice(span.start..span.end);
        let off = (pos - span.start).min(line.len());
        let rs = w.rows(&line);
        let row = wrap::row_of(rs, off);
        (
            row,
            preferred.unwrap_or_else(|| wrap::visual_col(&line, rs, off, tab)),
        )
    };
    let from = RowPos {
        start: span.start,
        row,
    };
    let (to, moved) = wrap::step_rows(t, w, from, rows);
    if moved == 0 && rows != 0 {
        let end = if rows < 0 { 0 } else { t.len_bytes() };
        let espan = t.line_span(end);
        let col = if long_line::is_long(espan) {
            long_line::col_at(t, espan, end, tab)
        } else {
            let eline = t.slice(espan.start..espan.end);
            wrap::visual_col(&eline, w.rows(&eline), end - espan.start, tab)
        };
        return Vertical {
            pos: end,
            preferred_col: col,
        };
    }
    let tspan = t.line_span(to.start);
    if long_line::is_long(tspan) {
        return Vertical {
            pos: long_line::byte_at_col(t, tspan, vcol, tab),
            preferred_col: vcol,
        };
    }
    let target = t.slice(tspan.start..tspan.end);
    let rs = w.rows(&target);
    Vertical {
        pos: tspan.start + wrap::byte_at_row_col(&target, rs, to.row, vcol, tab),
        preferred_col: vcol,
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

    fn rows_move(text: &str, pos: usize, preferred: Option<usize>, n: isize) -> Vertical {
        let t = RopeBuffer::from_text(text);
        let mut cache = wrap::WrapCache::default();
        let mut w = WrapView::new(&mut cache, Some(8), 4);
        vertical_rows(&t, &mut w, pos, preferred, n)
    }

    #[test]
    fn vertical_rows_moves_within_wrapped_lines() {
        // Width 8: "hello world" wraps after "hello ".
        assert_eq!(
            rows_move("hello world", 2, None, 1),
            Vertical {
                pos: 8,
                preferred_col: 2
            }
        );
        assert_eq!(rows_move("hello world", 8, Some(2), -1).pos, 2);
        assert_eq!(
            rows_move("hello world", 11, None, -1).pos,
            5,
            "before the hanging space"
        );
        let two = "hello world\nab";
        assert_eq!(rows_move(two, 4, None, 1).pos, 10);
        assert_eq!(rows_move(two, 10, Some(4), 1).pos, 14);
        assert_eq!(rows_move(two, 14, Some(4), -1).pos, 10);
    }

    #[test]
    fn vertical_rows_pages_and_reaches_the_end() {
        let text = "hello world\nhello world\nhello world";
        assert_eq!(rows_move(text, 2, None, 3).pos, 20);
        assert_eq!(rows_move(text, 20, Some(2), 3).pos, 32);
        assert_eq!(rows_move(text, 32, Some(2), 3).pos, 35);
    }

    #[test]
    fn vertical_rows_without_wrap_matches_vertical() {
        let text = "abcdef\nab\nabcdef\n日本\n\tx";
        let t = RopeBuffer::from_text(text);
        let mut cache = wrap::WrapCache::default();
        for pos in [0, 3, 5, 9, 12, 16, 20, 23] {
            for n in [-2, -1, 1, 2] {
                let mut w = WrapView::new(&mut cache, None, 4);
                assert_eq!(
                    vertical_rows(&t, &mut w, pos, None, n),
                    vertical(&t, pos, None, n, 4),
                    "pos {pos} n {n}"
                );
            }
        }
    }

    #[test]
    fn page_moves_clamp_then_reach_the_end() {
        let t = RopeBuffer::from_text("abc\nabc\nabc");
        let v = vertical(&t, 1, None, 10, 4);
        assert_eq!(v.pos, 9);
        assert_eq!(vertical(&t, v.pos, Some(v.preferred_col), 10, 4).pos, 11);
    }

    #[test]
    fn movement_on_a_long_line_reads_only_around_the_cursor() {
        let long = "word ".repeat(40_000); // 200 KB, one line
        let text = format!("top\n    {long}\nend");
        let t = RopeBuffer::from_text(&text);
        let start = 4; // the long line
        let mid = start + 100_002;
        assert_eq!(next_grapheme(&t, mid), mid + 1);
        assert_eq!(prev_grapheme(&t, mid), mid - 1);
        // Words start 8 bytes in and repeat every 5: mid is on the "d".
        assert_eq!((mid - 8) % 5, 3);
        assert_eq!(word_right(&t, mid), mid + 1, "to the end of the word");
        assert_eq!(word_left(&t, mid), mid - 3, "to the start of the word");
        assert_eq!(smart_home(&t, mid), start + 4, "after the indent");
        // Columns are exact for ASCII; up and down keep them.
        assert_eq!(column_of(&t, mid, 4), mid - start);
        let up = vertical(&t, mid, None, -1, 4);
        assert_eq!(up.pos, 3, "the short line above ends first");
        let down = vertical(&t, up.pos, Some(up.preferred_col), 1, 4);
        assert_eq!(down.pos, mid);
        let end = t.line_span(mid).end;
        assert_eq!(next_grapheme(&t, end), end + 1, "over the line break");
        assert_eq!(line_end(&t, mid), end);
    }
}
