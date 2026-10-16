//! Saving a memory-mapped document (M7).
//!
//! A mapped document's text is mostly the bytes of the file it was opened
//! from, read through a memory mapping ([`crate::mapping`]). Saving it over
//! that same file is the delicate case: the new file is made *from* the old
//! one, so the old one must stay readable until the new one is complete,
//! and the document must never be left reading bytes that are gone.
//!
//! # The steps
//!
//! 1. **Write**, on a worker thread ([`SaveJob::run`]). A snapshot of the
//!    document is streamed into a new temporary file beside the target,
//!    4 MiB at a time through one reused buffer. The original file's pieces
//!    are read with ordinary file reads (`pread`), not through the mapping,
//!    so a gigabyte of the file is never counted against Nimbus's memory.
//!    The same bytes feed a line-index [`Scanner`], so the new file arrives
//!    with every line counted, and the file is flushed to disk
//!    (`sync_all`). Meanwhile edits wait; moving, scrolling and searching
//!    do not.
//! 2. **Switch**, on the main thread (`Document::save_written`). The temp
//!    file is mapped and the document becomes a fresh piece table over it:
//!    the same bytes, one piece, every line counted. Nothing of the
//!    document refers to the old file any more: its mapping, its line
//!    counter and its undo history (whose steps may hold pieces of the old
//!    file, `Content::Pieces`) are all let go.
//! 3. **Replace**: the temp file is renamed over the target.
//!
//! # Why the order differs by platform
//!
//! On macOS and Linux a rename only changes which file a *name* points to.
//! The old file lives on, nameless and unchanged, for as long as anything
//! has it open or mapped, so a search still reading it is undisturbed and
//! the rename happens straight after the switch. The document's last hold
//! on the old file is let go after the rename, on a short-lived thread
//! ([`release_later`]): closing a nameless file for the last time frees its
//! disk space, about 20 ms per GB on APFS, which the window should not wait
//! for.
//!
//! Windows refuses to replace a file that is mapped anywhere, so the old
//! file's mapping must be gone first. The app checks every [`POLL`] (from
//! the event loop, never blocking the window) whether the old file's
//! `Arc<MappedFile>` is down to the one reference the save keeps, stopping
//! a running search meanwhile, for up to [`RELEASE_WAIT`]. Then it drops
//! that reference, which unmaps the file, and renames. If Windows also
//! refuses to rename the temp file because *it* is mapped, the document
//! lets go of that mapping for a moment, renames the file, and maps it
//! again through a handle kept open throughout, so the bytes never become
//! unreachable (`Document::replace_unmapped`).
//!
//! # When the rename fails
//!
//! The new contents are safe on disk in the temp file, and the document is
//! already reading them, so it stays that way: dirty, with a message saying
//! where the file is. The temp file is deleted once nothing reads it (after
//! the next successful save, or when the document closes), by
//! [`MappedFile::delete_on_drop`].
//!
//! # Invariants
//!
//! - The worker reads only its snapshot, whose pieces name bytes that never
//!   change (`nimbus_core::mapped`, invariant 3), so what it writes is
//!   exactly the text at the moment the save began.
//! - Edits are refused from the start of a save until it ends (the app
//!   checks `Document::is_saving`), so that text is still the document's
//!   text when it switches files, and the undo history after the switch is
//!   empty. In case an edit slips through, the document compares its
//!   version (which every edit changes) before switching, and keeps its
//!   text rather than switch to a file without the edit.
//! - The document always reads valid bytes: the old file's until the new
//!   one is mapped; on Windows, the moment without a mapping in the
//!   fallback lies within one call on the main thread, where nothing can
//!   draw or edit the document.
//! - The target is only ever touched by the final rename, which happens
//!   whole or not at all.

use std::fs::{File, Metadata};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use nimbus_core::Snapshot;
use nimbus_core::line_index::{LineIndex, Scanner};
use nimbus_core::{Source, Text};

use crate::io::{self, SaveError, step};
use crate::mapping::MappedFile;

/// Progress is reported after every this many bytes: 64 MiB, a few dozen
/// messages for a 4 GB file.
const PROGRESS_EVERY: usize = 64 * 1024 * 1024;

/// How long a save waits for the old file to be released before giving up
/// on replacing it (Windows; see the module documentation).
pub const RELEASE_WAIT: Duration = Duration::from_secs(2);

/// How often the app checks whether the old file is released.
pub const POLL: Duration = Duration::from_millis(10);

static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

/// How the written temp file replaces the target.
#[derive(Debug, Clone, Copy)]
pub struct ReplacePolicy {
    /// Wait until nothing maps the old file before renaming, and if
    /// renaming the mapped temp file fails, unmap it and try again. Needed
    /// on Windows (see the module documentation); on by default only there,
    /// and tests turn it on elsewhere to run the same steps.
    pub unmap_first: bool,
    /// Renames the temp file over the target: [`io::rename_with_retry`].
    /// Tests put a failing one here.
    pub rename: fn(&Path, &Path) -> std::io::Result<()>,
    /// How long to wait for the old file to be released: [`RELEASE_WAIT`].
    pub wait: Duration,
}

