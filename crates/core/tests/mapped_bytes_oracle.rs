//! Property tests: a [`MappedBuffer`] holding bytes that are not UTF-8
//! (M7 task 8) must agree with a plain `Vec<u8>` oracle.
//!
//! A *unit* of text is a valid UTF-8 character or one invalid byte, split
//! the way `<[u8]>::utf8_chunks` splits them: every byte of an invalid
//! sequence is a unit of its own. The oracle works the units out from
//! scratch after every step, and the buffer must agree on:
//!
//! - which offsets are unit boundaries (`is_char_boundary`);
//! - the text it shows (`slice`, `slice_into`, `line`): the raw bytes with
//!   each invalid byte replaced by SUB, so lengths and offsets are the raw
//!   bytes';
//! - the raw bytes themselves (`read_into`);
//! - where lines are (`line_span`, `line_to_byte`);
//! - and cursor movement: `next_grapheme`, `prev_grapheme` and `clamp`
//!   only ever return unit boundaries, and walking right and walking left
//!   stop at the same places.
//!
//! Edits are made the way the editor makes them: `replace` at unit
//! boundaries with valid text, and undo and redo through [`History`], which
//! puts bytes back with `put` wherever they came from, even where a deletion
//! has since joined the bytes around them into one character (`E2 41 82
//! AC` minus the `41` is `€`). Some steps also `put` raw invalid bytes, as
//! undo of such a deletion does.

use std::ops::Range;
use std::sync::Arc;

use nimbus_core::line_index::{LineIndex, Spacing};
use nimbus_core::movement::{clamp, next_grapheme, prev_grapheme};
use nimbus_core::{
    Buffer, Content, Edit, EditError, History, Limits, LineSpan, MappedBuffer, SUBSTITUTE,
    Selections, Text, VecSource,
};
use proptest::prelude::*;
use proptest::sample::Index;
use proptest::test_runner::Config;

/// Building blocks for the original bytes and for raw insertions.
const PIECES: &[&[u8]] = &[
    b"a",
    b"Zq",
    b"\n",
    b"\r\n",
    b"\r",
    b"\t",
    "é".as_bytes(),
    "日本".as_bytes(),
    "€".as_bytes(),
    "😀".as_bytes(),
    "e\u{301}".as_bytes(),
    // Never valid.
    b"\xff",
    // `€` cut short.
    b"\xe2\x82",
    // `€` and `😀` in halves, which a deletion between them joins.
    b"\xe2",
    b"\x82\xac",
    b"\xf0\x9f",
    b"\x98\x80",
    // A surrogate, an overlong `/`, and a code point past U+10FFFF.
    b"\xed\xa0\x80",
    b"\xc0\xaf",
    b"\xf4\x90\x80\x80",
    // A lone continuation byte.
    b"\x80",
    // Characters split by one unit: deleting it joins them.
    b"\xe2A\x82\xac",
    b"\xf0\x9f\n\x98\x80",
    b"\xc3\xff\xa9",
];

/// Valid text typed or pasted with `replace`.
const TYPED: &[&str] = &["x", "é", "日", "€", "😀", "\n", "\r\n", "ab", "\u{301}", ""];

/// Limits that make every path run on small texts.
const TINY: Limits = Limits {
    copy_limit: 4,
    segment: 16,
    max_piece: 6,
    scan: 3,
    pseudo_line: 8,
};

fn config(cases: u32) -> Config {
    Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    }
}

fn bytes(max_pieces: usize) -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(prop::sample::select(PIECES), 0..max_pieces).prop_map(|v| v.concat())
}

/// One step of an editing session. Positions are `Index`es, resolved
/// against the oracle's unit boundaries when the step runs.
#[derive(Debug, Clone)]
enum Op {
    Replace {
        a: Index,
        b: Index,
        text: &'static str,
    },
    Delete {
        a: Index,
        b: Index,
    },
    /// Delete one unit, which is what most often joins the bytes either
    /// side of it into a character.
    DeleteUnit {
        at: Index,
    },
    /// Put raw bytes, valid or not, at a unit boundary, as undo does.
    PutRaw {
        at: Index,
        bytes: Vec<u8>,
    },
    /// Try to edit at an offset inside a character: refused.
    Inside {
        at: Index,
    },
    Undo,
    Redo,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (any::<Index>(), any::<Index>(), prop::sample::select(TYPED))
            .prop_map(|(a, b, text)| Op::Replace { a, b, text }),
        2 => (any::<Index>(), any::<Index>()).prop_map(|(a, b)| Op::Delete { a, b }),
        4 => any::<Index>().prop_map(|at| Op::DeleteUnit { at }),
        2 => (any::<Index>(), bytes(3)).prop_map(|(at, bytes)| Op::PutRaw { at, bytes }),
        1 => any::<Index>().prop_map(|at| Op::Inside { at }),
        3 => Just(Op::Undo),
        2 => Just(Op::Redo),
    ]
}

