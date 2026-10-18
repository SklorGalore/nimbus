//! Sparse line index for huge files (M7, DESIGN.md §4.1).
//!
//! # Why sparse
//!
//! To answer "where does line 40 000 000 start?" or "which line is byte
//! 3 000 000 000 on?" a buffer needs some kind of line index. The obvious
//! one stores the start offset of *every* line. For a 4 GB log with 100-byte
//! lines that is 40 million `u64`s: 320 MB of index for a file the editor
//! deliberately does not load into memory (the mapped backend lets the OS
//! page it in on demand).
//!
//! A sparse index stores only a few **checkpoints** and scans the bytes
//! between them on demand. A [`Checkpoint`] is a pair: a byte offset and the
//! number of `\n` bytes before it. Checkpoints go at every 1024th line start
//! and, because one line can be hundreds of megabytes long, also at least
//! every 1 MiB of bytes even in the middle of a line. Any lookup therefore
//! binary-searches the checkpoints, then scans at most 1 MiB (and at most
//! 1023 newlines) with [`memchr`], which uses SIMD to look at 16 or 32 bytes
//! per instruction and takes tens of microseconds for that distance.
//!
//! The memory arithmetic: a checkpoint is 16 bytes. A 4 GB log with about
//! 70 million lines needs about 70 million / 1024 ≈ 68 000 line checkpoints
//! plus at most 4096 byte checkpoints (one per MiB, and only where a line
//! gap exceeds a MiB), so roughly 72 000 checkpoints or 1.1 MB, against
//! 560 MB for a full index. The worst case is a file of nothing but `\n`,
//! where every 1024th byte starts a checkpointed line: the index is then
//! 1/64 of the file size.
//!
//! # How it is built and used
//!
//! A [`Scanner`] is fed the file from the start, in pieces of any size (a
//! background worker reads or maps 64 MB at a time). Every so often the
//! worker calls [`Scanner::take_chunk`] and sends the resulting
//! [`IndexChunk`] to the editor, which appends it to its [`LineIndex`] with
//! [`LineIndex::merge`]. Until the scanner has seen the whole file the index
//! covers only a prefix: queries about bytes or lines past that frontier
//! return `None`, and the caller falls back to byte-based scrolling.
//!
//! The index stores no text. Queries take the file's bytes as `src`, which
//! for a huge file is the memory mapping, so the index and the mapping can
//! be owned separately and the index stays a plain, testable value.
//!
//! Lines end only at `\n` (see [`crate::buffer`]); a `\r` before it is part
//! of the line ending but does not change where lines start, so the index
//! only ever looks for `\n`. Bytes need not be valid UTF-8.

use std::fmt;
use std::sync::Arc;

/// A remembered position: `newlines` is the number of `\n` bytes in
/// `[0, offset)`. Checkpoints after a `\n` are line starts; byte-spacing
/// checkpoints can fall in the middle of a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    /// Byte offset into the file.
    pub offset: u64,
    /// Number of `\n` bytes before `offset`, which is also the 0-based line
    /// that `offset` is on.
    pub newlines: u64,
}

/// The implicit checkpoint at the start of every file. It is never stored.
const ORIGIN: Checkpoint = Checkpoint {
    offset: 0,
    newlines: 0,
};

/// How far apart checkpoints may be. The defaults are for real files;
/// tests use tiny values so a few hundred bytes exercise every boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spacing {
    lines: u64,
    bytes: u64,
}

impl Spacing {
    /// Every 1024th line start, and at least every 1 MiB.
    pub const DEFAULT: Self = Self {
        lines: 1024,
        bytes: 1 << 20,
    };

    /// A checkpoint at every `lines`-th line start and at least every
    /// `bytes` bytes. Zero is treated as one, so the spacing is never empty.
    #[must_use]
    pub fn new(lines: u64, bytes: u64) -> Self {
        Self {
            lines: lines.max(1),
            bytes: bytes.max(1),
        }
    }

    /// Lines between line-start checkpoints.
    #[must_use]
    pub fn lines(self) -> u64 {
        self.lines
    }

    /// The most bytes between two consecutive checkpoints.
    #[must_use]
    pub fn bytes(self) -> u64 {
        self.bytes
    }
}

