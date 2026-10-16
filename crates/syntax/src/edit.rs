//! Telling tree-sitter about an edit.
//!
//! A syntax tree is reused across edits: tree-sitter shifts the parts after
//! an edit and reparses only around it, which is what makes highlighting
//! cheap per keystroke. For that it needs each edit in its own terms: byte
//! offsets plus (row, column) positions of the start, the old end and the
//! new end, where columns count bytes within the row.

use nimbus_core::{Edit, Text};
use tree_sitter::{InputEdit, Point};

/// The tree-sitter position of byte `byte` in `text`, which is the text the
/// position is meant for (before an edit, for the start of that edit).
#[must_use]
pub fn point_at(text: &dyn Text, byte: usize) -> Point {
    let row = text.byte_to_line(byte);
    Point {
        row,
        column: byte - text.line_to_byte(row),
    }
}

/// `start` moved past `s`: rows for each line break, then the bytes after
/// the last one.
#[must_use]
pub fn advance(start: Point, s: &str) -> Point {
    match s.rfind('\n') {
        Some(last) => Point {
            row: start.row + s.bytes().filter(|b| *b == b'\n').count(),
            column: s.len() - last - 1,
        },
        None => Point {
            row: start.row,
            column: start.column + s.len(),
        },
    }
}

/// Convert `edit`, given the position of its start in the text before it
/// was applied. The start position does not change, so it can equally be
/// computed from the text after.
#[must_use]
pub fn input_edit(edit: &Edit, start_position: Point) -> InputEdit {
    InputEdit {
        start_byte: edit.start,
        old_end_byte: edit.start + edit.removed.len(),
        new_end_byte: edit.start + edit.inserted.len(),
        start_position,
        old_end_position: advance(start_position, &edit.removed),
        new_end_position: advance(start_position, &edit.inserted),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nimbus_core::{Buffer, RopeBuffer};

    #[test]
    fn advance_counts_rows_and_bytes() {
        let p = Point { row: 2, column: 5 };
        assert_eq!(advance(p, "abc"), Point { row: 2, column: 8 });
        assert_eq!(advance(p, "a\nbc"), Point { row: 3, column: 2 });
        assert_eq!(advance(p, "a\r\n"), Point { row: 3, column: 0 });
        assert_eq!(
            advance(p, "é"),
            Point { row: 2, column: 7 },
            "columns are bytes"
        );
        assert_eq!(advance(p, ""), p);
    }

    #[test]
    fn input_edit_matches_positions_before_and_after() {
        let mut t = RopeBuffer::from_text("fn a() {\n    x\n}\n");
        let start = point_at(&t, 13);
        let e = t.replace(13..14, "let y = 1;\nlet z").unwrap();
        let ie = input_edit(&e, start);
        assert_eq!(ie.start_position, Point { row: 1, column: 4 });
        assert_eq!(ie.old_end_position, Point { row: 1, column: 5 });
        assert_eq!(
            ie.new_end_position,
            point_at(&t, ie.new_end_byte),
            "agrees with the new text"
        );
        assert_eq!(ie.new_end_position, Point { row: 2, column: 5 });
    }
}
