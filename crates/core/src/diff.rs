//! Line diffs for compare and merge (M9).
//!
//! A diff is computed in two steps. First an [`Interner`] turns every line
//! of each text into a small integer, giving equal lines the same integer,
//! so comparing two lines is comparing two numbers. Then the diff algorithm
//! (histogram, from `imara-diff`) works on those integer sequences and
//! reports [`Hunk`]s: runs of lines on one side that were replaced by runs
//! on the other.
//!
//! Lines are read straight from the text's storage chunks
//! ([`Text::chunk_at`]) and hashed as they stream past, so no line is ever
//! copied, however long it is, and the mapped backend's file is read in
//! order. Two lines with the same hash are compared byte by byte before
//! they share a token, so a hash collision can only cost time, never a
//! wrong diff.
//!
//! A line's token covers its ending too (`\n` or `\r\n`), so a change of
//! line ending shows as a change, and applying the hunks to the old text
//! reproduces the new text exactly, which the oracle tests in
//! `tests/diff_oracle.rs` check. A text that ends with a newline has no
//! extra empty line after it; a text that does not has a last line without
//! an ending. Line numbers agree with [`Text::line_to_byte`] either way.
//!
//! **Options (M18).** An interner made with [`Interner::with_options`] can
//! treat lines as equal when they differ only in whitespace or case, and
//! can drop changes that only add or remove blank lines. That is a change
//! to what "equal" means, not to the algorithm: each line is normalised
//! (whitespace trimmed or removed, letters lowercased) before it is hashed
//! and compared, so the diff below runs unchanged. It can also cut lines
//! longer than [`SPLIT_AT`] into pieces ([`crate::split`]); each piece is
//! then a token of its own.
//!
//! Memory: 4 bytes per line per side for the tokens, and about 40 bytes
//! per *distinct* line in the interner (where a copy of it lives, its
//! hash, its indentation). The texts are kept as snapshots, which share
//! storage with the live document, for the comparisons on collision.

use std::collections::HashMap;
use std::fmt;
use std::hash::{BuildHasherDefault, Hasher};
use std::ops::Range;
use std::sync::Arc;

use imara_diff::{Algorithm, Diff, IndentHeuristic, IndentLevel, Token};

use crate::split::{self, SPLIT_AT};
use crate::{Snapshot, Text};

/// How whitespace counts when comparing lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Whitespace {
    /// Every byte counts.
    #[default]
    Exact,
    /// Whitespace at the start and end of a line (its indentation, any
    /// trailing spaces and the line ending) is ignored; inside the line
    /// it counts.
    Ends,
    /// All whitespace is ignored: `a b` equals `ab`.
    All,
}

/// What two lines must share to count as equal. The default is exact,
/// which the merge views always use: a merge must not lose a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Normalize {
    /// How whitespace counts.
    pub whitespace: Whitespace,
    /// ASCII letters compare without case. Only ASCII, because a byte at a
    /// time keeps hashing streaming; other scripts compare exactly.
    pub case: bool,
    /// Changes made only of blank (whitespace-only) lines are dropped, as
    /// `git diff --ignore-blank-lines` does.
    pub blank_lines: bool,
}

impl Normalize {
    /// Every byte counts and nothing is dropped.
    #[must_use]
    pub fn is_exact(self) -> bool {
        self == Self::default()
    }

    /// Normalise the line in `buf` in place: whitespace trimmed or
    /// removed, then ASCII letters lowercased.
    fn apply(self, buf: &mut Vec<u8>) {
        match self.whitespace {
            Whitespace::Exact => {}
            Whitespace::Ends => {
                let end = buf.iter().rposition(|&b| !is_space(b)).map_or(0, |i| i + 1);
                buf.truncate(end);
                let start = buf.iter().position(|&b| !is_space(b)).unwrap_or(buf.len());
                buf.drain(..start);
            }
            Whitespace::All => buf.retain(|&b| !is_space(b)),
        }
        if self.case {
            buf.make_ascii_lowercase();
        }
    }
}

/// ASCII whitespace, line endings included.
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0B | 0x0C)
}

