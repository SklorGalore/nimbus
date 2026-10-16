//! Opening and saving files.
//!
//! Opening picks the buffer (DESIGN.md §4.1): most files are read into an
//! in-memory rope; files at or above [`MAX_ROPE_BYTES`], and files that are
//! not valid UTF-8, are memory-mapped ([`crate::mapping`]) and edited as a
//! piece table over the mapping, so a 4 GB log opens without being read.
//!
//! Saving is *atomic*: the new contents go to a temporary file next to the
//! target, which is flushed to disk and then renamed over the target. A
//! rename within one directory either fully happens or does not, so a crash
//! or power cut mid-save leaves the old file or the new file, never half of
//! each. Mapped documents are saved the same way, but on a worker and with
//! the document switching to the new file ([`crate::saver`]).

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use nimbus_core::mapped::Limits;
use nimbus_core::{AnyBuffer, Buffer, LineEnding, MappedBuffer, RopeBuffer, Source, Text};

use crate::mapping::MappedFile;

/// Files at or above this size open in the mapped backend instead of being
/// read into a rope (DESIGN.md §4.1). Measured in M7 (bench/threshold.sh):
/// the rope opens 256 MB in 0.2 s at about 1.25x the file in memory and
/// stays within budget up to 512 MB, while the mapped buffer opens any
/// size in 0.02 s at a flat ~33 MB. Below this, the rope's advantages win:
/// undo survives saving, saving does not pause edits, and another program
/// truncating the file cannot crash Nimbus.
pub const MAX_ROPE_BYTES: u64 = 256 * 1024 * 1024;

/// How much of a mapped file is read to guess its line ending: 1 MiB, so a
/// 4 GB log is not read to the end before its first screen.
const LINE_ENDING_SAMPLE: usize = 1024 * 1024;

/// Saving reads the document and writes the file this many bytes at a
/// time: 4 MiB, so saving a huge document never holds a copy of it.
pub(crate) const SAVE_CHUNK: usize = 4 * 1024 * 1024;

/// The UTF-8 byte order mark some Windows tools put at the start of a file.
pub(crate) const BOM: &str = "\u{feff}";

/// Which kind of buffer holds a file's text (DESIGN.md §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// [`RopeBuffer`]: the whole text in memory. For valid UTF-8 files
    /// under [`MAX_ROPE_BYTES`].
    Rope,
    /// [`MappedBuffer`] over a [`MappedFile`]: the file stays on disk and
    /// is read as needed. For files at or above [`MAX_ROPE_BYTES`], and for
    /// files that are not valid UTF-8 at any size.
    Mapped,
}

impl Backend {
    /// Parse the `--backend` option's value.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "rope" => Some(Self::Rope),
            "mapped" => Some(Self::Mapped),
            _ => None,
        }
    }
}

/// The backend `--backend` asked for, if any; see [`force_backend`].
static FORCED_BACKEND: OnceLock<Backend> = OnceLock::new();

/// Open every file in `backend` from now on, whatever its size: the
/// `--backend` option, for measuring the two against each other. A forced
/// rope has no size limit but still cannot hold bytes that are not UTF-8;
/// a forced mapping works for any file. Only the first call counts.
pub fn force_backend(backend: Backend) {
    let _ = FORCED_BACKEND.set(backend);
}

/// A file opened for editing.
#[derive(Debug)]
pub struct Opened {
    /// The file's text, without any byte order mark.
    pub buffer: AnyBuffer,
    /// The line ending most of its lines use; Enter inserts this.
    pub line_ending: LineEnding,
    /// The file started with a UTF-8 byte order mark. It is kept out of the
    /// text, so it cannot be edited or typed in front of, and is written
    /// back on save.
    pub bom: bool,
    /// The mapping behind a mapped buffer: its original bytes, readable
    /// without going through the mapping, and the length to compare the
    /// file on disk with. `None` for a rope.
    pub mapping: Option<Arc<MappedFile>>,
    /// The rest of a mapped file's line counting, for a worker; `None` when
    /// it was all done at open (or for a rope).
    pub indexing: Option<crate::indexer::Indexing>,
}

