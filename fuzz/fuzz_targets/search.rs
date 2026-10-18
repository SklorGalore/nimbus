//! Fuzz search: the chunked whole-document search (`find_all`), the
//! windowed literal search (`find_literal`), and the block matchers that
//! find in files uses (`LineMatcher`, `MultiLineMatcher`).
//!
//! Each is compared with the obvious oracle, one search of the whole text
//! with the regex crate, under the limits each one documents:
//!
//! - `find_all` reads 1 MB chunks with a 4 KB overlap. Matches never
//!   overlap and come in order; and when no match (found or expected) is
//!   longer than the overlap, the matches are exactly the oracle's. Some
//!   literal queries get the fuzzer's text across the first chunk
//!   boundary, behind a megabyte of filler, so the chunk edge falls
//!   inside it.
//! - `find_literal` finds the first occurrence of the needle on character
//!   boundaries within the range, also across its window edge.
//! - `LineMatcher` finds the same matches whatever the block size.
//! - `MultiLineMatcher` with a small window finds exactly the oracle's
//!   matches when every match is within the window, whatever the block
//!   size.
//!
//! The chunk size of `find_all` is fixed at 1 MB in the public API (the
//! unit tests use a private smaller one), hence the filler.

#![no_main]

use std::ops::Range;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use nimbus_core::search::{
    self, CHUNK, LineHit, LineMatcher, Literal, MultiLineMatcher, OVERLAP, Query,
};
use nimbus_core::{Buffer as _, RopeBuffer, Text};
use nimbus_fuzz::{Pos, Txt, mapped_buffer, rope_buffer};
use regex::bytes::Regex;

const MAX_TEXT: usize = 2048;
const MAX_PATTERN: usize = 48;

#[derive(Arbitrary, Debug)]
struct Input {
    text: Txt,
    pattern: Pattern,
    regex: bool,
    case_sensitive: bool,
    whole_word: bool,
    /// Bytes per block for the block matchers (1 to 64).
    block: u8,
    /// Window of the multi-line matcher (1 to 32).
    window: u8,
    /// Put the text across the first 1 MB chunk boundary, this many bytes
    /// (mod 256) of it before the edge, after filler made of this piece.
    /// Literal queries only (see below).
    straddle: Option<(u8, Filler)>,
    /// A needle for `find_literal`, taken from the text: start and length.
    needle: (Pos, u8),
    /// The range for `find_literal`.
    range: (Option<Pos>, Pos),
}

/// The pattern: either fuzzer text, or a few pieces of regex syntax that
/// look at line and word edges, so those cases come up quickly.
#[derive(Arbitrary, Debug)]
enum Pattern {
    Raw(String),
    Pieces(Vec<u8>),
}

const SYNTAX: &[&str] = &[
    "a", "b", "é", " ", "^", "$", r"\b", r"\B", r"\s", r"\w", r"\n", r"\r", ".", "*", "+", "?",
    "|", "(", ")", "[ab]", "[^\\n]", "{0,3}", "(?s)", "(?-m)", "\\z", "\\A", "x*",
];

#[derive(Arbitrary, Debug, Clone, Copy)]
enum Filler {
    Space,
    Letter,
    Newline,
    Accent,
}

impl Pattern {
    fn build(&self) -> String {
        let mut out = String::new();
        match self {
            Pattern::Raw(s) => {
                for c in s.chars() {
                    if out.len() + c.len_utf8() > MAX_PATTERN {
                        break;
                    }
                    out.push(c);
                }
            }
            Pattern::Pieces(v) => {
                for &i in v.iter().take(16) {
                    out.push_str(SYNTAX[usize::from(i) % SYNTAX.len()]);
                }
            }
        }
        out
    }
}

impl Filler {
    fn piece(self) -> &'static str {
        match self {
            Filler::Space => " ",
            Filler::Letter => "a",
            Filler::Newline => "\n",
            Filler::Accent => "é",
        }
    }
}

const ALL_FILLERS: [Filler; 4] = [
    Filler::Space,
    Filler::Letter,
    Filler::Newline,
    Filler::Accent,
];

thread_local! {
    /// A rope of a little over one chunk of each filler. Building a
    /// megabyte rope for every input would dominate the run time; ropes
    /// share structure, so each input clones one and cuts it down.
    static FILLERS: Vec<RopeBuffer> = ALL_FILLERS
        .iter()
        .map(|f| rope_buffer(&f.piece().repeat(CHUNK / f.piece().len() + 1)))
        .collect();
}

/// `hay` behind filler, so that `before` bytes of it (or all of it, if
/// shorter) come before the first chunk boundary: the text and a rope
/// holding it.
fn straddle(hay: &str, before: u8, filler: Filler) -> (String, RopeBuffer) {
    let piece = filler.piece();
    let before = usize::from(before).min(hay.len());
    // Whole pieces only, so the filler is valid text.
    let filler_len = (CHUNK - before) / piece.len() * piece.len();
    let mut text = piece.repeat(filler_len / piece.len());
    text.push_str(hay);
    let mut rope = FILLERS.with(|f| f[filler as usize].clone());
    let cut = rope.len_bytes() - filler_len;
    assert!(rope.replace(0..cut, "").is_ok());
    assert!(rope.replace(filler_len..filler_len, hay).is_ok());
    (text, rope)
}

