//! Find: literal or regex search over a [`Text`], in chunks.
//!
//! Searching works on the raw bytes in chunks of [`CHUNK`] rather than the
//! whole document at once, for two reasons: the text may not be in one
//! piece (a rope, or a memory-mapped file), and a search on a background
//! thread can stop between chunks when the user types a new query. Bytes
//! rather than strings, so files with invalid UTF-8 can be searched, and
//! chunks need no line numbers, so a file still being indexed can be.
//!
//! Each chunk is searched together with a little text before it
//! ([`CONTEXT`]), so `^` and `\b` see what precedes the chunk, and a little
//! after it ([`OVERLAP`]), so a match that starts near the end of one chunk
//! is still found. Matches are only *kept* if they start inside the chunk,
//! so none are reported twice. The honest limit: a match longer than the
//! overlap that crosses a chunk boundary is missed.

use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use regex::bytes::{Regex, RegexBuilder};

use crate::Text;

/// Target size of one search chunk, in bytes.
pub const CHUNK: usize = 1024 * 1024;

/// Extra text searched after each chunk, so matches crossing the chunk end
/// are found. Also the longest such match that is guaranteed to be found.
pub const OVERLAP: usize = 4096;

/// Text included before each chunk so that `^`, `$` and `\b` at its start
/// look at the real text before it; one character is enough, and this is
/// a few.
pub const CONTEXT: usize = 16;

/// What to search for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    /// The text typed in the find field.
    pub pattern: String,
    /// Treat `pattern` as a regular expression rather than literal text.
    pub regex: bool,
    /// Match case exactly.
    pub case_sensitive: bool,
    /// Match only where the pattern starts and ends at a word boundary.
    pub whole_word: bool,
}

/// A query that could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryError(pub String);

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid pattern: {}", self.0)
    }
}

impl std::error::Error for QueryError {}

impl Query {
    /// Compile the query. An empty pattern compiles to `None`: nothing to
    /// find, rather than a match at every position.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] when a regex pattern is malformed or too big.
    pub fn compile(&self) -> Result<Option<Regex>, QueryError> {
        if self.pattern.is_empty() {
            return Ok(None);
        }
        let source = if self.regex {
            self.pattern.clone()
        } else {
            regex::escape(&self.pattern)
        };
        let source = if self.whole_word {
            format!(r"\b(?:{source})\b")
        } else {
            source
        };
        RegexBuilder::new(&source)
            .case_insensitive(!self.case_sensitive)
            // ^ and $ match at line starts and ends, including before \r\n.
            .multi_line(true)
            .crlf(true)
            .size_limit(16 * 1024 * 1024)
            .build()
            .map(Some)
            .map_err(|e| QueryError(e.to_string()))
    }
}

/// Lets a newer search cancel an older one on another thread. Each search
/// takes a generation number; bumping the shared counter makes every
/// search with an older number stop at its next chunk.
#[derive(Debug, Default)]
pub struct Generation(AtomicU64);