/// The most text, per side, that gets a full line diff: 256 MB, the same
/// as the rope threshold (DESIGN.md §4.1). Above it the layered diff (M9
/// task 2a) works in bounded windows instead. Set from measurement (M9
/// doc, Results): time is fine well past this (interning runs at about
/// 230 MB/s and Myers at over 20 million lines per second on similar
/// texts), so memory sets the limit: tokens cost 4 bytes per line per side
/// and the interner about 40 bytes per distinct line, which for a log of
/// unique 80-byte lines is about 150 MB at this size, and would be 2.5 GB
/// for a 4 GB log.
pub const DIFF_LIMIT: usize = 256 << 20;

/// A run of lines that differ: lines `old` of the old text were replaced by
/// lines `new` of the new text. Either range may be empty (a pure insertion
/// or deletion), not both. Hunks come sorted and never touch or overlap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// Line range in the old text.
    pub old: Range<usize>,
    /// Line range in the new text.
    pub new: Range<usize>,
}

/// Why a text could not be interned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffError {
    /// More distinct lines than the token type can number (over four
    /// billion); the diff algorithm works in `u32`.
    TooManyLines,
    /// A text is over [`DIFF_LIMIT`], so a live diff (which holds a token
    /// per line) is refused; [`crate::compare`] handles it in layers.
    TooLarge,
}

impl fmt::Display for DiffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyLines => f.write_str("too many distinct lines to diff"),
            Self::TooLarge => f.write_str("text too large for a live diff"),
        }
    }
}

impl std::error::Error for DiffError {}

/// The lines of one text as tokens, produced by [`Interner::intern`] and
/// only meaningful with the interner that made them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lines {
    pub(crate) tokens: Vec<Token>,
}

impl Lines {
    /// How many lines the text has, by the convention in the module
    /// documentation.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// The text had no lines at all (it was empty).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

/// Where a copy of a token's line can be read, for comparing on collision.
#[derive(Debug, Clone, Copy)]
struct Rep {
    text: u32,
    start: usize,
    len: usize,
}

/// Maps equal lines to equal tokens across any number of texts.
///
/// Create one, [`intern`](Interner::intern) each text, then
/// [`diff`](Interner::diff) any two of the results. A three-way merge
/// interns three texts and diffs two pairs; a local re-diff after an edit
/// interns the new text and keeps the old tokens.
#[derive(Default)]
pub struct Interner {
    /// Every interned text, kept alive so representatives stay readable.
    texts: Vec<Arc<dyn Snapshot>>,
    /// Hash of a line to its token. The key is already a hash, so the map
    /// uses it as is.
    table: HashMap<u64, Token, BuildHasherDefault<Identity>>,
    /// Lines whose hash another, different line already had. Practically
    /// always empty; checked only when a hash matches but the bytes differ.
    overflow: Vec<(u64, Token)>,
    /// A representative line per token.
    reps: Vec<Rep>,
    /// Indentation per token, for sliding hunks to readable positions.
    indents: Vec<IndentLevel>,
    /// Per token: its line is blank (whitespace only). Filled only when
    /// blank-line changes are dropped.
    blank: Vec<bool>,
    /// What counts as equal.
    normalize: Normalize,
    /// Cut lines over [`SPLIT_AT`] into pieces.
    split: bool,
    /// A line (or piece) being normalised, and a representative's, reused.
    line: Vec<u8>,
    rep_line: Vec<u8>,
    /// Cut points of the long line being interned, reused.
    cuts: Vec<usize>,
}

impl fmt::Debug for Interner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Interner")
            .field("texts", &self.texts.len())
            .field("tokens", &self.reps.len())
            .field("overflow", &self.overflow.len())
            .finish_non_exhaustive()
    }
}

impl Interner {
    /// An interner with no lines yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An interner that compares lines by `normalize` and, when `split`
    /// is set, cuts long lines into pieces. Tokens are then units: whole
    /// lines and pieces ([`crate::split::Splits`] maps them back).
    #[must_use]
    pub fn with_options(normalize: Normalize, split: bool) -> Self {
        Self {
            normalize,
            split,
            ..Self::default()
        }
    }

    /// How many distinct lines have been seen.
    #[must_use]
    pub fn num_tokens(&self) -> usize {
        self.reps.len()
    }

    /// How many texts the interner keeps alive: one per call to
    /// [`intern_range`](Interner::intern_range), since a line's bytes are
    /// checked against the text it was first seen in. A live diff grows
    /// this by one per edit, so its owner starts over past a bound.
    #[must_use]
    pub fn pinned_texts(&self) -> usize {
        self.texts.len()
    }

