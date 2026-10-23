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
//!
//! Files in other encodings than UTF-8 (M17) are decoded into a rope when
//! they open and encoded back when they save, in blocks; the buffer only
//! ever holds UTF-8 ([`crate::encoding`]). Which encoding a file is in comes
//! from an explicit choice (`--encoding`, the config's `[encodings]` table,
//! "Reopen with Encoding"), else its byte order mark, else a guess from its
//! first 64 KB.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, PoisonError, RwLock};
use std::time::Instant;

use nimbus_core::mapped::Limits;
use nimbus_core::{AnyBuffer, Buffer, LineEnding, MappedBuffer, RopeBuffer, Source, Text};

use crate::encoding::{self, Encoding, Sniffed};
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

/// Encodings chosen by file pattern in the config's `[encodings]` table;
/// see [`set_encoding_rules`].
static ENCODING_RULES: RwLock<Vec<(globset::GlobMatcher, bool, Encoding)>> =
    RwLock::new(Vec::new());

/// Use the config's `[encodings]` table, pattern to encoding name
/// (`"*.csv" = "windows-1252"`), for files opened from now on: a file whose
/// name matches is read in that encoding, skipping detection. Returns a
/// warning for each entry that is not understood; see [`encoding_glob`]
/// for how a pattern is matched.
pub fn set_encoding_rules(table: &HashMap<String, String>) -> Vec<String> {
    let mut rules = Vec::new();
    let mut warnings = Vec::new();
    let mut entries: Vec<_> = table.iter().collect();
    // The table has no order; sorted, the first match is at least stable.
    entries.sort();
    for (pattern, name) in entries {
        let Some(encoding) = Encoding::from_label(name) else {
            warnings.push(format!("config: encodings: unknown encoding {name:?}"));
            continue;
        };
        match encoding_glob(pattern) {
            Ok((glob, whole)) => rules.push((glob, whole, encoding)),
            Err(e) => warnings.push(format!("config: encodings: pattern {pattern:?}: {e}")),
        }
    }
    *ENCODING_RULES
        .write()
        .unwrap_or_else(PoisonError::into_inner) = rules;
    warnings
}

/// Compile one `[encodings]` pattern, and say whether it is matched against
/// the whole path (`true`) or the file name alone.
///
/// A pattern without a `/` (`*.csv`) names files anywhere. One with a `/`
/// is matched against the whole path, but paths are absolute, so a
/// relative pattern such as `legacy/*.txt` would never match as written.
/// It is read the way people mean it, as the end of a path: a folder
/// `legacy` anywhere, holding `*.txt`. Only patterns that start with `/`,
/// `**` or a Windows drive are taken as they are. `*` stops at `/`, so
/// `legacy/*.txt` does not reach into `legacy/old/`; `**` crosses folders.
fn encoding_glob(pattern: &str) -> Result<(globset::GlobMatcher, bool), globset::Error> {
    let whole = pattern.contains('/');
    let anchored = pattern.starts_with('/')
        || pattern.starts_with("**")
        || pattern.as_bytes().get(1) == Some(&b':');
    let full = if whole && !anchored {
        Cow::Owned(format!("**/{pattern}"))
    } else {
        Cow::Borrowed(pattern)
    };
    let glob = globset::GlobBuilder::new(&full)
        .literal_separator(true)
        .build()?;
    Ok((glob.compile_matcher(), whole))
}

/// Whether a compiled `[encodings]` pattern matches `path`.
fn glob_matches(glob: &globset::GlobMatcher, whole: bool, path: &Path) -> bool {
    if whole {
        glob.is_match(path)
    } else {
        path.file_name().is_some_and(|name| glob.is_match(name))
    }
}

/// The encoding the config's `[encodings]` table gives `path`, if any.
fn configured_encoding(path: &Path) -> Option<Encoding> {
    let rules = ENCODING_RULES
        .read()
        .unwrap_or_else(PoisonError::into_inner);
    rules
        .iter()
        .find(|(glob, whole, _)| glob_matches(glob, *whole, path))
        .map(|(_, _, e)| *e)
}

