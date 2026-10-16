//! Lines too long to read whole.
//!
//! Minified JSON or a log without line breaks can be one line of hundreds of
//! megabytes. Copying such a line to draw it, or to move the cursor one
//! character, would cost that much per frame. So a line longer than
//! [`LONG_LINE`] is only ever read in pieces: the part on screen, or a few
//! kilobytes around the cursor.
//!
//! Columns on a long line are approximate. Working out a column exactly
//! means reading every character before it, so instead the line is split
//! into blocks of [`BLOCK`] bytes, each block is assumed to start at the
//! column equal to its byte offset (one column per byte, exact for ASCII
//! without tabs), and columns within a block are counted exactly. The
//! mapping is a fixed function of the byte offset, so moving the cursor and
//! drawing always agree.

use std::borrow::Cow;
use std::ops::Range;

use crate::column;
use crate::{LineSpan, Text};

/// Lines longer than this, in bytes, are never read whole.
pub const LONG_LINE: usize = 64 * 1024;

/// Size of the blocks that columns on a long line are counted within.
pub const BLOCK: usize = 4096;

/// How far either side of the cursor movement reads on a long line.
pub const NEAR: usize = 4096;

/// Whether `span` is a long line.
#[must_use]
pub fn is_long(span: LineSpan) -> bool {
    span.end - span.start > LONG_LINE
}

/// The first character boundary at or after `b`, not past `end`.
fn ceil_boundary(t: &dyn Text, mut b: usize, end: usize) -> usize {
    while b < end && !t.is_char_boundary(b) {
        b += 1;
    }
    b.min(end)
}

/// The last character boundary at or before `b`, not before `start`.
fn floor_boundary(t: &dyn Text, mut b: usize, start: usize) -> usize {
    while b > start && !t.is_char_boundary(b) {
        b -= 1;
    }
    b
}

/// Start of the block containing byte `b` of the line, on a character
/// boundary, and the column that block is taken to start at.
fn block_of(t: &dyn Text, span: LineSpan, b: usize) -> (usize, usize) {
    let block = (b - span.start) / BLOCK * BLOCK;
    let bs = ceil_boundary(t, span.start + block, b.max(span.start + block));
    (bs, bs - span.start)
}

/// The text of the line around `pos`, and the byte where it starts: the
/// whole line, or for a long line [`NEAR`] bytes either side of `pos`.
#[must_use]
pub fn around(t: &dyn Text, span: LineSpan, pos: usize) -> (Cow<'_, str>, usize) {
    if !is_long(span) {
        return (t.slice(span.start..span.end), span.start);
    }
    let a = floor_boundary(t, pos.saturating_sub(NEAR).max(span.start), span.start);
    let b = (pos + NEAR).min(span.end);
    (t.slice(a..b), a)
}

/// Display column of byte `b` on the line `span`: exact on a normal line,
/// block-approximate on a long one.
#[must_use]
pub fn col_at(t: &dyn Text, span: LineSpan, b: usize, tab_width: usize) -> usize {
    let b = b.clamp(span.start, span.end);
    if !is_long(span) {
        let line = t.slice(span.start..span.end);
        return column::col_at(&line, b - span.start, tab_width);
    }
    let (bs, base) = block_of(t, span, b);
    let text = t.slice(bs..b);
    column::col_at_from(&text, b - bs, base, tab_width)
}

/// The byte at half-column `half` on the line `span` (for clicks, which
/// pick the nearer side of a character): exact on a normal line,
/// block-approximate on a long one.
#[must_use]
pub fn byte_at_half_col(t: &dyn Text, span: LineSpan, half: usize, tab_width: usize) -> usize {
    if !is_long(span) {
        let line = t.slice(span.start..span.end);
        return span.start + column::byte_at_half_col(&line, half, tab_width);
    }
    let col = half / 2;
    let guess = (span.start + col).min(span.end);
    let (bs, base) = block_of(t, span, guess);
    // Columns past the block's base come from its bytes; read enough of
    // the next block too for wide characters and tabs.
    let text = t.slice(bs..(bs + 2 * BLOCK).min(span.end));
    let rel = half.saturating_sub(2 * base);
    (bs + column::byte_at_half_col_from(&text, rel, base, tab_width)).min(span.end)
}