impl Default for Spacing {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Checkpoints found by a [`Scanner`] since its previous
/// [`Scanner::take_chunk`], ready to be sent to another thread and merged
/// into a [`LineIndex`]. The fields are private so a chunk can only come
/// from a scanner and is always internally consistent.
#[derive(Clone, Debug)]
pub struct IndexChunk {
    from: u64,
    upto: u64,
    lines: u64,
    done: bool,
    checkpoints: Arc<[Checkpoint]>,
}

impl IndexChunk {
    /// Byte offset where this chunk starts: the previous chunk's `upto`.
    #[must_use]
    pub fn from(&self) -> u64 {
        self.from
    }

    /// Bytes scanned from the start of the file up to the end of this chunk.
    #[must_use]
    pub fn upto(&self) -> u64 {
        self.upto
    }

    /// Newlines in `[0, upto)`.
    #[must_use]
    pub fn lines(&self) -> u64 {
        self.lines
    }

    /// Whether the scanner had been told the file ended
    /// ([`Scanner::finish`]).
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// The checkpoints in this chunk, in increasing offset order.
    #[must_use]
    pub fn checkpoints(&self) -> &[Checkpoint] {
        &self.checkpoints
    }
}

/// Builds checkpoints from a file's bytes, fed in order from the start.
///
/// The scanner keeps no bytes, only counters, so the worker can feed it
/// from a reused read buffer or straight from a memory mapping.
#[derive(Debug)]
pub struct Scanner {
    spacing: Spacing,
    /// Bytes fed so far.
    pos: u64,
    /// Newlines in `[0, pos)`.
    lines: u64,
    /// Offset of the most recent checkpoint (0 for the implicit origin).
    last: u64,
    /// Checkpoints not yet handed over by `take_chunk`.
    pending: Vec<Checkpoint>,
    /// `pos` at the previous `take_chunk`.
    chunk_from: u64,
    done: bool,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new(Spacing::DEFAULT)
    }
}

impl Scanner {
    /// A scanner at the start of a file.
    #[must_use]
    pub fn new(spacing: Spacing) -> Self {
        Self {
            spacing,
            pos: 0,
            lines: 0,
            last: 0,
            pending: Vec::new(),
            chunk_from: 0,
            done: false,
        }
    }

    /// A scanner that carries on where `index` ends, for a file that grew
    /// after it was indexed (M12's Follow Tail): fed the bytes from
    /// [`LineIndex::indexed`] on, its chunks merge into `index`. `spacing`
    /// must be the one the index was built with.
    #[must_use]
    pub fn resume(index: &LineIndex, spacing: Spacing) -> Self {
        Self {
            spacing,
            pos: index.upto,
            lines: index.lines,
            last: index.checkpoints().last().map_or(0, |c| c.offset),
            pending: Vec::new(),
            chunk_from: index.upto,
            done: false,
        }
    }

    /// Bytes fed so far.
    #[must_use]
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Scans the next `bytes` of the file. Pieces may be any size,
    /// including empty, and may split a `\r\n` pair or a UTF-8 character.
    /// Bytes fed after [`Scanner::finish`] are ignored.
    pub fn feed(&mut self, bytes: &[u8]) {
        if self.done {
            return;
        }
        let mut rest = bytes;
        while !rest.is_empty() {
            // Bytes until a checkpoint is due. The invariant
            // pos - last < spacing.bytes makes this at least 1; the `max`
            // guarantees the loop always advances even if it were broken.
            let to_forced = self
                .last
                .saturating_add(self.spacing.bytes)
                .saturating_sub(self.pos)
                .max(1);
            let window_len = usize::try_from(to_forced).map_or(rest.len(), |n| n.min(rest.len()));
            let (window, after) = rest.split_at(window_len);
            // Newlines until the next line count that is a multiple of the
            // spacing; at least 1.
            let need = self.spacing.lines - self.lines % self.spacing.lines;
            match nth_newline(window, need) {
                Ok(i) => {
                    let (_, tail) = rest.split_at(i + 1);
                    rest = tail;
                    self.pos += to_u64(i + 1);
                    self.lines += need;
                    self.push_checkpoint();
                }
                Err(found) => {
                    rest = after;
                    self.pos += to_u64(window_len);
                    self.lines += found;
                    if self.pos - self.last >= self.spacing.bytes {
                        self.push_checkpoint();
                    }
                }
            }
        }
    }

