//! [`AnyBuffer`]: one type for a document's buffer, whichever backend it is.
//!
//! A document opens as a [`RopeBuffer`] or a [`MappedBuffer`] depending on
//! the file (M7 task 9), but the code holding it (`Document` in the app)
//! needs a single type. Rust offers two ways to say "one of several types
//! that share a trait": a trait object (`Box<dyn Buffer>`), which calls
//! through a pointer to a table of functions, or an enum with a variant per
//! type, which calls through a `match`. The enum is used here: there are
//! exactly two backends, the `match` costs nothing measurable, and code that
//! needs backend-specific features (saving streams a rope's chunks but a
//! mapped buffer's pieces) can match on it.
//!
//! Every [`Text`] and [`Buffer`] method is passed straight to the backend,
//! including the ones with default implementations, so a backend's faster
//! override is always the one that runs.

use std::borrow::Cow;
use std::ops::Range;

use crate::buffer::{Buffer, Content, Edit, EditError, LineSpan, Snapshot, Text};
use crate::mapped::MappedBuffer;
use crate::rope::RopeBuffer;

/// A text buffer of either kind.
#[derive(Debug, Clone)]
pub enum AnyBuffer {
    /// An in-memory rope, for files that fit comfortably in memory.
    Rope(RopeBuffer),
    /// A piece table over a file's original bytes, for huge files and
    /// files that are not valid UTF-8.
    Mapped(MappedBuffer),
}

/// An empty rope, like a new untitled document.
impl Default for AnyBuffer {
    fn default() -> Self {
        Self::Rope(RopeBuffer::default())
    }
}

impl From<RopeBuffer> for AnyBuffer {
    fn from(b: RopeBuffer) -> Self {
        Self::Rope(b)
    }
}

impl From<MappedBuffer> for AnyBuffer {
    fn from(b: MappedBuffer) -> Self {
        Self::Mapped(b)
    }
}

impl AnyBuffer {
    /// Whether this is the mapped backend.
    #[must_use]
    pub fn is_mapped(&self) -> bool {
        matches!(self, Self::Mapped(_))
    }

    /// The rope, if this is one.
    #[must_use]
    pub fn as_rope(&self) -> Option<&RopeBuffer> {
        match self {
            Self::Rope(b) => Some(b),
            Self::Mapped(_) => None,
        }
    }

    /// The mapped buffer, if this is one.
    #[must_use]
    pub fn as_mapped(&self) -> Option<&MappedBuffer> {
        match self {
            Self::Mapped(b) => Some(b),
            Self::Rope(_) => None,
        }
    }
}

/// Run the same expression on whichever backend `$self` holds, with the
/// backend bound to `$b`.
macro_rules! each {
    ($self:expr, $b:ident => $e:expr) => {
        match $self {
            AnyBuffer::Rope($b) => $e,
            AnyBuffer::Mapped($b) => $e,
        }
    };
}

impl Text for AnyBuffer {
    fn len_bytes(&self) -> usize {
        each!(self, b => b.len_bytes())
    }

    fn len_lines(&self) -> usize {
        each!(self, b => b.len_lines())
    }

    fn line(&self, idx: usize) -> Cow<'_, str> {
        each!(self, b => b.line(idx))
    }

    fn byte_to_line(&self, byte: usize) -> usize {
        each!(self, b => b.byte_to_line(byte))
    }

    fn line_to_byte(&self, line: usize) -> usize {
        each!(self, b => b.line_to_byte(line))
    }

    fn slice(&self, range: Range<usize>) -> Cow<'_, str> {
        each!(self, b => b.slice(range))
    }

    fn is_char_boundary(&self, byte: usize) -> bool {
        each!(self, b => b.is_char_boundary(byte))
    }

    fn chunk_at(&self, byte: usize) -> (&[u8], usize) {
        each!(self, b => b.chunk_at(byte))
    }

    fn byte_at(&self, byte: usize) -> Option<u8> {
        each!(self, b => b.byte_at(byte))
    }

    fn line_span(&self, byte: usize) -> LineSpan {
        each!(self, b => b.line_span(byte))
    }

    fn line_number(&self, byte: usize) -> Option<usize> {
        each!(self, b => b.line_number(byte))
    }

    fn indexed_bytes(&self) -> usize {
        each!(self, b => b.indexed_bytes())
    }

    fn read_into(&self, range: Range<usize>, out: &mut Vec<u8>) -> std::io::Result<()> {
        each!(self, b => b.read_into(range, out))
    }

    fn slice_into(&self, range: Range<usize>, out: &mut String) {
        each!(self, b => b.slice_into(range, out));
    }
}