/// The byte at column `col` on the line `span`: the character covering it,
/// or the line end past the last one.
#[must_use]
pub fn byte_at_col(t: &dyn Text, span: LineSpan, col: usize, tab_width: usize) -> usize {
    byte_at_half_col(t, span, (2 * col).saturating_sub(1), tab_width)
}

/// The bytes to read to draw a long line from column `first_col`,
/// `width_cols` wide: from the start of the block holding `first_col`, far
/// enough to cover the screen however wide its characters are.
#[must_use]
pub fn window(t: &dyn Text, span: LineSpan, first_col: usize, width_cols: usize) -> Range<usize> {
    let guess = (span.start + first_col).min(span.end);
    let (bs, _) = block_of(t, span, guess);
    let end = (bs + 4 * (BLOCK + width_cols)).min(span.end);
    bs..ceil_boundary(t, end, span.end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RopeBuffer;

    fn long(text: &str) -> (RopeBuffer, LineSpan) {
        let t = RopeBuffer::from_text(text);
        let span = t.line_span(0);
        (t, span)
    }

    #[test]
    fn short_lines_are_exact() {
        let (t, span) = long("a\tb日c");
        assert!(!is_long(span));
        assert_eq!(col_at(&t, span, 2, 4), 4);
        assert_eq!(byte_at_col(&t, span, 4, 4), 2);
        let (text, base) = around(&t, span, 3);
        assert_eq!((text.as_ref(), base), ("a\tb日c", 0));
    }

    #[test]
    fn long_lines_count_within_blocks_and_round_trip() {
        // ASCII: exact everywhere.
        let text = "abcdefgh".repeat(LONG_LINE / 4);
        let (t, span) = long(&text);
        assert!(is_long(span));
        for b in [
            0,
            1,
            BLOCK - 1,
            BLOCK,
            BLOCK + 7,
            3 * BLOCK + 100,
            text.len(),
        ] {
            assert_eq!(col_at(&t, span, b, 4), b, "ascii column of {b}");
            assert_eq!(byte_at_col(&t, span, b, 4), b, "ascii byte of {b}");
        }
        // Wide characters: within a block the count is exact; the block
        // starts at its byte offset.
        let text = "日".repeat(LONG_LINE);
        let (t, span) = long(&text);
        let b = 3 * 2740; // inside block 2, on a char boundary (8220)
        let (bs, base) = block_of(&t, span, b);
        assert_eq!(base, bs);
        assert_eq!(col_at(&t, span, b, 4), base + 2 * ((b - bs) / 3));
        // Columns grow with bytes: a fixed, monotone mapping.
        let mut last = 0;
        for b in (0..text.len()).step_by(3 * 97) {
            let c = col_at(&t, span, b, 4);
            assert!(c >= last, "column went back at {b}");
            last = c;
            assert_eq!(byte_at_col(&t, span, c, 4), b, "round trip at {b}");
        }
    }

    #[test]
    fn windows_and_neighbourhoods_stay_small_and_on_boundaries() {
        let text = "é".repeat(LONG_LINE);
        let (t, span) = long(&text);
        let w = window(&t, span, 50_000, 200);
        assert!(w.end - w.start <= 4 * (BLOCK + 200) + 3);
        assert!(t.is_char_boundary(w.start) && t.is_char_boundary(w.end));
        let (piece, base) = around(&t, span, 60_001);
        assert!(piece.len() <= 2 * NEAR + 3);
        assert!(t.is_char_boundary(base));
        assert!(base <= 60_001 && 60_001 <= base + piece.len());
    }
}