/// A file opened for editing.
#[derive(Debug)]
pub struct Opened {
    /// The file's text, without any byte order mark, as UTF-8.
    pub buffer: AnyBuffer,
    /// The line ending most of its lines use; Enter inserts this.
    pub line_ending: LineEnding,
    /// The file started with its encoding's byte order mark. It is kept out
    /// of the text, so it cannot be edited or typed in front of, and is
    /// written back on save.
    pub bom: bool,
    /// The encoding the file was read in, and is saved back in. A file that
    /// is not UTF-8 but was opened as bytes (M7's path) says UTF-8: its
    /// bytes are kept as they are and written back unchanged.
    pub encoding: Encoding,
    /// Some bytes were not valid in `encoding` and show as U+FFFD; saving
    /// would write that character in their place, so the first save asks.
    pub lossy: bool,
    /// Why the document cannot be edited, when it cannot: a huge file in
    /// another encoding, shown as bytes rather than converted in memory.
    pub read_only: Option<String>,
    /// The mapping behind a mapped buffer: its original bytes, readable
    /// without going through the mapping, and the length to compare the
    /// file on disk with. `None` for a rope.
    pub mapping: Option<Arc<MappedFile>>,
    /// The rest of a mapped file's line counting, for a worker; `None` when
    /// it was all done at open (or for a rope).
    pub indexing: Option<crate::indexer::Indexing>,
    /// A huge file in another encoding, decoded into a private UTF-8 copy
    /// that `mapping` maps (M24): the document never reads the file it is
    /// named after, and saves encode back into `encoding`.
    pub transcoded: bool,
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
    open_as(path, None)
}

/// [`open`], in `encoding` if given (`--encoding`, "Reopen with
/// Encoding"), else the one the config's `[encodings]` table gives, else
/// the detected one.
///
/// # Errors
///
/// Returns an [`OpenError`] describing why the file cannot be edited.
pub fn open_as(path: &Path, encoding: Option<Encoding>) -> Result<Opened, OpenError> {
    let encoding = encoding.or_else(|| configured_encoding(path));
    open_full(
        path,
        MAX_ROPE_BYTES,
        FORCED_BACKEND.get().copied(),
        encoding,
    )
}

/// [`open`], with the size from which files are mapped and the forced
/// backend as parameters, so tests can use tiny limits.
#[cfg(test)]
pub(crate) fn open_with(
    path: &Path,
    rope_limit: u64,
    forced: Option<Backend>,
) -> Result<Opened, OpenError> {
    open_full(path, rope_limit, forced, None)
}

/// [`open_with`] in a chosen encoding.
pub(crate) fn open_full(
    path: &Path,
    rope_limit: u64,
    forced: Option<Backend>,
    chosen: Option<Encoding>,
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
    // The first 64 KB say whether there is a byte order mark and, if the
    // file is not UTF-8, what it might be instead. Never more: a 4 GB file
    // is not read to guess.
    let mut sample = Vec::new();
    (&file)
        .take(encoding::SNIFF_BYTES as u64)
        .read_to_end(&mut sample)
        .and_then(|_| (&file).seek(SeekFrom::Start(0)))
        .map_err(OpenError::Io)?;
    let complete = sample.len() < encoding::SNIFF_BYTES;
    let bom = encoding::bom_of(&sample);
    // An explicit choice wins; else a UTF-16 byte order mark decides. A
    // UTF-8 one is handled by the UTF-8 path below.
    let decode_as = chosen.or(bom.map(|(e, _)| e)).filter(|e| !e.is_utf8());
    if let Some(e) = decode_as {
        let skip = bom.filter(|(b, _)| *b == e).map_or(0, |(_, n)| n);
        log::debug!(
            "open: {} in {} ({})",
            path.display(),
            e.name(),
            if chosen.is_some() {
                "chosen"
            } else {
                "byte order mark"
            }
        );
        return open_decoded(path, file, (e, skip), backend);
    }
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
                if chosen.is_none() && encoding::sniff_not_utf8(&sample) == Sniffed::Windows1252 {
                    log::debug!(
                        "open: {} is not UTF-8; its first {} bytes read as Windows-1252",
                        path.display(),
                        sample.len()
                    );
                    (&file).seek(SeekFrom::Start(0)).map_err(OpenError::Io)?;
                    return decode_rope(&file, encoding::WINDOWS_1252, 0).map_err(OpenError::Io);
                }
                log::debug!("open: {} is not UTF-8 and not text: bytes", path.display());
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
    // A guess does not take editing away: a huge file whose start reads
    // as Windows-1252 stays on M7's byte path, editable, its bytes kept.
    if encoding::sniff(&sample, complete) == Sniffed::Windows1252 {
        log::debug!(
            "open: {}'s first {} bytes are not UTF-8 but read as Windows-1252; \
             too large to convert, so bytes",
            path.display(),
            sample.len()
        );
    }
    let opened = open_mapped(file)?;
    log::debug!("open: {} mapped in {:?}", path.display(), started.elapsed());
    Ok(opened)
}

