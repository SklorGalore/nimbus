//! Find in files: the search workers and the results they stream back.
//!
//! The walk and the search share threads. `ignore`'s parallel walker visits
//! files on several threads, and each thread searches the files it visits
//! itself, so no file list is ever built and no thread sits idle feeding
//! another. Each thread reads a file in blocks through
//! [`LineMatcher`] and sends what it found to the event loop in batches:
//! at most once per [`BATCH_TIME`] or [`BATCH_HITS`], whichever comes
//! first, so a million hits do not wake the loop a million times.
//!
//! Memory stays bounded whatever the search finds: hits stop at
//! [`MAX_HITS`], each one keeps a line number, a byte range and a preview
//! of at most [`PREVIEW_BYTES`], and a file's path is stored once however
//! many hits it has.
//!
//! A newer search cancels an older one through the shared [`Generation`]
//! counter, as the in-editor find does: a worker checks it before every
//! file and stops when it has moved on.

use std::collections::HashMap;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::{DirEntry, WalkBuilder, WalkState};
use nimbus_core::search::{Generation, LineHit, LineMatcher, MultiLineMatcher, Query, spans_lines};
use nimbus_core::{Snapshot, Text};
use regex::bytes::Regex;

/// Hits kept in all, over every file; past this the search stops and the
/// panel says to narrow it (DESIGN.md §9).
pub const MAX_HITS: usize = 10_000;

/// Files larger than this are skipped and counted; the in-editor find
/// handles them one at a time.
pub const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// How much of a file is read at once.
pub const BLOCK: usize = 4 * 1024 * 1024;

/// Longest preview kept for one hit, in bytes.
pub const PREVIEW_BYTES: usize = 200;

/// Bytes at the start of a file checked for a NUL, which marks it binary.
const BINARY_PROBE: usize = 8 * 1024;

/// A worker sends what it has after this many hits, or after
/// [`BATCH_TIME`], whichever comes first.
const BATCH_HITS: usize = 64;

/// The longest a found hit waits before it is sent.
const BATCH_TIME: Duration = Duration::from_millis(16);

/// What to search for and where.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Params {
    /// The pattern and its options.
    pub query: Query,
    /// The folders searched: the workspace's roots, or the folders typed in
    /// the include field (M19). Paths in results are relative to the root
    /// they are in, prefixed with its name when there are several.
    pub roots: Vec<PathBuf>,
    /// Comma-separated globs a file must match, or empty for all.
    pub include: String,
    /// Comma-separated globs for files and folders left out.
    pub exclude: String,
    /// Search `.gitignore`d files too.
    pub show_ignored: bool,
}

/// The include and exclude globs, compiled.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
}

/// Compile a comma-separated list of globs; `None` when it is empty.
fn glob_set(list: &str) -> Result<Option<GlobSet>, String> {
    let mut builder = GlobSetBuilder::new();
    let mut any = false;
    for pattern in list.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let glob = Glob::new(pattern).map_err(|e| format!("{pattern}: {}", e.kind()))?;
        builder.add(glob);
        any = true;
    }
    if !any {
        return Ok(None);
    }
    builder.build().map(Some).map_err(|e| e.to_string())
}

impl Filter {
    /// Compile both lists.
    ///
    /// # Errors
    ///
    /// Returns a message naming the pattern that does not parse.
    pub fn new(include: &str, exclude: &str) -> Result<Self, String> {
        Ok(Self {
            include: glob_set(include)?,
            exclude: glob_set(exclude)?,
        })
    }

    /// Whether a folder at `rel` (relative to the root) is left out with
    /// everything in it.
    #[must_use]
    pub fn skips_dir(&self, rel: &Path) -> bool {
        self.exclude.as_ref().is_some_and(|g| g.is_match(rel))
    }

