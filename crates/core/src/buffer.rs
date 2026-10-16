//! The text buffer interface every layer above `core` talks to.
//!
//! Two implementations exist or will: [`crate::rope::RopeBuffer`] for normal
//! files (M2) and a memory-mapped piece table for huge files (M7). Both work
//! in **byte offsets into UTF-8**, so cursors, undo, search and rendering
//! never need to know which one they are using (DESIGN.md §4.1).
//!
//! Lines end at `\n` only. A `\r` directly before a `\n` belongs to the line
//! ending, so [`Text::line`] never returns it; a lone `\r` is ordinary text.

use std::borrow::Cow;
use std::ops::Range;

/// Read-only access to text. Implemented by buffers and by their snapshots.
pub trait Text {
    /// Total length in bytes.
    fn len_bytes(&self) -> usize;

    /// Number of lines: one more than the number of `\n`s, so an empty text
    /// has one empty line and a text ending in `\n` ends with an empty line.
    fn len_lines(&self) -> usize;

    /// Line `idx` without its line ending. Borrowed when the line is stored
    /// contiguously, which is almost always.
    ///
    /// Returns an empty string for an index past the last line.
    fn line(&self, idx: usize) -> Cow<'_, str>;

    /// The line containing byte offset `byte`. Offsets past the end map to
    /// the last line.
    fn byte_to_line(&self, byte: usize) -> usize;

    /// Byte offset where line `line` starts. Lines past the end map to
    /// [`Text::len_bytes`].
    fn line_to_byte(&self, line: usize) -> usize;

    /// The text in `range`, clamped to the text and to char boundaries.
    fn slice(&self, range: Range<usize>) -> Cow<'_, str>;

    /// Whether `byte` is a valid cursor position: within the text and on a
    /// UTF-8 character boundary.
    fn is_char_boundary(&self, byte: usize) -> bool;
}

/// A record of one replacement, enough to undo it and to tell other systems
/// (syntax trees, search results) what moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// Where the replacement started, in bytes, in the text before the edit.
    pub start: usize,
    /// The text that was removed.
    pub removed: String,
    /// The text that was inserted in its place.
    pub inserted: String,
}

impl Edit {
    /// Byte range of the removed text in the old text.
    #[must_use]
    pub fn old_range(&self) -> Range<usize> {
        self.start..self.start + self.removed.len()
    }

    /// Byte range of the inserted text in the new text.
    #[must_use]
    pub fn new_range(&self) -> Range<usize> {
        self.start..self.start + self.inserted.len()
    }
}

/// Why an edit was refused. Both are bugs in the caller, but returning them
/// beats crashing with unsaved work in the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// The range ends past the end of the text, or starts after it ends.
    OutOfBounds {
        /// The requested range.
        range: Range<usize>,
        /// The text length.
        len: usize,
    },
    /// An end of the range falls inside a multi-byte character.
    NotCharBoundary {
        /// The offending offset.
        offset: usize,
    },
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::OutOfBounds { range, len } => {
                write!(f, "edit range {range:?} is outside text of length {len}")
            }
            EditError::NotCharBoundary { offset } => {
                write!(f, "edit offset {offset} is inside a character")
            }
        }
    }
}

impl std::error::Error for EditError {}

/// A read-only copy of a buffer that another thread can own while the editor
/// keeps editing. Cheap to make: ropes share structure, so a snapshot of a
/// 10 MB file costs a few pointer copies.
pub trait Snapshot: Text + Send + Sync {}

impl<T: Text + Send + Sync> Snapshot for T {}

/// An editable text buffer.
pub trait Buffer: Text {
    /// Replace `range` with `text`, returning a record of the change.
    ///
    /// # Errors
    ///
    /// Returns [`EditError`] if `range` is outside the text or does not fall
    /// on character boundaries. The buffer is unchanged in that case.
    fn replace(&mut self, range: Range<usize>, text: &str) -> Result<Edit, EditError>;

    /// A read-only copy for use on another thread.
    fn snapshot(&self) -> Box<dyn Snapshot>;

    /// Whether every line has been located. Always true for in-memory
    /// buffers; a mapped buffer indexes in the background (M7), and until it
    /// finishes [`Text::len_lines`] is a lower bound.
    fn is_fully_indexed(&self) -> bool {
        true
    }
}

/// Check that `range` is a valid edit range for `text`.
pub(crate) fn check_range(text: &dyn Text, range: &Range<usize>) -> Result<(), EditError> {
    let len = text.len_bytes();
    if range.start > range.end || range.end > len {
        return Err(EditError::OutOfBounds {
            range: range.clone(),
            len,
        });
    }
    for offset in [range.start, range.end] {
        if !text.is_char_boundary(offset) {
            return Err(EditError::NotCharBoundary { offset });
        }
    }
    Ok(())
}
