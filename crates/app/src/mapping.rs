//! [`MappedFile`]: a file's bytes as a memory mapping, the original bytes
//! of a mapped document (M7).
//!
//! # Memory mapping
//!
//! Reading a 4 GB file into memory takes seconds and 4 GB. *Mapping* it
//! instead asks the operating system to make the file appear in the
//! program's address space as one long `&[u8]`, without reading anything.
//! The first time a byte is touched, the CPU finds no memory behind that
//! address (a *page fault*), the OS reads that page of the file (16 KB on
//! Apple silicon, 4 KB elsewhere) and the read carries on. Pages that have
//! not been used for a while can be dropped again, since the file still
//! has them. So "opening" a 4 GB file this way costs almost nothing, and
//! memory is spent only on the parts looked at.
//!
//! # Why this needs `unsafe`
//!
//! A `&[u8]` in Rust promises that the bytes do not change while it exists.
//! For a mapping that promise is not ours to make: another program can
//! write to the file, or cut it short, while Nimbus has it mapped. The
//! `SAFETY` comment in [`MappedFile::new`] spells out what can happen and
//! what keeps it rare. This is the only `unsafe` in Nimbus outside the
//! menus, which is why it sits behind `nimbus_core::Source`: `core` stays
//! free of both I/O and `unsafe`.
//!
//! # Reading without the mapping
//!
//! [`MappedFile::read_at`] reads with an ordinary positioned read
//! (`pread`) into a buffer the caller owns. Work that goes through the
//! whole file (indexing lines, searching, saving) should use it, so that a
//! file cut short makes the read fail with an error instead of crashing,
//! and so that sweeping 4 GB does not leave the file's pages counted
//! against Nimbus. Only the text on screen is read through the mapping.

#![expect(
    unsafe_code,
    reason = "memory-mapping a file is unsafe: another process can change it (see MappedFile::new)"
)]

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use memmap2::Mmap;
use nimbus_core::Source;

/// A file mapped read-only into memory, as the original bytes of a mapped
/// document.
///
/// It can leave out a few bytes at the start of the file
/// ([`MappedFile::skip_prefix`]), such as a byte order mark, so that the
/// document starts after them. Offsets given to and returned by its
/// methods count from there; [`MappedFile::file_len`] is the whole file's.
///
/// It can also own a temporary file ([`MappedFile::delete_on_drop`]): a
/// saved document whose new file could not replace the old one goes on
/// reading the temporary file it was saved to, which is deleted once
/// nothing reads it any more.
pub struct MappedFile {
    // Fields are dropped in declaration order, and `temp` relies on it: the
    // file is closed and unmapped before it is deleted, because Windows
    // refuses to delete a file that is still mapped.
    /// The open file, for [`MappedFile::read_at`]. The mapping does not need
    /// it to stay open, but reading beside the mapping does.
    file: File,
    /// The mapping; `None` for an empty file, which cannot be mapped on
    /// every platform (a mapping of length 0 is an error on Linux and
    /// Windows).
    map: Option<Mmap>,
    /// Bytes at the start of the file that are not part of the document.
    skip: usize,
    /// The file's first and last few bytes when it was mapped, copied, so
    /// a later mapping can tell whether the file only grew since
    /// ([`MappedFile::continues`]): a file rewritten in place changes the
    /// mapped bytes too, so they cannot be compared with themselves.
    head: Vec<u8>,
    tail: Vec<u8>,
    /// A temporary file to delete when this is dropped. Last, so it is
    /// dropped last.
    temp: Option<TempPath>,
}

/// A temporary file, deleted on drop while `armed`. The flag is atomic
/// because by the time a save decides to keep the file after all (it was
/// renamed into place), the mapping is shared through an `Arc`.
#[derive(Debug)]
struct TempPath {
    path: PathBuf,
    armed: AtomicBool,
}

impl Drop for TempPath {
    fn drop(&mut self) {
        if *self.armed.get_mut()
            && let Err(e) = std::fs::remove_file(&self.path)
            && e.kind() != io::ErrorKind::NotFound
        {
            log::warn!("could not delete {}: {e}", self.path.display());
        }
    }
}

/// How many bytes at each end of a file [`MappedFile`] copies.
const SAMPLE: u64 = 64;