    /// Whether a file at `rel` is searched. A glob matches the relative
    /// path, or just the file's name, so `*.rs` and `src/*.rs` both work.
    #[must_use]
    pub fn takes_file(&self, rel: &Path) -> bool {
        let name = rel.file_name().map(Path::new);
        let matches = |g: &GlobSet| g.is_match(rel) || name.is_some_and(|n| g.is_match(n));
        if self.exclude.as_ref().is_some_and(matches) {
            return false;
        }
        self.include.as_ref().is_none_or(matches)
    }
}

/// One hit as a worker found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawHit {
    /// Line number, from 0.
    pub line: usize,
    /// The match, as byte offsets in the file as searched.
    pub range: Range<usize>,
    /// The line, or the part of it around the match.
    pub preview: String,
    /// The match within `preview`.
    pub mark: Range<usize>,
    /// The line the match ends on (a multi-line match, M19), from 0.
    pub last_line: usize,
    /// The match was longer than the multi-line window and is cut short.
    pub truncated: bool,
}

/// The hits of one file in a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileBatch {
    /// The file's path, as the walker found it.
    pub path: PathBuf,
    /// Searched in an open tab's unsaved text rather than on disk.
    pub dirty: bool,
    /// Hits, in file order.
    pub hits: Vec<RawHit>,
}

/// What a worker sends to the event loop.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Batch {
    /// The search this belongs to; stale ones are dropped.
    pub generation: u64,
    /// Hits by file; one file can appear in several batches.
    pub items: Vec<FileBatch>,
    /// Files searched since the last batch from this worker.
    pub searched: usize,
    /// Files skipped since then: binary, or over [`MAX_FILE_BYTES`].
    pub skipped: usize,
    /// Files that could not be read since then.
    pub errors: usize,
    /// The hit cap was reached: the search stopped early.
    pub capped: bool,
    /// The last batch of the search.
    pub done: bool,
}

/// One hit in the results, with its preview in [`Results::previews`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Line number, from 0.
    pub line: usize,
    /// The match, as byte offsets in the file as searched.
    pub range: Range<usize>,
    /// The preview's bytes in [`Results::previews`].
    pub preview: Range<usize>,
    /// The match within the preview.
    pub mark: Range<usize>,
    /// The line the match ends on: `line` unless it spans lines.
    pub last_line: usize,
    /// Cut short at the multi-line window: shown, never replaced.
    pub truncated: bool,
    /// Chosen for Replace All.
    pub ticked: bool,
}

/// One file with hits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHits {
    /// Full path.
    pub path: PathBuf,
    /// Path relative to the root, for display.
    pub rel: String,
    /// Searched in an open tab's unsaved text.
    pub dirty: bool,
    /// Hits in file order.
    pub hits: Vec<Hit>,
}

/// Everything a search has found so far.
#[derive(Debug, Clone, Default)]
pub struct Results {
    /// Files with hits, in the order their first hit arrived.
    pub files: Vec<FileHits>,
    /// Every preview, back to back.
    pub previews: String,
    /// Hits in all.
    pub hits: usize,
    /// Files searched so far.
    pub searched: usize,
    /// Files skipped (binary, or too large).
    pub skipped: usize,
    /// Files that could not be read.
    pub errors: usize,
    /// The search reached the end of the tree.
    pub done: bool,
    /// The search stopped at [`MAX_HITS`].
    pub capped: bool,
    /// The search these belong to.
    pub generation: u64,
    /// The folders searched, to make paths relative.
    roots: Vec<PathBuf>,
    /// Index into `files` by path.
    index: HashMap<PathBuf, usize>,
}

impl Results {
    /// Empty results for the search `generation` under `roots`.
    #[must_use]
    pub fn new(generation: u64, roots: Vec<PathBuf>) -> Self {
        Self {
            generation,
            roots,
            ..Self::default()
        }
    }

