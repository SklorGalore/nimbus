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
//! a piece of the original file, line breaks are found with the file's line
//! index; inside an add-buffer piece, by scanning it, which is cheap because
//! add pieces are at most [`MAX_PIECE`] long.
//!
//! The list is a plain `Vec`: human editing makes at most thousands of
//! pieces, so updating the prefix sums after an edit is a few microseconds.
//! A tree would make that logarithmic, if profiling ever asks for it.
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
//! 1. No piece is empty, and each entry's prefix sums equal the totals of
//!    the pieces before it. (Binary searches return the right piece.)
//! 2. A piece's `newlines` is the number of `\n` in its bytes. (Line lookups
//!    trust it and skip pieces without line breaks.)
//! 3. The original file's bytes and every sealed segment never change, and
//!    the open segment only grows. (A piece's bytes never change.)
//! 4. Add-buffer pieces are at most [`MAX_PIECE`] bytes. (Scanning one for
//!    line breaks is bounded.)
//! 5. In a valid UTF-8 document every piece starts and ends on a character
//!    boundary, so a range inside one piece is valid UTF-8 by itself and
//!    can be lent out as a `&str` without copying.
//! 6. Pieces of the original file appear in increasing file order and do
//!    not overlap. Edits only cut pieces and add new add-buffer pieces, and
//!    undo puts pieces back exactly where they came from, so this holds;
//!    saving (M7 task 12) relies on it to stream the file in order.
//! 7. A [`PieceRun`] only refers to sealed segments (the open one is sealed
//!    first), so the run's own `Arc`s keep every byte it names alive.
//!
//! This version assumes the original file is valid UTF-8 and indexes all its
//! line breaks when the buffer is made. Invalid bytes and the background,
//! sparse line index come next (M7 task 8): only `OriginalLines` and
//! character boundaries need to change for them.

use std::borrow::Cow;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use memchr::{memchr, memchr_iter, memrchr, memrchr_iter};

use crate::buffer::{
    Buffer, Content, Edit, EditError, LineSpan, SUBSTITUTE, Snapshot, Text, check_range,
    check_sorted,
};
use crate::source::{Source, VecSource};

/// Removals longer than this many bytes are kept in undo as pieces instead
/// of copied: 256 KiB.
pub const COPY_LIMIT: usize = 256 * 1024;

/// The open add-buffer segment is sealed before it grows past this many
/// bytes: 64 KiB, the most a snapshot copies.
pub const SEGMENT: usize = 64 * 1024;

/// Inserted text is split into pieces of at most this many bytes: 1 MiB.
pub const MAX_PIECE: usize = 1024 * 1024;

/// Sizes that trade memory against copying. The defaults are the constants
/// above; tests use tiny ones so that every path runs on small texts.
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
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            copy_limit: COPY_LIMIT,
            segment: SEGMENT,
            max_piece: MAX_PIECE,
        }
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
    /// How many `\n` bytes it holds.
    newlines: usize,
}

/// A piece in the document's list, with its prefix sums.
#[derive(Debug, Clone, Copy)]
struct Entry {
    piece: Piece,
    /// Where the piece starts in the document.
    at: usize,
    /// How many `\n` come before it in the document.
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

/// Count the `\n` bytes in `bytes`.
#[expect(
    clippy::naive_bytecount,
    reason = "the compiler vectorises this loop; not worth a crate"
)]
fn count_newlines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| b == b'\n').count()
}

