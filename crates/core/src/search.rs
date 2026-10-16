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

#[cfg(test)]
mod tests {
    use super::*;
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

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config {
            cases: 256,
            failure_persistence: None,
            ..Default::default()
        })]

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
            let q = Query { pattern: pattern.into(), regex: true, case_sensitive: true };
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
