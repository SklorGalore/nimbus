//! One open file: its text, where it lives on disk, the selection, its undo
//! history, and whether it has unsaved changes.

use std::borrow::Cow;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime};

use nimbus_core::live_diff::EditSpan;
use nimbus_core::search::{self, Literal};
use nimbus_core::{
    AnyBuffer, Bias, Buffer, Edit, EditError, History, LineEnding, MappedBuffer, Selection,
    Selections, Snapshot, Source, Text, report_sorted,
};
use nimbus_core::{long_line, movement, multi};

use nimbus_syntax::state::SyntaxState;

use crate::indexer::Indexing;
use crate::input::Motion;
use crate::io::{self, OpenError, SaveError};
use crate::languages::{Detected, Languages};
use crate::mapping::MappedFile;
use crate::saver::{self, ReplacePolicy, SaveJob, SaveStep, Written};
use nimbus_core::line_index::{IndexChunk, LineIndex};
use nimbus_core::mapped::Limits;

/// Copy and Cut refuse selections longer than this: 64 MiB. The text is
/// copied into memory, then again by the system clipboard, and Select All
/// in a 4 GB file must not try.
pub const MAX_COPY_BYTES: usize = 64 * 1024 * 1024;

/// What identifies a version of a file on disk without reading it: its
/// size and modification time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    fn of(path: &Path) -> Option<Self> {
        std::fs::metadata(path).ok().map(|m| Self::from(&m))
    }

    fn from(meta: &std::fs::Metadata) -> Self {
        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
        }
    }
}

/// What a change on disk did to a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskChange {
    /// Nothing: our own save, or the same file.
    None,
    /// Reloaded: there were no unsaved edits.
    Reloaded,
    /// Changed on disk, and there are unsaved edits here.
    Conflict,
    /// Changed on disk, but a mapped document does not reload by itself
    /// (a 4 GB log being appended to would reload every second). It is
    /// marked [`Document::conflict`] so the tab says so.
    NotReloaded,
    /// A mapped document's file got shorter than its mapping. Reading the
    /// lost part through the mapping crashes Nimbus (`SIGBUS`, see
    /// `crate::mapping`), so a document without unsaved edits is reloaded
    /// (`reloaded`); one with edits is marked as a conflict and must be
    /// reloaded by the user.
    Truncated {
        /// Whether it was reloaded.
        reloaded: bool,
    },
    /// Deleted on disk.
    Deleted,
    /// Changed on disk, but reading it failed.
    Failed(String),
}

/// Most lines a column selection covers ([`Document::column_select`]).
pub const MAX_COLUMN_LINES: usize = 10_000;

/// Most bytes [`Document::add_next_occurrence`] reads per press.
pub const NEXT_BUDGET: usize = 32 * 1024 * 1024;

/// Longest selection [`Document::add_next_occurrence`] looks for.
const MAX_NEEDLE: usize = 64 * 1024;

/// What [`Document::add_next_occurrence`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occurrence {
    /// The cursor was on a word, which is now selected.
    SelectedWord,
    /// The next occurrence was added as a cursor.
    Added,
    /// Every occurrence is already selected.
    AllSelected,
    /// The selection's text occurs nowhere else.
    NotFound,
    /// The cursor is not on a word.
    NoWord,
    /// The selection is too long to look for.
    TooLong,
    /// None within [`NEXT_BUDGET`] bytes; pressing again goes on from
    /// there.
    GaveUp,
}

/// Lines touched by the byte range `r`. A range ending at the very start of
/// a line does not include that line.
fn lines_of(t: &dyn Text, r: Range<usize>) -> Range<usize> {
    let first = t.byte_to_line(r.start);
    let mut last = t.byte_to_line(r.end);
    if last > first && t.line_to_byte(last) == r.end {
        last -= 1;
    }
    first..last + 1
}

/// Reads a file for a document: [`io::open`], or in tests another way.
#[cfg(test)]
type Opener = fn(&Path) -> Result<io::Opened, OpenError>;

/// An open document.
#[derive(Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts about the document, not a state machine"
)]
pub struct Document {
    /// The text: a rope, or a piece table over a mapped file (see
    /// [`io::Backend`]).
    pub buffer: AnyBuffer,
    /// The file mapping behind a mapped buffer, which knows the file's
    /// length when it was mapped; `None` for a rope or a piece table held
    /// in memory.
    mapping: Option<Arc<MappedFile>>,
    /// How tests opened the file ([`Document::open_mapped`]), so reloads
    /// open it the same way; `None` for [`io::open`].
    #[cfg(test)]
    opener: Option<Opener>,
    /// Where it saves to; `None` for an untitled buffer.
    pub path: Option<PathBuf>,
    /// Inserted by Enter, detected when the file was opened.
    pub line_ending: LineEnding,
    /// Cursor and selection.
    pub selections: Selections,
    /// Display column that up and down movement aims for, kept across
    /// shorter lines; `None` until the first vertical move.
    pub preferred_column: Option<usize>,
    /// With several cursors, the column each aims for in the same way, in
    /// selection order; empty until the first vertical move, and cleared
    /// when anything else moves or merges cursors.
    goals: Vec<Option<usize>>,
    /// Where [`Document::add_next_occurrence`] gave up last time, for the
    /// next press to go on from: the text's version, the primary it was
    /// looking from, and the offset reached.
    next_resume: Option<(u64, Selection, usize)>,
    /// The file had a UTF-8 byte order mark, written back on save.
    pub bom: bool,
    /// Increments on every change to the text. Background work (search,
    /// parsing) records the version it read, so stale results can be
    /// recognised and dropped.
    pub version: u64,
    /// Undo and redo.
    pub history: History,
    /// The syntax tree for highlighting, kept in step with every edit.
    pub syntax: SyntaxState,
    /// Key of the detected language (`Detected::key`), if any.
    language: Option<&'static str>,
    /// Its name for the status bar.
    language_name: Option<&'static str>,
    /// The file on disk as last read or written; a change event that still
    /// matches it is our own save.
    disk: Option<Stamp>,
    /// The file was deleted on disk; saving recreates it.
    pub deleted: bool,
    /// The file changed on disk while there were unsaved edits here.
    pub conflict: bool,
    /// `path` resolved (`io::canonical`), as file watching reports it;
    /// worked out at open and save, not per use.
    pub canonical: Option<PathBuf>,
    /// When the file was last reloaded because it changed on disk.
    pub reloaded_at: Option<Instant>,
    /// [`History::top_id`] when last saved or opened. The document is dirty
    /// whenever the top of the history differs, so undoing back to the saved
    /// point makes it clean again.
    saved_at: u64,
    /// The rest of a mapped file's line counting, until the app starts it
    /// on a worker.
    pub indexing: Option<Indexing>,
    /// Which indexing run this document's index chunks come from.
    pub index_token: Option<u64>,
    /// Stops the indexing worker when the document goes away or reloads.
    index_cancel: Option<CancelOnDrop>,
    /// A save of this mapped document under way ([`crate::saver`]). Edits
    /// wait while it is set.
    saving: Option<Saving>,
    /// Edits since a compare view (M9) last looked, so it can follow them.
    edits: EditLog,
    /// The text as the file held it when last opened, reloaded or saved,
    /// for merging with a changed file (M9 task 10). A rope snapshot
    /// shares storage with the buffer until edits part them, so this costs
    /// little; mapped documents keep none.
    disk_text: DiskText,
    /// A stand-in for a restored tab whose file has not been read yet
    /// ([`Document::placeholder`]).
    placeholder: bool,
}

/// A snapshot of the text as the file held it, or none; a wrapper only
/// because a snapshot has no `Debug`.
#[derive(Default, Clone)]
struct DiskText(Option<Arc<dyn Snapshot>>);

impl std::fmt::Debug for DiskText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Some(t) => write!(f, "DiskText({} bytes)", t.len_bytes()),
            None => f.write_str("DiskText(none)"),
        }
    }
}

/// One edit an observer follows: its span and the text right after it.
pub type NotedEdit = (EditSpan, Arc<dyn Snapshot>);

/// Edits noted for an observer, such as the compare view's live diff,
/// which needs every change in order, undo and redo included. Off until
/// something asks; drained each frame while on.
#[derive(Default)]
pub struct EditLog {
    on: bool,
    /// One entry per edit, in application order, with the text as it was
    /// right after that edit (a rope snapshot shares storage, so this is
    /// cheap): an observer following several edits from one frame needs
    /// each one's own text, as spans name lines of the text they were
    /// made in. `None` for an edit whose span could not be worked out
    /// (the observer then starts over).
    spans: Vec<Option<NotedEdit>>,
    /// The text was replaced wholesale (a reload), with no edit records.
    reset: bool,
}

impl std::fmt::Debug for EditLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The snapshots are whole texts; show what the log holds, not them.
        f.debug_struct("EditLog")
            .field("on", &self.on)
            .field("spans", &self.spans.len())
            .field("reset", &self.reset)
            .finish()
    }
}

impl EditLog {
    fn note(&mut self, buffer: &dyn Buffer, edit: &Edit) {
        if self.on {
            let entry = EditSpan::after(buffer, edit).map(|span| (span, buffer.snapshot().into()));
            self.spans.push(entry);
        }
    }
}

/// Stops a worker when dropped: when its document closes, reloads or goes
/// away.
#[derive(Debug)]
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// `Document::saved_at` while the document's text is not in its file:
/// never equal to a history id, so the document stays dirty.
const NOT_SAVED: u64 = u64::MAX;

/// The error for saving a document that has no file name.
fn untitled() -> SaveError {
    SaveError {
        step: "choosing a path",
        source: std::io::Error::other("the document has no file name yet"),
    }
}

/// A save of a mapped document under way ([`crate::saver`]).
#[derive(Debug)]
struct Saving {
    /// The save's [`SaveJob::token`]: its results carry it.
    token: u64,
    /// [`Document::version`] when the save began. Every edit changes it, so
    /// the document can tell that the file written is still its text.
    version: u64,
    policy: ReplacePolicy,
    /// Whether the target is the file the document reads, so the old
    /// mapping must be let go before the rename on Windows. Not after a
    /// Save As to another file, nor for a document still reading the temp
    /// file of an earlier save.
    over_mapped: bool,
    /// Stops the worker if the document goes away first.
    _cancel: CancelOnDrop,
    /// Set once the new file is written and the document reads it, until
    /// it has replaced the target.
    replace: Option<Replace>,
}

/// A written save waiting to replace its target (see [`crate::saver`]).
#[derive(Debug)]
struct Replace {
    /// The file the document read before the save. Dropped before the
    /// rename; on Windows only once this is its last reference.
    old: Option<Arc<MappedFile>>,
    /// The new file, which the document now reads; it deletes itself when
    /// dropped until it is renamed into place.
    new: Arc<MappedFile>,
    temp: PathBuf,
    target: PathBuf,
    /// The new file's size and time, which the rename keeps.
    meta: std::fs::Metadata,
    /// Stop waiting for `old` to be released at this time.
    until: Instant,
}

impl Document {
    /// Whether this document's lines are still being counted: a go-to-line
    /// past the counted part waits for them. False once counting is done,
    /// or stopped because the file could not be read.
    #[must_use]
    pub fn counting_lines(&self) -> bool {
        self.index_token.is_some() && !self.buffer.is_fully_indexed()
    }

    /// Take on the line count still to run for this document's file,
    /// stopping any earlier one. Its token is what [`Document::merge_index`]
    /// accepts chunks for, and dropping the cancel guard (on close, reload
    /// or another call here) stops its worker.
    fn adopt_indexing(&mut self, indexing: Option<Indexing>) {
        self.stop_indexing();
        self.index_token = indexing.as_ref().map(|i| i.token);
        self.index_cancel = indexing
            .as_ref()
            .map(|i| CancelOnDrop(Arc::clone(&i.cancel)));
        self.indexing = indexing;
    }