/// Why a file could not be opened.
#[derive(Debug)]
pub enum OpenError {
    /// No file at the path. The editor treats this as a new, empty file
    /// that will be created on save.
    NotFound,
    /// The file is not valid UTF-8 and `--backend rope` asked for the
    /// in-memory buffer, which cannot hold such bytes. Without that option
    /// such files open mapped.
    NotUtf8,
    /// The path is a directory.
    IsDirectory,
    /// The path is a device, pipe or socket, not a regular file.
    NotAFile,
    /// Any other I/O failure, such as missing permission.
    Io(std::io::Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::NotFound => write!(f, "file not found"),
            OpenError::NotUtf8 => write!(
                f,
                "file is not valid UTF-8, which --backend rope cannot open"
            ),
            OpenError::IsDirectory => write!(f, "path is a directory"),
            OpenError::NotAFile => write!(f, "not a regular file"),
            OpenError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for OpenError {}

/// Open `path`: read it into a rope, or map it if it is large or not
/// valid UTF-8 (see [`Backend`]), unless [`force_backend`] chose one.
///
/// # Errors
///
/// Returns an [`OpenError`] describing why the file cannot be edited.
pub fn open(path: &Path) -> Result<Opened, OpenError> {
    open_with(path, MAX_ROPE_BYTES, FORCED_BACKEND.get().copied())
}

/// [`open`], with the size from which files are mapped and the forced
/// backend as parameters, so tests can use tiny limits.
pub(crate) fn open_with(
    path: &Path,
    rope_limit: u64,
    forced: Option<Backend>,
) -> Result<Opened, OpenError> {
    let started = Instant::now();
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(OpenError::NotFound),
        Err(e) => return Err(OpenError::Io(e)),
    };
    if meta.is_dir() {
        return Err(OpenError::IsDirectory);
    }
    if !meta.is_file() {
        // Devices such as /dev/zero report length 0 and never end.
        return Err(OpenError::NotAFile);
    }
    let file = fs::File::open(path).map_err(OpenError::Io)?;
    let backend = forced.unwrap_or(if meta.len() >= rope_limit {
        Backend::Mapped
    } else {
        Backend::Rope
    });
    if backend == Backend::Rope {
        // A forced rope has no limit: measuring big ropes is its purpose.
        let limit = if forced.is_some() {
            u64::MAX
        } else {
            rope_limit
        };
        match read_rope(&file, limit) {
            Ok(Some(opened)) => {
                log::debug!(
                    "open: {} read into a rope in {:?}",
                    path.display(),
                    started.elapsed()
                );
                return Ok(opened);
            }
            // It grew past the limit while being read: map it instead.
            Ok(None) => {}
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                if forced.is_some() {
                    return Err(OpenError::NotUtf8);
                }
                // Not UTF-8: a piece table keeps such bytes as they are. A
                // file this small is held in memory rather than mapped, so
                // another program cutting it short cannot crash Nimbus.
                let bytes = fs::read(path).map_err(OpenError::Io)?;
                if (bytes.len() as u64) < rope_limit {
                    log::debug!(
                        "open: {} (not UTF-8) read into memory in {:?}",
                        path.display(),
                        started.elapsed()
                    );
                    return Ok(open_bytes(bytes));
                }
            }
            Err(e) => return Err(OpenError::Io(e)),
        }
    }
    let opened = open_mapped(file)?;
    log::debug!("open: {} mapped in {:?}", path.display(), started.elapsed());
    Ok(opened)
}

/// Read `file` into a rope, streaming with no full-size intermediate
/// string. `Ok(None)` if it has `limit` bytes or more (it may have grown
/// since its size was checked).
///
/// # Errors
///
/// Returns the read error, or [`std::io::ErrorKind::InvalidData`] for
/// bytes that are not UTF-8.
fn read_rope(file: &fs::File, limit: u64) -> std::io::Result<Option<Opened>> {
    let mut limited = std::io::BufReader::new(file).take(limit);
    let mut buffer = RopeBuffer::from_reader(&mut limited)?;
    if limited.limit() == 0 {
        return Ok(None);
    }
    let bom = buffer.slice(0..BOM.len()) == BOM;
    if bom {
        buffer
            .replace(0..BOM.len(), "")
            .map_err(std::io::Error::other)?;
    }
    let line_ending = LineEnding::detect(buffer.chunks());
    Ok(Some(Opened {
        buffer: buffer.into(),
        line_ending,
        bom,
        mapping: None,
        indexing: None,
    }))
}