    /// Turn every line of `text` into a token. Reads the text once, in
    /// storage order, without copying any line.
    ///
    /// # Errors
    ///
    /// [`DiffError::TooManyLines`] when distinct lines run past `u32`.
    pub fn intern(&mut self, text: &Arc<dyn Snapshot>) -> Result<Lines, DiffError> {
        self.intern_range(text, 0..text.len_bytes())
    }

    /// Like [`intern`](Interner::intern) for the lines in `range`, which
    /// must start at a line start and end at a line end (after a `\n`, or
    /// at the end of the text); a range that does not is widened to the
    /// line boundaries around it only in the sense that its bytes are
    /// split into tokens as they are. Used by the layered diff to work on
    /// the part between a common start and end.
    ///
    /// # Errors
    ///
    /// [`DiffError::TooManyLines`] when distinct lines run past `u32`.
    pub fn intern_range(
        &mut self,
        text: &Arc<dyn Snapshot>,
        range: Range<usize>,
    ) -> Result<Lines, DiffError> {
        let Ok(text_idx) = u32::try_from(self.texts.len()) else {
            return Err(DiffError::TooManyLines);
        };
        self.texts.push(Arc::clone(text));
        let text = Arc::clone(text);
        let len = range.end.min(text.len_bytes());
        let mut tokens = Vec::with_capacity(len.saturating_sub(range.start) / 32);
        if !self.normalize.is_exact() {
            let mut start = range.start;
            while start < len {
                let end = split::line_end(&*text, start, len);
                self.intern_unit_or_pieces(&*text, text_idx, start..end, &mut tokens)?;
                start = end;
            }
            return Ok(Lines { tokens });
        }
        let mut hash = LineHash::default();
        let mut line_start = range.start;
        let mut head = LineHead::default();
        let mut pos = range.start;
        while pos < len {
            let (chunk, base) = text.chunk_at(pos);
            let Some(bytes) = chunk.get(pos - base..) else {
                break;
            };
            let bytes = &bytes[..bytes.len().min(len - pos)];
            if bytes.is_empty() {
                break;
            }
            let mut off = 0;
            while let Some(i) = memchr::memchr(b'\n', &bytes[off..]) {
                let end = off + i + 1;
                let piece = &bytes[off..end];
                head.write(piece);
                let indent = head.indent();
                head = LineHead::default();
                hash.write(piece);
                let line_end = pos + end;
                if self.split && line_end - line_start > SPLIT_AT {
                    self.intern_unit_or_pieces(
                        &*text,
                        text_idx,
                        line_start..line_end,
                        &mut tokens,
                    )?;
                    hash = LineHash::default();
                    line_start = line_end;
                    off = end;
                    continue;
                }
                // The whole line is in this chunk when it started here.
                let contiguous = (line_start >= pos).then_some(piece);
                let token = self.token_for(
                    hash.finish(),
                    Rep {
                        text: text_idx,
                        start: line_start,
                        len: line_end - line_start,
                    },
                    indent,
                    &*text,
                    contiguous,
                )?;
                tokens.push(token);
                hash = LineHash::default();
                line_start = line_end;
                off = end;
            }
            let rest = &bytes[off..];
            if !rest.is_empty() {
                head.write(rest);
                hash.write(rest);
            }
            pos = base + chunk.len();
        }
        if self.split && len - line_start > SPLIT_AT {
            self.intern_unit_or_pieces(&*text, text_idx, line_start..len, &mut tokens)?;
        } else if line_start < len {
            let token = self.token_for(
                hash.finish(),
                Rep {
                    text: text_idx,
                    start: line_start,
                    len: len - line_start,
                },
                head.indent(),
                &*text,
                None,
            )?;
            tokens.push(token);
        }
        Ok(Lines { tokens })
    }

    /// The token for the line at `rep` in `text`, which hashes to `hash`:
    /// an existing one when a line with the same bytes was seen, else new.
    /// `bytes` is the line itself when it sits in one storage chunk, which
    /// saves looking it up again for the comparison.
    fn token_for(
        &mut self,
        hash: u64,
        rep: Rep,
        indent: IndentLevel,
        text: &dyn Text,
        bytes: Option<&[u8]>,
    ) -> Result<Token, DiffError> {
        let candidate = self.table.get(&hash).copied();
        let mut new_hash = true;
        if let Some(token) = candidate {
            new_hash = false;
            if self.same_bytes(token, rep, text, bytes) {
                return Ok(token);
            }
            for &(h, token) in &self.overflow {
                if h == hash && self.same_bytes(token, rep, text, bytes) {
                    return Ok(token);
                }
            }
        }
        let Ok(id) = u32::try_from(self.reps.len()) else {
            return Err(DiffError::TooManyLines);
        };
        let token = Token(id);
        self.reps.push(rep);
        self.indents.push(indent);
        self.blank.push(false);
        if new_hash {
            self.table.insert(hash, token);
        } else {
            self.overflow.push((hash, token));
        }
        Ok(token)
    }