    /// Take a batch. Returns `false` if it was from an older search and
    /// was ignored.
    pub fn receive(&mut self, batch: Batch) -> bool {
        if batch.generation != self.generation {
            return false;
        }
        self.searched += batch.searched;
        self.skipped += batch.skipped;
        self.errors += batch.errors;
        self.done |= batch.done;
        self.capped |= batch.capped;
        for item in batch.items {
            let i = if let Some(&i) = self.index.get(&item.path) {
                i
            } else {
                let rel = crate::tree::shown_path(&self.roots, &item.path)
                    .unwrap_or_else(|| item.path.clone())
                    .to_string_lossy()
                    .into_owned();
                self.files.push(FileHits {
                    path: item.path.clone(),
                    rel,
                    dirty: item.dirty,
                    hits: Vec::new(),
                });
                self.index.insert(item.path, self.files.len() - 1);
                self.files.len() - 1
            };
            for hit in item.hits {
                let start = self.previews.len();
                self.previews.push_str(&hit.preview);
                self.files[i].hits.push(Hit {
                    line: hit.line,
                    range: hit.range,
                    preview: start..self.previews.len(),
                    mark: hit.mark,
                    last_line: hit.last_line,
                    truncated: hit.truncated,
                    ticked: !hit.truncated,
                });
                self.hits += 1;
            }
        }
        true
    }

    /// The preview text of `hit`.
    #[must_use]
    pub fn preview(&self, hit: &Hit) -> &str {
        self.previews.get(hit.preview.clone()).unwrap_or("")
    }

    /// Hits that are ticked, by file: `(file index, hit indices)`.
    #[must_use]
    pub fn ticked(&self) -> Vec<(usize, Vec<usize>)> {
        self.files
            .iter()
            .enumerate()
            .filter_map(|(i, f)| {
                let hits: Vec<usize> = f
                    .hits
                    .iter()
                    .enumerate()
                    .filter(|(_, h)| h.ticked)
                    .map(|(j, _)| j)
                    .collect();
                (!hits.is_empty()).then_some((i, hits))
            })
            .collect()
    }
}

/// The preview of a line around a match: the whole line when it fits in
/// [`PREVIEW_BYTES`], else that much of it with the match near the start.
/// Returns the text and the match's range within it.
fn preview_of(line: &[u8], line_start: usize, range: &Range<usize>) -> (String, Range<usize>) {
    let ms = range.start.saturating_sub(line_start).min(line.len());
    let me = range.end.saturating_sub(line_start).clamp(ms, line.len());
    let (mut a, mut b) = (0, line.len());
    if line.len() > PREVIEW_BYTES {
        // Some context before the match, the rest after.
        a = ms.saturating_sub(PREVIEW_BYTES / 4);
        b = (a + PREVIEW_BYTES).min(line.len());
        a = b.saturating_sub(PREVIEW_BYTES);
        // Stay on character boundaries (continuation bytes are 10xxxxxx).
        while a > 0 && a < line.len() && line[a] & 0xC0 == 0x80 {
            a -= 1;
        }
        while b < line.len() && line[b] & 0xC0 == 0x80 {
            b += 1;
        }
    }
    let (ms, me) = (ms.clamp(a, b), me.clamp(a, b));
    // Lossy decoding can change lengths, so measure each part.
    let before = String::from_utf8_lossy(&line[a..ms]);
    let matched = String::from_utf8_lossy(&line[ms..me]);
    let after = String::from_utf8_lossy(&line[me..b]);
    let mark = before.len()..before.len() + matched.len();
    let mut text = String::with_capacity(before.len() + matched.len() + after.len());
    text.push_str(&before);
    text.push_str(&matched);
    text.push_str(&after);
    (text, mark)
}

