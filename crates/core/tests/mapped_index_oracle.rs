//! Property tests: a [`MappedBuffer`] whose line index is still being
//! built (M7 task 8) must agree with a plain `Vec<u8>` oracle.
//!
//! A buffer opens with its original file indexed only up to some prefix.
//! Random edits (before and after the indexed part), undo and redo are
//! interleaved with chunks from a [`Scanner`] fed the original bytes, which
//! [`MappedBuffer::merge_index`] adds to the index. After every step the
//! buffer is checked against an oracle that holds the document's bytes and,
//! for each byte, the offset in the original file it came from (`None` for
//! typed text). From those alone the oracle works out `K`, the document
//! offset up to which line numbers must be known: the position of the first
//! byte that came from the original at or past the indexed frontier `F`, or
//! the length if there is none. Then:
//!
//! - `indexed_bytes` is `K`, and `is_fully_indexed` says whether `K` is the
//!   length;
//! - `line_number`, `byte_to_line`, `line_to_byte`, `len_lines` and `line`
//!   are exact up to `K`, give the documented lower bounds past it, and are
//!   exact everywhere once fully indexed;
//! - `line_span` at every offset matches the lines the oracle derives from
//!   the bytes, `K` and the pseudo-line rule (with tiny scan and pseudo-line
//!   sizes, so the rule applies to short texts), and is exactly the real
//!   line wherever that line ends by `K`;
//! - snapshots keep the answers they had when taken.
//!
//! Every case ends by indexing the rest of the file, after which every
//! answer must be exact.

use std::sync::Arc;

use nimbus_core::line_index::{LineIndex, Scanner, Spacing};
use nimbus_core::{
    Buffer, Content, Edit, Limits, LineSpan, MappedBuffer, SUBSTITUTE, Snapshot, Text, VecSource,
};
use proptest::prelude::*;
use proptest::sample::Index;
use proptest::test_runner::Config;

/// Building blocks for the original file: line endings, runs without line
/// breaks long enough to need pseudo-lines, multi-byte characters (so
/// pseudo-line points move back to a character start) and invalid bytes.
const PIECES: &[&[u8]] = &[
    b"\n",
    b"\n",
    b"\r\n",
    b"\r",
    b"ab",
    b"xxxxxxxxxxxxxxxx",
    b"yyyyyyy",
    "é".as_bytes(),
    "日本".as_bytes(),
    "😀".as_bytes(),
    b"\xff",
    b"\xe2\x82",
];

/// Text inserted with `replace`.
const TYPED: &[&str] = &["\n", "\r\n", "z", "zzzzzzzzzzzz", "日", "a\nb", ""];

fn config(cases: u32) -> Config {
    Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    }
}

fn original() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(prop::sample::select(PIECES), 0..40).prop_map(|v| v.concat())
}

/// Tiny limits: removals over 4 bytes are kept as pieces, line breaks are
/// looked for 1 to 5 bytes each way, and pseudo-lines are a few bytes
/// longer than that.
fn limits() -> impl Strategy<Value = Limits> {
    (1usize..6, 4usize..16).prop_map(|(scan, extra)| Limits {
        copy_limit: 4,
        segment: 16,
        max_piece: 6,
        scan,
        pseudo_line: scan + extra,
    })
}

fn spacing() -> impl Strategy<Value = Spacing> {
    (1u64..4, 1u64..12).prop_map(|(lines, bytes)| Spacing::new(lines, bytes))
}

/// One step. Positions are `Index`es, resolved against the oracle's unit
/// boundaries when the step runs.
#[derive(Debug, Clone)]
enum Op {
    Replace {
        a: Index,
        b: Index,
        text: &'static str,
    },
    /// Feed the scanner up to this many more bytes of the original, then
    /// merge what it found; `finish` tells it the file ends if it does.
    Index {
        feed: usize,
        finish: bool,
    },
    Undo,
    Redo,
    Snapshot,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (any::<Index>(), any::<Index>(), prop::sample::select(TYPED))
            .prop_map(|(a, b, text)| Op::Replace { a, b, text }),
        3 => (0usize..48, any::<bool>()).prop_map(|(feed, finish)| Op::Index { feed, finish }),
        2 => Just(Op::Undo),
        1 => Just(Op::Redo),
        1 => Just(Op::Snapshot),
    ]
}