impl Buffer for AnyBuffer {
    fn replace(&mut self, range: Range<usize>, text: &str) -> Result<Edit, EditError> {
        each!(self, b => b.replace(range, text))
    }

    fn put(&mut self, range: Range<usize>, content: &Content) -> Result<Edit, EditError> {
        each!(self, b => b.put(range, content))
    }

    fn snapshot(&self) -> Box<dyn Snapshot> {
        each!(self, b => b.snapshot())
    }

    fn is_fully_indexed(&self) -> bool {
        each!(self, b => b.is_fully_indexed())
    }

    fn replace_sorted(&mut self, items: &[(Range<usize>, &str)]) -> Result<Vec<Edit>, EditError> {
        each!(self, b => b.replace_sorted(items))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::mapped::Limits;
    use crate::source::VecSource;

    const INITIAL: &str = "first\r\nsecond é\nthird 日本\n";

    fn both() -> [AnyBuffer; 2] {
        let limits = Limits {
            copy_limit: 4,
            segment: 8,
            max_piece: 6,
        };
        [
            RopeBuffer::from_text(INITIAL).into(),
            MappedBuffer::with_limits(Arc::new(VecSource::from(INITIAL)), limits).into(),
        ]
    }

    /// Every method's answers, as one string to compare between backends.
    /// (Where chunks split differs by design, so only their emptiness is
    /// compared.)
    fn answers(b: &AnyBuffer) -> String {
        use std::fmt::Write;
        let len = b.len_bytes();
        let mut out = format!("{len} {} {}|", b.len_lines(), b.indexed_bytes());
        for i in 0..=b.len_lines() {
            write!(out, "{}:{}:{}|", b.line(i), b.line_to_byte(i), i).unwrap();
        }
        for byte in 0..=len + 1 {
            let mut raw = Vec::new();
            b.read_into(byte..byte + 3, &mut raw).unwrap();
            let mut s = String::new();
            b.slice_into(byte..byte + 3, &mut s);
            write!(
                out,
                "{} {} {:?} {:?} {:?} {raw:?} {s} {} {:?}|",
                b.byte_to_line(byte),
                b.is_char_boundary(byte),
                b.byte_at(byte),
                b.line_span(byte),
                b.line_number(byte),
                b.slice(byte..byte + 2),
                b.chunk_at(byte).0.is_empty(),
            )
            .unwrap();
        }
        out
    }

    #[test]
    fn both_backends_answer_alike_through_the_enum() {
        let [mut rope, mut mapped] = both();
        assert!(!rope.is_mapped() && mapped.is_mapped());
        assert!(rope.as_rope().is_some() && rope.as_mapped().is_none());
        assert!(mapped.as_mapped().is_some() && mapped.as_rope().is_none());
        assert_eq!(answers(&rope), answers(&mapped));

        for b in [&mut rope, &mut mapped] {
            assert!(b.is_fully_indexed());
            let e = b.replace(0..5, "1st").unwrap();
            b.put(e.new_range(), &e.removed).unwrap();
            b.replace(7..7, "2nd ").unwrap();
            let edits = b.replace_sorted(&[(0..1, "F"), (3..3, "-")]).unwrap();
            assert_eq!(edits.len(), 2);
        }
        assert_eq!(answers(&rope), answers(&mapped));
        let (rs, ms) = (rope.snapshot(), mapped.snapshot());
        assert_eq!(rs.slice(0..usize::MAX), ms.slice(0..usize::MAX));
    }

    #[test]
    fn pieces_go_back_only_into_the_mapped_buffer_they_came_from() {
        let [mut rope, mut mapped] = both();
        let removed = mapped.replace(0..10, "").unwrap().removed;
        assert!(matches!(removed, Content::Pieces(_)));
        assert_eq!(rope.put(0..0, &removed), Err(EditError::Unsupported));
        mapped.put(0..0, &removed).unwrap();
        assert_eq!(mapped.slice(0..usize::MAX), INITIAL);
    }

    #[test]
    fn default_is_an_empty_rope() {
        let b = AnyBuffer::default();
        assert!(!b.is_mapped());
        assert_eq!((b.len_bytes(), b.len_lines()), (0, 1));
    }
}