fuzz_target!(|input: Input| {
    let hay = input.text.build(MAX_TEXT);
    let hay_len = hay.len();
    let query = Query {
        pattern: input.pattern.build(),
        regex: input.regex,
        case_sensitive: input.case_sensitive,
        whole_word: input.whole_word,
    };
    let Ok(Some(re)) = query.compile() else {
        // Empty and invalid patterns: nothing to search with.
        return;
    };

    let block = usize::from(input.block % 64) + 1;
    line_matcher(&re, hay.as_bytes(), block);
    let window = usize::from(input.window % 32) + 1;
    multi_line_matcher(&re, hay.as_bytes(), block, window);

    // Only literal queries go behind the filler: a megabyte through an
    // instrumented regex costs about half a second, a literal about 10 ms.
    // The regex features that look across a chunk edge (`^`, `$`, `\b`)
    // are covered at small chunk sizes by the unit tests in search.rs.
    let (text, rope) = match input.straddle {
        Some((before, filler)) if !input.regex => straddle(&hay, before, filler),
        _ => {
            let rope = rope_buffer(&hay);
            (hay, rope)
        }
    };
    let expected: Vec<Range<usize>> = re.find_iter(text.as_bytes()).map(|m| m.range()).collect();
    find_all(&rope, &re, &text, &expected);
    if text.len() <= MAX_TEXT {
        // The piece table with tiny limits, so reads cross many pieces.
        find_all(&mapped_buffer(&text), &re, &text, &expected);
    }

    // Positions near the fuzzer's text only: the boundaries of a megabyte
    // of filler would cost more than the search.
    let mut tail = text.len() - (hay_len + 64).min(text.len());
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    let bounds: Vec<usize> = nimbus_fuzz::boundaries(&text[tail..])
        .into_iter()
        .map(|b| b + tail)
        .collect();
    let start = input.needle.0.get(&bounds);
    let mut end = (start + usize::from(input.needle.1 % 16) + 1).min(text.len());
    while !text.is_char_boundary(end) {
        end += 1;
    }
    let needle = &text[start..end];
    // From the start (so the window edge falls inside a long text) or
    // from somewhere near the fuzzer's text.
    let a = input.range.0.map_or(0, |p| p.get(&bounds));
    let b = input.range.1.get(&bounds);
    find_literal(&rope, &text, needle, a.min(b)..a.max(b));
});

/// Every match `find_all` reports, in order, and whether it finished.
fn find_all(t: &dyn Text, re: &Regex, text: &str, expected: &[Range<usize>]) {
    let mut got: Vec<Range<usize>> = Vec::new();
    let finished = search::find_all(
        t,
        re,
        || true,
        |b| {
            got.extend_from_slice(b);
            true
        },
    );
    assert!(finished, "find_all stopped early");
    let mut prev_end = 0;
    for m in &got {
        assert!(
            m.start <= m.end && m.end <= text.len(),
            "match {m:?} out of bounds"
        );
        assert!(
            m.start >= prev_end,
            "matches overlap or are out of order: {got:?}"
        );
        prev_end = m.end;
    }
    let long = |ms: &[Range<usize>]| ms.iter().any(|m| m.len() > OVERLAP);
    if !long(expected) && !long(&got) {
        assert_eq!(got, expected, "find_all {re:?} in {:?}", short(text));
    }
}

/// The first occurrence of `needle` in `range` on character boundaries.
fn find_literal(t: &dyn Text, text: &str, needle: &str, range: Range<usize>) {
    let got = search::find_literal(t, needle.as_bytes(), range.clone(), usize::MAX);
    // The needle is whole characters of valid text, so every place its
    // bytes occur is on character boundaries, and `str::find` is the
    // oracle.
    let want = if needle.is_empty() {
        Literal::NotFound
    } else {
        text[range.clone()]
            .find(needle)
            .map_or(Literal::NotFound, |i| {
                Literal::Found(range.start + i..range.start + i + needle.len())
            })
    };
    assert_eq!(
        got,
        want,
        "find_literal {needle:?} in {range:?} of {:?}",
        short(text)
    );
}

/// Every hit, as (line, range, line text, last line, truncated).
type Hit = (usize, Range<usize>, Vec<u8>, usize, bool);

