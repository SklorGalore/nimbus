//! [`MappedBuffer`]: the buffer for files too big to load (M7).
//!
//! # Piece tables
//!
//! A *piece table* never changes the file it opened. The document is a list
//! of *pieces*, each naming a run of bytes in one of two places: the
//! original file (a [`Source`], usually a memory mapping) or the *add
//! buffer*, which only ever grows and holds everything typed or pasted.
//! Reading the document means reading its pieces in order. Typing "x" in the
//! middle of a 4 GB file appends "x" to the add buffer and splits the one
//! piece covering the file into three: the bytes before, the "x", the bytes
//! after. Nothing of the file is copied, which is why opening and editing a
//! huge file costs almost no memory.
//!
//! # Finding things fast
//!
//! Each entry of the piece list also records where its piece starts in the
//! document and how many line breaks come before it (*prefix sums*). Both
//! grow along the list, so "which piece holds byte 3,000,000,000?" and
//! "which piece holds the 50,000th line break?" are binary searches. Inside
//! a piece of the original file, line breaks are found with the file's
//! sparse [`LineIndex`], which scans at most the gap between two of its
//! checkpoints, or by scanning a short stretch directly. Inside an
//! add-buffer piece they are found by scanning it, which is cheap because
//! add pieces are at most [`MAX_PIECE`] long.
//!
//! The list is a plain `Vec`: human editing makes at most thousands of
//! pieces, so updating the prefix sums after an edit is a few microseconds.
//! A tree would make that logarithmic, if profiling ever asks for it.
//!
//! # Line numbers while the file is still being indexed
//!
//! Counting the line breaks of a 4 GB file takes seconds, so the original
//! file's [`LineIndex`] is built by a background worker and handed over in
//! chunks with [`MappedBuffer::merge_index`]. Until it is done, only a
//! prefix of the file is indexed: its first `F` bytes, the *frontier*.
//!
//! A piece of the original file that ends at or before `F` has a known
//! line-break count. One that reaches past `F` does not (`None`); it is
//! counted when a merge moves `F` past its end. Add-buffer pieces are
//! always known, because their bytes were counted when they were stored.
//! The original's pieces stay in file order (invariant 6), so the unknown
//! pieces are exactly the original pieces from the first one reaching past
//! `F` onward: everything before that piece, and its part below `F`, is
//! counted. The document offset where that counted part ends is `K`
//! ([`Text::indexed_bytes`]).
//!
//! Line numbers are exact up to `K`. Past it nothing is numbered:
//! [`Text::line_number`] is `None`, [`Text::len_lines`] counts only the
//! lines that start at or before `K` (a lower bound), and
//! [`Text::byte_to_line`] stops at the last of those. Editing before `K`
//! moves it along with the text, editing after it leaves it where it is,
//! and merging more of the index moves it forward until it reaches the
//! end, when the buffer is fully indexed.
//!
//! # Finding lines without line numbers
//!
//! [`Text::line_span`] works anywhere, indexed or not, by looking for the
//! nearest `\n` on either side with `memchr`, which checks 16 or 32 bytes
//! per instruction. It looks [`Limits::scan`] bytes (64 KiB) each way.
//! Further away than that the line is long, and:
//!
//! - before `K`, the index finds where it starts and ends;
//! - past `K` there is no index, so a long line is cut into *pseudo-lines*,
//!   short enough to find by scanning, so that a file without line breaks
//!   never needs a scan of the whole file.
//!
//! The rule for pseudo-lines: take each multiple of [`Limits::pseudo_line`]
//! (1 MiB) of document offset, moved back to a unit boundary (see below).
//! If it is past `K` and there is no `\n` within [`Limits::scan`] bytes
//! before it nor at it, a pseudo-line starts there. The rule depends only on
//! the bytes and `K`, never on where a lookup started, so walking forward
//! and walking backward always find the same lines. A pseudo-line has no
//! line ending: its `end` equals the next line's start. Once indexing
//! passes them, the pseudo-lines join back into one long line.
//!
//! # Bytes that are not UTF-8
//!
//! A mapped file may hold bytes that are not UTF-8: a binary file, a log
//! with a stray Latin-1 byte, a file cut off mid-character. They are kept
//! exactly. A *unit* of text is a valid UTF-8 character or one invalid
//! byte, split the way [`<[u8]>::utf8_chunks`] splits them: every byte of
//! an invalid sequence is a unit of its own. Cursor positions and the ends
//! of edits are unit boundaries ([`Text::is_char_boundary`]). Strings handed
//! out show each invalid byte as [`SUBSTITUTE`], one byte for one byte, so
//! their offsets are still file offsets, and a range between unit
//! boundaries decodes the same on its own as in place.
//!
//! Whether an offset is inside a character depends only on the 3 bytes
//! before it and the 2 after it: it is exactly when a lead byte 1 to 3
//! bytes back starts a *valid* character long enough to reach it. So the
//! check takes constant time; nothing is decoded from the start of a line.
//!
//! [`Buffer::replace`] cuts only at unit boundaries and inserts valid
//! UTF-8, which never changes how the bytes around it decode. A deletion
//! can: removing `41` ("A") from the bytes `E2 41 82 AC` joins the three
//! invalid bytes left into `€`. Undo must still put the `41` back at offset
//! 1, which is now inside a character, so [`Buffer::put`] checks bounds
//! only.
//!
//! # Sharing with other threads
//!
//! Search, indexing and saving read a [`MappedSnapshot`] on a worker while
//! the user keeps typing. Nothing is copied to make one, apart from the
//! small part of the add buffer still being written:
//!
//! - The piece list sits behind an `Arc`. Editing calls [`Arc::make_mut`],
//!   which edits in place when no snapshot shares the list and copies it
//!   first when one does (*copy on write*), so a snapshot's list never
//!   changes under it.
//! - The add buffer is a list of *sealed* segments, each an `Arc<Vec<u8>>`
//!   that is never written again, plus one *open* `Vec<u8>` that typing
//!   appends to. When the open segment would grow past [`SEGMENT`] bytes it
//!   is sealed and a new one starts. A snapshot shares the sealed segments
//!   and copies the open one, so at most [`SEGMENT`] bytes are copied, and
//!   appending afterwards (which may move the open `Vec` in memory) cannot
//!   affect it. No `unsafe` is needed for any of this.
//! - The line index sits behind an `Arc` too, and merging more of it
//!   copies its short list of chunk pointers if a snapshot shares it, so a
//!   snapshot keeps the index as it was when taken.
//!
//! # Undo without copying
//!
//! Deleting 1 GB would normally copy that gigabyte into the undo history.
//! Here a removal longer than [`COPY_LIMIT`] is recorded as the pieces that
//! held it ([`Content::Pieces`]); the bytes stay where they are, kept alive
//! by the `Arc`s the run holds, and undo splices the same pieces back in.
//!
//! # Invariants
//!
//! Everything below relies on these; the tests check them.
//!
//! 1. No piece is empty. Each entry's `at` is the total length of the
//!    pieces before it, and its `lines` is the total of their line-break
//!    counts, taking unknown counts as 0. (Binary searches find the right
//!    piece; `lines` is exact up to and including the first unknown piece,
//!    which is as far as line numbers are used.)
//! 2. A piece's `newlines` is known exactly for add-buffer pieces and for
//!    pieces of the original that end at or before the frontier `F`, and
//!    then it is the number of `\n` in its bytes. (Line lookups trust it,
//!    and skip pieces known to have none.)
//! 3. The original file's bytes and every sealed segment never change, and
//!    the open segment only grows. (A piece's bytes never change.)
//! 4. Add-buffer pieces are at most [`MAX_PIECE`] bytes. (Scanning one for
//!    line breaks is bounded.)
//! 5. In a valid UTF-8 document made by typing, every piece starts and ends
//!    on a character boundary, so a range inside one piece can be lent out
//!    as a `&str` without copying. Correctness never depends on this: a
//!    deletion that joins invalid bytes into a character can leave a piece
//!    boundary inside it, and readers only need ranges on unit boundaries.
//! 6. Pieces of the original file appear in increasing file order and do
//!    not overlap. Edits only cut pieces and add new add-buffer pieces, and
//!    undo puts pieces back exactly where they came from, so this holds.
//!    Saving (M7 task 12) relies on it to stream the file in order, and `K`
//!    relies on it to be a single place.
//! 7. A [`PieceRun`] only refers to sealed segments (the open one is sealed
//!    first), so the run's own `Arc`s keep every byte it names alive.
//! 8. The cached frontier (the first unknown piece, `K` and the lines
//!    before it) matches the piece list and the index: it is brought up to
//!    date after every edit and every merge.

use std::borrow::Cow;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use memchr::{memchr, memchr_iter, memrchr, memrchr_iter};

use crate::buffer::{
    Buffer, Content, Edit, EditError, LineSpan, SUBSTITUTE, Snapshot, Text, check_range,
    check_sorted,
};
use crate::line_index::{IndexChunk, LineIndex, MergeError, Spacing, meter};
use crate::source::{Source, VecSource};

/// Gives each new document's stores a number of their own (its lineage).
static NEXT_LINEAGE: AtomicU64 = AtomicU64::new(1);

/// How many bytes at each end of the old source
/// [`MappedBuffer::extend_source`] compares with the new one.
const EXTEND_CHECK: usize = 64;

/// Why [`MappedBuffer::extend_source`] refused a new source.
#[derive(Debug)]
pub enum ExtendError {
    /// The new source is shorter than the old: the file was cut short, and
    /// must be opened again.
    Shorter {
        /// The old source's length.
        old: usize,
        /// The new source's length.
        new: usize,
    },
    /// The new source does not start with the old one's bytes: the file
    /// was rewritten or replaced, not appended to.
    Differs,
    /// Reading either source to compare them failed.
    Read(std::io::Error),
}

impl fmt::Display for ExtendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shorter { old, new } => {
                write!(f, "the file shrank from {old} to {new} bytes")
            }
            Self::Differs => f.write_str("the file was rewritten, not appended to"),
            Self::Read(e) => write!(f, "could not compare the file: {e}"),
        }
    }
}

impl std::error::Error for ExtendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read(e) => Some(e),
            _ => None,
        }
    }
}

/// Removals longer than this many bytes are kept in undo as pieces instead
/// of copied: 256 KiB.
pub const COPY_LIMIT: usize = 256 * 1024;

/// The open add-buffer segment is sealed before it grows past this many
/// bytes: 64 KiB, the most a snapshot copies.
pub const SEGMENT: usize = 64 * 1024;

/// Inserted text is split into pieces of at most this many bytes: 1 MiB.
pub const MAX_PIECE: usize = 1024 * 1024;

/// How far [`Text::line_span`] looks for a line break on each side before
/// treating the line as long: 64 KiB, the size at which
/// [`crate::long_line`] starts reading lines in windows.
pub const SCAN: usize = 64 * 1024;

/// Past the indexed part of a file, long lines are cut into pseudo-lines at
/// multiples of this many bytes: 1 MiB, the line index's checkpoint
/// spacing, so a lookup scans about as far either way.
pub const PSEUDO_LINE: usize = 1024 * 1024;

/// Sizes that trade memory against copying and scanning. The defaults are
/// the constants above; tests use tiny ones so that every path runs on
/// small texts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Removals longer than this are recorded as [`Content::Pieces`].
    pub copy_limit: usize,
    /// The open add-buffer segment is sealed before growing past this
    /// (at least 1). Text longer than this gets a sealed segment of its own.
    pub segment: usize,
    /// Add-buffer pieces are at most this long (at least 4, the longest
    /// UTF-8 character, so a piece can always end on a character boundary).
    pub max_piece: usize,
    /// How far [`Text::line_span`] scans for a line break on each side
    /// before turning to the line index or to pseudo-lines (at least 1).
    /// Also the longest stretch of the original file whose line breaks are
    /// counted by scanning it rather than through the index.
    pub scan: usize,
    /// Spacing of pseudo-line breaks in the part of the document not yet
    /// indexed (see the module documentation). At least `scan + 4`, so a
    /// scan window never holds two of them.
    pub pseudo_line: usize,
}

impl Limits {
    /// The default limits: [`COPY_LIMIT`], [`SEGMENT`], [`MAX_PIECE`],
    /// [`SCAN`] and [`PSEUDO_LINE`].
    pub const DEFAULT: Self = Self {
        copy_limit: COPY_LIMIT,
        segment: SEGMENT,
        max_piece: MAX_PIECE,
        scan: SCAN,
        pseudo_line: PSEUDO_LINE,
    };

    /// Each size raised to its minimum.
    fn normalized(self) -> Self {
        let scan = self.scan.max(1);
        Self {
            copy_limit: self.copy_limit,
            segment: self.segment.max(1),
            max_piece: self.max_piece.max(4),
            scan,
            pseudo_line: self.pseudo_line.max(scan.saturating_add(4)),
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Where a piece's bytes live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Store {
    /// The original file.
    Original,
    /// Add-buffer segment number `n`: sealed segment `n`, or the open one
    /// when `n` equals the number of sealed segments.
    Add(usize),
}

/// A run of bytes in one store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Piece {
    store: Store,
    /// Offset of the first byte in the store.
    start: usize,
    /// Length in bytes; never 0 in a piece list.
    len: usize,
    /// How many `\n` bytes it holds; `None` for a piece of the original
    /// reaching past the indexed frontier (invariant 2).
    newlines: Option<usize>,
}

/// A piece in the document's list, with its prefix sums.
#[derive(Debug, Clone, Copy)]
struct Entry {
    piece: Piece,
    /// Where the piece starts in the document.
    at: usize,
    /// How many `\n` come before it in the document, taking unknown counts
    /// as 0: exact up to and including the first unknown piece.
    lines: usize,
}

/// The add buffer's sealed segments. The outer `Arc` lets snapshots and
/// undo runs share the list; the inner ones let a new list share segments.
type Segments = Arc<Vec<Arc<Vec<u8>>>>;

/// The bytes `piece` names, given the stores. Empty if the piece does not
/// fit its store, which the invariants rule out; reading never panics.
fn piece_bytes<'a>(
    source: &'a dyn Source,
    sealed: &'a [Arc<Vec<u8>>],
    open: &'a [u8],
    piece: &Piece,
) -> &'a [u8] {
    let store: &[u8] = match piece.store {
        Store::Original => source.bytes(),
        Store::Add(n) if n == sealed.len() => open,
        Store::Add(n) => sealed.get(n).map_or(&[], |s| s.as_slice()),
    };
    store
        .get(piece.start..piece.start.saturating_add(piece.len))
        .unwrap_or_default()
}