    fn push_checkpoint(&mut self) {
        self.pending.push(Checkpoint {
            offset: self.pos,
            newlines: self.lines,
        });
        self.last = self.pos;
    }

    /// Marks the end of the file; chunks taken from now on report
    /// [`IndexChunk::is_done`].
    pub fn finish(&mut self) {
        self.done = true;
    }

    /// Hands over everything scanned since the previous call. Cheap: it
    /// moves the pending checkpoints into a shared slice.
    pub fn take_chunk(&mut self) -> IndexChunk {
        let chunk = IndexChunk {
            from: self.chunk_from,
            upto: self.pos,
            lines: self.lines,
            done: self.done,
            checkpoints: Arc::from(std::mem::take(&mut self.pending)),
        };
        self.chunk_from = self.pos;
        chunk
    }
}

/// Why a chunk (or a whole index) could not be merged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeError {
    /// The chunk does not start where the index ends: a chunk was skipped,
    /// repeated, or came from a scanner of a different file.
    Gap {
        /// Bytes the index covers.
        indexed: u64,
        /// Where the chunk starts.
        chunk_from: u64,
    },
    /// The index was already complete and the chunk would extend it.
    AfterDone,
    /// The chunk or index covers more bytes than the document's original
    /// file has, so it was built from a different file. Only
    /// [`crate::mapped::MappedBuffer`] can tell; [`LineIndex::merge`] never
    /// returns this.
    PastEnd {
        /// Bytes the chunk or index claims to cover.
        upto: u64,
        /// The original file's length.
        len: u64,
    },
}

impl fmt::Display for MergeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gap {
                indexed,
                chunk_from,
            } => write!(
                f,
                "line index chunk starts at byte {chunk_from} but the index ends at {indexed}"
            ),
            Self::AfterDone => f.write_str("line index chunk arrived after the index was complete"),
            Self::PastEnd { upto, len } => write!(
                f,
                "line index covers {upto} bytes but the file has only {len}"
            ),
        }
    }
}

impl std::error::Error for MergeError {}

/// The sparse line index of one file: checkpoints for the prefix scanned so
/// far, plus the totals for that prefix.
///
/// Checkpoints are kept as one shared slice per merged chunk rather than one
/// big `Vec`, so `clone` (taken for every snapshot handed to a worker
/// thread) copies a few dozen `Arc` pointers instead of every checkpoint.
/// `Arc` is a reference-counted pointer: cloning it bumps a counter, and the
/// slice is freed when the last clone is dropped.
#[derive(Clone, Debug, Default)]
pub struct LineIndex {
    chunks: Vec<Arc<[Checkpoint]>>,
    upto: u64,
    lines: u64,
    done: bool,
}

impl LineIndex {
    /// An index that covers nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The finished index of `bytes`, scanned in one go: for files small
    /// enough to index before showing them, and for tests.
    #[must_use]
    pub fn build(bytes: &[u8], spacing: Spacing) -> Self {
        let mut scanner = Scanner::new(spacing);
        scanner.feed(bytes);
        scanner.finish();
        let chunk = scanner.take_chunk();
        // An empty index ends at byte 0, where a fresh scanner's first
        // chunk starts, so this merge cannot fail; the fallback only avoids
        // a panic path.
        let mut index = Self::new();
        match index.merge(chunk) {
            Ok(()) => index,
            Err(_) => Self::new(),
        }
    }

    /// Appends the next chunk from the scanner.
    ///
    /// # Errors
    ///
    /// [`MergeError::Gap`] if the chunk does not continue exactly where the
    /// index ends, and [`MergeError::AfterDone`] if the index is complete and
    /// the chunk covers more bytes. The index is unchanged on error.
    pub fn merge(&mut self, chunk: IndexChunk) -> Result<(), MergeError> {
        if chunk.from != self.upto {
            return Err(MergeError::Gap {
                indexed: self.upto,
                chunk_from: chunk.from,
            });
        }
        if self.done && chunk.upto != self.upto {
            return Err(MergeError::AfterDone);
        }
        if !chunk.checkpoints.is_empty() {
            self.chunks.push(chunk.checkpoints);
        }
        self.upto = chunk.upto;
        self.lines = chunk.lines;
        self.done |= chunk.done;
        Ok(())
    }