    /// Intern the line at `line` (its ending included): one token, or
    /// one per piece when splitting is on and the line is long.
    fn intern_unit_or_pieces(
        &mut self,
        text: &dyn Text,
        text_idx: u32,
        line: Range<usize>,
        tokens: &mut Vec<Token>,
    ) -> Result<(), DiffError> {
        let content = split::content_end(text, line.start, line.end);
        if !self.split || content - line.start <= SPLIT_AT {
            tokens.push(self.unit_token(text, text_idx, line)?);
            return Ok(());
        }
        let mut cuts = std::mem::take(&mut self.cuts);
        cuts.clear();
        split::cuts(text, line.start..content, &mut cuts);
        let mut start = line.start;
        for &k in &cuts {
            let end = line.start + k;
            tokens.push(self.unit_token(text, text_idx, start..end)?);
            start = end;
        }
        // The last piece keeps the line ending.
        let last = self.unit_token(text, text_idx, start..line.end);
        self.cuts = cuts;
        tokens.push(last?);
        Ok(())
    }

    /// The token of the bytes `unit` (a line or a piece), read from the
    /// text again: the slower path, for normalised compares and pieces.
    fn unit_token(
        &mut self,
        text: &dyn Text,
        text_idx: u32,
        unit: Range<usize>,
    ) -> Result<Token, DiffError> {
        let rep = Rep {
            text: text_idx,
            start: unit.start,
            len: unit.len(),
        };
        if self.normalize.is_exact() {
            let mut hash = LineHash::default();
            let mut head = LineHead::default();
            for_each_chunk(text, unit, |bytes| {
                head.write(bytes);
                hash.write(bytes);
            });
            return self.token_for(hash.finish(), rep, head.indent(), text, None);
        }
        let mut line = std::mem::take(&mut self.line);
        line.clear();
        for_each_chunk(text, unit, |bytes| line.extend_from_slice(bytes));
        let indent = IndentLevel::for_ascii_line(line.iter().take(64).copied(), 8);
        let blank = self.normalize.blank_lines && line.iter().all(|&b| is_space(b));
        self.normalize.apply(&mut line);
        let mut hash = LineHash::default();
        hash.write(&line);
        let token = self.normalized_token(hash.finish(), rep, indent, blank, &line);
        self.line = line;
        token
    }

    /// Like [`token_for`](Interner::token_for) for a normalised line:
    /// representatives are normalised again to compare, so nothing but
    /// their place in the text is stored.
    fn normalized_token(
        &mut self,
        hash: u64,
        rep: Rep,
        indent: IndentLevel,
        blank: bool,
        line: &[u8],
    ) -> Result<Token, DiffError> {
        let candidate = self.table.get(&hash).copied();
        if let Some(token) = candidate {
            if self.same_normalized(token, line) {
                return Ok(token);
            }
            let overflow: Vec<Token> = self
                .overflow
                .iter()
                .filter(|(h, _)| *h == hash)
                .map(|&(_, t)| t)
                .collect();
            for token in overflow {
                if self.same_normalized(token, line) {
                    return Ok(token);
                }
            }
        }
        let Ok(id) = u32::try_from(self.reps.len()) else {
            return Err(DiffError::TooManyLines);
        };
        let token = Token(id);
        self.reps.push(rep);
        self.indents.push(indent);
        self.blank.push(blank);
        if candidate.is_none() {
            self.table.insert(hash, token);
        } else {
            self.overflow.push((hash, token));
        }
        Ok(token)
    }

    /// Whether `token`'s representative, normalised, equals `line`.
    fn same_normalized(&mut self, token: Token, line: &[u8]) -> bool {
        let Some(&known) = self.reps.get(token.0 as usize) else {
            return false;
        };
        let Some(other) = self.texts.get(known.text as usize) else {
            return false;
        };
        let mut buf = std::mem::take(&mut self.rep_line);
        buf.clear();
        for_each_chunk(&**other, known.start..known.start + known.len, |b| {
            buf.extend_from_slice(b);
        });
        self.normalize.apply(&mut buf);
        let same = buf == line;
        self.rep_line = buf;
        same
    }