    /// Stop a running line count for this document.
    pub fn stop_indexing(&mut self) {
        // Dropping the guard stops the worker.
        self.index_cancel = None;
        self.indexing = None;
        self.index_token = None;
    }

    /// Add a chunk of the line index from the worker. Returns `false` if it
    /// does not belong here (another run, or out of order).
    pub fn merge_index(&mut self, token: u64, chunk: IndexChunk) -> bool {
        if self.index_token != Some(token) {
            return false;
        }
        let Some(mapped) = self.buffer.as_mapped_mut() else {
            return false;
        };
        match mapped.merge_index(chunk) {
            Ok(()) => {
                // Line numbers changed under the highlighter's checkpoints.
                self.syntax.touch();
                true
            }
            Err(e) => {
                log::warn!("line index chunk refused: {e}");
                false
            }
        }
    }

    /// An empty untitled document.
    #[must_use]
    pub fn untitled() -> Self {
        Self::default()
    }

    /// A stand-in for `path` in a restored tab that has not been shown yet:
    /// the name for the tab bar and nothing else, no text read and no
    /// syntax state. Restoring ten tabs then costs one file's work before
    /// the first frame; the app swaps in the real document when the tab is
    /// first shown, or when a worker has read it.
    ///
    /// It has no [`Document::canonical`] path, so everything that finds
    /// open files by path (file watching, find and replace in files, the
    /// tree's marker) treats the file as not open and reads the disk. It is
    /// never dirty, so saving all and the quit prompt pass it by.
    #[must_use]
    pub fn placeholder(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            placeholder: true,
            ..Self::default()
        }
    }

    /// Whether this is a [`Document::placeholder`].
    #[must_use]
    pub fn is_placeholder(&self) -> bool {
        self.placeholder
    }

    /// An untitled document holding `text`, unmodified.
    #[cfg(test)]
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Self::with_text(None, text)
    }

    /// A document holding `text` that saves to `path` (none: untitled), as
    /// a merge result starts (M9): not yet on disk, so saving writes it
    /// whole, and dirty from the start when it has a path, since the file
    /// does not hold this text yet.
    #[must_use]
    pub fn with_text(path: Option<PathBuf>, text: &str) -> Self {
        let mut doc = Self {
            buffer: nimbus_core::RopeBuffer::from_text(text).into(),
            canonical: path.as_deref().map(io::canonical),
            path,
            ..Self::default()
        };
        if doc.path.is_some() {
            // Not what the file holds: `saved_at` points at no history
            // entry, so the tab shows dirty until the first save.
            doc.saved_at = NOT_SAVED;
        }
        doc
    }

    /// Open `path`. A path that does not exist yet gives an empty document
    /// that will be created on first save.
    ///
    /// # Errors
    ///
    /// Returns an [`OpenError`] for anything other than a missing file.
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        Self::open_by(path, io::open)
    }

    /// Open `path` memory-mapped whatever its size, for tests of mapped
    /// documents.
    #[cfg(test)]
    pub fn open_mapped(path: &Path) -> Result<Self, OpenError> {
        let opener: Opener = |p| io::open_with(p, 0, None);
        let mut d = Self::open_by(path, opener)?;
        d.opener = Some(opener);
        Ok(d)
    }

    /// [`Document::open`], reading the file with `open`.
    fn open_by(
        path: &Path,
        open: impl FnOnce(&Path) -> Result<io::Opened, OpenError>,
    ) -> Result<Self, OpenError> {
        // The stamp is taken before reading: a write landing between the
        // two then looks like a change, never like the version we read.
        let disk = Stamp::of(path);
        let opened = match open(path) {
            Ok(opened) => opened,
            Err(OpenError::NotFound) => io::Opened {
                buffer: AnyBuffer::default(),
                line_ending: LineEnding::default(),
                bom: false,
                mapping: None,
                indexing: None,
            },
            Err(e) => return Err(e),
        };
        let mut doc = Self {
            buffer: opened.buffer,
            mapping: opened.mapping,
            path: Some(path.to_path_buf()),
            line_ending: opened.line_ending,
            bom: opened.bom,
            disk,
            canonical: Some(io::canonical(path)),
            ..Self::default()
        };
        doc.adopt_indexing(opened.indexing);
        doc.remember_disk_text();
        Ok(doc)
    }

    /// Replace the whole text with `text`, as a merge result arriving from
    /// its worker does: a fresh rope, no undo history, the language to be
    /// detected again by the caller (so the syntax state sees the new
    /// text), and dirty when the document has a path.
    pub fn set_text(&mut self, text: &str) {
        self.buffer = nimbus_core::RopeBuffer::from_text(text).into();
        self.history = History::default();
        self.selections = Selections::default();
        self.preferred_column = None;
        self.goals.clear();
        self.language = None;
        self.saved_at = if self.path.is_some() {
            NOT_SAVED
        } else {
            self.history.top_id()
        };
        self.edits.reset = true;
        self.version += 1;
    }

    /// Keep a snapshot of the text as the file holds it now (rope documents
    /// only; a mapped file's bytes are the file itself).
    fn remember_disk_text(&mut self) {
        self.disk_text =
            DiskText((!self.buffer.is_mapped()).then(|| self.buffer.snapshot().into()));
    }

    /// The text as the file held it when last opened, reloaded or saved,
    /// when known: the base for merging edits with a changed file.
    #[must_use]
    pub fn disk_text(&self) -> Option<&Arc<dyn Snapshot>> {
        self.disk_text.0.as_ref()
    }

    /// The file changed on disk (a watcher said so): work out what to do.
    /// Reloads by itself when nothing here is unsaved, except a mapped
    /// document, which reloads by itself only when its file got shorter.
    ///
    /// While a save is under way nothing happens: the save replaces the
    /// file and records it, and the app looks again once it ends.
    pub fn disk_changed(&mut self, languages: &Languages) -> DiskChange {
        let Some(path) = self.path.clone() else {
            return DiskChange::None;
        };
        if self.saving.is_some() {
            return DiskChange::None;
        }
        let now = Stamp::of(&path);
        let Some(stamp) = now else {
            if self.disk.is_none() {
                return DiskChange::None; // never existed: an unsaved new file
            }
            self.deleted = true;
            self.disk = None;
            return DiskChange::Deleted;
        };
        if self.disk == Some(stamp) {
            return DiskChange::None; // our own save, or no real change
        }
        self.deleted = false;
        if let Some(mapping) = &self.mapping {
            // A document still reading the temp file of a save that could
            // not replace this file is not reading this file: cutting it
            // short takes nothing from under the document.
            let truncated = mapping.temp_path().is_none() && stamp.len < mapping.file_len();
            return self.mapped_disk_changed(truncated, languages);
        }
        if self.is_dirty() {
            self.conflict = true;
            return DiskChange::Conflict;
        }
        match self.reload(languages) {
            Ok(()) => DiskChange::Reloaded,
            Err(e) => DiskChange::Failed(e.to_string()),
        }
    }

    /// [`Document::disk_changed`] for a mapped document, whose file is
    /// `truncated` if it is now shorter than its mapping.
    fn mapped_disk_changed(&mut self, truncated: bool, languages: &Languages) -> DiskChange {
        if truncated {
            // Part of the mapping is gone. Re-mapping is the safe response,
            // and loses nothing when there are no unsaved edits.
            if !self.is_dirty() {
                match self.reload(languages) {
                    Ok(()) => return DiskChange::Truncated { reloaded: true },
                    Err(e) => log::warn!("could not reload a truncated file: {e}"),
                }
            }
            self.conflict = true;
            return DiskChange::Truncated { reloaded: false };
        }
        if self.conflict {
            // Already said, and a growing log changes every second.
            return DiskChange::None;
        }
        self.conflict = true;
        if self.is_dirty() {
            DiskChange::Conflict
        } else {
            DiskChange::NotReloaded
        }
    }

    /// Replace the text with the file on disk, discarding unsaved edits
    /// and the undo history. The cursor stays on the same line and column
    /// where they still exist (in a mapped document, at the same byte
    /// offset). The new text may open in the other backend.
    ///
    /// # Errors
    ///
    /// Returns the [`OpenError`] if the file cannot be read, or while the
    /// document is being saved; nothing changes then.
    pub fn reload(&mut self, languages: &Languages) -> Result<(), OpenError> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        if self.saving.is_some() {
            return Err(OpenError::Io(std::io::Error::other(
                "it is being saved; reload once that is done",
            )));
        }
        let disk = Stamp::of(&path);
        #[cfg(test)]
        let opened = self.opener.unwrap_or(io::open)(&path)?;
        #[cfg(not(test))]
        let opened = io::open(&path)?;
        // Where the cursor was, to put it back. A rope keeps its line and
        // column. A mapped document keeps the byte offset instead: its file
        // may have been cut short, and working out the line could read the
        // old mapping where the bytes are gone, which crashes.
        let head = self.selections.primary().head;
        let place = (!self.buffer.is_mapped()).then(|| {
            let head = self.cursor();
            let line = self.buffer.byte_to_line(head);
            (line, head - self.buffer.line_to_byte(line))
        });
        self.adopt_indexing(opened.indexing);
        self.buffer = opened.buffer;
        self.mapping = opened.mapping;
        self.line_ending = opened.line_ending;
        self.bom = opened.bom;
        self.disk = disk;
        self.reloaded_at = Some(Instant::now());
        self.history = History::default();
        self.saved_at = self.history.top_id();
        self.deleted = false;
        self.conflict = false;
        self.edits.reset = true;
        self.version += 1;
        self.remember_disk_text();
        let pos = match place {
            Some((line, col)) => {
                let line = line.min(self.buffer.len_lines().saturating_sub(1));
                let start = self.buffer.line_to_byte(line);
                let len = self.buffer.line_span(start).end - start;
                start + col.min(len)
            }
            None => head.min(self.buffer.len_bytes()),
        };
        self.set_selection(Selection::cursor(movement::clamp(&self.buffer, pos)));
        self.preferred_column = None;
        // The text is new, so the tree is too, even in the same language.
        self.language = None;
        self.detect_language(languages);
        Ok(())
    }

    /// Keep the edits here over a change on disk: the next save overwrites
    /// it, and only a newer change asks again.
    pub fn keep_mine(&mut self) {
        if let Some(path) = &self.path {
            self.disk = Stamp::of(path);
        }
        self.conflict = false;
    }

    /// Pick the language (see [`Languages::detect`]) and start its syntax
    /// state over. Keeps the state when the language is unchanged.
    pub fn detect_language(&mut self, languages: &Languages) {
        // Only a shebang needs the text, and only its start: never copy a
        // whole (possibly huge) first line.
        let head = self.buffer.slice(0..self.buffer.len_bytes().min(256));
        let detected = languages.detect(self.path.as_deref(), head.lines().next().unwrap_or(""));
        drop(head);
        let key = detected.map(Detected::key);
        if key == self.language && self.syntax.off_reason().is_none() {
            return;
        }
        self.language = key;
        self.language_name = detected.map(|d| match d {
            Detected::Builtin(id) => id.name(),
            Detected::Simple(name, _) => name,
        });
        match detected {
            // A mapped file is not in memory for tree-sitter to parse, so
            // it gets the simple rules at any size, even when small.
            Some(Detected::Builtin(id)) if self.buffer.is_mapped() => {
                self.syntax.reset_lexical(id);
            }
            Some(Detected::Builtin(id)) => self.syntax.reset(Some(id), self.buffer.len_bytes()),
            Some(Detected::Simple(_, rules)) => self.syntax.reset_simple(rules),
            None => self.syntax.reset(None, self.buffer.len_bytes()),
        }
    }

    /// The detected language's name, for the status bar.
    #[must_use]
    pub fn language_name(&self) -> Option<&'static str> {
        self.language_name
    }

    /// The encoding, as far as it is known, for the status bar. A rope
    /// holds only UTF-8. A piece table held in memory holds a small file
    /// that is not UTF-8. A huge mapped file's bytes are never checked as
    /// a whole, so nothing is claimed for it.
    #[must_use]
    pub fn encoding_label(&self) -> Option<&'static str> {
        match (self.buffer.is_mapped(), self.mapping.is_some(), self.bom) {
            (false, _, false) => Some("UTF-8"),
            (false, _, true) => Some("UTF-8 BOM"),
            (true, false, _) => Some("Not UTF-8"),
            (true, true, true) => Some("BOM"),
            (true, true, false) => None,
        }
    }

    /// A separate document holding a copy of this one's text, for the
    /// compare view's read-only side when comparing two tabs. `None` for
    /// a text too large to copy (over the diff limit) or held as pieces of
    /// a mapped file; the caller compares with the file instead.
    #[must_use]
    pub fn copy_of(&self) -> Option<Self> {
        if self.buffer.len_bytes() > nimbus_core::DIFF_LIMIT || self.buffer.is_mapped() {
            return None;
        }
        let text = self.buffer.slice(0..self.buffer.len_bytes());
        Some(Self {
            buffer: nimbus_core::RopeBuffer::from_text(&text).into(),
            path: self.path.clone(),
            line_ending: self.line_ending,
            ..Self::default()
        })
    }

    /// Start or stop noting edits for an observer ([`Document::take_edits`]).
    /// Starting clears anything noted before.
    pub fn watch_edits(&mut self, on: bool) {
        self.edits.on = on;
        self.edits.spans.clear();
        self.edits.reset = false;
    }

    /// The edits noted since the last call, in order, each with the text
    /// right after it, and whether the text was replaced wholesale in
    /// between (then the spans are moot).
    pub fn take_edits(&mut self) -> (Vec<Option<NotedEdit>>, bool) {
        let reset = std::mem::take(&mut self.edits.reset);
        (std::mem::take(&mut self.edits.spans), reset)
    }

    /// Whether there are unsaved changes.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.history.top_id() != self.saved_at
    }

    /// Short name for the window title: file name, or "untitled".
    #[must_use]
    pub fn display_name(&self) -> String {
        self.path.as_ref().and_then(|p| p.file_name()).map_or_else(
            || "untitled".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    /// Save to the document's path, on this thread. Only for a document in
    /// memory; a mapped one saves on a worker ([`Document::begin_save`]).
    ///
    /// # Errors
    ///
    /// Returns a [`SaveError`] if writing fails, if the document is
    /// untitled (the app asks for a name first), or if it is mapped. The
    /// document stays dirty.
    pub fn save(&mut self) -> Result<(), SaveError> {
        let Some(path) = self.path.clone() else {
            return Err(untitled());
        };
        if self.saves_on_worker() {
            // Saving over the mapped file needs the steps in crate::saver;
            // nothing is written here, so the file cannot be damaged.
            return Err(SaveError {
                step: "saving a memory-mapped file",
                source: std::io::Error::other("it saves on a worker; nothing was written"),
            });
        }
        let written = io::save(&path, &self.buffer, self.bom)?;
        self.mark_saved(&written);
        Ok(())
    }

    /// Record that the text is now the file at `path`, whose size and time
    /// are `meta`: the watcher then sees the save as our own.
    fn mark_saved(&mut self, meta: &std::fs::Metadata) {
        self.disk = Some(Stamp::from(meta));
        self.canonical = self.path.as_deref().map(io::canonical);
        self.deleted = false;
        self.conflict = false;
        // Typing after a save must start a new undo step, or it would join
        // the saved step and the document would look clean.
        self.history.seal();
        self.saved_at = self.history.top_id();
        self.remember_disk_text();
    }

    /// Whether saving goes through [`Document::begin_save`] on a worker:
    /// the document reads a memory-mapped file. A piece table held in
    /// memory (a small file that is not UTF-8) saves at once, like a rope.
    #[must_use]
    pub fn saves_on_worker(&self) -> bool {
        self.mapping.is_some()
    }

    /// Whether a save is under way: edits wait until it ends.
    #[must_use]
    pub fn is_saving(&self) -> bool {
        self.saving.is_some()
    }

    /// The save under way, by its [`SaveJob::token`].
    #[must_use]
    pub fn save_token(&self) -> Option<u64> {
        self.saving.as_ref().map(|s| s.token)
    }

    /// Whether a written save is waiting to replace its target (Windows:
    /// for the old file to be released); see [`Document::poll_replace`].
    #[must_use]
    pub fn is_replacing(&self) -> bool {
        self.saving.as_ref().is_some_and(|s| s.replace.is_some())
    }

    /// The file a mapped document reads: the one it was opened from or
    /// saved to, or the temp file of a save that could not replace it.
    #[cfg(test)]
    #[must_use]
    pub fn backing_file(&self) -> Option<&Arc<MappedFile>> {
        self.mapping.as_ref()
    }

    /// Start saving a mapped document to its path. Returns the job that
    /// writes the new file, to run on a worker ([`SaveJob::spawn`]); its
    /// result goes to [`Document::save_written`]. Edits are refused (by the
    /// app, which checks [`Document::is_saving`]) until the save ends.
    ///
    /// # Errors
    ///
    /// Returns a [`SaveError`] if the document is untitled, not mapped, or
    /// already being saved.
    pub fn begin_save(&mut self, policy: ReplacePolicy) -> Result<SaveJob, SaveError> {
        let Some(path) = self.path.clone() else {
            return Err(untitled());
        };
        let refuse = |why: &str| SaveError {
            step: "starting the save",
            source: std::io::Error::other(why),
        };
        if self.saving.is_some() {
            return Err(refuse("it is already being saved"));
        }
        if !self.saves_on_worker() {
            return Err(refuse("only mapped documents save on a worker"));
        }
        // No undo step may stay open to absorb edits made after the save.
        self.history.seal();
        let over_mapped = self
            .mapping
            .as_ref()
            .is_some_and(|m| m.temp_path().is_none())
            && self.canonical.as_deref() == Some(io::canonical(&path).as_path());
        let job = SaveJob::new(path, self.buffer.snapshot(), self.bom);
        self.saving = Some(Saving {
            token: job.token,
            version: self.version,
            policy,
            over_mapped,
            _cancel: CancelOnDrop(job.cancel_flag()),
            replace: None,
        });
        Ok(job)
    }

    /// The worker of save `token` finished: on success, switch the document
    /// to the new file and replace the target with it (on Unix at once; on
    /// Windows once the old file is released, see
    /// [`Document::poll_replace`]).
    ///
    /// Switching clears the undo history. Its steps may hold pieces of the
    /// old file (`Content::Pieces`, for large deletions), which can only be
    /// put back into a buffer over that file, and which would keep it
    /// mapped; the new buffer is over another file. Undo cannot step back
    /// past a save of a mapped document.
    pub fn save_written(
        &mut self,
        token: u64,
        result: Result<Written, SaveError>,
        now: Instant,
    ) -> SaveStep {
        if self.save_token() != Some(token) {
            // Dropping the result deletes its temp file.
            return SaveStep::Stale;
        }
        let mut written = match result {
            Ok(w) => w,
            Err(e) => {
                self.saving = None;
                return SaveStep::Failed(e);
            }
        };
        let (new, index) = match self.check_written(&mut written) {
            Ok(mapped) => mapped,
            Err(e) => {
                // Nothing changed; dropping the mapping deletes the file.
                self.saving = None;
                return SaveStep::Failed(e);
            }
        };
        let old = match self.switch_to(&new, index) {
            Ok(old) => old,
            Err(e) => {
                self.saving = None;
                return SaveStep::Failed(e);
            }
        };
        let Some(saving) = self.saving.as_mut() else {
            return SaveStep::Stale;
        };
        let old = match old {
            Some(old) if !saving.over_mapped => {
                // The rename does not touch the old file: nothing to wait
                // for, whoever still reads it.
                saver::release_later(old);
                None
            }
            old => old,
        };
        saving.replace = Some(Replace {
            old,
            new,
            temp: written.temp.clone(),
            target: written.target.clone(),
            meta: written.meta.clone(),
            until: now + saving.policy.wait,
        });
        if saving.policy.unmap_first {
            self.poll_replace(now)
        } else {
            self.replace_now()
        }
    }

    /// Map the written file, checking it is this document's text.
    fn check_written(
        &self,
        written: &mut Written,
    ) -> Result<(Arc<MappedFile>, LineIndex), SaveError> {
        let refuse = |why: &str| SaveError {
            step: "switching to the new file",
            source: std::io::Error::other(why.to_owned()),
        };
        // The app refuses edits while saving. If one got through anyway, the
        // file is not the document's text, and switching to it would lose
        // the edit: keep the document as it is instead.
        let version = self.saving.as_ref().map(|s| s.version);
        if !self.buffer.is_mapped()
            || version != Some(self.version)
            || written.len != self.buffer.len_bytes()
        {
            return Err(refuse("the document changed while it was saved"));
        }
        written.map(self.bom)
    }

    /// Make the document a fresh piece table over `new`, whose finished
    /// line index is `index`, letting go of everything that refers to the
    /// old file. Returns the old file's mapping, still to be released.
    ///
    /// Relies on `new` holding exactly the document's bytes (checked by
    /// [`Document::check_written`]), so every offset (cursor, selection,
    /// search results, scroll position) means the same afterwards.
    fn switch_to(
        &mut self,
        new: &Arc<MappedFile>,
        index: LineIndex,
    ) -> Result<Option<Arc<MappedFile>>, SaveError> {
        let limits = self
            .buffer
            .as_mapped()
            .map_or(Limits::DEFAULT, MappedBuffer::limits);
        let source: Arc<dyn Source> = Arc::clone(new) as Arc<dyn Source>;
        let buffer = MappedBuffer::with_index(source, index, limits).map_err(|e| SaveError {
            step: "switching to the new file",
            source: std::io::Error::other(e),
        })?;
        // From here the document reads the new file. The old buffer, the
        // undo history and the line counter are the old file's other
        // holders in this document.
        self.buffer = buffer.into();
        self.history = History::default();
        self.saved_at = NOT_SAVED;
        self.stop_indexing();
        let old = self.mapping.replace(Arc::clone(new));
        // Every line is counted now: the highlighter's line-based state
        // may change.
        self.syntax.touch();
        self.clamp_selections();
        Ok(old)
    }

    /// Check whether a written save can replace its target yet, and if so,
    /// do it. On Windows the old file must not be mapped anywhere, which
    /// the app checks every [`saver::POLL`] while this returns
    /// [`SaveStep::Waiting`] (stopping a search that reads it), for up to
    /// [`ReplacePolicy::wait`]; then it gives up, and the document stays on
    /// the temp file.
    pub fn poll_replace(&mut self, now: Instant) -> SaveStep {
        let Some(replace) = self.saving.as_ref().and_then(|s| s.replace.as_ref()) else {
            return SaveStep::Stale;
        };
        // One reference is `replace.old` itself. More means a search (or a
        // line counter on its way out) still maps the old file.
        let held = replace
            .old
            .as_ref()
            .is_some_and(|o| Arc::strong_count(o) > 1);
        if !held {
            return self.replace_now();
        }
        if now < replace.until {
            return SaveStep::Waiting;
        }
        let message = saver::not_replaced(
            &replace.temp,
            &replace.target,
            &"the old file is still in use",
        );
        self.saving = None;
        SaveStep::NotReplaced(message)
    }

    /// Release the old file and rename the temp file over the target.
    fn replace_now(&mut self) -> SaveStep {
        let Some(mut saving) = self.saving.take() else {
            return SaveStep::Stale;
        };
        let Some(mut replace) = saving.replace.take() else {
            return SaveStep::Stale;
        };
        let mut old = replace.old.take();
        if saving.policy.unmap_first {
            // Windows: this is the old file's last reference (checked by
            // `poll_replace`), so dropping it unmaps the file, and the
            // rename may replace it.
            old = None;
        }
        let renamed = (saving.policy.rename)(&replace.temp, &replace.target);
        if let Some(old) = old {
            // Unix: kept until the rename has happened, so the rename only
            // moved a name. Let it go off this thread: closing the replaced
            // file for the last time frees its disk space, which takes
            // about 20 ms per GB on APFS.
            saver::release_later(old);
        }
        match renamed {
            Ok(()) => {
                // The temp name is free again; the file is the target now.
                replace.new.keep_file();
                self.mark_saved(&replace.meta);
                SaveStep::Saved
            }
            Err(e) if saving.policy.unmap_first => {
                self.replace_unmapped(replace, saving.policy, &e)
            }
            Err(e) => {
                SaveStep::NotReplaced(saver::not_replaced(&replace.temp, &replace.target, &e))
            }
        }
    }

    /// Windows may refuse to rename the temp file while it is mapped. Let
    /// go of the mapping, rename, and map the file again through a handle
    /// kept open throughout (which follows the file through the rename), so
    /// its bytes stay reachable whatever happens. The document holds no
    /// text only between the unmapping and the remapping, inside this one
    /// call on the main thread, where nothing can draw or edit it.
    fn replace_unmapped(
        &mut self,
        replace: Replace,
        policy: ReplacePolicy,
        first: &std::io::Error,
    ) -> SaveStep {
        let Replace {
            new,
            temp,
            target,
            meta,
            ..
        } = replace;
        let not_replaced = |e: &dyn std::fmt::Display| {
            SaveStep::NotReplaced(saver::not_replaced(&temp, &target, e))
        };
        // Its holders should be `new`, `self.mapping` and the buffer (edits
        // were refused, so the history holds none of it). Any other, such
        // as a search, keeps it mapped whatever we drop here.
        if Arc::strong_count(&new) > 3 {
            return not_replaced(first);
        }
        let Ok(handle) = new.try_clone_file() else {
            return not_replaced(first);
        };
        let Some(mapped) = self.buffer.as_mapped() else {
            return not_replaced(first);
        };
        let (index, limits) = (mapped.line_index().clone(), mapped.limits());
        // The temp file must outlive its mapping now.
        new.keep_file();
        self.buffer = AnyBuffer::default();
        self.mapping = None;
        drop(new); // the last reference: unmapped and closed
        let renamed = (policy.rename)(&temp, &target);
        let (at, keep_temp) = match &renamed {
            Ok(()) => (&target, None),
            Err(_) => (&temp, Some(temp.clone())),
        };
        let changed = match self.remap(handle, index, limits, keep_temp, &meta) {
            Ok(changed) => changed,
            Err(e) => return self.lost_backing(at, &e),
        };
        match renamed {
            Ok(()) => {
                self.mark_saved(&meta);
                // Another program wrote to the file in between: the
                // document shows what it holds now, and says so.
                self.conflict = changed;
                SaveStep::Saved
            }
            Err(e) => not_replaced(&e),
        }
    }

    /// Map `file` again as the document's text (see
    /// [`Document::replace_unmapped`]), deleting it on drop if `temp`.
    /// Returns whether its size or time is no longer `meta`: another
    /// program changed it since it was written, so its lines are counted
    /// afresh instead of trusting `index`.
    fn remap(
        &mut self,
        file: std::fs::File,
        index: LineIndex,
        limits: Limits,
        temp: Option<PathBuf>,
        meta: &std::fs::Metadata,
    ) -> std::io::Result<bool> {
        let mut mapped = MappedFile::new(file)?;
        if let Some(temp) = temp {
            mapped = mapped.delete_on_drop(temp);
        }
        if self.bom {
            mapped = mapped.skip_prefix(io::BOM.len());
        }
        let changed = mapped
            .metadata()
            .map_or(true, |now| Stamp::from(&now) != Stamp::from(meta));
        let mapped = Arc::new(mapped);
        let index = if changed {
            let (index, indexing) = crate::indexer::start(&mapped)?;
            self.adopt_indexing(indexing);
            index
        } else {
            index
        };
        let source: Arc<dyn Source> = Arc::clone(&mapped) as Arc<dyn Source>;
        let buffer =
            MappedBuffer::with_index(source, index, limits).map_err(std::io::Error::other)?;
        self.buffer = buffer.into();
        self.mapping = Some(mapped);
        self.clamp_selections();
        self.syntax.touch();
        Ok(changed)
    }

    /// The saved file at `at` could not be mapped again after a moment
    /// without its mapping (which should never happen: it was mapped just
    /// before). Its text is safe on disk there, but this document is now
    /// empty: make it untitled, so no save can write the empty text over
    /// that file, and say where the text is.
    fn lost_backing(&mut self, at: &Path, e: &std::io::Error) -> SaveStep {
        log::error!("could not map {} again: {e}", at.display());
        self.path = None;
        self.canonical = None;
        self.disk = None;
        self.stop_indexing();
        self.selections = Selections::default();
        self.saved_at = self.history.top_id();
        SaveStep::NotReplaced(format!(
            "Saved to {} but could not read it back ({e}); open that file to go on",
            at.display()
        ))
    }

    /// The cursor's byte offset, always a valid position in the text.
    #[must_use]
    pub fn cursor(&self) -> usize {
        movement::clamp(&self.buffer, self.selections.primary().head)
    }

    /// The primary selection, snapped to valid positions.
    #[must_use]
    pub fn selection(&self) -> Selection {
        let s = self.selections.primary();
        Selection {
            anchor: movement::clamp(&self.buffer, s.anchor),
            head: movement::clamp(&self.buffer, s.head),
        }
    }

    fn set_selection(&mut self, sel: Selection) {
        debug_assert!(
            self.buffer.is_char_boundary(sel.head),
            "cursor off a char boundary"
        );
        self.selections.set_single(sel);
    }

    /// Replace `range` with `text`, put the cursor after it, and record the
    /// change for undo.
    ///
    /// # Errors
    ///
    /// Returns the buffer's [`EditError`] for an invalid range; nothing
    /// changes in that case.
    pub fn replace(&mut self, range: Range<usize>, text: &str) -> Result<Edit, EditError> {
        let before = self.selections.clone();
        let edit = self.buffer.replace(range, text)?;
        self.edits.note(&self.buffer, &edit);
        self.syntax.edit(&self.buffer, &edit);
        self.set_selection(Selection::cursor(edit.new_range().end));
        self.history
            .record(edit.clone(), &before, &self.selections, Instant::now());
        self.preferred_column = None;
        self.version += 1;
        Ok(edit)
    }

    /// Apply several replacements as one undo step. `items` pairs each
    /// range with its new text; ranges must be sorted, must not overlap, and
    /// refer to the text before any of them. The cursor ends after the last
    /// replacement. Returns how many were applied.
    ///
    /// # Errors
    ///
    /// Returns the buffer's [`EditError`] if a range is invalid. The ones
    /// applied before it (later in the text) stay applied and recorded.
    pub fn replace_many(&mut self, items: &[(Range<usize>, String)]) -> Result<usize, EditError> {
        let before = self.selections.clone();
        let mut edits = Vec::with_capacity(items.len());
        let mut result = Ok(());
        if self.buffer.is_mapped() {
            // A mapped buffer rebuilds its pieces once for all items; one at
            // a time would cost a rebuild per item (100,000 matches took
            // over a minute). All or nothing.
            let refs: Vec<(Range<usize>, &str)> =
                items.iter().map(|(r, t)| (r.clone(), t.as_str())).collect();
            match self.buffer.replace_sorted(&refs) {
                Ok(done) => edits = done,
                Err(e) => result = Err(e),
            }
            let (syntax, log, buffer) = (&mut self.syntax, &mut self.edits, &self.buffer);
            nimbus_core::report_sorted(&mut edits, |edit| {
                log.note(buffer, edit);
                syntax.edit(buffer, edit);
            });
        } else {
            // Apply from the end so earlier offsets stay valid; record in
            // application order, which is what undo reverses. Each edit is
            // shown to the syntax tree against the text it applies to.
            for (range, text) in items.iter().rev() {
                match self.buffer.replace(range.clone(), text) {
                    Ok(edit) => {
                        self.edits.note(&self.buffer, &edit);
                        self.syntax.edit(&self.buffer, &edit);
                        edits.push(edit);
                    }
                    Err(e) => {
                        result = Err(e);
                        break;
                    }
                }
            }
        }
        let count = edits.len();
        if count > 0 {
            // The last item lands after every applied item before it has
            // grown or shrunk the text.
            let applied = &items[items.len() - count..];
            let (last_range, last_text) = &applied[applied.len() - 1];
            let grown: usize = applied[..applied.len() - 1]
                .iter()
                .map(|(_, t)| t.len())
                .sum();
            let shrunk: usize = applied[..applied.len() - 1]
                .iter()
                .map(|(r, _)| r.len())
                .sum();
            let end = (last_range.start + last_text.len() + grown).saturating_sub(shrunk);
            self.set_selection(Selection::cursor(movement::clamp(&self.buffer, end)));
            self.history.record_group(edits, &before, &self.selections);
            self.version += 1;
        }
        result.map(|()| count)
    }

    /// Type `text` at every cursor, replacing any selections.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn insert(&mut self, text: &str) -> Result<(), EditError> {
        let items = self.each(|_, s| Some((s.range(), Cow::Borrowed(text))));
        self.apply_each(items, (Bias::After, Bias::After)).map(drop)
    }

    /// Paste: like typing, but line endings are converted to the file's.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    ///
    /// With several cursors, and as many lines on the clipboard as there
    /// are cursors (as copying from those cursors gives), each cursor gets
    /// its own line; otherwise each gets the whole text.
    pub fn paste(&mut self, text: &str) -> Result<(), EditError> {
        let unified = text.replace("\r\n", "\n");
        // One line per cursor, as copying from these cursors gives (an
        // empty last line for a bare last cursor), or, from elsewhere, the
        // same with one line break at the end.
        let count = self.selections.count();
        let mut lines: Vec<&str> = unified.split('\n').collect();
        if lines.len() == count + 1 && lines.last() == Some(&"") {
            lines.pop();
        }
        let ending = self.line_ending.as_str();
        let convert = |s: &str| match self.line_ending {
            LineEnding::Lf => s.to_owned(),
            LineEnding::CrLf => s.replace('\n', ending),
        };
        let items = if count > 1 && lines.len() == count {
            self.selections
                .iter()
                .zip(&lines)
                .map(|(s, line)| (s.range(), Cow::Owned(convert(line))))
                .collect()
        } else {
            let whole = convert(&unified);
            self.each(|_, s| Some((s.range(), Cow::Owned(whole.clone()))))
        };
        // A paste is one undo step even when it is a single character.
        self.history.seal();
        let result = self.apply_each(items, (Bias::After, Bias::After));
        self.history.seal();
        result.map(drop)
    }

    /// Enter: insert the line ending, then the current line's indentation,
    /// so a new line starts where the previous one did.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn insert_newline(&mut self) -> Result<(), EditError> {
        let ending = self.line_ending.as_str();
        let items = self.each(|t, s| {
            let r = s.range();
            let line_start = movement::line_start(t, r.start);
            // Only the indentation before the cursor carries over, so only
            // the line up to the cursor (and at most a few KB of it) is read.
            let head = t.slice(line_start..r.start.min(line_start + nimbus_core::long_line::NEAR));
            let indent_len = head.len() - head.trim_start_matches([' ', '\t']).len();
            Some((r, Cow::Owned(format!("{ending}{}", &head[..indent_len]))))
        });
        self.apply_each(items, (Bias::After, Bias::After)).map(drop)
    }

    /// How many bytes the selections hold together.
    #[must_use]
    pub fn selected_len(&self) -> usize {
        self.selections.iter().map(|s| s.range().len()).sum()
    }

    /// The selected text, or `None` if nothing is selected or it is longer
    /// than [`MAX_COPY_BYTES`]. Several selections are joined with line
    /// breaks, one line per cursor (an empty one for a bare cursor), so
    /// pasting at the same number of cursors gives each its own back.
    #[must_use]
    pub fn selected_text(&self) -> Option<String> {
        let len = self.selected_len();
        if len == 0 || len > MAX_COPY_BYTES {
            return None;
        }
        // The raw bytes, not the display string: bytes that are not UTF-8
        // become `�` here rather than the display stand-in.
        let mut bytes = Vec::with_capacity(len + self.selections.count());
        for (i, s) in self.selections.iter().enumerate() {
            if i > 0 {
                bytes.push(b'\n');
            }
            self.buffer.read_into(s.range(), &mut bytes).ok()?;
        }
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Put the cursor at `pos` (snapped to a valid position), as a click
    /// does; with `extend`, move only the head, as a Shift+click does.
    pub fn click(&mut self, pos: usize, extend: bool) {
        let pos = movement::clamp(&self.buffer, pos);
        let sel = self.selection().moved_to(pos, extend);
        self.set_selection(sel);
        self.preferred_column = None;
        self.history.seal();
    }

    /// Select `range` with the cursor at its end (double- and triple-click).
    pub fn select(&mut self, range: Range<usize>) {
        let t = &self.buffer;
        let sel = Selection {
            anchor: movement::clamp(t, range.start),
            head: movement::clamp(t, range.end),
        };
        self.set_selection(sel);
        self.preferred_column = None;
        self.history.seal();
    }

    /// Select from `anchor` to `head`, keeping the direction: the cursor is
    /// at `head`. Used when dragging with the mouse.
    pub fn select_span(&mut self, anchor: usize, head: usize) {
        let t = &self.buffer;
        let sel = Selection {
            anchor: movement::clamp(t, anchor),
            head: movement::clamp(t, head),
        };
        self.set_selection(sel);
        self.preferred_column = None;
    }

    /// Primary+D. With an empty primary, select the word under it. With a
    /// selection, add the next place its text occurs, after the primary
    /// and wrapping around, skipping places already selected, as a new
    /// primary. On a huge file each press reads at most [`NEXT_BUDGET`]
    /// bytes, so it never stalls; a later press goes on from there.
    pub fn add_next_occurrence(&mut self) -> Occurrence {
        let p = self.selection();
        let t = &self.buffer;
        if p.is_empty() {
            let word = movement::word_at(t, p.head);
            let is_word = !word.is_empty()
                && t.slice(word.clone())
                    .chars()
                    .any(|c| c.is_alphanumeric() || c == '_');
            if !is_word {
                return Occurrence::NoWord;
            }
            self.selections.replace_primary(Selection {
                anchor: word.start,
                head: word.end,
            });
            self.history.seal();
            return Occurrence::SelectedWord;
        }
        let r = p.range();
        if r.len() > MAX_NEEDLE {
            return Occurrence::TooLong;
        }
        let mut needle = Vec::with_capacity(r.len());
        if t.read_into(r.clone(), &mut needle).is_err() {
            return Occurrence::NotFound;
        }
        // From the primary to the end, then from the start back to it; or
        // on from where the last press gave up.
        let len = t.len_bytes();
        let resume = self
            .next_resume
            .take()
            .filter(|&(v, sel, _)| v == self.version && sel == p)
            .map(|(_, _, at)| at);
        let ranges = match resume {
            Some(at) if at >= r.end => [at..len, 0..r.end],
            Some(at) => [at..r.end, 0..0],
            None => [r.end..len, 0..r.end],
        };
        let mut budget = NEXT_BUDGET;
        for range in ranges {
            let mut from = range.start;
            loop {
                match search::find_literal(t, &needle, from..range.end, budget) {
                    Literal::Found(m) => {
                        let taken = self.selections.iter().any(|s| {
                            let sr = s.range();
                            sr.start < m.end && m.start < sr.end
                        });
                        if taken {
                            budget = budget.saturating_sub(m.end - from);
                            from = m.start + 1;
                            continue;
                        }
                        let sel = if p.head < p.anchor {
                            Selection {
                                anchor: m.end,
                                head: m.start,
                            }
                        } else {
                            Selection {
                                anchor: m.start,
                                head: m.end,
                            }
                        };
                        self.selections.push_primary(sel);
                        self.goals.clear();
                        self.history.seal();
                        return Occurrence::Added;
                    }
                    Literal::NotFound => {
                        budget = budget.saturating_sub(range.end - from);
                        break;
                    }
                    Literal::GaveUp(at) => {
                        self.next_resume = Some((self.version, p, at));
                        return Occurrence::GaveUp;
                    }
                }
            }
        }
        if self.selections.count() > 1 {
            Occurrence::AllSelected
        } else {
            Occurrence::NotFound
        }
    }

    /// Alt+click: add a cursor at `pos`, or remove the cursor already there
    /// (unless it is the only one). Returns whether a new cursor now sits
    /// at `pos` as the primary, which a drag may then stretch into a
    /// selection; not after a removal, nor a click inside a selection,
    /// which the new cursor merges into.
    pub fn click_add(&mut self, pos: usize) -> bool {
        let pos = movement::clamp(&self.buffer, pos);
        let existing = self.selections.iter().position(|s| s.head == pos);
        let added = if let Some(i) = existing {
            if self.selections.count() > 1 {
                self.selections.remove(i);
            }
            false
        } else {
            let before = self.selections.count();
            self.selections.push_primary(Selection::cursor(pos));
            self.selections.count() > before
        };
        self.goals.clear();
        self.preferred_column = None;
        self.history.seal();
        added
    }

    /// Alt+drag: the selection being dragged out (the primary, added by
    /// [`Document::click_add`]) runs from `anchor` to `head`; the others stay.
    pub fn drag_added(&mut self, anchor: usize, head: usize) {
        let t = &self.buffer;
        self.selections.replace_primary(Selection {
            anchor: movement::clamp(t, anchor),
            head: movement::clamp(t, head),
        });
        self.goals.clear();
    }

    /// Column (box) selection, as Shift+Alt+drag makes it: one selection on
    /// every line from the line starting at `from.0` to the one starting at
    /// `to.0`, each from display column `from.1` to column `to.1` (a cursor
    /// when they are equal). It replaces the selections there were.
    ///
    /// A line too short to reach the nearer column gets nothing, as in
    /// other editors; one that reaches it but not the farther column is
    /// selected to its end. At most [`MAX_COLUMN_LINES`] lines, so a drag
    /// across a huge file stays quick. The selection on the pointer's line
    /// is the primary.
    pub fn column_select(&mut self, from: (usize, usize), to: (usize, usize), tab_width: usize) {
        let t = &self.buffer;
        // Any byte of a line names it.
        let (from_line, to_line) = (t.line_span(from.0).start, t.line_span(to.0).start);
        let (top, bottom) = (from_line.min(to_line), from_line.max(to_line));
        let (a_col, h_col) = (from.1, to.1);
        let near = a_col.min(h_col);
        let mut rows: Vec<Selection> = Vec::new();
        let mut primary = None;
        let mut pos = top;
        for _ in 0..MAX_COLUMN_LINES {
            let span = t.line_span(pos);
            let width = long_line::col_at(t, span, span.end, tab_width);
            if width >= near {
                if span.start == to_line {
                    primary = Some(rows.len());
                }
                rows.push(Selection {
                    anchor: long_line::byte_at_col(t, span, a_col, tab_width),
                    head: long_line::byte_at_col(t, span, h_col, tab_width),
                });
            }
            match span.next {
                Some(next) if span.start < bottom => pos = next,
                _ => break,
            }
        }
        if rows.is_empty() {
            // Every line was too short: a cursor at the end of the
            // pointer's line.
            rows.push(Selection::cursor(t.line_span(to_line).end));
        }
        let primary = primary.unwrap_or(rows.len() - 1);
        self.selections = Selections::from_vec(rows, primary);
        self.goals.clear();
        self.preferred_column = None;
        self.history.seal();
    }

    /// Primary+Alt+Up or Down: add a cursor on the line above the topmost
    /// cursor (`lines` -1) or below the bottommost (1), at the column the
    /// primary aims for, as the new primary. So pressing it again keeps
    /// going. Does nothing at the first or last line.
    pub fn add_cursor_vertical(&mut self, lines: isize, tab_width: usize) {
        let all = self.selections.as_slice();
        let edge = if lines < 0 { all.first() } else { all.last() };
        let Some(&from) = edge else { return };
        let t = &self.buffer;
        let goal = self
            .preferred_column
            .unwrap_or_else(|| movement::column_of(t, self.selections.primary().head, tab_width));
        let v = movement::vertical(t, from.head, Some(goal), lines, tab_width);
        if v.pos == from.head || t.line_span(v.pos).start == t.line_span(from.head).start {
            return;
        }
        self.selections.push_primary(Selection::cursor(v.pos));
        self.goals.clear();
        self.preferred_column = Some(v.preferred_col);
        self.history.seal();
    }

    /// Escape with several cursors: keep only the primary. Returns whether
    /// there were several.
    pub fn collapse_cursors(&mut self) -> bool {
        if self.selections.count() < 2 {
            return false;
        }
        self.selections.collapse_to_primary();
        self.goals.clear();
        true
    }

    /// Select the whole document.
    pub fn select_all(&mut self) {
        self.select(0..self.buffer.len_bytes());
    }

    /// Move every cursor. With `extend` each selection grows or shrinks;
    /// without it, a selection collapses (Left and Right collapse to the
    /// selection's edge instead of moving). `page_lines` is one screenful.
    /// Up and Down keep each cursor's own column across shorter lines.
    pub fn move_cursor(
        &mut self,
        motion: Motion,
        extend: bool,
        tab_width: usize,
        page_lines: usize,
    ) {
        let page = isize::try_from(page_lines.max(1)).unwrap_or(isize::MAX);
        let lines = match motion {
            Motion::Up => Some(-1),
            Motion::Down => Some(1),
            Motion::PageUp => Some(-page),
            Motion::PageDown => Some(page),
            _ => None,
        };
        self.move_each(motion, extend, |t, pos, goal| match lines {
            Some(lines) => {
                let v = movement::vertical(t, pos, goal, lines, tab_width);
                (v.pos, Some(v.preferred_col))
            }
            None => (
                match motion {
                    Motion::Left => movement::prev_grapheme(t, pos),
                    Motion::Right => movement::next_grapheme(t, pos),
                    Motion::WordLeft => movement::word_left(t, pos),
                    Motion::WordRight => movement::word_right(t, pos),
                    Motion::LineStart => movement::smart_home(t, pos),
                    Motion::LineEnd => movement::line_end(t, pos),
                    Motion::DocStart => 0,
                    _ => t.len_bytes(),
                },
                None,
            ),
        });
    }

    /// Like [`Document::move_cursor`] with soft wrap on: Up, Down and the
    /// Page keys move by visual row; the column aimed for is then a column
    /// within the row. Other motions are the same as unwrapped.
    pub fn move_cursor_wrapped(
        &mut self,
        motion: Motion,
        extend: bool,
        w: &mut nimbus_core::wrap::WrapView<'_>,
        page_lines: usize,
    ) {
        let rows = match motion {
            Motion::Up => -1,
            Motion::Down => 1,
            Motion::PageUp => -isize::try_from(page_lines.max(1)).unwrap_or(isize::MAX),
            Motion::PageDown => isize::try_from(page_lines.max(1)).unwrap_or(isize::MAX),
            _ => return self.move_cursor(motion, extend, w.tab_width(), page_lines),
        };
        self.move_each(motion, extend, |t, pos, goal| {
            let v = movement::vertical_rows(t, w, pos, goal, rows);
            (v.pos, Some(v.preferred_col))
        });
    }

    /// Move every selection's head with `step`, which takes the text, the
    /// head and the column it aims for, and returns where it goes and the column to
    /// aim for next (`None` for motions that do not keep one). Collapses
    /// selections for Left and Right without `extend`, then sorts and
    /// merges. The primary's column is [`Document::preferred_column`].
    fn move_each(
        &mut self,
        motion: Motion,
        extend: bool,
        mut step: impl FnMut(&dyn Text, usize, Option<usize>) -> (usize, Option<usize>),
    ) {
        self.history.seal();
        let count = self.selections.count();
        let primary = self.selections.primary_index();
        if self.goals.len() != count {
            self.goals.clear();
            self.goals.resize(count, None);
        }
        let (goals, preferred) = (&mut self.goals, &mut self.preferred_column);
        let t = &self.buffer;
        if let Some(g) = goals.get_mut(primary) {
            *g = *preferred;
        }
        let edge = !extend && matches!(motion, Motion::Left | Motion::Right);
        self.selections.transform(|i, s| {
            let goal = goals.get(i).copied().flatten();
            let (sel, next) = if edge && !s.is_empty() {
                let r = s.range();
                let pos = if motion == Motion::Left {
                    r.start
                } else {
                    r.end
                };
                (Selection::cursor(pos), None)
            } else {
                let (pos, next) = step(t, s.head, goal);
                (s.moved_to(pos, extend), next)
            };
            if let Some(g) = goals.get_mut(i) {
                *g = next;
            }
            if i == primary {
                *preferred = next;
            }
            sel
        });
        if self.selections.count() != count {
            // Cursors merged: which goal belongs to which is lost.
            self.goals.clear();
        }
    }

    /// Backspace at every cursor: delete each selection, or the grapheme
    /// before each cursor. At the start of a line this joins it to the
    /// previous one, removing a `\r\n` as one unit. Returns whether
    /// anything was deleted.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn delete_back(&mut self) -> Result<bool, EditError> {
        let items = self.each(|t, s| {
            let r = if s.is_empty() {
                movement::prev_grapheme(t, s.head)..s.head
            } else {
                s.range()
            };
            Some((r, Cow::Borrowed("")))
        });
        self.apply_each(items, (Bias::After, Bias::After))
    }

    /// Delete key at every cursor: delete each selection, or the grapheme
    /// after each cursor. Returns whether anything was deleted.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn delete_forward(&mut self) -> Result<bool, EditError> {
        let items = self.each(|t, s| {
            let r = if s.is_empty() {
                s.head..movement::next_grapheme(t, s.head)
            } else {
                s.range()
            };
            Some((r, Cow::Borrowed("")))
        });
        self.apply_each(items, (Bias::After, Bias::After))
    }

    /// Delete only the non-empty selections (Cut): cursors without a
    /// selection delete nothing. Returns whether anything was deleted.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn delete_selections(&mut self) -> Result<bool, EditError> {
        let items = self.each(|_, s| (!s.is_empty()).then(|| (s.range(), Cow::Borrowed(""))));
        self.apply_each(items, (Bias::After, Bias::After))
    }

    /// Delete the selection, or from the cursor to where `motion` would move
    /// it (a word or the line start, for Option/Ctrl+Backspace and similar).
    /// Returns `None` when there is nothing to delete.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn delete_to(&mut self, motion: Motion) -> Result<bool, EditError> {
        let items = self.each(|t, s| {
            let r = if s.is_empty() {
                let target = match motion {
                    Motion::WordLeft => movement::word_left(t, s.head),
                    Motion::WordRight => movement::word_right(t, s.head),
                    Motion::LineStart => movement::line_start(t, s.head),
                    _ => s.head,
                };
                s.head.min(target)..s.head.max(target)
            } else {
                s.range()
            };
            Some((r, Cow::Borrowed("")))
        });
        self.apply_each(items, (Bias::After, Bias::After))
    }

    /// Each line touched by any selection, once, in order: for indenting,
    /// where two cursors on one line must not indent it twice.
    fn touched_lines(&self) -> Vec<usize> {
        let mut lines: Vec<usize> = Vec::new();
        for s in self.selections.iter() {
            for l in lines_of(&self.buffer, s.range()) {
                if lines.last().is_none_or(|&last| l > last) {
                    lines.push(l);
                }
            }
        }
        lines
    }

    /// Tab when a selection spans lines: indent every line touched by any
    /// selection by one tab, as one undo step, and select those whole
    /// lines. Returns `false` (doing nothing) when every selection is
    /// within one line; the caller then types a tab at each cursor.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn indent_lines(&mut self) -> Result<bool, EditError> {
        let t = &self.buffer;
        if !self
            .selections
            .iter()
            .any(|s| lines_of(t, s.range()).len() > 1)
        {
            return Ok(false);
        }
        let items = self
            .touched_lines()
            .into_iter()
            .map(|l| {
                let s = self.buffer.line_to_byte(l);
                (s..s, Cow::Borrowed("\t"))
            })
            .collect();
        self.history.seal();
        self.apply_each(items, (Bias::Before, Bias::After))?;
        self.history.seal();
        self.select_whole_lines();
        Ok(true)
    }

    /// Shift+Tab: remove one level of indentation (a tab, or up to
    /// `tab_width` spaces) from every line touched by any selection, as
    /// one undo step. A selection spanning lines then covers them whole; a
    /// cursor or selection within one line keeps its place in the text.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn outdent_lines(&mut self, tab_width: usize) -> Result<(), EditError> {
        let items: Vec<_> = self
            .touched_lines()
            .into_iter()
            .filter_map(|l| {
                let t = &self.buffer;
                let s = t.line_to_byte(l);
                // Only the start of the line matters.
                let end = t.line_span(s).end.min(s + tab_width + 1);
                let text = t.slice(s..end);
                let n = if text.starts_with('\t') {
                    1
                } else {
                    text.bytes()
                        .take(tab_width)
                        .take_while(|b| *b == b' ')
                        .count()
                };
                (n > 0).then(|| (s..s + n, Cow::Borrowed("")))
            })
            .collect();
        self.history.seal();
        self.apply_each(items, (Bias::Before, Bias::After))?;
        self.history.seal();
        self.select_whole_lines();
        Ok(())
    }

    /// Make every selection that spans lines cover those lines whole, from
    /// the start of the first to the end of the last, as after indenting.
    fn select_whole_lines(&mut self) {
        let t = &self.buffer;
        self.selections.transform(|_, s| {
            let lines = lines_of(t, s.range());
            if lines.len() < 2 {
                return s;
            }
            Selection {
                anchor: t.line_to_byte(lines.start),
                head: movement::line_end(t, t.line_to_byte(lines.end - 1)),
            }
        });
    }

    /// Undo the last step. Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// Returns an [`EditError`] if the history no longer matches the text,
    /// which would be a bug.
    pub fn undo(&mut self) -> Result<bool, EditError> {
        let (syntax, log) = (&mut self.syntax, &mut self.edits);
        let Some(restored) = self.history.undo(&mut self.buffer, &mut |text, edit| {
            log.note(text, edit);
            syntax.edit(text, edit);
        })?
        else {
            return Ok(false);
        };
        self.restore(&restored);
        Ok(true)
    }

    /// Redo the last undone step. Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// As for [`Document::undo`].
    pub fn redo(&mut self) -> Result<bool, EditError> {
        let (syntax, log) = (&mut self.syntax, &mut self.edits);
        let Some(restored) = self.history.redo(&mut self.buffer, &mut |text, edit| {
            log.note(text, edit);
            syntax.edit(text, edit);
        })?
        else {
            return Ok(false);
        };
        self.restore(&restored);
        Ok(true)
    }

    fn restore(&mut self, restored: &Selections) {
        self.selections = restored.clone();
        self.clamp_selections();
        self.preferred_column = None;
        self.goals.clear();
        self.version += 1;
    }

    /// Snap every selection to a valid position in the text as it is now
    /// (after undo, or after switching to a saved file), merging any that
    /// now coincide.
    fn clamp_selections(&mut self) {
        let t = &self.buffer;
        self.selections.transform(|_, s| Selection {
            anchor: movement::clamp(t, s.anchor),
            head: movement::clamp(t, s.head),
        });
    }

    /// Apply one replacement per cursor (`items`, from [`Document::each`])
    /// as one batch: one pass over the buffer, the syntax tree told of
    /// every edit, every cursor moved through the edits with the biases
    /// `bias` (start, end), and one undo batch, which typing coalesces
    /// with the batch before it. Returns whether anything changed.
    fn apply_each(
        &mut self,
        mut items: Vec<(Range<usize>, Cow<'_, str>)>,
        bias: (Bias, Bias),
    ) -> Result<bool, EditError> {
        let before = self.selections.clone();
        let mut edits = multi::apply_each(&mut self.buffer, &mut items)?;
        if edits.is_empty() {
            return Ok(false);
        }
        let (syntax, log, buffer) = (&mut self.syntax, &mut self.edits, &self.buffer);
        report_sorted(&mut edits, |edit| {
            log.note(buffer, edit);
            syntax.edit(buffer, edit);
        });
        self.selections.map_through(&edits, bias.0, bias.1);
        self.history
            .record_batch(edits, &before, &self.selections, Instant::now());
        self.preferred_column = None;
        self.goals.clear();
        self.version += 1;
        Ok(true)
    }

    /// One replacement per selection: what `f` returns for it, if anything.
    fn each<'a>(
        &self,
        mut f: impl FnMut(&dyn Text, Selection) -> Option<(Range<usize>, Cow<'a, str>)>,
    ) -> Vec<(Range<usize>, Cow<'a, str>)> {
        self.selections
            .iter()
            .filter_map(|&s| f(&self.buffer, s))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> Document {
        Document::from_text(text)
    }

    fn text(d: &Document) -> String {
        d.buffer.slice(0..d.buffer.len_bytes()).into_owned()
    }

    #[test]
    fn typing_moves_the_cursor_and_marks_dirty() {
        let mut d = Document::untitled();
        assert!(!d.is_dirty());
        d.insert("hé").unwrap();
        d.insert("llo").unwrap();
        assert_eq!(d.buffer.line(0), "héllo");
        assert_eq!(d.cursor(), 6);
        assert!(d.is_dirty());
        assert_eq!(d.version, 2);
    }

    #[test]
    fn undoing_back_to_the_start_is_clean_again() {
        let mut d = doc("abc");
        d.click(3, false);
        d.insert("d").unwrap();
        assert!(d.is_dirty());
        d.undo().unwrap();
        assert!(!d.is_dirty());
        assert_eq!(d.cursor(), 3, "undo restores the cursor");
    }

    #[test]
    fn enter_uses_the_line_ending_and_keeps_indentation() {
        let mut d = doc("\t  foo");
        d.line_ending = LineEnding::CrLf;
        d.click(6, false);
        d.insert_newline().unwrap();
        assert_eq!(text(&d), "\t  foo\r\n\t  ");
        d.click(1, false);
        d.insert_newline().unwrap();
        assert_eq!(
            text(&d),
            "\t\r\n\t  foo\r\n\t  ",
            "only indentation before the cursor"
        );
    }

    #[test]
    fn typing_replaces_a_selection() {
        let mut d = doc("hello world");
        d.select(6..11);
        d.insert("there").unwrap();
        assert_eq!(d.buffer.line(0), "hello there");
    }

    #[test]
    fn shift_movement_extends_and_plain_left_right_collapse() {
        let mut d = doc("abcdef");
        d.click(1, false);
        d.move_cursor(Motion::Right, true, 4, 10);
        d.move_cursor(Motion::Right, true, 4, 10);
        assert_eq!(d.selection().range(), 1..3);
        assert_eq!(d.selected_text().as_deref(), Some("bc"));
        d.move_cursor(Motion::Left, false, 4, 10);
        assert_eq!(
            d.selection(),
            Selection::cursor(1),
            "collapses to the start"
        );
    }

    #[test]
    fn paste_converts_line_endings_and_is_one_step() {
        let mut d = doc("");
        d.line_ending = LineEnding::CrLf;
        d.insert("x").unwrap();
        d.paste("a\nb\r\nc").unwrap();
        assert_eq!(text(&d), "xa\r\nb\r\nc");
        d.undo().unwrap();
        assert_eq!(text(&d), "x");
    }

    #[test]
    fn indent_and_outdent_selected_lines_as_one_step() {
        let mut d = doc("a\n    b\nc");
        d.select(0..7);
        assert!(d.indent_lines().unwrap());
        assert_eq!(text(&d), "\ta\n\t    b\nc");
        d.outdent_lines(4).unwrap();
        assert_eq!(text(&d), "a\n    b\nc");
        d.outdent_lines(4).unwrap();
        assert_eq!(text(&d), "a\nb\nc");
        d.undo().unwrap();
        assert_eq!(text(&d), "a\n    b\nc");
        d.click(0, false);
        assert!(
            !d.indent_lines().unwrap(),
            "single line: caller types a tab"
        );
    }

    #[test]
    fn replace_many_is_one_step_with_cursor_after_the_last() {
        let mut d = doc("a-b-c");
        let items = [(1..2, "--".to_owned()), (3..4, "--".to_owned())];
        assert_eq!(d.replace_many(&items).unwrap(), 2);
        assert_eq!(text(&d), "a--b--c");
        assert_eq!(d.cursor(), 6);
        d.undo().unwrap();
        assert_eq!(text(&d), "a-b-c");
    }

    #[test]
    fn backspace_removes_whole_graphemes_and_crlf() {
        let mut d = doc("ab\r\ne\u{301}");
        d.move_cursor(Motion::DocEnd, false, 4, 10);
        d.delete_back().unwrap();
        assert_eq!(text(&d), "ab\r\n");
        d.delete_back().unwrap();
        assert_eq!(text(&d), "ab");
        d.move_cursor(Motion::DocStart, false, 4, 10);
        assert!(!d.delete_back().unwrap());
    }

    #[test]
    fn vertical_moves_keep_the_column_and_others_reset_it() {
        let mut d = doc("abcdef\nab\nabcdef");
        d.click(5, false);
        d.move_cursor(Motion::Down, false, 4, 10);
        assert_eq!(d.cursor(), 9);
        d.move_cursor(Motion::Down, false, 4, 10);
        assert_eq!(d.cursor(), 15);
        d.move_cursor(Motion::Left, false, 4, 10);
        assert_eq!(d.preferred_column, None);
    }

    #[test]
    fn click_snaps_inside_characters() {
        let mut d = doc("é\r\nx");
        d.click(1, false);
        assert_eq!(d.cursor(), 0);
        d.click(3, false);
        assert_eq!(d.cursor(), 2);
    }

    #[test]
    fn untitled_save_fails_and_stays_dirty() {
        let mut d = Document::untitled();
        d.insert("x").unwrap();
        assert!(d.save().is_err());
        assert!(d.is_dirty());
        assert_eq!(d.display_name(), "untitled");
    }

    #[test]
    fn delete_word_and_to_line_start() {
        let mut d = doc("let foo_bar = 1;");
        d.move_cursor(Motion::DocEnd, false, 4, 10);
        d.delete_to(Motion::WordLeft).unwrap();
        assert_eq!(text(&d), "let foo_bar = 1");
        d.click(11, false);
        d.delete_to(Motion::WordLeft).unwrap();
        assert_eq!(text(&d), "let  = 1");
        d.click(4, false);
        d.delete_to(Motion::LineStart).unwrap();
        assert_eq!(text(&d), " = 1");
        d.click(0, false);
        d.delete_to(Motion::WordRight).unwrap();
        assert_eq!(text(&d), " 1");
    }

    #[test]
    fn changes_on_disk_reload_conflict_or_mark_deleted() {
        let dir = std::env::temp_dir().join(format!("nimbus-disk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let langs = Languages::default();
        let mut d = Document::open(&path).unwrap();
        d.set_selection(Selection::cursor(6)); // line 1, column 2
        assert_eq!(d.disk_changed(&langs), DiskChange::None, "nothing changed");

        // Changed outside, nothing unsaved: reloads, cursor on its line.
        std::fs::write(&path, "ONE\nTWO!\nTHREE\nFOUR\n").unwrap();
        assert_eq!(d.disk_changed(&langs), DiskChange::Reloaded);
        assert_eq!(d.buffer.line(1), "TWO!");
        assert_eq!(d.cursor(), 4 + 2);
        assert!(!d.is_dirty() && !d.history.can_undo());

        // Our own save is not a change.
        d.insert("x").unwrap();
        d.save().unwrap();
        assert_eq!(d.disk_changed(&langs), DiskChange::None);

        // Unsaved edits here: a conflict, until kept.
        d.insert("y").unwrap();
        std::fs::write(&path, "theirs, a longer text\n").unwrap();
        assert_eq!(d.disk_changed(&langs), DiskChange::Conflict);
        assert!(d.conflict && d.is_dirty());
        d.keep_mine();
        assert!(!d.conflict);
        assert_eq!(d.disk_changed(&langs), DiskChange::None, "asked once");

        // Deleted, then recreated by saving.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(d.disk_changed(&langs), DiskChange::Deleted);
        assert!(d.deleted);
        d.save().unwrap();
        assert!(!d.deleted && path.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A fresh directory for one test, removed on drop (after the
    /// documents declared later, which may hold mappings of its files).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("nimbus-doc-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn raw(d: &Document) -> Vec<u8> {
        let mut out = Vec::new();
        d.buffer
            .read_into(0..d.buffer.len_bytes(), &mut out)
            .unwrap();
        out
    }

    fn append(path: &Path, bytes: &[u8]) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(bytes).unwrap();
    }

    #[test]
    fn a_mapped_document_is_highlighted_simply_and_saved_on_a_worker() {
        let dir = TempDir::new("mapped-save");
        let path = dir.0.join("odd.rs");
        // Not UTF-8, so it opens mapped although it is tiny.
        let original = b"fn main() {}\n// \xff\n";
        std::fs::write(&path, original).unwrap();
        let mut d = Document::open_mapped(&path).unwrap();
        assert!(d.buffer.is_mapped());
        assert_eq!(raw(&d), original);
        d.detect_language(&Languages::default());
        assert!(d.syntax.lexical().is_some(), "small, but never parsed");
        assert!(d.syntax.take_job(&d.buffer).is_none());

        // Saving on this thread is refused (crate::saver tests the rest).
        d.insert("x").unwrap();
        let err = d.save().unwrap_err();
        assert!(err.to_string().contains("worker"), "{err}");
        assert!(d.is_dirty());
        assert_eq!(std::fs::read(&path).unwrap(), original, "nothing written");

        // While a save runs, changes on disk wait for it and reloading is
        // refused.
        let job = d.begin_save(ReplacePolicy::default()).unwrap();
        append(&path, b"more\n");
        assert_eq!(d.disk_changed(&Languages::default()), DiskChange::None);
        assert!(d.reload(&Languages::default()).is_err());
        let token = job.token;
        let step = d.save_written(token, job.run(|_, _| {}), Instant::now());
        assert!(matches!(step, SaveStep::Saved), "{step:?}");
        assert_eq!(std::fs::read(&path).unwrap(), b"xfn main() {}\n// \xff\n");
        assert_eq!(d.disk_changed(&Languages::default()), DiskChange::None);
    }

    #[test]
    fn copy_refuses_huge_selections() {
        let mut d = Document {
            buffer: nimbus_core::MappedBuffer::from_bytes(vec![b'x'; MAX_COPY_BYTES + 1]).into(),
            ..Document::default()
        };
        d.select_all();
        assert_eq!(d.selected_text(), None);
        d.select(0..3);
        assert_eq!(d.selected_text().as_deref(), Some("xxx"));
    }

    #[test]
    fn mapped_documents_reload_by_themselves_only_when_cut_short() {
        let dir = TempDir::new("mapped-disk");
        let path = dir.0.join("app.log");
        std::fs::write(&path, b"one \xff\ntwo\n").unwrap();
        let langs = Languages::default();
        let mut d = Document::open_mapped(&path).unwrap();
        assert!(d.buffer.is_mapped());

        // A growing log: marked as changed, never reloaded, said once.
        append(&path, b"three\n");
        assert_eq!(d.disk_changed(&langs), DiskChange::NotReloaded);
        assert!(d.conflict && !d.is_dirty());
        assert_eq!(raw(&d), b"one \xff\ntwo\n");
        append(&path, b"four\n");
        assert_eq!(d.disk_changed(&langs), DiskChange::None);
        d.insert("x").unwrap();
        append(&path, b"five\n");
        assert_eq!(d.disk_changed(&langs), DiskChange::None, "still said once");

        // Reloading by hand maps the longer file.
        d.reload(&langs).unwrap();
        assert!(!d.conflict && !d.is_dirty());
        assert_eq!(raw(&d), b"one \xff\ntwo\nthree\nfour\nfive\n");
        append(&path, b"six\n");
        d.insert("y").unwrap();
        assert_eq!(d.disk_changed(&langs), DiskChange::Conflict);

        // Windows refuses to truncate a mapped file, so the rest is Unix.
        #[cfg(unix)]
        {
            // Cut short with nothing unsaved: re-mapped at once, the cursor
            // kept at its byte offset where it still exists.
            d.reload(&langs).unwrap();
            d.click(5, false);
            std::fs::write(&path, b"on\xff").unwrap();
            assert_eq!(
                d.disk_changed(&langs),
                DiskChange::Truncated { reloaded: true }
            );
            assert_eq!(raw(&d), b"on\xff");
            assert_eq!(d.cursor(), 3);
            assert!(!d.conflict);

            // With unsaved edits it must wait for the user.
            d.insert("z").unwrap();
            std::fs::write(&path, b"o").unwrap();
            assert_eq!(
                d.disk_changed(&langs),
                DiskChange::Truncated { reloaded: false }
            );
            assert!(d.conflict && d.is_dirty());
        }
    }

    #[test]
    fn small_files_that_are_not_utf8_stay_in_memory_and_save_at_once() {
        let dir = TempDir::new("bytes-save");
        let path = dir.0.join("latin1.txt");
        std::fs::write(&path, b"caf\xe9\n").unwrap();
        let mut d = Document::open(&path).unwrap();
        assert!(d.buffer.is_mapped() && !d.saves_on_worker());
        d.click(0, false);
        d.insert("le ").unwrap();
        d.save().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"le caf\xe9\n");
        assert!(!d.is_dirty());
    }

    fn cursors(d: &mut Document, at: &[usize]) {
        let sels = at.iter().map(|&p| Selection::cursor(p)).collect();
        d.selections = Selections::from_vec(sels, 0);
    }

    fn heads(d: &Document) -> Vec<usize> {
        d.selections.iter().map(|s| s.head).collect()
    }

    #[test]
    fn typing_and_deleting_at_several_cursors_undo_as_one_step() {
        let mut d = doc("ab\ncd\nef");
        cursors(&mut d, &[2, 5, 8]);
        let start = d.selections.clone();
        d.insert("x").unwrap();
        d.insert("y").unwrap();
        assert_eq!(text(&d), "abxy\ncdxy\nefxy");
        assert_eq!(heads(&d), [4, 9, 14]);
        d.delete_back().unwrap();
        assert_eq!(text(&d), "abx\ncdx\nefx");
        // Typing then deleting: two steps. Undo restores every cursor.
        d.undo().unwrap();
        assert_eq!(text(&d), "abxy\ncdxy\nefxy");
        d.undo().unwrap();
        assert_eq!(text(&d), "ab\ncd\nef");
        assert_eq!(d.selections, start);
        d.redo().unwrap();
        assert_eq!(heads(&d), [4, 9, 14]);
    }

    #[test]
    fn enter_indents_each_line_from_its_own_indentation() {
        let mut d = doc("a\n\tb\n    c");
        cursors(&mut d, &[1, 4, 10]);
        d.insert_newline().unwrap();
        assert_eq!(text(&d), "a\n\n\tb\n\t\n    c\n    ");
    }

    #[test]
    fn paste_gives_each_cursor_its_line_when_the_counts_match() {
        let mut d = doc("a\nb\nc");
        cursors(&mut d, &[1, 3, 5]);
        d.paste("1\n2\n3\n").unwrap();
        assert_eq!(text(&d), "a1\nb2\nc3");
        // Otherwise the whole text at each.
        let mut d = doc("a\nb");
        cursors(&mut d, &[1, 3]);
        d.paste("x\ny\nz").unwrap();
        assert_eq!(text(&d), "ax\ny\nz\nbx\ny\nz");
    }

    #[test]
    fn a_line_shared_by_two_selections_is_indented_once() {
        let mut d = doc("one\ntwo\nthree\n");
        d.selections = Selections::from_vec(
            vec![
                Selection { anchor: 0, head: 5 },
                Selection {
                    anchor: 6,
                    head: 10,
                },
            ],
            0,
        );
        assert!(d.indent_lines().unwrap());
        assert_eq!(text(&d), "\tone\n\ttwo\n\tthree\n");
        d.outdent_lines(4).unwrap();
        assert_eq!(text(&d), "one\ntwo\nthree\n");
        d.undo().unwrap();
        d.undo().unwrap();
        assert_eq!(text(&d), "one\ntwo\nthree\n");
    }

    #[test]
    fn every_cursor_moves_and_keeps_its_own_column() {
        // Cursors at column 4 of line 0 and column 1 of line 1; line 2 is
        // short, line 3 long again.
        let mut d = doc("abcdef\nxyz\nq\nlonglong\nlonglong");
        cursors(&mut d, &[4, 8]);
        d.move_cursor(Motion::Down, false, 4, 10);
        // Line 1 has 3 columns: the first cursor stops at its end (col 3).
        assert_eq!(heads(&d), [10, 12]);
        d.move_cursor(Motion::Down, false, 4, 10);
        d.move_cursor(Motion::Down, false, 4, 10);
        // Back at their own columns on the long lines: 4 and 1.
        assert_eq!(heads(&d), [17, 23]);
        // Moving to the line start merges cursors that meet.
        d.move_cursor(Motion::DocStart, false, 4, 10);
        assert_eq!(heads(&d), [0]);
    }

    #[test]
    fn undo_forgets_the_columns_cursors_were_aiming_for() {
        // Two cursors at columns 4 and 1; type, move down (columns kept),
        // then undo: the next Down aims from where undo put them.
        let mut d = doc("abcdef\nabcdef\nabcdef\nabcdef");
        cursors(&mut d, &[4, 8]);
        d.insert("x").unwrap();
        d.move_cursor(Motion::Down, false, 4, 10);
        d.undo().unwrap();
        assert_eq!(heads(&d), [4, 8]);
        d.move_cursor(Motion::Down, false, 4, 10);
        assert_eq!(heads(&d), [11, 15], "columns 4 and 1 of the next lines");
    }

    #[test]
    fn column_selection_puts_a_selection_on_each_line() {
        // Lines start at 0, 7, 10, 17 and 23 ("\tx yz" is a tab, then x).
        let mut d = doc("abcdef\nab\nabcdef\n\tx yz\nabcdef");
        // Cursors in column 1 down four lines; column 1 on the tab line is
        // nearer the tab's start, so that cursor is before it.
        d.column_select((0, 1), (17, 1), 4);
        assert_eq!(heads(&d), [1, 8, 11, 17]);
        assert_eq!(d.selection().head, 17, "the pointer's line is primary");
        // Columns 3 to 5: "ab" is too short and gets nothing. A column
        // inside a tab goes to the tab's nearer edge: column 3 is nearer
        // its end (column 4), so that line's selection is just "x".
        d.column_select((0, 3), (17, 5), 4);
        assert_eq!(
            d.selections
                .iter()
                .map(Selection::range)
                .collect::<Vec<_>>(),
            [3..5, 13..15, 18..19]
        );
        // Dragged upwards: the same lines, the pointer's line primary.
        d.column_select((23, 2), (7, 2), 4);
        assert_eq!(heads(&d), [9, 12, 17, 25]);
        assert_eq!(d.selection().head, 9);
        // Any byte of a line names it.
        d.column_select((3, 0), (8, 0), 4);
        assert_eq!(heads(&d), [0, 7]);
        // Typing then goes to every line.
        d.insert("> ").unwrap();
        assert_eq!(text(&d), "> abcdef\n> ab\nabcdef\n\tx yz\nabcdef");
        // Past every line's end: a cursor at the end of the pointer's line.
        d.column_select((0, 40), (11, 40), 4);
        assert_eq!(d.selections.count(), 1);
    }

    #[test]
    fn copy_joins_selections_one_line_each() {
        let mut d = doc("one two three");
        d.selections = Selections::from_vec(
            vec![
                Selection { anchor: 0, head: 3 },
                Selection::cursor(5),
                Selection {
                    anchor: 8,
                    head: 13,
                },
            ],
            0,
        );
        assert_eq!(d.selected_text().as_deref(), Some("one\n\nthree"));
        assert_eq!(d.selected_len(), 8);
        // Shift+Right extends each; Right collapses each to its end.
        d.move_cursor(Motion::Right, true, 4, 10);
        assert_eq!(heads(&d), [4, 6, 13]);
        d.move_cursor(Motion::Right, false, 4, 10);
        assert_eq!(heads(&d), [4, 6, 13]);
    }

    fn ranges(d: &Document) -> Vec<Range<usize>> {
        d.selections.iter().map(Selection::range).collect()
    }

    #[test]
    fn primary_d_selects_the_word_then_adds_each_next_occurrence() {
        // Cursor inside the second "foo"; occurrences at 0, 8 and 16.
        let mut d = doc("foo bar foo baz foo");
        d.click(9, false);
        assert_eq!(d.add_next_occurrence(), Occurrence::SelectedWord);
        assert_eq!((d.selections.count(), d.selection().range()), (1, 8..11));
        // The next one after it, then wrapping round to the first.
        assert_eq!(d.add_next_occurrence(), Occurrence::Added);
        assert_eq!(ranges(&d), [8..11, 16..19]);
        assert_eq!(d.selection().range(), 16..19, "the newest is primary");
        assert_eq!(d.add_next_occurrence(), Occurrence::Added);
        assert_eq!(ranges(&d), [0..3, 8..11, 16..19]);
        assert_eq!(d.add_next_occurrence(), Occurrence::AllSelected);
        // Typing replaces all three.
        d.insert("x").unwrap();
        assert_eq!(text(&d), "x bar x baz x");
        // Escape keeps the primary.
        assert!(d.collapse_cursors());
        assert_eq!(heads(&d), [1]);
        assert!(!d.collapse_cursors());

        let mut d = doc("a  b");
        d.click(2, false);
        assert_eq!(d.add_next_occurrence(), Occurrence::NoWord);
        d.select(0..1);
        assert_eq!(d.add_next_occurrence(), Occurrence::NotFound);
    }

    #[test]
    fn alt_click_adds_and_removes_cursors_and_cursors_go_above_and_below() {
        let mut d = doc("one\ntwo\nthree");
        d.click(1, false);
        assert!(d.click_add(5));
        assert_eq!(heads(&d), [1, 5]);
        assert!(!d.click_add(1), "a removal is not dragged");
        assert_eq!(heads(&d), [5], "clicking a cursor removes it");
        assert!(!d.click_add(5));
        assert_eq!(heads(&d), [5], "but never the last one");
        d.select(0..4);
        assert!(!d.click_add(2), "inside a selection: merged, not dragged");
        d.click(5, false);
        d.drag_added(5, 7);
        assert_eq!((d.selections.count(), d.selection().range()), (1, 5..7));

        d.click(5, false);
        d.add_cursor_vertical(1, 4);
        d.add_cursor_vertical(-1, 4);
        assert_eq!(heads(&d), [1, 5, 9]);
        d.add_cursor_vertical(-1, 4);
        assert_eq!(heads(&d), [1, 5, 9], "nothing above the first line");
        d.add_cursor_vertical(1, 4);
        assert_eq!(heads(&d), [1, 5, 9], "nor below the last");
    }

    #[test]
    fn copy_and_paste_round_trip_with_a_bare_last_cursor() {
        let mut d = doc("abc\nxyz");
        d.selections = Selections::from_vec(
            vec![Selection { anchor: 0, head: 1 }, Selection::cursor(6)],
            0,
        );
        let copied = d.selected_text().unwrap();
        assert_eq!(copied, "a\n");
        d.paste(&copied).unwrap();
        // The first cursor's "a" replaces "a"; the second gets "".
        assert_eq!(text(&d), "abc\nxyz");
        // From elsewhere, with a trailing line break: still one each.
        d.selections = Selections::from_vec(vec![Selection::cursor(0), Selection::cursor(4)], 0);
        d.paste("1\n2\n").unwrap();
        assert_eq!(text(&d), "1abc\n2xyz");
    }

    #[test]
    fn cut_deletes_only_the_selections() {
        let mut d = doc("abc def");
        d.selections = Selections::from_vec(
            vec![Selection { anchor: 0, head: 3 }, Selection::cursor(5)],
            0,
        );
        assert!(d.delete_selections().unwrap());
        assert_eq!(text(&d), " def");
    }

    #[test]
    fn replace_many_on_a_mapped_document_is_one_pass_and_undoes() {
        let text = "ab\n".repeat(100_000);
        let mut d = Document {
            buffer: nimbus_core::MappedBuffer::from_bytes(text.clone().into_bytes()).into(),
            ..Document::default()
        };
        let items: Vec<(Range<usize>, String)> = (0..100_000)
            .map(|i| (3 * i..3 * i + 1, "X".to_owned()))
            .collect();
        let started = Instant::now();
        assert_eq!(d.replace_many(&items).unwrap(), 100_000);
        assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
        assert_eq!(d.buffer.slice(0..6), "Xb\nXb\n");
        d.undo().unwrap();
        assert_eq!(d.buffer.slice(0..d.buffer.len_bytes()), text);
        d.redo().unwrap();
        assert_eq!(d.buffer.slice(0..6), "Xb\nXb\n");
        assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
    }
}
