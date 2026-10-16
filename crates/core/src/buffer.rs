//! The text buffer interface every layer above `core` talks to.
//!
//! Two implementations exist: [`crate::rope::RopeBuffer`] for normal files
//! (M2) and [`crate::mapped::MappedBuffer`], a piece table over a
//! memory-mapped file, for huge files (M7). [`crate::any::AnyBuffer`] holds
//! either. All work in **byte offsets into UTF-8**, so cursors, undo, search
//! and rendering never need to know which one they are using (DESIGN.md
//! §4.1).
//!
//! Lines end at `\n` only. A `\r` directly before a `\n` belongs to the line
//! ending, so [`Text::line`] never returns it; a lone `\r` is ordinary text.
//!
//! A buffer may hold bytes that are not valid UTF-8 (a mapped file, M7).
//! Then a unit of text is either a valid UTF-8 character or one invalid
//! byte. Strings handed out ([`Text::line`], [`Text::slice`]) replace each
//! invalid byte by the one-byte character [`SUBSTITUTE`], so their lengths
//! and offsets are still the raw bytes'; readers that need the real bytes
//! (save, clipboard, search) use [`Text::read_into`] or
//! [`Text::chunk_at`].

use std::borrow::Cow;
use std::ops::Range;

use crate::mapped::PieceRun;

/// Stands in for each invalid byte in strings from [`Text::line`] and
/// [`Text::slice`]: one byte for one byte, so offsets stay the same. The
/// renderer draws it as `�`.
pub const SUBSTITUTE: char = '\u{1a}';

/// One line found from a byte position, without needing its line number
/// (which a mapped file may not know yet).
///
/// In the part of a mapped file not indexed yet, a very long line may be
/// cut into *pseudo-lines* (see [`crate::mapped`]) so that finding it never
/// means scanning the whole file. A pseudo-line has no line ending, so its
/// `end` equals its `next`; a real line always has `end < next`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineSpan {
    /// Where the line starts.
    pub start: usize,
    /// Where its content ends: before the `\n` or `\r\n`, or at `next` for
    /// a pseudo-line.
    pub end: usize,
    /// Where the next line starts; `None` on the last line.
    pub next: Option<usize>,
}

/// Read-only access to text. Implemented by buffers and by their snapshots.
pub trait Text {
    /// Total length in bytes.
    fn len_bytes(&self) -> usize;

    /// Number of lines: one more than the number of `\n`s, so an empty text
    /// has one empty line and a text ending in `\n` ends with an empty line.
    /// While a mapped file is still being indexed, only the lines starting
    /// at or before [`Text::indexed_bytes`] are counted: a lower bound.
    fn len_lines(&self) -> usize;

    /// Line `idx` without its line ending. Borrowed when the line is stored
    /// contiguously, which is almost always.
    ///
    /// Returns an empty string for an index past the last line.
    fn line(&self, idx: usize) -> Cow<'_, str>;

    /// The line containing byte offset `byte`. Offsets past the end map to
    /// the last line, and while a mapped file is being indexed, offsets
    /// past [`Text::indexed_bytes`] map to the last line counted.
    fn byte_to_line(&self, byte: usize) -> usize;

    /// Byte offset where line `line` starts. Lines past the end map to
    /// [`Text::len_bytes`].
    fn line_to_byte(&self, line: usize) -> usize;

    /// The text in `range`, clamped to the text and to char boundaries.
    fn slice(&self, range: Range<usize>) -> Cow<'_, str>;

    /// Whether `byte` is a valid cursor position: within the text and on a
    /// unit boundary, which is a UTF-8 character boundary in valid text and
    /// also either side of each invalid byte.
    fn is_char_boundary(&self, byte: usize) -> bool;

    /// The contiguous piece of storage containing byte `byte`, as raw
    /// bytes, and the offset where that piece starts. For readers that
    /// stream the whole text without copying it, such as the syntax parser.
    /// At or past the end it returns `(&[], len_bytes)`.
    fn chunk_at(&self, byte: usize) -> (&[u8], usize);

