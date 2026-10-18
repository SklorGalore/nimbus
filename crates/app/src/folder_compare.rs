//! Folder compare: walk two folders, pair their files by relative path,
//! and stream what differs to the sidebar.
//!
//! A folder diff is a diff of two sorted lists of paths. Each side is
//! walked by one single-threaded `ignore` walker with its children sorted
//! by name. A depth-first walk that visits siblings in name order yields
//! paths in exactly the order [`Path`]'s `Ord` puts them, because that
//! `Ord` compares paths component by component (`a/b` sorts before
//! `a-b` and `a.txt`, since the component `a` is shorter). So the two
//! walks can be merged as they run, like the merge step of merge sort,
//! without collecting either list first: whichever head is smaller is
//! only on its side; equal heads are a pair.
//!
//! The threads, all started by [`start`]:
//!
//! - two walkers, one per root, each feeding a bounded channel;
//! - one merger, which pairs the two streams. A path on one side only, or
//!   a pair whose sizes differ, is decided at once. A same-size pair needs
//!   its contents read, so it goes to a job queue;
//! - a few content workers (one per core, less one for the main thread),
//!   each reading pairs in [`BLOCK`]-sized blocks into two buffers it
//!   allocated once, and stopping at the first block that differs.
//!
//! The merger and every worker batch what they found and send it to the
//! event loop at most once per [`BATCH_TIME`] or [`BATCH_ENTRIES`], as
//! find in files does, and a final batch marked `done` follows once all of
//! them have finished. A newer [`start`] bumps the shared [`Generation`],
//! and every thread checks it before each file and stops.

use std::cmp::Ordering;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ignore::WalkBuilder;
use nimbus_core::search::Generation;

use crate::find_files::{BATCH_TIME, BLOCK, Filter};

/// A merger or worker sends what it has after this many rows, or after
/// [`BATCH_TIME`], whichever comes first.
const BATCH_ENTRIES: usize = 256;

/// How many walked files may wait in each walker's channel. Bounded so a
/// fast walk of a huge tree cannot outrun the merger without limit, and
/// large enough that the two walkers rarely wait on each other.
const WALK_QUEUE: usize = 4096;

/// How many same-size pairs may wait for a content worker.
const JOB_QUEUE: usize = 1024;

/// The two folders to compare and which files count.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Params {
    /// The left folder; [`Results::pair`] joins relative paths onto it.
    pub left: PathBuf,
    /// The right folder.
    pub right: PathBuf,
    /// Comma-separated globs a file must match, or empty for all (the same
    /// syntax as find in files).
    pub include: String,
    /// Comma-separated globs for files and folders left out.
    pub exclude: String,
    /// Compare `.gitignore`d files too.
    pub show_ignored: bool,
}

/// How the two sides of one relative path compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The file exists only in the left folder.
    OnlyLeft,
    /// The file exists only in the right folder.
    OnlyRight,
    /// Both folders have it and the bytes differ.
    Differs,
    /// Both folders have it with the same bytes. Counted, never sent as a
    /// row: the list shows only differences.
    Same,
    /// One side could not be read; the message says why. The comparison
    /// carries on with the other files.
    Error(String),
}

/// One row of the listing: a relative path and how its sides compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The path below both roots, as the walker found it.
    pub rel: PathBuf,
    /// How the two sides compare; never [`Status::Same`] in a row.
    pub status: Status,
}

/// What the comparison threads send to the event loop.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Batch {
    /// The comparison this belongs to; stale batches are dropped.
    pub generation: u64,
    /// Rows that differ, in no promised order (workers finish out of
    /// order); [`Results::receive`] sorts them in.
    pub entries: Vec<Entry>,
    /// Pairs found identical since the sender's last batch.
    pub same: usize,
    /// The last batch of the comparison.
    pub done: bool,
}