/// For each offset `0..=len`, whether it is a unit boundary.
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

/// The bytes as the buffer shows them: invalid bytes as SUB.
fn substituted(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.utf8_chunks() {
        out.push_str(chunk.valid());
        for _ in chunk.invalid() {
            out.push(SUBSTITUTE);
        }
    }
    out
}

#[expect(clippy::unwrap_used, reason = "test helper")]
fn bytes_of(content: &Content) -> Vec<u8> {
    let mut out = Vec::new();
    content.read_into(&mut out).unwrap();
    out
}

/// The document as the oracle sees it.
struct Oracle {
    bytes: Vec<u8>,
    /// Where each byte came from in the original file, if it did.
    origin: Vec<Option<usize>>,
}

impl Oracle {
    /// `K`: the first byte from the original at or past the frontier `f`.
    fn k(&self, f: usize) -> usize {
        self.origin
            .iter()
            .position(|o| o.is_some_and(|x| x >= f))
            .unwrap_or(self.bytes.len())
    }
}

/// Every line start, by the rule in the `mapped` module docs: 0, after
/// each `\n`, and each multiple of `pseudo_line` moved back to a unit
/// boundary, if past `k`, inside the text, and with no `\n` in the `scan`
/// bytes before it nor at it.
fn line_starts(bytes: &[u8], k: usize, limits: Limits) -> Vec<usize> {
    let units = unit_boundaries(bytes);
    let mut out = vec![0];
    out.extend(
        bytes
            .iter()
            .enumerate()
            .filter(|&(_, &c)| c == b'\n')
            .map(|(i, _)| i + 1),
    );
    let mut point = limits.pseudo_line;
    while point < bytes.len() {
        let mut q = point;
        while !units[q] {
            q -= 1;
        }
        let window = &bytes[q.saturating_sub(limits.scan)..=q];
        if q > k && !window.contains(&b'\n') {
            out.push(q);
        }
        point += limits.pseudo_line;
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The line holding `b` among lines starting at `starts`.
fn span_from(bytes: &[u8], starts: &[usize], b: usize) -> LineSpan {
    let len = bytes.len();
    let b = b.min(len);
    let start = starts[starts.partition_point(|&s| s <= b) - 1];
    let next = if b < len {
        starts.get(starts.partition_point(|&s| s <= b)).copied()
    } else {
        None
    };
    let end = match next {
        Some(n) if bytes[n - 1] == b'\n' => {
            if n - 1 > start && bytes[n - 2] == b'\r' {
                n - 2
            } else {
                n - 1
            }
        }
        Some(n) => n,
        None => len,
    };
    LineSpan { start, end, next }
}

/// What a snapshot must keep answering.
struct Then {
    snap: Box<dyn Snapshot>,
    text: String,
    indexed: usize,
    lines: usize,
    numbers: Vec<Option<usize>>,
}

/// Every answer of `t` against the oracle, given the frontier `f` and
/// whether the scanner has been told the file ended.
#[expect(clippy::unwrap_used, reason = "test helper")]
fn check(buf: &MappedBuffer, o: &Oracle, f: usize, limits: Limits) {
    let bytes = &o.bytes;
    let len = bytes.len();
    let k = o.k(f);
    let what = format!("{:?} with K = {k}", String::from_utf8_lossy(bytes));
    assert_eq!(buf.len_bytes(), len, "{what}");
    assert_eq!(buf.indexed_bytes(), k, "indexed_bytes of {what}");
    assert_eq!(
        buf.is_fully_indexed(),
        k == len,
        "is_fully_indexed of {what}"
    );
    assert_eq!(buf.line_index().indexed(), f as u64);

    let newlines: Vec<usize> = (0..len).filter(|&i| bytes[i] == b'\n').collect();
    let before = |b: usize| newlines.partition_point(|&p| p < b);
    let known = before(k);
    assert_eq!(buf.len_lines(), known + 1, "len_lines of {what}");
    for b in 0..=len + 2 {
        let line = before(b.min(k));
        assert_eq!(buf.byte_to_line(b), line, "byte_to_line({b}) of {what}");
        let number = (b <= k || k == len).then_some(line);
        assert_eq!(buf.line_number(b), number, "line_number({b}) of {what}");
    }
    for l in 0..=known + 2 {
        let want = match l {
            0 => 0,
            l if l <= known => newlines[l - 1] + 1,
            _ => len,
        };
        assert_eq!(buf.line_to_byte(l), want, "line_to_byte({l}) of {what}");
    }

    let starts = line_starts(bytes, k, limits);
    let real = line_starts(bytes, len, limits);
    let shown = substituted(bytes);
    for b in 0..=len + 2 {
        let want = span_from(bytes, &starts, b);
        assert_eq!(buf.line_span(b), want, "line_span({b}) of {what}");
        // Lines that end by K are exactly the real lines.
        let line = span_from(bytes, &real, b);
        if line.next.is_some_and(|n| n <= k) || k == len {
            assert_eq!(want, line, "real line at {b} of {what}");
        }
    }
    for l in 0..=known + 1 {
        let want = if l <= known {
            let span = span_from(bytes, &starts, buf.line_to_byte(l));
            &shown[span.start..span.end]
        } else {
            ""
        };
        assert_eq!(buf.line(l), want, "line({l}) of {what}");
    }

    let mut raw = Vec::new();
    buf.read_into(0..usize::MAX, &mut raw).unwrap();
    assert_eq!(&raw, bytes, "read_into of {what}");
    assert_eq!(buf.slice(0..len), shown, "slice of {what}");
}

/// What `snap` answered when taken.
fn remember(buf: &MappedBuffer) -> Then {
    let len = buf.len_bytes();
    Then {
        snap: buf.snapshot(),
        text: buf.slice(0..len).into_owned(),
        indexed: buf.indexed_bytes(),
        lines: buf.len_lines(),
        numbers: (0..=len).map(|b| buf.line_number(b)).collect(),
    }
}

fn check_snapshot(then: &Then) {
    let s = &*then.snap;
    assert_eq!(s.slice(0..usize::MAX), then.text.as_str(), "snapshot text");
    assert_eq!(s.indexed_bytes(), then.indexed, "snapshot indexed_bytes");
    assert_eq!(s.len_lines(), then.lines, "snapshot len_lines");
    let numbers: Vec<_> = (0..then.numbers.len()).map(|b| s.line_number(b)).collect();
    assert_eq!(numbers, then.numbers, "snapshot line numbers");
}

/// One undoable step: the edit and where the removed bytes came from.
struct Step {
    edit: Edit,
    removed_origin: Vec<Option<usize>>,
}

/// Put `content` over `range` in both the buffer and the oracle. Bytes
/// held directly go back as new text (no origin); pieces go back as the
/// very bytes they were, with `origin`.
#[expect(clippy::unwrap_used, reason = "test helper")]
fn put(
    buf: &mut MappedBuffer,
    o: &mut Oracle,
    range: std::ops::Range<usize>,
    content: &Content,
    origin: &[Option<usize>],
) {
    buf.put(range.clone(), content).unwrap();
    let bytes = bytes_of(content);
    let origin = match content {
        Content::Pieces(_) => origin.to_vec(),
        Content::Bytes(_) => vec![None; bytes.len()],
    };
    o.bytes.splice(range.clone(), bytes);
    o.origin.splice(range, origin);
}

#[expect(clippy::unwrap_used, reason = "test helper")]
fn run(src: &[u8], prefix: Index, spacing: Spacing, limits: Limits, ops: &[Op]) {
    // Open with the first `fed` bytes indexed.
    let mut scanner = Scanner::new(spacing);
    let mut fed = prefix.index(src.len() + 1);
    scanner.feed(&src[..fed]);
    let mut index = LineIndex::new();
    index.merge(scanner.take_chunk()).unwrap();
    let source = Arc::new(VecSource::new(src.to_vec()));
    let mut buf = MappedBuffer::with_index(source, index, limits).unwrap();
    let mut f = fed;
    let mut o = Oracle {
        bytes: src.to_vec(),
        origin: (0..src.len()).map(Some).collect(),
    };
    let (mut undo, mut redo): (Vec<Step>, Vec<Step>) = (Vec::new(), Vec::new());
    let mut snapshots = Vec::new();
    check(&buf, &o, f, limits);

    for op in ops {
        let units = unit_boundaries(&o.bytes);
        let bounds: Vec<usize> = (0..units.len()).filter(|&i| units[i]).collect();
        match op {
            Op::Replace { a, b, text } => {
                let (x, y) = (*a.get(&bounds), *b.get(&bounds));
                let range = x.min(y)..x.max(y);
                let removed_origin = o.origin[range.clone()].to_vec();
                let edit = buf.replace(range.clone(), text).unwrap();
                assert_eq!(bytes_of(&edit.removed), &o.bytes[range.clone()]);
                o.bytes.splice(range.clone(), text.bytes());
                o.origin.splice(range, vec![None; text.len()]);
                undo.push(Step {
                    edit,
                    removed_origin,
                });
                redo.clear();
            }
            Op::Index { feed, finish } => {
                let n = (*feed).min(src.len() - fed);
                scanner.feed(&src[fed..fed + n]);
                fed += n;
                if *finish && fed == src.len() {
                    scanner.finish();
                }
                buf.merge_index(scanner.take_chunk()).unwrap();
                f = fed;
            }
            Op::Undo => {
                if let Some(step) = undo.pop() {
                    let e = &step.edit;
                    put(
                        &mut buf,
                        &mut o,
                        e.new_range(),
                        &e.removed,
                        &step.removed_origin,
                    );
                    redo.push(step);
                }
            }
            Op::Redo => {
                if let Some(step) = redo.pop() {
                    let e = &step.edit;
                    put(&mut buf, &mut o, e.old_range(), &e.inserted, &[]);
                    undo.push(step);
                }
            }
            Op::Snapshot => snapshots.push(remember(&buf)),
        }
        check(&buf, &o, f, limits);
        for then in &snapshots {
            check_snapshot(then);
        }
    }

    // Index the rest: every answer is now exact.
    scanner.feed(&src[fed..]);
    scanner.finish();
    buf.merge_index(scanner.take_chunk()).unwrap();
    assert!(buf.line_index().is_done());
    assert!(buf.is_fully_indexed());
    check(&buf, &o, src.len(), limits);
    // Undoing everything gets back to the original, fully indexed.
    while let Some(step) = undo.pop() {
        let e = &step.edit;
        put(
            &mut buf,
            &mut o,
            e.new_range(),
            &e.removed,
            &step.removed_origin,
        );
        assert!(buf.is_fully_indexed());
    }
    check(&buf, &o, src.len(), limits);
    assert_eq!(o.bytes, src);
    for then in &snapshots {
        check_snapshot(then);
    }
}

proptest! {
    #![proptest_config(config(512))]

    /// Edits, undo and redo interleaved with index chunks agree with the
    /// oracle at every step, and exactly once indexing is done.
    #[test]
    fn partial_index_matches_oracle(
        src in original(),
        prefix in any::<Index>(),
        spacing in spacing(),
        limits in limits(),
        ops in prop::collection::vec(op(), 0..32),
    ) {
        run(&src, prefix, spacing, limits, &ops);
    }
}

/// An unindexed file of one long line and a short one: the long line comes
/// as pseudo-lines until indexing reaches it, then as one line.
#[test]
fn pseudo_lines_join_when_indexed() {
    let src = [&[b'x'; 40][..], b"\nend"].concat();
    let limits = Limits {
        copy_limit: 4,
        segment: 16,
        max_piece: 6,
        scan: 4,
        pseudo_line: 16,
    };
    let source = Arc::new(VecSource::new(src.clone()));
    let mut buf = MappedBuffer::with_index(source, LineIndex::new(), limits).unwrap();
    let o = Oracle {
        bytes: src.clone(),
        origin: (0..src.len()).map(Some).collect(),
    };
    check(&buf, &o, 0, limits);
    let starts: Vec<usize> = (0..=src.len()).map(|b| buf.line_span(b).start).collect();
    assert!(starts.contains(&16) && starts.contains(&32), "{starts:?}");
    let mut scanner = Scanner::new(Spacing::new(1, 8));
    scanner.feed(&src);
    buf.merge_index(scanner.take_chunk()).unwrap();
    check(&buf, &o, src.len(), limits);
    assert_eq!(buf.line_span(20).start, 0);
    assert_eq!(buf.line(0).len(), 40);
}