    /// The byte at `byte`, if within the text.
    fn byte_at(&self, byte: usize) -> Option<u8> {
        let (chunk, start) = self.chunk_at(byte);
        chunk.get(byte.checked_sub(start)?).copied()
    }

    /// The line containing `byte`. Offsets past the end give the last line.
    fn line_span(&self, byte: usize) -> LineSpan {
        let line = self.byte_to_line(byte);
        let start = self.line_to_byte(line);
        let next = (line + 1 < self.len_lines()).then(|| self.line_to_byte(line + 1));
        let end = match next {
            // Before the "\n", and a "\r" before it.
            Some(n) => {
                let lf = n - 1;
                if lf > start && self.byte_at(lf - 1) == Some(b'\r') {
                    lf - 1
                } else {
                    lf
                }
            }
            None => self.len_bytes(),
        };
        LineSpan { start, end, next }
    }

    /// The number of the line containing `byte`, or `None` if lines that
    /// far in are not counted yet (a mapped file still indexing): exactly
    /// when `byte` is past [`Text::indexed_bytes`] and that is short of
    /// the end.
    fn line_number(&self, byte: usize) -> Option<usize> {
        Some(self.byte_to_line(byte))
    }

    /// Line numbers are exact for every offset up to this one: the length,
    /// except in a mapped file whose line index is still being built.
    fn indexed_bytes(&self) -> usize {
        self.len_bytes()
    }

    /// Append the raw bytes in `range` (clamped to the text) to `out`.
    ///
    /// # Errors
    ///
    /// Returns the error from reading the file behind a mapped buffer; an
    /// in-memory buffer never fails.
    fn read_into(&self, range: Range<usize>, out: &mut Vec<u8>) -> std::io::Result<()> {
        let end = range.end.min(self.len_bytes());
        let mut pos = range.start.min(end);
        while pos < end {
            let (chunk, start) = self.chunk_at(pos);
            let Some(rest) = chunk.get(pos - start..) else {
                break;
            };
            if rest.is_empty() {
                break;
            }
            let take = rest.len().min(end - pos);
            out.extend_from_slice(&rest[..take]);
            pos += take;
        }
        Ok(())
    }

    /// Append [`Text::slice`] of `range` to `out`, reusing its storage.
    fn slice_into(&self, range: Range<usize>, out: &mut String) {
        out.push_str(&self.slice(range));
    }
}

/// Text removed or inserted by an edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// The bytes themselves. Always valid UTF-8 from typing and from an
    /// in-memory buffer; may hold invalid bytes from a mapped file.
    Bytes(Vec<u8>),
    /// A run of a mapped document's pieces, standing for text that is still
    /// in that document's storage: its original file and its add buffer.
    /// Deleting 1 GB from a mapped file records this instead of copying the
    /// gigabyte, and undo puts the same pieces back. Only a
    /// [`crate::mapped::MappedBuffer`] sharing that storage can put it back;
    /// other buffers refuse it with [`EditError::Unsupported`].
    Pieces(PieceRun),
}

impl Content {
    /// Length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Bytes(b) => b.len(),
            Self::Pieces(run) => run.len(),
        }
    }

    /// Whether there is nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The bytes, when they are held directly. `None` for
    /// [`Content::Pieces`], whose bytes would have to be read (see
    /// [`Content::read_into`]).
    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(b) => Some(b),
            Self::Pieces(_) => None,
        }
    }

    /// Whether it contains a line break. For [`Content::Pieces`] taken from
    /// a part of a file not indexed yet this scans the run (see
    /// [`PieceRun::has_newline`]).
    #[must_use]
    pub fn has_newline(&self) -> bool {
        match self {
            Self::Bytes(b) => b.contains(&b'\n'),
            Self::Pieces(run) => run.has_newline(),
        }
    }

    /// Append the bytes to `out`, reading them from the mapped document's
    /// storage for [`Content::Pieces`]. That can be a lot of bytes: a
    /// `Pieces` run exists because the text was too long to copy.
    ///
    /// # Errors
    ///
    /// Returns the error from reading the file behind a mapped document;
    /// bytes held directly never fail.
    pub fn read_into(&self, out: &mut Vec<u8>) -> std::io::Result<()> {
        match self {
            Self::Bytes(b) => {
                out.extend_from_slice(b);
                Ok(())
            }
            Self::Pieces(run) => run.read_into(out),
        }
    }
}

