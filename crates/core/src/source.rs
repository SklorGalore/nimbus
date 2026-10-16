//! Where a mapped document's original bytes come from.
//!
//! [`crate::mapped::MappedBuffer`] never copies or changes the file it
//! opened: every untouched part of the document is read straight from the
//! original bytes. Those bytes are usually a memory mapping of the file,
//! which needs `unsafe` and I/O, so they live in the app crate (M7 task 9).
//! `core` only sees this trait, which keeps it free of both. Tests and small
//! files use [`VecSource`], which holds the bytes in memory.

use std::fmt;

/// The original, unchanging bytes of a document.
///
/// `Send + Sync` because snapshots of the document are read on worker
/// threads (search, indexing, saving) while the editor keeps working.
///
/// **Invariant**: the bytes never change while anything holds the source.
/// Every piece of a mapped document records offsets into them, so a change
/// would silently change the document. (For a memory mapping this is a
/// promise the operating system cannot fully keep; the app's mapping module
/// explains the risk.)
pub trait Source: Send + Sync {
    /// The whole original content.
    fn bytes(&self) -> &[u8];

    /// Length in bytes.
    fn len(&self) -> usize {
        self.bytes().len()
    }

    /// Whether there are no bytes at all.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Fill `buf` with the bytes starting at `offset`, for work that reads
    /// a lot of the file (search, saving, copying a large selection). A
    /// memory-mapped source overrides this with an ordinary file read, so
    /// sweeping a huge file does not pull it all into memory through the
    /// mapping, and a file cut short by another program gives an error
    /// rather than a crash. The default copies from [`Source::bytes`].
    ///
    /// # Errors
    ///
    /// Returns `UnexpectedEof` for bytes past the end, and the file
    /// system's error for a file read.
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> std::io::Result<()> {
        let src = offset
            .checked_add(buf.len())
            .and_then(|end| self.bytes().get(offset..end))
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

/// Original bytes held in an ordinary `Vec`: for tests, and for any
/// document whose original content is already in memory.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct VecSource(Vec<u8>);

impl VecSource {
    /// A source holding `bytes`.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl Source for VecSource {
    fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for VecSource {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl From<&str> for VecSource {
    fn from(text: &str) -> Self {
        Self(text.as_bytes().to_vec())
    }
}

impl From<String> for VecSource {
    fn from(text: String) -> Self {
        Self(text.into_bytes())
    }
}

/// Prints the length only: the bytes may be megabytes.
impl fmt::Debug for VecSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VecSource")
            .field("len", &self.0.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vec_source_hands_out_its_bytes() {
        let s = VecSource::from("héllo");
        assert_eq!(s.bytes(), "héllo".as_bytes());
        assert_eq!(s.len(), 6);
        assert!(!s.is_empty());
        assert!(VecSource::default().is_empty());
        assert_eq!(format!("{s:?}"), "VecSource { len: 6 }");
    }

    #[test]
    fn read_at_copies_or_refuses_past_the_end() {
        let s = VecSource::from("abcdef");
        let mut buf = [0; 3];
        s.read_at(2, &mut buf).unwrap();
        assert_eq!(&buf, b"cde");
        assert!(s.read_at(4, &mut buf).is_err());
        assert!(s.read_at(usize::MAX, &mut buf).is_err());
    }
}