// ---------------------------------------------------------------------------
// The oracle: the slow, obvious answers.

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

/// The bytes as the buffer shows them: each unit that is not a valid
/// character (one invalid byte) becomes SUB.
fn substituted(bytes: &[u8], units: &[bool]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let mut end = at + 1;
        while !units[end] {
            end += 1;
        }
        match std::str::from_utf8(&bytes[at..end]) {
            Ok(s) => out.push_str(s),
            Err(_) => out.push(SUBSTITUTE),
        }
        at = end;
    }
    out
}

/// `range` clamped to the text and widened outward to unit boundaries.
fn widen(units: &[bool], range: &Range<usize>) -> Range<usize> {
    let len = units.len() - 1;
    let mut start = range.start.min(len);
    let mut end = range.end.min(len).max(start);
    while !units[start] {
        start -= 1;
    }
    while !units[end] {
        end += 1;
    }
    start..end
}

#[expect(clippy::unwrap_used, reason = "test helper")]
fn bytes_of(content: &Content) -> Vec<u8> {
    let mut out = Vec::new();
    content.read_into(&mut out).unwrap();
    out
}

// ---------------------------------------------------------------------------
// Checks.

/// Lines, from every byte of each, against lines split at `\n`.
fn check_lines(t: &dyn Text, bytes: &[u8], shown: &str) {
    let len = bytes.len();
    let (mut start, mut line) = (0, 0);
    loop {
        let lf = bytes[start..]
            .iter()
            .position(|&c| c == b'\n')
            .map(|i| start + i);
        let next = lf.map(|i| i + 1);
        let end = match lf {
            Some(i) if i > start && bytes[i - 1] == b'\r' => i - 1,
            Some(i) => i,
            None => len,
        };
        let want = LineSpan { start, end, next };
        for b in start..next.unwrap_or(len + 2) {
            assert_eq!(t.line_span(b), want, "line_span({b}) of {bytes:x?}");
        }
        assert_eq!(
            t.line(line),
            &shown[start..end],
            "line({line}) of {bytes:x?}"
        );
        assert_eq!(t.line_to_byte(line), start, "line_to_byte({line})");
        line += 1;
        match next {
            Some(n) => start = n,
            None => break,
        }
    }
    assert_eq!(t.len_lines(), line, "len_lines of {bytes:x?}");
}

/// Movement only stops on unit boundaries, and the same stops either way.
fn check_movement(t: &dyn Text, bytes: &[u8], units: &[bool]) {
    let len = bytes.len();
    let mut right = vec![0];
    let mut at = 0;
    while at < len {
        let next = next_grapheme(t, at);
        assert!(next > at, "next_grapheme({at}) = {next} in {bytes:x?}");
        assert!(
            units[next],
            "next_grapheme({at}) = {next}, inside a character"
        );
        right.push(next);
        at = next;
    }
    let mut left = vec![len];
    while at > 0 {
        let prev = prev_grapheme(t, at);
        assert!(prev < at, "prev_grapheme({at}) = {prev} in {bytes:x?}");
        assert!(
            units[prev],
            "prev_grapheme({at}) = {prev}, inside a character"
        );
        left.push(prev);
        at = prev;
    }
    left.reverse();
    assert_eq!(left, right, "stops walking left and right in {bytes:x?}");
    // From any unit boundary, even one that is not a cursor stop (inside a
    // "\r\n" or a grapheme cluster), and from any offset for `clamp`.
    for b in 0..=len + 1 {
        let c = clamp(t, b);
        assert!(units[c] && c <= b, "clamp({b}) = {c} in {bytes:x?}");
        if b <= len && units[b] {
            let n = next_grapheme(t, b);
            assert!(units[n] && (n > b || b == len), "next_grapheme({b}) = {n}");
            let p = prev_grapheme(t, b);
            assert!(units[p] && (p < b || b == 0), "prev_grapheme({b}) = {p}");
        }
    }
}