impl Generation {
    /// Start a new generation, cancelling searches from older ones.
    pub fn bump(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// The current generation.
    pub fn current(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Find every match of `re` in `text`, calling `found` with the matches of
/// each chunk (in order, possibly empty) as soon as that chunk is searched.
/// `found` returns `false` to stop early.
///
/// Returns `false` if the search stopped early (cancelled, `found`
/// declined, or the text could not be read), `true` if it reached the end.
pub fn find_all(
    text: &dyn Text,
    re: &Regex,
    keep_going: impl FnMut() -> bool,
    found: impl FnMut(&[Range<usize>]) -> bool,
) -> bool {
    find_all_in_chunks(text, re, CHUNK, keep_going, found)
}

/// [`find_all`] with chunks of `chunk` bytes, so tests can cross many chunk
/// boundaries with small texts.
fn find_all_in_chunks(
    text: &dyn Text,
    re: &Regex,
    chunk: usize,
    mut keep_going: impl FnMut() -> bool,
    mut found: impl FnMut(&[Range<usize>]) -> bool,
) -> bool {
    let len = text.len_bytes();
    let mut start: usize = 0;
    let mut batch = Vec::new();
    let mut window = Vec::with_capacity(CONTEXT + chunk + OVERLAP);
    // End of the last match kept. A match that ran past a chunk boundary
    // ends inside the next chunk; anything found there that starts before
    // this point would overlap it, and replacing overlapping ranges would
    // corrupt the text.
    let mut kept_end = 0;
    // End of the last match found, kept or not, as an offset in the text.
    // It carries over from chunk to chunk: an empty match right where a
    // match in the previous chunk ended is not reported, as in one search.
    let mut last_end = None;
    loop {
        if !keep_going() {
            return false;
        }
        let end = start.saturating_add(chunk).min(len);
        let window_start = start.saturating_sub(CONTEXT);
        let window_end = end.saturating_add(OVERLAP).min(len);
        window.clear();
        if text
            .read_into(window_start..window_end, &mut window)
            .is_err()
        {
            return false;
        }
        let at = start - window_start;
        batch.clear();
        let mut pos = at;
        while pos <= window.len() {
            let Some(m) = re.find_at(&window, pos) else {
                break;
            };
            // As the regex crate's own iterator does: an empty match moves
            // the search on by one byte, and none is reported right where
            // the previous match ended.
            pos = if m.end() > m.start() {
                m.end()
            } else {
                m.end() + 1
            };
            let s = window_start + m.start();
            if m.start() == m.end() && last_end == Some(s) {
                continue;
            }
            if s >= end && end < len {
                break; // starts in the overlap: the next chunk reports it
            }
            last_end = Some(window_start + m.end());
            if s < kept_end {
                continue;
            }
            kept_end = window_start + m.end();
            batch.push(s..kept_end);
        }
        if !found(&batch) {
            return false;
        }
        if end >= len {
            return true;
        }
        start = end;
    }
}

/// What [`find_literal`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal {
    /// The first match, on character boundaries.
    Found(Range<usize>),
    /// No match in the range.
    NotFound,
    /// The byte budget ran out before the end of the range; searching
    /// stopped before this offset.
    GaveUp(usize),
}

/// Find the first occurrence of exactly `needle` in `range` of `text`,
/// reading at most about `budget` bytes. Matches must start and end on
/// character boundaries (a needle of whole characters can only fail that
/// next to bytes that are not UTF-8).
///
/// Reads `range` in [`CHUNK`]-sized windows that overlap by one byte less
/// than the needle, so a match across a window edge is still seen, and so
/// a 4 GB file is never read whole: for adding the next occurrence of a
/// selection as a cursor, which happens on a key press.
pub fn find_literal(text: &dyn Text, needle: &[u8], range: Range<usize>, budget: usize) -> Literal {
    let end = range.end.min(text.len_bytes());
    if needle.is_empty() {
        return Literal::NotFound;
    }
    let finder = memchr::memmem::Finder::new(needle);
    let mut window = Vec::new();
    let mut start = range.start;
    let mut read = 0usize;
    while start + needle.len() <= end {
        if read >= budget {
            return Literal::GaveUp(start);
        }
        let stop = (start + CHUNK + needle.len() - 1).min(end);
        window.clear();
        if text.read_into(start..stop, &mut window).is_err() {
            return Literal::NotFound;
        }
        read += window.len();
        for at in finder.find_iter(&window) {
            let (s, e) = (start + at, start + at + needle.len());
            if text.is_char_boundary(s) && text.is_char_boundary(e) {
                return Literal::Found(s..e);
            }
        }
        if stop == end {
            break;
        }
        start = stop + 1 - needle.len();
    }
    Literal::NotFound
}

/// Index of the first match at or after `pos`, wrapping to the first match.
#[must_use]
pub fn next_match(matches: &[Range<usize>], pos: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let i = matches.partition_point(|m| m.start < pos);
    Some(if i == matches.len() { 0 } else { i })
}

/// Index of the last match ending at or before `pos`, wrapping to the last.
#[must_use]
pub fn prev_match(matches: &[Range<usize>], pos: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let i = matches.partition_point(|m| m.start < pos);
    Some(if i == 0 { matches.len() - 1 } else { i - 1 })
}

/// Longest line the block matcher carries whole from one block to the
/// next, in bytes. A line longer than this (a minified file is one line of
/// megabytes) is searched in pieces at block edges, where a match straddling
/// the edge can be missed and `^` can match at the edge.
pub const MAX_CARRY: usize = 4 * 1024 * 1024;

/// One match found by a [`LineMatcher`], with the line it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineHit<'a> {
    /// Line number, counted from 0 by `\n`.
    pub line: usize,
    /// The match, as byte offsets in the whole text.
    pub range: Range<usize>,
    /// The line's bytes, without its line ending.
    pub text: &'a [u8],
    /// Offset of `text` in the whole text.
    pub line_start: usize,
    /// The line the match ends on: `line` unless the match spans lines
    /// ([`MultiLineMatcher`]).
    pub last_line: usize,
    /// The match was longer than [`MULTI_WINDOW`] and is cut short there.
    pub truncated: bool,
}

/// Searches a text fed to it in blocks of any size, finding the same
/// matches whatever the block boundaries, as long as no match spans a line
/// break. Find in files reads each file in blocks through this, so a file
/// is never held in memory whole.
///
/// The idea: a block usually ends in the middle of a line. Searching that
/// partial line would find matches that the full line does not have (a
/// `$` at the cut), and miss ones it does (a word cut in two). So each
/// block is searched only up to its last line break, and the tail after it
/// is *carried* to the front of the next block, where the line is complete.
/// [`LineMatcher::finish`] searches whatever is carried at the end.
///
/// A match that would span a line break (`\s+` over a line ending) is cut
/// at the line: the line is searched again on its own from the match's
/// start, so what is found never depends on which block the next line
/// landed in. Patterns that need the next line (`a\nb`) therefore never
/// match; the in-editor find handles those.
#[derive(Debug)]
pub struct LineMatcher<'r> {
    re: &'r Regex,
    /// Text after the last line break fed so far: the start of a line whose
    /// end has not arrived yet.
    carry: Vec<u8>,
    /// Offset in the whole text of `carry[0]` (or of the next byte to be
    /// fed, when nothing is carried).
    offset: usize,
    /// Line number at `offset`.
    line: usize,
    /// The carry plus the current block, reused between blocks.
    window: Vec<u8>,
    max_carry: usize,
}