/// The line-by-line matcher, or the multi-line one for a pattern that
/// names a line break (M19). Both take a file in blocks.
enum Matcher<'r> {
    Line(LineMatcher<'r>),
    Multi(MultiLineMatcher<'r>),
}

impl<'r> Matcher<'r> {
    fn new(re: &'r Regex, multi: bool) -> Self {
        if multi {
            Self::Multi(MultiLineMatcher::new(re))
        } else {
            Self::Line(LineMatcher::new(re))
        }
    }

    fn feed(&mut self, block: &[u8], found: &mut dyn FnMut(&LineHit<'_>) -> bool) -> bool {
        match self {
            Self::Line(m) => m.feed(block, found),
            Self::Multi(m) => m.feed(block, found),
        }
    }

    fn finish(&mut self, found: &mut dyn FnMut(&LineHit<'_>) -> bool) -> bool {
        match self {
            Self::Line(m) => m.finish(found),
            Self::Multi(m) => m.finish(found),
        }
    }
}

/// Snapshots of open tabs with unsaved edits, by canonical path: searched
/// instead of the file on disk.
pub type Snapshots = HashMap<PathBuf, Arc<dyn Snapshot>>;

/// What every worker thread shares.
struct Shared {
    re: Regex,
    /// The pattern can match across lines.
    multi: bool,
    filter: Filter,
    roots: Vec<PathBuf>,
    snapshots: Snapshots,
    generation: Arc<Generation>,
    /// The generation this search runs as.
    ours: u64,
    /// Hits sent by every worker together, for the cap.
    hits: AtomicUsize,
    capped: AtomicBool,
}

impl Shared {
    fn cancelled(&self) -> bool {
        self.generation.current() != self.ours
    }
}

/// One walker thread's state: its reusable buffers and the batch it is
/// filling.
struct Worker<S: Fn(Batch) -> bool> {
    shared: Arc<Shared>,
    send: S,
    block: Vec<u8>,
    pending: Batch,
    pending_hits: usize,
    last_sent: Instant,
}

impl<S: Fn(Batch) -> bool> Worker<S> {
    fn new(shared: Arc<Shared>, send: S) -> Self {
        Self {
            pending: Batch {
                generation: shared.ours,
                ..Batch::default()
            },
            shared,
            send,
            block: Vec::new(),
            pending_hits: 0,
            last_sent: Instant::now(),
        }
    }

    fn flush(&mut self) {
        let batch = std::mem::replace(
            &mut self.pending,
            Batch {
                generation: self.shared.ours,
                ..Batch::default()
            },
        );
        self.pending_hits = 0;
        self.last_sent = Instant::now();
        if !batch.items.is_empty() || batch.searched + batch.skipped + batch.errors > 0 {
            (self.send)(batch);
        }
    }

    fn flush_if_due(&mut self) {
        if self.pending_hits >= BATCH_HITS || self.last_sent.elapsed() >= BATCH_TIME {
            self.flush();
        }
    }

    fn visit(&mut self, entry: Result<DirEntry, ignore::Error>) -> WalkState {
        if self.shared.cancelled() {
            return WalkState::Quit;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                log::debug!("find in files: {e}");
                self.pending.errors += 1;
                return WalkState::Continue;
            }
        };
        if entry.depth() == 0 {
            return WalkState::Continue;
        }
        let path = entry.path();
        let Some(rel) = self
            .shared
            .roots
            .iter()
            .find_map(|r| path.strip_prefix(r).ok())
        else {
            return WalkState::Continue;
        };
        let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
        if is_dir {
            let skip = entry.file_name() == ".git" || self.shared.filter.skips_dir(rel);
            return if skip {
                WalkState::Skip
            } else {
                WalkState::Continue
            };
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) || !self.shared.filter.takes_file(rel) {
            return WalkState::Continue;
        }
        let state = self.search_one(path);
        self.flush_if_due();
        state
    }

    /// Search one file, in its tab's unsaved text if it has one.
    fn search_one(&mut self, path: &Path) -> WalkState {
        let shared = Arc::clone(&self.shared);
        let mut matcher = Matcher::new(&shared.re, shared.multi);
        let mut hits = Vec::new();
        let mut keep = |h: &LineHit<'_>| {
            // Count against the cap as found, so every worker stops
            // together once it is reached.
            if shared.hits.fetch_add(1, Ordering::Relaxed) >= MAX_HITS {
                shared.capped.store(true, Ordering::Relaxed);
                return false;
            }
            let (preview, mark) = preview_of(h.text, h.line_start, &h.range);
            hits.push(RawHit {
                line: h.line,
                range: h.range.clone(),
                preview,
                mark,
                last_line: h.last_line,
                truncated: h.truncated,
            });
            true
        };
        let (finished, dirty) = if let Some(snapshot) = shared.snapshots.get(path) {
            (
                feed_text(&**snapshot, &mut self.block, &mut matcher, &mut keep),
                true,
            )
        } else {
            match feed_file(path, &mut self.block, &mut matcher, &mut keep) {
                Ok(Fed::Done(finished)) => (finished, false),
                Ok(Fed::Skipped) => {
                    self.pending.skipped += 1;
                    return WalkState::Continue;
                }
                Err(e) => {
                    log::debug!("find in files: {}: {e}", path.display());
                    self.pending.errors += 1;
                    return WalkState::Continue;
                }
            }
        };
        self.pending.searched += 1;
        if !hits.is_empty() {
            self.pending_hits += hits.len();
            self.pending.items.push(FileBatch {
                path: path.to_path_buf(),
                dirty,
                hits,
            });
        }
        if finished {
            WalkState::Continue
        } else {
            // The cap: tell the other threads too.
            WalkState::Quit
        }
    }
}

impl<S: Fn(Batch) -> bool> Drop for Worker<S> {
    fn drop(&mut self) {
        self.flush();
    }
}

/// How reading a file for the search ended.
enum Fed {
    /// Searched; `true` if to the end, `false` if the matcher stopped.
    Done(bool),
    /// Binary or too large: not searched.
    Skipped,
}

/// Feed `path` to `matcher` in blocks.
fn feed_file(
    path: &Path,
    block: &mut Vec<u8>,
    matcher: &mut Matcher<'_>,
    found: &mut dyn FnMut(&LineHit<'_>) -> bool,
) -> std::io::Result<Fed> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len > MAX_FILE_BYTES {
        return Ok(Fed::Skipped);
    }
    let mut first = true;
    loop {
        block.clear();
        // `take` caps one read at a block; `read_to_end` loops over short
        // reads for us.
        let read = file.by_ref().take(BLOCK as u64).read_to_end(block)?;
        if first {
            first = false;
            let probe = &block[..block.len().min(BINARY_PROBE)];
            if probe.contains(&0) {
                return Ok(Fed::Skipped);
            }
        }
        if read == 0 {
            break;
        }
        if !matcher.feed(block, found) {
            return Ok(Fed::Done(false));
        }
        if read < BLOCK {
            break;
        }
    }
    Ok(Fed::Done(matcher.finish(found)))
}

/// Feed an open tab's text to `matcher` in blocks.
fn feed_text(
    text: &dyn Text,
    block: &mut Vec<u8>,
    matcher: &mut Matcher<'_>,
    found: &mut dyn FnMut(&LineHit<'_>) -> bool,
) -> bool {
    let len = text.len_bytes();
    let mut pos = 0;
    while pos < len {
        let end = pos.saturating_add(BLOCK).min(len);
        block.clear();
        if text.read_into(pos..end, block).is_err() {
            break;
        }
        if !matcher.feed(block, found) {
            return false;
        }
        pos = end;
    }
    matcher.finish(found)
}

/// Start a search on worker threads. `generation` is bumped, and the
/// search runs as the new value, which is returned: results carry it, and
/// bumping it again stops the search. `send` delivers each batch to the
/// event loop and returns `false` when the loop is gone.
///
/// # Errors
///
/// Returns a message when the pattern or a glob does not compile; nothing
/// is started then.
pub fn start(
    params: &Params,
    snapshots: Snapshots,
    generation: &Arc<Generation>,
    send: impl Fn(Batch) -> bool + Send + Sync + 'static,
) -> Result<u64, String> {
    let re = params
        .query
        .compile()
        .map_err(|e| e.0)?
        .ok_or_else(|| "nothing to search for".to_owned())?;
    let filter = Filter::new(&params.include, &params.exclude)?;
    let Some(first) = params.roots.first().cloned() else {
        return Err("no folder to search".to_owned());
    };
    if let Some(r) = params.roots.iter().find(|r| !r.is_dir()) {
        return Err(format!("{} is not a folder", r.display()));
    }
    let ours = generation.bump();
    let shared = Arc::new(Shared {
        re,
        multi: spans_lines(&params.query),
        filter,
        roots: params.roots.clone(),
        snapshots,
        generation: Arc::clone(generation),
        ours,
        hits: AtomicUsize::new(0),
        capped: AtomicBool::new(false),
    });
    let send = Arc::new(send);
    let show_ignored = params.show_ignored;
    let spawned = std::thread::Builder::new()
        .name("nimbus-search".to_owned())
        .spawn(move || {
            // One core stays free for the main thread.
            let threads = std::thread::available_parallelism()
                .map_or(2, |n| n.get().saturating_sub(1))
                .max(1);
            // One walk over every root: the walker takes several starting
            // folders, and each keeps its own `.gitignore`.
            let mut builder = WalkBuilder::new(&first);
            for r in shared.roots.iter().skip(1) {
                builder.add(r);
            }
            let walker = builder
                .hidden(false)
                .git_ignore(!show_ignored)
                .git_exclude(!show_ignored)
                .git_global(!show_ignored)
                .ignore(!show_ignored)
                .parents(true)
                .threads(threads)
                .build_parallel();
            walker.run(|| {
                let send = Arc::clone(&send);
                let mut worker = Worker::new(Arc::clone(&shared), move |b| send(b));
                Box::new(move |entry| worker.visit(entry))
            });
            if !shared.cancelled() {
                send(Batch {
                    generation: ours,
                    capped: shared.capped.load(Ordering::Relaxed),
                    done: true,
                    ..Batch::default()
                });
            }
        });
    spawned.map_err(|e| format!("could not start the search: {e}"))?;
    Ok(ours)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn tree() -> tempfile_dir::Dir {
        let dir = tempfile_dir::Dir::new("find-files");
        dir.write("a.txt", "needle one\nhay\nneedle two\n");
        dir.write("src/b.rs", "fn needle() {}\n");
        dir.write("src/deep/c.rs", "// no match here\n");
        dir.write("bin.dat", "needle\0\0\0binary");
        dir.write(".gitignore", "ignored.txt\n");
        dir.write("ignored.txt", "needle in an ignored file\n");
        // `.gitignore` counts only inside a repository, as in the sidebar.
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        dir
    }

    /// Run a search to the end and return its results.
    fn search(params: &Params, snapshots: Snapshots) -> Results {
        let generation = Arc::new(Generation::default());
        let (tx, rx) = mpsc::channel();
        let g = start(params, snapshots, &generation, move |b| tx.send(b).is_ok()).unwrap();
        let mut results = Results::new(g, params.roots.clone());
        while !results.done {
            assert!(results.receive(rx.recv().unwrap()));
        }
        results
    }

    fn params(root: &Path, pattern: &str) -> Params {
        Params {
            query: Query {
                pattern: pattern.into(),
                ..Query::default()
            },
            roots: vec![root.to_path_buf()],
            ..Params::default()
        }
    }

    /// Relative paths with `/` as the separator, whatever the OS shows.
    fn rels(r: &Results) -> Vec<String> {
        let mut v: Vec<String> = r.files.iter().map(|f| f.rel.replace('\\', "/")).collect();
        v.sort_unstable();
        v
    }

    /// Measure a search over a real tree: `NIMBUS_BENCH_ROOT` names the
    /// folder and `NIMBUS_BENCH_PATTERN` the pattern (default `e`). Prints
    /// the time to the first batch, the time to the end, the counts, and
    /// an estimate of what the results occupy in memory (M10 task 9).
    #[test]
    #[ignore = "benchmark; run explicitly in release mode"]
    fn search_tree_benchmark() {
        let Ok(root) = std::env::var("NIMBUS_BENCH_ROOT") else {
            eprintln!("set NIMBUS_BENCH_ROOT to the folder to search");
            return;
        };
        let pattern = std::env::var("NIMBUS_BENCH_PATTERN").unwrap_or_else(|_| "e".into());
        // A relative root is taken from the crate folder, where cargo runs
        // tests; the app always searches under a canonical root.
        let root = std::fs::canonicalize(&root).unwrap();
        let p = params(&root, &pattern);
        let generation = Arc::new(Generation::default());
        let (tx, rx) = mpsc::channel();
        let started = Instant::now();
        let g = start(&p, Snapshots::new(), &generation, move |b| {
            tx.send(b).is_ok()
        })
        .unwrap();
        let mut results = Results::new(g, p.roots.clone());
        let mut first = None;
        while !results.done {
            assert!(results.receive(rx.recv().unwrap()));
            if first.is_none() && results.hits > 0 {
                first = Some(started.elapsed());
            }
        }
        let total = started.elapsed();
        let bytes = results.previews.capacity()
            + results.files.capacity() * std::mem::size_of::<FileHits>()
            + results
                .files
                .iter()
                .map(|f| {
                    f.path.as_os_str().len()
                        + f.rel.capacity()
                        + f.hits.capacity() * std::mem::size_of::<Hit>()
                })
                .sum::<usize>()
            + results.index.capacity() * (std::mem::size_of::<(PathBuf, usize)>() + 16);
        eprintln!(
            "pattern {pattern:?} under {}: first batch after {:?}, done after {total:?}; \
             {} hits in {} files, {} searched, {} skipped, {} errors, capped {}; \
             results hold about {} KB",
            root.display(),
            first.unwrap_or(total),
            results.hits,
            results.files.len(),
            results.searched,
            results.skipped,
            results.errors,
            results.capped,
            bytes / 1024
        );
    }

    #[test]
    fn searches_a_tree_with_line_numbers_and_previews() {
        let dir = tree();
        let r = search(&params(dir.path(), "needle"), Snapshots::new());
        assert_eq!(
            rels(&r),
            vec!["a.txt", "src/b.rs"],
            "ignored and binary left out"
        );
        assert_eq!(r.hits, 3);
        assert_eq!(r.skipped, 1, "the binary file");
        let a = r.files.iter().find(|f| f.rel == "a.txt").unwrap();
        assert_eq!(a.hits[1].line, 2);
        assert_eq!(a.hits[1].range, 15..21);
        assert_eq!(r.preview(&a.hits[1]), "needle two");
        assert_eq!(a.hits[1].mark, 0..6);
        assert!(!r.capped);
    }

    #[test]
    fn globs_and_ignored_files() {
        let dir = tree();
        let mut p = params(dir.path(), "needle");
        p.include = "*.rs".into();
        assert_eq!(rels(&search(&p, Snapshots::new())), vec!["src/b.rs"]);
        p.include = String::new();
        p.exclude = "src".into();
        assert_eq!(rels(&search(&p, Snapshots::new())), vec!["a.txt"]);
        p.exclude = String::new();
        p.show_ignored = true;
        let r = search(&p, Snapshots::new());
        assert!(rels(&r).iter().any(|r| r == "ignored.txt"));
        let mut bad = params(dir.path(), "needle");
        bad.include = "[".into();
        let g = Arc::new(Generation::default());
        assert!(start(&bad, Snapshots::new(), &g, |_| true).is_err());
    }

    #[test]
    fn searches_every_root_and_names_them() {
        let first = tree();
        let second = tempfile_dir::Dir::new("find-files-second");
        second.write("lib.rs", "needle in the second root\n");
        second.write("skip/x.rs", "needle\n");
        let mut p = params(first.path(), "needle");
        p.roots.push(second.path().to_path_buf());
        p.exclude = "skip".into();
        let r = search(&p, Snapshots::new());
        let name = |d: &Path| d.file_name().unwrap().to_string_lossy().into_owned();
        let mut want = vec![
            format!("{}/a.txt", name(first.path())),
            format!("{}/lib.rs", name(second.path())),
            format!("{}/src/b.rs", name(first.path())),
        ];
        want.sort_unstable();
        assert_eq!(rels(&r), want, "excludes apply within each root");
        p.roots.push(first.path().join("no-such-folder"));
        let g = Arc::new(Generation::default());
        assert!(start(&p, Snapshots::new(), &g, |_| true).is_err());
        p.roots.clear();
        assert!(start(&p, Snapshots::new(), &g, |_| true).is_err());
    }

    #[test]
    fn regex_with_a_line_break_matches_across_lines() {
        let dir = tree();
        let mut p = params(dir.path(), r"needle one\nhay");
        p.query.regex = true;
        let r = search(&p, Snapshots::new());
        assert_eq!(rels(&r), vec!["a.txt"]);
        let h = &r.files[0].hits[0];
        assert_eq!(
            (h.line, h.last_line, h.truncated, h.ticked),
            (0, 1, false, true)
        );
        assert_eq!(r.preview(h), "needle one");
        // Without regex, `\n` is two characters to find: no hits.
        p.query.regex = false;
        assert_eq!(search(&p, Snapshots::new()).hits, 0);
    }

    #[test]
    fn unsaved_tabs_are_searched_in_their_text() {
        let dir = tree();
        let path = std::fs::canonicalize(dir.path().join("a.txt")).unwrap();
        let text = nimbus_core::RopeBuffer::from_text("changed: needle\n");
        let snapshot: Arc<dyn Snapshot> = Arc::from(nimbus_core::Buffer::snapshot(&text));
        let mut snapshots = Snapshots::new();
        snapshots.insert(path, snapshot);
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let r = search(&params(&root, "needle"), snapshots);
        let a = r.files.iter().find(|f| f.rel == "a.txt").unwrap();
        assert!(a.dirty);
        assert_eq!(a.hits.len(), 1);
        assert_eq!(r.preview(&a.hits[0]), "changed: needle");
    }

    #[test]
    fn stops_at_the_cap_and_when_cancelled() {
        let dir = tempfile_dir::Dir::new("find-files-cap");
        let line = "x\n".repeat(MAX_HITS);
        dir.write("many.txt", &line);
        dir.write("more.txt", &line);
        let r = search(&params(dir.path(), "x"), Snapshots::new());
        assert!(r.capped);
        assert_eq!(r.hits, MAX_HITS);
        // Cancelled: the final batch never comes.
        let generation = Arc::new(Generation::default());
        let (tx, rx) = mpsc::channel();
        let p = params(dir.path(), "x");
        let g = start(&p, Snapshots::new(), &generation, move |b| {
            tx.send(b).is_ok()
        })
        .unwrap();
        generation.bump();
        let batches: Vec<Batch> = rx.iter().collect();
        assert!(!batches.iter().any(|b| b.done && b.generation == g));
    }

    #[test]
    fn long_lines_preview_around_the_match() {
        let line = format!("{}needle{}", "a".repeat(500), "é".repeat(300));
        let (text, mark) = preview_of(line.as_bytes(), 0, &(500..506));
        assert!(text.len() <= PREVIEW_BYTES + 3);
        assert_eq!(&text[mark.clone()], "needle");
        assert!(text.is_char_boundary(mark.end));
        let (text, mark) = preview_of(b"short", 0, &(1..3));
        assert_eq!((text.as_str(), mark), ("short", 1..3));
        let (text, mark) = preview_of(b"bad \xff byte", 0, &(4..5));
        assert_eq!(&text[mark], "\u{FFFD}");
    }

    /// A folder of files under the system temp directory, removed on drop.
    mod tempfile_dir {
        use std::path::{Path, PathBuf};

        pub struct Dir(PathBuf);

        impl Dir {
            pub fn new(name: &str) -> Self {
                let unique = format!(
                    "nimbus-{name}-{}-{:?}",
                    std::process::id(),
                    std::thread::current().id()
                );
                let path = std::env::temp_dir().join(unique);
                let _ = std::fs::remove_dir_all(&path);
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }

            pub fn write(&self, rel: &str, text: &str) {
                let p = self.0.join(rel);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, text).unwrap();
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
