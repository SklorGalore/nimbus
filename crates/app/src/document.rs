//! One open file: its text, where it lives on disk, the cursor, and whether
//! it has unsaved changes.

use std::path::{Path, PathBuf};

use nimbus_core::movement;
use nimbus_core::{Buffer, Edit, EditError, LineEnding, RopeBuffer, Selection, Selections, Text};

use crate::input::Motion;
use crate::io::{self, OpenError, SaveError};

/// An open document.
#[derive(Debug, Default)]
pub struct Document {
    /// The text.
    pub buffer: RopeBuffer,
    /// Where it saves to; `None` for an untitled buffer.
    pub path: Option<PathBuf>,
    /// Inserted by Enter, detected when the file was opened.
    pub line_ending: LineEnding,
    /// Cursor and selection.
    pub selections: Selections,
    /// Display column that up and down movement aims for, kept across
    /// shorter lines; `None` until the first vertical move.
    pub preferred_column: Option<usize>,
    /// Unsaved changes exist.
    pub dirty: bool,
    /// The file had a UTF-8 byte order mark, written back on save.
    pub bom: bool,
    /// Increments on every edit. Background work (search, parsing) records
    /// the version it read, so stale results can be recognised and dropped.
    pub version: u64,
}

impl Document {
    /// An empty untitled document.
    #[must_use]
    pub fn untitled() -> Self {
        Self::default()
    }