/// Append bytes `from..to` of `piece` to `out`. Original bytes are read
/// with [`Source::read_at`] (a file read for a mapped file), the add
/// buffer's straight from memory.
fn append_piece(
    source: &dyn Source,
    sealed: &[Arc<Vec<u8>>],
    open: &[u8],
    piece: &Piece,
    (from, to): (usize, usize),
    out: &mut Vec<u8>,
) -> std::io::Result<()> {
    let to = to.min(piece.len);
    if from >= to {
        return Ok(());
    }
    if piece.store == Store::Original {
        let old = out.len();
        out.resize(old + (to - from), 0);
        let read = source.read_at(piece.start + from, &mut out[old..]);
        if read.is_err() {
            out.truncate(old);
        }
        return read;
    }
    let bytes = piece_bytes(source, sealed, open, piece);
    out.extend_from_slice(bytes.get(from..to).unwrap_or_default());
    Ok(())
}

/// Count the `\n` bytes in `bytes`.
#[expect(
    clippy::naive_bytecount,
    reason = "the compiler vectorises this loop; not worth a crate"
)]
fn count_nl(bytes: &[u8]) -> usize {
    meter::add(bytes.len());
    bytes.iter().filter(|&&b| b == b'\n').count()
}

/// Whether `b` continues a multi-byte UTF-8 character (`10xxxxxx`).
fn is_continuation(b: u8) -> bool {
    b & 0xC0 == 0x80
}

/// Length of the valid UTF-8 character at the start of `bytes`, if one is
/// there whole: 1 to 4. `None` for a byte that cannot start one, or a start
/// whose following bytes are missing or wrong: truncated, overlong, a
/// surrogate, or past U+10FFFF.
fn scalar_len(bytes: &[u8]) -> Option<usize> {
    let n = match *bytes.first()? {
        0x00..=0x7F => 1,
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return None,
    };
    std::str::from_utf8(bytes.get(..n)?).ok().map(|_| n)
}

/// `bytes` as text: lent as is when valid UTF-8, otherwise copied with each
/// invalid byte replaced by [`SUBSTITUTE`] (same length, same offsets).
fn decode(bytes: &[u8]) -> Cow<'_, str> {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Cow::Borrowed(s);
    }
    let mut s = String::with_capacity(bytes.len());
    push_substituted(&mut s, bytes);
    Cow::Owned(s)
}

/// Append `bytes` to `out`, each invalid byte as [`SUBSTITUTE`].
fn push_substituted(out: &mut String, bytes: &[u8]) {
    for chunk in bytes.utf8_chunks() {
        out.push_str(chunk.valid());
        for _ in chunk.invalid() {
            out.push(SUBSTITUTE);
        }
    }
}

/// `n` as a `usize`, saturating. Lossless on 64-bit targets, and a file
/// over 4 GB cannot be mapped on a 32-bit one anyway.
fn to_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

/// `n` as a `u64`. Lossless on every platform Rust supports.
fn to_u64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// Where line numbers stop being known (see "Line numbers while the file is
/// still being indexed" above). Cached, because nearly every line query
/// needs it.
#[derive(Debug, Clone, Copy, Default)]
struct Frontier {
    /// Index of the first entry whose line-break count is unknown, or the
    /// number of entries when every count is known.
    entry: usize,
    /// `K`: line numbers are exact up to this document offset.
    offset: usize,
    /// How many `\n` come before `K`.
    lines: usize,
    /// `base` is the number of `\n` in the original file before offset
    /// `base_at`, kept for the first unknown piece's start so that line
    /// lookups in that piece need not ask the index for it again.
    base_at: usize,
    base: usize,
}

/// A mapped document as it is at one moment: the piece list, the stores it
/// points into and the original file's line index. A [`MappedBuffer`] edits
/// one of these in place, and [`Buffer::snapshot`] hands out a copy of it
/// that later edits do not affect. It is `Send + Sync`, so a worker thread
/// can read it.
#[derive(Clone)]
pub struct MappedSnapshot {
    /// The original file.
    source: Arc<dyn Source>,
    /// Which document's stores these are: set when the document is made,
    /// and kept when its source grows ([`MappedBuffer::extend_source`]),
    /// so a [`PieceRun`] recorded before the growth still belongs here.
    lineage: u64,
    /// Line breaks of the original file, perhaps only of a prefix of it.
    /// Shared with snapshots, and copied on write like the piece list.
    index: Arc<LineIndex>,
    /// Full add-buffer segments, never written again.
    sealed: Segments,
    /// The add-buffer segment being appended to. Its number is
    /// `sealed.len()`. A snapshot holds a copy of it.
    open: Vec<u8>,
    /// The document: pieces in order, with prefix sums.
    table: Arc<Vec<Entry>>,
    limits: Limits,
    frontier: Frontier,
}

impl MappedSnapshot {
    /// The unedited document of `source`, whose line index is `index`. The
    /// index must not cover more than the source.
    fn new(source: Arc<dyn Source>, index: LineIndex, limits: Limits) -> Self {
        let len = source.len();
        let mut snap = Self {
            source,
            lineage: NEXT_LINEAGE.fetch_add(1, Ordering::Relaxed),
            index: Arc::new(index),
            sealed: Arc::new(Vec::new()),
            open: Vec::new(),
            table: Arc::new(Vec::new()),
            limits,
            frontier: Frontier::default(),
        };
        if len > 0 {
            let newlines = (snap.original_indexed() >= len).then(|| to_usize(snap.index.lines()));
            snap.table = Arc::new(vec![Entry {
                piece: Piece {
                    store: Store::Original,
                    start: 0,
                    len,
                    newlines,
                },
                at: 0,
                lines: 0,
            }]);
        }
        snap.refresh(0);
        snap
    }

    fn len(&self) -> usize {
        self.table.last().map_or(0, |e| e.at + e.piece.len)
    }

    /// `F`: how many bytes of the original file the line index covers.
    fn original_indexed(&self) -> usize {
        to_usize(self.index.indexed()).min(self.source.len())
    }

    fn bytes(&self, piece: &Piece) -> &[u8] {
        piece_bytes(&*self.source, &self.sealed, &self.open, piece)
    }

    /// Index of the piece holding `byte`, which must be before the end.
    fn find(&self, byte: usize) -> usize {
        self.table
            .partition_point(|e| e.at <= byte)
            .saturating_sub(1)
    }

    /// The entry holding `byte`; `None` at or past the end.
    fn entry(&self, byte: usize) -> Option<&Entry> {
        if byte >= self.len() {
            return None;
        }
        self.table.get(self.find(byte))
    }

    /// Copy the document's bytes from offset `from` into `buf`, as many as
    /// fit and exist, and return how many were copied.
    fn peek(&self, from: usize, buf: &mut [u8]) -> usize {
        let end = from.saturating_add(buf.len()).min(self.len());
        let mut n = 0;
        let mut pos = from;
        for e in self.table.iter().skip(self.find(from)) {
            if pos >= end {
                break;
            }
            let Some(off) = pos.checked_sub(e.at) else {
                break;
            };
            let part = self.bytes(&e.piece).get(off..).unwrap_or_default();
            let take = part.len().min(end - pos);
            for (dst, &src) in buf.iter_mut().skip(n).zip(part.iter().take(take)) {
                *dst = src;
            }
            n += take;
            pos += take;
        }
        n
    }

    /// The last unit boundary at or before `byte` (within the text): at
    /// most 3 bytes back.
    fn floor_boundary(&self, mut byte: usize) -> usize {
        while byte > 0 && !self.is_char_boundary(byte) {
            byte -= 1;
        }
        byte
    }

    /// Line breaks in the original file before offset `x`, which must be
    /// indexed. Scans at most one checkpoint gap of the index.
    fn rank(&self, x: usize) -> Option<usize> {
        let r = self.index.rank(self.source.bytes(), to_u64(x))?;
        Some(to_usize(r))
    }

    /// Offset in the original file of its line break number `n`, counting
    /// from 1, which must be indexed.
    fn select_nl(&self, n: usize) -> Option<usize> {
        let start = self.index.select(self.source.bytes(), to_u64(n))?;
        to_usize(start).checked_sub(1)
    }

    /// Line breaks in bytes `a..z` of the original file, which must be
    /// indexed (`z <= F`): scanned if short, otherwise from the index.
    fn count_original(&self, a: usize, z: usize) -> Option<usize> {
        if z <= a {
            return Some(0);
        }
        if z - a <= self.limits.scan {
            return Some(count_nl(self.source.bytes().get(a..z)?));
        }
        Some(self.rank(z)?.saturating_sub(self.rank(a)?))
    }

    /// Line breaks in bytes `from..to` of `piece`; `None` if they reach past
    /// the indexed part of the original file.
    fn count(&self, piece: &Piece, from: usize, to: usize) -> Option<usize> {
        let to = to.min(piece.len);
        if from >= to {
            return Some(0);
        }
        if from == 0 && to == piece.len {
            return piece.newlines;
        }
        if piece.store == Store::Original && piece.start + to > self.original_indexed() {
            return None;
        }
        let bytes = self.bytes(piece);
        let part = |a: usize, z: usize| count_nl(bytes.get(a..z).unwrap_or_default());
        // Scan whichever is short: the range, or what is outside it.
        let short = match piece.store {
            Store::Add(_) => piece.len / 2,
            Store::Original => self.limits.scan,
        };
        let inside = to - from;
        if inside <= short {
            return Some(part(from, to));
        }
        if let Some(total) = piece.newlines
            && piece.len - inside <= short
        {
            return Some(total.saturating_sub(part(0, from) + part(to, piece.len)));
        }
        match piece.store {
            Store::Add(_) => Some(part(from, to)),
            Store::Original => self.count_original(piece.start + from, piece.start + to),
        }
    }

    /// Offset in `piece` of its line break number `k` (from 0). Only asked
    /// about line breaks before `K`, which are all counted.
    fn nth_newline(&self, piece: &Piece, k: usize) -> Option<usize> {
        let bytes = self.bytes(piece);
        match piece.store {
            Store::Add(_) => {
                let total = piece.newlines?;
                if k >= total {
                    return None;
                }
                meter::add(bytes.len());
                // Scan from whichever end is nearer.
                if k < total / 2 {
                    memchr_iter(b'\n', bytes).nth(k)
                } else {
                    memrchr_iter(b'\n', bytes).nth(total - 1 - k)
                }
            }
            Store::Original if piece.len <= self.limits.scan => {
                meter::add(bytes.len());
                memchr_iter(b'\n', bytes).nth(k)
            }
            Store::Original => {
                let base = if piece.start == self.frontier.base_at {
                    self.frontier.base
                } else {
                    self.rank(piece.start)?
                };
                let at = self.select_nl(base.checked_add(k)?.checked_add(1)?)?;
                at.checked_sub(piece.start).filter(|&off| off < piece.len)
            }
        }
    }

    /// Document offset of line break number `n` (from 0), if it is before
    /// `K`.
    fn newline_pos(&self, n: usize) -> Option<usize> {
        if n >= self.frontier.lines {
            return None;
        }
        // The entries before the first unknown one have exact prefix sums;
        // a line break past all of them is in that piece's counted part.
        let known = self.table.get(..self.frontier.entry).unwrap_or(&self.table);
        let i = known.partition_point(|e| e.lines + e.piece.newlines.unwrap_or(0) <= n);
        let e = self.table.get(i)?;
        Some(e.at + self.nth_newline(&e.piece, n.checked_sub(e.lines)?)?)
    }

    /// How many `\n` come before `byte`, for `byte` up to `K`; for any
    /// later byte, how many come before `K`.
    fn lines_before(&self, byte: usize) -> usize {
        if byte >= self.frontier.offset {
            return self.frontier.lines;
        }
        let Some(e) = self.entry(byte) else {
            return self.frontier.lines;
        };
        // No need to count in a piece known to have no line break before
        // K, which is every piece of a file without line breaks.
        let after = match e.piece.newlines {
            Some(n) => e.lines + n,
            None => self.frontier.lines,
        };
        if after == e.lines {
            return e.lines;
        }
        e.lines + self.count(&e.piece, 0, byte - e.at).unwrap_or(0)
    }

    /// Where the content of the line starting at `start` ends, given where
    /// the next line starts: before the `\n`, and before a `\r` ahead of
    /// it. A pseudo-line has no line ending, so it ends where the next
    /// starts.
    fn line_end(&self, start: usize, next: Option<usize>) -> usize {
        let Some(next) = next else {
            return self.len();
        };
        match next.checked_sub(1) {
            Some(lf) if self.byte_at(lf) == Some(b'\n') => {
                if lf > start && self.byte_at(lf - 1) == Some(b'\r') {
                    lf - 1
                } else {
                    lf
                }
            }
            _ => next,
        }
    }

