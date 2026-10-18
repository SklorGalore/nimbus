//! Shared pieces of the fuzz targets: turning fuzzer bytes into text that
//! exercises the awkward cases, building buffers, and the `String` oracle
//! checks ported from `crates/core/tests/buffer_oracle.rs`.
//!
//! A fuzz target is a function libFuzzer calls millions of times with
//! inputs it mutates to reach new code. With the `arbitrary` crate the
//! input arrives as a typed value (an edit script, two texts) rather than
//! raw bytes; `#[derive(Arbitrary)]` writes the decoding. Everything here
//! must only *assert* properties: any other panic would be reported as a
//! crash, which is exactly what we want for the code under test and
//! exactly what we do not want from the harness.

use std::ops::Range;
use std::sync::Arc;

use arbitrary::Arbitrary;
use nimbus_core::line_index::{LineIndex, Spacing};
use nimbus_core::{Limits, LineSpan, MappedBuffer, RopeBuffer, Snapshot, Text, VecSource};

/// Building blocks for generated text, as in the property tests: line
/// endings (`\r\n`, whose `\r` belongs to the ending, and a lone `\r`,
/// which is ordinary text), multi-byte characters so most byte offsets are
/// not valid positions, an emoji ZWJ sequence, a combining accent, tabs.
/// The fuzzer could find these by itself, but handing them over makes the
/// interesting texts common from the first second.
pub const PIECES: &[&str] = &[
    "a",
    "Zq",
    "hello ",
    "fn x() {}",
    "\n",
    "\r\n",
    "\r",
    "é",
    "日本",
    "👩\u{200d}💻",
    "e\u{301}",
    "\t",
    "a\n",
    "b\n",
    "    x\n",
    "}\n",
    " ",
    "",
];

/// One piece of a generated text: a common building block, or any string
/// the fuzzer likes.
#[derive(Arbitrary, Debug, Clone)]
pub enum Piece {
    /// `PIECES[i % PIECES.len()]`.
    Common(u8),
    /// Arbitrary text, which lets the fuzzer reach characters and byte
    /// patterns the list above lacks.
    Raw(String),
}

/// A generated text: pieces joined, capped in length so one input cannot
/// make an iteration slow.
#[derive(Arbitrary, Debug, Clone, Default)]
pub struct Txt(pub Vec<Piece>);

impl Txt {
    /// The text, at most about `cap` bytes (cut at a piece boundary, so it
    /// stays valid UTF-8).
    #[must_use]
    pub fn build(&self, cap: usize) -> String {
        let mut out = String::new();
        for p in &self.0 {
            let s = match p {
                Piece::Common(i) => PIECES[usize::from(*i) % PIECES.len()],
                Piece::Raw(s) => s.as_str(),
            };
            if out.len() + s.len() > cap {
                break;
            }
            out.push_str(s);
        }
        out
    }
}

/// A position chosen by the fuzzer, resolved against the char boundaries
/// of the text when the step runs (so it is always a valid position, as
/// `proptest::sample::Index` is in the property tests).
#[derive(Arbitrary, Debug, Clone, Copy)]
pub struct Pos(pub u16);

impl Pos {
    /// The boundary this picks out of `bounds`, which must not be empty.
    #[must_use]
    pub fn get(self, bounds: &[usize]) -> usize {
        bounds[usize::from(self.0) % bounds.len()]
    }
}

/// Every valid cursor position of `s`, including the end.
#[must_use]
pub fn boundaries(s: &str) -> Vec<usize> {
    s.char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(s.len()))
        .collect()
}

/// `a..b` or `b..a`, whichever is in order.
#[must_use]
pub fn span(x: usize, y: usize) -> Range<usize> {
    x.min(y)..x.max(y)
}

/// Limits for the mapped backend that make every path run on small texts
/// (as in the property tests): removals over 4 bytes are recorded as
/// pieces, add-buffer segments seal every 16 bytes, inserted text is cut
/// into pieces of at most 6 bytes, and line breaks more than 3 bytes away
/// are found through the line index.
pub const TINY: Limits = Limits {
    copy_limit: 4,
    segment: 16,
    max_piece: 6,
    scan: 3,
    pseudo_line: 8,
};