impl MappedFile {
    /// Map all of `file`, which must be open for reading. The mapping's
    /// length is the file's length now; if the file grows later, the new
    /// bytes are not part of it.
    ///
    /// # Errors
    ///
    /// Returns the error from reading the file's size or from mapping it,
    /// for example when the system is out of address space.
    pub fn new(file: File) -> io::Result<Self> {
        let len = file.metadata()?.len();
        let map = if len == 0 {
            None
        } else {
            // SAFETY: `Mmap::map` is unsafe because the returned mapping
            // hands out a `&[u8]` whose bytes the program does not own:
            // they are the file's, and the operating system lets any
            // process with write access change them while they are mapped.
            // Nimbus never writes through it: the mapping is read-only
            // (`Mmap`, not `MmapMut`) and saving writes a new file and
            // renames it over the old one, never in place (`crate::saver`):
            // on Unix the replaced file lives on, unnamed and unchanged,
            // until its last mapping is gone, and Windows refuses to
            // replace a mapped file at all, so a save there first waits
            // for every mapping of it to go. What another process can do,
            // and what follows:
            //
            // - Change bytes in place. The document's text changes under
            //   it. Rust's rules say the bytes behind a `&[u8]` never
            //   change, so in the language's terms this is undefined
            //   behaviour. In practice each read loads whatever the file
            //   holds at that moment, and `core` never trusts the bytes
            //   (UTF-8 is checked on every read, every lookup is
            //   bounds-checked), so the outcome is wrong text or wrong line
            //   numbers until the watcher notices and the user reloads.
            // - Truncate the file. On macOS and Linux, touching a mapped
            //   page that now lies wholly past the end of the file raises
            //   `SIGBUS`, which kills Nimbus at once, unsaved work in every
            //   tab included. Rust cannot catch that. On Windows this
            //   cannot happen: the OS refuses to truncate a file while a
            //   view of it is mapped.
            //
            // What keeps the truncation risk small: work that reads the
            // whole file (indexing, search, save) is meant to use
            // `read_at`, an ordinary read that fails with an error instead;
            // only the text on screen is read through the mapping; and the
            // file watcher compares the file's size with the mapping's,
            // re-mapping a clean document that got shorter at once and
            // warning loudly about one with unsaved edits. Logs, the files
            // this is for, are appended to, which is harmless: the new
            // bytes are simply not in the mapping.
            //
            // Following such a log (M12's Follow Tail) maps the grown file
            // again through this same call: a second, longer mapping
            // beside the first, with the same hazards and nothing more.
            // The document swaps it in for the first only after
            // `continues` has checked that the file still holds the first
            // mapping's bytes where they were, which is what keeps every
            // piece of the document reading the same text through it. The
            // first mapping stays valid, and mapped, until the last
            // snapshot or undo step holding it is dropped.
            Some(unsafe { Mmap::map(&file)? })
        };
        // Sampled beside the mapping, from the length that was mapped.
        let mapped = map.as_deref().map_or(0, <[u8]>::len) as u64;
        let n = SAMPLE.min(mapped);
        let mut head = vec![0; usize::try_from(n).unwrap_or(0)];
        let mut tail = head.clone();
        read_exact_at(&file, &mut head, 0)?;
        read_exact_at(&file, &mut tail, mapped - n)?;
        Ok(Self {
            file,
            map,
            skip: 0,
            head,
            tail,
            temp: None,
        })
    }

    /// Whether this mapping is of `earlier`'s file grown at its end: it is
    /// at least as long, and holds the bytes `earlier` had at its start
    /// and its end. Follow Tail (M12) maps a growing file again and swaps
    /// this mapping in for the earlier one, which is right only when the
    /// earlier one's bytes are still where they were. A file rewritten
    /// with both ends kept would pass, as with the watcher's other checks.
    ///
    /// # Errors
    ///
    /// Returns the system's error from reading this file.
    pub fn continues(&self, earlier: &Self) -> io::Result<bool> {
        let len = earlier.file_len();
        if self.file_len() < len || self.skip != earlier.skip {
            return Ok(false);
        }
        let mut buf = vec![0; earlier.head.len()];
        read_exact_at(&self.file, &mut buf, 0)?;
        if buf != earlier.head {
            return Ok(false);
        }
        read_exact_at(&self.file, &mut buf, len - earlier.tail.len() as u64)?;
        Ok(buf == earlier.tail)
    }