    /// Offset of the last `\n` in document bytes `lo..end`. Pieces known to
    /// hold no line break are skipped without reading them.
    fn prev_nl(&self, end: usize, lo: usize) -> Option<usize> {
        let end = end.min(self.len());
        if lo >= end {
            return None;
        }
        let mut i = self.find(end - 1);
        loop {
            let e = self.table.get(i)?;
            let from = lo.max(e.at) - e.at;
            let to = end.min(e.at + e.piece.len).saturating_sub(e.at);
            if from < to && e.piece.newlines != Some(0) {
                let bytes = self.bytes(&e.piece).get(from..to).unwrap_or_default();
                meter::add(bytes.len());
                if let Some(p) = memrchr(b'\n', bytes) {
                    return Some(e.at + from + p);
                }
            }
            if e.at <= lo {
                return None;
            }
            i = i.checked_sub(1)?;
        }
    }

    /// Offset of the first `\n` in document bytes `start..hi`. Pieces known
    /// to hold no line break are skipped without reading them.
    fn next_nl(&self, start: usize, hi: usize) -> Option<usize> {
        let hi = hi.min(self.len());
        if start >= hi {
            return None;
        }
        for e in self.table.iter().skip(self.find(start)) {
            if e.at >= hi {
                break;
            }
            let from = start.max(e.at) - e.at;
            let to = hi.min(e.at + e.piece.len) - e.at;
            if from < to && e.piece.newlines != Some(0) {
                let bytes = self.bytes(&e.piece).get(from..to).unwrap_or_default();
                meter::add(bytes.len());
                if let Some(p) = memchr(b'\n', bytes) {
                    return Some(e.at + from + p);
                }
            }
        }
        None
    }

    /// Pseudo-line point number `m` (from 1): `m * pseudo_line` moved back
    /// to a unit boundary, if that multiple is inside the text.
    fn pseudo(&self, m: usize) -> Option<usize> {
        let x = m
            .checked_mul(self.limits.pseudo_line)
            .filter(|&x| x > 0 && x < self.len())?;
        Some(self.floor_boundary(x))
    }

    /// The last pseudo-line point at or before `byte`.
    fn pseudo_at_or_before(&self, byte: usize) -> Option<usize> {
        let m = byte / self.limits.pseudo_line;
        // Point m + 1 is moved back by up to 3 bytes, so it may land at or
        // before `byte`.
        if let Some(q) = self.pseudo(m + 1).filter(|&q| q <= byte) {
            return Some(q);
        }
        // Point m is at or before `byte`. It is missing only when its
        // multiple is the end of the text; then point m - 1 is the one.
        self.pseudo(m).or_else(|| self.pseudo(m.checked_sub(1)?))
    }

    /// The first pseudo-line point after `byte`.
    fn pseudo_after(&self, byte: usize) -> Option<usize> {
        let m = byte / self.limits.pseudo_line;
        let q = self.pseudo(m + 1)?;
        if q > byte {
            Some(q)
        } else {
            self.pseudo(m + 2)
        }
    }

    /// Where the line holding `byte` (at most the length) starts: the last
    /// line start at or before it, by the rules in the module docs.
    fn line_start_at(&self, byte: usize) -> usize {
        let scan = self.limits.scan;
        let k = self.frontier.offset;
        // `[free, byte)` is known to hold no line break.
        let mut free = byte.saturating_sub(scan);
        if let Some(p) = self.prev_nl(byte, free) {
            return p + 1;
        }
        if free == 0 {
            return 0;
        }
        if byte <= k {
            // A long line in the indexed part: the index knows its start.
            return self.line_to_byte(self.lines_before(byte));
        }
        // Past K the line starts after a `\n`, at a pseudo-line break, or
        // where the line holding K starts. Pseudo-line points are more than
        // `scan` apart, so only the nearest one at or before `byte` can be
        // the start, unless it is `byte` itself and a `\n` there stops it
        // being a break; then it is the one before.
        let mut cand = self.pseudo_at_or_before(byte);
        loop {
            match cand {
                Some(q) if q > k => {
                    let lo = q.saturating_sub(scan);
                    if lo < free {
                        // A `\n` here starts the line, after q or (being in
                        // q's window) instead of it.
                        if let Some(p) = self.prev_nl(free, lo) {
                            return p + 1;
                        }
                        free = lo;
                    }
                    // No `\n` in `[q - scan, byte)`: q is a break unless a
                    // `\n` sits at q, which can only be when q is `byte`.
                    if q < byte || self.byte_at(q) != Some(b'\n') {
                        return q;
                    }
                    cand = q.checked_sub(1).and_then(|x| self.pseudo_at_or_before(x));
                }
                _ => {
                    // No pseudo-line break in `(K, byte]`: the line starts
                    // after the last `\n` from K on, or is the one holding K.
                    if k < free
                        && let Some(p) = self.prev_nl(free, k)
                    {
                        return p + 1;
                    }
                    return self.line_to_byte(self.frontier.lines);
                }
            }
        }
    }

    /// Where the line after the one holding `byte` (before the end) starts,
    /// by the rules in the module docs; `None` if that is the last line.
    fn next_line_start(&self, byte: usize) -> Option<usize> {
        let scan = self.limits.scan;
        let k = self.frontier.offset;
        let len = self.len();
        // `[byte, from)` is known to hold no line break.
        let mut from = byte;
        if byte < k {
            let near = byte.saturating_add(scan).min(k);
            if let Some(p) = self.next_nl(byte, near) {
                return Some(p + 1);
            }
            if near < k {
                let line = self.lines_before(byte);
                if line < self.frontier.lines {
                    // The next line starts by K: the index knows where.
                    return Some(self.line_to_byte(line + 1));
                }
            }
            // No `\n` in `[byte, K)`, so K is not a line start either (the
            // byte before it would be a `\n`): carry on from K.
            from = k;
        }
        let mut cand = self.pseudo_after(from);
        loop {
            // Scan up to and including the candidate point, because a
            // `\n` at it means it is not a break.
            let limit = cand.map_or(len, |q| (q + 1).min(len));
            if let Some(p) = self.next_nl(from, limit) {
                return Some(p + 1);
            }
            let q = cand?;
            // `[byte, q]` holds no `\n`. q is a break unless one is in the
            // rest of its window, before `byte`.
            let lo = q.saturating_sub(scan);
            if lo < byte && self.prev_nl(byte, lo).is_some() {
                // The next point is more than `scan` past q, so its window
                // is all in `[byte, ...)`: it will be decided by the scan.
                from = q + 1;
                cand = self.pseudo_after(q);
                continue;
            }
            return Some(q);
        }
    }

    /// `piece` cut to its bytes `from..to`.
    fn cut(&self, piece: &Piece, from: usize, to: usize) -> Piece {
        if from == 0 && to >= piece.len {
            return *piece;
        }
        Piece {
            store: piece.store,
            start: piece.start + from,
            len: to - from,
            newlines: self.count(piece, from, to),
        }
    }

    /// Push the pieces covering document `range` to `out`, cut to fit,
    /// starting the search at entry `i`. Returns the index of the last
    /// entry used, where a following range can resume.
    fn copy_range(&self, mut i: usize, range: Range<usize>, out: &mut Vec<Piece>) -> usize {
        if range.is_empty() {
            return i;
        }
        while self
            .table
            .get(i)
            .is_some_and(|e| e.at + e.piece.len <= range.start)
        {
            i += 1;
        }
        while let Some(e) = self.table.get(i) {
            if e.at >= range.end {
                break;
            }
            let from = range.start.saturating_sub(e.at);
            let to = (range.end - e.at).min(e.piece.len);
            out.push(self.cut(&e.piece, from, to));
            if e.at + e.piece.len >= range.end {
                break;
            }
            i += 1;
        }
        i
    }

    /// Append the raw bytes of document `range` (within the text) to `out`.
    fn read_raw(&self, range: Range<usize>, out: &mut Vec<u8>) {
        if range.is_empty() {
            return;
        }
        for e in self.table.iter().skip(self.find(range.start)) {
            if e.at >= range.end {
                break;
            }
            let from = range.start.saturating_sub(e.at);
            let to = (range.end - e.at).min(e.piece.len);
            out.extend_from_slice(self.bytes(&e.piece).get(from..to).unwrap_or_default());
        }
    }

    /// `range` clamped to the text and widened outward to unit boundaries,
    /// as [`Text::slice`] promises.
    fn widen(&self, range: Range<usize>) -> Range<usize> {
        let len = self.len();
        let mut start = range.start.min(len);
        let mut end = range.end.min(len).max(start);
        while start > 0 && !self.is_char_boundary(start) {
            start -= 1;
        }
        while end < len && !self.is_char_boundary(end) {
            end += 1;
        }
        start..end
    }

    /// The text in `range` (within the text, on unit boundaries). Lent
    /// without copying when it lies in one piece and is valid UTF-8.
    fn get(&self, range: Range<usize>) -> Cow<'_, str> {
        if range.is_empty() {
            return Cow::Borrowed("");
        }
        let Some(e) = self.entry(range.start) else {
            return Cow::Borrowed("");
        };
        if range.end <= e.at + e.piece.len {
            let bytes = self.bytes(&e.piece);
            return decode(
                bytes
                    .get(range.start - e.at..range.end - e.at)
                    .unwrap_or_default(),
            );
        }
        let mut v = Vec::with_capacity(range.len());
        self.read_raw(range, &mut v);
        match String::from_utf8(v) {
            Ok(s) => Cow::Owned(s),
            Err(e) => {
                let mut s = String::with_capacity(e.as_bytes().len());
                push_substituted(&mut s, e.as_bytes());
                Cow::Owned(s)
            }
        }
    }

    /// Whether `run` refers to this document's stores: the same original
    /// file (or an earlier, shorter mapping of it, see
    /// [`MappedBuffer::extend_source`]) and the very same sealed segments,
    /// each piece within its store.
    fn owns(&self, run: &PieceRun) -> bool {
        let run = &*run.0;
        let fits = |p: &Piece, store_len: usize| {
            p.start
                .checked_add(p.len)
                .is_some_and(|end| end <= store_len)
        };
        self.lineage == run.lineage
            && run.pieces.iter().all(|p| match p.store {
                Store::Original => fits(p, self.source.len()),
                Store::Add(n) => match (self.sealed.get(n), run.sealed.get(n)) {
                    (Some(mine), Some(theirs)) => Arc::ptr_eq(mine, theirs) && fits(p, mine.len()),
                    _ => false,
                },
            })
    }

    /// Bring the cached frontier up to date (invariant 8) after the entries
    /// from `from` on changed or the index grew. Entries before `from` must
    /// be as they were when the frontier was last computed.
    fn refresh(&mut self, from: usize) {
        let old = self.frontier;
        let count = self.table.len();
        let entry = if old.entry < from {
            old.entry
        } else {
            let rest = self.table.get(from..).unwrap_or_default();
            rest.iter()
                .position(|e| e.piece.newlines.is_none())
                .map_or(count, |i| from + i)
        };
        let f = self.original_indexed();
        self.frontier = match self.table.get(entry) {
            None => Frontier {
                entry: count,
                offset: self.len(),
                lines: self
                    .table
                    .last()
                    .map_or(0, |e| e.lines + e.piece.newlines.unwrap_or(0)),
                ..old
            },
            Some(e) => {
                let p = e.piece;
                let base = if p.start >= f {
                    None
                } else if p.start == old.base_at {
                    Some(old.base)
                } else {
                    self.rank(p.start)
                };
                match base {
                    // The piece's part below F is counted by the index.
                    Some(base) => Frontier {
                        entry,
                        offset: e.at + (f - p.start),
                        lines: e.lines + to_usize(self.index.lines()).saturating_sub(base),
                        base_at: p.start,
                        base,
                    },
                    None => Frontier {
                        entry,
                        offset: e.at,
                        lines: e.lines,
                        ..old
                    },
                }
            }
        };
    }

    /// After the index grew: count the pieces of the original it now covers
    /// whole (invariant 2), then move the frontier.
    fn fill_counts(&mut self) {
        let from = self.frontier.entry;
        let f = self.original_indexed();
        let mut counted = Vec::new();
        for (i, e) in self.table.iter().enumerate().skip(from) {
            let p = &e.piece;
            if p.store != Store::Original || p.newlines.is_some() {
                continue;
            }
            // Pieces of the original are in file order, so once one
            // reaches past F, so do all after it.
            if p.start + p.len > f {
                break;
            }
            if let Some(n) = self.count_original(p.start, p.start + p.len) {
                counted.push((i, n));
            }
        }
        if !counted.is_empty() {
            let entries = Arc::make_mut(&mut self.table);
            for (i, n) in counted {
                if let Some(e) = entries.get_mut(i) {
                    e.piece.newlines = Some(n);
                }
            }
            fix_prefix(entries, from);
        }
        self.refresh(from);
    }
}

impl Text for MappedSnapshot {
    fn len_bytes(&self) -> usize {
        self.len()
    }

    /// The lines starting at or before `K`: all of them once fully indexed,
    /// a lower bound before.
    fn len_lines(&self) -> usize {
        self.frontier.lines + 1
    }