/// Whether `b` continues a multi-byte UTF-8 character (`10xxxxxx`).
fn is_continuation(b: u8) -> bool {
    b & 0xC0 == 0x80
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

/// Where the line breaks of the original file are: the offset of every
/// `\n`, sorted, built once when the buffer is made.
///
/// This full index costs 8 bytes per line, fine for tests and modest files
/// but 320 MB for a 4 GB log. M7 task 8 replaces it with the sparse,
/// background-built index of `line_index.rs`; the four questions below are
/// all the piece table asks of it.
#[derive(Clone)]
struct OriginalLines {
    at: Arc<[usize]>,
}

impl OriginalLines {
    fn build(bytes: &[u8]) -> Self {
        Self {
            at: memchr_iter(b'\n', bytes).collect(),
        }
    }

    fn total(&self) -> usize {
        self.at.len()
    }

    /// How many line breaks come before `offset`.
    fn below(&self, offset: usize) -> usize {
        self.at.partition_point(|&p| p < offset)
    }

    /// How many line breaks are in `range`.
    fn count(&self, range: Range<usize>) -> usize {
        self.below(range.end)
            .saturating_sub(self.below(range.start))
    }

    /// The first line break in `range`.
    fn first(&self, range: Range<usize>) -> Option<usize> {
        self.nth(range, 0)
    }

    /// The last line break in `range`.
    fn last(&self, range: Range<usize>) -> Option<usize> {
        let i = self.below(range.end).checked_sub(1)?;
        self.at.get(i).copied().filter(|&p| p >= range.start)
    }

    /// Line break number `k` (from 0) in `range`.
    fn nth(&self, range: Range<usize>, k: usize) -> Option<usize> {
        let i = self.below(range.start).checked_add(k)?;
        self.at.get(i).copied().filter(|&p| p < range.end)
    }
}

/// A mapped document as it is at one moment: the piece list and the stores
/// it points into. A [`MappedBuffer`] edits one of these in place, and
/// [`Buffer::snapshot`] hands out a copy of it that later edits do not
/// affect. It is `Send + Sync`, so a worker thread can read it.
#[derive(Clone)]
pub struct MappedSnapshot {
    /// The original file.
    source: Arc<dyn Source>,
    /// Line breaks of the original file.
    lines: OriginalLines,
    /// Full add-buffer segments, never written again.
    sealed: Segments,
    /// The add-buffer segment being appended to. Its number is
    /// `sealed.len()`. A snapshot holds a copy of it.
    open: Vec<u8>,
    /// The document: pieces in order, with prefix sums.
    table: Arc<Vec<Entry>>,
}

impl MappedSnapshot {
    fn new(source: Arc<dyn Source>) -> Self {
        let lines = OriginalLines::build(source.bytes());
        let len = source.len();
        let mut table = Vec::new();
        if len > 0 {
            table.push(Entry {
                piece: Piece {
                    store: Store::Original,
                    start: 0,
                    len,
                    newlines: lines.total(),
                },
                at: 0,
                lines: 0,
            });
        }
        Self {
            source,
            lines,
            sealed: Arc::new(Vec::new()),
            open: Vec::new(),
            table: Arc::new(table),
        }
    }

    fn len(&self) -> usize {
        self.table.last().map_or(0, |e| e.at + e.piece.len)
    }

    /// Number of `\n` in the document.
    fn newlines(&self) -> usize {
        self.table.last().map_or(0, |e| e.lines + e.piece.newlines)
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

    /// Line breaks in bytes `from..to` of `piece`.
    fn count(&self, piece: &Piece, from: usize, to: usize) -> usize {
        if from >= to {
            return 0;
        }
        if from == 0 && to >= piece.len {
            return piece.newlines;
        }
        match piece.store {
            Store::Original => self.lines.count(piece.start + from..piece.start + to),
            Store::Add(_) => {
                // Scan whichever is shorter: the range, or what is outside it.
                let b = self.bytes(piece);
                if 2 * (to - from) <= piece.len {
                    count_newlines(b.get(from..to).unwrap_or_default())
                } else {
                    let outside = count_newlines(b.get(..from).unwrap_or_default())
                        + count_newlines(b.get(to..).unwrap_or_default());
                    piece.newlines.saturating_sub(outside)
                }
            }
        }
    }

    /// Offset in `piece` of its first line break at or after `from`.
    fn first_newline(&self, piece: &Piece, from: usize) -> Option<usize> {
        if piece.newlines == 0 {
            return None;
        }
        match piece.store {
            Store::Original => self
                .lines
                .first(piece.start + from..piece.start + piece.len)
                .map(|p| p - piece.start),
            Store::Add(_) => memchr(b'\n', self.bytes(piece).get(from..)?).map(|i| i + from),
        }
    }

    /// Offset in `piece` of its last line break before `to`.
    fn last_newline(&self, piece: &Piece, to: usize) -> Option<usize> {
        if piece.newlines == 0 {
            return None;
        }
        match piece.store {
            Store::Original => self
                .lines
                .last(piece.start..piece.start + to)
                .map(|p| p - piece.start),
            Store::Add(_) => memrchr(b'\n', self.bytes(piece).get(..to)?),
        }
    }

    /// Offset in `piece` of its line break number `k` (from 0).
    fn nth_newline(&self, piece: &Piece, k: usize) -> Option<usize> {
        if k >= piece.newlines {
            return None;
        }
        match piece.store {
            Store::Original => self
                .lines
                .nth(piece.start..piece.start + piece.len, k)
                .map(|p| p - piece.start),
            Store::Add(_) => {
                // Scan from whichever end is nearer.
                let b = self.bytes(piece);
                if k < piece.newlines / 2 {
                    memchr_iter(b'\n', b).nth(k)
                } else {
                    memrchr_iter(b'\n', b).nth(piece.newlines - 1 - k)
                }
            }
        }
    }

    /// Document offset of line break number `n` (from 0).
    fn newline_pos(&self, n: usize) -> Option<usize> {
        let i = self
            .table
            .partition_point(|e| e.lines + e.piece.newlines <= n);
        let e = self.table.get(i)?;
        let k = n.checked_sub(e.lines)?;
        Some(e.at + self.nth_newline(&e.piece, k)?)
    }

    /// Where the content of the line starting at `start` ends, given where
    /// the next line starts: before the `\n`, and before a `\r` ahead of it.
    fn line_end(&self, start: usize, next: Option<usize>) -> usize {
        match next {
            Some(n) => {
                let lf = n - 1;
                if lf > start && self.byte_at(lf - 1) == Some(b'\r') {
                    lf - 1
                } else {
                    lf
                }
            }
            None => self.len(),
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

    /// `range` clamped to the text and widened outward to character
    /// boundaries, as [`Text::slice`] promises.
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

    /// The text in `range` (within the text, on boundaries). Lent without
    /// copying when it lies in one piece.
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
    /// file and the very same sealed segments, each piece within its store.
    fn owns(&self, run: &PieceRun) -> bool {
        let run = &*run.0;
        let fits = |p: &Piece, store_len: usize| {
            p.start
                .checked_add(p.len)
                .is_some_and(|end| end <= store_len)
        };
        std::ptr::addr_eq(Arc::as_ptr(&self.source), Arc::as_ptr(&run.source))
            && run.pieces.iter().all(|p| match p.store {
                Store::Original => fits(p, self.source.len()),
                Store::Add(n) => match (self.sealed.get(n), run.sealed.get(n)) {
                    (Some(mine), Some(theirs)) => Arc::ptr_eq(mine, theirs) && fits(p, mine.len()),
                    _ => false,
                },
            })
    }
}

impl Text for MappedSnapshot {
    fn len_bytes(&self) -> usize {
        self.len()
    }

    fn len_lines(&self) -> usize {
        self.newlines() + 1
    }

    fn line(&self, idx: usize) -> Cow<'_, str> {
        let total = self.newlines();
        if idx > total {
            return Cow::Borrowed("");
        }
        let start = self.line_to_byte(idx);
        let next = (idx < total).then(|| self.line_to_byte(idx + 1));
        self.get(start..self.line_end(start, next))
    }

    fn byte_to_line(&self, byte: usize) -> usize {
        match self.entry(byte) {
            Some(e) => e.lines + self.count(&e.piece, 0, byte - e.at),
            None => self.newlines(),
        }
    }

    fn line_to_byte(&self, line: usize) -> usize {
        match line.checked_sub(1) {
            None => 0,
            Some(n) => self.newline_pos(n).map_or(self.len(), |p| p + 1),
        }
    }

    fn slice(&self, range: Range<usize>) -> Cow<'_, str> {
        self.get(self.widen(range))
    }

    fn is_char_boundary(&self, byte: usize) -> bool {
        match byte.cmp(&self.len()) {
            std::cmp::Ordering::Less => self.byte_at(byte).is_some_and(|b| !is_continuation(b)),
            std::cmp::Ordering::Equal => true,
            std::cmp::Ordering::Greater => false,
        }
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
    /// either side of it, in its own piece first. Pieces without line
    /// breaks are skipped using the prefix sums, so this costs the length
    /// of the line within add-buffer pieces and a few binary searches
    /// otherwise, however long the document.
    fn line_span(&self, byte: usize) -> LineSpan {
        let Some(e) = self.entry(byte) else {
            // At or past the end: the last line.
            let start = self.line_to_byte(self.newlines());
            return LineSpan {
                start,
                end: self.len(),
                next: None,
            };
        };
        let off = byte - e.at;
        let start = match self.last_newline(&e.piece, off) {
            Some(p) => e.at + p + 1,
            // The last line break before this piece, if any.
            None => match e.lines.checked_sub(1) {
                Some(n) => self.newline_pos(n).map_or(0, |p| p + 1),
                None => 0,
            },
        };
        let next = if let Some(p) = self.first_newline(&e.piece, off) {
            Some(e.at + p + 1)
        } else {
            // The first line break after this piece, if any.
            let n = e.lines + e.piece.newlines;
            (n < self.newlines())
                .then(|| self.newline_pos(n))
                .flatten()
                .map(|p| p + 1)
        };
        LineSpan {
            start,
            end: self.line_end(start, next),
            next,
        }
    }

    fn read_into(&self, range: Range<usize>, out: &mut Vec<u8>) -> std::io::Result<()> {
        let end = range.end.min(self.len());
        let start = range.start.min(end);
        out.reserve(end - start);
        self.read_raw(start..end, out);
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
    newlines: usize,
    source: Arc<dyn Source>,
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

    /// How many line breaks it holds.
    #[must_use]
    pub fn newlines(&self) -> usize {
        self.0.newlines
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
            out.extend_from_slice(piece_bytes(&*run.source, &run.sealed, &[], piece));
        }
        Ok(())
    }
}

/// Two runs are equal when they name the same pieces of the same stores:
/// the same original file and the very same segments.
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
        Arc::ptr_eq(&self.0, &other.0)
            || (mine.len == theirs.len
                && std::ptr::addr_eq(Arc::as_ptr(&mine.source), Arc::as_ptr(&theirs.source))
                && mine.pieces == theirs.pieces
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
/// and the result is not too long for an add piece.
fn merged(a: &Piece, b: &Piece, max_piece: usize) -> Option<Piece> {
    let adjacent = a.store == b.store && a.start + a.len == b.start;
    let fits = a.store == Store::Original || a.len + b.len <= max_piece;
    (adjacent && fits).then_some(Piece {
        len: a.len + b.len,
        newlines: a.newlines + b.newlines,
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

/// Recompute the prefix sums of `entries` from index `from` on.
fn fix_prefix(entries: &mut [Entry], from: usize) {
    let (mut at, mut lines) = from
        .checked_sub(1)
        .and_then(|i| entries.get(i))
        .map_or((0, 0), |e| (e.at + e.piece.len, e.lines + e.piece.newlines));
    for e in entries.iter_mut().skip(from) {
        e.at = at;
        e.lines = lines;
        at += e.piece.len;
        lines += e.piece.newlines;
    }
}

/// A piece table over a [`Source`], for files too big to load and files
/// that are not valid UTF-8 (M7). See the module documentation for how it
/// works and the invariants it keeps.
#[derive(Clone)]
pub struct MappedBuffer {
    /// The document now; snapshots are copies of it.
    state: MappedSnapshot,
    limits: Limits,
    /// Reused between edits so that typing does not allocate: the pieces
    /// of the text being inserted, and the stretch of the piece list being
    /// rebuilt around an edit.
    inserted: Vec<Piece>,
    window: Vec<Piece>,
}

impl MappedBuffer {
    /// A buffer showing `source` unchanged, with the default [`Limits`].
    ///
    /// Indexes every line break of the source first (until M7 task 8 moves
    /// that to the background), so this reads the whole source once.
    #[must_use]
    pub fn new(source: Arc<dyn Source>) -> Self {
        Self::with_limits(source, Limits::default())
    }

    /// A buffer showing `source` unchanged, with the given limits (raised
    /// to their minimums if below them).
    #[must_use]
    pub fn with_limits(source: Arc<dyn Source>, limits: Limits) -> Self {
        Self {
            state: MappedSnapshot::new(source),
            limits: Limits {
                copy_limit: limits.copy_limit,
                segment: limits.segment.max(1),
                max_piece: limits.max_piece.max(4),
            },
            inserted: Vec::new(),
            window: Vec::new(),
        }
    }

    /// A buffer whose original content is `bytes`, held in memory.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self::new(Arc::new(VecSource::new(bytes)))
    }

    /// The limits in use.
    #[must_use]
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// The original content the document was opened from.
    #[must_use]
    pub fn source(&self) -> &Arc<dyn Source> {
        &self.state.source
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
        let (segment, base) = if bytes.len() > self.limits.segment {
            // Too big for the open segment: a sealed segment of its own.
            // The open one is sealed first, so the new segment does not
            // take its number.
            self.seal();
            let sealed = Arc::make_mut(&mut self.state.sealed);
            sealed.push(Arc::new(bytes.to_vec()));
            (sealed.len() - 1, 0)
        } else {
            if self.state.open.len() + bytes.len() > self.limits.segment {
                self.seal();
            }
            let base = self.state.open.len();
            self.state.open.extend_from_slice(bytes);
            (self.state.sealed.len(), base)
        };
        let mut from = 0;
        while from < bytes.len() {
            let to = split_point(bytes, from, self.limits.max_piece);
            self.inserted.push(Piece {
                store: Store::Add(segment),
                start: base + from,
                len: to - from,
                newlines: count_newlines(bytes.get(from..to).unwrap_or_default()),
            });
            from = to;
        }
    }

    /// Record what `range` holds before it is replaced: a copy, or the
    /// pieces themselves when longer than the copy limit.
    fn removed(&mut self, range: Range<usize>) -> Content {
        if range.len() <= self.limits.copy_limit {
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
            sealed: Arc::clone(&self.state.sealed),
        })))
    }

    /// Replace `range` (already checked) with `insert`.
    fn apply(&mut self, range: Range<usize>, insert: Insert<'_>) -> Edit {
        let removed = self.removed(range.clone());
        self.inserted.clear();
        let inserted = match insert {
            Insert::Bytes(bytes) => {
                self.append(bytes);
                Content::Bytes(bytes.to_vec())
            }
            Insert::Run(run) => {
                self.inserted.extend_from_slice(&run.0.pieces);
                Content::Pieces(run.clone())
            }
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
    /// are brought up to date.
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
        merge_adjacent(window, self.limits.max_piece);

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

    /// Builds the new piece list in one pass over the old one, instead of
    /// one splice per item, so replacing 100,000 matches is linear in the
    /// number of pieces rather than quadratic.
    fn replace_sorted(&mut self, items: &[(Range<usize>, &str)]) -> Result<Vec<Edit>, EditError> {
        check_sorted(&self.state, items)?;
        if items.is_empty() {
            return Ok(Vec::new());
        }
        // What each item removes, recorded before anything changes.
        let removed: Vec<Content> = items
            .iter()
            .map(|(range, _)| self.removed(range.clone()))
            .collect();
        // Store every new text; `ends[i]` is where item i's pieces end in
        // `self.inserted`.
        self.inserted.clear();
        let mut ends = Vec::with_capacity(items.len());
        for (_, text) in items {
            self.append(text.as_bytes());
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
        merge_adjacent(&mut pieces, self.limits.max_piece);
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
}

/// Prints sizes only.
impl fmt::Debug for MappedBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MappedBuffer")
            .field("state", &self.state)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{History, RopeBuffer, Selections};

    fn text(t: &dyn Text) -> String {
        t.slice(0..usize::MAX).into_owned()
    }

    /// Tiny limits, so a few bytes exercise every path.
    const TINY: Limits = Limits {
        copy_limit: 3,
        segment: 8,
        max_piece: 5,
    };

    fn tiny(initial: &str) -> MappedBuffer {
        MappedBuffer::with_limits(Arc::new(VecSource::from(initial)), TINY)
    }

    /// Invariants 1, 2, 4, 6 and that no two neighbours could be merged.
    fn check_invariants(b: &MappedBuffer) {
        let s = &b.state;
        let (mut at, mut lines) = (0, 0);
        for (i, e) in s.table.iter().enumerate() {
            assert!(e.piece.len > 0, "piece {i} is empty");
            assert_eq!((e.at, e.lines), (at, lines), "prefix sums of piece {i}");
            let bytes = s.bytes(&e.piece);
            assert_eq!(bytes.len(), e.piece.len, "piece {i} fits its store");
            assert_eq!(count_newlines(bytes), e.piece.newlines, "newlines of {i}");
            if let Store::Add(_) = e.piece.store {
                assert!(e.piece.len <= b.limits.max_piece, "add piece {i} too long");
            }
            if i > 0 {
                let prev = &s.table[i - 1].piece;
                assert!(
                    merged(prev, &e.piece, b.limits.max_piece).is_none(),
                    "pieces {} and {i} should have been merged",
                    i - 1
                );
            }
            at += e.piece.len;
            lines += e.piece.newlines;
        }
        let mut prev_end = 0;
        for r in b.original_ranges() {
            assert!(r.start >= prev_end, "original pieces out of order: {r:?}");
            prev_end = r.end;
        }
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
    fn long_removals_are_recorded_as_pieces_and_undo_without_copying() {
        let initial = "abc\ndef\nghi\njkl\n";
        let mut b = tiny(initial);
        b.replace(4..4, "XY\n").unwrap();
        let e = b.replace(2..12, "").unwrap();
        let Content::Pieces(run) = &e.removed else {
            panic!("expected pieces, got {:?}", e.removed);
        };
        assert_eq!((run.len(), run.newlines(), run.piece_count()), (10, 3, 3));
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
        // Full support is task 8; this only checks nothing panics and
        // lengths are kept.
        let b = MappedBuffer::from_bytes(b"ok\xff\xfe\n\x80end".to_vec());
        assert_eq!(b.line(0), "ok\u{1a}\u{1a}");
        assert_eq!(b.line(1).len(), 4);
        let mut raw = Vec::new();
        b.read_into(0..usize::MAX, &mut raw).unwrap();
        assert_eq!(raw, b"ok\xff\xfe\n\x80end");
        let mut s = String::from(">");
        b.slice_into(0..b.len_bytes(), &mut s);
        assert_eq!(s.len(), 1 + b.len_bytes());
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
        assert!(s.len() < 300, "{s}");
        assert!(s.contains("len: 10000"));
    }
}