/// Every read-only answer of `t` against the oracle bytes.
#[expect(clippy::unwrap_used, reason = "test helper")]
fn check(t: &dyn Text, bytes: &[u8]) {
    let len = bytes.len();
    let units = unit_boundaries(bytes);
    let shown = substituted(bytes, &units);
    assert_eq!(shown.len(), len, "substitution keeps the length");
    assert_eq!(t.len_bytes(), len);
    for b in 0..=len + 2 {
        let want = units.get(b) == Some(&true);
        assert_eq!(
            t.is_char_boundary(b),
            want,
            "is_char_boundary({b}) of {bytes:x?}"
        );
    }
    let mut raw = Vec::new();
    t.read_into(0..usize::MAX, &mut raw).unwrap();
    assert_eq!(raw, bytes, "read_into the whole text");
    assert_eq!(t.slice(0..len), shown, "slice of the whole text");

    let step = if len <= 40 { 1 } else { 3 };
    let mut into = String::new();
    for s in (0..=len + 1).step_by(step) {
        for e in (s.saturating_sub(2)..=len + 1).step_by(step) {
            let w = widen(&units, &(s..e));
            assert_eq!(
                t.slice(s..e),
                &shown[w.clone()],
                "slice({s}..{e}) of {bytes:x?}"
            );
            into.clear();
            into.push('>');
            t.slice_into(s..e, &mut into);
            assert_eq!(&into[1..], &shown[w], "slice_into({s}..{e}) of {bytes:x?}");
            raw.clear();
            t.read_into(s..e, &mut raw).unwrap();
            let z = e.min(len);
            assert_eq!(raw, &bytes[s.min(z)..z], "read_into({s}..{e})");
        }
    }
    check_lines(t, bytes, &shown);
    check_movement(t, bytes, &units);
}

/// A callback for undo and redo that applies each edit they report to
/// `oracle`.
fn replay(oracle: &mut Vec<u8>) -> impl FnMut(&dyn Text, &Edit) + '_ {
    move |after, edit| {
        oracle.splice(edit.old_range(), bytes_of(&edit.inserted));
        assert_eq!(after.len_bytes(), oracle.len(), "reported edit");
    }
}

#[expect(clippy::unwrap_used, reason = "test helper")]
fn build(initial: &[u8]) -> MappedBuffer {
    let index = LineIndex::build(initial, Spacing::new(2, 5));
    MappedBuffer::with_index(Arc::new(VecSource::new(initial.to_vec())), index, TINY).unwrap()
}

