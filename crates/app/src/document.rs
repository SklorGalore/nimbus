//! One open file: its text, where it lives on disk, the selection, its undo
//! history, and whether it has unsaved changes.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime};

use nimbus_core::movement;
use nimbus_core::{
    AnyBuffer, Buffer, Edit, EditError, History, LineEnding, MappedBuffer, Selection, Selections,
    Source, Text,
};

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

/// Reads a file for a document: [`io::open`], or in tests another way.
#[cfg(test)]
type Opener = fn(&Path) -> Result<io::Opened, OpenError>;

/// An open document.
#[derive(Debug, Default)]
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

    /// An untitled document holding `text`, unmodified.
    #[cfg(test)]
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Self {
            buffer: nimbus_core::RopeBuffer::from_text(text).into(),
            ..Self::default()
        }
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
        Ok(doc)
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
        self.version += 1;
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
        let p = self.selections.primary();
        self.selections = Selections::single(Selection {
            anchor: movement::clamp(&self.buffer, p.anchor),
            head: movement::clamp(&self.buffer, p.head),
        });
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
        let p = self.selections.primary();
        self.selections = Selections::single(Selection {
            anchor: movement::clamp(&self.buffer, p.anchor),
            head: movement::clamp(&self.buffer, p.head),
        });
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
        self.selections.set_primary(sel);
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
            let (syntax, buffer) = (&mut self.syntax, &self.buffer);
            nimbus_core::report_sorted(&mut edits, |edit| syntax.edit(buffer, edit));
        } else {
            // Apply from the end so earlier offsets stay valid; record in
            // application order, which is what undo reverses. Each edit is
            // shown to the syntax tree against the text it applies to.
            for (range, text) in items.iter().rev() {
                match self.buffer.replace(range.clone(), text) {
                    Ok(edit) => {
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

    /// Type `text` at the cursor, replacing any selection.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn insert(&mut self, text: &str) -> Result<Edit, EditError> {
        self.replace(self.selection().range(), text)
    }

    /// Paste: like typing, but line endings are converted to the file's.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn paste(&mut self, text: &str) -> Result<Edit, EditError> {
        let unified = text.replace("\r\n", "\n");
        let converted = match self.line_ending {
            LineEnding::Lf => unified,
            LineEnding::CrLf => unified.replace('\n', "\r\n"),
        };
        // A paste is one undo step even when it is a single character.
        self.history.seal();
        let edit = self.insert(&converted);
        self.history.seal();
        edit
    }

    /// Enter: insert the line ending, then the current line's indentation,
    /// so a new line starts where the previous one did.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn insert_newline(&mut self) -> Result<Edit, EditError> {
        let sel = self.selection().range();
        let line_start = movement::line_start(&self.buffer, sel.start);
        // Only the indentation before the cursor carries over, so only the
        // line up to the cursor (and at most a few KB of it) is read.
        let head = self
            .buffer
            .slice(line_start..sel.start.min(line_start + nimbus_core::long_line::NEAR));
        let indent_len = head.len() - head.trim_start_matches([' ', '\t']).len();
        let indent = &head[..indent_len];
        let text = format!("{}{indent}", self.line_ending.as_str());
        self.insert(&text)
    }

    /// The selected text, or `None` if the selection is empty or longer
    /// than [`MAX_COPY_BYTES`].
    #[must_use]
    pub fn selected_text(&self) -> Option<String> {
        let range = self.selection().range();
        if range.is_empty() || range.len() > MAX_COPY_BYTES {
            return None;
        }
        // The raw bytes, not the display string: bytes that are not UTF-8
        // become `�` here rather than the display stand-in.
        let mut bytes = Vec::with_capacity(range.len());
        self.buffer.read_into(range, &mut bytes).ok()?;
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

    /// Select the whole document.
    pub fn select_all(&mut self) {
        self.select(0..self.buffer.len_bytes());
    }

    /// Move the cursor. With `extend` the selection grows or shrinks;
    /// without it, a selection collapses (Left and Right collapse to the
    /// selection's edge instead of moving). `page_lines` is one screenful.
    pub fn move_cursor(
        &mut self,
        motion: Motion,
        extend: bool,
        tab_width: usize,
        page_lines: usize,
    ) {
        let sel = self.selection();
        self.history.seal();
        if !extend && !sel.is_empty() && matches!(motion, Motion::Left | Motion::Right) {
            let r = sel.range();
            let pos = if motion == Motion::Left {
                r.start
            } else {
                r.end
            };
            self.set_selection(Selection::cursor(pos));
            self.preferred_column = None;
            return;
        }
        let t = &self.buffer;
        let pos = sel.head;
        let page = isize::try_from(page_lines.max(1)).unwrap_or(isize::MAX);
        let vertical =
            |lines: isize| movement::vertical(t, pos, self.preferred_column, lines, tab_width);
        let (new_pos, preferred) = match motion {
            Motion::Left => (movement::prev_grapheme(t, pos), None),
            Motion::Right => (movement::next_grapheme(t, pos), None),
            Motion::WordLeft => (movement::word_left(t, pos), None),
            Motion::WordRight => (movement::word_right(t, pos), None),
            Motion::LineStart => (movement::smart_home(t, pos), None),
            Motion::LineEnd => (movement::line_end(t, pos), None),
            Motion::DocStart => (0, None),
            Motion::DocEnd => (t.len_bytes(), None),
            Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown => {
                let lines = match motion {
                    Motion::Up => -1,
                    Motion::Down => 1,
                    Motion::PageUp => -page,
                    _ => page,
                };
                let v = vertical(lines);
                (v.pos, Some(v.preferred_col))
            }
        };
        self.set_selection(sel.moved_to(new_pos, extend));
        self.preferred_column = preferred;
    }

    /// Like [`Document::move_cursor`] with soft wrap on: Up, Down and the
    /// Page keys move by visual row; `preferred_column` is then a column
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
        let sel = self.selection();
        self.history.seal();
        let v = movement::vertical_rows(&self.buffer, w, sel.head, self.preferred_column, rows);
        self.set_selection(sel.moved_to(v.pos, extend));
        self.preferred_column = Some(v.preferred_col);
    }

    /// Backspace: delete the selection, or the grapheme before the cursor.
    /// At the start of a line this joins it to the previous one, removing a
    /// `\r\n` as one unit. Returns `None` when there is nothing to delete.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn delete_back(&mut self) -> Result<Option<Edit>, EditError> {
        let sel = self.selection();
        let range = if sel.is_empty() {
            movement::prev_grapheme(&self.buffer, sel.head)..sel.head
        } else {
            sel.range()
        };
        if range.is_empty() {
            return Ok(None);
        }
        self.replace(range, "").map(Some)
    }

    /// Delete key: delete the selection, or the grapheme after the cursor.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn delete_forward(&mut self) -> Result<Option<Edit>, EditError> {
        let sel = self.selection();
        let range = if sel.is_empty() {
            sel.head..movement::next_grapheme(&self.buffer, sel.head)
        } else {
            sel.range()
        };
        if range.is_empty() {
            return Ok(None);
        }
        self.replace(range, "").map(Some)
    }

    /// Delete the selection, or from the cursor to where `motion` would move
    /// it (a word or the line start, for Option/Ctrl+Backspace and similar).
    /// Returns `None` when there is nothing to delete.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn delete_to(&mut self, motion: Motion) -> Result<Option<Edit>, EditError> {
        let sel = self.selection();
        let range = if sel.is_empty() {
            let t = &self.buffer;
            let target = match motion {
                Motion::WordLeft => movement::word_left(t, sel.head),
                Motion::WordRight => movement::word_right(t, sel.head),
                Motion::LineStart => movement::line_start(t, sel.head),
                _ => sel.head,
            };
            sel.head.min(target)..sel.head.max(target)
        } else {
            sel.range()
        };
        if range.is_empty() {
            return Ok(None);
        }
        self.replace(range, "").map(Some)
    }

    /// Lines touched by the selection. A selection ending at the very start
    /// of a line does not include that line.
    fn selected_lines(&self) -> Range<usize> {
        let r = self.selection().range();
        let first = self.buffer.byte_to_line(r.start);
        let mut last = self.buffer.byte_to_line(r.end);
        if last > first && self.buffer.line_to_byte(last) == r.end {
            last -= 1;
        }
        first..last + 1
    }

    /// Tab with a multi-line selection: indent every selected line by one
    /// tab, as one undo step, keeping the lines selected. Returns `false`
    /// (doing nothing) when the selection is within one line; the caller
    /// then types a tab instead.
    ///
    /// # Errors
    ///
    /// See [`Document::replace_many`].
    pub fn indent_lines(&mut self) -> Result<bool, EditError> {
        let lines = self.selected_lines();
        if lines.len() < 2 {
            return Ok(false);
        }
        let starts: Vec<(Range<usize>, String)> = lines
            .clone()
            .map(|l| {
                let s = self.buffer.line_to_byte(l);
                (s..s, "\t".to_owned())
            })
            .collect();
        self.replace_many(&starts)?;
        self.select_lines(lines);
        Ok(true)
    }

    /// Shift+Tab: remove one level of indentation (a tab, or up to
    /// `tab_width` spaces) from every selected line, as one undo step.
    ///
    /// # Errors
    ///
    /// See [`Document::replace_many`].
    pub fn outdent_lines(&mut self, tab_width: usize) -> Result<(), EditError> {
        let lines = self.selected_lines();
        let ranges: Vec<(Range<usize>, String)> = lines
            .clone()
            .filter_map(|l| {
                let s = self.buffer.line_to_byte(l);
                // Only the start of the line matters.
                let end = self.buffer.line_span(s).end.min(s + tab_width + 1);
                let text = self.buffer.slice(s..end);
                let n = if text.starts_with('\t') {
                    1
                } else {
                    text.bytes()
                        .take(tab_width)
                        .take_while(|b| *b == b' ')
                        .count()
                };
                (n > 0).then(|| (s..s + n, String::new()))
            })
            .collect();
        if ranges.is_empty() {
            return Ok(());
        }
        let before = self.selection();
        self.replace_many(&ranges)?;
        if lines.len() > 1 {
            self.select_lines(lines);
        } else if let Some((removed, _)) = ranges.first() {
            // One line: keep the cursor (or selection) where it was in the
            // text, shifted left by the indentation removed before it.
            let shift = |p: usize| {
                if p >= removed.end {
                    p - removed.len()
                } else {
                    p.min(removed.start)
                }
            };
            self.select_span(shift(before.anchor), shift(before.head));
        }
        Ok(())
    }

    fn select_lines(&mut self, lines: Range<usize>) {
        let start = self.buffer.line_to_byte(lines.start);
        let end = movement::line_end(&self.buffer, self.buffer.line_to_byte(lines.end - 1));
        self.select(start..end);
    }

    /// Undo the last step. Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// Returns an [`EditError`] if the history no longer matches the text,
    /// which would be a bug.
    pub fn undo(&mut self) -> Result<bool, EditError> {
        let syntax = &mut self.syntax;
        let Some(restored) = self
            .history
            .undo(&mut self.buffer, &mut |text, edit| syntax.edit(text, edit))?
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
        let syntax = &mut self.syntax;
        let Some(restored) = self
            .history
            .redo(&mut self.buffer, &mut |text, edit| syntax.edit(text, edit))?
        else {
            return Ok(false);
        };
        self.restore(&restored);
        Ok(true)
    }

    fn restore(&mut self, restored: &Selections) {
        let p = restored.primary();
        self.selections = Selections::single(Selection {
            anchor: movement::clamp(&self.buffer, p.anchor),
            head: movement::clamp(&self.buffer, p.head),
        });
        self.preferred_column = None;
        self.version += 1;
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
        assert!(d.delete_back().unwrap().is_none());
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