/// The two block matchers, fed the same way.
trait Blocks {
    fn feed(&mut self, block: &[u8], found: &mut dyn FnMut(&LineHit<'_>) -> bool) -> bool;
    fn finish(&mut self, found: &mut dyn FnMut(&LineHit<'_>) -> bool) -> bool;
}

impl Blocks for LineMatcher<'_> {
    fn feed(&mut self, block: &[u8], found: &mut dyn FnMut(&LineHit<'_>) -> bool) -> bool {
        LineMatcher::feed(self, block, found)
    }
    fn finish(&mut self, found: &mut dyn FnMut(&LineHit<'_>) -> bool) -> bool {
        LineMatcher::finish(self, found)
    }
}

impl Blocks for MultiLineMatcher<'_> {
    fn feed(&mut self, block: &[u8], found: &mut dyn FnMut(&LineHit<'_>) -> bool) -> bool {
        MultiLineMatcher::feed(self, block, found)
    }
    fn finish(&mut self, found: &mut dyn FnMut(&LineHit<'_>) -> bool) -> bool {
        MultiLineMatcher::finish(self, found)
    }
}

/// Feed `text` to `m` in blocks of `block` bytes and collect every hit.
fn hits(m: &mut dyn Blocks, text: &[u8], block: usize) -> Vec<Hit> {
    let mut out = Vec::new();
    let mut keep = |h: &LineHit<'_>| {
        out.push((
            h.line,
            h.range.clone(),
            h.text.to_vec(),
            h.last_line,
            h.truncated,
        ));
        true
    };
    for chunk in text.chunks(block) {
        assert!(m.feed(chunk, &mut keep), "feed stopped");
    }
    assert!(m.finish(&mut keep), "finish stopped");
    out
}

/// Whether `re` anchors to the start or end of the whole text: `\A`, `\z`,
/// or `^` and `$` with multi-line mode switched off. Unparseable (which
/// cannot happen for a compiled regex) counts as yes, to skip the check.
fn anchors_to_text(re: &Regex) -> bool {
    regex_syntax::ParserBuilder::new()
        .utf8(false)
        .multi_line(true)
        .crlf(true)
        .build()
        .parse(re.as_str())
        .map_or(true, |hir| {
            hir.properties().look_set().contains_anchor_haystack()
        })
}

fn newlines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| b == b'\n').count()
}

/// Line-by-line matching ignores block boundaries, and each hit's line
/// number and line text are right.
fn line_matcher(re: &Regex, text: &[u8], block: usize) {
    let whole = hits(&mut LineMatcher::new(re), text, text.len().max(1));
    let blocks = hits(&mut LineMatcher::new(re), text, block);
    // The matcher searches each run of whole lines as a text of its own,
    // so `\A` and `\z` match at the ends of every run: a known limit (see
    // fuzz/README.md), so block independence is only checked without them.
    if !anchors_to_text(re) {
        assert_eq!(
            blocks,
            whole,
            "line matcher {re:?} in {:?}, blocks of {block}",
            short_bytes(text)
        );
    }
    let mut prev_end = 0;
    for (line, range, line_text, last, truncated) in &whole {
        assert!(
            range.start >= prev_end && range.start <= range.end,
            "hits out of order: {whole:?}"
        );
        prev_end = range.end;
        assert_eq!(*line, newlines(&text[..range.start]), "line of {range:?}");
        assert_eq!(last, line, "a line hit stays on its line");
        assert!(!truncated);
        let start = text[..range.start]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1);
        let mut end = text[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(text.len(), |i| start + i);
        assert!(range.end <= end, "match {range:?} runs past its line");
        if end > start && text[end - 1] == b'\r' {
            end -= 1;
        }
        assert_eq!(line_text, &text[start..end], "line text of {range:?}");
    }
}

/// Matching across lines in blocks with a small window gives the oracle's
/// matches whenever every match fits the window.
fn multi_line_matcher(re: &Regex, text: &[u8], block: usize, window: usize) {
    // The line text is left out: a hit's line may start before the text
    // the matcher still holds, so it is only what is in view.
    let got: Vec<Hit> = hits(&mut MultiLineMatcher::with_window(re, window), text, block)
        .into_iter()
        .map(|(line, range, _, last, truncated)| (line, range, Vec::new(), last, truncated))
        .collect();
    let mut prev_end = 0;
    for (_, range, _, _, _) in &got {
        assert!(range.start <= range.end && range.end <= text.len());
        assert!(
            range.start >= prev_end,
            "multi-line hits out of order: {got:?}"
        );
        assert!(
            range.len() <= window,
            "hit {range:?} longer than the window {window}"
        );
        prev_end = range.end;
    }
    let oracle: Vec<Hit> = re
        .find_iter(text)
        .map(|m| {
            let line = newlines(&text[..m.start()]);
            let last = line + newlines(&text[m.range()]);
            (line, m.range(), Vec::new(), last, false)
        })
        .collect();
    let fits = oracle.iter().all(|h| h.1.len() <= window);
    let none_cut = got.iter().all(|h| !h.4);
    if fits && none_cut {
        assert_eq!(
            got,
            oracle,
            "multi-line matcher {re:?} in {:?}, blocks of {block}, window {window}",
            short_bytes(text)
        );
    }
}

/// The text for a failure message, without a megabyte of filler.
fn short(text: &str) -> String {
    if text.len() <= 4096 {
        format!("{text:?}")
    } else {
        let mut at = text.len() - 4096;
        while !text.is_char_boundary(at) {
            at += 1;
        }
        format!("<{at} bytes of filler>{:?}", &text[at..])
    }
}

fn short_bytes(text: &[u8]) -> String {
    format!("{:?}", String::from_utf8_lossy(text))
}
