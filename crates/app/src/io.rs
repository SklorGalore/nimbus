//! Opening and saving files.
//!
//! Saving is *atomic*: the new contents go to a temporary file next to the
//! target, which is flushed to disk and then renamed over the target. A
//! rename within one directory either fully happens or does not, so a crash
//! or power cut mid-save leaves the old file or the new file, never half of
//! each.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use nimbus_core::{Buffer, LineEnding, RopeBuffer, Text};

/// Files at or above this size are refused until the memory-mapped buffer
/// arrives in M7 (DESIGN.md §4.1).
pub const MAX_ROPE_BYTES: u64 = 256 * 1024 * 1024;

/// Lines longer than this are refused until M7 adds column-windowed access
/// to long lines (DESIGN.md §4.1). Each frame finds and shapes the visible
/// part of every line, and finding it is linear in the line's length.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// The UTF-8 byte order mark some Windows tools put at the start of a file.
const BOM: &str = "\u{feff}";

/// A file read into memory.
#[derive(Debug)]
pub struct Opened {
    /// The file's text, without any byte order mark.
    pub buffer: RopeBuffer,
    /// The line ending most of its lines use; Enter inserts this.
    pub line_ending: LineEnding,
    /// The file started with a UTF-8 byte order mark. It is kept out of the
    /// text, so it cannot be edited or typed in front of, and is written
    /// back on save.
    pub bom: bool,
}