/// Open `file`, known to be in `encoding` after `skip` bytes of byte order
/// mark: decoded into a rope, or read-only as bytes if it is to be mapped.
fn open_decoded(
    path: &Path,
    file: fs::File,
    (encoding, skip): (Encoding, usize),
    backend: Backend,
) -> Result<Opened, OpenError> {
    if backend == Backend::Mapped {
        let started = Instant::now();
        return match transcode(&file, encoding, skip) {
            Ok(opened) => {
                log::debug!(
                    "open: {} decoded from {} into a private UTF-8 copy in {:?}",
                    path.display(),
                    encoding.name(),
                    started.elapsed()
                );
                Ok(opened)
            }
            Err(e) => {
                // No room for the copy, most likely: still show the file.
                log::warn!(
                    "open: cannot decode {} into a copy ({e}); read-only",
                    path.display()
                );
                open_read_only(path, file, encoding)
            }
        };
    }
    let started = Instant::now();
    let opened = decode_rope(&file, encoding, skip).map_err(OpenError::Io)?;
    log::debug!(
        "open: {} decoded into a rope in {:?}",
        path.display(),
        started.elapsed()
    );
    Ok(opened)
}

/// Decode a huge `file` in `encoding` (after `skip` bytes of byte order
/// mark) into a private UTF-8 copy in the system's temporary folder, and
/// map the copy as the document (M24).
///
/// M17 left such files read-only: converting one in memory would hold all
/// of it, and editing its bytes as UTF-8 would mix two encodings in one
/// file. A copy on disk costs neither: the decoding streams a block at a
/// time (the same [`Decoding`] reader a rope uses), the copy's lines are
/// counted as it is written, and from then on the document is an ordinary
/// mapped UTF-8 document, with M7's piece table, undo and search. Saving
/// encodes it back into `encoding` beside the real file and renames it
/// over, never touching the copy (`Document::begin_save`).
///
/// The copy costs disk space: about the file's size for single-byte
/// encodings with mostly ASCII text, up to one and a half times for East
/// Asian text (two bytes become three) or twice for UTF-16. On Unix its
/// name is removed as soon as it is created, so it vanishes when the
/// document closes, even after a crash; on Windows, which cannot remove a
/// file that is open, it is deleted when the document lets go of it.
///
/// # Errors
///
/// Returns the error from creating, writing or mapping the copy, for
/// example when the temporary folder's disk is full. Nothing is left
/// behind then.
fn transcode(file: &fs::File, encoding: Encoding, skip: usize) -> std::io::Result<Opened> {
    let (temp, mut out) = private_temp()?;
    #[cfg(unix)]
    let remove_later: Option<PathBuf> = {
        // Open and nameless from here: the space is freed on close.
        fs::remove_file(&temp)?;
        None
    };
    #[cfg(not(unix))]
    let remove_later = Some(temp.clone());
    let written = write_decoded(file, encoding, skip, &mut out);
    let (index, lossy) = match written {
        Ok(done) => done,
        Err(e) => {
            drop(out);
            if let Some(temp) = &remove_later {
                let _ = fs::remove_file(temp);
            }
            return Err(e);
        }
    };
    let mapped = match MappedFile::new(out) {
        Ok(m) => m,
        Err(e) => {
            if let Some(temp) = &remove_later {
                let _ = fs::remove_file(temp);
            }
            return Err(e);
        }
    };
    let mapped = Arc::new(match remove_later {
        Some(temp) => mapped.delete_on_drop(temp),
        None => mapped,
    });
    let mut sample = vec![0; mapped.len().min(LINE_ENDING_SAMPLE)];
    mapped.read_at(0, &mut sample)?;
    let line_ending = LineEnding::detect([&sample]);
    let source: Arc<dyn Source> = mapped.clone();
    let buffer = MappedBuffer::with_index(source, index, Limits::default())
        .map_err(std::io::Error::other)?;
    Ok(Opened {
        buffer: buffer.into(),
        line_ending,
        bom: skip > 0,
        encoding,
        lossy,
        read_only: None,
        mapping: Some(mapped),
        indexing: None,
        transcoded: true,
    })
}