    /// Delete the file at `path`, which must be the mapped file, when this
    /// is dropped (after unmapping and closing it), unless
    /// [`MappedFile::keep_file`] is called first. For a save's temporary
    /// file, which the document reads until it has replaced the target.
    #[must_use]
    pub fn delete_on_drop(mut self, path: PathBuf) -> Self {
        self.temp = Some(TempPath {
            path,
            armed: AtomicBool::new(true),
        });
        self
    }

    /// Do not delete the file on drop after all: it was renamed to the
    /// target, and its temporary name may soon belong to another file.
    pub fn keep_file(&self) {
        if let Some(temp) = &self.temp {
            temp.armed.store(false, Ordering::SeqCst);
        }
    }

    /// The temporary file this mapping deletes when dropped, if any. A
    /// document reading one is not reading the file it is named after.
    #[must_use]
    pub fn temp_path(&self) -> Option<&Path> {
        self.temp
            .as_ref()
            .filter(|t| t.armed.load(Ordering::SeqCst))
            .map(|t| t.path.as_path())
    }

    /// Another handle to the mapped file. It stays open when this mapping
    /// is dropped and still names the same file after a rename, so the
    /// file can be mapped again without looking it up by path.
    ///
    /// # Errors
    ///
    /// Returns the system's error from duplicating the handle.
    pub fn try_clone_file(&self) -> io::Result<File> {
        self.file.try_clone()
    }

    /// The file's size and times now, read through the open handle: from
    /// this very file, wherever it has been renamed to.
    ///
    /// # Errors
    ///
    /// Returns the system's error.
    pub fn metadata(&self) -> io::Result<std::fs::Metadata> {
        self.file.metadata()
    }

    /// Leave the first `n` bytes of the file out of the document (clamped
    /// to the file's length), for a byte order mark.
    #[must_use]
    pub fn skip_prefix(mut self, n: usize) -> Self {
        self.skip = n.min(self.whole().len());
        self
    }

    /// The whole mapped file, prefix included.
    fn whole(&self) -> &[u8] {
        self.map.as_deref().unwrap_or_default()
    }

    /// The length of the file when it was mapped, prefix included. A file
    /// on disk shorter than this has been truncated under the mapping.
    #[must_use]
    pub fn file_len(&self) -> u64 {
        u64::try_from(self.whole().len()).unwrap_or(u64::MAX)
    }

    /// Fill `buf` with the bytes starting at `offset` (counted after any
    /// skipped prefix), reading the file directly rather than through the
    /// mapping.
    ///
    /// For readers that go through much of the file: the pages read this
    /// way are the system's file cache, not memory touched through the
    /// mapping, and a file truncated meanwhile gives an error rather than
    /// a crash.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::UnexpectedEof`] if the range reaches past
    /// the end of the mapping, or past the end of the file because it was
    /// truncated, and any other read error from the system.
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> io::Result<()> {
        let end = offset.checked_add(buf.len());
        if end.is_none_or(|end| end > self.len()) {
            // Bytes past the mapping are not part of the document, even if
            // the file has grown to have them.
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "read past the end of the mapped file",
            ));
        }
        let at = u64::try_from(self.skip + offset).map_err(io::Error::other)?;
        read_exact_at(&self.file, buf, at)
    }
}

/// `pread` until `buf` is full.
#[cfg(unix)]
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

/// Windows has no `read_exact_at`; `seek_read` reads at an offset (and
/// moves the file's cursor, which nothing else here uses) and may return
/// fewer bytes than asked, so loop until `buf` is full.
#[cfg(windows)]
fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "file ended before the read did",
                ));
            }
            Ok(n) => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

impl Source for MappedFile {
    fn bytes(&self) -> &[u8] {
        self.whole().get(self.skip..).unwrap_or_default()
    }

    /// A file read, not a copy from the mapping: see
    /// [`MappedFile::read_at`].
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> io::Result<()> {
        MappedFile::read_at(self, offset, buf)
    }
}