impl Default for ReplacePolicy {
    fn default() -> Self {
        Self {
            unmap_first: cfg!(windows),
            rename: io::rename_with_retry,
            wait: RELEASE_WAIT,
        }
    }
}

/// Where a save stands, as `Document::save_written` and
/// `Document::poll_replace` report it.
#[derive(Debug)]
pub enum SaveStep {
    /// Saved: the target holds the new contents and the document reads
    /// them; it is clean.
    Saved,
    /// Written and switched to, but the old file is still mapped somewhere
    /// (Windows): poll again after [`POLL`].
    Waiting,
    /// The new contents are on disk in a temporary file that could not
    /// replace the target. The document reads that file and stays dirty;
    /// the message says where it is and why.
    NotReplaced(String),
    /// Nothing was saved, and the document is as it was.
    Failed(SaveError),
    /// The result of a save this document is no longer waiting for: it was
    /// dropped (and its temp file deleted).
    Stale,
}

/// What the worker reports to the event loop.
#[derive(Debug)]
pub enum SaveEvent {
    /// `written` of `total` bytes are in the temp file.
    Progress {
        /// The save ([`SaveJob::token`]).
        token: u64,
        /// Bytes written so far.
        written: u64,
        /// Bytes to write in all.
        total: u64,
    },
    /// The worker is done, and has let go of the document's snapshot.
    Done {
        /// The save ([`SaveJob::token`]).
        token: u64,
        /// The written file, or why there is none.
        result: Box<Result<Written, SaveError>>,
    },
}

/// The work of writing one save, to run on a worker thread.
pub struct SaveJob {
    /// Identifies this save; results for any other are dropped.
    pub token: u64,
    path: PathBuf,
    snapshot: Box<dyn Snapshot>,
    bom: bool,
    cancel: Arc<AtomicBool>,
    /// Bytes read and written at a time: [`io::SAVE_CHUNK`]; smaller in
    /// tests, so a few bytes cross many chunk boundaries.
    chunk: usize,
}

impl std::fmt::Debug for SaveJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaveJob")
            .field("token", &self.token)
            .field("path", &self.path)
            .field("len", &self.snapshot.len_bytes())
            .finish_non_exhaustive()
    }
}

/// A finished temp file, not yet in use. Dropping it deletes the file, so
/// a result nobody takes leaves nothing behind.
#[derive(Debug)]
pub struct Written {
    /// The temp file, beside the target.
    pub temp: PathBuf,
    /// Open on the temp file for reading and writing; taken by
    /// [`Written::map`].
    file: Option<File>,
    /// Where the temp file goes: the path saved to, with a symlink
    /// followed so the link itself is kept.
    pub target: PathBuf,
    /// The temp file's size and modification time once written, which a
    /// rename keeps: the document's record of the file on disk, so the
    /// watcher sees the save as its own.
    pub meta: Metadata,
    /// Line index of the document's bytes as written (after any byte order
    /// mark), complete.
    index: LineIndex,
    /// The document's length in bytes.
    pub len: usize,
}

impl Drop for Written {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            drop(file);
            let _ = std::fs::remove_file(&self.temp);
        }
    }
}

impl Written {
    /// Map the temp file (leaving out a byte order mark if `bom`), with its
    /// finished line index. From here the mapping owns the temp file: it
    /// is deleted when the mapping is dropped, until
    /// [`MappedFile::keep_file`] says it was renamed into place.
    ///
    /// # Errors
    ///
    /// Returns a [`SaveError`] if the file cannot be mapped, or does not
    /// hold what was written (another program changed it), and deletes it.
    pub fn map(&mut self, bom: bool) -> Result<(Arc<MappedFile>, LineIndex), SaveError> {
        let Some(file) = self.file.take() else {
            return Err(step("mapping the new file")(std::io::Error::other(
                "already mapped",
            )));
        };
        let mapped = match MappedFile::new(file) {
            Ok(m) => m.delete_on_drop(self.temp.clone()),
            Err(e) => {
                let _ = std::fs::remove_file(&self.temp);
                return Err(step("mapping the new file")(e));
            }
        };
        let mapped = if bom {
            mapped.skip_prefix(io::BOM.len())
        } else {
            mapped
        };
        let index = std::mem::take(&mut self.index);
        let indexed = usize::try_from(index.indexed()).unwrap_or(usize::MAX);
        if mapped.len() != self.len || indexed != self.len || !index.is_done() {
            // Dropping `mapped` deletes the file.
            return Err(step("reading back the file")(std::io::Error::other(
                "the new file changed while it was being saved",
            )));
        }
        Ok((Arc::new(mapped), index))
    }
}

impl SaveJob {
    /// A job writing `snapshot` (preceded by a byte order mark if `bom`) to
    /// a temp file beside `path`.
    #[must_use]
    pub fn new(path: PathBuf, snapshot: Box<dyn Snapshot>, bom: bool) -> Self {
        Self {
            token: NEXT_TOKEN.fetch_add(1, Ordering::Relaxed),
            path,
            snapshot,
            bom,
            cancel: Arc::new(AtomicBool::new(false)),
            chunk: io::SAVE_CHUNK,
        }
    }