    /// Open `path`. A path that does not exist yet gives an empty document
    /// that will be created on first save.
    ///
    /// # Errors
    ///
    /// Returns an [`OpenError`] for anything other than a missing file.
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        let (buffer, line_ending, bom) = match io::open(path) {
            Ok(opened) => (opened.buffer, opened.line_ending, opened.bom),
            Err(OpenError::NotFound) => (RopeBuffer::default(), LineEnding::default(), false),
            Err(e) => return Err(e),
        };
        Ok(Self {
            buffer,
            path: Some(path.to_path_buf()),
            line_ending,
            bom,
            ..Self::default()
        })
    }

    /// Short name for the window title: file name, or "untitled".
    #[must_use]
    pub fn display_name(&self) -> String {
        self.path.as_ref().and_then(|p| p.file_name()).map_or_else(
            || "untitled".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    /// Save to the document's path.
    ///
    /// # Errors
    ///
    /// Returns a [`SaveError`] if writing fails, or if the document is
    /// untitled (until M4 adds a save dialog). The document stays dirty.
    pub fn save(&mut self) -> Result<(), SaveError> {
        let Some(path) = self.path.as_deref() else {
            return Err(SaveError {
                step: "choosing a path",
                source: std::io::Error::other("untitled documents can be saved from M4 on"),
            });
        };
        io::save(path, &self.buffer, self.bom)?;
        self.dirty = false;
        Ok(())
    }

    /// Replace `range` with `text` and put the cursor at the end of the
    /// inserted text.
    ///
    /// # Errors
    ///
    /// Returns the buffer's [`EditError`] for an invalid range; nothing
    /// changes in that case.
    pub fn replace(
        &mut self,
        range: std::ops::Range<usize>,
        text: &str,
    ) -> Result<Edit, EditError> {
        let edit = self.buffer.replace(range, text)?;
        self.selections
            .set_primary(Selection::cursor(edit.new_range().end));
        self.preferred_column = None;
        self.dirty = true;
        self.version += 1;
        Ok(edit)
    }

    /// Type `text` at the cursor, replacing any selection.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn insert(&mut self, text: &str) -> Result<Edit, EditError> {
        self.replace(self.selections.primary().range(), text)
    }

    /// Enter: insert the document's line ending.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn insert_newline(&mut self) -> Result<Edit, EditError> {
        self.insert(self.line_ending.as_str())
    }

    /// The cursor's byte offset, always a valid position in the text.
    #[must_use]
    pub fn cursor(&self) -> usize {
        movement::clamp(&self.buffer, self.selections.primary().head)
    }

    fn set_cursor(&mut self, pos: usize) {
        debug_assert!(
            self.buffer.is_char_boundary(pos),
            "cursor {pos} off a char boundary"
        );
        self.selections.set_primary(Selection::cursor(pos));
    }

    /// Put the cursor at `pos` (snapped to a valid position), as a click does.
    pub fn click(&mut self, pos: usize) {
        self.set_cursor(movement::clamp(&self.buffer, pos));
        self.preferred_column = None;
    }

    /// Move the cursor. `page_lines` is how many lines one screen holds.
    pub fn move_cursor(&mut self, motion: Motion, tab_width: usize, page_lines: usize) {
        let t = &self.buffer;
        let pos = self.cursor();
        let page = isize::try_from(page_lines.max(1)).unwrap_or(isize::MAX);
        let vertical =
            |lines: isize| movement::vertical(t, pos, self.preferred_column, lines, tab_width);
        let (new_pos, preferred) = match motion {
            Motion::Left => (movement::prev_grapheme(t, pos), None),
            Motion::Right => (movement::next_grapheme(t, pos), None),
            Motion::LineStart => (movement::line_start(t, pos), None),
            Motion::LineEnd => (movement::line_end(t, pos), None),
            Motion::DocStart => (0, None),
            Motion::DocEnd => (t.len_bytes(), None),
            Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown => {
                let lines = match motion {
                    Motion::Up => -1,
                    Motion::Down => 1,
                    Motion::PageUp => -page,
                    _ => page,
                };
                let v = vertical(lines);
                (v.pos, Some(v.preferred_col))
            }
        };
        self.set_cursor(new_pos);
        self.preferred_column = preferred;
    }

    /// Backspace: delete the selection, or the grapheme before the cursor.
    /// At the start of a line this joins it to the previous one, removing a
    /// `\r\n` as one unit. Returns `None` when there is nothing to delete.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn delete_back(&mut self) -> Result<Option<Edit>, EditError> {
        let sel = self.selections.primary();
        let range = if sel.is_empty() {
            let pos = self.cursor();
            movement::prev_grapheme(&self.buffer, pos)..pos
        } else {
            sel.range()
        };
        if range.is_empty() {
            return Ok(None);
        }
        self.replace(range, "").map(Some)
    }

    /// Delete key: delete the selection, or the grapheme after the cursor.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn delete_forward(&mut self) -> Result<Option<Edit>, EditError> {
        let sel = self.selections.primary();
        let range = if sel.is_empty() {
            let pos = self.cursor();
            pos..movement::next_grapheme(&self.buffer, pos)
        } else {
            sel.range()
        };
        if range.is_empty() {
            return Ok(None);
        }
        self.replace(range, "").map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_moves_the_cursor_and_marks_dirty() {
        let mut doc = Document::untitled();
        doc.insert("hé").unwrap();
        doc.insert("llo").unwrap();
        assert_eq!(doc.buffer.line(0), "héllo");
        assert_eq!(doc.cursor(), 6);
        assert!(doc.dirty);
        assert_eq!(doc.version, 2);
    }

    #[test]
    fn enter_uses_the_files_line_ending() {
        let mut doc = Document {
            line_ending: LineEnding::CrLf,
            ..Document::untitled()
        };
        doc.insert("a").unwrap();
        doc.insert_newline().unwrap();
        assert_eq!(doc.buffer.slice(0..doc.buffer.len_bytes()), "a\r\n");
        assert_eq!(doc.buffer.len_lines(), 2);
    }

    #[test]
    fn typing_replaces_a_selection() {
        let mut doc = Document::untitled();
        doc.insert("hello world").unwrap();
        doc.selections.set_primary(Selection {
            anchor: 6,
            head: 11,
        });
        doc.insert("there").unwrap();
        assert_eq!(doc.buffer.line(0), "hello there");
    }

    fn doc(text: &str) -> Document {
        Document {
            buffer: RopeBuffer::from_text(text),
            ..Document::untitled()
        }
    }

    #[test]
    fn backspace_removes_whole_graphemes_and_crlf() {
        let mut d = doc("ab\r\ne\u{301}");
        d.move_cursor(Motion::DocEnd, 4, 10);
        d.delete_back().unwrap();
        assert_eq!(d.buffer.slice(0..99), "ab\r\n");
        d.delete_back().unwrap();
        assert_eq!(d.buffer.slice(0..99), "ab");
        assert_eq!(d.cursor(), 2);
        d.move_cursor(Motion::DocStart, 4, 10);
        assert!(
            d.delete_back().unwrap().is_none(),
            "nothing before the start"
        );
    }

    #[test]
    fn delete_forward_at_end_does_nothing() {
        let mut d = doc("x");
        d.move_cursor(Motion::DocEnd, 4, 10);
        assert!(d.delete_forward().unwrap().is_none());
        d.move_cursor(Motion::Left, 4, 10);
        d.delete_forward().unwrap();
        assert_eq!(d.buffer.len_bytes(), 0);
    }

    #[test]
    fn vertical_moves_keep_the_column_and_others_reset_it() {
        let mut d = doc("abcdef\nab\nabcdef");
        d.click(5);
        d.move_cursor(Motion::Down, 4, 10);
        assert_eq!(d.cursor(), 9);
        d.move_cursor(Motion::Down, 4, 10);
        assert_eq!(d.cursor(), 15);
        d.move_cursor(Motion::Left, 4, 10);
        assert_eq!(d.preferred_column, None);
    }

    #[test]
    fn page_down_moves_by_page_lines() {
        let mut d = doc("0\n1\n2\n3\n4\n5");
        d.move_cursor(Motion::PageDown, 4, 2);
        assert_eq!(d.buffer.byte_to_line(d.cursor()), 2);
    }

    #[test]
    fn click_snaps_inside_characters() {
        let mut d = doc("é\r\nx");
        d.click(1);
        assert_eq!(d.cursor(), 0);
        d.click(3);
        assert_eq!(d.cursor(), 2);
    }

    #[test]
    fn untitled_save_fails_and_stays_dirty() {
        let mut doc = Document::untitled();
        doc.insert("x").unwrap();
        assert!(doc.save().is_err());
        assert!(doc.dirty);
        assert_eq!(doc.display_name(), "untitled");
    }
}