/// Prints the lengths only: the bytes may be gigabytes.
impl std::fmt::Debug for MappedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MappedFile")
            .field("file_len", &self.file_len())
            .field("skip", &self.skip)
            .field("temp", &self.temp_path())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// A file under the system temp dir, removed on drop.
    struct TempFile(PathBuf);

    impl TempFile {
        fn new(name: &str, bytes: &[u8]) -> Self {
            let path = std::env::temp_dir()
                .join(format!("nimbus-mapping-test-{name}-{}", std::process::id()));
            std::fs::write(&path, bytes).unwrap();
            Self(path)
        }

        fn map(&self) -> MappedFile {
            MappedFile::new(File::open(&self.0).unwrap()).unwrap()
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn maps_a_file_and_reads_it() {
        let bytes = b"line one\nline two\xff\xfe\n";
        let f = TempFile::new("read", bytes);
        let m = f.map();
        assert_eq!(m.bytes(), bytes);
        assert_eq!(m.len(), bytes.len());
        assert_eq!(m.file_len(), bytes.len() as u64);
        assert!(format!("{m:?}").contains("file_len: 20"));
    }

    #[test]
    fn an_empty_file_needs_no_mapping() {
        let f = TempFile::new("empty", b"");
        let m = f.map();
        assert!(m.map.is_none());
        assert!(m.bytes().is_empty() && m.is_empty());
        assert_eq!(m.file_len(), 0);
        m.read_at(0, &mut []).unwrap();
        assert!(m.read_at(0, &mut [0]).is_err());
    }

    #[test]
    fn read_at_reads_the_file_and_stops_at_the_mapping() {
        let f = TempFile::new("read-at", b"0123456789");
        let m = f.map();
        let mut buf = [0u8; 4];
        m.read_at(3, &mut buf).unwrap();
        assert_eq!(&buf, b"3456");
        m.read_at(6, &mut buf).unwrap();
        assert_eq!(&buf, b"6789");
        let err = m.read_at(7, &mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert!(m.read_at(usize::MAX, &mut buf).is_err(), "no overflow");

        // Bytes appended after mapping are not part of the document.
        // (Appended, not rewritten: Windows refuses to truncate a mapped
        // file, which rewriting starts with.)
        let mut log = std::fs::OpenOptions::new().append(true).open(&f.0).unwrap();
        std::io::Write::write_all(&mut log, b"abc").unwrap();
        assert_eq!(m.len(), 10);
        assert!(m.read_at(8, &mut buf).is_err());
    }

    #[test]
    fn a_skipped_prefix_is_left_out_of_every_offset() {
        let f = TempFile::new("skip", "\u{feff}héllo".as_bytes());
        let m = f.map().skip_prefix(3);
        assert_eq!(m.bytes(), "héllo".as_bytes());
        assert_eq!(m.file_len(), 9);
        let mut buf = [0u8; 2];
        m.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf, b"h\xc3");
        assert!(m.read_at(5, &mut buf).is_err());
        let all = f.map().skip_prefix(100);
        assert!(all.bytes().is_empty(), "clamped to the file");
    }

    #[test]
    fn a_temporary_file_is_deleted_after_its_mapping_unless_kept() {
        let f = TempFile::new("temp-delete", b"saved bytes");
        let m = f.map().delete_on_drop(f.0.clone());
        assert_eq!(m.temp_path(), Some(f.0.as_path()));
        assert_eq!(m.metadata().unwrap().len(), 11);
        let handle = m.try_clone_file().unwrap();
        drop(m);
        assert!(!f.0.exists(), "deleted on drop");
        drop(handle);

        let kept = TempFile::new("temp-keep", b"renamed into place");
        let m = kept.map().delete_on_drop(kept.0.clone());
        m.keep_file();
        assert_eq!(m.temp_path(), None);
        drop(m);
        assert!(kept.0.exists(), "kept");
    }

    /// On Unix a truncated file makes `read_at` fail cleanly; reading the
    /// lost part through the mapping would raise SIGBUS instead, which is
    /// why bulk readers use `read_at`. (Windows refuses the truncation.)
    #[cfg(unix)]
    #[test]
    fn read_at_fails_cleanly_after_truncation() {
        let f = TempFile::new("truncate", &vec![b'x'; 64 * 1024]);
        let m = f.map();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&f.0)
            .unwrap()
            .set_len(10)
            .unwrap();
        let mut buf = [0u8; 100];
        let err = m.read_at(32 * 1024, &mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        let mut head = [0u8; 10];
        m.read_at(0, &mut head).unwrap();
        assert_eq!(&head, b"xxxxxxxxxx");
    }
}