/// Everything a folder comparison has found so far: the model the
/// sidebar's Compare mode draws, one flat row per differing file.
#[derive(Debug, Clone, Default)]
pub struct Results {
    /// The comparison these belong to.
    pub generation: u64,
    /// The left root.
    pub left: PathBuf,
    /// The right root.
    pub right: PathBuf,
    /// Files only in the left folder.
    pub only_left: usize,
    /// Files only in the right folder.
    pub only_right: usize,
    /// Pairs whose contents differ.
    pub differs: usize,
    /// Pairs with identical contents (not listed).
    pub same: usize,
    /// Pairs that could not be read.
    pub errors: usize,
    /// Both walks and every content comparison have finished.
    pub done: bool,
    /// The rows, sorted by relative path.
    entries: Vec<Entry>,
}

impl Results {
    /// Empty results for the comparison `generation` of `left` and `right`.
    #[must_use]
    pub fn new(generation: u64, left: PathBuf, right: PathBuf) -> Self {
        Self {
            generation,
            left,
            right,
            ..Self::default()
        }
    }

    /// Take a batch. Returns `false` if it was from an older comparison
    /// and was ignored.
    ///
    /// Rows are kept sorted by path. They arrive mostly in order (the
    /// merger emits them in order; only content workers lag a little), so
    /// most land at the end with one comparison and the rest need a binary
    /// search and a short shift.
    pub fn receive(&mut self, batch: Batch) -> bool {
        if batch.generation != self.generation {
            return false;
        }
        self.same += batch.same;
        self.done |= batch.done;
        for entry in batch.entries {
            match entry.status {
                Status::OnlyLeft => self.only_left += 1,
                Status::OnlyRight => self.only_right += 1,
                Status::Differs => self.differs += 1,
                Status::Error(_) => self.errors += 1,
                Status::Same => {
                    self.same += 1;
                    continue;
                }
            }
            let at_end = self.entries.last().is_none_or(|last| last.rel < entry.rel);
            if at_end {
                self.entries.push(entry);
            } else {
                let i = self
                    .entries
                    .binary_search_by(|e| e.rel.cmp(&entry.rel))
                    .unwrap_or_else(|i| i);
                self.entries.insert(i, entry);
            }
        }
        true
    }

    /// The rows so far, sorted by relative path.
    #[must_use]
    pub fn rows(&self) -> &[Entry] {
        &self.entries
    }

    /// The absolute left and right paths of row `i`; `None` for a side the
    /// file is missing from, or both for a row that does not exist.
    #[must_use]
    pub fn pair(&self, i: usize) -> (Option<PathBuf>, Option<PathBuf>) {
        let Some(entry) = self.entries.get(i) else {
            return (None, None);
        };
        let left = (entry.status != Status::OnlyRight).then(|| self.left.join(&entry.rel));
        let right = (entry.status != Status::OnlyLeft).then(|| self.right.join(&entry.rel));
        (left, right)
    }

    /// The next row after `i` that can be opened in the compare view
    /// (rows that failed to read are skipped), for "Next Change" at the
    /// end of a file.
    #[must_use]
    pub fn next_after(&self, i: usize) -> Option<usize> {
        let start = i.checked_add(1)?;
        (start..self.entries.len()).find(|&j| self.openable(j))
    }

    /// The previous openable row before `i`.
    #[must_use]
    pub fn prev_before(&self, i: usize) -> Option<usize> {
        (0..i.min(self.entries.len()))
            .rev()
            .find(|&j| self.openable(j))
    }

    fn openable(&self, i: usize) -> bool {
        self.entries
            .get(i)
            .is_some_and(|e| !matches!(e.status, Status::Error(_)))
    }
}

/// One file a walker found.
#[derive(Debug)]
struct Walked {
    /// Path below the root.
    rel: PathBuf,
    /// Its size, or `None` if it could not be read; the content worker
    /// then finds out what is wrong.
    size: Option<u64>,
}

/// What every comparison thread shares.
struct Shared {
    filter: Filter,
    show_ignored: bool,
    left: PathBuf,
    right: PathBuf,
    block: usize,
    generation: Arc<Generation>,
    /// The generation this comparison runs as.
    ours: u64,
    /// The event loop is gone: stop as if cancelled.
    gone: AtomicBool,
}