    /// Read and write `chunk` bytes at a time instead of
    /// [`io::SAVE_CHUNK`].
    #[cfg(test)]
    #[must_use]
    pub fn with_chunk(mut self, chunk: usize) -> Self {
        self.chunk = chunk.max(1);
        self
    }

    /// The flag that stops the worker early (at its next chunk), deleting
    /// the temp file: set when the document goes away.
    #[must_use]
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    /// Write the temp file, calling `progress` with the bytes written and
    /// the total every [`PROGRESS_EVERY`] bytes. Lets go of the snapshot
    /// before returning, so the old file is no longer read by this job.
    ///
    /// # Errors
    ///
    /// Returns a [`SaveError`] naming the step that failed; the temp file
    /// is deleted then, and the target was never touched.
    pub fn run(self, mut progress: impl FnMut(u64, u64)) -> Result<Written, SaveError> {
        let Self {
            path,
            snapshot,
            bom,
            cancel,
            chunk,
            ..
        } = self;
        // Write next to the real file, so the rename stays within one
        // directory (and one filesystem), and so a symlink is followed, not
        // overwritten.
        let target = std::fs::canonicalize(&path).unwrap_or(path);
        let (temp, mut file) = io::create_temp(&target)?;
        let total = snapshot.len_bytes();
        let written = write(
            &mut file,
            &target,
            &*snapshot,
            (bom, chunk),
            &cancel,
            &mut progress,
        );
        // The old file must not stay open on this thread's account: on
        // Windows the rename waits until nothing maps it.
        drop(snapshot);
        match written {
            Ok((index, meta)) => Ok(Written {
                temp,
                file: Some(file),
                target,
                meta,
                index,
                len: total,
            }),
            Err(e) => {
                drop(file);
                let _ = std::fs::remove_file(&temp);
                Err(e)
            }
        }
    }

    /// Run [`SaveJob::run`] on its own thread, reporting through `send`.
    ///
    /// # Errors
    ///
    /// Returns the error from starting the thread; the job is dropped then,
    /// having written nothing.
    pub fn spawn(self, send: impl Fn(SaveEvent) -> bool + Send + 'static) -> std::io::Result<()> {
        let token = self.token;
        std::thread::Builder::new()
            .name("nimbus-save".to_owned())
            .spawn(move || {
                let result = self.run(|written, total| {
                    let _ = send(SaveEvent::Progress {
                        token,
                        written,
                        total,
                    });
                });
                let _ = send(SaveEvent::Done {
                    token,
                    result: Box::new(result),
                });
            })
            .map(drop)
    }
}