/// A piece table over `bytes` held in memory, for a small file that is not
/// UTF-8. A byte order mark is left out, as in a rope.
fn open_bytes(mut bytes: Vec<u8>) -> Opened {
    let bom = bytes.starts_with(BOM.as_bytes());
    if bom {
        bytes.drain(..BOM.len());
    }
    let line_ending = LineEnding::detect([bytes.get(..LINE_ENDING_SAMPLE).unwrap_or(&bytes)]);
    Opened {
        buffer: MappedBuffer::from_bytes(bytes).into(),
        line_ending,
        bom,
        mapping: None,
        indexing: None,
    }
}

/// Map `file` and make a mapped buffer over it. A byte order mark is left
/// out of the mapping's bytes, so the document starts after it.
fn open_mapped(file: fs::File) -> Result<Opened, OpenError> {
    let mapped = MappedFile::new(file).map_err(OpenError::Io)?;
    let bom = mapped.bytes().starts_with(BOM.as_bytes());
    let mapped = Arc::new(if bom {
        mapped.skip_prefix(BOM.len())
    } else {
        mapped
    });
    // Read beside the mapping, so these pages are not kept.
    let mut sample = vec![0; mapped.len().min(LINE_ENDING_SAMPLE)];
    mapped.read_at(0, &mut sample).map_err(OpenError::Io)?;
    let line_ending = LineEnding::detect([&sample]);
    drop(sample);
    let started = Instant::now();
    // Count lines in the first few MB now, so the first screen has line
    // numbers; a worker counts the rest.
    let (index, indexing) = crate::indexer::start(&mapped).map_err(OpenError::Io)?;
    let source: Arc<dyn Source> = mapped.clone();
    let buffer = MappedBuffer::with_index(source, index, Limits::default())
        .map_err(|e| OpenError::Io(std::io::Error::other(e)))?;
    log::debug!(
        "open: piece table over {} bytes made in {:?}",
        mapped.len(),
        started.elapsed()
    );
    Ok(Opened {
        buffer: buffer.into(),
        line_ending,
        bom,
        mapping: Some(mapped),
        indexing,
    })
}

/// Why a save failed. The buffer stays dirty so nothing is lost.
#[derive(Debug)]
pub struct SaveError {
    /// The step that failed, for the message shown to the user.
    pub step: &'static str,
    /// The underlying error.
    pub source: std::io::Error,
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "save failed while {}: {}", self.step, self.source)
    }
}

impl std::error::Error for SaveError {}

/// Turns an I/O error into a [`SaveError`] at `step`, for `map_err`.
pub(crate) fn step(step: &'static str) -> impl FnOnce(std::io::Error) -> SaveError {
    move |source| SaveError { step, source }
}

/// `path` with its folder resolved (symlinks, `..`), so it compares equal
/// to the paths file watching reports. Works for a file that does not exist
/// (yet, or any more), as long as its folder does.
#[must_use]
pub fn canonical(path: &Path) -> std::path::PathBuf {
    if let Ok(p) = fs::canonicalize(path) {
        return p;
    }
    match (
        path.parent().and_then(|d| fs::canonicalize(d).ok()),
        path.file_name(),
    ) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => path.to_path_buf(),
    }
}