    /// Lines past [`Text::len_lines`] are empty. While indexing, the last
    /// counted line may continue past `K`; it is then cut where its
    /// [`Text::line_span`] ends, which may be a pseudo-line break.
    fn line(&self, idx: usize) -> Cow<'_, str> {
        if idx > self.frontier.lines {
            return Cow::Borrowed("");
        }
        let span = self.line_span(self.line_to_byte(idx));
        self.get(span.start..span.end)
    }

    /// Exact up to `K`; any later byte gives the last line counted so far.
    fn byte_to_line(&self, byte: usize) -> usize {
        self.lines_before(byte)
    }

    /// Lines past [`Text::len_lines`], which may not be counted yet, give
    /// the length.
    fn line_to_byte(&self, line: usize) -> usize {
        match line.checked_sub(1) {
            None => 0,
            Some(n) => self.newline_pos(n).map_or(self.len(), |p| p + 1),
        }
    }

    fn slice(&self, range: Range<usize>) -> Cow<'_, str> {
        self.get(self.widen(range))
    }

    /// True at unit boundaries: between two valid characters, either side
    /// of an invalid byte, and at both ends. Looks at no more than 3 bytes
    /// back and 2 ahead.
    fn is_char_boundary(&self, byte: usize) -> bool {
        let len = self.len();
        if byte >= len {
            return byte == len;
        }
        // Only a continuation byte (10xxxxxx) can be inside a character.
        if !self.byte_at(byte).is_some_and(is_continuation) {
            return true;
        }
        // It is inside one when a lead byte k = 1 to 3 bytes back starts a
        // valid character more than k bytes long. Read those bytes: from 3
        // back to 2 ahead.
        let from = byte.saturating_sub(3);
        let mut buf = [0u8; 6];
        let n = self.peek(from, &mut buf);
        let window = buf.get(..n).unwrap_or_default();
        !(1..=byte - from).any(|k| {
            window
                .get(byte - k - from..)
                .and_then(scalar_len)
                .is_some_and(|len| len > k)
        })
    }

    fn chunk_at(&self, byte: usize) -> (&[u8], usize) {
        match self.entry(byte) {
            Some(e) => (self.bytes(&e.piece), e.at),
            None => (&[], self.len()),
        }
    }

    fn byte_at(&self, byte: usize) -> Option<u8> {
        let e = self.entry(byte)?;
        self.bytes(&e.piece).get(byte - e.at).copied()
    }

    /// Finds the line around `byte` by looking for the nearest line breaks
    /// either side of it, then turning to the index or to pseudo-lines for
    /// long lines (see the module documentation). Each lookup scans at most
    /// about two pseudo-lines plus two index checkpoint gaps, however long
    /// the line or the document.
    fn line_span(&self, byte: usize) -> LineSpan {
        let len = self.len();
        let byte = byte.min(len);
        let start = self.line_start_at(byte);
        let next = if byte < len {
            self.next_line_start(byte)
        } else {
            None
        };
        LineSpan {
            start,
            end: self.line_end(start, next),
            next,
        }
    }

    /// `None` past `K` while the document is not fully indexed.
    fn line_number(&self, byte: usize) -> Option<usize> {
        let k = self.frontier.offset;
        (byte <= k || k == self.len()).then(|| self.lines_before(byte))
    }

    fn indexed_bytes(&self) -> usize {
        self.frontier.offset
    }

    fn read_into(&self, range: Range<usize>, out: &mut Vec<u8>) -> std::io::Result<()> {
        let end = range.end.min(self.len());
        let start = range.start.min(end);
        if start == end {
            return Ok(());
        }
        out.reserve(end - start);
        // Bulk reads go through the file, not the mapping (see
        // `Source::read_at`); text on screen still reads the mapping.
        for e in self.table.iter().skip(self.find(start)) {
            if e.at >= end {
                break;
            }
            let from = start.saturating_sub(e.at);
            let to = end - e.at;
            append_piece(
                &*self.source,
                &self.sealed,
                &self.open,
                &e.piece,
                (from, to),
                out,
            )?;
        }
        Ok(())
    }

    fn slice_into(&self, range: Range<usize>, out: &mut String) {
        let range = self.widen(range);
        let Some(e) = self.entry(range.start) else {
            return;
        };
        if range.end <= e.at + e.piece.len {
            // One piece: no copy beyond `out` itself.
            let bytes = self.bytes(&e.piece);
            let part = bytes
                .get(range.start - e.at..range.end - e.at)
                .unwrap_or_default();
            match std::str::from_utf8(part) {
                Ok(s) => out.push_str(s),
                Err(_) => push_substituted(out, part),
            }
            return;
        }
        // Across pieces: gather the bytes in `out`'s own storage, then check
        // them as a whole (a character may straddle two pieces).
        let mut bytes = std::mem::take(out).into_bytes();
        let kept = bytes.len();
        self.read_raw(range, &mut bytes);
        *out = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => {
                let bytes = e.into_bytes();
                let (before, added) = bytes.split_at(kept);
                let mut s = String::with_capacity(bytes.len());
                // What `out` held was a `String`, so valid.
                push_substituted(&mut s, before);
                push_substituted(&mut s, added);
                s
            }
        };
    }
}

/// Prints sizes only: the stores may be gigabytes.
impl fmt::Debug for MappedSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MappedSnapshot")
            .field("len", &self.len())
            .field("indexed", &self.frontier.offset)
            .field("pieces", &self.table.len())
            .field("sealed_segments", &self.sealed.len())
            .field("open_segment", &self.open.len())
            .finish_non_exhaustive()
    }
}

/// Text removed from a mapped document, recorded as the pieces that held it
/// instead of as a copy (see [`Content::Pieces`]). It holds `Arc`s to the
/// original file and to the add-buffer segments it names, so those bytes
/// stay alive as long as the run does, even if the document lets go of
/// them.
///
/// The data sits behind one more `Arc`, so a run is a single pointer:
/// cloning it is one counter increment, and [`Content`] (and so every
/// [`Edit`] in the undo history) stays as small as a `Vec`.
#[derive(Clone)]
pub struct PieceRun(Arc<Run>);

/// What a [`PieceRun`] points to.
struct Run {
    pieces: Vec<Piece>,
    len: usize,
    /// `None` if a piece's count was unknown when the run was made.
    newlines: Option<usize>,
    source: Arc<dyn Source>,
    /// The document's lineage ([`MappedSnapshot`]).
    lineage: u64,
    /// The document's sealed segments when the run was made. Segments are
    /// only ever added, so the numbers in `pieces` mean the same in the
    /// document for as long as it keeps this add buffer.
    sealed: Segments,
}

impl PieceRun {
    /// Length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len
    }

    /// Whether the run is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.len == 0
    }

    /// How many line breaks it holds, if known: `None` when it took text
    /// from a part of the original file that was not indexed yet.
    #[must_use]
    pub fn newlines(&self) -> Option<usize> {
        self.0.newlines
    }

    /// Whether it holds a line break. When the count is unknown this scans
    /// the uncounted pieces until it finds one, which for a long run with
    /// no line breaks reads all of it.
    #[must_use]
    pub fn has_newline(&self) -> bool {
        let run = &*self.0;
        if let Some(n) = run.newlines {
            return n > 0;
        }
        run.pieces.iter().any(|p| match p.newlines {
            Some(n) => n > 0,
            // Invariant 7: no piece names the open segment.
            None => memchr(b'\n', piece_bytes(&*run.source, &run.sealed, &[], p)).is_some(),
        })
    }

    /// How many pieces it is made of.
    #[must_use]
    pub fn piece_count(&self) -> usize {
        self.0.pieces.len()
    }

    /// Append the run's bytes to `out`.
    ///
    /// # Errors
    ///
    /// Returns the error from reading the original file. Reading from
    /// memory never fails; a file read with `pread` (M7 task 9) can.
    pub fn read_into(&self, out: &mut Vec<u8>) -> std::io::Result<()> {
        let run = &*self.0;
        out.reserve(run.len);
        for piece in &run.pieces {
            // Invariant 7: no piece names the open segment, so it is empty.
            append_piece(&*run.source, &run.sealed, &[], piece, (0, piece.len), out)?;
        }
        Ok(())
    }
}

/// Two runs are equal when they name the same pieces of the same stores:
/// the same original file (by lineage) and the very same segments.
impl PartialEq for PieceRun {
    fn eq(&self, other: &Self) -> bool {
        let (mine, theirs) = (&*self.0, &*other.0);
        let same_segment = |piece: &Piece| match piece.store {
            Store::Original => true,
            Store::Add(n) => match (mine.sealed.get(n), theirs.sealed.get(n)) {
                (Some(x), Some(y)) => Arc::ptr_eq(x, y),
                _ => false,
            },
        };
        let same_place = |a: &Piece, b: &Piece| {
            // Counts may differ only by one having been unknown.
            (a.store, a.start, a.len) == (b.store, b.start, b.len)
        };
        Arc::ptr_eq(&self.0, &other.0)
            || (mine.len == theirs.len
                && mine.lineage == theirs.lineage
                && mine.pieces.len() == theirs.pieces.len()
                && mine
                    .pieces
                    .iter()
                    .zip(&theirs.pieces)
                    .all(|(a, b)| same_place(a, b))
                && mine.pieces.iter().all(same_segment))
    }
}

impl Eq for PieceRun {}

/// Prints sizes only.
impl fmt::Debug for PieceRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PieceRun")
            .field("len", &self.0.len)
            .field("newlines", &self.0.newlines)
            .field("pieces", &self.0.pieces.len())
            .finish_non_exhaustive()
    }
}

