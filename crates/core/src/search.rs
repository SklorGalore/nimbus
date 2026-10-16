//! Find: literal or regex search over a [`Text`], in chunks.
//!
//! Searching works on chunks of whole lines (about 1 MB each) rather than
//! the whole document at once, for two reasons: the text may not be in one
//! piece (a rope, or a memory-mapped file in M7), and a search on a
//! background thread can stop between chunks when the user types a new
//! query. Each chunk is searched together with a little of the text after
//! it ([`OVERLAP`]), so a match that starts near the end of one chunk is
//! still found; matches are only *kept* if they start inside the chunk, so
//! none are reported twice. The honest limit: a match longer than the
//! overlap that crosses a chunk boundary is missed.

use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use regex::{Regex, RegexBuilder};

use crate::Text;

/// Target size of one search chunk, in bytes.
pub const CHUNK: usize = 1024 * 1024;

/// Extra text searched after each chunk, so matches crossing the chunk end
/// are found. Also the longest such match that is guaranteed to be found.
pub const OVERLAP: usize = 4096;

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
/// Returns `false` if the search stopped early (cancelled or `found`
/// declined), `true` if it reached the end.
pub fn find_all(
    text: &dyn Text,
    re: &Regex,
    mut keep_going: impl FnMut() -> bool,
    mut found: impl FnMut(&[Range<usize>]) -> bool,
) -> bool {
    let len = text.len_bytes();
    let mut start = 0;
    let mut batch = Vec::new();
    while start < len || (start == 0 && len == 0) {
        if !keep_going() {
            return false;
        }
        let end = chunk_end(text, start);
        let window_end = (end + OVERLAP).min(len);
        let window = text.slice(start..window_end);
        batch.clear();
        for m in re.find_iter(&window) {
            let s = start + m.start();
            if s >= end && end < len {
                break; // starts in the overlap: the next chunk reports it
            }
            if m.start() == m.end() && s == len && len > 0 && !batch.is_empty() {
                continue;
            }
            batch.push(s..start + m.end());
        }
        if !found(&batch) {
            return false;
        }
        if end >= len {
            break;
        }
        start = end;
    }
    true
}

/// End of the chunk starting at `start`: the first line start at least
/// [`CHUNK`] bytes later, or the end of the text.
fn chunk_end(text: &dyn Text, start: usize) -> usize {
    let len = text.len_bytes();
    let target = start.saturating_add(CHUNK);
    if target >= len {
        return len;
    }
    let line = text.byte_to_line(target);
    let next = text.line_to_byte(line + 1);
    // A single line longer than a chunk: cut at a character boundary.
    if next >= len || next <= start {
        let mut cut = target;
        while !text.is_char_boundary(cut) {
            cut += 1;
        }
        return cut.min(len);
    }
    next
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
        let expected: Vec<_> = re.find_iter(&text).map(|m| m.range()).collect();
        assert_eq!(all(&text, &q), expected);
        // A match spanning a line break, also across chunk boundaries.
        let q = Query {
            pattern: "here\nline".into(),
            regex: true,
            ..Query::default()
        };
        let re = q.compile().unwrap().unwrap();
        assert_eq!(all(&text, &q).len(), re.find_iter(&text).count());
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
}