/// Write `text` to `path` atomically, byte for byte as it is in the
/// buffer, preceded by a byte order mark if `bom` is set.
///
/// The bytes are the raw ones ([`Text::read_into`]), so bytes that are not
/// UTF-8 are written back as they were. Line endings are not rewritten:
/// the buffer already holds whatever the file had plus what Enter
/// inserted, so an unedited file round-trips exactly, mixed endings
/// included. The text is copied out [`SAVE_CHUNK`] bytes at a time, never
/// whole.
///
/// This must not be given a mapped buffer whose mapping is `path` itself:
/// Windows refuses to rename over a mapped file, and elsewhere the
/// document would go on reading the replaced file's bytes. Mapped
/// documents save through [`crate::saver`] instead, which handles both.
///
/// If `path` is a symlink, the file it points to is replaced and the link
/// is kept. The original file's permissions are kept.
///
/// # Errors
///
/// Returns a [`SaveError`] naming the step that failed. The target file is
/// untouched unless the final rename succeeded.
pub fn save(path: &Path, text: &dyn Text, bom: bool) -> Result<fs::Metadata, SaveError> {
    // Write next to the real file, so the rename stays within one directory
    // (and one filesystem), and so a symlink is followed, not overwritten.
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let (tmp, mut file) = create_temp(&target)?;

    let result = write_temp(&mut file, &target, text, bom).and_then(|()| {
        // The written file's size and time, read from the file itself before
        // it replaces the old one: a write by anyone else after this point
        // looks different. Renaming keeps both.
        let meta = file.metadata().map_err(step("reading back the file"))?;
        drop(file);
        rename_with_retry(&tmp, &target).map_err(step("replacing the file"))?;
        Ok(meta)
    });
    if result.is_err() {
        // Best effort: do not leave temp files behind.
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Create a new temporary file beside `target`, open for reading and
/// writing (a mapped document maps the file it was saved to).
///
/// `create_new` refuses to open anything that already exists, including a
/// symlink someone planted at the predictable name, so a save can never be
/// redirected into another file. A clash moves on to the next name.
///
/// # Errors
///
/// Returns the error from creating the file, or from the last name tried
/// if every name was taken.
pub(crate) fn create_temp(target: &Path) -> Result<(PathBuf, fs::File), SaveError> {
    let name = target
        .file_name()
        .map_or_else(|| "untitled".into(), |n| n.to_string_lossy().into_owned());
    let mut last_err = std::io::Error::other("no temporary name was free");
    // Replacing a file: owner-only until its permissions are copied, so a
    // secret file's new contents are never readable by others mid-save. A
    // new file has nothing to copy: it gets the usual permissions for a
    // new file (0o666 less the umask, typically 0o644).
    #[cfg(unix)]
    let mode = if fs::metadata(target).is_ok() {
        0o600
    } else {
        0o666
    };
    for attempt in 0..16u32 {
        let tmp = target.with_file_name(format!(
            ".{name}.nimbus-save-{}-{attempt}",
            std::process::id()
        ));
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        match options.open(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last_err = e,
            Err(e) => return Err(step("creating a temporary file")(e)),
        }
    }
    Err(step("creating a temporary file")(last_err))
}

fn write_temp(
    file: &mut fs::File,
    target: &Path,
    text: &dyn Text,
    bom: bool,
) -> Result<(), SaveError> {
    copy_permissions(file, target)?;
    if bom {
        file.write_all(BOM.as_bytes())
            .map_err(step("writing the temporary file"))?;
    }
    write_text(file, text, SAVE_CHUNK, |_| true)?;
    // Make sure the bytes are on disk before the rename makes them visible.
    file.sync_all().map_err(step("flushing to disk"))
}

/// Give the new `file` the permissions of `target`, if it exists. Done
/// before any content is written, so the contents of a file only its owner
/// may read never sit in a file others may read.
///
/// # Errors
///
/// Returns the error from setting the permissions.
pub(crate) fn copy_permissions(file: &fs::File, target: &Path) -> Result<(), SaveError> {
    if let Ok(meta) = fs::metadata(target) {
        file.set_permissions(meta.permissions())
            .map_err(step("copying permissions"))?;
    }
    Ok(())
}

/// Write all of `text`'s raw bytes to `out`, copying at most `chunk` bytes
/// at a time into one reused buffer. Each write is large, so `out` needs
/// no buffering of its own.
///
/// `each` sees every piece just before it is written (saving a mapped
/// document indexes the lines of the new file this way); returning `false`
/// stops the write with an [`std::io::ErrorKind::Interrupted`] error.
///
/// # Errors
///
/// Returns a [`SaveError`] for a failed or short read of the document, a
/// failed write, or a stop asked for by `each`.
pub(crate) fn write_text(
    out: &mut impl Write,
    text: &dyn Text,
    chunk: usize,
    mut each: impl FnMut(&[u8]) -> bool,
) -> Result<(), SaveError> {
    let len = text.len_bytes();
    let chunk = chunk.max(1);
    let mut buf = Vec::with_capacity(len.min(chunk));
    let mut pos = 0;
    while pos < len {
        let end = len.min(pos.saturating_add(chunk));
        buf.clear();
        text.read_into(pos..end, &mut buf)
            .map_err(step("reading the document"))?;
        if buf.len() != end - pos {
            // A short read would silently drop text from the file.
            return Err(step("reading the document")(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the document ended early",
            )));
        }
        if !each(&buf) {
            return Err(step("writing the temporary file")(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "the save was cancelled",
            )));
        }
        out.write_all(&buf)
            .map_err(step("writing the temporary file"))?;
        pos = end;
    }
    out.flush().map_err(step("writing the temporary file"))
}