/// A rope buffer holding `text`.
#[must_use]
pub fn rope_buffer(text: &str) -> RopeBuffer {
    RopeBuffer::from_text(text)
}

/// A mapped buffer (piece table) over `text`, with [`TINY`] limits and a
/// line index with a checkpoint every 2 lines or 5 bytes.
///
/// # Panics
///
/// If the buffer cannot be built over an in-memory source, which would be
/// a bug worth a crash report.
#[must_use]
pub fn mapped_buffer(text: &str) -> MappedBuffer {
    let index = LineIndex::build(text.as_bytes(), Spacing::new(2, 5));
    match MappedBuffer::with_index(Arc::new(VecSource::from(text)), index, TINY) {
        Ok(b) => b,
        Err(e) => panic!("mapped buffer over {text:?}: {e:?}"),
    }
}

/// A rope snapshot of `text`, for the diff and merge targets.
#[must_use]
pub fn rope(text: &str) -> Arc<dyn Snapshot> {
    use nimbus_core::Buffer as _;
    rope_buffer(text).snapshot().into()
}

/// A mapped snapshot of `text` whose storage is many small pieces of both
/// kinds (original and typed), so readers cross a piece boundary every few
/// bytes. As `mapped` in `crates/core/tests/diff_oracle.rs`.
#[must_use]
pub fn mapped(text: &str) -> Arc<dyn Snapshot> {
    use nimbus_core::Buffer as _;
    let mut b = MappedBuffer::with_limits(Arc::new(VecSource::new(text.as_bytes().to_vec())), TINY);
    if text.len() >= 2 && text.is_char_boundary(1) {
        let first = text[..1].to_owned();
        assert!(b.replace(0..1, "").is_ok());
        assert!(b.replace(0..0, &first).is_ok());
    }
    assert_eq!(b.slice(0..b.len_bytes()), text);
    b.snapshot().into()
}

// ---------------------------------------------------------------------------
// The oracle: the obvious, slow answers from a `String`.

/// Lines split at `\n`; a `\r` right before a `\n` belongs to the ending.
/// The last piece has no `\n` after it, so a trailing lone `\r` stays.
#[must_use]
pub fn oracle_lines(s: &str) -> Vec<&str> {
    let parts: Vec<&str> = s.split('\n').collect();
    let last = parts.len() - 1;
    parts
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            if i < last {
                p.strip_suffix('\r').unwrap_or(p)
            } else {
                p
            }
        })
        .collect()
}