    /// The file grew after it was indexed to its end: it is not done any
    /// more, and chunks from a [`Scanner::resume`]d scanner can follow.
    pub fn reopen(&mut self) {
        self.done = false;
    }

    /// Bytes from the start of the file that the index covers.
    #[must_use]
    pub fn indexed(&self) -> u64 {
        self.upto
    }

    /// Newlines in the indexed prefix. The file has `lines() + 1` lines once
    /// [`LineIndex::is_done`]; before that it is a lower bound.
    #[must_use]
    pub fn lines(&self) -> u64 {
        self.lines
    }

    /// Whether the whole file has been indexed.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Every stored checkpoint, in increasing offset order.
    pub fn checkpoints(&self) -> impl Iterator<Item = Checkpoint> + '_ {
        self.chunks.iter().flat_map(|c| c.iter().copied())
    }

    /// Number of stored checkpoints; times 16 bytes is the index's size.
    #[must_use]
    pub fn checkpoint_count(&self) -> usize {
        self.chunks.iter().map(|c| c.len()).sum()
    }

    /// The last checkpoint (or the origin) for which `pred` holds. `pred`
    /// must be true for a prefix of the checkpoints and false after it,
    /// which lets both levels use binary search.
    fn last_where(&self, pred: impl Fn(&Checkpoint) -> bool) -> Checkpoint {
        let chunk = self
            .chunks
            .partition_point(|c| c.first().is_some_and(&pred));
        let Some(chunk) = chunk.checked_sub(1).and_then(|i| self.chunks.get(i)) else {
            return ORIGIN;
        };
        let within = chunk.partition_point(&pred);
        within
            .checked_sub(1)
            .and_then(|i| chunk.get(i))
            .copied()
            .unwrap_or(ORIGIN)
    }

    /// Number of `\n` bytes in `[0, x)`, which is also the 0-based line that
    /// byte `x` is on.
    ///
    /// `src` is the file's bytes (at least the indexed prefix). Scans at
    /// most the spacing's byte count from the nearest checkpoint. Returns
    /// `None` if `x` is past the indexed region or `src` is too short.
    #[must_use]
    pub fn rank(&self, src: &[u8], x: u64) -> Option<u64> {
        if x > self.upto {
            return None;
        }
        let cp = self.last_where(|c| c.offset <= x);
        let start = usize::try_from(cp.offset).ok()?;
        let end = usize::try_from(x).ok()?;
        let between = src.get(start..end)?;
        meter::add(between.len());
        Some(cp.newlines + to_u64(memchr::memchr_iter(b'\n', between).count()))
    }

    /// Byte offset where line `n` (0-based) starts: 0 for line 0, otherwise
    /// just after the `n`-th `\n`.
    ///
    /// `src` is the file's bytes (at least the indexed prefix). Scans at
    /// most the spacing's byte count from the nearest checkpoint. Returns
    /// `None` if indexing has not yet reached that line or `src` is too
    /// short.
    #[must_use]
    pub fn select(&self, src: &[u8], n: u64) -> Option<u64> {
        if n == 0 {
            return Some(0);
        }
        if n > self.lines {
            return None;
        }
        // The last checkpoint still before the n-th newline. The next one
        // has at least n newlines before it and is at most a spacing away,
        // so the scan below is bounded by the spacing.
        let cp = self.last_where(|c| c.newlines < n);
        let start = usize::try_from(cp.offset).ok()?;
        let end = usize::try_from(self.upto).ok()?;
        let hay = src.get(start..end)?;
        let found = nth_newline(hay, n - cp.newlines);
        meter::add(found.map_or(hay.len(), |i| i + 1));
        let i = found.ok()?;
        Some(cp.offset + to_u64(i + 1))
    }
}

/// Counts the bytes scanned for line breaks on the current thread, so tests
/// can check that lookups stay bounded however big the file (for example,
/// that a file without line breaks is never scanned whole). Outside tests
/// it compiles to nothing.
#[cfg(test)]
pub(crate) mod meter {
    use std::cell::Cell;

    thread_local! {
        static SCANNED: Cell<usize> = const { Cell::new(0) };
    }

    /// Record that `n` bytes were scanned.
    pub(crate) fn add(n: usize) {
        SCANNED.with(|c| c.set(c.get().saturating_add(n)));
    }

    /// Bytes scanned on this thread since the previous call.
    pub(crate) fn take() -> usize {
        SCANNED.with(|c| c.replace(0))
    }
}