/// Why a file could not be opened.
#[derive(Debug)]
pub enum OpenError {
    /// No file at the path. The editor treats this as a new, empty file
    /// that will be created on save.
    NotFound,
    /// Larger than [`MAX_ROPE_BYTES`].
    TooLarge {
        /// File size in bytes.
        bytes: u64,
    },
    /// A line is longer than [`MAX_LINE_BYTES`].
    LineTooLong {
        /// Length of the longest line in bytes.
        bytes: usize,
    },
    /// The file is not valid UTF-8, so it may be binary or use another
    /// encoding. M7 opens these in a byte-preserving buffer.
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
            OpenError::TooLarge { bytes } => write!(
                f,
                "file is {} MB; files over {} MB need the huge-file mode planned for M7",
                bytes / (1024 * 1024),
                MAX_ROPE_BYTES / (1024 * 1024)
            ),
            OpenError::LineTooLong { bytes } => write!(
                f,
                "a line is {} KB long; lines over {} KB need the long-line support planned for M7",
                bytes / 1024,
                MAX_LINE_BYTES / 1024
            ),
            OpenError::NotUtf8 => write!(f, "file is not valid UTF-8 (binary or another encoding)"),
            OpenError::IsDirectory => write!(f, "path is a directory"),
            OpenError::NotAFile => write!(f, "not a regular file"),
            OpenError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for OpenError {}

/// Read `path` into a buffer.
///
/// # Errors
///
/// Returns an [`OpenError`] describing why the file cannot be edited.
pub fn open(path: &Path) -> Result<Opened, OpenError> {
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
    if meta.len() >= MAX_ROPE_BYTES {
        return Err(OpenError::TooLarge { bytes: meta.len() });
    }

    // Stream straight into the rope, with no full-size intermediate string.
    // The limit also covers a file that grew after the size check.
    let file = fs::File::open(path).map_err(OpenError::Io)?;
    let mut limited = std::io::BufReader::new(file).take(MAX_ROPE_BYTES);
    let mut buffer = RopeBuffer::from_reader(&mut limited).map_err(|e| {
        if e.kind() == std::io::ErrorKind::InvalidData {
            OpenError::NotUtf8
        } else {
            OpenError::Io(e)
        }
    })?;
    if limited.limit() == 0 {
        return Err(OpenError::TooLarge {
            bytes: MAX_ROPE_BYTES,
        });
    }

    let longest = longest_line(buffer.chunks());
    if longest > MAX_LINE_BYTES {
        return Err(OpenError::LineTooLong { bytes: longest });
    }
    let bom = buffer.slice(0..BOM.len()) == BOM;
    if bom {
        buffer
            .replace(0..BOM.len(), "")
            .map_err(|e| OpenError::Io(std::io::Error::other(e)))?;
    }
    let line_ending = LineEnding::detect(buffer.chunks());
    Ok(Opened {
        buffer,
        line_ending,
        bom,
    })
}

/// Length in bytes of the longest line in the text formed by `chunks`.
fn longest_line<'a>(chunks: impl IntoIterator<Item = &'a str>) -> usize {
    let (mut longest, mut current) = (0, 0);
    for chunk in chunks {
        for &b in chunk.as_bytes() {
            if b == b'\n' {
                longest = longest.max(current);
                current = 0;
            } else {
                current += 1;
            }
        }
    }
    longest.max(current)
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

fn step(step: &'static str) -> impl FnOnce(std::io::Error) -> SaveError {
    move |source| SaveError { step, source }
}

/// Write `buffer` to `path` atomically, byte for byte as it is in memory,
/// preceded by a byte order mark if `bom` is set.
///
/// Line endings are not rewritten: the buffer already holds whatever the
/// file had plus what Enter inserted, so an unedited file round-trips
/// exactly, mixed endings included.
///
/// If `path` is a symlink, the file it points to is replaced and the link
/// is kept. The original file's permissions are kept.
///
/// # Errors
///
/// Returns a [`SaveError`] naming the step that failed. The target file is
/// untouched unless the final rename succeeded.
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

pub fn save(path: &Path, buffer: &RopeBuffer, bom: bool) -> Result<fs::Metadata, SaveError> {
    // Write next to the real file, so the rename stays within one directory
    // (and one filesystem), and so a symlink is followed, not overwritten.
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let (tmp, mut file) = create_temp(&target)?;

    let result = write_temp(&mut file, &target, buffer, bom).and_then(|()| {
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

/// Create a new temporary file beside `target`.
///
/// `create_new` refuses to open anything that already exists, including a
/// symlink someone planted at the predictable name, so a save can never be
/// redirected into another file. A clash moves on to the next name.
fn create_temp(target: &Path) -> Result<(PathBuf, fs::File), SaveError> {
    let name = target
        .file_name()
        .map_or_else(|| "untitled".into(), |n| n.to_string_lossy().into_owned());
    let mut last_err = std::io::Error::other("no temporary name was free");
    for attempt in 0..16u32 {
        let tmp = target.with_file_name(format!(
            ".{name}.nimbus-save-{}-{attempt}",
            std::process::id()
        ));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            // Owner-only until the real permissions are copied, so a secret
            // file's new contents are never readable by others mid-save.
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
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
    buffer: &RopeBuffer,
    bom: bool,
) -> Result<(), SaveError> {
    // Copy permissions before writing any content.
    if let Ok(meta) = fs::metadata(target) {
        file.set_permissions(meta.permissions())
            .map_err(step("copying permissions"))?;
    }
    {
        let mut out = std::io::BufWriter::new(&mut *file);
        if bom {
            out.write_all(BOM.as_bytes())
                .map_err(step("writing the temporary file"))?;
        }
        for chunk in buffer.chunks() {
            out.write_all(chunk.as_bytes())
                .map_err(step("writing the temporary file"))?;
        }
        out.flush().map_err(step("writing the temporary file"))?;
    }
    // Make sure the bytes are on disk before the rename makes them visible.
    file.sync_all().map_err(step("flushing to disk"))
}

/// Rename, retrying once: on Windows a virus scanner or indexer briefly
/// holding the target open makes the first attempt fail.
fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
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
    fn refuses_non_utf8_directories_and_long_lines() {
        let dir = TempDir::new("refuse");
        let bin = dir.0.join("bin");
        fs::write(&bin, [0xff, 0xfe, 0x00]).unwrap();
        assert!(matches!(open(&bin), Err(OpenError::NotUtf8)));
        assert!(matches!(open(&dir.0), Err(OpenError::IsDirectory)));
        let long = dir.0.join("min.json");
        fs::write(&long, "x".repeat(MAX_LINE_BYTES + 1)).unwrap();
        assert!(matches!(open(&long), Err(OpenError::LineTooLong { .. })));
        assert_eq!(longest_line(["ab\nabcd", "ef\n", "g"]), 6);
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

    #[cfg(unix)]
    #[test]
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