impl Shared {
    fn cancelled(&self) -> bool {
        self.generation.current() != self.ours || self.gone.load(AtomicOrdering::Relaxed)
    }
}

/// Walk `root` in path order, calling `out` with each file that passes the
/// filter. Stops when `out` returns `false` or the comparison is
/// cancelled.
///
/// The walker settings match find in files: hidden files are included,
/// `.git` folders are skipped, and ignore files count unless
/// `show_ignored` (`parents(true)` reads `.gitignore` files above the root
/// too, so comparing a subfolder of a repository still respects them).
fn walk(shared: &Shared, root: &Path, mut out: impl FnMut(Walked) -> bool) {
    let show = shared.show_ignored;
    let dir_filter = shared.filter.clone();
    let dir_root = root.to_path_buf();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(!show)
        .git_exclude(!show)
        .git_global(!show)
        .ignore(!show)
        .parents(true)
        // Name order within each folder plus depth-first order is
        // `Path`'s order; the merge relies on it.
        .sort_by_file_name(std::cmp::Ord::cmp)
        .filter_entry(move |e| {
            if !e.file_type().is_some_and(|t| t.is_dir()) {
                return true;
            }
            let rel = e.path().strip_prefix(&dir_root).unwrap_or(e.path());
            e.file_name() != ".git" && !dir_filter.skips_dir(rel)
        })
        .build();
    for entry in walker {
        if shared.cancelled() {
            return;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                log::debug!("folder compare: {e}");
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        if !shared.filter.takes_file(rel) {
            continue;
        }
        let size = entry.metadata().ok().map(|m| m.len());
        if !out(Walked {
            rel: rel.to_path_buf(),
            size,
        }) {
            return;
        }
    }
}

/// A merger's or worker's batch in progress, sent when due.
struct Outbox<'a, S: Fn(Batch) -> bool> {
    shared: &'a Shared,
    send: &'a S,
    pending: Batch,
    last_sent: Instant,
}

impl<'a, S: Fn(Batch) -> bool> Outbox<'a, S> {
    fn new(shared: &'a Shared, send: &'a S) -> Self {
        Self {
            shared,
            send,
            pending: Batch {
                generation: shared.ours,
                ..Batch::default()
            },
            last_sent: Instant::now(),
        }
    }

    fn push(&mut self, rel: PathBuf, status: Status) {
        if status == Status::Same {
            self.pending.same += 1;
        } else {
            self.pending.entries.push(Entry { rel, status });
        }
        self.flush_if_due();
    }

    fn flush_if_due(&mut self) {
        if self.pending.entries.len() >= BATCH_ENTRIES || self.last_sent.elapsed() >= BATCH_TIME {
            self.flush();
        }
    }

    fn flush(&mut self) {
        self.last_sent = Instant::now();
        if self.pending.entries.is_empty() && self.pending.same == 0 {
            return;
        }
        let batch = std::mem::replace(
            &mut self.pending,
            Batch {
                generation: self.shared.ours,
                ..Batch::default()
            },
        );
        if self.shared.cancelled() {
            return;
        }
        if !(self.send)(batch) {
            self.shared.gone.store(true, AtomicOrdering::Relaxed);
        }
    }
}

impl<S: Fn(Batch) -> bool> Drop for Outbox<'_, S> {
    fn drop(&mut self) {
        self.flush();
    }
}