/// What an edit inserts: new bytes, or pieces coming back from undo.
#[derive(Clone, Copy)]
enum Insert<'a> {
    Bytes(&'a [u8]),
    Run(&'a PieceRun),
}

/// Where the piece of `bytes` starting at `from` should end: at most `max`
/// bytes on, backed up to the start of a character so pieces of valid text
/// stay valid on their own (invariant 5). `max` is at least 4, so the piece
/// is never empty.
fn split_point(bytes: &[u8], from: usize, max: usize) -> usize {
    if bytes.len() - from <= max {
        return bytes.len();
    }
    let mut to = from + max;
    // A UTF-8 character has at most 3 continuation bytes. If there are
    // more, the bytes are not UTF-8 and any split will do.
    for _ in 0..3 {
        if to > from + 1 && bytes.get(to).is_some_and(|&b| is_continuation(b)) {
            to -= 1;
        }
    }
    to
}

/// `a` followed by `b` as one piece, if they are adjacent in the same store
/// and the result is not too long for an add piece. The count is unknown
/// if either part's is.
fn merged(a: &Piece, b: &Piece, max_piece: usize) -> Option<Piece> {
    let adjacent = a.store == b.store && a.start + a.len == b.start;
    let fits = a.store == Store::Original || a.len + b.len <= max_piece;
    (adjacent && fits).then_some(Piece {
        len: a.len + b.len,
        newlines: a.newlines.zip(b.newlines).map(|(x, y)| x + y),
        ..*a
    })
}

/// Join each run of mergeable neighbours in `pieces` into one piece. This is
/// what makes typing extend the piece it continues, and what turns the
/// three pieces left by deleting and undoing back into one.
fn merge_adjacent(pieces: &mut Vec<Piece>, max_piece: usize) {
    let mut kept: usize = 0;
    for i in 0..pieces.len() {
        let p = pieces[i];
        if let Some(prev) = kept.checked_sub(1)
            && let Some(m) = merged(&pieces[prev], &p, max_piece)
        {
            pieces[prev] = m;
        } else {
            pieces[kept] = p;
            kept += 1;
        }
    }
    pieces.truncate(kept);
}

/// Recompute the prefix sums of `entries` from index `from` on, taking
/// unknown counts as 0 (invariant 1).
fn fix_prefix(entries: &mut [Entry], from: usize) {
    let (mut at, mut lines) = from
        .checked_sub(1)
        .and_then(|i| entries.get(i))
        .map_or((0, 0), |e| {
            (e.at + e.piece.len, e.lines + e.piece.newlines.unwrap_or(0))
        });
    for e in entries.iter_mut().skip(from) {
        e.at = at;
        e.lines = lines;
        at += e.piece.len;
        lines += e.piece.newlines.unwrap_or(0);
    }
}

/// A piece table over a [`Source`], for files too big to load and files
/// that are not valid UTF-8 (M7). See the module documentation for how it
/// works and the invariants it keeps.
#[derive(Clone)]
pub struct MappedBuffer {
    /// The document now; snapshots are copies of it.
    state: MappedSnapshot,
    /// Reused between edits so that typing does not allocate: the pieces
    /// of the text being inserted, and the stretch of the piece list being
    /// rebuilt around an edit.
    inserted: Vec<Piece>,
    window: Vec<Piece>,
}

impl MappedBuffer {
    /// A buffer showing `source` unchanged, with the default [`Limits`].
    /// The bytes may be anything, UTF-8 or not.
    ///
    /// Indexes every line break of the source first, so this reads the
    /// whole source once; a huge file should use
    /// [`MappedBuffer::with_index`] with a partial index instead.
    #[must_use]
    pub fn new(source: Arc<dyn Source>) -> Self {
        Self::with_limits(source, Limits::DEFAULT)
    }

    /// A buffer showing `source` unchanged, with the given limits (raised
    /// to their minimums if below them). Indexes the whole source first,
    /// like [`MappedBuffer::new`].
    #[must_use]
    pub fn with_limits(source: Arc<dyn Source>, limits: Limits) -> Self {
        let index = LineIndex::build(source.bytes(), Spacing::DEFAULT);
        Self::build(source, index, limits)
    }

    /// A buffer showing `source` unchanged, whose line index is `index`,
    /// which may cover only a prefix of the source (or nothing): line
    /// numbers are known that far, and [`MappedBuffer::merge_index`] adds
    /// the rest as a background scan finds it. This is how a huge file
    /// opens without reading it all first.
    ///
    /// # Errors
    ///
    /// Returns [`MergeError::PastEnd`] if the index covers more bytes than
    /// the source has, which means it was made from another file.
    pub fn with_index(
        source: Arc<dyn Source>,
        index: LineIndex,
        limits: Limits,
    ) -> Result<Self, MergeError> {
        let len = to_u64(source.len());
        if index.indexed() > len {
            return Err(MergeError::PastEnd {
                upto: index.indexed(),
                len,
            });
        }
        Ok(Self::build(source, index, limits))
    }

    fn build(source: Arc<dyn Source>, index: LineIndex, limits: Limits) -> Self {
        Self {
            state: MappedSnapshot::new(source, index, limits.normalized()),
            inserted: Vec::new(),
            window: Vec::new(),
        }
    }

    /// A buffer whose original content is `bytes`, held in memory. The
    /// bytes may be anything, UTF-8 or not.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self::new(Arc::new(VecSource::new(bytes)))
    }

    /// The limits in use.
    #[must_use]
    pub fn limits(&self) -> Limits {
        self.state.limits
    }

    /// The original content the document was opened from.
    #[must_use]
    pub fn source(&self) -> &Arc<dyn Source> {
        &self.state.source
    }

    /// The original file's line index as merged so far.
    #[must_use]
    pub fn line_index(&self) -> &LineIndex {
        &self.state.index
    }

    /// Extend the original file's line index with the next chunk from the
    /// [`crate::line_index::Scanner`] reading it. Pieces of the original
    /// that the index now covers get their line counts, and line numbers
    /// become known further into the document ([`Text::indexed_bytes`]).
    /// Snapshots taken earlier keep the index they had.
    ///
    /// # Errors
    ///
    /// Returns the [`LineIndex::merge`] errors for a chunk that does not
    /// continue where the index ends, and [`MergeError::PastEnd`] for one
    /// reaching past the end of the source. Nothing changes on error.
    pub fn merge_index(&mut self, chunk: IndexChunk) -> Result<(), MergeError> {
        let len = to_u64(self.state.source.len());
        if chunk.upto() > len {
            return Err(MergeError::PastEnd {
                upto: chunk.upto(),
                len,
            });
        }
        // Copies the index's list of chunk pointers first if a snapshot
        // shares it. A refused chunk leaves the index unchanged.
        Arc::make_mut(&mut self.state.index).merge(chunk)?;
        self.state.fill_counts();
        Ok(())
    }

    /// The original file grew: read it from `source` from now on, and
    /// append its new bytes to the end of the document as one piece of the
    /// original, unindexed until [`MappedBuffer::merge_index`] reaches it.
    /// Returns the edit, for whatever follows edits (the syntax state); it
    /// is not meant for the undo history, since the document still matches
    /// the file.
    ///
    /// `source` must hold the old source's bytes first, then the new ones:
    /// a second mapping of the same file at its new length. Every piece of
    /// the original names bytes below the old length, so they read the
    /// same through either source, and the pieces, line counts and undo
    /// runs recorded so far all stay right. Only a few bytes at each end of
    /// the old source are compared, to refuse a file that was replaced
    /// rather than appended to; a file rewritten in the middle with both
    /// ends kept is taken as appended, like the watcher's other checks.
    /// Snapshots keep the source they were taken with.
    ///
    /// # Errors
    ///
    /// Returns [`ExtendError`] when `source` is shorter, starts or ends its
    /// old part differently, or cannot be read; nothing changes then.
    pub fn extend_source(&mut self, source: Arc<dyn Source>) -> Result<Edit, ExtendError> {
        let old = self.state.source.len();
        let new = source.len();
        if new < old {
            return Err(ExtendError::Shorter { old, new });
        }
        let check = EXTEND_CHECK.min(old);
        for at in [0, old - check] {
            let mut mine = [0; EXTEND_CHECK];
            let mut theirs = [0; EXTEND_CHECK];
            let (mine, theirs) = (&mut mine[..check], &mut theirs[..check]);
            self.state
                .source
                .read_at(at, mine)
                .map_err(ExtendError::Read)?;
            source.read_at(at, theirs).map_err(ExtendError::Read)?;
            if mine != theirs {
                return Err(ExtendError::Differs);
            }
        }
        self.state.source = source;
        let end = self.state.len();
        if new == old {
            return Ok(Edit {
                start: end,
                removed: Content::Bytes(Vec::new()),
                inserted: Content::Bytes(Vec::new()),
            });
        }
        // Copies the index's chunk list first if a snapshot shares it.
        Arc::make_mut(&mut self.state.index).reopen();
        // Invariant 6 holds: the new piece is last in the document and
        // starts past every byte any other piece of the original names.
        let run = PieceRun(Arc::new(Run {
            pieces: vec![Piece {
                store: Store::Original,
                start: old,
                len: new - old,
                newlines: None,
            }],
            len: new - old,
            newlines: None,
            source: Arc::clone(&self.state.source),
            lineage: self.state.lineage,
            sealed: Arc::clone(&self.state.sealed),
        }));
        Ok(self.apply(end..end, Insert::Run(&run)))
    }

    /// How many pieces the document is made of: 1 for an unedited file,
    /// growing by up to two per edit in a new place.
    #[must_use]
    pub fn piece_count(&self) -> usize {
        self.state.table.len()
    }

    /// The parts of the original file the document still uses, in document
    /// order. They are in increasing file order and never overlap
    /// (invariant 6), which lets saving stream them in one pass.
    pub fn original_ranges(&self) -> impl Iterator<Item = Range<usize>> + '_ {
        self.state
            .table
            .iter()
            .filter(|e| e.piece.store == Store::Original)
            .map(|e| e.piece.start..e.piece.start + e.piece.len)
    }

    /// Turn the open add-buffer segment into a sealed one. Nothing that
    /// refers to it changes: its number stays the same.
    fn seal(&mut self) {
        if self.state.open.is_empty() {
            return;
        }
        let full = std::mem::take(&mut self.state.open);
        Arc::make_mut(&mut self.state.sealed).push(Arc::new(full));
    }

    /// Store `bytes` in the add buffer and push the pieces naming them to
    /// `self.inserted`.
    fn append(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let limits = self.state.limits;
        let (segment, base) = if bytes.len() > limits.segment {
            // Too big for the open segment: a sealed segment of its own.
            // The open one is sealed first, so the new segment does not
            // take its number.
            self.seal();
            let sealed = Arc::make_mut(&mut self.state.sealed);
            sealed.push(Arc::new(bytes.to_vec()));
            (sealed.len() - 1, 0)
        } else {
            if self.state.open.len() + bytes.len() > limits.segment {
                self.seal();
            }
            let base = self.state.open.len();
            self.state.open.extend_from_slice(bytes);
            (self.state.sealed.len(), base)
        };
        let mut from = 0;
        while from < bytes.len() {
            let to = split_point(bytes, from, limits.max_piece);
            self.inserted.push(Piece {
                store: Store::Add(segment),
                start: base + from,
                len: to - from,
                newlines: Some(count_nl(bytes.get(from..to).unwrap_or_default())),
            });
            from = to;
        }
    }

    /// Record what `range` holds before it is replaced: a copy, or the
    /// pieces themselves when longer than the copy limit.
    fn removed(&mut self, range: Range<usize>) -> Content {
        if range.len() <= self.state.limits.copy_limit {
            let mut bytes = Vec::with_capacity(range.len());
            self.state.read_raw(range, &mut bytes);
            return Content::Bytes(bytes);
        }
        let mut pieces = Vec::new();
        self.state
            .copy_range(self.state.find(range.start), range.clone(), &mut pieces);
        // Invariant 7: a run keeps its bytes alive through `Arc`s, and the
        // open segment is not behind one, so seal it if the run names it.
        // Sealing keeps the segment's number, so the pieces stay right.
        let open = Store::Add(self.state.sealed.len());
        if pieces.iter().any(|p| p.store == open) {
            self.seal();
        }
        Content::Pieces(PieceRun(Arc::new(Run {
            newlines: pieces.iter().map(|p| p.newlines).sum(),
            pieces,
            len: range.len(),
            source: Arc::clone(&self.state.source),
            lineage: self.state.lineage,
            sealed: Arc::clone(&self.state.sealed),
        })))
    }

    /// Add the pieces of `insert` to the end of `self.inserted`.
    fn add(&mut self, insert: Insert<'_>) {
        match insert {
            Insert::Bytes(bytes) => self.append(bytes),
            Insert::Run(run) => {
                let from = self.inserted.len();
                self.inserted.extend_from_slice(&run.0.pieces);
                // Pieces that reached past the frontier when the run was
                // made may be indexed by now; count them (invariant 2).
                let state = &self.state;
                let f = state.original_indexed();
                for p in self.inserted.get_mut(from..).unwrap_or_default() {
                    if p.store == Store::Original && p.newlines.is_none() && p.start + p.len <= f {
                        p.newlines = state.count_original(p.start, p.start + p.len);
                    }
                }
            }
        }
    }

    /// Replace `range` (already checked) with `insert`.
    fn apply(&mut self, range: Range<usize>, insert: Insert<'_>) -> Edit {
        let removed = self.removed(range.clone());
        self.inserted.clear();
        self.add(insert);
        let inserted = match insert {
            Insert::Bytes(bytes) => Content::Bytes(bytes.to_vec()),
            Insert::Run(run) => Content::Pieces(run.clone()),
        };
        if !range.is_empty() || !self.inserted.is_empty() {
            self.splice(range.clone());
        }
        Edit {
            start: range.start,
            removed,
            inserted,
        }
    }

    /// Replace the pieces covering `range` with `self.inserted`.
    ///
    /// Only a short stretch of the list is rebuilt: the pieces the range
    /// touches plus one untouched neighbour each side, so a typed character
    /// can merge into the piece before it and pieces put back by undo can
    /// merge with what they were cut from. Then the prefix sums after it
    /// and the frontier are brought up to date.
    fn splice(&mut self, range: Range<usize>) {
        let table = &self.state.table;
        // Entries lo..hi overlap the range; for an insertion inside a
        // piece that is the one piece, to be split around it.
        let lo = table.partition_point(|e| e.at + e.piece.len <= range.start);
        let hi = table.partition_point(|e| e.at < range.end);
        let a = lo.saturating_sub(1);
        let b = (hi + 1).min(table.len());

        let window = &mut self.window;
        window.clear();
        window.extend(table[a..lo].iter().map(|e| e.piece));
        if lo < hi {
            let first = &table[lo];
            if first.at < range.start {
                window.push(self.state.cut(&first.piece, 0, range.start - first.at));
            }
        }
        window.extend_from_slice(&self.inserted);
        if lo < hi {
            let last = &table[hi - 1];
            if last.at + last.piece.len > range.end {
                window.push(
                    self.state
                        .cut(&last.piece, range.end - last.at, last.piece.len),
                );
            }
        }
        window.extend(table[hi..b].iter().map(|e| e.piece));
        merge_adjacent(window, self.state.limits.max_piece);

        // Copies the list first if a snapshot shares it.
        let entries = Arc::make_mut(&mut self.state.table);
        entries.splice(
            a..b,
            self.window.iter().map(|&piece| Entry {
                piece,
                at: 0,
                lines: 0,
            }),
        );
        fix_prefix(entries, a);
        self.state.refresh(a);
    }
}

impl Text for MappedBuffer {
    fn len_bytes(&self) -> usize {
        self.state.len_bytes()
    }

    fn len_lines(&self) -> usize {
        self.state.len_lines()
    }

    fn line(&self, idx: usize) -> Cow<'_, str> {
        self.state.line(idx)
    }

    fn byte_to_line(&self, byte: usize) -> usize {
        self.state.byte_to_line(byte)
    }

    fn line_to_byte(&self, line: usize) -> usize {
        self.state.line_to_byte(line)
    }

    fn slice(&self, range: Range<usize>) -> Cow<'_, str> {
        self.state.slice(range)
    }

    fn is_char_boundary(&self, byte: usize) -> bool {
        self.state.is_char_boundary(byte)
    }

    fn chunk_at(&self, byte: usize) -> (&[u8], usize) {
        self.state.chunk_at(byte)
    }

    fn byte_at(&self, byte: usize) -> Option<u8> {
        self.state.byte_at(byte)
    }

    fn line_span(&self, byte: usize) -> LineSpan {
        self.state.line_span(byte)
    }

    fn line_number(&self, byte: usize) -> Option<usize> {
        self.state.line_number(byte)
    }

    fn indexed_bytes(&self) -> usize {
        self.state.indexed_bytes()
    }

    fn read_into(&self, range: Range<usize>, out: &mut Vec<u8>) -> std::io::Result<()> {
        self.state.read_into(range, out)
    }

    fn slice_into(&self, range: Range<usize>, out: &mut String) {
        self.state.slice_into(range, out);
    }
}

impl Buffer for MappedBuffer {
    fn replace(&mut self, range: Range<usize>, text: &str) -> Result<Edit, EditError> {
        check_range(&self.state, &range)?;
        Ok(self.apply(range, Insert::Bytes(text.as_bytes())))
    }

    /// Puts back bytes (valid UTF-8 or not) or a [`PieceRun`] this document
    /// recorded earlier. A run from any other document is refused with
    /// [`EditError::Unsupported`].
    fn put(&mut self, range: Range<usize>, content: &Content) -> Result<Edit, EditError> {
        let len = self.state.len();
        if range.start > range.end || range.end > len {
            return Err(EditError::OutOfBounds { range, len });
        }
        let insert = match content {
            Content::Bytes(bytes) => Insert::Bytes(bytes),
            Content::Pieces(run) if self.state.owns(run) => Insert::Run(run),
            Content::Pieces(_) => return Err(EditError::Unsupported),
        };
        Ok(self.apply(range, insert))
    }

    fn snapshot(&self) -> Box<dyn Snapshot> {
        Box::new(self.state.clone())
    }

    /// True once every line break is counted: the original file is indexed
    /// as far as the document still uses it.
    fn is_fully_indexed(&self) -> bool {
        self.state.frontier.entry >= self.state.table.len()
    }