/// Decode `file` from `encoding`, after `skip` bytes, into `out` as UTF-8,
/// a block at a time, counting its lines on the way. Returns the finished
/// line index and whether any bytes were not valid in `encoding` (they
/// become U+FFFD).
fn write_decoded(
    file: &fs::File,
    encoding: Encoding,
    skip: usize,
    out: &mut fs::File,
) -> std::io::Result<(nimbus_core::line_index::LineIndex, bool)> {
    use nimbus_core::line_index::{LineIndex, Scanner};
    let mut input = file;
    input.seek(SeekFrom::Start(skip as u64))?;
    let mut decoder = encoding.decoder();
    let mut block = vec![0; DECODE_BLOCK];
    let mut text = String::new();
    let mut scanner = Scanner::default();
    loop {
        let n = input.read(&mut block)?;
        let last = n == 0;
        text.clear();
        decoder.decode(&block[..n], last, &mut text);
        scanner.feed(text.as_bytes());
        out.write_all(text.as_bytes())?;
        if last {
            break;
        }
    }
    out.flush()?;
    scanner.finish();
    let mut index = LineIndex::new();
    index
        .merge(scanner.take_chunk())
        .map_err(std::io::Error::other)?;
    Ok((index, decoder.lossy()))
}

/// A new file in the system's temporary folder that only this user can
/// read, open for reading and writing. `create_new` never opens a file
/// someone else put at the name.
///
/// # Errors
///
/// Returns the error from creating the file, or from the last name tried.
fn private_temp() -> std::io::Result<(PathBuf, fs::File)> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir();
    let mut last = std::io::Error::other("no temporary name was free");
    for _ in 0..16 {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("nimbus-decoded-{}-{n}.txt", std::process::id()));
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// Map a huge `file` known to be in `encoding` (by its byte order mark or
/// an explicit choice) as bytes, read-only: the fallback when it cannot be
/// decoded into a copy ([`transcode`]), for example with the temporary
/// folder's disk full.
fn open_read_only(path: &Path, file: fs::File, encoding: Encoding) -> Result<Opened, OpenError> {
    let mut opened = open_mapped(file)?;
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    opened.encoding = encoding;
    opened.read_only = Some(format!(
        "{name} is {}, too large to convert (over {} MB): read-only, shown as bytes",
        encoding.name(),
        MAX_ROPE_BYTES / (1024 * 1024)
    ));
    log::debug!("open: {} read-only, in {}", path.display(), encoding.name());
    Ok(opened)
}

/// Read `file` from where it is positioned, after skipping `skip` bytes (a
/// byte order mark), decoding it from `encoding` into a rope a block at a
/// time: no full-size copy of the bytes or of the text is held besides the
/// rope itself.
///
/// # Errors
///
/// Returns the read error.
fn decode_rope(file: &fs::File, encoding: Encoding, skip: usize) -> std::io::Result<Opened> {
    let mut reader = Decoding {
        file,
        decoder: encoding.decoder(),
        input: vec![0; DECODE_BLOCK],
        text: String::new(),
        pos: 0,
        done: false,
    };
    std::io::copy(
        &mut (&mut reader.file).take(skip as u64),
        &mut std::io::sink(),
    )?;
    let buffer = RopeBuffer::from_reader(&mut reader)?;
    let line_ending = LineEnding::detect(buffer.chunks());
    Ok(Opened {
        buffer: buffer.into(),
        line_ending,
        bom: skip > 0,
        encoding,
        lossy: reader.decoder.lossy(),
        read_only: None,
        mapping: None,
        indexing: None,
        transcoded: false,
    })
}