    /// Whether the line at `rep` in `text` has the same bytes as `token`'s
    /// representative. `text` is `self.texts[rep.text]`, passed separately
    /// so the caller's borrow of it does not conflict with `&mut self`.
    fn same_bytes(&self, token: Token, rep: Rep, text: &dyn Text, bytes: Option<&[u8]>) -> bool {
        let Some(known) = self.reps.get(token.0 as usize) else {
            return false;
        };
        if known.len != rep.len {
            return false;
        }
        let Some(other) = self.texts.get(known.text as usize) else {
            return false;
        };
        match bytes {
            Some(bytes) => slice_equal(&**other, known.start, bytes),
            None => bytes_equal(&**other, known.start, text, rep.start, rep.len),
        }
    }

    /// The hunks that turn `old` into `new`. Both must come from this
    /// interner.
    ///
    /// Uses Myers' algorithm with `imara-diff`'s cut-offs for large inputs,
    /// then slides hunks to indentation-friendly positions. The M9 plan
    /// expected histogram diff, but measurement (M9 doc, Results) showed
    /// its weakness on text where most lines repeat, which is exactly
    /// what source code and logs are: on a synthetic 60 MB pair it took
    /// 19 s against 0.19 s for Myers, and on the 10 MB bench file (the
    /// project's sources repeated) it merged 291 edits into 83 coarse
    /// hunks where Myers found each one. Time is close to linear in the
    /// number of lines when the texts are similar; the cut-offs keep
    /// unrelated texts from going quadratic at the cost of a coarser
    /// result.
    #[must_use]
    pub fn diff(&self, old: &Lines, new: &Lines) -> Vec<Hunk> {
        self.diff_with(Algorithm::Myers, old, new)
    }

    fn diff_with(&self, algorithm: Algorithm, old: &Lines, new: &Lines) -> Vec<Hunk> {
        self.diff_tokens_with(algorithm, &old.tokens, &new.tokens)
    }

    /// [`diff`](Interner::diff) over slices of token sequences: the live
    /// diff re-diffs the region around an edit this way.
    pub(crate) fn diff_tokens(&self, old: &[Token], new: &[Token]) -> Vec<Hunk> {
        self.diff_tokens_with(Algorithm::Myers, old, new)
    }

    fn diff_tokens_with(&self, algorithm: Algorithm, old: &[Token], new: &[Token]) -> Vec<Hunk> {
        let Ok(num_tokens) = u32::try_from(self.reps.len()) else {
            return Vec::new();
        };
        let mut diff = Diff::default();
        diff.compute_with(algorithm, old, new, num_tokens);
        let indent_of = |token: Token| {
            self.indents
                .get(token.0 as usize)
                .copied()
                .unwrap_or_else(|| IndentLevel::for_ascii_line(std::iter::empty(), 8))
        };
        diff.postprocess_with(old, new, IndentHeuristic::new(indent_of));
        let blank_only = |h: &Hunk| {
            let blank = |t: &Token| self.blank.get(t.0 as usize).copied().unwrap_or(false);
            old.get(h.old.clone())
                .is_some_and(|ts| ts.iter().all(blank))
                && new
                    .get(h.new.clone())
                    .is_some_and(|ts| ts.iter().all(blank))
        };
        diff.hunks()
            .map(|h| Hunk {
                old: h.before.start as usize..h.before.end as usize,
                new: h.after.start as usize..h.after.end as usize,
            })
            .filter(|h| !(self.normalize.blank_lines && blank_only(h)))
            .collect()
    }
}

/// Call `f` on the bytes of `range` of `t`, chunk by chunk.
fn for_each_chunk(t: &dyn Text, range: Range<usize>, mut f: impl FnMut(&[u8])) {
    let mut pos = range.start;
    while pos < range.end {
        let (chunk, base) = t.chunk_at(pos);
        let Some(bytes) = chunk.get(pos - base..) else {
            return;
        };
        let bytes = &bytes[..bytes.len().min(range.end - pos)];
        if bytes.is_empty() {
            return;
        }
        f(bytes);
        pos += bytes.len();
    }
}