    /// Builds the new piece list once for all items: see
    /// [`MappedBuffer::rebuild`].
    fn replace_sorted(&mut self, items: &[(Range<usize>, &str)]) -> Result<Vec<Edit>, EditError> {
        check_sorted(&self.state, items.iter().map(|(r, _)| r), true)?;
        let inserts: Vec<(Range<usize>, Insert<'_>)> = items
            .iter()
            .map(|(r, t)| (r.clone(), Insert::Bytes(t.as_bytes())))
            .collect();
        let removed = self.rebuild(&inserts);
        // Applied from the last item to the first, so each edit's start is
        // its item's start.
        Ok(items
            .iter()
            .zip(removed)
            .rev()
            .map(|((range, text), removed)| Edit {
                start: range.start,
                removed,
                inserted: Content::from(*text),
            })
            .collect())
    }

    /// One pass, like [`MappedBuffer::replace_sorted`]: undoing a Replace
    /// All of 100,000 matches one put at a time took over a minute.
    fn put_sorted(&mut self, items: &[(Range<usize>, &Content)]) -> Result<Vec<Edit>, EditError> {
        check_sorted(&self.state, items.iter().map(|(r, _)| r), false)?;
        let mut inserts = Vec::with_capacity(items.len());
        for (range, content) in items {
            let insert = match content {
                Content::Bytes(bytes) => Insert::Bytes(bytes),
                Content::Pieces(run) if self.state.owns(run) => Insert::Run(run),
                Content::Pieces(_) => return Err(EditError::Unsupported),
            };
            inserts.push((range.clone(), insert));
        }
        let removed = self.rebuild(&inserts);
        Ok(items
            .iter()
            .zip(removed)
            .rev()
            .map(|((range, content), removed)| Edit {
                start: range.start,
                removed,
                inserted: (*content).clone(),
            })
            .collect())
    }
}

impl MappedBuffer {
    /// Apply sorted, checked items by building the new piece list in one
    /// pass over the old one, instead of one splice per item, so the cost
    /// is linear in the number of pieces rather than quadratic. Returns what
    /// each item removed, in item order.
    fn rebuild(&mut self, items: &[(Range<usize>, Insert<'_>)]) -> Vec<Content> {
        if items.is_empty() {
            return Vec::new();
        }
        // What each item removes, recorded before anything changes.
        let removed: Vec<Content> = items
            .iter()
            .map(|(range, _)| self.removed(range.clone()))
            .collect();
        // Store every insert; `ends[i]` is where item i's pieces end in
        // `self.inserted`.
        self.inserted.clear();
        let mut ends = Vec::with_capacity(items.len());
        for (_, insert) in items {
            self.add(*insert);
            ends.push(self.inserted.len());
        }

        let state = &self.state;
        let mut pieces =
            Vec::with_capacity(state.table.len() + self.inserted.len() + items.len() + 1);
        let (mut i, mut pos, mut from) = (0, 0, 0);
        for ((range, _), &end) in items.iter().zip(&ends) {
            i = state.copy_range(i, pos..range.start, &mut pieces);
            pieces.extend_from_slice(self.inserted.get(from..end).unwrap_or_default());
            from = end;
            pos = range.end;
        }
        state.copy_range(i, pos..state.len(), &mut pieces);
        merge_adjacent(&mut pieces, state.limits.max_piece);
        let mut entries: Vec<Entry> = pieces
            .into_iter()
            .map(|piece| Entry {
                piece,
                at: 0,
                lines: 0,
            })
            .collect();
        fix_prefix(&mut entries, 0);
        self.state.table = Arc::new(entries);
        self.state.refresh(0);
        removed
    }
}

/// Prints sizes only.
impl fmt::Debug for MappedBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MappedBuffer")
            .field("state", &self.state)
            .field("limits", &self.state.limits)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::line_index::Scanner;
    use crate::{History, RopeBuffer, Selections, movement};

    fn text(t: &dyn Text) -> String {
        t.slice(0..usize::MAX).into_owned()
    }

    /// Tiny limits, so a few bytes exercise every path.
    const TINY: Limits = Limits {
        copy_limit: 3,
        segment: 8,
        max_piece: 5,
        scan: 3,
        pseudo_line: 8,
    };

    fn tiny(initial: &str) -> MappedBuffer {
        MappedBuffer::with_limits(Arc::new(VecSource::from(initial)), TINY)
    }

    /// A buffer over `bytes` whose line index covers only the first
    /// `indexed` bytes, with a checkpoint every 2 lines or 5 bytes, and the
    /// scanner that made it, to feed the rest later.
    fn partly_indexed(bytes: &[u8], indexed: usize, limits: Limits) -> (MappedBuffer, Scanner) {
        let mut scanner = Scanner::new(Spacing::new(2, 5));
        scanner.feed(&bytes[..indexed]);
        let mut index = LineIndex::new();
        index.merge(scanner.take_chunk()).unwrap();
        let source = Arc::new(VecSource::new(bytes.to_vec()));
        let b = MappedBuffer::with_index(source, index, limits).unwrap();
        (b, scanner)
    }

    /// Invariants 1, 2, 4, 6 and 8, and that no two neighbours could be
    /// merged.
    fn check_invariants(b: &MappedBuffer) {
        let s = &b.state;
        let f = s.original_indexed();
        let (mut at, mut lines) = (0, 0);
        let mut first_unknown = None;
        for (i, e) in s.table.iter().enumerate() {
            assert!(e.piece.len > 0, "piece {i} is empty");
            assert_eq!((e.at, e.lines), (at, lines), "prefix sums of piece {i}");
            let bytes = s.bytes(&e.piece);
            assert_eq!(bytes.len(), e.piece.len, "piece {i} fits its store");
            let known = match e.piece.store {
                Store::Add(_) => {
                    assert!(
                        e.piece.len <= b.limits().max_piece,
                        "add piece {i} too long"
                    );
                    true
                }
                Store::Original => e.piece.start + e.piece.len <= f,
            };
            assert_eq!(
                e.piece.newlines,
                known.then(|| count_nl(bytes)),
                "newlines of {i}"
            );
            if !known && first_unknown.is_none() {
                first_unknown = Some(i);
            }
            if i > 0 {
                let prev = &s.table[i - 1].piece;
                assert!(
                    merged(prev, &e.piece, b.limits().max_piece).is_none(),
                    "pieces {} and {i} should have been merged",
                    i - 1
                );
            }
            at += e.piece.len;
            lines += e.piece.newlines.unwrap_or(0);
        }
        let mut prev_end = 0;
        for r in b.original_ranges() {
            assert!(r.start >= prev_end, "original pieces out of order: {r:?}");
            prev_end = r.end;
        }
        // The cached frontier is what computing it afresh gives.
        let mut fresh = s.clone();
        fresh.frontier = Frontier::default();
        fresh.refresh(0);
        let parts = |f: &Frontier| (f.entry, f.offset, f.lines);
        assert_eq!(parts(&s.frontier), parts(&fresh.frontier), "frontier");
        assert_eq!(s.frontier.entry, first_unknown.unwrap_or(s.table.len()));
    }

    #[test]
    fn lines_strip_lf_and_crlf_but_keep_lone_cr() {
        let b = MappedBuffer::from_bytes(b"one\r\ntwo\nthr\ree\n".to_vec());
        assert_eq!(b.len_lines(), 4);
        assert_eq!(b.line(0), "one");
        assert_eq!(b.line(1), "two");
        assert_eq!(b.line(2), "thr\ree");
        assert_eq!(b.line(3), "");
        assert_eq!(b.line(99), "");
    }

    #[test]
    fn empty_source_has_one_empty_line() {
        let mut b = MappedBuffer::from_bytes(Vec::new());
        assert_eq!((b.len_bytes(), b.len_lines(), b.piece_count()), (0, 1, 0));
        assert_eq!(b.line(0), "");
        assert_eq!(b.chunk_at(0), (&[][..], 0));
        assert!(b.is_fully_indexed());
        b.replace(0..0, "hi").unwrap();
        assert_eq!(text(&b), "hi");
    }

    #[test]
    fn lines_and_slices_in_one_piece_are_borrowed() {
        let mut b = MappedBuffer::from_bytes(b"alpha\nbeta\ngamma".to_vec());
        b.replace(6..6, "BETA ").unwrap();
        assert_eq!(b.piece_count(), 3);
        assert!(matches!(b.line(0), Cow::Borrowed("alpha")));
        assert!(matches!(b.line(2), Cow::Borrowed("gamma")));
        assert!(matches!(b.slice(6..11), Cow::Borrowed("BETA ")));
        // Across pieces, the text is copied.
        assert!(matches!(b.line(1), Cow::Owned(ref s) if s == "BETA beta"));
    }

    #[test]
    fn typing_at_the_end_of_the_last_insertion_extends_its_piece() {
        let mut b = MappedBuffer::from_bytes(b"hello world".to_vec());
        b.replace(5..5, ",").unwrap();
        assert_eq!(b.piece_count(), 3);
        for (i, ch) in " there".chars().enumerate() {
            let mut buf = [0; 4];
            b.replace(6 + i..6 + i, ch.encode_utf8(&mut buf)).unwrap();
        }
        assert_eq!(text(&b), "hello, there world");
        assert_eq!(b.piece_count(), 3, "typing extended the inserted piece");
        check_invariants(&b);
    }

    #[test]
    fn deleting_and_undoing_restores_one_piece() {
        let limits = Limits {
            copy_limit: 16,
            ..Limits::default()
        };
        let source = Arc::new(VecSource::new(b"0123456789".repeat(10)));
        let mut b = MappedBuffer::with_limits(source, limits);
        let e = b.replace(10..90, "").unwrap();
        assert!(matches!(e.removed, Content::Pieces(_)));
        assert_eq!(b.piece_count(), 2);
        b.put(10..10, &e.removed).unwrap();
        assert_eq!(b.piece_count(), 1, "the three original pieces merge back");
        assert_eq!(text(&b), "0123456789".repeat(10));
        check_invariants(&b);
    }

    #[test]
    fn large_insertions_are_split_into_pieces_of_at_most_a_mebibyte() {
        let mut b = MappedBuffer::from_bytes(b"[]".to_vec());
        // 3-byte characters, so a split at exactly 1 MiB would cut one.
        let big = "日".repeat(MAX_PIECE * 5 / 6);
        b.replace(1..1, &big).unwrap();
        assert_eq!(b.len_bytes(), big.len() + 2);
        assert_eq!(b.piece_count(), 5, "[, three pieces of 日, ]");
        let mut pos = 0;
        while pos < b.len_bytes() {
            let (chunk, start) = b.chunk_at(pos);
            assert!(chunk.len() <= MAX_PIECE);
            assert!(
                std::str::from_utf8(chunk).is_ok(),
                "pieces end on characters"
            );
            pos = start + chunk.len();
        }
        assert_eq!(text(&b), format!("[{big}]"));
        check_invariants(&b);
    }

    #[test]
    fn snapshots_are_unaffected_by_appends_past_a_sealed_segment() {
        let mut b = tiny("ab\ncd");
        b.replace(2..2, "1234").unwrap();
        let snap = b.snapshot();
        let then = text(&b);
        // Fill the open segment past its size several times over, sealing
        // it (and the bytes the snapshot's pieces name) along the way.
        for i in 0..40 {
            let at = 6 + i;
            b.replace(at..at, "x").unwrap();
        }
        assert!(b.state.sealed.len() >= 4, "segments were sealed");
        b.replace(0..1, "Z").unwrap();
        assert_eq!(text(&*snap), then);
        assert_eq!(snap.line(0), "ab1234");
        assert_eq!(snap.line(1), "cd");
        assert_eq!(text(&b), format!("Zb1234{}\ncd", "x".repeat(40)));
        // And a snapshot taken now reads from sealed and open segments.
        let snap2 = b.snapshot();
        b.replace(0..b.len_bytes(), "").unwrap();
        assert_eq!(text(&*snap2), format!("Zb1234{}\ncd", "x".repeat(40)));
        check_invariants(&b);
    }

    #[test]
    fn original_pieces_stay_in_file_order_through_edits_and_undo() {
        let initial = "line one\nline two\r\nline three\nlast";
        let mut b = tiny(initial);
        let mut hist = History::new();
        let sel = Selections::default();
        let steps: [(Range<usize>, &str); 6] = [
            (5..8, "1"),
            (0..0, "> "),
            (12..20, ""),
            (3..3, "é\n"),
            (20..25, "日本"),
            (1..30, "x"),
        ];
        for (range, t) in steps {
            let e = b.replace(range, t).unwrap();
            hist.record_group(vec![e], &sel, &sel);
            check_invariants(&b);
        }
        while hist.undo(&mut b, &mut |_, _| {}).unwrap().is_some() {
            check_invariants(&b);
        }
        assert_eq!(text(&b), initial);
        // Removals of up to 3 bytes were copied, so undo put copies back
        // in the add buffer; the longer ones went back as original pieces.
        let ranges: Vec<_> = b.original_ranges().collect();
        assert_eq!(ranges, vec![0..5, 8..initial.len()]);
        while hist.redo(&mut b, &mut |_, _| {}).unwrap().is_some() {
            check_invariants(&b);
        }
    }

    #[test]
    fn a_grown_source_extends_the_document_and_keeps_its_undo_runs() {
        let first: &[u8] = b"one\ntwo\nthree\nfour\n";
        let (mut b, _) = partly_indexed(first, first.len(), TINY);
        // A removal recorded as pieces before the growth.
        let e = b.replace(2..15, "").unwrap();
        assert!(matches!(e.removed, Content::Pieces(_)));
        let before = b.snapshot();
        let mut grown = first.to_vec();
        grown.extend_from_slice(b"five\nsix");
        let edit = b
            .extend_source(Arc::new(VecSource::new(grown.clone())))
            .unwrap();
        assert_eq!(edit.new_range(), 6..14);
        assert!(edit.removed.is_empty());
        check_invariants(&b);
        assert_eq!(text(&b), "onour\nfive\nsix");
        assert_eq!(before.len_bytes(), 6, "a snapshot keeps its text");
        assert!(!b.is_fully_indexed(), "the new bytes are not counted yet");

        // Undo still puts the old pieces back, and they join up again.
        b.put(2..2, &e.removed).unwrap();
        assert_eq!(text(&b), "one\ntwo\nthree\nfour\nfive\nsix");
        assert_eq!(b.piece_count(), 1);
        check_invariants(&b);

        // Counting lines carries on where it stopped.
        let mut more = Scanner::resume(b.line_index(), Spacing::new(2, 5));
        more.feed(&grown[first.len()..]);
        more.finish();
        b.merge_index(more.take_chunk()).unwrap();
        assert!(b.is_fully_indexed());
        assert_eq!(b.line_number(b.len_bytes()), Some(5));
        assert_eq!(
            b.line_index().checkpoints().collect::<Vec<_>>(),
            LineIndex::build(&grown, Spacing::new(2, 5))
                .checkpoints()
                .collect::<Vec<_>>(),
            "the same checkpoints as indexing it whole"
        );
        check_invariants(&b);

        // Refused: a shorter source, or one whose start differs.
        assert!(matches!(
            b.extend_source(Arc::new(VecSource::new(first.to_vec()))),
            Err(ExtendError::Shorter { .. })
        ));
        let mut other = grown.clone();
        other[0] = b'O';
        other.push(b'\n');
        assert!(matches!(
            b.extend_source(Arc::new(VecSource::new(other))),
            Err(ExtendError::Differs)
        ));
        assert_eq!(b.len_bytes(), grown.len(), "nothing changed");
    }

    #[test]
    fn long_removals_are_recorded_as_pieces_and_undo_without_copying() {
        let initial = "abc\ndef\nghi\njkl\n";
        let mut b = tiny(initial);
        b.replace(4..4, "XY\n").unwrap();
        let e = b.replace(2..12, "").unwrap();
        let Content::Pieces(run) = &e.removed else {
            panic!("expected pieces, got {:?}", e.removed);
        };
        assert_eq!(
            (run.len(), run.newlines(), run.piece_count()),
            (10, Some(3), 3)
        );
        assert!(e.removed.has_newline());
        assert_eq!(e.removed.as_bytes(), None);
        let mut bytes = Vec::new();
        e.removed.read_into(&mut bytes).unwrap();
        assert_eq!(bytes, b"c\nXY\ndef\ng");
        // Short removals are still copied.
        let short = b.replace(0..2, "").unwrap();
        assert_eq!(short.removed, "ab");

        b.put(0..0, &short.removed).unwrap();
        let back = b.put(2..2, &e.removed).unwrap();
        assert_eq!(back.inserted, e.removed);
        assert_eq!(text(&b), "abc\nXY\ndef\nghi\njkl\n");
        check_invariants(&b);
    }

    #[test]
    fn pieces_from_another_document_are_refused() {
        let mut a = tiny("0123456789");
        let removed = a.replace(0..8, "").unwrap().removed;
        assert!(matches!(removed, Content::Pieces(_)));

        // Same text, different storage.
        let mut other = tiny("0123456789");
        assert_eq!(other.put(0..0, &removed), Err(EditError::Unsupported));
        let mut rope = RopeBuffer::from_text("x");
        assert_eq!(rope.put(0..0, &removed), Err(EditError::Unsupported));
        // Bounds are checked first.
        assert!(matches!(
            a.put(5..9, &removed),
            Err(EditError::OutOfBounds { .. })
        ));
        assert_eq!(text(&other), "0123456789");
        assert_eq!(text(&rope), "x");

        // Pieces naming the add buffer: a clone of the document has its own
        // segments (equal now, but they will diverge), so it is refused too.
        let mut c = tiny("");
        c.replace(0..0, "hello world").unwrap();
        let run = c.replace(0..11, "").unwrap().removed;
        let mut copy = MappedBuffer::with_limits(Arc::clone(c.source()), TINY);
        assert_eq!(copy.put(0..0, &run), Err(EditError::Unsupported));
        c.put(0..0, &run).unwrap();
        assert_eq!(text(&c), "hello world");
    }

    #[test]
    fn invalid_utf8_reads_back_raw_and_shows_as_substitutes() {
        let b = MappedBuffer::from_bytes(b"ok\xff\xfe\n\x80end".to_vec());
        assert_eq!(b.line(0), "ok\u{1a}\u{1a}");
        assert_eq!(b.line(1), "\u{1a}end");
        let mut raw = Vec::new();
        b.read_into(0..usize::MAX, &mut raw).unwrap();
        assert_eq!(raw, b"ok\xff\xfe\n\x80end");
        let mut s = String::from(">");
        b.slice_into(0..b.len_bytes(), &mut s);
        assert_eq!(s, ">ok\u{1a}\u{1a}\n\u{1a}end");
    }

    /// The unit boundaries of `bytes`, worked out from `utf8_chunks`: every
    /// character boundary of each valid run, and both sides of every
    /// invalid byte.
    fn unit_boundaries(bytes: &[u8]) -> Vec<bool> {
        let mut out = vec![false; bytes.len() + 1];
        let mut at = 0;
        for chunk in bytes.utf8_chunks() {
            for (i, _) in chunk.valid().char_indices() {
                out[at + i] = true;
            }
            at += chunk.valid().len();
            for _ in chunk.invalid() {
                out[at] = true;
                at += 1;
            }
        }
        out[bytes.len()] = true;
        out
    }

    /// `bytes` as the buffer shows them, worked out byte by byte: each
    /// invalid byte becomes SUB.
    fn substituted(bytes: &[u8]) -> String {
        let bounds = unit_boundaries(bytes);
        let mut out = String::new();
        let mut at = 0;
        while at < bytes.len() {
            let mut end = at + 1;
            while !bounds[end] {
                end += 1;
            }
            match std::str::from_utf8(&bytes[at..end]) {
                Ok(s) => out.push_str(s),
                Err(_) => out.push('\u{1a}'),
            }
            at = end;
        }
        out
    }

    #[test]
    fn boundaries_follow_utf8_chunks_within_and_across_pieces() {
        const PARTS: &[&[u8]] = &[
            b"a",
            b"\n",
            "é".as_bytes(),
            "€".as_bytes(),
            "😀".as_bytes(),
            b"\xff",
            b"\xe2\x82",
            b"\xe2",
            b"\x82\xac",
            b"\xf0\x9f",
            b"\x98\x80",
            b"\xed\xa0\x80",
            b"\xc0\xaf",
            b"\xf4\x90\x80\x80",
            b"\x80",
        ];
        for a in PARTS {
            for b in PARTS {
                for c in PARTS {
                    let bytes = [*a, *b, *c].concat();
                    let want = unit_boundaries(&bytes);
                    let whole =
                        MappedBuffer::with_limits(Arc::new(VecSource::new(bytes.clone())), TINY);
                    // The same bytes as three add-buffer pieces that cannot
                    // merge (put in reverse, so they are out of store order).
                    let mut split = MappedBuffer::with_limits(Arc::new(VecSource::default()), TINY);
                    for part in [c, b, a] {
                        split.put(0..0, &Content::Bytes(part.to_vec())).unwrap();
                    }
                    for t in [&whole, &split] {
                        for (i, &w) in want.iter().enumerate() {
                            assert_eq!(t.is_char_boundary(i), w, "{bytes:x?} at {i}");
                        }
                        assert!(!t.is_char_boundary(bytes.len() + 1));
                        assert_eq!(text(t), substituted(&bytes), "{bytes:x?}");
                    }
                }
            }
        }
    }

    #[test]
    fn deleting_can_join_invalid_bytes_and_undo_splits_them_again() {
        let mut b = MappedBuffer::from_bytes(b"\xe2A\x82\xac\n".to_vec());
        assert_eq!(b.line(0), "\u{1a}A\u{1a}\u{1a}");
        assert!((0..=5).all(|i| b.is_char_boundary(i)));
        let mut hist = History::new();
        let sel = Selections::default();
        let e = b.replace(1..2, "").unwrap();
        hist.record_group(vec![e], &sel, &sel);
        assert_eq!(b.line(0), "€", "three invalid bytes became a character");
        let bounds: Vec<bool> = (0..=4).map(|i| b.is_char_boundary(i)).collect();
        assert_eq!(bounds, [true, false, false, true, true]);
        assert_eq!(
            b.replace(1..1, "x"),
            Err(EditError::NotCharBoundary { offset: 1 })
        );
        assert_eq!(movement::next_grapheme(&b, 0), 3);
        assert_eq!(movement::prev_grapheme(&b, 3), 0);

        // Undo puts the A back at offset 1, inside the character.
        hist.undo(&mut b, &mut |_, _| {}).unwrap();
        let mut raw = Vec::new();
        b.read_into(0..usize::MAX, &mut raw).unwrap();
        assert_eq!(raw, b"\xe2A\x82\xac\n");
        assert_eq!(b.line(0), "\u{1a}A\u{1a}\u{1a}");
        assert_eq!(movement::next_grapheme(&b, 0), 1);
        hist.redo(&mut b, &mut |_, _| {}).unwrap();
        assert_eq!(b.line(0), "€");
        check_invariants(&b);
    }

    #[test]
    fn slice_into_matches_slice_across_pieces() {
        let mut b = tiny("héllo\nwörld");
        b.replace(3..3, "日本\n").unwrap();
        b.replace(0..1, "ÿ").unwrap();
        let len = b.len_bytes();
        for s in 0..=len + 1 {
            for e in s..=len + 1 {
                let mut out = String::from("~");
                b.slice_into(s..e, &mut out);
                assert_eq!(out, format!("~{}", b.slice(s..e)), "slice_into({s}..{e})");
            }
        }
    }

    #[test]
    fn line_numbers_are_known_up_to_the_frontier() {
        let src = b"ab\ncd\nef\ngh\n";
        let (mut b, mut scanner) = partly_indexed(src, 4, TINY);
        assert_eq!(b.indexed_bytes(), 4);
        assert!(!b.is_fully_indexed());
        assert_eq!(b.len_lines(), 2, "lines starting by byte 4");
        assert_eq!(b.line_number(4), Some(1));
        assert_eq!(b.line_number(5), None);
        assert_eq!(b.byte_to_line(10), 1, "clamped to the last line counted");
        assert_eq!(b.line_to_byte(1), 3);
        assert_eq!(b.line_to_byte(2), src.len(), "not counted yet");
        assert_eq!(b.line(1), "cd");
        assert_eq!(b.line(2), "");
        // Lines are found past the frontier all the same.
        assert_eq!(
            b.line_span(7),
            LineSpan {
                start: 6,
                end: 8,
                next: Some(9)
            }
        );
        check_invariants(&b);

        // An edit behind the frontier moves it; one ahead of it does not.
        b.replace(0..0, "x\n").unwrap();
        assert_eq!((b.indexed_bytes(), b.len_lines()), (6, 3));
        check_invariants(&b);
        b.replace(10..10, "\n\n").unwrap();
        assert_eq!((b.indexed_bytes(), b.len_lines()), (6, 3));
        assert_eq!(b.line_number(7), None);
        check_invariants(&b);

        // Indexing the rest makes everything exact.
        scanner.feed(&src[4..]);
        scanner.finish();
        b.merge_index(scanner.take_chunk()).unwrap();
        assert!(b.is_fully_indexed());
        assert_eq!(text(&b), "x\nab\ncd\nef\n\n\ngh\n");
        assert_eq!((b.indexed_bytes(), b.len_lines()), (b.len_bytes(), 8));
        assert_eq!(b.line_number(7), Some(2));
        assert_eq!(b.line_to_byte(6), 13);
        check_invariants(&b);
    }

    #[test]
    fn long_lines_past_the_frontier_are_cut_into_pseudo_lines() {
        let limits = Limits {
            scan: 4,
            pseudo_line: 10,
            ..TINY
        };
        let starts = |b: &MappedBuffer| {
            let mut out = vec![0];
            let mut at = 0;
            while let Some(n) = b.line_span(at).next {
                out.push(n);
                at = n;
            }
            out
        };
        let (b, _) = partly_indexed(&[b'x'; 35], 0, limits);
        assert_eq!(starts(&b), [0, 10, 20, 30]);
        for i in 0..=36 {
            let s = b.line_span(i);
            let start = i.min(35) / 10 * 10;
            assert_eq!(s.start, start, "start at {i}");
            assert_eq!(s.end, s.next.unwrap_or(35), "no line ending at {i}");
        }
        assert_eq!(
            b.line(0),
            "x".repeat(10),
            "the last line counted is cut too"
        );

        // A line break within `scan` bytes before a point, or at it,
        // cancels that point; one further back does not.
        let mut src = vec![b'x'; 35];
        src[7] = b'\n';
        src[20] = b'\n';
        let (mut b, mut scanner) = partly_indexed(&src, 0, limits);
        assert_eq!(starts(&b), [0, 8, 21, 30]);
        assert_eq!(
            b.line_span(20),
            LineSpan {
                start: 8,
                end: 20,
                next: Some(21)
            }
        );
        // Once indexed, they join back into lines.
        scanner.feed(&src);
        b.merge_index(scanner.take_chunk()).unwrap();
        assert_eq!(starts(&b), [0, 8, 21]);
        check_invariants(&b);
    }

    #[test]
    fn the_cursor_crosses_pseudo_line_breaks_one_character_at_a_time() {
        let limits = Limits {
            scan: 4,
            pseudo_line: 10,
            ..TINY
        };
        // 日 is 3 bytes, so the points at 10 and 20 move back to 9 and 18.
        let (b, _) = partly_indexed("日".repeat(10).as_bytes(), 0, limits);
        assert_eq!(b.line_span(10).start, 9);
        assert_eq!(b.line_span(17).next, Some(18));
        let mut right = vec![0];
        let mut at = 0;
        while at < b.len_bytes() {
            at = movement::next_grapheme(&b, at);
            right.push(at);
        }
        assert_eq!(right, (0..=30).step_by(3).collect::<Vec<_>>());
        let mut left = vec![b.len_bytes()];
        while at > 0 {
            at = movement::prev_grapheme(&b, at);
            left.push(at);
        }
        left.reverse();
        assert_eq!(left, right);
    }

    #[test]
    fn a_file_without_line_breaks_is_never_scanned_whole() {
        const MIB: usize = PSEUDO_LINE;
        const LEN: usize = 3 * MIB;
        // A lookup scans at most a pseudo-line and the scan distance each
        // way; the whole file is 3 MiB.
        const BOUND: usize = 2 * (PSEUDO_LINE + SCAN) + 64;
        let src = vec![b'a'; LEN];
        for indexed in [0, MIB] {
            let mut feeder = Scanner::default();
            feeder.feed(&src[..indexed]);
            let mut index = LineIndex::new();
            index.merge(feeder.take_chunk()).unwrap();
            let source = Arc::new(VecSource::new(src.clone()));
            let b = MappedBuffer::with_index(source, index, Limits::DEFAULT).unwrap();
            meter::take();
            let mut starts = Vec::new();
            let mut at = 0;
            loop {
                let span = b.line_span(at);
                let scanned = meter::take();
                assert!(scanned <= BOUND, "line_span({at}) scanned {scanned} bytes");
                starts.push(span.start);
                match span.next {
                    Some(n) => at = n,
                    None => break,
                }
            }
            // Pseudo-lines start at each MiB past the indexed part.
            let want = if indexed == 0 {
                vec![0, MIB, 2 * MIB]
            } else {
                vec![0, 2 * MIB]
            };
            assert_eq!(starts, want, "indexed {indexed}");
            for at in [1, MIB - 1, MIB, MIB + 1, 2 * MIB + 5, LEN - 1, LEN] {
                let span = b.line_span(at);
                let scanned = meter::take();
                assert!(scanned <= BOUND, "line_span({at}) scanned {scanned} bytes");
                assert!(span.start <= at && span.next.unwrap_or(LEN + 1) > at.min(LEN - 1));
                assert_eq!(movement::prev_grapheme(&b, at.max(1)), at.max(1) - 1);
                assert!(meter::take() <= 2 * BOUND, "moving left from {at}");
            }
            // Line numbers past K cost nothing.
            assert_eq!((b.byte_to_line(LEN), b.len_lines()), (0, 1));
            assert_eq!(b.line_number(LEN), None);
            assert_eq!(meter::take(), 0);
        }
    }

    #[test]
    fn merging_checks_that_the_chunk_fits() {
        let src = b"one\ntwo\nthree\n";
        let (mut b, mut scanner) = partly_indexed(src, 5, TINY);
        let mut other = Scanner::new(Spacing::new(2, 5));
        other.feed(b"one\ntwo");
        assert_eq!(
            b.merge_index(other.take_chunk()),
            Err(MergeError::Gap {
                indexed: 5,
                chunk_from: 0
            })
        );
        scanner.feed(&src[5..]);
        scanner.feed(b"more");
        assert_eq!(
            b.merge_index(scanner.take_chunk()),
            Err(MergeError::PastEnd { upto: 18, len: 14 })
        );
        assert_eq!(b.indexed_bytes(), 5);
        let long = LineIndex::build(b"longer than the source", Spacing::DEFAULT);
        let short: Arc<dyn Source> = Arc::new(VecSource::from("short"));
        assert!(matches!(
            MappedBuffer::with_index(short, long, TINY),
            Err(MergeError::PastEnd { upto: 22, len: 5 })
        ));
        assert!(
            MergeError::PastEnd { upto: 22, len: 5 }
                .to_string()
                .contains("only 5")
        );
    }

    #[test]
    fn snapshots_keep_the_index_they_were_taken_with() {
        let src = b"a\nb\nc\nd\n";
        let (mut b, mut scanner) = partly_indexed(src, 2, TINY);
        let snap = b.snapshot();
        scanner.feed(&src[2..]);
        scanner.finish();
        b.merge_index(scanner.take_chunk()).unwrap();
        assert_eq!((snap.indexed_bytes(), snap.len_lines()), (2, 2));
        assert_eq!((b.indexed_bytes(), b.len_lines()), (8, 5));
        assert_eq!(snap.line_number(3), None);
        assert_eq!(b.line_number(3), Some(1));
        check_invariants(&b);
    }

    #[test]
    fn pieces_removed_before_indexing_are_counted_when_put_back_after() {
        let src = b"aa\nbb\ncc\ndd\nee\n";
        let (mut b, mut scanner) = partly_indexed(src, 3, TINY);
        let e = b.replace(4..13, "").unwrap();
        let Content::Pieces(run) = &e.removed else {
            panic!("expected pieces, got {:?}", e.removed);
        };
        assert_eq!(run.newlines(), None, "removed from the unindexed part");
        assert!(e.removed.has_newline());
        check_invariants(&b);
        scanner.feed(&src[3..]);
        scanner.finish();
        b.merge_index(scanner.take_chunk()).unwrap();
        assert!(b.is_fully_indexed());
        b.put(4..4, &e.removed).unwrap();
        assert!(b.is_fully_indexed(), "the pieces put back were counted");
        assert_eq!(b.len_lines(), 6);
        assert_eq!(b.piece_count(), 1);
        check_invariants(&b);
    }

    #[test]
    fn grouped_undo_and_redo_in_one_pass_match_a_rope() {
        // Random sorted replacements, recorded as one group and undone and
        // redone with `put_sorted`, against a rope doing the same one edit
        // at a time. A small generator keeps the cases reproducible.
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            usize::try_from(seed % (n as u64)).unwrap()
        };
        let words = ["", "x", "yy\n", "zzzzzzz", "\r\n", "é"];
        for case in 0..300 {
            let initial: String = (0..next(40)).map(|_| words[next(words.len())]).collect();
            let limits = if case % 2 == 0 {
                TINY
            } else {
                Limits::default()
            };
            let mut mapped =
                MappedBuffer::with_limits(Arc::new(VecSource::from(&*initial)), limits);
            let mut rope = RopeBuffer::from_text(&initial);
            // Sorted, non-overlapping ranges on character boundaries.
            let mut cuts: Vec<usize> = (0..next(8))
                .map(|_| next(initial.len() + 1))
                .filter(|&c| initial.is_char_boundary(c))
                .collect();
            cuts.sort_unstable();
            let items: Vec<(Range<usize>, &str)> = cuts
                .chunks(2)
                .map(|c| (c[0]..*c.last().unwrap(), words[next(words.len())]))
                .collect();
            let mut hm = History::new();
            let mut hr = History::new();
            let sel = Selections::default();
            hm.record_group(mapped.replace_sorted(&items).unwrap(), &sel, &sel);
            hr.record_group(rope.replace_sorted(&items).unwrap(), &sel, &sel);
            for step in 0..4 {
                if step % 2 == 0 {
                    hm.undo(&mut mapped, &mut |_, _| {}).unwrap();
                    hr.undo(&mut rope, &mut |_, _| {}).unwrap();
                } else {
                    hm.redo(&mut mapped, &mut |_, _| {}).unwrap();
                    hr.redo(&mut rope, &mut |_, _| {}).unwrap();
                }
                assert_eq!(
                    text(&mapped),
                    text(&rope),
                    "case {case} step {step}: {items:?}"
                );
                check_invariants(&mapped);
            }
            assert_eq!(text(&mapped), text(&rope.clone()));
        }
    }

