//! One open file: its text, where it lives on disk, the selection, its undo
//! history, and whether it has unsaved changes.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Instant;

use nimbus_core::movement;
use nimbus_core::{
    Buffer, Edit, EditError, History, LineEnding, RopeBuffer, Selection, Selections, Text,
};

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
    /// The file had a UTF-8 byte order mark, written back on save.
    pub bom: bool,
    /// Increments on every change to the text. Background work (search,
    /// parsing) records the version it read, so stale results can be
    /// recognised and dropped.
    pub version: u64,
    /// Undo and redo.
    pub history: History,
    /// [`History::top_id`] when last saved or opened. The document is dirty
    /// whenever the top of the history differs, so undoing back to the saved
    /// point makes it clean again.
    saved_at: u64,
}

impl Document {
    /// An empty untitled document.
    #[must_use]
    pub fn untitled() -> Self {
        Self::default()
    }

    /// An untitled document holding `text`, unmodified.
    #[cfg(test)]
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Self {
            buffer: RopeBuffer::from_text(text),
            ..Self::default()
        }
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

    /// Whether there are unsaved changes.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.history.top_id() != self.saved_at
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
        // Typing after a save must start a new undo step, or it would join
        // the saved step and the document would look clean.
        self.history.seal();
        self.saved_at = self.history.top_id();
        Ok(())
    }

    /// The cursor's byte offset, always a valid position in the text.
    #[must_use]
    pub fn cursor(&self) -> usize {
        movement::clamp(&self.buffer, self.selections.primary().head)
    }

    /// The primary selection, snapped to valid positions.
    #[must_use]
    pub fn selection(&self) -> Selection {
        let s = self.selections.primary();
        Selection {
            anchor: movement::clamp(&self.buffer, s.anchor),
            head: movement::clamp(&self.buffer, s.head),
        }
    }

    fn set_selection(&mut self, sel: Selection) {
        debug_assert!(
            self.buffer.is_char_boundary(sel.head),
            "cursor off a char boundary"
        );
        self.selections.set_primary(sel);
    }

    /// Replace `range` with `text`, put the cursor after it, and record the
    /// change for undo.
    ///
    /// # Errors
    ///
    /// Returns the buffer's [`EditError`] for an invalid range; nothing
    /// changes in that case.
    pub fn replace(&mut self, range: Range<usize>, text: &str) -> Result<Edit, EditError> {
        let before = self.selections.clone();
        let edit = self.buffer.replace(range, text)?;
        self.set_selection(Selection::cursor(edit.new_range().end));
        self.history
            .record(edit.clone(), &before, &self.selections, Instant::now());
        self.preferred_column = None;
        self.version += 1;
        Ok(edit)
    }

    /// Apply several replacements as one undo step. `items` pairs each
    /// range with its new text; ranges must be sorted, must not overlap, and
    /// refer to the text before any of them. The cursor ends after the last
    /// replacement. Returns how many were applied.
    ///
    /// # Errors
    ///
    /// Returns the buffer's [`EditError`] if a range is invalid. The ones
    /// applied before it (later in the text) stay applied and recorded.
    pub fn replace_many(&mut self, items: &[(Range<usize>, String)]) -> Result<usize, EditError> {
        let before = self.selections.clone();
        let mut edits = Vec::with_capacity(items.len());
        // Apply from the end so earlier offsets stay valid; record in
        // application order, which is what undo reverses.
        let mut result = Ok(());
        for (range, text) in items.iter().rev() {
            match self.buffer.replace(range.clone(), text) {
                Ok(edit) => edits.push(edit),
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        let count = edits.len();
        if count > 0 {
            // The last item lands after every applied item before it has
            // grown or shrunk the text.
            let applied = &items[items.len() - count..];
            let (last_range, last_text) = &applied[applied.len() - 1];
            let grown: usize = applied[..applied.len() - 1]
                .iter()
                .map(|(_, t)| t.len())
                .sum();
            let shrunk: usize = applied[..applied.len() - 1]
                .iter()
                .map(|(r, _)| r.len())
                .sum();
            let end = (last_range.start + last_text.len() + grown).saturating_sub(shrunk);
            self.set_selection(Selection::cursor(movement::clamp(&self.buffer, end)));
            self.history.record_group(edits, &before, &self.selections);
            self.version += 1;
        }
        result.map(|()| count)
    }

    /// Type `text` at the cursor, replacing any selection.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn insert(&mut self, text: &str) -> Result<Edit, EditError> {
        self.replace(self.selection().range(), text)
    }

    /// Paste: like typing, but line endings are converted to the file's.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn paste(&mut self, text: &str) -> Result<Edit, EditError> {
        let unified = text.replace("\r\n", "\n");
        let converted = match self.line_ending {
            LineEnding::Lf => unified,
            LineEnding::CrLf => unified.replace('\n', "\r\n"),
        };
        // A paste is one undo step even when it is a single character.
        self.history.seal();
        let edit = self.insert(&converted);
        self.history.seal();
        edit
    }

    /// Enter: insert the line ending, then the current line's indentation,
    /// so a new line starts where the previous one did.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn insert_newline(&mut self) -> Result<Edit, EditError> {
        let sel = self.selection().range();
        let line = self.buffer.line(self.buffer.byte_to_line(sel.start));
        let line_start = movement::line_start(&self.buffer, sel.start);
        let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
        // Only the indentation before the cursor carries over.
        let indent = &line[..indent_len.min(sel.start - line_start)];
        let text = format!("{}{indent}", self.line_ending.as_str());
        self.insert(&text)
    }

    /// The selected text, or `None` if the selection is empty.
    #[must_use]
    pub fn selected_text(&self) -> Option<String> {
        let range = self.selection().range();
        (!range.is_empty()).then(|| self.buffer.slice(range).into_owned())
    }

    /// Put the cursor at `pos` (snapped to a valid position), as a click
    /// does; with `extend`, move only the head, as a Shift+click does.
    pub fn click(&mut self, pos: usize, extend: bool) {
        let pos = movement::clamp(&self.buffer, pos);
        let sel = self.selection().moved_to(pos, extend);
        self.set_selection(sel);
        self.preferred_column = None;
        self.history.seal();
    }

    /// Select `range` with the cursor at its end (double- and triple-click).
    pub fn select(&mut self, range: Range<usize>) {
        let t = &self.buffer;
        let sel = Selection {
            anchor: movement::clamp(t, range.start),
            head: movement::clamp(t, range.end),
        };
        self.set_selection(sel);
        self.preferred_column = None;
        self.history.seal();
    }

    /// Select from `anchor` to `head`, keeping the direction: the cursor is
    /// at `head`. Used when dragging with the mouse.
    pub fn select_span(&mut self, anchor: usize, head: usize) {
        let t = &self.buffer;
        let sel = Selection {
            anchor: movement::clamp(t, anchor),
            head: movement::clamp(t, head),
        };
        self.set_selection(sel);
        self.preferred_column = None;
    }

    /// Select the whole document.
    pub fn select_all(&mut self) {
        self.select(0..self.buffer.len_bytes());
    }

    /// Move the cursor. With `extend` the selection grows or shrinks;
    /// without it, a selection collapses (Left and Right collapse to the
    /// selection's edge instead of moving). `page_lines` is one screenful.
    pub fn move_cursor(
        &mut self,
        motion: Motion,
        extend: bool,
        tab_width: usize,
        page_lines: usize,
    ) {
        let sel = self.selection();
        self.history.seal();
        if !extend && !sel.is_empty() && matches!(motion, Motion::Left | Motion::Right) {
            let r = sel.range();
            let pos = if motion == Motion::Left {
                r.start
            } else {
                r.end
            };
            self.set_selection(Selection::cursor(pos));
            self.preferred_column = None;
            return;
        }
        let t = &self.buffer;
        let pos = sel.head;
        let page = isize::try_from(page_lines.max(1)).unwrap_or(isize::MAX);
        let vertical =
            |lines: isize| movement::vertical(t, pos, self.preferred_column, lines, tab_width);
        let (new_pos, preferred) = match motion {
            Motion::Left => (movement::prev_grapheme(t, pos), None),
            Motion::Right => (movement::next_grapheme(t, pos), None),
            Motion::WordLeft => (movement::word_left(t, pos), None),
            Motion::WordRight => (movement::word_right(t, pos), None),
            Motion::LineStart => (movement::smart_home(t, pos), None),
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
        self.set_selection(sel.moved_to(new_pos, extend));
        self.preferred_column = preferred;
    }

    /// Like [`Document::move_cursor`] with soft wrap on: Up, Down and the
    /// Page keys move by visual row; `preferred_column` is then a column
    /// within the row. Other motions are the same as unwrapped.
    pub fn move_cursor_wrapped(
        &mut self,
        motion: Motion,
        extend: bool,
        w: &mut nimbus_core::wrap::WrapView<'_>,
        page_lines: usize,
    ) {
        let rows = match motion {
            Motion::Up => -1,
            Motion::Down => 1,
            Motion::PageUp => -isize::try_from(page_lines.max(1)).unwrap_or(isize::MAX),
            Motion::PageDown => isize::try_from(page_lines.max(1)).unwrap_or(isize::MAX),
            _ => return self.move_cursor(motion, extend, w.tab_width(), page_lines),
        };
        let sel = self.selection();
        self.history.seal();
        let v = movement::vertical_rows(&self.buffer, w, sel.head, self.preferred_column, rows);
        self.set_selection(sel.moved_to(v.pos, extend));
        self.preferred_column = Some(v.preferred_col);
    }

    /// Backspace: delete the selection, or the grapheme before the cursor.
    /// At the start of a line this joins it to the previous one, removing a
    /// `\r\n` as one unit. Returns `None` when there is nothing to delete.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn delete_back(&mut self) -> Result<Option<Edit>, EditError> {
        let sel = self.selection();
        let range = if sel.is_empty() {
            movement::prev_grapheme(&self.buffer, sel.head)..sel.head
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
        let sel = self.selection();
        let range = if sel.is_empty() {
            sel.head..movement::next_grapheme(&self.buffer, sel.head)
        } else {
            sel.range()
        };
        if range.is_empty() {
            return Ok(None);
        }
        self.replace(range, "").map(Some)
    }

    /// Lines touched by the selection. A selection ending at the very start
    /// of a line does not include that line.
    fn selected_lines(&self) -> Range<usize> {
        let r = self.selection().range();
        let first = self.buffer.byte_to_line(r.start);
        let mut last = self.buffer.byte_to_line(r.end);
        if last > first && self.buffer.line_to_byte(last) == r.end {
            last -= 1;
        }
        first..last + 1
    }

    /// Tab with a multi-line selection: indent every selected line by one
    /// tab, as one undo step, keeping the lines selected. Returns `false`
    /// (doing nothing) when the selection is within one line; the caller
    /// then types a tab instead.
    ///
    /// # Errors
    ///
    /// See [`Document::replace_many`].
    pub fn indent_lines(&mut self) -> Result<bool, EditError> {
        let lines = self.selected_lines();
        if lines.len() < 2 {
            return Ok(false);
        }
        let starts: Vec<(Range<usize>, String)> = lines
            .clone()
            .map(|l| {
                let s = self.buffer.line_to_byte(l);
                (s..s, "\t".to_owned())
            })
            .collect();
        self.replace_many(&starts)?;
        self.select_lines(lines);
        Ok(true)
    }

    /// Shift+Tab: remove one level of indentation (a tab, or up to
    /// `tab_width` spaces) from every selected line, as one undo step.
    ///
    /// # Errors
    ///
    /// See [`Document::replace_many`].
    pub fn outdent_lines(&mut self, tab_width: usize) -> Result<(), EditError> {
        let lines = self.selected_lines();
        let ranges: Vec<(Range<usize>, String)> = lines
            .clone()
            .filter_map(|l| {
                let text = self.buffer.line(l);
                let n = if text.starts_with('\t') {
                    1
                } else {
                    text.bytes()
                        .take(tab_width)
                        .take_while(|b| *b == b' ')
                        .count()
                };
                let s = self.buffer.line_to_byte(l);
                (n > 0).then(|| (s..s + n, String::new()))
            })
            .collect();
        if ranges.is_empty() {
            return Ok(());
        }
        self.replace_many(&ranges)?;
        if lines.len() > 1 {
            self.select_lines(lines);
        }
        Ok(())
    }

    fn select_lines(&mut self, lines: Range<usize>) {
        let start = self.buffer.line_to_byte(lines.start);
        let end = movement::line_end(&self.buffer, self.buffer.line_to_byte(lines.end - 1));
        self.select(start..end);
    }

    /// Undo the last step. Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// Returns an [`EditError`] if the history no longer matches the text,
    /// which would be a bug.
    pub fn undo(&mut self) -> Result<bool, EditError> {
        let Some(restored) = self.history.undo(&mut self.buffer)? else {
            return Ok(false);
        };
        self.restore(&restored);
        Ok(true)
    }

    /// Redo the last undone step. Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// As for [`Document::undo`].
    pub fn redo(&mut self) -> Result<bool, EditError> {
        let Some(restored) = self.history.redo(&mut self.buffer)? else {
            return Ok(false);
        };
        self.restore(&restored);
        Ok(true)
    }

    fn restore(&mut self, restored: &Selections) {
        let p = restored.primary();
        self.selections = Selections::single(Selection {
            anchor: movement::clamp(&self.buffer, p.anchor),
            head: movement::clamp(&self.buffer, p.head),
        });
        self.preferred_column = None;
        self.version += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> Document {
        Document::from_text(text)
    }

    fn text(d: &Document) -> String {
        d.buffer.slice(0..d.buffer.len_bytes()).into_owned()
    }

    #[test]
    fn typing_moves_the_cursor_and_marks_dirty() {
        let mut d = Document::untitled();
        assert!(!d.is_dirty());
        d.insert("hé").unwrap();
        d.insert("llo").unwrap();
        assert_eq!(d.buffer.line(0), "héllo");
        assert_eq!(d.cursor(), 6);
        assert!(d.is_dirty());
        assert_eq!(d.version, 2);
    }

    #[test]
    fn undoing_back_to_the_start_is_clean_again() {
        let mut d = doc("abc");
        d.click(3, false);
        d.insert("d").unwrap();
        assert!(d.is_dirty());
        d.undo().unwrap();
        assert!(!d.is_dirty());
        assert_eq!(d.cursor(), 3, "undo restores the cursor");
    }

    #[test]
    fn enter_uses_the_line_ending_and_keeps_indentation() {
        let mut d = doc("\t  foo");
        d.line_ending = LineEnding::CrLf;
        d.click(6, false);
        d.insert_newline().unwrap();
        assert_eq!(text(&d), "\t  foo\r\n\t  ");
        d.click(1, false);
        d.insert_newline().unwrap();
        assert_eq!(
            text(&d),
            "\t\r\n\t  foo\r\n\t  ",
            "only indentation before the cursor"
        );
    }

    #[test]
    fn typing_replaces_a_selection() {
        let mut d = doc("hello world");
        d.select(6..11);
        d.insert("there").unwrap();
        assert_eq!(d.buffer.line(0), "hello there");
    }

    #[test]
    fn shift_movement_extends_and_plain_left_right_collapse() {
        let mut d = doc("abcdef");
        d.click(1, false);
        d.move_cursor(Motion::Right, true, 4, 10);
        d.move_cursor(Motion::Right, true, 4, 10);
        assert_eq!(d.selection().range(), 1..3);
        assert_eq!(d.selected_text().as_deref(), Some("bc"));
        d.move_cursor(Motion::Left, false, 4, 10);
        assert_eq!(
            d.selection(),
            Selection::cursor(1),
            "collapses to the start"
        );
    }

    #[test]
    fn paste_converts_line_endings_and_is_one_step() {
        let mut d = doc("");
        d.line_ending = LineEnding::CrLf;
        d.insert("x").unwrap();
        d.paste("a\nb\r\nc").unwrap();
        assert_eq!(text(&d), "xa\r\nb\r\nc");
        d.undo().unwrap();
        assert_eq!(text(&d), "x");
    }

    #[test]
    fn indent_and_outdent_selected_lines_as_one_step() {
        let mut d = doc("a\n    b\nc");
        d.select(0..7);
        assert!(d.indent_lines().unwrap());
        assert_eq!(text(&d), "\ta\n\t    b\nc");
        d.outdent_lines(4).unwrap();
        assert_eq!(text(&d), "a\n    b\nc");
        d.outdent_lines(4).unwrap();
        assert_eq!(text(&d), "a\nb\nc");
        d.undo().unwrap();
        assert_eq!(text(&d), "a\n    b\nc");
        d.click(0, false);
        assert!(
            !d.indent_lines().unwrap(),
            "single line: caller types a tab"
        );
    }

    #[test]
    fn replace_many_is_one_step_with_cursor_after_the_last() {
        let mut d = doc("a-b-c");
        let items = [(1..2, "--".to_owned()), (3..4, "--".to_owned())];
        assert_eq!(d.replace_many(&items).unwrap(), 2);
        assert_eq!(text(&d), "a--b--c");
        assert_eq!(d.cursor(), 6);
        d.undo().unwrap();
        assert_eq!(text(&d), "a-b-c");
    }

    #[test]
    fn backspace_removes_whole_graphemes_and_crlf() {
        let mut d = doc("ab\r\ne\u{301}");
        d.move_cursor(Motion::DocEnd, false, 4, 10);
        d.delete_back().unwrap();
        assert_eq!(text(&d), "ab\r\n");
        d.delete_back().unwrap();
        assert_eq!(text(&d), "ab");
        d.move_cursor(Motion::DocStart, false, 4, 10);
        assert!(d.delete_back().unwrap().is_none());
    }

    #[test]
    fn vertical_moves_keep_the_column_and_others_reset_it() {
        let mut d = doc("abcdef\nab\nabcdef");
        d.click(5, false);
        d.move_cursor(Motion::Down, false, 4, 10);
        assert_eq!(d.cursor(), 9);
        d.move_cursor(Motion::Down, false, 4, 10);
        assert_eq!(d.cursor(), 15);
        d.move_cursor(Motion::Left, false, 4, 10);
        assert_eq!(d.preferred_column, None);
    }

    #[test]
    fn click_snaps_inside_characters() {
        let mut d = doc("é\r\nx");
        d.click(1, false);
        assert_eq!(d.cursor(), 0);
        d.click(3, false);
        assert_eq!(d.cursor(), 2);
    }

    #[test]
    fn untitled_save_fails_and_stays_dirty() {
        let mut d = Document::untitled();
        d.insert("x").unwrap();
        assert!(d.save().is_err());
        assert!(d.is_dirty());
        assert_eq!(d.display_name(), "untitled");
    }
}