/// Where each line starts.
#[must_use]
pub fn line_starts(s: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(s.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

/// Texts up to this long get every per-byte check; longer ones get them at
/// a sample of offsets, so a long input does not make an iteration slow.
const EVERY_BYTE: usize = 1024;

/// The offsets to check per byte: all of them up to a few past the end for
/// short texts, a spread (plus the ends) for long ones.
fn offsets(len: usize) -> Vec<usize> {
    if len <= EVERY_BYTE {
        (0..=len + 3).collect()
    } else {
        let step = len / EVERY_BYTE + 1;
        let mut v: Vec<usize> = (0..=len).step_by(step).collect();
        v.extend([len.saturating_sub(1), len, len + 1, len + 3]);
        v
    }
}

/// `chunk_at` hands out the buffer's storage as it is, so every answer
/// must be a piece of the oracle at the start it reports, contain the
/// offset asked for, and walking from 0 must rebuild the whole text.
pub fn check_chunks<T: Text + ?Sized>(t: &T, oracle: &str) {
    let len = oracle.len();
    for b in offsets(len).into_iter().chain([usize::MAX]) {
        let (chunk, start) = t.chunk_at(b);
        if b >= len {
            assert_eq!(
                (chunk, start),
                (&[][..], len),
                "chunk_at({b}) at or past the end"
            );
            continue;
        }
        assert!(
            start <= b && b < start + chunk.len(),
            "chunk_at({b}) = {chunk:?} at {start} does not contain it, in {oracle:?}"
        );
        assert_eq!(
            oracle.as_bytes().get(start..start + chunk.len()),
            Some(chunk),
            "chunk_at({b}) is not the text at {start} of {oracle:?}"
        );
    }
    let mut rebuilt = Vec::with_capacity(len);
    let mut pos = 0;
    while pos < len {
        let (chunk, start) = t.chunk_at(pos);
        assert_eq!(
            start, pos,
            "the chunk after one ending at {pos} starts there"
        );
        assert_ne!(chunk, b"", "chunk_at({pos}) is empty before the end");
        rebuilt.extend_from_slice(chunk);
        pos = start + chunk.len();
    }
    assert_eq!(rebuilt, oracle.as_bytes(), "chunks rebuild the text");
}

/// `line_span`, `byte_at` and `read_into` against the oracle.
pub fn check_spans<T: Text + ?Sized>(t: &T, oracle: &str) {
    let len = oracle.len();
    let bytes = oracle.as_bytes();
    let full = len <= EVERY_BYTE;
    let mut start = 0;
    loop {
        let lf = oracle[start..].find('\n').map(|i| start + i);
        let next = lf.map(|i| i + 1);
        let end = match lf {
            Some(i) if i > start && bytes[i - 1] == b'\r' => i - 1,
            Some(i) => i,
            None => len,
        };
        let want = LineSpan { start, end, next };
        let last = next.unwrap_or(len + 1);
        if full {
            for b in start..last {
                assert_eq!(t.line_span(b), want, "line_span({b}) of {oracle:?}");
            }
        } else {
            for b in [start, end, last - 1] {
                assert_eq!(t.line_span(b), want, "line_span({b}) of {oracle:?}");
            }
        }
        match next {
            Some(n) => start = n,
            None => break,
        }
    }
    for b in offsets(len) {
        assert_eq!(t.byte_at(b), bytes.get(b).copied(), "byte_at({b})");
    }
    for (a, z) in [(0, len), (0, 0), (len / 3, len / 2), (len, len + 4)] {
        let mut out = Vec::new();
        assert!(t.read_into(a..z, &mut out).is_ok(), "read_into({a}..{z})");
        let z = z.min(len);
        assert_eq!(out, &bytes[a.min(z)..z], "read_into({a}..{z})");
    }
}

/// Compare every read-only answer of `t` with the oracle: text, length,
/// lines, line starts and ends, offset conversions, char boundaries.
pub fn check_text<T: Text + ?Sized>(t: &T, oracle: &str) {
    let len = oracle.len();
    check_chunks(t, oracle);
    check_spans(t, oracle);
    assert_eq!(t.len_bytes(), len, "len_bytes of {oracle:?}");
    assert_eq!(t.slice(0..len), oracle, "full text");

    let lines = oracle_lines(oracle);
    let n = lines.len();
    assert_eq!(t.len_lines(), n, "len_lines of {oracle:?}");
    for (i, line) in lines.iter().enumerate() {
        assert_eq!(t.line(i), *line, "line({i}) of {oracle:?}");
    }
    for i in n..n + 3 {
        assert_eq!(t.line(i), "", "line({i}) past the end of {oracle:?}");
    }
    assert_eq!(t.line(usize::MAX), "", "line(usize::MAX)");

    let starts = line_starts(oracle);
    for (l, &start) in starts.iter().enumerate() {
        assert_eq!(t.line_to_byte(l), start, "line_to_byte({l}) of {oracle:?}");
        assert_eq!(
            t.byte_to_line(start),
            l,
            "byte_to_line({start}) of {oracle:?}"
        );
        assert_eq!(
            t.slice(start..start + lines[l].len()),
            lines[l],
            "line {l} via slice of {oracle:?}"
        );
    }
    for l in n..n + 3 {
        assert_eq!(t.line_to_byte(l), len, "line_to_byte({l}) past the end");
    }
    assert_eq!(t.line_to_byte(usize::MAX), len, "line_to_byte(usize::MAX)");

    for b in offsets(len) {
        let line = match starts.binary_search(&b.min(len)) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        assert_eq!(t.byte_to_line(b), line, "byte_to_line({b}) of {oracle:?}");
        let boundary = oracle.is_char_boundary(b);
        assert_eq!(
            t.is_char_boundary(b),
            boundary,
            "is_char_boundary({b}) of {oracle:?}"
        );
    }
    assert_eq!(
        t.byte_to_line(usize::MAX),
        n - 1,
        "byte_to_line(usize::MAX)"
    );
}