/// How many bytes of a file are decoded at a time: 4 MiB.
const DECODE_BLOCK: usize = 4 * 1024 * 1024;

/// A reader that hands out a file's text as UTF-8, decoding it from another
/// encoding a block at a time; what [`RopeBuffer::from_reader`] reads from.
struct Decoding<'a> {
    file: &'a fs::File,
    decoder: encoding::Decoder,
    /// One block of the file's bytes, reused.
    input: Vec<u8>,
    /// The block decoded, handed out from `pos` on.
    text: String,
    pos: usize,
    /// The end of the file was reached and the decoder told so.
    done: bool,
}

impl Read for Decoding<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.pos == self.text.len() {
            if self.done {
                return Ok(0);
            }
            let n = self.file.read(&mut self.input)?;
            self.text.clear();
            self.pos = 0;
            self.done = n == 0;
            self.decoder
                .decode(&self.input[..n], self.done, &mut self.text);
        }
        let rest = &self.text.as_bytes()[self.pos..];
        let n = rest.len().min(buf.len());
        buf[..n].copy_from_slice(&rest[..n]);
        self.pos += n;
        Ok(n)
    }
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
        encoding: Encoding::Utf8,
        lossy: false,
        read_only: None,
        mapping: None,
        indexing: None,
        transcoded: false,
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
        encoding: Encoding::Utf8,
        lossy: false,
        read_only: None,
        mapping: None,
        indexing: None,
        transcoded: false,
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
        encoding: Encoding::Utf8,
        lossy: false,
        read_only: None,
        mapping: Some(mapped),
        indexing,
        transcoded: false,
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

impl SaveError {
    /// The character that stopped the save, when the target encoding has
    /// no bytes for it.
    #[must_use]
    pub fn unencodable(&self) -> Option<&Unencodable> {
        self.source.get_ref()?.downcast_ref()
    }
}

/// A character the file's encoding cannot hold (an emoji in a
/// Windows-1252 file): the save stops there and writes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unencodable {
    /// The character.
    pub ch: char,
    /// Its byte offset in the document.
    pub offset: usize,
    /// The encoding's name.
    pub encoding: &'static str,
}

impl std::fmt::Display for Unencodable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} cannot hold the character {:?} (U+{:04X})",
            self.encoding,
            self.ch,
            u32::from(self.ch)
        )
    }
}

impl std::error::Error for Unencodable {}

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
    save_encoded(path, text, Encoding::Utf8, bom)
}

