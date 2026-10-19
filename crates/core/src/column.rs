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
    col_at_from(line, byte, 0, tab_width)
}

/// Display column of byte offset `byte` within `seg`, a piece of a line
/// that starts at column `start_col` of that line. Tab stops are measured
/// from the start of the whole line. Returns an absolute column.
#[must_use]
pub fn col_at_from(seg: &str, byte: usize, start_col: usize, tab_width: usize) -> usize {
    let mut col = start_col;
    for (i, g) in seg.grapheme_indices(true) {
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
    byte_at_half_col_from(line, half, 0, tab_width)
}

/// Like [`byte_at_half_col`] for `seg`, a piece of a line starting at
/// column `start_col`; `half` is measured from the start of `seg`.
#[must_use]
pub fn byte_at_half_col_from(seg: &str, half: usize, start_col: usize, tab_width: usize) -> usize {
    let mut rel = 0;
    for (i, g) in seg.grapheme_indices(true) {
        let w = grapheme_cols(g, start_col + rel, tab_width);
        if half < 2 * rel + w {
            return i;
        }
        if half < 2 * (rel + w) {
            return i + g.len();
        }
        rel += w;
    }
    seg.len()
}

/// Byte offset of the grapheme boundary nearest to column `col`, ties going
/// left. Used by up and down: aiming at column 3 inside a wide character
/// spanning columns 2 to 4 lands before it.
#[must_use]
pub fn byte_at_col(line: &str, col: usize, tab_width: usize) -> usize {
    byte_at_half_col(line, (2 * col).saturating_sub(1), tab_width)
}

/// Whether `c` draws as nothing yet can change what text means: a
/// bidirectional control or a zero-width character (M26).
///
/// Bidirectional controls (U+202A–202E, U+2066–2069, and the marks U+200E,
/// U+200F and U+061C) tell a bidi-aware viewer to show text in another
/// order than its bytes. "Trojan Source" code uses them so a reviewer
/// reads one thing while the compiler sees another; a zero-width space,
/// word joiner or byte-order mark inside a word makes two identifiers
/// that look the same differ. The editor draws a box on each one so it
/// cannot hide in plain sight.
///
/// The zero-width joiner and non-joiner (U+200D, U+200C) are left out on
/// purpose: emoji sequences and scripts such as Persian and Devanagari
/// need them, and they sit inside a grapheme with the characters they
/// join rather than alone.
#[must_use]
pub fn is_hidden_char(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{061C}'
            | '\u{200B}'
            | '\u{2060}'..='\u{2064}'
            | '\u{FEFF}'
            | '\u{00AD}'
            | '\u{180E}'
    )
}

/// The columns of the hidden characters ([`is_hidden_char`]) in `seg`, a
/// piece of a line starting at column `start_col`: one `(column, width)`
/// per grapheme that holds one. Each is its own grapheme (Unicode puts a
/// break around format characters), and [`grapheme_cols`] gives it one
/// column, so there is a cell to draw the mark in.
pub fn hidden_cols(
    seg: &str,
    start_col: usize,
    tab_width: usize,
) -> impl Iterator<Item = (usize, usize)> + '_ {
    // Plain ASCII, by far the common case, cannot hold one: skip the
    // grapheme walk.
    let any = !seg.is_ascii() && seg.chars().any(is_hidden_char);
    let mut col = start_col;
    seg.graphemes(true)
        .take_while(move |_| any)
        .filter_map(move |g| {
            let at = col;
            let w = grapheme_cols(g, col, tab_width);
            col += w;
            g.chars().any(is_hidden_char).then_some((at, w))
        })
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

    #[test]
    fn hidden_characters_get_a_cell_each() {
        // Trojan Source: an RLO and a PDI around a comment's end.
        let line = "x = 1; /* \u{202E} } \u{2066}if admin\u{2069} */";
        let cols: Vec<_> = hidden_cols(line, 0, 4).collect();
        assert_eq!(cols, [(10, 1), (14, 1), (23, 1)]);
        // A zero-width space inside an identifier, after a tab.
        assert_eq!(
            hidden_cols("\tab\u{200B}c", 0, 4).collect::<Vec<_>>(),
            [(6, 1)]
        );
        // Columns count from the segment's start column.
        assert_eq!(hidden_cols("\u{FEFF}", 7, 4).collect::<Vec<_>>(), [(7, 1)]);
        assert_eq!(hidden_cols("plain ascii", 0, 4).count(), 0);
        assert_eq!(hidden_cols("日本語 é", 0, 4).count(), 0);
    }

    #[test]
    fn joiners_in_emoji_and_scripts_are_not_hidden() {
        assert!(is_hidden_char('\u{202E}'));
        assert!(is_hidden_char('\u{00AD}'));
        assert!(!is_hidden_char('\u{200D}'));
        assert!(!is_hidden_char('\u{200C}'));
        assert_eq!(hidden_cols("a👨‍👩‍👧b می‌خواهم", 0, 4).count(), 0);
    }
}