/// The next file from a walker, waiting in short steps so the outbox is
/// still sent on time while the walk is slow. `None` once the walk ended
/// or the comparison was cancelled.
fn next_walked<S: Fn(Batch) -> bool>(
    rx: &Receiver<Walked>,
    outbox: &mut Outbox<'_, S>,
) -> Option<Walked> {
    loop {
        if outbox.shared.cancelled() {
            return None;
        }
        match rx.recv_timeout(BATCH_TIME) {
            Ok(w) => return Some(w),
            Err(RecvTimeoutError::Timeout) => outbox.flush_if_due(),
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
}

/// Merge the two sorted walks: decide what can be decided from paths and
/// sizes, and queue same-size pairs for the content workers.
fn merge<S: Fn(Batch) -> bool>(
    shared: &Shared,
    send: &S,
    left: &Receiver<Walked>,
    right: &Receiver<Walked>,
    jobs: &SyncSender<PathBuf>,
) {
    let mut outbox = Outbox::new(shared, send);
    let mut l = next_walked(left, &mut outbox);
    let mut r = next_walked(right, &mut outbox);
    loop {
        if shared.cancelled() {
            return;
        }
        let order = match (&l, &r) {
            (None, None) => return,
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some(a), Some(b)) => a.rel.cmp(&b.rel),
        };
        match order {
            Ordering::Less => {
                if let Some(a) = l.take() {
                    outbox.push(a.rel, Status::OnlyLeft);
                }
                l = next_walked(left, &mut outbox);
            }
            Ordering::Greater => {
                if let Some(b) = r.take() {
                    outbox.push(b.rel, Status::OnlyRight);
                }
                r = next_walked(right, &mut outbox);
            }
            Ordering::Equal => {
                if let (Some(a), Some(b)) = (l.take(), r.take()) {
                    match (a.size, b.size) {
                        (Some(x), Some(y)) if x != y => outbox.push(a.rel, Status::Differs),
                        (Some(0), Some(0)) => outbox.push(a.rel, Status::Same),
                        _ => {
                            // Waiting here when every worker is busy is
                            // the back-pressure that bounds memory.
                            outbox.flush_if_due();
                            if jobs.send(a.rel).is_err() {
                                return;
                            }
                        }
                    }
                }
                l = next_walked(left, &mut outbox);
                r = next_walked(right, &mut outbox);
            }
        }
    }
}

/// A content worker: take same-size pairs off the queue and compare them
/// with two block buffers allocated once for the worker's whole life.
fn content_worker<S: Fn(Batch) -> bool>(
    shared: &Shared,
    send: &S,
    jobs: &Mutex<Receiver<PathBuf>>,
) {
    let mut outbox = Outbox::new(shared, send);
    // Zeroed allocations of this size are mapped lazily by the OS, so a
    // worker that only meets small files touches only the first pages.
    let mut a = vec![0u8; shared.block];
    let mut b = vec![0u8; shared.block];
    loop {
        // Hold the lock only to take one job. A cancelled worker keeps
        // taking jobs and dropping them, so a merger blocked on a full
        // queue is released and sees the cancel too.
        let job = match jobs.lock() {
            Ok(rx) => rx.recv_timeout(BATCH_TIME),
            Err(_) => return,
        };
        let rel = match job {
            Ok(rel) => rel,
            Err(RecvTimeoutError::Timeout) => {
                outbox.flush_if_due();
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if shared.cancelled() {
            continue;
        }
        let status = match same_contents(
            &shared.left.join(&rel),
            &shared.right.join(&rel),
            &mut a,
            &mut b,
        ) {
            Ok(true) => Status::Same,
            Ok(false) => Status::Differs,
            Err(e) => Status::Error(e.to_string()),
        };
        outbox.push(rel, status);
    }
}

/// Fill `buf` from `file` until it is full or the file ends; returns how
/// many bytes were read. A single `read` may return less than asked even
/// mid-file, so it loops.
fn fill(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Whether two files hold the same bytes, read block by block into `a` and
/// `b` (equal lengths) and stopping at the first block that differs.
fn same_contents(
    left: &Path,
    right: &Path,
    left_buf: &mut [u8],
    right_buf: &mut [u8],
) -> std::io::Result<bool> {
    let mut left_file = File::open(left)?;
    let mut right_file = File::open(right)?;
    loop {
        let read = fill(&mut left_file, left_buf)?;
        if fill(&mut right_file, right_buf)? != read || left_buf[..read] != right_buf[..read] {
            return Ok(false);
        }
        if read < left_buf.len() {
            return Ok(true);
        }
    }
}

/// Start comparing two folders on background threads. `generation` is
/// bumped and the comparison runs as the new value, which is returned:
/// batches carry it, and bumping it again stops the comparison. `send`
/// delivers each batch to the event loop and returns `false` when the loop
/// is gone, which stops the comparison too.
///
/// # Errors
///
/// Returns a message when a glob does not compile or the threads cannot be
/// started; nothing runs then.
pub fn start(
    params: &Params,
    generation: &Arc<Generation>,
    send: impl Fn(Batch) -> bool + Send + Sync + 'static,
) -> Result<u64, String> {
    start_with_block(params, generation, send, BLOCK)
}

/// [`start`] with content read in blocks of `block` bytes, so tests can
/// cross block boundaries with small files.
fn start_with_block(
    params: &Params,
    generation: &Arc<Generation>,
    send: impl Fn(Batch) -> bool + Send + Sync + 'static,
    block: usize,
) -> Result<u64, String> {
    let filter = Filter::new(&params.include, &params.exclude)?;
    let ours = generation.bump();
    let shared = Shared {
        filter,
        show_ignored: params.show_ignored,
        left: params.left.clone(),
        right: params.right.clone(),
        block: block.max(1),
        generation: Arc::clone(generation),
        ours,
        gone: AtomicBool::new(false),
    };
    let spawned = std::thread::Builder::new()
        .name("nimbus-folder-compare".to_owned())
        .spawn(move || run(&shared, &send));
    spawned.map_err(|e| format!("could not start the folder comparison: {e}"))?;
    Ok(ours)
}

/// The coordinator: start the walkers, the merger's inputs and the content
/// workers as scoped threads, merge on this thread, then send `done` once
/// every thread has finished and flushed.
fn run<S: Fn(Batch) -> bool + Sync>(shared: &Shared, send: &S) {
    // One core stays free for the main thread.
    let workers = std::thread::available_parallelism()
        .map_or(2, |n| n.get().saturating_sub(1))
        .max(1);
    let (left_tx, left_rx) = mpsc::sync_channel(WALK_QUEUE);
    let (right_tx, right_rx) = mpsc::sync_channel(WALK_QUEUE);
    let (job_tx, job_rx) = mpsc::sync_channel(JOB_QUEUE);
    let job_rx = Mutex::new(job_rx);
    let started = std::thread::scope(|s| {
        let spawn_walker = |root: &'static str, tx: SyncSender<Walked>| {
            std::thread::Builder::new()
                .name(format!("nimbus-compare-walk-{root}"))
                .spawn_scoped(s, move || {
                    let dir = if root == "left" {
                        &shared.left
                    } else {
                        &shared.right
                    };
                    walk(shared, dir, |w| tx.send(w).is_ok());
                })
        };
        if let Err(e) = spawn_walker("left", left_tx).and(spawn_walker("right", right_tx)) {
            log::warn!("folder compare: could not start a walker: {e}");
            return false;
        }
        let mut any_worker = false;
        for i in 0..workers {
            let job_rx = &job_rx;
            let spawned = std::thread::Builder::new()
                .name(format!("nimbus-compare-{i}"))
                .spawn_scoped(s, move || content_worker(shared, send, job_rx));
            match spawned {
                Ok(_) => any_worker = true,
                Err(e) => log::debug!("folder compare: worker {i} not started: {e}"),
            }
        }
        if !any_worker {
            log::warn!("folder compare: no content worker could start");
            return false;
        }
        merge(shared, send, &left_rx, &right_rx, &job_tx);
        // Closing the queue lets the workers drain it and stop; dropping
        // the walk receivers stops walkers that are still sending.
        drop(job_tx);
        drop(left_rx);
        drop(right_rx);
        true
    });
    if started && !shared.cancelled() {
        send(Batch {
            generation: shared.ours,
            done: true,
            ..Batch::default()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Run a comparison to the end with blocks of `block` bytes.
    fn compare_with(params: &Params, block: usize) -> Results {
        let generation = Arc::new(Generation::default());
        let (tx, rx) = mpsc::channel();
        let g = start_with_block(params, &generation, move |b| tx.send(b).is_ok(), block).unwrap();
        let mut results = Results::new(g, params.left.clone(), params.right.clone());
        while !results.done {
            let batch = rx.recv_timeout(Duration::from_secs(60)).unwrap();
            assert!(results.receive(batch));
        }
        results
    }

    fn compare(params: &Params) -> Results {
        compare_with(params, BLOCK)
    }

    fn params(left: &tempfile_dir::Dir, right: &tempfile_dir::Dir) -> Params {
        Params {
            left: left.path().to_path_buf(),
            right: right.path().to_path_buf(),
            ..Params::default()
        }
    }

    /// Rows as `(path with /, status)`.
    fn rows(r: &Results) -> Vec<(String, Status)> {
        r.rows()
            .iter()
            .map(|e| (e.rel.to_string_lossy().replace('\\', "/"), e.status.clone()))
            .collect()
    }

    #[test]
    fn classifies_pairs() {
        let l = tempfile_dir::Dir::new("fc-classify-l");
        let r = tempfile_dir::Dir::new("fc-classify-r");
        l.write("same.txt", "hello\n");
        r.write("same.txt", "hello\n");
        l.write("empty", "");
        r.write("empty", "");
        l.write("size.txt", "short");
        r.write("size.txt", "longer text");
        l.write("sub/samesize.txt", "abcd");
        r.write("sub/samesize.txt", "abce");
        l.write("left-only.txt", "x");
        r.write("right/only.txt", "y");
        // Larger than one 8-byte block: equal, and differing only in the
        // last block.
        let big = "0123456789".repeat(10);
        l.write("big-same", &big);
        r.write("big-same", &big);
        let mut changed = big.clone();
        changed.replace_range(95..96, "X");
        l.write("big-diff", &big);
        r.write("big-diff", &changed);
        // Exactly two blocks, so the end falls on a block boundary.
        l.write("exact", "0123456789abcdef");
        r.write("exact", "0123456789abcdef");

        let res = compare_with(&params(&l, &r), 8);
        assert_eq!(
            rows(&res),
            vec![
                ("big-diff".into(), Status::Differs),
                ("left-only.txt".into(), Status::OnlyLeft),
                ("right/only.txt".into(), Status::OnlyRight),
                ("size.txt".into(), Status::Differs),
                ("sub/samesize.txt".into(), Status::Differs),
            ]
        );
        assert_eq!(
            (
                res.only_left,
                res.only_right,
                res.differs,
                res.same,
                res.errors
            ),
            (1, 1, 3, 4, 0)
        );

        let (a, b) = res.pair(1);
        assert_eq!(a, Some(l.path().join("left-only.txt")));
        assert_eq!(b, None);
        let (a, b) = res.pair(4);
        assert_eq!(a, Some(l.path().join("sub/samesize.txt")));
        assert_eq!(b, Some(r.path().join("sub/samesize.txt")));
        assert_eq!(res.pair(99), (None, None));
        assert_eq!(res.next_after(0), Some(1));
        assert_eq!(res.next_after(4), None);
        assert_eq!(res.prev_before(0), None);
        assert_eq!(res.prev_before(3), Some(2));
    }

    #[test]
    fn walk_order_is_path_order() {
        let l = tempfile_dir::Dir::new("fc-order-l");
        let r = tempfile_dir::Dir::new("fc-order-r");
        for name in [
            "a/b", "a/c/d", "a.txt", "a-b", "A", "a b", "ab", "b/a", "a/b.c", "z", "_x",
        ] {
            l.write(name, name);
        }
        // `a` is a file on the right but a folder on the left.
        for name in ["a", "a-b", "B", "a.txt"] {
            r.write(name, name);
        }

        let shared = Shared {
            filter: Filter::default(),
            show_ignored: false,
            left: l.path().to_path_buf(),
            right: r.path().to_path_buf(),
            block: BLOCK,
            generation: Arc::new(Generation::default()),
            ours: 0,
            gone: AtomicBool::new(false),
        };
        let mut walked = Vec::new();
        walk(&shared, l.path(), |w| {
            walked.push(w.rel);
            true
        });
        let mut sorted = walked.clone();
        sorted.sort();
        assert_eq!(walked.len(), 11);
        assert_eq!(walked, sorted, "the walk yields paths in Path order");

        let res = compare(&params(&l, &r));
        let only_right: Vec<_> = rows(&res)
            .into_iter()
            .filter(|(_, s)| *s == Status::OnlyRight)
            .map(|(p, _)| p)
            .collect();
        assert_eq!(only_right, vec!["B", "a"]);
        assert_eq!(res.same, 2, "a-b and a.txt");
        let paths: Vec<PathBuf> = res.rows().iter().map(|e| e.rel.clone()).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted);
    }

    #[test]
    fn respects_gitignore_and_globs() {
        let l = tempfile_dir::Dir::new("fc-ignore-l");
        let r = tempfile_dir::Dir::new("fc-ignore-r");
        for d in [&l, &r] {
            d.write(".gitignore", "ignored.txt\n");
            // `.gitignore` counts only inside a repository.
            std::fs::create_dir_all(d.path().join(".git")).unwrap();
        }
        l.write("ignored.txt", "left");
        l.write("src/a.rs", "fn a() {}");
        l.write("src/b.rs", "fn b() {}");
        r.write("src/b.rs", "fn B() {}");
        l.write("notes.md", "x");
        r.write("docs/c.md", "y");

        let mut p = params(&l, &r);
        let all = rows(&compare(&p));
        let names: Vec<&str> = all.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["docs/c.md", "notes.md", "src/a.rs", "src/b.rs"]);

        p.show_ignored = true;
        let names: Vec<String> = rows(&compare(&p)).into_iter().map(|(n, _)| n).collect();
        assert!(names.iter().any(|n| n == "ignored.txt"));
        assert!(!names.iter().any(|n| n.starts_with(".git/")));

        p.show_ignored = false;
        p.include = "*.rs".into();
        let names: Vec<String> = rows(&compare(&p)).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["src/a.rs", "src/b.rs"]);

        p.include = String::new();
        p.exclude = "src, *.md".into();
        let res = compare(&p);
        assert_eq!(res.rows(), []);
        assert!(res.done);

        p.include = "[".into();
        let g = Arc::new(Generation::default());
        assert!(start(&p, &g, |_| true).is_err());
    }

    #[test]
    fn read_errors_are_rows_not_aborts() {
        let l = tempfile_dir::Dir::new("fc-error-l");
        let r = tempfile_dir::Dir::new("fc-error-r");
        l.write("ok.txt", "same");
        r.write("ok.txt", "same");
        let shared = Shared {
            filter: Filter::default(),
            show_ignored: false,
            left: l.path().to_path_buf(),
            right: r.path().to_path_buf(),
            block: 8,
            generation: Arc::new(Generation::default()),
            ours: 0,
            gone: AtomicBool::new(false),
        };
        // A pair whose left side vanished after the walk.
        let (tx, rx) = mpsc::sync_channel(4);
        tx.send(PathBuf::from("gone.txt")).unwrap();
        tx.send(PathBuf::from("ok.txt")).unwrap();
        drop(tx);
        let outbox = Mutex::new(Vec::new());
        let send = |b: Batch| {
            outbox.lock().unwrap().push(b);
            true
        };
        content_worker(&shared, &send, &Mutex::new(rx));
        let mut res = Results::new(0, shared.left.clone(), shared.right.clone());
        for b in outbox.into_inner().unwrap() {
            res.receive(b);
        }
        assert_eq!((res.errors, res.same), (1, 1));
        assert!(matches!(res.rows()[0].status, Status::Error(_)));
        assert_eq!(res.next_after(0), None, "error rows are not stepped to");
    }

    #[test]
    fn a_newer_start_cancels_and_stale_batches_are_ignored() {
        let l = tempfile_dir::Dir::new("fc-cancel-l");
        let r = tempfile_dir::Dir::new("fc-cancel-r");
        for i in 0..200 {
            l.write(&format!("d/{i}.txt"), "left");
        }
        let generation = Arc::new(Generation::default());
        let (tx, rx) = mpsc::channel();
        let p = params(&l, &r);
        let g = start(&p, &generation, move |b| tx.send(b).is_ok()).unwrap();
        generation.bump();
        let batches: Vec<Batch> = rx.iter().collect();
        assert!(!batches.iter().any(|b| b.done && b.generation == g));

        let mut res = Results::new(g + 1, p.left.clone(), p.right.clone());
        assert!(!res.receive(Batch {
            generation: g,
            same: 3,
            ..Batch::default()
        }));
        assert_eq!(res.same, 0);
    }

    #[test]
    fn receive_sorts_late_rows_in() {
        let mut res = Results::new(1, "L".into(), "R".into());
        let entry = |p: &str| Entry {
            rel: p.into(),
            status: Status::Differs,
        };
        res.receive(Batch {
            generation: 1,
            entries: vec![entry("a"), entry("c/d"), entry("e")],
            ..Batch::default()
        });
        res.receive(Batch {
            generation: 1,
            entries: vec![entry("c"), entry("b"), entry("f")],
            ..Batch::default()
        });
        let paths: Vec<_> = rows(&res).into_iter().map(|(p, _)| p).collect();
        assert_eq!(paths, vec!["a", "b", "c", "c/d", "e", "f"]);
        assert_eq!(res.differs, 6);
    }

    /// Two trees of 100,000 small files (1000 folders of 100), with a few
    /// hundred files changed, added and removed: time to the first batch
    /// and to the end (M18's budget is under two seconds).
    #[test]
    #[ignore = "benchmark; run explicitly in release mode"]
    fn folder_compare_benchmark() {
        let l = tempfile_dir::Dir::new("fc-bench-l");
        let r = tempfile_dir::Dir::new("fc-bench-r");
        let made = Instant::now();
        // (only left, only right, differs, same, errors)
        let mut expected = (0, 0, 0, 0, 0);
        for d in 0..1000 {
            for f in 0..100 {
                let rel = format!("dir{d:04}/file{f:03}.txt");
                let text = format!("folder {d} file {f}\n{}\n", "x".repeat(f));
                let n = d * 100 + f;
                let in_left = n % 997 != 0;
                let in_right = n % 991 == 0 || n % 983 != 0;
                match (in_left, in_right) {
                    (true, false) => expected.0 += 1,
                    (false, true) => expected.1 += 1,
                    (true, true) if n % 991 == 0 => expected.2 += 1,
                    (true, true) => expected.3 += 1,
                    (false, false) => {}
                }
                if in_left {
                    l.write(&rel, &text);
                }
                if n % 991 == 0 {
                    // Same size, different content.
                    r.write(&rel, &text.replace("folder", "FOLDER"));
                } else if n % 983 != 0 {
                    r.write(&rel, &text);
                }
            }
        }
        println!("made the trees in {:?}", made.elapsed());
        let p = params(&l, &r);
        let generation = Arc::new(Generation::default());
        let (tx, rx) = mpsc::channel();
        let started = Instant::now();
        let g = start(&p, &generation, move |b| tx.send(b).is_ok()).unwrap();
        let mut res = Results::new(g, p.left.clone(), p.right.clone());
        let mut first = None;
        while !res.done {
            assert!(res.receive(rx.recv().unwrap()));
            if first.is_none() && !res.rows().is_empty() {
                first = Some(started.elapsed());
            }
        }
        let total = started.elapsed();
        println!(
            "folder compare of 100,000 files: first row after {:?}, done after {total:?}; \
             {} only left, {} only right, {} differ, {} same, {} errors",
            first.unwrap_or(total),
            res.only_left,
            res.only_right,
            res.differs,
            res.same,
            res.errors
        );
        assert_eq!(
            (
                res.only_left,
                res.only_right,
                res.differs,
                res.same,
                res.errors
            ),
            expected
        );
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