impl<'r> LineMatcher<'r> {
    /// A matcher for `re`, positioned at the start of the text.
    #[must_use]
    pub fn new(re: &'r Regex) -> Self {
        Self {
            re,
            carry: Vec::new(),
            offset: 0,
            line: 0,
            window: Vec::new(),
            max_carry: MAX_CARRY,
        }
    }

    /// Search the next block of the text. `found` is called for each match
    /// in order; it returns `false` to stop, and then so does this.
    pub fn feed(&mut self, block: &[u8], found: &mut dyn FnMut(&LineHit<'_>) -> bool) -> bool {
        self.window.clear();
        self.window.append(&mut self.carry);
        self.window.extend_from_slice(block);
        let complete = match memchr::memrchr(b'\n', &self.window) {
            Some(nl) => nl + 1,
            // No complete line yet: carry it all, unless the line is too
            // long to wait for, in which case search what there is.
            None if self.window.len() <= self.max_carry => {
                std::mem::swap(&mut self.carry, &mut self.window);
                return true;
            }
            None => self.window.len(),
        };
        let ok = search_lines(
            self.re,
            &self.window[..complete],
            (self.offset, self.line),
            false,
            found,
        );
        self.carry.extend_from_slice(&self.window[complete..]);
        self.line += memchr::memchr_iter(b'\n', &self.window[..complete]).count();
        self.offset += complete;
        ok
    }

    /// The text has ended: search the last line, which has no line break
    /// after it. The matcher is then back at the start, ready for another
    /// text.
    pub fn finish(&mut self, found: &mut dyn FnMut(&LineHit<'_>) -> bool) -> bool {
        self.window.clear();
        self.window.append(&mut self.carry);
        let ok = search_lines(self.re, &self.window, (self.offset, self.line), true, found);
        self.offset = 0;
        self.line = 0;
        ok
    }
}

/// Search `hay`, which holds whole lines starting at byte `offset` and line
/// `line` of the text; `last` says it ends where the text does. Matches
/// are cut at line breaks (see [`LineMatcher`]).
fn search_lines(
    re: &Regex,
    hay: &[u8],
    (offset, line): (usize, usize),
    last: bool,
    found: &mut dyn FnMut(&LineHit<'_>) -> bool,
) -> bool {
    let mut pos = 0;
    let mut last_end = None;
    let mut line = line;
    // Line breaks before `counted` are in `line` already.
    let mut counted = 0;
    while pos <= hay.len() {
        let Some(m) = re.find_at(hay, pos) else {
            break;
        };
        let (mut start, mut end) = (m.start(), m.end());
        if let Some(nl) = memchr::memchr(b'\n', &hay[start..end]) {
            // The match runs into the next line: take what the line alone
            // gives from the same start, which cannot reach a line break.
            let line_end = start + nl;
            let Some(m) = re.find_at(&hay[..line_end], start) else {
                pos = line_end + 1;
                continue;
            };
            (start, end) = (m.start(), m.end());
        }
        // As the regex crate's own iterator: an empty match moves on by a
        // byte, and none is reported where the previous match ended.
        pos = if end > start { end } else { end + 1 };
        if start == end && last_end == Some(start) {
            continue;
        }
        if start == hay.len() && !last {
            // An empty match at the very end is at the start of the next
            // line, which is searched with the next block.
            break;
        }
        last_end = Some(end);
        line += memchr::memchr_iter(b'\n', &hay[counted..start]).count();
        counted = start;
        let line_start = memchr::memrchr(b'\n', &hay[..start]).map_or(0, |i| i + 1);
        let mut line_end = memchr::memchr(b'\n', &hay[start..]).map_or(hay.len(), |i| start + i);
        if line_end > line_start && hay[line_end - 1] == b'\r' {
            line_end -= 1;
        }
        let hit = LineHit {
            line,
            range: offset + start..offset + end,
            text: &hay[line_start..line_end],
            line_start: offset + line_start,
            last_line: line,
            truncated: false,
        };
        if !found(&hit) {
            return false;
        }
    }
    true
}

/// Longest match a [`MultiLineMatcher`] finds whole, in bytes; longer
/// ones are cut here and marked [`LineHit::truncated`].
pub const MULTI_WINDOW: usize = 1024 * 1024;

/// Whether `query` is meant to match across lines: a regular expression
/// that names a line break (`\n`, or a literal one pasted in). Such a
/// query needs a [`MultiLineMatcher`]; anything else stays on the cheaper
/// line-by-line [`LineMatcher`].
#[must_use]
pub fn spans_lines(query: &Query) -> bool {
    query.regex && (query.pattern.contains("\\n") || query.pattern.contains('\n'))
}

/// Searches a text fed in blocks for matches that may span lines, holding
/// at most [`MULTI_WINDOW`] of it past the block being searched.
///
/// The idea: a match near the end of a block might continue into the next
/// one, so it cannot be reported yet. Every match that *starts* more than
/// the window before the end of what has arrived is final, because a match
/// starting there and running past the end would be longer than the
/// window anyway. Those are reported; the last window's worth of text (and
/// a few bytes before it, so `^` and `\b` see what precedes it) is carried
/// to the front of the next block. A match longer than the window is cut
/// at the window and marked truncated, so one pattern like `(?s).*` cannot
/// make a match, or the memory held for it, grow without bound.
///
/// For a text whose matches are all shorter than the window, the matches
/// are the same as searching the whole text at once, wherever the blocks
/// are cut; the tests check that against a whole-text oracle. A longer
/// match is found only when it arrives within one window plus one block,
/// and is then cut; otherwise it is missed, which is the price of never
/// holding more than that.
#[derive(Debug)]
pub struct MultiLineMatcher<'r> {
    re: &'r Regex,
    /// Text kept from earlier blocks.
    carry: Vec<u8>,
    /// Where in `carry` the next match may start; before it is context.
    resume: usize,
    /// Offset in the whole text of `carry[0]`.
    offset: usize,
    /// Line number at `carry[0]`.
    line: usize,
    /// Where the last reported match ended, as an offset in the whole
    /// text: an empty match there is not reported (as the regex crate's
    /// own iterator does).
    last_end: Option<usize>,
    /// The carry plus the current block, reused between blocks.
    window: Vec<u8>,
    limit: usize,
}

impl<'r> MultiLineMatcher<'r> {
    /// A matcher for `re`, positioned at the start of the text.
    #[must_use]
    pub fn new(re: &'r Regex) -> Self {
        Self::with_window(re, MULTI_WINDOW)
    }

    /// [`MultiLineMatcher::new`] with another window, so tests can use a
    /// small one.
    #[must_use]
    pub fn with_window(re: &'r Regex, limit: usize) -> Self {
        Self {
            re,
            carry: Vec::new(),
            resume: 0,
            offset: 0,
            line: 0,
            last_end: None,
            window: Vec::new(),
            limit: limit.max(1),
        }
    }

    /// Search the next block of the text. `found` is called for each match
    /// in order; it returns `false` to stop, and then so does this.
    pub fn feed(&mut self, block: &[u8], found: &mut dyn FnMut(&LineHit<'_>) -> bool) -> bool {
        self.window.clear();
        self.window.append(&mut self.carry);
        self.window.extend_from_slice(block);
        let last = self.window.len().saturating_sub(self.limit);
        self.search(last, found)
    }

    /// The text has ended: report what is left. The matcher is then back
    /// at the start, ready for another text.
    pub fn finish(&mut self, found: &mut dyn FnMut(&LineHit<'_>) -> bool) -> bool {
        self.window.clear();
        self.window.append(&mut self.carry);
        let len = self.window.len();
        let ok = self.search(len + 1, found);
        *self = Self::with_window(self.re, self.limit);
        ok
    }

    /// Report the matches in the window that start before `before`, then
    /// keep what a later match could still need.
    fn search(&mut self, before: usize, found: &mut dyn FnMut(&LineHit<'_>) -> bool) -> bool {
        let hay = &self.window;
        let mut pos = self.resume;
        let mut line = self.line;
        let mut counted = 0;
        while pos <= hay.len() {
            // No match before `before` now means none: one starting there
            // that more text would complete is longer than the window.
            let Some(m) = self.re.find_at(hay, pos) else {
                break;
            };
            let (start, mut end) = (m.start(), m.end());
            if start >= before {
                break;
            }
            let truncated = end - start > self.limit;
            if truncated {
                end = start + self.limit;
            }
            pos = if end > start { end } else { end + 1 };
            if start == end && self.last_end == Some(self.offset + start) {
                continue;
            }
            self.last_end = Some(self.offset + end);
            line += memchr::memchr_iter(b'\n', &hay[counted..start]).count();
            counted = start;
            let line_start = memchr::memrchr(b'\n', &hay[..start]).map_or(0, |i| i + 1);
            let mut line_end =
                memchr::memchr(b'\n', &hay[start..]).map_or(hay.len(), |i| start + i);
            if line_end > line_start && hay[line_end - 1] == b'\r' {
                line_end -= 1;
            }
            let hit = LineHit {
                line,
                range: self.offset + start..self.offset + end,
                text: &hay[line_start..line_end],
                line_start: self.offset + line_start,
                last_line: line + memchr::memchr_iter(b'\n', &hay[start..end]).count(),
                truncated,
            };
            if !found(&hit) {
                return false;
            }
        }
        // Keep from where the next match may start (never before the
        // window's worth at the end), with a little context before it.
        let keep_at = pos.max(before.min(hay.len())).min(hay.len());
        let keep_from = keep_at.saturating_sub(CONTEXT);
        self.line += memchr::memchr_iter(b'\n', &hay[..keep_from]).count();
        self.offset += keep_from;
        self.resume = keep_at - keep_from;
        self.carry.extend_from_slice(&hay[keep_from..]);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every match of `pattern` in `text` fed in blocks of `block` bytes
    /// with a window of `window`: (range, first line, last line).
    fn multi(
        pattern: &str,
        text: &str,
        block: usize,
        window: usize,
    ) -> Vec<(Range<usize>, usize, usize, bool)> {
        let re = Query {
            pattern: pattern.into(),
            regex: true,
            case_sensitive: true,
            whole_word: false,
        }
        .compile()
        .unwrap()
        .unwrap();
        let mut m = MultiLineMatcher::with_window(&re, window);
        let mut out = Vec::new();
        let mut keep = |h: &LineHit<'_>| {
            out.push((h.range.clone(), h.line, h.last_line, h.truncated));
            true
        };
        for chunk in text.as_bytes().chunks(block.max(1)) {
            assert!(m.feed(chunk, &mut keep));
        }
        assert!(m.finish(&mut keep));
        out
    }

    /// The same, all at once with the regex crate: the oracle.
    fn oracle(pattern: &str, text: &str) -> Vec<(Range<usize>, usize, usize, bool)> {
        let re = Query {
            pattern: pattern.into(),
            regex: true,
            case_sensitive: true,
            whole_word: false,
        }
        .compile()
        .unwrap()
        .unwrap();
        re.find_iter(text.as_bytes())
            .map(|m| {
                let line = text[..m.start()].matches('\n').count();
                let last = line + text[m.range()].matches('\n').count();
                (m.range(), line, last, false)
            })
            .collect()
    }

    #[test]
    fn multi_line_matches_cross_lines_and_blocks() {
        let text = "fn a() {\n    x\n}\nfn b() {\n}\n";
        let want = oracle(r"\{\n\s*\}", text);
        assert_eq!(want.len(), 1);
        assert_eq!(want[0].1, 3);
        assert_eq!(want[0].2, 4);
        for block in 1..text.len() + 1 {
            assert_eq!(multi(r"\{\n\s*\}", text, block, 16), want, "block {block}");
        }
        assert!(spans_lines(&Query {
            pattern: r"a\nb".into(),
            regex: true,
            ..Query::default()
        }));
        assert!(!spans_lines(&Query {
            pattern: r"a\nb".into(),
            ..Query::default()
        }));
    }

    #[test]
    fn multi_line_matches_longer_than_the_window_are_cut() {
        let text = format!("start{}end\n", "x\n".repeat(50));
        // Found whole in one block: reported, cut at the window.
        let found = multi(r"start[\s\S]*end", &text, 200, 32);
        assert_eq!(found.len(), 1);
        let (range, line, last, truncated) = found[0].clone();
        assert_eq!((range.start, range.end - range.start), (0, 32));
        assert_eq!((line, last), (0, 13));
        assert!(truncated);
        // Spread over blocks smaller than itself, it is never seen whole:
        // the window bounds memory, so such a match is missed.
        assert_eq!(multi(r"start[\s\S]*end", &text, 7, 32), Vec::new());
    }

    #[test]
    fn multi_line_agrees_with_a_whole_text_oracle() {
        // A small deterministic generator: texts of a few letters and line
        // breaks, patterns whose matches stay short, every block size.
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let patterns = [
            r"a\nb",
            r"^b",
            r"a$",
            r"\bab\b",
            r"\n\n",
            r"a\n?b",
            r"(?m)^$",
            r"b*",
            r"a[^\n]{0,3}\n[ab]{1,2}",
            r"\r?\n",
        ];
        for _ in 0..300 {
            let len = usize::try_from(next(60)).unwrap();
            let text: String = (0..len)
                .map(|_| match next(5) {
                    0 => 'a',
                    1 => 'b',
                    2 => '\n',
                    3 => ' ',
                    _ => '\r',
                })
                .collect();
            for p in patterns {
                let want = oracle(p, &text);
                let block = usize::try_from(next(9) + 1).unwrap();
                assert_eq!(
                    multi(p, &text, block, 12),
                    want,
                    "pattern {p:?} in {text:?}, blocks of {block}"
                );
            }
        }
    }
    use crate::RopeBuffer;
    use std::fmt::Write as _;

    fn all(text: &str, q: &Query) -> Vec<Range<usize>> {
        let t = RopeBuffer::from_text(text);
        let re = q.compile().unwrap().unwrap();
        let mut out = Vec::new();
        find_all(
            &t,
            &re,
            || true,
            |b| {
                out.extend_from_slice(b);
                true
            },
        );
        out
    }

    fn lit(p: &str) -> Query {
        Query {
            pattern: p.into(),
            ..Query::default()
        }
    }

    #[test]
    fn literal_search_escapes_and_ignores_case_by_default() {
        assert_eq!(all("a.b A.B axb", &lit("a.b")), vec![0..3, 4..7]);
        let exact = Query {
            case_sensitive: true,
            ..lit("a.b")
        };
        assert_eq!(all("a.b A.B", &exact), vec![0..3]);
    }

    #[test]
    fn regex_anchors_work_per_line_including_crlf() {
        let q = Query {
            pattern: "^fo+$".into(),
            regex: true,
            ..Query::default()
        };
        assert_eq!(all("foo\r\nbar\nfoooo", &q), vec![0..3, 9..14]);
    }

    #[test]
    fn literal_search_crosses_windows_and_respects_its_budget() {
        // A needle straddling the first window edge.
        let mut text = "x".repeat(CHUNK - 2);
        text.push_str("needle");
        text.push_str(&"y".repeat(CHUNK));
        text.push_str("needle");
        let t = RopeBuffer::from_text(&text);
        let at = CHUNK - 2;
        assert_eq!(
            find_literal(&t, b"needle", 0..t.len_bytes(), usize::MAX),
            Literal::Found(at..at + 6)
        );
        // From after it: the second one, two windows on.
        let second = text.len() - 6;
        assert_eq!(
            find_literal(&t, b"needle", at + 1..t.len_bytes(), usize::MAX),
            Literal::Found(second..second + 6)
        );
        // With a budget of one window it gives up before the second.
        assert!(matches!(
            find_literal(&t, b"needle", at + 1..t.len_bytes(), CHUNK),
            Literal::GaveUp(_)
        ));
        // Within a range that ends inside the match: not found.
        assert_eq!(
            find_literal(&t, b"needle", 0..at + 5, usize::MAX),
            Literal::NotFound
        );
        // Not in the middle of a character.
        let t = RopeBuffer::from_text("é e");
        assert_eq!(
            find_literal(&t, "e".as_bytes(), 0..4, usize::MAX),
            Literal::Found(3..4)
        );
    }

    #[test]
    fn empty_and_invalid_patterns() {
        assert!(lit("").compile().unwrap().is_none());
        let bad = Query {
            pattern: "(".into(),
            regex: true,
            ..Query::default()
        };
        assert!(bad.compile().is_err());
    }

    #[test]
    fn chunked_search_equals_whole_text_search_across_boundaries() {
        // Lines of ~100 bytes, ~3 MB total: several chunks.
        let mut text = String::new();
        for i in 0..30_000 {
            let _ = writeln!(
                text,
                "line {i:05} with some filler text, needle{} and more filler here",
                i % 7
            );
        }
        let q = Query {
            pattern: "needle[0-3]".into(),
            regex: true,
            ..Query::default()
        };
        let re = q.compile().unwrap().unwrap();
        let expected: Vec<_> = re.find_iter(text.as_bytes()).map(|m| m.range()).collect();
        assert_eq!(all(&text, &q), expected);
        // A match spanning a line break, also across chunk boundaries.
        let q = Query {
            pattern: "here\nline".into(),
            regex: true,
            ..Query::default()
        };
        let re = q.compile().unwrap().unwrap();
        assert_eq!(all(&text, &q).len(), re.find_iter(text.as_bytes()).count());
    }

    #[test]
    fn matches_never_overlap_across_chunk_boundaries() {
        // Every line is "foo" then an indented "bar"; `\s+` matches the
        // line break plus indentation, which straddles chunk boundaries.
        let text = "foo\n    bar\n".repeat(CHUNK / 6);
        let q = Query {
            pattern: r"\s+".into(),
            regex: true,
            ..Query::default()
        };
        let found = all(&text, &q);
        for pair in found.windows(2) {
            assert!(pair[0].end <= pair[1].start, "{pair:?} overlap");
        }
        let re = q.compile().unwrap().unwrap();
        assert_eq!(found.len(), re.find_iter(text.as_bytes()).count());
    }

    #[test]
    fn anchors_and_boundaries_hold_at_chunk_edges() {
        // Chunk edges fall mid-line; `^`, `$` and `\b` must still only
        // match where they would in the whole text.
        let line = "ab cd ".repeat(10_000) + "\n";
        let text = line.repeat(40);
        for pattern in [r"^ab", r"cd $", r"\bcd\b", r"b c"] {
            let q = Query {
                pattern: pattern.into(),
                regex: true,
                case_sensitive: true,
                whole_word: false,
            };
            let re = q.compile().unwrap().unwrap();
            let expected: Vec<_> = re.find_iter(text.as_bytes()).map(|m| m.range()).collect();
            assert_eq!(all(&text, &q), expected, "{pattern}");
        }
    }

    #[test]
    fn a_3_mb_line_without_breaks_is_searched() {
        let text = "x".repeat(3 * CHUNK) + "needle" + &"y".repeat(10);
        assert_eq!(all(&text, &lit("needle")), vec![3 * CHUNK..3 * CHUNK + 6]);
    }

    #[test]
    fn empty_matches_are_found_once_each() {
        let q = Query {
            pattern: "x*".into(),
            regex: true,
            ..Query::default()
        };
        let re = q.compile().unwrap().unwrap();
        let text = "axxb";
        let expected: Vec<_> = re.find_iter(text.as_bytes()).map(|m| m.range()).collect();
        assert_eq!(all(text, &q), expected);
    }

    #[test]
    fn cancellation_stops_between_chunks() {
        let text = "x\n".repeat(CHUNK);
        let t = RopeBuffer::from_text(&text);
        let re = lit("x").compile().unwrap().unwrap();
        let chunks = std::cell::Cell::new(0);
        let finished = find_all(
            &t,
            &re,
            || chunks.get() < 1,
            |_| {
                chunks.set(chunks.get() + 1);
                true
            },
        );
        assert!(!finished);
        assert_eq!(chunks.get(), 1);
    }

    #[test]
    fn next_and_prev_wrap_around() {
        let m = vec![2..3, 5..6, 9..10];
        assert_eq!(next_match(&m, 0), Some(0));
        assert_eq!(next_match(&m, 5), Some(1));
        assert_eq!(next_match(&m, 6), Some(2));
        assert_eq!(next_match(&m, 10), Some(0), "wraps");
        assert_eq!(prev_match(&m, 5), Some(0));
        assert_eq!(prev_match(&m, 2), Some(2), "wraps");
        assert_eq!(next_match(&[], 0), None);
    }

    #[test]
    fn generations_increase() {
        let g = Generation::default();
        let a = g.bump();
        let b = g.bump();
        assert!(b > a);
        assert_eq!(g.current(), b);
    }

    /// Every hit from feeding `text` in blocks of `block` bytes.
    fn hits_in_blocks(
        re: &Regex,
        text: &[u8],
        block: usize,
    ) -> Vec<(usize, Range<usize>, Vec<u8>)> {
        let mut out = Vec::new();
        let mut keep = |h: &LineHit<'_>| {
            out.push((h.line, h.range.clone(), h.text.to_vec()));
            true
        };
        let mut m = LineMatcher::new(re);
        for chunk in text.chunks(block.max(1)) {
            assert!(m.feed(chunk, &mut keep));
        }
        assert!(m.finish(&mut keep));
        out
    }

    fn re(pattern: &str) -> Regex {
        Query {
            pattern: pattern.into(),
            regex: true,
            case_sensitive: true,
            whole_word: false,
        }
        .compile()
        .unwrap()
        .unwrap()
    }

    #[test]
    fn line_matcher_reports_lines_and_their_text() {
        let text = b"one needle\r\ntwo\nthree needle needle\n";
        let got = hits_in_blocks(&re("needle"), text, 7);
        assert_eq!(
            got,
            vec![
                (0, 4..10, b"one needle".to_vec()),
                (2, 22..28, b"three needle needle".to_vec()),
                (2, 29..35, b"three needle needle".to_vec()),
            ]
        );
        // The last line, with no break after it, is found by `finish`.
        let got = hits_in_blocks(&re("^t.*$"), b"one\ntwo\nthree", 1000);
        assert_eq!(got[1].1, 8..13);
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn line_matcher_equals_whole_text_search_for_single_line_patterns() {
        let mut text = Vec::new();
        for i in 0..5_000 {
            text.extend_from_slice(format!("line {i} needle{} ab  cd\n", i % 5).as_bytes());
        }
        for pattern in [
            "needle[0-2]",
            "^line",
            r"cd$",
            r"\bab\b",
            "ab  ",
            "x*",
            " +",
        ] {
            let re = re(pattern);
            let expected: Vec<_> = re.find_iter(&text).map(|m| m.range()).collect();
            for block in [1, 7, 100, 4096, usize::MAX] {
                let got: Vec<_> = hits_in_blocks(&re, &text, block)
                    .into_iter()
                    .map(|h| h.1)
                    .collect();
                assert_eq!(got, expected, "{pattern} in blocks of {block}");
            }
        }
    }

    #[test]
    fn line_matcher_cuts_matches_at_line_breaks() {
        // `\s+` would match the break and the indentation after it as one.
        let got = hits_in_blocks(&re(r"\s+"), b"a \n  b", 100);
        let ranges: Vec<_> = got.iter().map(|h| h.1.clone()).collect();
        assert_eq!(ranges, vec![1..2, 3..5]);
        assert_eq!(got[1].0, 1);
    }

    #[test]
    fn a_line_longer_than_the_carry_is_searched_in_pieces() {
        let text = "x".repeat(100) + "needle" + &"y".repeat(100);
        let re = re("needle|^x");
        let mut m = LineMatcher::new(&re);
        m.max_carry = 50;
        let mut n = 0;
        let mut count = |_: &LineHit<'_>| {
            n += 1;
            true
        };
        for chunk in text.as_bytes().chunks(60) {
            m.feed(chunk, &mut count);
        }
        m.finish(&mut count);
        // The real line start plus the cut ones; the needle lies in one piece.
        assert!(n >= 2, "{n}");
        assert_eq!(hits_in_blocks(&re, text.as_bytes(), 1000).len(), 2);
    }

    #[test]
    fn line_matcher_stops_when_asked_and_can_be_reused() {
        let re = re("a");
        let mut m = LineMatcher::new(&re);
        let mut seen = 0;
        let mut stop = |_: &LineHit<'_>| {
            seen += 1;
            false
        };
        assert!(!m.feed(b"a a\n", &mut stop));
        assert_eq!(seen, 1);
        assert!(m.finish(&mut |_| true));
        assert_eq!(hits_in_blocks(&re, b"b\na", 1)[0].0, 1);
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config {
            cases: 256,
            failure_persistence: None,
            ..Default::default()
        })]

        /// Feeding the text in blocks of any size finds the same lines and
        /// matches as feeding it whole.
        #[test]
        fn line_matcher_ignores_block_boundaries(
            pieces in proptest::collection::vec(
                proptest::sample::select(vec!["ab", "a", "b", " ", "\n", "\r\n", "é", "日", "ba ", "aaa", "\n\n"]),
                0..80,
            ),
            pattern in proptest::sample::select(vec![
                "a", "ab", "^a", "b$", r"\ba", r"a\b", r"\s+", "a+", "^$", "é", r"\w+",
                "a*", "b?", r"\s*", "^", "$", r"[ab]+$", r"^\s*b",
            ]),
            block in 1usize..12,
        ) {
            let text: Vec<u8> = pieces.concat().into_bytes();
            let re = re(pattern);
            let whole = hits_in_blocks(&re, &text, usize::MAX);
            let blocks = hits_in_blocks(&re, &text, block);
            proptest::prop_assert_eq!(blocks, whole, "{:?} in {:?}", pattern, text);
        }

        /// Searching in tiny chunks finds exactly what one search of the
        /// whole text finds, for patterns that look at line starts, word
        /// boundaries and line breaks around the chunk edges.
        #[test]
        fn chunked_search_equals_one_search(
            pieces in proptest::collection::vec(
                proptest::sample::select(vec!["ab", "a", "b", " ", "\n", "\r\n", "é", "日", "ba ", "aaa"]),
                0..80,
            ),
            pattern in proptest::sample::select(vec![
                "a", "ab", "^a", "b$", r"\ba", r"a\b", r"\s+", "a+", "b\na", "^$", "é", r"\w+",
                // Patterns that can match nothing, next to a match that
                // ended at a chunk boundary.
                "a*", "b?", r"\s*",
            ]),
            chunk in 1usize..12,
        ) {
            let text: String = pieces.concat();
            let q = Query { pattern: pattern.into(), regex: true, case_sensitive: true, whole_word: false };
            let re = q.compile().unwrap().unwrap();
            let expected: Vec<_> = re.find_iter(text.as_bytes()).map(|m| m.range()).collect();
            let t = RopeBuffer::from_text(&text);
            let mut got = Vec::new();
            find_all_in_chunks(&t, &re, chunk, || true, |b| {
                got.extend_from_slice(b);
                true
            });
            proptest::prop_assert_eq!(got, expected, "{:?} in {:?}", pattern, text);
        }
    }
}