/// Write `text` into `file` (a new temp file for `target`), indexing its
/// lines as it goes; then flush it to disk. Returns the finished index and
/// the file's metadata.
fn write(
    file: &mut File,
    target: &Path,
    text: &dyn Text,
    (bom, chunk): (bool, usize),
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(LineIndex, Metadata), SaveError> {
    use std::io::Write as _;
    io::copy_permissions(file, target)?;
    if bom {
        file.write_all(io::BOM.as_bytes())
            .map_err(step("writing the temporary file"))?;
    }
    let total = text.len_bytes();
    let to_u64 = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    let mut scanner = Scanner::default();
    let (mut written, mut reported) = (0, 0);
    io::write_text(file, text, chunk, |bytes| {
        // The index counts from after the byte order mark, as the mapping
        // of the new file will (`MappedFile::skip_prefix`).
        scanner.feed(bytes);
        written += bytes.len();
        if written - reported >= PROGRESS_EVERY {
            reported = written;
            progress(to_u64(written), to_u64(total));
        }
        !cancel.load(Ordering::Relaxed)
    })?;
    scanner.finish();
    let mut index = LineIndex::new();
    index
        .merge(scanner.take_chunk())
        .map_err(|e| step("counting lines")(std::io::Error::other(e)))?;
    // The bytes must be on disk before the rename makes them the file.
    file.sync_all().map_err(step("flushing to disk"))?;
    // Read from the file itself before it replaces the old one: a write by
    // anyone else after this point looks different. Renaming keeps both.
    let meta = file.metadata().map_err(step("reading back the file"))?;
    Ok((index, meta))
}

/// Drop `old`, the mapping of a file a save has just replaced, on a
/// short-lived thread if it is the last reference. Closing a replaced
/// (nameless) file for the last time makes the file system free its disk
/// space, which takes about 20 ms per GB on APFS: too long for the main
/// thread. If another reader still holds it, that reader's thread does
/// this instead, and if no thread can start, it happens here.
///
/// The temp file of an earlier save that could not replace its target is
/// dropped here, which deletes it at once: a rare case, and the next save
/// is then known to have cleaned up.
pub fn release_later(old: Arc<MappedFile>) {
    if Arc::strong_count(&old) > 1 || old.temp_path().is_some() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("nimbus-release".to_owned())
        .spawn(move || drop(old));
    if let Err(e) = spawned {
        log::debug!("released the old file on the main thread: {e}");
    }
}

/// The message for a save whose temp file could not replace the target.
#[must_use]
pub fn not_replaced(temp: &Path, target: &Path, why: &dyn std::fmt::Display) -> String {
    format!(
        "Saved to {} but could not replace {}: {why}",
        temp.display(),
        target.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use nimbus_core::mapped::Limits;
    use nimbus_core::{AnyBuffer, Buffer, MappedBuffer, movement};
    use std::cell::Cell;
    use std::time::Instant;

    /// A fresh directory for one test, removed on drop (after the
    /// documents declared later, which may hold mappings of its files).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("nimbus-save-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        /// Files in the directory other than `keep`.
        fn others(&self, keep: &str) -> Vec<String> {
            std::fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n != keep)
                .collect()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Tiny piece-table limits: deleting more than 16 bytes records pieces
    /// instead of bytes (`Content::Pieces`), and pieces stay short.
    const SMALL: Limits = Limits {
        copy_limit: 16,
        segment: 8,
        max_piece: 5,
        scan: 3,
        pseudo_line: 8,
    };

    /// All the document's raw bytes.
    fn raw(d: &Document) -> Vec<u8> {
        let mut out = Vec::new();
        d.buffer
            .read_into(0..d.buffer.len_bytes(), &mut out)
            .unwrap();
        out
    }

    /// Open `path`, which must open mapped, with `limits` instead of the
    /// default ones and only the first `indexed` bytes' lines counted.
    fn open_mapped(path: &Path, limits: Limits, indexed: usize) -> Document {
        let mut d = Document::open(path).unwrap();
        assert!(d.buffer.is_mapped(), "{} must open mapped", path.display());
        let source = d.backing_file().unwrap().clone() as Arc<dyn Source>;
        let mut scanner = Scanner::new(nimbus_core::line_index::Spacing::new(2, 5));
        let n = indexed.min(source.len());
        scanner.feed(&source.bytes()[..n]);
        if n == source.len() {
            scanner.finish();
        }
        let mut index = LineIndex::new();
        index.merge(scanner.take_chunk()).unwrap();
        d.buffer = AnyBuffer::from(MappedBuffer::with_index(source, index, limits).unwrap());
        d
    }

    /// Save `d` the way the app does, but on this thread: write, switch,
    /// then poll every millisecond while it waits.
    fn save_with(d: &mut Document, policy: ReplacePolicy, chunk: usize) -> SaveStep {
        let job = d.begin_save(policy).unwrap().with_chunk(chunk);
        let token = job.token;
        let result = job.run(|_, _| {});
        let mut step = d.save_written(token, result, Instant::now());
        while matches!(step, SaveStep::Waiting) {
            std::thread::sleep(Duration::from_millis(1));
            step = d.poll_replace(Instant::now());
        }
        step
    }

    fn save(d: &mut Document) -> SaveStep {
        save_with(d, ReplacePolicy::default(), 7)
    }

    /// The bytes before `pos` that are `\n`: the line `pos` is on.
    #[expect(
        clippy::naive_bytecount,
        reason = "the test's oracle: as plain as possible"
    )]
    fn line_of(bytes: &[u8], pos: usize) -> usize {
        bytes[..pos].iter().filter(|&&b| b == b'\n').count()
    }

    /// The document reads the file at `path` (not a temp file), is clean,
    /// has every line counted, and its line numbers match `expected`.
    fn check_saved(d: &Document, path: &Path, expected: &[u8], bom: bool) {
        let mut on_disk = Vec::new();
        if bom {
            on_disk.extend_from_slice(io::BOM.as_bytes());
        }
        on_disk.extend_from_slice(expected);
        assert_eq!(std::fs::read(path).unwrap(), on_disk, "the file");
        assert_eq!(raw(d), expected, "the document");
        assert!(!d.is_dirty() && !d.is_saving() && !d.conflict && !d.deleted);
        assert!(!d.history.can_undo() && !d.history.can_redo());
        assert!(d.buffer.is_fully_indexed());
        assert_eq!(d.backing_file().unwrap().temp_path(), None);
        let lines = line_of(expected, expected.len()) + 1;
        assert_eq!(d.buffer.len_lines(), lines);
        let mut pos = 0;
        while pos <= expected.len() {
            assert_eq!(
                d.buffer.byte_to_line(pos),
                line_of(expected, pos),
                "at {pos}"
            );
            assert_eq!(d.buffer.line_number(pos), Some(line_of(expected, pos)));
            pos += 1 + pos / 3;
        }
        for line in 0..lines {
            let start = d.buffer.line_to_byte(line);
            assert!(start == 0 || expected[start - 1] == b'\n', "line {line}");
            assert_eq!(line_of(expected, start), line);
        }
    }

    /// A small deterministic random number generator (xorshift), so the
    /// edits are the same on every run without a dependency.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            usize::try_from(self.0 % (n.max(1) as u64)).unwrap()
        }
    }

    /// Random edits: insertions (text with line breaks and non-ASCII
    /// characters), deletions short and long (past the copy limit, so undo
    /// holds pieces), and undo and redo, mirrored on a `Vec<u8>` oracle.
    fn edit_randomly(d: &mut Document, oracle: &mut Vec<u8>, rng: &mut Rng, steps: usize) {
        const TEXTS: [&str; 6] = ["x", "hello\n", "é", "\r\n", "日本語 ", "\tline\nline\n"];
        // The oracle after each undo step, to follow undo and redo.
        let mut undone: Vec<Vec<u8>> = Vec::new();
        let mut done: Vec<Vec<u8>> = vec![oracle.clone()];
        for _ in 0..steps {
            let len = d.buffer.len_bytes();
            let a = movement::clamp(&d.buffer, rng.below(len + 1));
            match rng.below(10) {
                0..=3 => {
                    let text = TEXTS[rng.below(TEXTS.len())];
                    d.replace(a..a, text).unwrap();
                    oracle.splice(a..a, text.bytes());
                }
                4..=6 => {
                    // Up to 60 bytes: often past the 16-byte copy limit.
                    let b = movement::clamp(&d.buffer, (a + rng.below(60)).min(len));
                    d.replace(a..b, "").unwrap();
                    oracle.drain(a..b);
                }
                7 => {
                    let b = movement::clamp(&d.buffer, (a + rng.below(30)).min(len));
                    d.replace(a..b, "+").unwrap();
                    oracle.splice(a..b, *b"+");
                }
                8 if done.len() > 1 => {
                    assert!(d.undo().unwrap());
                    undone.push(done.pop().unwrap());
                    oracle.clone_from(done.last().unwrap());
                    assert_eq!(&raw(d), oracle, "after undo");
                    continue;
                }
                9 if !undone.is_empty() => {
                    assert!(d.redo().unwrap());
                    done.push(undone.pop().unwrap());
                    oracle.clone_from(done.last().unwrap());
                    assert_eq!(&raw(d), oracle, "after redo");
                    continue;
                }
                _ => continue,
            }
            d.history.seal();
            undone.clear();
            done.push(oracle.clone());
        }
        assert_eq!(&raw(d), oracle);
    }

    #[test]
    fn saving_after_random_edits_writes_exactly_the_edited_bytes() {
        let dir = TempDir::new("random");
        // Invalid bytes make even a small file open mapped.
        let mut original = Vec::new();
        for i in 0..200 {
            original.extend_from_slice(format!("line {i} caf\u{e9} ").as_bytes());
            original.extend_from_slice(if i % 7 == 0 { b"\xff\xfe" } else { b"ok" });
            original.extend_from_slice(if i % 5 == 0 { b"\r\n" } else { b"\n" });
        }
        for (seed, bom) in [(1, false), (2, true), (3, false), (4, true)] {
            let name = format!("f{seed}.log");
            let path = dir.0.join(&name);
            let mut file = Vec::new();
            if bom {
                file.extend_from_slice(io::BOM.as_bytes());
            }
            file.extend_from_slice(&original);
            std::fs::write(&path, &file).unwrap();
            // Partly indexed, as a huge file is while its lines are counted.
            let mut d = open_mapped(&path, SMALL, 300);
            assert_eq!(d.bom, bom);
            let mut oracle = original.clone();
            let mut rng = Rng(seed * 7919);
            edit_randomly(&mut d, &mut oracle, &mut rng, 120);
            assert!(d.is_dirty());
            let step = save(&mut d);
            assert!(matches!(step, SaveStep::Saved), "{step:?}");
            check_saved(&d, &path, &oracle, bom);
            assert_eq!(dir.others(&name), Vec::<String>::new(), "no temp file left");

            // Edit and save again: now over the file the last save made,
            // which the document maps.
            edit_randomly(&mut d, &mut oracle, &mut rng, 60);
            let step = save(&mut d);
            assert!(matches!(step, SaveStep::Saved), "{step:?}");
            check_saved(&d, &path, &oracle, bom);
            std::fs::remove_file(&path).unwrap();
        }
    }

    #[test]
    fn saving_keeps_the_cursor_and_the_line_ending() {
        let dir = TempDir::new("cursor");
        let path = dir.0.join("crlf.txt");
        std::fs::write(&path, b"one\r\ntwo \xff\r\nthree\r\n").unwrap();
        let mut d = Document::open(&path).unwrap();
        d.click(6, false);
        d.insert("X").unwrap();
        d.select(2..9);
        assert!(matches!(save(&mut d), SaveStep::Saved));
        assert_eq!(d.selection().range(), 2..9);
        assert_eq!(d.line_ending, nimbus_core::LineEnding::CrLf);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"one\r\ntXwo \xff\r\nthree\r\n"
        );
        // Typing after a save starts a new, undoable step.
        d.click(0, false);
        d.insert("Y").unwrap();
        assert!(d.is_dirty());
        d.undo().unwrap();
        assert!(!d.is_dirty());
    }

    #[test]
    fn empty_documents_and_save_as_work_too() {
        let dir = TempDir::new("empty");
        let path = dir.0.join("bin");
        std::fs::write(&path, b"\xff").unwrap();
        let mut d = Document::open(&path).unwrap();
        d.select_all();
        d.delete_back().unwrap();
        assert!(matches!(save(&mut d), SaveStep::Saved));
        check_saved(&d, &path, b"", false);

        // Save As: the same steps, to a new name.
        d.insert("new \u{1F600}").unwrap();
        let other = dir.0.join("other.txt");
        d.path = Some(other.clone());
        assert!(matches!(save(&mut d), SaveStep::Saved));
        check_saved(&d, &other, "new \u{1F600}".as_bytes(), false);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"",
            "the old file is left alone"
        );
        assert_eq!(d.canonical, Some(io::canonical(&other)));
    }

    #[test]
    fn edits_wait_while_saving() {
        let dir = TempDir::new("busy");
        let path = dir.0.join("f");
        std::fs::write(&path, b"abc\xff").unwrap();
        let mut d = Document::open(&path).unwrap();
        let job = d.begin_save(ReplacePolicy::default()).unwrap();
        assert!(d.is_saving());
        assert!(
            d.begin_save(ReplacePolicy::default()).is_err(),
            "one at a time"
        );
        assert_eq!(d.save_token(), Some(job.token));
        let token = job.token;
        let step = d.save_written(token + 1000, job.run(|_, _| {}), Instant::now());
        assert!(matches!(step, SaveStep::Stale));
        assert_eq!(
            dir.others("f"),
            Vec::<String>::new(),
            "stale result deleted"
        );
        assert!(d.is_saving(), "still waiting for its own result");
    }

    #[test]
    fn a_failed_write_leaves_the_document_as_it_was() {
        let dir = TempDir::new("failed");
        let path = dir.0.join("f");
        std::fs::write(&path, b"abc\xff").unwrap();
        let mut d = Document::open(&path).unwrap();
        d.insert("x").unwrap();
        let job = d.begin_save(ReplacePolicy::default()).unwrap();
        let token = job.token;
        job.cancel_flag().store(true, Ordering::Relaxed);
        let step = d.save_written(token, job.run(|_, _| {}), Instant::now());
        assert!(matches!(step, SaveStep::Failed(_)), "{step:?}");
        assert!(!d.is_saving() && d.is_dirty() && d.history.can_undo());
        assert_eq!(raw(&d), b"xabc\xff");
        assert_eq!(std::fs::read(&path).unwrap(), b"abc\xff");
        assert_eq!(dir.others("f"), Vec::<String>::new(), "temp file deleted");
    }

    /// The app refuses edits while saving; if one slipped through, even
    /// one that keeps the length, the switch is refused rather than lose it.
    #[test]
    fn an_edit_during_the_save_keeps_the_document_off_the_new_file() {
        let dir = TempDir::new("raced");
        let path = dir.0.join("f");
        std::fs::write(&path, b"abc\xff").unwrap();
        let mut d = Document::open(&path).unwrap();
        let job = d.begin_save(ReplacePolicy::default()).unwrap();
        let token = job.token;
        let written = job.run(|_, _| {});
        d.replace(0..1, "A").unwrap();
        let step = d.save_written(token, written, Instant::now());
        assert!(matches!(step, SaveStep::Failed(_)), "{step:?}");
        assert_eq!(raw(&d), b"Abc\xff");
        assert!(d.is_dirty() && !d.is_saving());
        assert_eq!(std::fs::read(&path).unwrap(), b"abc\xff", "untouched");
        assert_eq!(dir.others("f"), Vec::<String>::new(), "temp file deleted");
    }

    fn refuse_rename(_: &Path, _: &Path) -> std::io::Result<()> {
        Err(std::io::Error::other("injected failure"))
    }

    #[test]
    fn a_failed_rename_leaves_the_document_on_the_temp_file() {
        let dir = TempDir::new("rename");
        let path = dir.0.join("app.log");
        std::fs::write(&path, b"old \xff\n").unwrap();
        let mut d = Document::open(&path).unwrap();
        d.insert("new ").unwrap();
        for unmap_first in [false, true] {
            let policy = ReplacePolicy {
                unmap_first,
                rename: refuse_rename,
                ..ReplacePolicy::default()
            };
            let step = save_with(&mut d, policy, 3);
            let SaveStep::NotReplaced(message) = step else {
                panic!("{step:?}");
            };
            assert!(message.starts_with("Saved to "), "{message}");
            assert!(message.contains("injected failure"), "{message}");
            assert_eq!(std::fs::read(&path).unwrap(), b"old \xff\n", "untouched");
            // The document reads the temp file, has its text, and is dirty.
            let temp = d.backing_file().unwrap().temp_path().unwrap().to_path_buf();
            assert!(message.contains(&temp.display().to_string()));
            assert_eq!(std::fs::read(&temp).unwrap(), b"new old \xff\n");
            assert_eq!(raw(&d), b"new old \xff\n");
            assert!(d.is_dirty() && !d.is_saving() && d.buffer.is_fully_indexed());
            assert_eq!(dir.others("app.log").len(), 1, "just the temp file");
        }

        // The next successful save replaces the target and deletes the
        // temp file it was reading.
        d.click(0, false);
        d.insert("!").unwrap();
        assert!(matches!(save(&mut d), SaveStep::Saved));
        check_saved(&d, &path, b"!new old \xff\n", false);
        assert_eq!(dir.others("app.log"), Vec::<String>::new());

        // Closing the document deletes it too.
        let step = save_with(
            &mut d,
            ReplacePolicy {
                rename: refuse_rename,
                ..ReplacePolicy::default()
            },
            64,
        );
        assert!(matches!(step, SaveStep::NotReplaced(_)));
        assert_eq!(dir.others("app.log").len(), 1);
        drop(d);
        assert_eq!(dir.others("app.log"), Vec::<String>::new());
    }

    thread_local! {
        /// Renames left to refuse in this test thread.
        static REFUSALS: Cell<usize> = const { Cell::new(0) };
    }

    /// Refuses while `REFUSALS` lasts, like Windows renaming a mapped file.
    fn refuse_then_rename(from: &Path, to: &Path) -> std::io::Result<()> {
        let left = REFUSALS.get();
        if left > 0 {
            REFUSALS.set(left - 1);
            return Err(std::io::Error::other("the file is mapped"));
        }
        std::fs::rename(from, to)
    }

    /// The Windows order, run here: wait for the old file to be released,
    /// then rename; if the mapped temp file cannot be renamed, unmap it,
    /// rename, and map it again.
    #[test]
    fn the_windows_order_waits_for_the_old_file_and_remaps_the_new_one() {
        let dir = TempDir::new("windows");
        let path = dir.0.join("w.log");
        let original = b"one\ntwo \xff\nthree\nfour five six seven eight\n";
        std::fs::write(&path, original).unwrap();
        let mut d = open_mapped(&path, SMALL, 4);
        d.replace(4..7, "TWO").unwrap();
        d.replace(0..20, "").unwrap(); // past the copy limit: undo holds pieces
        let expected = raw(&d);
        assert_eq!(expected, b" five six seven eight\n");
        let policy = ReplacePolicy {
            unmap_first: true,
            rename: refuse_then_rename,
            ..ReplacePolicy::default()
        };

        // A reader of the old file (a search) holds it: the save waits.
        let search = d.buffer.snapshot();
        let job = d.begin_save(policy).unwrap().with_chunk(5);
        let token = job.token;
        REFUSALS.set(1);
        let now = Instant::now();
        let step = d.save_written(token, job.run(|_, _| {}), now);
        assert!(matches!(step, SaveStep::Waiting), "{step:?}");
        assert!(d.is_saving(), "edits still wait");
        assert_eq!(raw(&d), expected, "already reading the new file");
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(matches!(d.poll_replace(now), SaveStep::Waiting));
        let mut old = Vec::new();
        search.read_into(0..search.len_bytes(), &mut old).unwrap();
        assert_eq!(old, expected, "the reader still reads its text");
        drop(search);

        // Released: the first rename is refused (the temp file is mapped),
        // so it is unmapped, renamed and mapped again.
        let step = d.poll_replace(now);
        assert!(matches!(step, SaveStep::Saved), "{step:?}");
        assert_eq!(REFUSALS.get(), 0);
        check_saved(&d, &path, &expected, false);
        assert_eq!(dir.others("w.log"), Vec::<String>::new());

        // A reader that never lets go: give up after the wait, on the temp.
        d.insert("more").unwrap();
        let expected = raw(&d);
        let search = d.buffer.snapshot();
        let step = save_with(
            &mut d,
            ReplacePolicy {
                wait: Duration::ZERO,
                ..policy
            },
            64,
        );
        let SaveStep::NotReplaced(message) = step else {
            panic!("{step:?}")
        };
        assert!(message.contains("in use"), "{message}");
        assert!(d.is_dirty());
        assert_eq!(raw(&d), expected);
        drop(search);
        assert!(matches!(save(&mut d), SaveStep::Saved));
        check_saved(&d, &path, &expected, false);
    }

    /// On Unix the rename happens at once, and a search reading the old
    /// file reads on: the replaced file lives on, nameless, until it is
    /// let go.
    #[cfg(unix)]
    #[test]
    fn a_search_snapshot_taken_before_the_save_reads_on_after_it() {
        let dir = TempDir::new("snapshot");
        let path = dir.0.join("big.log");
        let mut original = Vec::new();
        for i in 0..20_000 {
            // "µs" in Latin-1: not UTF-8, so the file opens mapped.
            original.extend_from_slice(format!("{i:08} INFO ").as_bytes());
            original.extend_from_slice(b"\xb5s request\n");
        }
        std::fs::write(&path, &original).unwrap();
        let mut d = Document::open(&path).unwrap();
        d.replace(0..9, "").unwrap();
        let before = raw(&d);
        let search = d.buffer.snapshot();
        assert!(matches!(save(&mut d), SaveStep::Saved));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        // Read through both paths a reader uses: the file and the mapping.
        let mut read = Vec::new();
        search.read_into(0..search.len_bytes(), &mut read).unwrap();
        assert_eq!(read, before);
        let line = search.line(1);
        assert!(
            line.starts_with("00000001 INFO ") && line.len() == 24,
            "{line}"
        );
        let (chunk, start) = search.chunk_at(100_000);
        assert_eq!(chunk, &before[start..start + chunk.len()]);
    }

    /// The milestone's check: the same substitution made by `sed` and by
    /// Nimbus's Replace All on a mapped file gives the same bytes.
    #[cfg(unix)]
    #[test]
    fn replace_all_and_save_match_sed() {
        let dir = TempDir::new("sed");
        let input = dir.0.join("in.log");
        let mut text = Vec::new();
        for i in 0..300_000 {
            let level = match i % 30 {
                7 => "ERROR",
                n if n % 2 == 0 => "INFO",
                _ => "WARN",
            };
            text.extend_from_slice(format!("2026-10-16 {i:06} {level} job ").as_bytes());
            // A Latin-1 byte: not UTF-8, so the file opens mapped, and the
            // save must keep it.
            text.extend_from_slice(if i % 11 == 0 { b"caf\xe9\n" } else { b"ok\n" });
        }
        assert!(text.len() > 2 * 4 * 1024 * 1024, "several save chunks");
        std::fs::write(&input, &text).unwrap();
        let expected = dir.0.join("sed.log");
        // No -i (it differs between BSD and GNU sed); the C locale makes
        // both treat the file as bytes.
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg("sed -e 's/ERROR/E!/g' \"$1\" > \"$2\"")
            .arg("sh")
            .arg(&input)
            .arg(&expected)
            .env("LC_ALL", "C")
            .status()
            .unwrap();
        assert!(status.success());

        let copy = dir.0.join("nimbus.log");
        std::fs::copy(&input, &copy).unwrap();
        let mut d = Document::open(&copy).unwrap();
        assert!(d.buffer.is_mapped() && !d.buffer.is_fully_indexed());
        let mut bar = crate::find::FindBar::default();
        bar.open(true, None);
        bar.find.insert("ERROR").unwrap();
        bar.replace.insert("E!").unwrap();
        bar.search_now(&d);
        assert_eq!(bar.matches.len(), 10_000);
        let items: Vec<_> = bar
            .matches
            .iter()
            .map(|r| (r.clone(), bar.replacement_for(&d, r.clone()).unwrap()))
            .collect();
        assert_eq!(d.replace_many(&items).unwrap(), 10_000);
        let step = save_with(&mut d, ReplacePolicy::default(), io::SAVE_CHUNK);
        assert!(matches!(step, SaveStep::Saved), "{step:?}");
        let (ours, sed) = (
            std::fs::read(&copy).unwrap(),
            std::fs::read(&expected).unwrap(),
        );
        assert!(ours == sed, "Nimbus and sed differ");
        assert!(d.buffer.is_fully_indexed() && !d.is_dirty());
        assert_eq!(d.buffer.len_lines(), 300_001);
        let last = ours[..ours.len() - 1]
            .iter()
            .rposition(|&b| b == b'\n')
            .unwrap();
        assert_eq!(d.buffer.line_to_byte(299_999), last + 1);
    }

    /// Save a copy of a huge file after one edit, timing it. Run with
    /// `NIMBUS_SAVE_BENCH=/path/to/copy.log cargo test --release -p nimbus
    /// saves_a_huge_file -- --ignored --nocapture`, under `/usr/bin/time -l`
    /// for peak memory. The file is overwritten.
    #[test]
    #[ignore = "benchmark; needs NIMBUS_SAVE_BENCH"]
    fn saves_a_huge_file() {
        let Some(path) = std::env::var_os("NIMBUS_SAVE_BENCH").map(PathBuf::from) else {
            return;
        };
        let started = Instant::now();
        let mut d = Document::open(&path).unwrap();
        println!(
            "opened {} bytes in {:?}",
            d.buffer.len_bytes(),
            started.elapsed()
        );
        let mid = movement::clamp(&d.buffer, d.buffer.len_bytes() / 2);
        let line = d.buffer.line_span(mid).start;
        d.replace(line..line, "edited by nimbus\n").unwrap();
        let job = d.begin_save(ReplacePolicy::default()).unwrap();
        let token = job.token;
        let started = Instant::now();
        let result = job.run(|written, total| {
            println!("  {}%", written * 100 / total.max(1));
        });
        let wrote = started.elapsed();
        let step = d.save_written(token, result, Instant::now());
        let all = started.elapsed();
        assert!(matches!(step, SaveStep::Saved), "{step:?}");
        println!(
            "saved {} bytes: written, indexed and flushed in {wrote:?}; switched and renamed by {all:?}",
            d.buffer.len_bytes()
        );
        assert!(d.buffer.is_fully_indexed());
        println!("{} lines", d.buffer.len_lines());
    }
}
