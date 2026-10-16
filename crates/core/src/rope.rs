//! [`RopeBuffer`]: the buffer for files that fit comfortably in memory.
//!
//! A *rope* stores text as a balanced tree of small chunks (around 1 KB)
//! instead of one big string. Inserting in the middle of a 10 MB file touches
//! one chunk and a few tree nodes, not 10 MB of memory, and every node also
//! records how many bytes, chars and line breaks sit under it, so finding
//! line 50,000 is a short walk down the tree.
//!
//! Ropey counts in `char`s internally. Everything outside this file uses
//! byte offsets, so all conversions happen here.

use std::borrow::Cow;
use std::ops::Range;

use ropey::{Rope, RopeSlice};

use crate::buffer::{Buffer, Edit, EditError, Snapshot, Text, check_range};

/// A rope-backed text buffer.
#[derive(Debug, Clone, Default)]
pub struct RopeBuffer {
    rope: Rope,
}

impl RopeBuffer {
    /// A buffer holding `text`.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
        }
    }

    /// Build a buffer by streaming UTF-8 from `reader`, without first holding
    /// the whole text in one string. Opening a 10 MB file this way peaks at
    /// about the size of the rope instead of twice the file size.
    ///
    /// # Errors
    ///
    /// Returns the reader's error, or [`std::io::ErrorKind::InvalidData`] if
    /// the bytes are not valid UTF-8.
    pub fn from_reader(reader: impl std::io::Read) -> std::io::Result<Self> {
        Rope::from_reader(reader).map(|rope| Self { rope })
    }

    /// Iterate the text as contiguous `&str` chunks, in order. Writing these
    /// one after another reproduces the text exactly; used for saving.
    pub fn chunks(&self) -> impl Iterator<Item = &str> {
        self.rope.chunks()
    }
}

/// Borrow a slice if it is one contiguous chunk, otherwise copy it.
fn cow(slice: RopeSlice<'_>) -> Cow<'_, str> {
    match slice.as_str() {
        Some(s) => Cow::Borrowed(s),
        None => Cow::Owned(slice.to_string()),
    }
}

/// Strip a trailing `\n` and a `\r` before it.
fn without_line_ending(slice: RopeSlice<'_>) -> RopeSlice<'_> {
    let mut end = slice.len_bytes();
    if end > 0 && slice.byte(end - 1) == b'\n' {
        end -= 1;
        if end > 0 && slice.byte(end - 1) == b'\r' {
            end -= 1;
        }
    }
    slice.byte_slice(..end)
}

impl Text for RopeBuffer {
    fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    fn line(&self, idx: usize) -> Cow<'_, str> {
        if idx >= self.rope.len_lines() {
            return Cow::Borrowed("");
        }
        cow(without_line_ending(self.rope.line(idx)))
    }

    fn byte_to_line(&self, byte: usize) -> usize {
        self.rope.byte_to_line(byte.min(self.rope.len_bytes()))
    }

    fn line_to_byte(&self, line: usize) -> usize {
        if line >= self.rope.len_lines() {
            return self.rope.len_bytes();
        }
        self.rope.line_to_byte(line)
    }

    fn slice(&self, range: Range<usize>) -> Cow<'_, str> {
        let len = self.rope.len_bytes();
        let mut start = range.start.min(len);
        let mut end = range.end.min(len).max(start);
        while !self.is_char_boundary(start) {
            start -= 1;
        }
        while !self.is_char_boundary(end) {
            end += 1;
        }
        cow(self.rope.byte_slice(start..end))
    }

    fn is_char_boundary(&self, byte: usize) -> bool {
        match self.rope.try_byte_to_char(byte) {
            Ok(ch) => self.rope.char_to_byte(ch) == byte,
            Err(_) => false,
        }
    }
}

impl Buffer for RopeBuffer {
    fn replace(&mut self, range: Range<usize>, text: &str) -> Result<Edit, EditError> {
        check_range(self, &range)?;
        let start_char = self.rope.byte_to_char(range.start);
        let end_char = self.rope.byte_to_char(range.end);
        let removed = self.rope.slice(start_char..end_char).to_string();
        self.rope.remove(start_char..end_char);
        self.rope.insert(start_char, text);
        Ok(Edit {
            start: range.start,
            removed,
            inserted: text.to_owned(),
        })
    }

    fn snapshot(&self) -> Box<dyn Snapshot> {
        // Cloning a rope copies one pointer; the chunks are shared and
        // copy-on-write, so later edits here do not affect the snapshot.
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_strip_lf_and_crlf_but_keep_lone_cr() {
        let b = RopeBuffer::from_text("one\r\ntwo\nthr\ree\n");
        assert_eq!(b.len_lines(), 4);
        assert_eq!(b.line(0), "one");
        assert_eq!(b.line(1), "two");
        assert_eq!(b.line(2), "thr\ree");
        assert_eq!(b.line(3), "");
        assert_eq!(b.line(99), "");
    }

    #[test]
    fn empty_text_has_one_empty_line() {
        let b = RopeBuffer::default();
        assert_eq!(b.len_lines(), 1);
        assert_eq!(b.line(0), "");
        assert_eq!(b.line_to_byte(0), 0);
        assert_eq!(b.byte_to_line(0), 0);
    }

    #[test]
    fn line_and_byte_conversions_round_trip() {
        let b = RopeBuffer::from_text("ab\ncdé\n\nf");
        assert_eq!(b.line_to_byte(1), 3);
        assert_eq!(b.line_to_byte(2), 8);
        assert_eq!(b.line_to_byte(3), 9);
        assert_eq!(b.line_to_byte(99), b.len_bytes());
        assert_eq!(b.byte_to_line(4), 1);
        assert_eq!(b.byte_to_line(9), 3);
        assert_eq!(b.byte_to_line(1000), 3);
    }

    #[test]
    fn replace_records_the_edit() {
        let mut b = RopeBuffer::from_text("hello world");
        let e = b.replace(6..11, "rope").unwrap();
        assert_eq!(b.slice(0..b.len_bytes()), "hello rope");
        assert_eq!(e.removed, "world");
        assert_eq!(e.new_range(), 6..10);
    }

    #[test]
    fn replace_rejects_bad_ranges_and_leaves_text_alone() {
        let mut b = RopeBuffer::from_text("café");
        assert!(matches!(
            b.replace(4..4, "x"),
            Err(EditError::NotCharBoundary { offset: 4 })
        ));
        assert!(matches!(
            b.replace(2..9, ""),
            Err(EditError::OutOfBounds { .. })
        ));
        assert_eq!(b.slice(0..99), "café");
    }

    #[test]
    fn slice_widens_to_char_boundaries() {
        let b = RopeBuffer::from_text("café!");
        assert_eq!(b.slice(4..5), "é");
    }

    #[test]
    fn snapshots_do_not_see_later_edits() {
        let mut b = RopeBuffer::from_text("before");
        let snap = b.snapshot();
        b.replace(0..6, "after").unwrap();
        assert_eq!(snap.line(0), "before");
        assert_eq!(b.line(0), "after");
    }
}