/// Compare the bytes of `a` from `pa` with `bytes`, chunk by chunk.
fn slice_equal(a: &dyn Text, mut pa: usize, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        let (chunk, start) = a.chunk_at(pa);
        let Some(chunk) = chunk.get(pa - start..) else {
            return false;
        };
        let k = bytes.len().min(chunk.len());
        if k == 0 || chunk[..k] != bytes[..k] {
            return false;
        }
        pa += k;
        bytes = &bytes[k..];
    }
    true
}

/// Compare `n` bytes of `a` from `pa` with `n` bytes of `b` from `pb`,
/// chunk by chunk, without copying either.
fn bytes_equal(a: &dyn Text, mut pa: usize, b: &dyn Text, mut pb: usize, mut n: usize) -> bool {
    while n > 0 {
        let (ca, sa) = a.chunk_at(pa);
        let (cb, sb) = b.chunk_at(pb);
        let (Some(ca), Some(cb)) = (ca.get(pa - sa..), cb.get(pb - sb..)) else {
            return false;
        };
        let k = n.min(ca.len()).min(cb.len());
        if k == 0 || ca[..k] != cb[..k] {
            return false;
        }
        pa += k;
        pb += k;
        n -= k;
    }
    true
}

/// The first bytes of a line, gathered across chunks, for its indentation.
/// Enough for any indentation the slider heuristic distinguishes (it caps
/// at 200 columns); the result depends only on the line's content, never
/// on where chunk boundaries fell, so both buffer backends diff alike.
struct LineHead {
    bytes: [u8; 64],
    len: usize,
}

impl Default for LineHead {
    fn default() -> Self {
        Self {
            bytes: [0; 64],
            len: 0,
        }
    }
}

impl LineHead {
    fn write(&mut self, bytes: &[u8]) {
        let room = self.bytes.len() - self.len;
        let n = bytes.len().min(room);
        self.bytes[self.len..self.len + n].copy_from_slice(&bytes[..n]);
        self.len += n;
    }

    fn indent(&self) -> IndentLevel {
        IndentLevel::for_ascii_line(self.bytes[..self.len].iter().copied(), 8)
    }
}

/// A streaming hash over a line's bytes, fed in any pieces: the result
/// depends only on the byte sequence, not on where the chunk boundaries
/// fell. Fast and weak on purpose: equal hashes are always confirmed by
/// comparing bytes, so a collision costs a comparison, nothing more.
#[derive(Default)]
struct LineHash {
    state: u64,
    /// Bytes not yet mixed in, little-endian in the low `pending_len * 8`
    /// bits.
    pending: u64,
    pending_len: u32,
    total: u64,
}

impl LineHash {
    const MUL: u64 = 0x9E37_79B9_7F4A_7C15;

    fn mix(&mut self, word: u64) {
        self.state = (self.state.rotate_left(27) ^ word).wrapping_mul(Self::MUL);
    }

    fn write(&mut self, mut bytes: &[u8]) {
        self.total += bytes.len() as u64;
        while self.pending_len > 0 && !bytes.is_empty() {
            self.push_byte(bytes[0]);
            bytes = &bytes[1..];
        }
        // Whole 8-byte words first, then the bytes left over.
        let (words, rest) = bytes.as_chunks::<8>();
        for word in words {
            self.mix(u64::from_le_bytes(*word));
        }
        for &b in rest {
            self.push_byte(b);
        }
    }

    fn push_byte(&mut self, b: u8) {
        self.pending |= u64::from(b) << (8 * self.pending_len);
        self.pending_len += 1;
        if self.pending_len == 8 {
            let word = self.pending;
            self.pending = 0;
            self.pending_len = 0;
            self.mix(word);
        }
    }

    fn finish(&self) -> u64 {
        let mut h = self.state;
        if self.pending_len > 0 {
            h = (h.rotate_left(27) ^ self.pending).wrapping_mul(Self::MUL);
        }
        h ^= self.total;
        // Final avalanche so the high bits, which the hash table's control
        // bytes come from, depend on every input bit.
        h ^= h >> 32;
        h = h.wrapping_mul(0xD6E8_FEB8_6659_FD93);
        h ^= h >> 32;
        h
    }
}

/// Hasher for keys that are already hashes: returns the `u64` as is.
#[derive(Default)]
struct Identity(u64);