impl From<&str> for Content {
    fn from(s: &str) -> Self {
        Self::Bytes(s.as_bytes().to_vec())
    }
}

impl From<String> for Content {
    fn from(s: String) -> Self {
        Self::Bytes(s.into_bytes())
    }
}

impl PartialEq<str> for Content {
    fn eq(&self, other: &str) -> bool {
        self.as_bytes() == Some(other.as_bytes())
    }
}

impl PartialEq<&str> for Content {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

/// A record of one replacement, enough to undo it and to tell other systems
/// (syntax trees, search results) what moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// Where the replacement started, in bytes, in the text before the edit.
    pub start: usize,
    /// The text that was removed.
    pub removed: Content,
    /// The text that was inserted in its place.
    pub inserted: Content,
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

    /// How many bytes longer the text is after the edit (negative if
    /// shorter). Lengths fit in `isize`: no allocation can be larger.
    #[must_use]
    pub fn growth(&self) -> isize {
        let signed = |n: usize| isize::try_from(n).unwrap_or(isize::MAX);
        signed(self.inserted.len()) - signed(self.removed.len())
    }
}

/// Why an edit was refused. Each is a bug in the caller, but returning them
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
    /// The content cannot be put into this kind of buffer: bytes that are
    /// not UTF-8 into an in-memory buffer, or [`Content::Pieces`] into any
    /// buffer other than the mapped one whose storage they refer to.
    Unsupported,
    /// In [`Buffer::replace_sorted`], item `index` starts before the item
    /// in front of it ends: the items are out of order or overlap.
    Unsorted {
        /// The offending item's position in the list.
        index: usize,
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
            EditError::Unsupported => write!(f, "this buffer cannot hold that text"),
            EditError::Unsorted { index } => {
                write!(
                    f,
                    "edit {index} overlaps or comes before the one ahead of it"
                )
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

    /// Replace `range` with `content`, for undo and redo replaying an
    /// [`Edit`]. Unlike [`Buffer::replace`], only the bounds are checked:
    /// a deletion can join invalid bytes into a valid character, so the
    /// place to put them back is no longer a character boundary.
    ///
    /// # Errors
    ///
    /// Returns [`EditError`] if `range` is outside the text, or the content
    /// cannot be held by this buffer.
    fn put(&mut self, range: Range<usize>, content: &Content) -> Result<Edit, EditError> {
        let bytes = content.as_bytes().ok_or(EditError::Unsupported)?;
        let text = std::str::from_utf8(bytes).map_err(|_| EditError::Unsupported)?;
        self.replace(range, text)
    }

    /// A read-only copy for use on another thread.
    fn snapshot(&self) -> Box<dyn Snapshot>;

    /// Whether every line has been counted, so that [`Text::len_lines`] is
    /// exact and [`Text::indexed_bytes`] is the length. Always true for
    /// in-memory buffers; a mapped buffer indexes in the background (M7),
    /// and until it finishes [`Text::len_lines`] is a lower bound.
    fn is_fully_indexed(&self) -> bool {
        true
    }

    /// Apply many replacements at once, such as Replace All. Each item pairs
    /// a range with its new text. Ranges refer to the text **before any of
    /// them** is applied, must be in order and must not overlap (an
    /// insertion may touch its neighbours: `2..2` after `0..2` is fine).
    ///
    /// The returned edits are in the order they were applied, which is from
    /// the last item to the first: applying from the end leaves the earlier
    /// offsets valid, so every edit's `start` is its item's `range.start`.
    /// [`crate::History::record_group`] takes them as they are.
    ///
    /// All items are checked before any is applied, so on error the buffer
    /// is unchanged. The default applies them one by one with
    /// [`Buffer::replace`]; a mapped buffer overrides it to rebuild its piece
    /// list once instead of once per item.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::Unsorted`] for items out of order or
    /// overlapping, and the [`Buffer::replace`] errors for a bad range.
    fn replace_sorted(&mut self, items: &[(Range<usize>, &str)]) -> Result<Vec<Edit>, EditError> {
        check_sorted(self, items.iter().map(|(r, _)| r), true)?;
        let mut edits: Vec<Edit> = Vec::with_capacity(items.len());
        for (range, text) in items.iter().rev() {
            match self.replace(range.clone(), text) {
                Ok(edit) => edits.push(edit),
                Err(e) => {
                    // Unreachable after the checks above, but if a backend
                    // disagrees, put back what was done so the buffer is
                    // as it was, as promised.
                    for done in edits.iter().rev() {
                        if self.put(done.new_range(), &done.removed).is_err() {
                            break;
                        }
                    }
                    return Err(e);
                }
            }
        }
        Ok(edits)
    }

    /// [`Buffer::replace_sorted`] for [`Buffer::put`]: many contents put at
    /// once, for undoing and redoing a Replace All. The rules for `items`
    /// and the order of the returned edits are the same; as with
    /// [`Buffer::put`], only the bounds are checked, not character
    /// boundaries. On error the buffer is unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::Unsorted`] for items out of order or
    /// overlapping, and the [`Buffer::put`] errors.
    fn put_sorted(&mut self, items: &[(Range<usize>, &Content)]) -> Result<Vec<Edit>, EditError> {
        check_sorted(self, items.iter().map(|(r, _)| r), false)?;
        let mut edits: Vec<Edit> = Vec::with_capacity(items.len());
        for (range, content) in items.iter().rev() {
            match self.put(range.clone(), content) {
                Ok(edit) => edits.push(edit),
                Err(e) => {
                    // A content this buffer cannot hold: put back what was
                    // done so the buffer is as it was.
                    for done in edits.iter().rev() {
                        if self.put(done.new_range(), &done.removed).is_err() {
                            break;
                        }
                    }
                    return Err(e);
                }
            }
        }
        Ok(edits)
    }
}

/// Call `f` with each edit of a [`Buffer::replace_sorted`] or
/// [`Buffer::put_sorted`] result in text order, first to last, with each
/// start moved to where it is once the edits before it are in place.
///
/// The result lists edits last first, each start in the text before any
/// of them. Told in that order to something that reads the text, such as
/// the syntax tree, each edit would be read against a text that already
/// holds the edits after it. Told first to last instead, everything up to
/// an edit's new end is already final, so the text after the whole batch
/// serves for each one. The starts are moved back afterwards.
pub fn report_sorted(edits: &mut [Edit], mut f: impl FnMut(&Edit)) {
    let mut shift = 0isize;
    for edit in edits.iter_mut().rev() {
        let start = edit.start;
        edit.start = start.saturating_add_signed(shift);
        f(edit);
        edit.start = start;
        shift += edit.growth();
    }
}

/// Check that `ranges` suit [`Buffer::replace_sorted`]: every range inside
/// `text` (and on character boundaries if `boundaries`), and each starting
/// at or after the end of the one before.
pub(crate) fn check_sorted<'a, T: Text + ?Sized>(
    text: &T,
    ranges: impl IntoIterator<Item = &'a Range<usize>>,
    boundaries: bool,
) -> Result<(), EditError> {
    let mut prev_end = 0;
    for (index, range) in ranges.into_iter().enumerate() {
        if boundaries {
            check_range(text, range)?;
        } else if range.start > range.end || range.end > text.len_bytes() {
            return Err(EditError::OutOfBounds {
                range: range.clone(),
                len: text.len_bytes(),
            });
        }
        if range.start < prev_end {
            return Err(EditError::Unsorted { index });
        }
        prev_end = range.end;
    }
    Ok(())
}

/// Check that `range` is a valid edit range for `text`.
pub(crate) fn check_range<T: Text + ?Sized>(
    text: &T,
    range: &Range<usize>,
) -> Result<(), EditError> {
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