/// Outside tests, counting scanned bytes costs nothing.
#[cfg(not(test))]
pub(crate) mod meter {
    /// Does nothing; see the test version.
    #[inline]
    pub(crate) fn add(_: usize) {}
}

/// Lossless on every platform Rust supports (`usize` is at most 64 bits).
fn to_u64(n: usize) -> u64 {
    n as u64
}

/// Block size for [`nth_newline`]: counting a whole block is faster than
/// stepping through its newlines one by one, so only the block holding the
/// answer is stepped through.
const BLOCK: usize = 4096;

/// Index of the `n`-th (1-based) `\n` in `hay`, or `Err(count)` with the
/// number of newlines in `hay` if there are fewer than `n`.
fn nth_newline(hay: &[u8], n: u64) -> Result<usize, u64> {
    if n == 0 {
        // There is no 0th newline; callers always ask for at least one.
        return Err(0);
    }
    let mut remaining = n;
    let mut base = 0;
    for block in hay.chunks(BLOCK) {
        let count = to_u64(memchr::memchr_iter(b'\n', block).count());
        if count >= remaining
            && let Some(i) =
                memchr::memchr_iter(b'\n', block).nth(to_usize_saturating(remaining - 1))
        {
            return Ok(base + i);
        }
        remaining -= count.min(remaining);
        base += block.len();
    }
    Err(n - remaining)
}