/// Run `ops` from `initial`, checking everything after every step; then
/// undo everything (back to `initial`) and redo everything.
#[expect(clippy::unwrap_used, reason = "test helper")]
fn run(initial: &[u8], ops: &[Op]) {
    let mut buf = build(initial);
    let mut oracle = initial.to_vec();
    let mut hist = History::new();
    let sel = Selections::default();
    check(&buf, &oracle);
    for op in ops {
        let units = unit_boundaries(&oracle);
        let bounds: Vec<usize> = (0..units.len()).filter(|&i| units[i]).collect();
        let span = |a: &Index, b: &Index| {
            let (x, y) = (*a.get(&bounds), *b.get(&bounds));
            x.min(y)..x.max(y)
        };
        match op {
            Op::Replace { a, b, text } => {
                let range = span(a, b);
                let e = buf.replace(range.clone(), text).unwrap();
                assert_eq!(bytes_of(&e.removed), &oracle[range.clone()]);
                oracle.splice(range, text.bytes());
                hist.record_group(vec![e], &sel, &sel);
            }
            Op::Delete { a, b } => {
                let range = span(a, b);
                let e = buf.replace(range.clone(), "").unwrap();
                oracle.splice(range, []);
                hist.record_group(vec![e], &sel, &sel);
            }
            Op::DeleteUnit { at } => {
                // The unit starting at a boundary, as Delete removes it.
                if let Some(i) = bounds.len().checked_sub(1).filter(|&n| n > 0) {
                    let k = at.index(i);
                    let range = bounds[k]..bounds[k + 1];
                    let e = buf.replace(range.clone(), "").unwrap();
                    oracle.splice(range, []);
                    hist.record_group(vec![e], &sel, &sel);
                }
            }
            Op::PutRaw { at, bytes } => {
                let p = *at.get(&bounds);
                let e = buf.put(p..p, &Content::Bytes(bytes.clone())).unwrap();
                oracle.splice(p..p, bytes.iter().copied());
                hist.record_group(vec![e], &sel, &sel);
            }
            Op::Inside { at } => {
                let inside: Vec<usize> = (0..oracle.len()).filter(|&i| !units[i]).collect();
                if !inside.is_empty() {
                    let x = *at.get(&inside);
                    let want = Err(EditError::NotCharBoundary { offset: x });
                    assert_eq!(buf.replace(x..x, "y"), want);
                    assert_eq!(buf.replace(0..x, ""), want);
                }
            }
            Op::Undo => {
                hist.undo(&mut buf, &mut replay(&mut oracle)).unwrap();
            }
            Op::Redo => {
                hist.redo(&mut buf, &mut replay(&mut oracle)).unwrap();
            }
        }
        check(&buf, &oracle);
    }
    // To the newest state, back to the start, and forward again.
    while hist
        .redo(&mut buf, &mut replay(&mut oracle))
        .unwrap()
        .is_some()
    {}
    check(&buf, &oracle);
    let newest = oracle.clone();
    while hist
        .undo(&mut buf, &mut replay(&mut oracle))
        .unwrap()
        .is_some()
    {}
    assert_eq!(oracle, initial, "undoing everything restores the start");
    check(&buf, initial);
    while hist
        .redo(&mut buf, &mut replay(&mut oracle))
        .unwrap()
        .is_some()
    {}
    assert_eq!(
        oracle, newest,
        "redoing everything restores the newest state"
    );
    check(&buf, &newest);
}

proptest! {
    #![proptest_config(config(512))]

    /// Random sessions of edits, raw puts, undo and redo over text full of
    /// invalid and half characters agree with the byte oracle throughout.
    #[test]
    fn invalid_bytes_match_oracle(
        initial in bytes(24),
        ops in prop::collection::vec(op(), 0..24),
    ) {
        run(&initial, &ops);
    }
}

/// The case the design calls out: deleting the `A` from `E2 41 82 AC`
/// joins three invalid bytes into `€`, so the offset where the `A` was is
/// no longer a unit boundary; undo must still put the `A` back there, and
/// redo take it out again. Checked with the removal copied (short) and kept
/// as pieces (longer than the copy limit).
#[test]
fn deleting_joins_invalid_bytes_and_undo_splits_them() {
    for (initial, removed) in [
        (&b"x\xe2A\x82\xac\ny"[..], 2..3),
        (&b"x\xe2ABCDEF\x82\xac\ny"[..], 2..8),
    ] {
        let mut buf = build(initial);
        let mut oracle = initial.to_vec();
        let mut hist = History::new();
        let sel = Selections::default();
        let e = buf.replace(removed.clone(), "").unwrap();
        assert_eq!(
            matches!(e.removed, Content::Pieces(_)),
            removed.len() > TINY.copy_limit
        );
        hist.record_group(vec![e], &sel, &sel);
        oracle.splice(removed, []);
        check(&buf, &oracle);
        assert_eq!(buf.line(0), "x€", "three invalid bytes became a character");
        assert!(!buf.is_char_boundary(2) && !buf.is_char_boundary(3));
        assert_eq!(
            buf.replace(2..2, "y"),
            Err(EditError::NotCharBoundary { offset: 2 })
        );
        assert_eq!(next_grapheme(&buf, 1), 4);
        assert_eq!(prev_grapheme(&buf, 4), 1);

        // Undo puts the removed bytes back at offset 2, inside the `€`.
        hist.undo(&mut buf, &mut replay(&mut oracle)).unwrap();
        assert_eq!(oracle, initial);
        check(&buf, initial);
        assert!(buf.line(0).starts_with("x\u{1a}A"));
        hist.redo(&mut buf, &mut replay(&mut oracle)).unwrap();
        check(&buf, &oracle);
        assert_eq!(buf.line(0), "x€");
    }
}
