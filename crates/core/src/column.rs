//! Display columns: where a byte offset appears on the monospace grid.
//!
//! Four different things can be called "one character": a byte, a `char`
//! (Unicode scalar), a *grapheme* (what a reader sees as one character, such
//! as `e` plus a combining accent, or a family emoji made of seven chars), and
//! a *column* (one cell of the grid). The cursor moves by grapheme; the
//! renderer places graphemes by column. Both use these functions so the cursor
//! always lines up with the text.
//!
//! Width rules: a tab runs to the next tab stop; wide characters such as CJK
//! take two columns; everything else takes one, including zero-width oddities
//! like a lone combining mark, so every cursor stop is distinct and clickable.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Columns grapheme `g` occupies when it starts at column `col`.
#[must_use]
pub fn grapheme_cols(g: &str, col: usize, tab_width: usize) -> usize {
    if g == "\t" {
        let t = tab_width.max(1);
        t - col % t
    } else {
        g.width().max(1)
    }
}

/// Display column of byte offset `byte` within line content `line`.
#[must_use]
pub fn col_at(line: &str, byte: usize, tab_width: usize) -> usize {
    let mut col = 0;
    for (i, g) in line.grapheme_indices(true) {
        if i >= byte {
            break;
        }
        col += grapheme_cols(g, col, tab_width);
    }
    col
}

/// Total display width of `line`.
#[must_use]
pub fn line_width(line: &str, tab_width: usize) -> usize {
    col_at(line, line.len(), tab_width)
}

/// Byte offset of the grapheme boundary for a horizontal position given in
/// *half*-columns. Half-columns let a click on the right half of a wide
/// character or tab land after it and the left half before it. Positions
/// past the end of the line map to its end.
#[must_use]
pub fn byte_at_half_col(line: &str, half: usize, tab_width: usize) -> usize {
    let mut col = 0;
    for (i, g) in line.grapheme_indices(true) {
        let w = grapheme_cols(g, col, tab_width);
        if half < 2 * col + w {
            return i;
        }
        if half < 2 * (col + w) {
            return i + g.len();
        }
        col += w;
    }
    line.len()
}

/// Byte offset of the grapheme boundary nearest to column `col`, ties going
/// left. Used by up and down: aiming at column 3 inside a wide character
/// spanning columns 2 to 4 lands before it.
#[must_use]
pub fn byte_at_col(line: &str, col: usize, tab_width: usize) -> usize {
    byte_at_half_col(line, (2 * col).saturating_sub(1), tab_width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn col_at_handles_tabs_wide_chars_and_clusters() {
        assert_eq!(col_at("\tx", 1, 4), 4);
        assert_eq!(col_at("ab\tx", 3, 4), 4);
        assert_eq!(col_at("abcd\tx", 5, 4), 8);
        assert_eq!(col_at("日本", 3, 4), 2);
        assert_eq!(col_at("日本", 6, 4), 4);
        assert_eq!(col_at("e\u{301}x", 3, 4), 1);
        assert_eq!(col_at("日日\tx", 7, 4), 8);
        assert_eq!(col_at("a👨‍👩‍👧b", 19, 4), 3);
        assert_eq!(col_at("\u{301}", 2, 4), 1);
    }

    #[test]
    fn byte_at_col_rounds_to_the_nearest_boundary() {
        let cjk = "日本語";
        for (col, byte) in [(0, 0), (1, 0), (2, 3), (3, 3), (4, 6), (99, 9)] {
            assert_eq!(byte_at_col(cjk, col, 4), byte, "col {col}");
        }
        for (col, byte) in [(1, 0), (2, 0), (3, 1), (4, 1), (5, 2)] {
            assert_eq!(byte_at_col("\tx", col, 4), byte, "col {col}");
        }
    }

    #[test]
    fn clicks_use_half_columns() {
        for (half, byte) in [(0, 0), (1, 0), (2, 3), (3, 3), (4, 3), (5, 4), (50, 4)] {
            assert_eq!(byte_at_half_col("日x", half, 4), byte, "half {half}");
        }
    }
}