    #[test]
    fn replace_sorted_equals_applying_one_by_one() {
        let initial = "fn a() {}\nfn b() {}\r\nfn c() {}\n";
        let items: [(Range<usize>, &str); 4] = [
            (0..2, "pub fn"),
            (10..10, "// x\n"),
            (13..14, "bee"),
            (21..31, ""),
        ];
        for limits in [TINY, Limits::default()] {
            let src: Arc<dyn Source> = Arc::new(VecSource::from(initial));
            let mut one = MappedBuffer::with_limits(Arc::clone(&src), limits);
            let mut each = MappedBuffer::with_limits(src, limits);
            let edits = one.replace_sorted(&items).unwrap();
            let mut expected = Vec::new();
            for (range, t) in items.iter().rev() {
                expected.push(each.replace(range.clone(), t).unwrap());
            }
            assert_eq!(text(&one), text(&each));
            assert_eq!(edits.len(), expected.len());
            for (got, want) in edits.iter().zip(&expected) {
                assert_eq!((got.start, &got.inserted), (want.start, &want.inserted));
                let (mut g, mut w) = (Vec::new(), Vec::new());
                got.removed.read_into(&mut g).unwrap();
                want.removed.read_into(&mut w).unwrap();
                assert_eq!(g, w);
            }
            check_invariants(&one);
            // The edits undo like any others.
            let mut hist = History::new();
            let sel = Selections::default();
            hist.record_group(edits, &sel, &sel);
            hist.undo(&mut one, &mut |_, _| {}).unwrap();
            assert_eq!(text(&one), initial);
            check_invariants(&one);
        }
    }