fn to_usize_saturating(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use proptest::test_runner::Config;

    /// Pieces of generated files: line endings (twice, so lines are
    /// common), multi-byte UTF-8, and bytes that are not UTF-8 at all.
    const PIECES: &[&[u8]] = &[
        b"\n",
        b"\n",
        b"\r\n",
        b"\r",
        b"a",
        b"hello ",
        "é".as_bytes(),
        "日本".as_bytes(),
        "👩\u{200d}💻".as_bytes(),
        &[0xFF],
        &[0xC3],
        &[0x80, 0x80],
        b"",
    ];

    fn config(cases: u32) -> Config {
        Config {
            cases,
            failure_persistence: None,
            ..Config::default()
        }
    }

    fn piece() -> impl Strategy<Value = Vec<u8>> {
        prop_oneof![
            6 => prop::sample::select(PIECES).prop_map(<[u8]>::to_vec),
            // Runs without newlines, longer than the tiny byte spacing.
            1 => (0usize..60).prop_map(|n| vec![b'x'; n]),
        ]
    }

    fn bytes() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(piece(), 0..80).prop_map(|v| v.concat())
    }

    fn spacing() -> impl Strategy<Value = Spacing> {
        (1u64..6, 1u64..20).prop_map(|(l, b)| Spacing::new(l, b))
    }

    /// A feed plan: piece lengths, and whether to take a chunk after each.
    fn plan() -> impl Strategy<Value = Vec<(usize, bool)>> {
        prop::collection::vec((0usize..40, any::<bool>()), 0..20)
    }

    /// The naive full index: the offset of every `\n`.
    fn newlines(src: &[u8]) -> Vec<u64> {
        src.iter()
            .enumerate()
            .filter(|&(_, &b)| b == b'\n')
            .map(|(i, _)| i as u64)
            .collect()
    }

    fn naive_rank(nl: &[u64], x: u64) -> u64 {
        nl.iter().filter(|&&p| p < x).count() as u64
    }

    fn naive_select(nl: &[u64], n: u64) -> Option<u64> {
        if n == 0 {
            return Some(0);
        }
        nl.get(usize::try_from(n - 1).ok()?).map(|p| p + 1)
    }

    /// Checks every query on `index` against the naive index of the
    /// prefix it claims to cover, with `src` being the whole file.
    fn check(index: &LineIndex, src: &[u8]) -> Result<(), TestCaseError> {
        let upto = index.indexed();
        let all = newlines(src);
        let known: Vec<u64> = all.iter().copied().filter(|&p| p < upto).collect();
        prop_assert_eq!(index.lines(), known.len() as u64);
        for x in 0..=src.len() as u64 + 2 {
            let want = (x <= upto).then(|| naive_rank(&all, x));
            prop_assert_eq!(index.rank(src, x), want, "rank({})", x);
        }
        for n in 0..=all.len() as u64 + 2 {
            let want = if n <= known.len() as u64 {
                naive_select(&all, n)
            } else {
                None
            };
            prop_assert_eq!(index.select(src, n), want, "select({})", n);
        }
        Ok(())
    }

    /// Feeds `src` following `plan` (the remainder in one last piece),
    /// merging chunks where the plan says and checking queries after each
    /// merge.
    fn build(
        src: &[u8],
        spacing: Spacing,
        plan: &[(usize, bool)],
        check_each: bool,
    ) -> Result<LineIndex, TestCaseError> {
        let mut scanner = Scanner::new(spacing);
        let mut index = LineIndex::new();
        let mut at = 0;
        for &(len, take) in plan {
            let end = (at + len).min(src.len());
            scanner.feed(&src[at..end]);
            at = end;
            prop_assert_eq!(scanner.position(), at as u64);
            if take {
                index.merge(scanner.take_chunk()).unwrap();
                prop_assert_eq!(index.indexed(), at as u64);
                prop_assert!(!index.is_done());
                if check_each {
                    check(&index, src)?;
                }
            }
        }
        scanner.feed(&src[at..]);
        scanner.finish();
        scanner.feed(b"\n ignored after finish");
        index.merge(scanner.take_chunk()).unwrap();
        prop_assert!(index.is_done());
        Ok(index)
    }

    proptest! {
        #![proptest_config(config(512))]

        /// rank and select agree with the naive index at every offset and
        /// line, while partially fed and when done.
        #[test]
        fn matches_naive_index(src in bytes(), spacing in spacing(), plan in plan()) {
            let index = build(&src, spacing, &plan, true)?;
            prop_assert_eq!(index.indexed(), src.len() as u64);
            check(&index, &src)?;
        }

        /// How the file is split into fed pieces and chunks does not change
        /// the checkpoints, and the checkpoints respect the spacing.
        #[test]
        fn chunking_is_invisible(src in bytes(), spacing in spacing(), plan in plan()) {
            let split = build(&src, spacing, &plan, false)?;
            let whole = build(&src, spacing, &[], false)?;
            let cps: Vec<Checkpoint> = split.checkpoints().collect();
            prop_assert_eq!(&cps, &whole.checkpoints().collect::<Vec<_>>());
            prop_assert_eq!(split.lines(), whole.lines());
            prop_assert_eq!(split.checkpoint_count(), cps.len());

            let nl = newlines(&src);
            let mut prev = ORIGIN;
            for &cp in cps.iter().chain(std::iter::once(&Checkpoint {
                offset: src.len() as u64,
                newlines: nl.len() as u64,
            })) {
                prop_assert_eq!(cp.newlines, naive_rank(&nl, cp.offset));
                prop_assert!(cp.offset > prev.offset || cp.offset == src.len() as u64);
                // The scan bound: no gap wider than the spacing.
                prop_assert!(cp.offset - prev.offset <= spacing.bytes());
                prop_assert!(cp.newlines - prev.newlines <= spacing.lines());
                prev = cp;
            }
            // Every spacing-th line start is a checkpoint.
            let mut k = spacing.lines();
            while k <= nl.len() as u64 {
                let at = naive_select(&nl, k);
                prop_assert!(cps.iter().any(|c| Some(c.offset) == at), "line {}", k);
                k += spacing.lines();
            }
        }
    }

    #[test]
    fn short_src_and_bad_merges_do_not_panic() {
        let mut scanner = Scanner::new(Spacing::new(2, 4));
        scanner.feed(b"ab\ncd\nef\ngh");
        let first = scanner.take_chunk();
        let mut index = LineIndex::new();
        index.merge(first.clone()).ok();
        assert_eq!(
            index.merge(first),
            Err(MergeError::Gap {
                indexed: 11,
                chunk_from: 0
            })
        );
        assert_eq!(index.rank(b"ab", 5), None);
        assert_eq!(index.select(b"ab", 2), None);
        assert_eq!(index.select(b"ab", 0), Some(0));

        scanner.finish();
        assert_eq!(index.merge(scanner.take_chunk()), Ok(()));
        assert!(index.is_done());
        assert_eq!(index.merge(scanner.take_chunk()), Ok(()));
        // A chunk from a scanner of a longer file cannot extend a finished
        // index.
        let mut other = Scanner::new(Spacing::new(2, 4));
        other.feed(b"ab\ncd\nef\ngh");
        other.take_chunk();
        other.feed(b"more");
        assert_eq!(index.merge(other.take_chunk()), Err(MergeError::AfterDone));
        assert!(MergeError::AfterDone.to_string().contains("complete"));
    }

    #[test]
    fn build_indexes_everything_in_one_go() {
        let src = b"a\nbb\nccc\n";
        let index = LineIndex::build(src, Spacing::new(1, 4));
        assert!(index.is_done());
        assert_eq!((index.indexed(), index.lines()), (9, 3));
        assert_eq!(index.select(src, 2), Some(5));
        assert_eq!(index.rank(src, 6), Some(2));
        assert!(LineIndex::build(b"", Spacing::DEFAULT).is_done());
    }

    #[test]
    fn zero_spacing_is_clamped() {
        let mut scanner = Scanner::new(Spacing::new(0, 0));
        scanner.feed(b"a\nb");
        let mut index = LineIndex::new();
        index.merge(scanner.take_chunk()).ok();
        assert_eq!(index.checkpoint_count(), 3);
        assert_eq!(index.select(b"a\nb", 1), Some(2));
    }

    /// 50 MB of 100-byte lines: 500 000 lines need one checkpoint per 1024
    /// lines (488) and no byte checkpoints, because 1024 lines is 100 KB.
    #[test]
    fn index_of_50_mb_is_small() {
        const LINES: u64 = 500_000;
        let mut line = vec![b'x'; 99];
        line.push(b'\n');
        let src = line.repeat(500_000);
        let mut scanner = Scanner::default();
        for piece in src.chunks(64 << 10) {
            scanner.feed(piece);
        }
        scanner.finish();
        let mut index = LineIndex::new();
        assert_eq!(index.merge(scanner.take_chunk()), Ok(()));
        assert_eq!(index.lines(), LINES);
        let count = index.checkpoint_count() as u64;
        let bound = LINES / 1024 + src.len() as u64 / (1 << 20) + 1;
        assert!(count <= bound, "{count} checkpoints, bound {bound}");
        assert_eq!(count, LINES / 1024);
        assert!(count * 16 < 8 << 10, "index under 8 KB");
        for n in [1, 1023, 1024, 1025, 250_000, LINES - 1, LINES] {
            assert_eq!(index.select(&src, n), Some(n * 100));
            assert_eq!(index.rank(&src, n * 100), Some(n));
            assert_eq!(index.rank(&src, n * 100 - 1), Some(n - 1));
        }
        assert_eq!(index.select(&src, LINES + 1), None);

        // One 50 MB line: only byte checkpoints, one per MiB.
        let long = vec![b'y'; src.len()];
        let mut scanner = Scanner::default();
        scanner.feed(&long);
        let mut index = LineIndex::new();
        assert_eq!(index.merge(scanner.take_chunk()), Ok(()));
        assert_eq!(index.checkpoint_count() as u64, long.len() as u64 >> 20);
        assert_eq!(index.rank(&long, long.len() as u64 - 3), Some(0));
    }

    #[test]
    #[ignore = "benchmark; run explicitly in release mode"]
    fn index_500_mb_benchmark() {
        let mut line = vec![b'x'; 99];
        line.push(b'\n');
        let src = line.repeat(5_000_000);
        let start = std::time::Instant::now();
        let mut scanner = Scanner::default();
        for piece in src.chunks(64 << 20) {
            scanner.feed(piece);
        }
        let mut index = LineIndex::new();
        assert_eq!(index.merge(scanner.take_chunk()), Ok(()));
        let took = start.elapsed();
        println!(
            "indexed {} MB in {took:?} ({:.1} GiB/s), {} checkpoints",
            src.len() >> 20,
            f64::from(u32::try_from(src.len() >> 20).unwrap_or(u32::MAX))
                / 1024.0
                / took.as_secs_f64(),
            index.checkpoint_count()
        );
        let start = std::time::Instant::now();
        let mut sum = 0;
        for n in (0..index.lines()).step_by(997) {
            sum += index.select(&src, n).unwrap_or(0);
        }
        let queries = index.lines() / 997;
        println!(
            "select: {:?} each (checksum {sum})",
            start.elapsed() / u32::try_from(queries).unwrap_or(u32::MAX)
        );
    }
}