/// [`save`], encoding the text in `encoding`, preceded by its byte order
/// mark if `bom` is set. For UTF-8 the bytes are written as they are, as
/// [`save`] says; for any other encoding the document must be valid UTF-8
/// (a rope always is).
///
/// # Errors
///
/// As for [`save`]; also a [`SaveError`] whose
/// [`unencodable`](SaveError::unencodable) names the first character
/// `encoding` cannot hold. The target is untouched then.
pub fn save_encoded(
    path: &Path,
    text: &dyn Text,
    encoding: Encoding,
    bom: bool,
) -> Result<fs::Metadata, SaveError> {
    // Write next to the real file, so the rename stays within one directory
    // (and one filesystem), and so a symlink is followed, not overwritten.
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let (tmp, mut file) = create_temp(&target)?;

    let result = write_temp(&mut file, &target, text, (encoding, bom)).and_then(|()| {
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
    (encoding, bom): (Encoding, bool),
) -> Result<(), SaveError> {
    copy_permissions(file, target)?;
    if bom && let Some(mark) = encoding.bom() {
        file.write_all(mark)
            .map_err(step("writing the temporary file"))?;
    }
    if encoding.is_utf8() {
        write_text(file, text, SAVE_CHUNK, |_| true)?;
    } else {
        write_encoded(file, text, encoding, SAVE_CHUNK)?;
    }
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

/// Write `text` to `out` encoded in `encoding`, reading at most `chunk`
/// bytes of it at a time. A character cut by the end of a chunk waits for
/// the next one, since an encoder takes whole characters.
///
/// # Errors
///
/// Returns a [`SaveError`] for a failed read or write, for bytes in the
/// document that are not UTF-8, or for a character `encoding` cannot hold
/// (see [`SaveError::unencodable`]).
pub(crate) fn write_encoded(
    out: &mut impl Write,
    text: &dyn Text,
    encoding: Encoding,
    chunk: usize,
) -> Result<(), SaveError> {
    let len = text.len_bytes();
    let chunk = chunk.max(4);
    let mut encoder = encoding.encoder();
    let mut buf = Vec::with_capacity(len.min(chunk) + 4);
    let mut bytes = Vec::new();
    // Document offset of `buf[0]`.
    let mut base = 0;
    let mut pos = 0;
    while pos < len || !buf.is_empty() {
        let end = len.min(pos.saturating_add(chunk));
        let before = buf.len();
        text.read_into(pos..end, &mut buf)
            .map_err(step("reading the document"))?;
        if buf.len() - before != end - pos {
            return Err(step("reading the document")(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the document ended early",
            )));
        }
        pos = end;
        let last = pos == len;
        let valid = match std::str::from_utf8(&buf) {
            Ok(s) => s.len(),
            Err(e) if e.error_len().is_none() && !last => e.valid_up_to(),
            Err(_) => {
                return Err(step("encoding the text")(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "the document holds bytes that are not text",
                )));
            }
        };
        let s = std::str::from_utf8(&buf[..valid]).map_err(|e| {
            step("encoding the text")(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        })?;
        bytes.clear();
        if let Err(u) = encoder.encode(s, last, &mut bytes) {
            return Err(step("encoding the text")(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                Unencodable {
                    ch: u.ch,
                    offset: base + u.at,
                    encoding: encoding.name(),
                },
            )));
        }
        out.write_all(&bytes)
            .map_err(step("writing the temporary file"))?;
        buf.drain(..valid);
        base += valid;
        if last {
            break;
        }
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
    fn other_encodings_decode_and_save_back_byte_for_byte() {
        let dir = TempDir::new("encodings");
        let little: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("naïve 日本\r\nb".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let big: Vec<u8> = [0xFE, 0xFF]
            .into_iter()
            .chain("x 👩‍💻\n".encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        for (name, bytes, text, encoding, bom) in [
            ("le.txt", little, "naïve 日本\r\nb", Encoding::Utf16Le, true),
            ("be.txt", big, "x 👩‍💻\n", Encoding::Utf16Be, true),
            (
                "w.csv",
                b"id,name\r\n1,caf\xe9 \x80 \x93ok\x94\r\n".to_vec(),
                "id,name\r\n1,café € “ok”\r\n",
                encoding::WINDOWS_1252,
                false,
            ),
        ] {
            let path = dir.0.join(name);
            fs::write(&path, &bytes).unwrap();
            let opened = open(&path).unwrap();
            assert_eq!(
                (opened.encoding, opened.bom, opened.lossy),
                (encoding, bom, false),
                "{name}"
            );
            assert!(!opened.buffer.is_mapped() && opened.read_only.is_none());
            assert_eq!(opened.buffer.slice(0..opened.buffer.len_bytes()), text);
            save_encoded(&path, &opened.buffer, opened.encoding, opened.bom).unwrap();
            assert_eq!(fs::read(&path).unwrap(), bytes, "{name}");
        }
    }

    #[test]
    fn decoding_crosses_block_boundaries() {
        // Characters cut by the 4 MiB block edge, in UTF-16 and in
        // Windows-1252 (one byte per character, so never cut).
        let dir = TempDir::new("blocks");
        let text = "é😀x".repeat(DECODE_BLOCK / 5);
        let path = dir.0.join("big16.txt");
        let bytes: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        fs::write(&path, &bytes).unwrap();
        let opened = open(&path).unwrap();
        assert_eq!(opened.buffer.len_bytes(), text.len());
        // Not assert_eq: a failure would print 20 MB.
        assert!(opened.buffer.slice(0..opened.buffer.len_bytes()).eq(&text));
        let copy = dir.0.join("copy");
        save_encoded(&copy, &opened.buffer, opened.encoding, opened.bom).unwrap();
        assert!(fs::read(&copy).unwrap().eq(&bytes));
    }

    #[test]
    fn explicit_encodings_skip_detection_and_config_patterns_choose_one() {
        let dir = TempDir::new("explicit");
        // An extension no other test uses: the rules are global.
        let path = dir.0.join("data.m17-test");
        // Valid UTF-8, but meant as Latin-1.
        fs::write(&path, "caf\u{e9}".as_bytes()).unwrap();
        assert_eq!(open(&path).unwrap().encoding, Encoding::Utf8);
        let latin = open_as(&path, Some(Encoding::Latin1)).unwrap();
        assert_eq!(latin.encoding, Encoding::Latin1);
        assert_eq!(latin.buffer.slice(0..latin.buffer.len_bytes()), "cafÃ©");

        let warnings = set_encoding_rules(&HashMap::from([
            ("*.m17-test".to_owned(), "latin1".to_owned()),
            ("*.x".to_owned(), "klingon".to_owned()),
        ]));
        assert_eq!(warnings.len(), 1);
        assert_eq!(configured_encoding(&path), Some(Encoding::Latin1));
        assert_eq!(configured_encoding(&dir.0.join("a.txt")), None);
        set_encoding_rules(&HashMap::new());
        assert_eq!(configured_encoding(&path), None);
    }

    #[test]
    fn encoding_patterns_with_a_folder_match_the_end_of_the_path() {
        let hit = |pattern: &str, path: &str| {
            let (glob, whole) = encoding_glob(pattern).unwrap();
            glob_matches(&glob, whole, Path::new(path))
        };
        assert!(hit("legacy/*.txt", "/home/a/proj/legacy/notes.txt"));
        assert!(!hit("legacy/*.txt", "/home/a/proj/legacy/old/notes.txt"));
        assert!(!hit("legacy/*.txt", "/home/a/proj/notlegacy/notes.txt"));
        assert!(!hit("legacy/*.txt", "/home/a/proj/notes.txt"));
        assert!(hit("**/legacy/*.txt", "/home/a/proj/legacy/notes.txt"));
        assert!(hit("legacy/**/*.txt", "/p/legacy/old/notes.txt"));
        assert!(hit("/home/a/*.txt", "/home/a/notes.txt"));
        assert!(!hit("/home/a/*.txt", "/srv/home/a/notes.txt"));
        assert!(hit("*.csv", "/x/y/data.csv"));
        assert!(!hit("*.csv", "/x/y.csv/data"));
    }

    #[test]
    fn unencodable_characters_stop_the_save_and_name_their_place() {
        let dir = TempDir::new("unencodable");
        let path = dir.0.join("w.txt");
        fs::write(&path, b"caf\xe9\n").unwrap();
        let mut opened = open(&path).unwrap();
        assert_eq!(opened.encoding, encoding::WINDOWS_1252);
        opened.buffer.replace(6..6, "\u{1F600}").unwrap();
        let err = save_encoded(&path, &opened.buffer, opened.encoding, false).unwrap_err();
        let u = err.unencodable().unwrap();
        assert_eq!((u.ch, u.offset), ('\u{1F600}', 6));
        assert_eq!(fs::read(&path).unwrap(), b"caf\xe9\n", "nothing written");
        // As UTF-8 it saves.
        save_encoded(&path, &opened.buffer, Encoding::Utf8, false).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "café\n\u{1F600}");
    }

    #[test]
    fn invalid_utf16_opens_with_replacements_marked_lossy() {
        let dir = TempDir::new("lossy");
        let path = dir.0.join("bad16.txt");
        fs::write(&path, b"\xFF\xFEa\x00\x00\xD8b\x00").unwrap();
        let opened = open(&path).unwrap();
        assert!(opened.lossy);
        assert_eq!(
            opened.buffer.slice(0..opened.buffer.len_bytes()),
            "a\u{fffd}b"
        );
    }

    #[test]
    fn huge_files_in_known_encodings_open_decoded_and_editable() {
        let dir = TempDir::new("huge-enc");
        let utf16 = dir.0.join("u16.log");
        fs::write(&utf16, b"\xFF\xFEa\x00\n\x00").unwrap();
        let opened = open_with(&utf16, 0, None).unwrap();
        assert!(opened.buffer.is_mapped() && opened.transcoded && opened.bom);
        assert_eq!(opened.encoding, Encoding::Utf16Le);
        assert_eq!(opened.read_only, None);
        assert_eq!(opened.buffer.slice(0..opened.buffer.len_bytes()), "a\n");
        let chosen = dir.0.join("w.log");
        fs::write(&chosen, b"caf\xe9 au lait\n").unwrap();
        let opened = open_full(&chosen, 0, None, Some(encoding::WINDOWS_1252)).unwrap();
        assert_eq!(opened.encoding, encoding::WINDOWS_1252);
        assert!(opened.transcoded && opened.read_only.is_none() && !opened.lossy);
        assert_eq!(
            opened.buffer.slice(0..opened.buffer.len_bytes()),
            "café au lait\n"
        );
        assert!(opened.buffer.is_fully_indexed());
        // A guess keeps M7's editable byte path; UTF-8 stays editable too.
        let guessed = open_with(&chosen, 0, None).unwrap();
        assert!(guessed.read_only.is_none() && guessed.encoding.is_utf8());
        assert!(!guessed.transcoded);
        let fine = dir.0.join("fine.log");
        fs::write(&fine, "café\n").unwrap();
        assert!(open_with(&fine, 0, None).unwrap().read_only.is_none());
    }

    #[test]
    fn decoding_a_huge_file_counts_its_lines_and_notes_bad_bytes() {
        let dir = TempDir::new("huge-utf16-bad");
        let path = dir.0.join("odd.txt");
        // "a\nb" then half a UTF-16 unit.
        fs::write(&path, b"\xFF\xFEa\x00\n\x00b\x00\x00").unwrap();
        let opened = open_with(&path, 0, None).unwrap();
        assert!(opened.transcoded && opened.lossy);
        let text = opened.buffer.slice(0..opened.buffer.len_bytes());
        assert_eq!(text, "a\nb\u{fffd}");
        assert_eq!(opened.buffer.len_lines(), 2);
    }

    /// `cargo test --release -p nimbus -- --ignored --nocapture encoding_open_bench`
    ///
    /// M17's budget: opening a 10 MB Windows-1252 file costs no more than
    /// twice opening the same text as UTF-8. Best of five each.
    #[test]
    #[ignore = "benchmark: run with --release"]
    fn encoding_open_bench() {
        let dir = TempDir::new("enc-bench");
        let line =
            "id,name,city,note\r\n42,Ren\u{e9}e,Z\u{fc}rich,caf\u{e9} cr\u{e8}me 5 \u{20ac}\r\n";
        let text = line.repeat(10 * 1024 * 1024 / line.len());
        let utf8 = dir.0.join("utf8.csv");
        fs::write(&utf8, &text).unwrap();
        let mut w1252 = Vec::new();
        encoding::WINDOWS_1252
            .encoder()
            .encode(&text, true, &mut w1252)
            .unwrap();
        let cp = dir.0.join("w1252.csv");
        fs::write(&cp, &w1252).unwrap();
        let best = |path: &Path| {
            (0..5)
                .map(|_| {
                    let t = Instant::now();
                    let opened = open(path).unwrap();
                    assert!(!opened.buffer.is_mapped());
                    t.elapsed()
                })
                .min()
                .unwrap()
        };
        let (a, b) = (best(&utf8), best(&cp));
        println!(
            "open 10 MB: UTF-8 {a:?}, Windows-1252 {b:?} ({:.2}x)",
            b.as_secs_f64() / a.as_secs_f64()
        );
        let t = Instant::now();
        let opened = open(&cp).unwrap();
        save_encoded(
            &dir.0.join("out.csv"),
            &opened.buffer,
            opened.encoding,
            false,
        )
        .unwrap();
        println!("open and save the Windows-1252 file: {:?}", t.elapsed());
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