impl Hasher for Identity {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(b);
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Buffer, RopeBuffer};

    fn snap(text: &str) -> Arc<dyn Snapshot> {
        RopeBuffer::from_text(text).snapshot().into()
    }

    fn hunks(old: &str, new: &str) -> (Interner, Lines, Lines, Vec<Hunk>) {
        let mut interner = Interner::new();
        let a = interner.intern(&snap(old)).unwrap();
        let b = interner.intern(&snap(new)).unwrap();
        let hunks = interner.diff(&a, &b);
        (interner, a, b, hunks)
    }

    #[test]
    fn line_counts_follow_the_convention() {
        let mut i = Interner::new();
        assert_eq!(i.intern(&snap("")).unwrap().len(), 0);
        assert_eq!(i.intern(&snap("a")).unwrap().len(), 1);
        assert_eq!(i.intern(&snap("a\n")).unwrap().len(), 1);
        assert_eq!(i.intern(&snap("a\nb")).unwrap().len(), 2);
        assert_eq!(i.intern(&snap("\n\n")).unwrap().len(), 2);
    }

    #[test]
    fn equal_lines_share_tokens_across_texts() {
        let mut i = Interner::new();
        let a = i.intern(&snap("x\ny\nx\n")).unwrap();
        let b = i.intern(&snap("y\nx\n")).unwrap();
        assert_eq!(a.tokens[0], a.tokens[2]);
        assert_eq!(a.tokens[1], b.tokens[0]);
        assert_eq!(a.tokens[0], b.tokens[1]);
        assert_eq!(i.num_tokens(), 2);
    }

    #[test]
    fn line_endings_are_part_of_the_line() {
        let (i, ..) = hunks("a\n", "a\r\n");
        assert_eq!(i.num_tokens(), 2);
        let (_, _, _, h) = hunks("a\n", "a\r\n");
        assert_eq!(
            h,
            vec![Hunk {
                old: 0..1,
                new: 0..1
            }]
        );
        let (_, _, _, h) = hunks("a\nb", "a\nb\n");
        assert_eq!(
            h,
            vec![Hunk {
                old: 1..2,
                new: 1..2
            }]
        );
    }

    #[test]
    fn identical_texts_have_no_hunks() {
        let (_, _, _, h) = hunks("a\nb\nc\n", "a\nb\nc\n");
        assert!(h.is_empty(), "{h:?}");
        let (_, _, _, h) = hunks("", "");
        assert!(h.is_empty(), "{h:?}");
    }

    #[test]
    fn insert_delete_and_change() {
        let (_, _, _, h) = hunks("a\nb\nc\n", "a\nx\nb\nc\n");
        assert_eq!(
            h,
            vec![Hunk {
                old: 1..1,
                new: 1..2
            }]
        );
        let (_, _, _, h) = hunks("a\nb\nc\n", "a\nc\n");
        assert_eq!(
            h,
            vec![Hunk {
                old: 1..2,
                new: 1..1
            }]
        );
        let (_, _, _, h) = hunks("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(
            h,
            vec![Hunk {
                old: 1..2,
                new: 1..2
            }]
        );
        let (_, _, _, h) = hunks("a\nb\nc\n", "");
        assert_eq!(
            h,
            vec![Hunk {
                old: 0..3,
                new: 0..0
            }]
        );
    }

    #[test]
    fn hash_is_independent_of_chunking() {
        let data: Vec<u8> = (0..100u8).collect();
        let mut whole = LineHash::default();
        whole.write(&data);
        for split in [0, 1, 3, 7, 8, 9, 16, 50, 99, 100] {
            let mut parts = LineHash::default();
            parts.write(&data[..split]);
            parts.write(&data[split..]);
            assert_eq!(parts.finish(), whole.finish(), "split at {split}");
        }
        let mut bytewise = LineHash::default();
        for b in &data {
            bytewise.write(std::slice::from_ref(b));
        }
        assert_eq!(bytewise.finish(), whole.finish());
    }

    #[test]
    fn collisions_are_told_apart_by_bytes() {
        // Force every line onto one hash by faking the table: intern two
        // different lines, then check that a third equal to the first gets
        // the first's token even when the second shares its hash.
        let mut i = Interner::new();
        let a = i.intern(&snap("p\n")).unwrap();
        let hash = *i.table.keys().next().unwrap();
        let b = i.intern(&snap("q\n")).unwrap();
        // Move `q` under `p`'s hash as if they had collided.
        let q_hash = *i.table.keys().find(|&&h| h != hash).unwrap();
        let q_tok = i.table.remove(&q_hash).unwrap();
        i.overflow.push((hash, q_tok));
        let text = snap("q\n");
        let rep = Rep {
            text: 2,
            start: 0,
            len: 2,
        };
        i.texts.push(Arc::clone(&text));
        let tok = i
            .token_for(
                hash,
                rep,
                IndentLevel::for_ascii_line(*b"q\n", 8),
                &*text,
                None,
            )
            .unwrap();
        assert_eq!(tok, b.tokens[0]);
        assert_ne!(tok, a.tokens[0]);
        assert_eq!(i.num_tokens(), 2);
    }

    /// Synthetic source-like text: `lines` lines drawn from a vocabulary
    /// with a long tail, so most lines repeat and some are unique, as in
    /// real code. Deterministic.
    fn synthetic(lines: usize, seed: u64) -> String {
        use std::fmt::Write as _;
        let mut s = String::with_capacity(lines * 40);
        let mut x = seed | 1;
        for i in 0..lines {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let kind = x % 10;
            match kind {
                0 => s.push_str("}\n"),
                1 => s.push_str("    }\n"),
                2 => s.push('\n'),
                3 => s.push_str("        return Ok(());\n"),
                4 => s.push_str("    // comment about the next block\n"),
                _ => {
                    let _ = writeln!(
                        s,
                        "    let value_{} = compute({}, {});",
                        x % 5000,
                        i % 97,
                        x % 13
                    );
                }
            }
        }
        s
    }

    /// Edit `text` in `edits` places: replace a few lines with new ones.
    fn edited(text: &str, edits: usize, seed: u64) -> String {
        use std::fmt::Write as _;
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let mut out = String::with_capacity(text.len());
        let mut x = seed | 1;
        let mut next_edit = lines.len() / (edits + 1);
        for (i, line) in lines.iter().enumerate() {
            if i == next_edit {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let _ = writeln!(out, "    let changed_{} = other();", x % 1000);
                if !x.is_multiple_of(3) {
                    out.push_str(line);
                }
                next_edit += lines.len() / (edits + 1);
            } else {
                out.push_str(line);
            }
        }
        out
    }

    /// `cargo test --release -p nimbus-core -- --ignored --nocapture diff_bench`
    ///
    /// Synthetic pairs by default; `NIMBUS_DIFF_FILES=OLD:NEW` diffs two
    /// files instead (for example `bench/out/src-10mb.rs` and an edited
    /// copy).
    #[test]
    #[ignore = "benchmark: run in release with --nocapture (M9 task 2)"]
    fn diff_bench() {
        let pairs: Vec<(String, String, String)> = if let Ok(spec) =
            std::env::var("NIMBUS_DIFF_FILES")
        {
            let (a, b) = spec.split_once(':').expect("OLD:NEW");
            vec![(
                spec.clone(),
                std::fs::read_to_string(a).expect("read OLD"),
                std::fs::read_to_string(b).expect("read NEW"),
            )]
        } else {
            {
                let mut pairs: Vec<(String, String, String)> = [(250_000, 200), (2_500_000, 2_000)]
                    .into_iter()
                    .map(|(lines, edits)| {
                        let old = synthetic(lines, 7);
                        let new = edited(&old, edits, 11);
                        (format!("synthetic {lines} lines"), old, new)
                    })
                    .collect();
                // Two texts with nothing in common but the short repeated
                // lines: the worst case for an edit-distance algorithm.
                pairs.push((
                    "unrelated 250000 lines".to_owned(),
                    synthetic(250_000, 7),
                    synthetic(250_000, 99),
                ));
                pairs
            }
        };
        for (name, old, new) in pairs {
            let (os, ns) = (snap(&old), snap(&new));
            let t = std::time::Instant::now();
            let mut i = Interner::new();
            let ol = i.intern(&os).unwrap();
            let nl = i.intern(&ns).unwrap();
            let interned = t.elapsed();
            println!(
                "{name}: {} MB + {} MB, {} + {} lines, {} distinct; intern {interned:?}",
                old.len() >> 20,
                new.len() >> 20,
                ol.len(),
                nl.len(),
                i.num_tokens(),
            );
            for algorithm in [Algorithm::Myers, Algorithm::Histogram] {
                let t = std::time::Instant::now();
                let hunks = i.diff_with(algorithm, &ol, &nl);
                println!(
                    "  {algorithm:?}: {} hunks in {:?}",
                    hunks.len(),
                    t.elapsed()
                );
            }
        }
    }
}