    #[test]
    fn replace_sorted_refuses_bad_items_and_changes_nothing() {
        let mut b = tiny("abcdef");
        assert_eq!(
            b.replace_sorted(&[(2..4, "x"), (3..5, "y")]),
            Err(EditError::Unsorted { index: 1 })
        );
        assert_eq!(
            b.replace_sorted(&[(4..5, "x"), (0..1, "y")]),
            Err(EditError::Unsorted { index: 1 })
        );
        assert!(matches!(
            b.replace_sorted(&[(0..1, "x"), (5..9, "y")]),
            Err(EditError::OutOfBounds { .. })
        ));
        assert_eq!(b.replace_sorted(&[]), Ok(Vec::new()));
        assert_eq!(text(&b), "abcdef");
        // Touching neighbours are fine: an insertion at the end of a range.
        b.replace_sorted(&[(0..2, "X"), (2..2, "Y"), (2..2, "Z")])
            .unwrap();
        assert_eq!(text(&b), "XYZcdef");
    }

    #[test]
    fn snapshots_can_go_to_other_threads() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<MappedSnapshot>();
        send_sync::<MappedBuffer>();
        send_sync::<PieceRun>();

        let mut b = tiny("shared\ntext");
        let snap = b.snapshot();
        let worker = std::thread::spawn(move || snap.line(1).into_owned());
        b.replace(0..6, "changed").unwrap();
        assert_eq!(worker.join().unwrap(), "text");
    }

    /// Lookups and typing on a 64 MB document cut into about 4,000 pieces.
    /// Ignored by default; run with `cargo test --release -p nimbus-core --
    /// --ignored --nocapture piece_table_benchmark`.
    #[test]
    #[ignore = "benchmark; run explicitly in release mode"]
    fn piece_table_benchmark() {
        use std::time::Instant;
        let line = "2026-10-16 12:00:00 INFO request served in 12 ms, status 200, path /api/v1/x\n";
        let text = line.repeat(64 * 1024 * 1024 / line.len());
        let start = Instant::now();
        let mut b = MappedBuffer::from_bytes(text.into_bytes());
        println!("open + full index of 64 MB: {:?}", start.elapsed());
        // A simple deterministic generator, so runs are comparable.
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            usize::try_from(seed % n as u64).unwrap()
        };
        let start = Instant::now();
        for _ in 0..2000 {
            let at = next(b.len_bytes());
            b.replace(at..at, "edit\n").unwrap();
        }
        println!(
            "2,000 edits at random places: {:?} ({} pieces)",
            start.elapsed(),
            b.piece_count()
        );
        let n = 100_000;
        let (len, lines) = (b.len_bytes(), b.len_lines());
        let start = Instant::now();
        let mut sum = 0;
        for _ in 0..n {
            sum += b.line_span(next(len)).start;
        }
        println!("line_span: {:?} each", start.elapsed() / n);
        let start = Instant::now();
        for _ in 0..n {
            sum += b.line_to_byte(next(lines)) + b.byte_to_line(next(len));
        }
        println!(
            "line_to_byte + byte_to_line: {:?} each",
            start.elapsed() / n
        );
        let start = Instant::now();
        for _ in 0..n {
            sum += b.line(next(lines)).len();
        }
        println!("line: {:?} each", start.elapsed() / n);
        let at = len / 2;
        let start = Instant::now();
        for i in 0..10_000 {
            b.replace(at + i..at + i, "x").unwrap();
        }
        println!(
            "typing in the middle: {:?} per key ({} pieces)",
            start.elapsed() / 10_000,
            b.piece_count()
        );
        // Worst case: a new snapshot before every key, so every edit copies
        // the piece list and the snapshot copies the open segment.
        let start = Instant::now();
        let mut snap = b.snapshot();
        for i in 0..1_000 {
            snap = b.snapshot();
            b.replace(at + i..at + i, "y").unwrap();
        }
        println!("snapshot + key: {:?} per key", start.elapsed() / 1_000);
        assert!(sum > 0 && snap.len_bytes() > 0);
    }

    #[test]
    fn pieces_do_not_make_undo_records_bigger() {
        // A run is one pointer, so `Content` stays the size of the `Vec`
        // it held before runs existed, and so does every `Edit` in undo.
        assert_eq!(
            std::mem::size_of::<Content>(),
            std::mem::size_of::<Vec<u8>>()
        );
    }

    #[test]
    fn debug_output_is_small() {
        let b = MappedBuffer::from_bytes(vec![b'x'; 10_000]);
        let s = format!("{b:?}");
        assert!(s.len() < 400, "{s}");
        assert!(s.contains("len: 10000"));
    }
}