/// Rename, retrying once: on Windows a virus scanner or indexer briefly
/// holding the target open makes the first attempt fail.
///
/// # Errors
///
/// Returns the second attempt's error.
pub(crate) fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::rename(from, to).or_else(|_| {
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::rename(from, to)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory under the system temp dir, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("nimbus-io-test-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// All of `text`'s raw bytes.
    fn raw(text: &dyn Text) -> Vec<u8> {
        let mut out = Vec::new();
        text.read_into(0..text.len_bytes(), &mut out).unwrap();
        out
    }

    #[test]
    fn unedited_files_round_trip_byte_for_byte() {
        let dir = TempDir::new("roundtrip");
        for (name, content) in [
            ("lf.txt", "a\nb\n"),
            ("crlf.txt", "a\r\nb\r\n"),
            ("mixed.txt", "a\r\nb\nc"),
            ("empty.txt", ""),
            ("unicode.txt", "naïve 日本 👩‍💻\n"),
            ("bom.txt", "\u{feff}x\r\n"),
        ] {
            let path = dir.0.join(name);
            fs::write(&path, content).unwrap();
            let opened = open(&path).unwrap();
            assert!(!opened.buffer.is_mapped(), "{name}");
            save(&path, &opened.buffer, opened.bom).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), content, "{name}");
        }
    }

    #[test]
    fn detects_line_endings() {
        let dir = TempDir::new("endings");
        let path = dir.0.join("w.txt");
        fs::write(&path, "a\r\nb\r\n").unwrap();
        assert_eq!(open(&path).unwrap().line_ending, LineEnding::CrLf);
        let mapped = open_with(&path, 0, None).unwrap();
        assert!(mapped.buffer.is_mapped());
        assert_eq!(mapped.line_ending, LineEnding::CrLf);
    }

    #[test]
    fn edits_are_saved_and_no_temp_file_remains() {
        let dir = TempDir::new("edit");
        let path = dir.0.join("f.txt");
        fs::write(&path, "hello\n").unwrap();
        let mut opened = open(&path).unwrap();
        opened.buffer.replace(0..5, "goodbye").unwrap();
        save(&path, &opened.buffer, false).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "goodbye\n");
        let leftovers: Vec<_> = fs::read_dir(&dir.0).unwrap().collect();
        assert_eq!(leftovers.len(), 1);
    }

    #[test]
    fn saving_a_new_path_creates_it() {
        let dir = TempDir::new("new");
        let path = dir.0.join("new.txt");
        assert!(matches!(open(&path), Err(OpenError::NotFound)));
        save(&path, &RopeBuffer::from_text("fresh"), false).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "fresh");
    }

    #[test]
    fn non_utf8_files_open_as_bytes_intact_in_memory_or_mapped() {
        let dir = TempDir::new("binary");
        let bin = dir.0.join("bin");
        let bytes = b"ok\n\xff\xfe\x00 caf\xc3\xa9 \xc3\n\x80end";
        fs::write(&bin, bytes).unwrap();
        // Small: a piece table held in memory, not mapped, so another
        // program cutting the file short cannot crash Nimbus.
        let small = open(&bin).unwrap();
        assert!(small.buffer.is_mapped() && small.mapping.is_none());
        assert_eq!(raw(&small.buffer), bytes);
        let opened = open_with(&bin, 0, None).unwrap();
        assert!(opened.buffer.is_mapped());
        assert!(!opened.bom);
        assert_eq!(raw(&opened.buffer), bytes);
        let mapping = opened.mapping.as_ref().unwrap();
        assert_eq!(mapping.file_len(), bytes.len() as u64);
        // Each invalid byte shows as one stand-in character, so offsets
        // still match the file.
        assert_eq!(
            opened.buffer.line(1).len(),
            b"\xff\xfe\x00 caf\xc3\xa9 \xc3".len()
        );

        // Saved elsewhere (saving over its own mapping goes through
        // `crate::saver`), the bytes come back unchanged.
        let copy = dir.0.join("copy");
        save(&copy, &opened.buffer, opened.bom).unwrap();
        assert_eq!(fs::read(&copy).unwrap(), bytes);

        // Only a forced rope refuses it.
        assert!(matches!(
            open_with(&bin, MAX_ROPE_BYTES, Some(Backend::Rope)),
            Err(OpenError::NotUtf8)
        ));
    }

    #[test]
    fn files_at_the_size_limit_open_mapped() {
        let dir = TempDir::new("threshold");
        let path = dir.0.join("f.txt");
        fs::write(&path, "0123456789abcdef").unwrap(); // 16 bytes
        assert!(!open_with(&path, 17, None).unwrap().buffer.is_mapped());
        let at = open_with(&path, 16, None).unwrap();
        assert!(at.buffer.is_mapped() && at.mapping.is_some());
        assert_eq!(at.buffer.slice(0..16), "0123456789abcdef");
        assert!(open_with(&path, 0, None).unwrap().buffer.is_mapped());

        // Forcing either backend ignores the size.
        let rope = open_with(&path, 1, Some(Backend::Rope)).unwrap();
        assert!(!rope.buffer.is_mapped() && rope.mapping.is_none());
        let mapped = open_with(&path, u64::MAX, Some(Backend::Mapped)).unwrap();
        assert!(mapped.buffer.is_mapped());
        assert_eq!(raw(&mapped.buffer), raw(&rope.buffer));
    }

    #[test]
    fn empty_and_bom_files_open_mapped() {
        let dir = TempDir::new("mapped-edges");
        let empty = dir.0.join("empty");
        fs::write(&empty, "").unwrap();
        let opened = open_with(&empty, 0, None).unwrap();
        assert!(opened.buffer.is_mapped());
        assert_eq!(
            (opened.buffer.len_bytes(), opened.buffer.len_lines()),
            (0, 1)
        );

        // The document starts after a byte order mark, as in a rope.
        let bom = dir.0.join("bom");
        fs::write(&bom, "\u{feff}hi\r\nyo").unwrap();
        let opened = open_with(&bom, 0, None).unwrap();
        assert!(opened.buffer.is_mapped() && opened.bom);
        assert_eq!(opened.buffer.line(0), "hi");
        assert_eq!(raw(&opened.buffer), b"hi\r\nyo");
        assert_eq!(opened.mapping.as_ref().unwrap().file_len(), 9);
        let copy = dir.0.join("copy");
        save(&copy, &opened.buffer, opened.bom).unwrap();
        assert_eq!(fs::read_to_string(&copy).unwrap(), "\u{feff}hi\r\nyo");
    }

    #[test]
    fn writing_streams_in_chunks_and_keeps_raw_bytes() {
        let bytes = b"abc\xffdef\r\nghij".to_vec();
        let mapped = MappedBuffer::from_bytes(bytes.clone());
        for chunk in [1, 3, 4, 100] {
            let mut out = Vec::new();
            let mut seen = Vec::new();
            write_text(&mut out, &mapped, chunk, |piece| {
                assert!(piece.len() <= chunk);
                seen.extend_from_slice(piece);
                true
            })
            .unwrap();
            assert_eq!(out, bytes, "chunk {chunk}");
            assert_eq!(seen, bytes, "each piece is shown before it is written");
        }
        let rope = RopeBuffer::from_text("naïve 日本\n");
        let mut out = Vec::new();
        write_text(&mut out, &rope, 2, |_| true).unwrap();
        assert_eq!(out, "naïve 日本\n".as_bytes());

        // Stopping part way writes nothing more.
        let mut out = Vec::new();
        let mut calls = 0;
        let err = write_text(&mut out, &mapped, 4, |_| {
            calls += 1;
            calls < 2
        })
        .unwrap_err();
        assert_eq!(err.source.kind(), std::io::ErrorKind::Interrupted);
        assert_eq!(out, b"abc\xff");
    }

    #[test]
    fn refuses_directories_but_opens_long_lines() {
        let dir = TempDir::new("refuse");
        assert!(matches!(open(&dir.0), Err(OpenError::IsDirectory)));
        // Long lines are read in windows (M7), so a 2 MB line opens.
        let long = dir.0.join("min.json");
        fs::write(&long, "x".repeat(2 * 1024 * 1024)).unwrap();
        assert_eq!(open(&long).unwrap().buffer.len_bytes(), 2 * 1024 * 1024);
    }

    #[test]
    fn bom_is_hidden_from_the_text_and_kept_on_save() {
        let dir = TempDir::new("bom");
        let path = dir.0.join("bom.txt");
        fs::write(&path, "\u{feff}hi\r\n").unwrap();
        let mut opened = open(&path).unwrap();
        assert!(opened.bom);
        assert_eq!(opened.buffer.line(0), "hi");
        opened.buffer.replace(0..0, "x").unwrap();
        save(&path, &opened.buffer, opened.bom).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "\u{feff}xhi\r\n");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_devices() {
        assert!(matches!(
            open(Path::new("/dev/zero")),
            Err(OpenError::NotAFile)
        ));
    }

    #[test]
    #[cfg(unix)]
    fn a_new_file_gets_the_usual_permissions_not_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("new-perms");
        let path = dir.0.join("new.txt");
        save(&path, &RopeBuffer::from_text("x"), false).unwrap();
        // Whatever the umask, a new file is made as 0o666 less it; compare
        // with a file the system creates the ordinary way.
        let plain = dir.0.join("plain.txt");
        fs::write(&plain, "x").unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), mode(&plain));
    }

    #[test]
    #[cfg(unix)]
    fn saving_through_a_symlink_keeps_the_link_and_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("symlink");
        let real = dir.0.join("real.sh");
        let link = dir.0.join("link.sh");
        fs::write(&real, "old").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        save(&link, &RopeBuffer::from_text("new"), false).unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&real).unwrap(), "new");
        let mode = fs::metadata(&real).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_symlink_at_the_temp_name_is_not_followed() {
        let dir = TempDir::new("planted");
        let victim = dir.0.join("victim");
        let target = dir.0.join("notes.txt");
        fs::write(&victim, "precious").unwrap();
        fs::write(&target, "old").unwrap();
        let planted =
            target.with_file_name(format!(".notes.txt.nimbus-save-{}-0", std::process::id()));
        std::os::unix::fs::symlink(&victim, &planted).unwrap();

        save(&target, &RopeBuffer::from_text("new"), false).unwrap();

        assert_eq!(fs::read_to_string(&victim).unwrap(), "precious");
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
    }

    #[test]
    fn opened_text_matches_file() {
        let dir = TempDir::new("read");
        let path = dir.0.join("r.txt");
        fs::write(&path, "x\ny").unwrap();
        let opened = open(&path).unwrap();
        assert_eq!(opened.buffer.line(1), "y");
    }
}
