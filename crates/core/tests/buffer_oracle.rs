//! Property tests: random edits on every buffer backend must agree with a
//! plain `String` oracle (DESIGN.md §6, M2 task 1; M7 task 7).
//!
//! Each property runs on [`RopeBuffer`] and on [`MappedBuffer`] (a piece
//! table over the initial text), and one also through [`AnyBuffer`]. The
//! mapped buffer gets tiny [`Limits`], so the paths that normally need
//! megabytes run on a few bytes: removals over 4 bytes are recorded as
//! pieces, add-buffer segments seal every 16 bytes, and inserted text is cut
//! into pieces of at most 6 bytes.
//!
//! The oracle is the obvious, slow implementation: a `String` edited with
//! `replace_range`, with lines found by splitting on `'\n'`. Every answer the
//! buffer gives (text, lengths, lines, offset conversions, edit records,
//! errors, slices, snapshots) is compared against it after every edit, and
//! the storage chunks `chunk_at` hands out must be consistent with it.
//!
//! The generated text deliberately mixes the cases that break naive code:
//! `\r\n` (whose `\r` is part of the line ending), a lone `\r` (ordinary
//! text), multi-byte characters (so most byte offsets are *not* valid
//! positions), an emoji ZWJ sequence, a combining accent, tabs, and empty
//! pieces. Edits splice at arbitrary char boundaries, so they also create
//! and split `\r\n` pairs.

use std::ops::Range;
use std::sync::Arc;

use nimbus_core::{
    AnyBuffer, Buffer, Content, Edit, EditError, Limits, MappedBuffer, RopeBuffer, Text, VecSource,
};
use proptest::prelude::*;
use proptest::sample::Index;
use proptest::test_runner::Config;

/// Building blocks for generated text. Line endings appear twice to make
/// multi-line texts common.
const PIECES: &[&str] = &[
    "a",
    "Zq",
    "hello ",
    "fn x() {}",
    "\n",
    "\n",
    "\r\n",
    "\r\n",
    "\r",
    "é",
    "日本",
    "👩\u{200d}💻",
    "e\u{301}",
    "\t",
    "",
];

/// Limits for the mapped backend that make every path run on small texts.
const TINY: Limits = Limits {
    copy_limit: 4,
    segment: 16,
    max_piece: 6,
};

/// A buffer implementation under test.
trait Backend: Buffer + Sized {
    /// A buffer holding `text`.
    fn build(text: &str) -> Self;

    /// Removals longer than this are recorded as [`Content::Pieces`];
    /// `None` if they never are.
    fn copy_limit() -> Option<usize> {
        None
    }

    /// Check invariants the backend keeps beyond what `Text` shows.
    fn check(&self) {}
}

impl Backend for RopeBuffer {
    fn build(text: &str) -> Self {
        RopeBuffer::from_text(text)
    }
}

impl Backend for MappedBuffer {
    fn build(text: &str) -> Self {
        MappedBuffer::with_limits(Arc::new(VecSource::from(text)), TINY)
    }

    fn copy_limit() -> Option<usize> {
        Some(TINY.copy_limit)
    }

    /// Pieces of the original text stay in file order, without overlaps,
    /// so saving can stream them in one pass.
    fn check(&self) {
        let mut prev_end = 0;
        for r in self.original_ranges() {
            assert!(
                prev_end <= r.start && r.start < r.end,
                "original pieces out of order or empty: {:?}",
                self.original_ranges().collect::<Vec<_>>()
            );
            prev_end = r.end;
        }
    }
}

impl Backend for AnyBuffer {
    fn build(text: &str) -> Self {
        AnyBuffer::Mapped(MappedBuffer::build(text))
    }

    fn copy_limit() -> Option<usize> {
        MappedBuffer::copy_limit()
    }

    fn check(&self) {
        if let Some(m) = self.as_mapped() {
            m.check();
        }
    }
}

/// The bytes of `content`, read from the buffer's storage for pieces.
#[expect(clippy::unwrap_used, reason = "test helper")]
fn bytes_of(content: &Content) -> Vec<u8> {
    let mut out = Vec::new();
    content.read_into(&mut out).unwrap();
    out
}

/// `got` must be `Ok` and record the same change as `want`, with the
/// removed text held as pieces exactly when it is over the copy limit.
#[expect(clippy::unwrap_used, reason = "test helper")]
fn same_edit<B: Backend>(got: &Result<Edit, EditError>, want: &Edit, what: &str) {
    assert!(got.is_ok(), "{what}: {got:?}");
    let got = got.as_ref().unwrap();
    assert_eq!(got.start, want.start, "{what}: start");
    assert_eq!(got.inserted, want.inserted, "{what}: inserted");
    assert_eq!(
        bytes_of(&got.removed),
        bytes_of(&want.removed),
        "{what}: removed"
    );
    assert_eq!(
        got.removed.len(),
        want.removed.len(),
        "{what}: removed length"
    );
    assert_eq!(
        got.removed.has_newline(),
        want.removed.has_newline(),
        "{what}: removed line breaks"
    );
    let over = B::copy_limit().is_some_and(|limit| want.removed.len() > limit);
    assert_eq!(
        matches!(got.removed, Content::Pieces(_)),
        over,
        "{what}: removal of {} bytes held as pieces",
        want.removed.len()
    );
}

/// `cases` runs per property. Failures are not persisted to a regressions
/// file; proptest prints the minimal failing input instead.
fn config(cases: u32) -> Config {
    Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    }
}

fn piece() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => prop::sample::select(PIECES).prop_map(str::to_owned),
        1 => "[ -~]{0,8}",
    ]
}

fn text(max_pieces: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(piece(), 0..max_pieces).prop_map(|v| v.concat())
}

/// Text long enough to span several ropey chunks (about 1 KB each), so the
/// non-contiguous paths (`Cow::Owned` lines and slices) get exercised.
fn long_text() -> impl Strategy<Value = String> {
    (text(24), 10usize..60).prop_map(|(t, n)| t.repeat(n))
}

/// Which way an invalid edit is wrong.
#[derive(Debug, Clone, Copy)]
enum Bad {
    EndPastEnd,
    StartPastEnd,
    Reversed,
    StartInsideChar,
    EndInsideChar,
    EmptyInsideChar,
}

/// One step of an edit sequence. Positions are `Index`es, resolved against
/// the oracle's current char boundaries when the step runs.
#[derive(Debug, Clone)]
enum Op {
    Insert {
        at: Index,
        text: String,
    },
    Delete {
        a: Index,
        b: Index,
    },
    Replace {
        a: Index,
        b: Index,
        text: String,
    },
    Invalid {
        bad: Bad,
        a: Index,
        b: Index,
        past: usize,
        text: String,
    },
    Snapshot,
}

fn bad() -> impl Strategy<Value = Bad> {
    prop_oneof![
        Just(Bad::EndPastEnd),
        Just(Bad::StartPastEnd),
        Just(Bad::Reversed),
        Just(Bad::StartInsideChar),
        Just(Bad::EndInsideChar),
        Just(Bad::EmptyInsideChar),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (any::<Index>(), text(6)).prop_map(|(at, text)| Op::Insert { at, text }),
        3 => (any::<Index>(), any::<Index>()).prop_map(|(a, b)| Op::Delete { a, b }),
        3 => (any::<Index>(), any::<Index>(), text(6))
            .prop_map(|(a, b, text)| Op::Replace { a, b, text }),
        2 => (bad(), any::<Index>(), any::<Index>(), 0usize..4, text(3))
            .prop_map(|(bad, a, b, past, text)| Op::Invalid { bad, a, b, past, text }),
        1 => Just(Op::Snapshot),
    ]
}

/// A slice endpoint: anywhere in the text (aligned or not), just past the
/// end, or absurdly large.
#[derive(Debug, Clone)]
enum Pos {
    Within(Index),
    PastEnd(usize),
    Huge(usize),
}

impl Pos {
    fn resolve(&self, len: usize) -> usize {
        match self {
            Pos::Within(i) => i.index(len + 1),
            Pos::PastEnd(k) => len + 1 + k,
            Pos::Huge(k) => usize::MAX - k,
        }
    }
}

fn pos() -> impl Strategy<Value = Pos> {
    prop_oneof![
        8 => any::<Index>().prop_map(Pos::Within),
        1 => (0usize..8).prop_map(Pos::PastEnd),
        1 => (0usize..2).prop_map(Pos::Huge),
    ]
}

// ---------------------------------------------------------------------------
// Oracle helpers: the slow, obvious answers.

/// Every valid cursor position, including the end.
fn boundaries(s: &str) -> Vec<usize> {
    s.char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(s.len()))
        .collect()
}

/// Byte offsets that fall inside a multi-byte character.
fn interior_bytes(s: &str) -> Vec<usize> {
    (0..s.len()).filter(|&i| !s.is_char_boundary(i)).collect()
}

/// Lines split at `\n`; a `\r` right before a `\n` belongs to the ending.
/// The last piece has no `\n` after it, so a trailing lone `\r` stays.
fn oracle_lines(s: &str) -> Vec<&str> {
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

fn line_starts(s: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(s.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

/// `range` clamped to `s` and widened outward to char boundaries. A reversed
/// range is treated as empty at its (clamped) start.
fn oracle_slice(s: &str, range: &Range<usize>) -> String {
    let len = s.len();
    let mut start = range.start.min(len);
    let mut end = range.end.min(len).max(start);
    while !s.is_char_boundary(start) {
        start -= 1;
    }
    while !s.is_char_boundary(end) {
        end += 1;
    }
    s[start..end].to_owned()
}

// ---------------------------------------------------------------------------
// Checks.

/// `chunk_at` hands out the buffer's storage as it is, so there is no
/// oracle answer for where chunks split; instead every answer must be
/// consistent with the oracle text. Checked at every byte offset (aligned
/// or not) up to a few past the end, plus `usize::MAX`: a chunk is a piece
/// of the text at the start it reports and contains the offset asked for;
/// at or past the end the answer is `("", len)`. Walking from 0, asking for
/// the byte just past each chunk, must visit contiguous non-empty chunks
/// that rebuild the whole text, which is how the parser streams it.
fn check_chunks<T: Text + ?Sized>(t: &T, oracle: &str) {
    let len = oracle.len();
    for b in (0..=len + 5).chain([usize::MAX]) {
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

/// `line_span`, `byte_at` and `read_into` against the oracle: every line's
/// span from every byte in it, and raw reads of sample ranges.
#[expect(clippy::unwrap_used, reason = "test helper")]
fn check_spans<T: Text + ?Sized>(t: &T, oracle: &str) {
    let len = oracle.len();
    let bytes = oracle.as_bytes();
    let mut start = 0;
    loop {
        let lf = oracle[start..].find('\n').map(|i| start + i);
        let next = lf.map(|i| i + 1);
        let end = match lf {
            Some(i) if i > start && bytes[i - 1] == b'\r' => i - 1,
            Some(i) => i,
            None => len,
        };
        let want = nimbus_core::LineSpan { start, end, next };
        for b in start..next.unwrap_or(len + 1) {
            assert_eq!(t.line_span(b), want, "line_span({b}) of {oracle:?}");
        }
        match next {
            Some(n) => start = n,
            None => break,
        }
    }
    for b in 0..len + 3 {
        assert_eq!(t.byte_at(b), bytes.get(b).copied(), "byte_at({b})");
    }
    for (a, z) in [(0, len), (0, 0), (len / 3, len / 2), (len, len + 4)] {
        let mut out = Vec::new();
        t.read_into(a..z, &mut out).unwrap();
        let z = z.min(len);
        assert_eq!(out, &bytes[a.min(z)..z], "read_into({a}..{z})");
    }
}

/// Compare every read-only answer of `t` with the oracle.
fn check_text<T: Text + ?Sized>(t: &T, oracle: &str) {
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
        // The line's text sits at its start offset.
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

    // Every byte offset, including unaligned ones and a few past the end.
    let mut line = 0;
    for b in 0..=len + 3 {
        assert_eq!(t.byte_to_line(b), line, "byte_to_line({b}) of {oracle:?}");
        let boundary = oracle.is_char_boundary(b);
        assert_eq!(
            t.is_char_boundary(b),
            boundary,
            "is_char_boundary({b}) of {oracle:?}"
        );
        if boundary {
            // Round trip: b lies within the line byte_to_line names.
            let l = t.byte_to_line(b);
            assert!(t.line_to_byte(l) <= b, "line_to_byte(byte_to_line({b}))");
            if l + 1 < n {
                assert!(
                    b < t.line_to_byte(l + 1),
                    "{b} before start of line {}",
                    l + 1
                );
            }
        }
        if oracle.as_bytes().get(b) == Some(&b'\n') {
            line += 1;
        }
    }
    assert_eq!(
        t.byte_to_line(usize::MAX),
        n - 1,
        "byte_to_line(usize::MAX)"
    );
}

/// Apply a valid edit to both and check the returned record.
fn apply<B: Backend>(buf: &mut B, oracle: &mut String, range: Range<usize>, text: &str) {
    let edit = Edit {
        start: range.start,
        removed: oracle[range.clone()].into(),
        inserted: text.into(),
    };
    same_edit::<B>(
        &buf.replace(range.clone(), text),
        &edit,
        &format!("replace({range:?}, {text:?}) on {oracle:?}"),
    );
    buf.check();
    oracle.replace_range(range.clone(), text);
    assert_eq!(edit.old_range(), range, "old_range");
    assert_eq!(
        edit.new_range(),
        range.start..range.start + text.len(),
        "new_range"
    );
    assert_eq!(
        &oracle[edit.new_range()],
        text,
        "new_range covers the insert"
    );
}

/// Attempt an invalid edit; it must fail with `expected` and change nothing.
fn reject<B: Backend>(
    buf: &mut B,
    oracle: &str,
    range: Range<usize>,
    text: &str,
    expected: EditError,
) {
    let got = buf.replace(range.clone(), text);
    assert_eq!(
        got,
        Err(expected),
        "replace({range:?}, {text:?}) on {oracle:?}"
    );
    check_text(buf, oracle);
}

/// Build an invalid range of kind `bad` for `oracle`, with exactly one thing wrong
/// so the expected error is unambiguous. `None` if it has no such range
/// (for example no multi-byte characters).
fn invalid_range(
    oracle: &str,
    bad: Bad,
    lo: Index,
    hi: Index,
    past: usize,
) -> Option<(Range<usize>, EditError)> {
    let len = oracle.len();
    let bounds = boundaries(oracle);
    let interior = interior_bytes(oracle);
    let oob = |range: Range<usize>| {
        let err = EditError::OutOfBounds {
            range: range.clone(),
            len,
        };
        Some((range, err))
    };
    let inside = matches!(
        bad,
        Bad::StartInsideChar | Bad::EndInsideChar | Bad::EmptyInsideChar
    );
    if inside && interior.is_empty() {
        return None;
    }
    match bad {
        Bad::EndPastEnd => oob(*lo.get(&bounds)..len + 1 + past),
        Bad::StartPastEnd => oob(len + 1 + past..len + 1 + past + hi.index(3)),
        Bad::Reversed => {
            if bounds.len() < 2 {
                return None;
            }
            let first = lo.index(bounds.len() - 1);
            let second = first + 1 + hi.index(bounds.len() - 1 - first);
            oob(bounds[second]..bounds[first])
        }
        Bad::StartInsideChar => {
            let at = *lo.get(&interior);
            let after: Vec<usize> = bounds.iter().copied().filter(|&p| p > at).collect();
            Some((
                at..*hi.get(&after),
                EditError::NotCharBoundary { offset: at },
            ))
        }
        Bad::EndInsideChar => {
            let at = *lo.get(&interior);
            let before: Vec<usize> = bounds.iter().copied().filter(|&p| p < at).collect();
            Some((
                *hi.get(&before)..at,
                EditError::NotCharBoundary { offset: at },
            ))
        }
        Bad::EmptyInsideChar => {
            let at = *lo.get(&interior);
            Some((at..at, EditError::NotCharBoundary { offset: at }))
        }
    }
}

/// Run `ops` against a buffer starting from `initial`, checking everything
/// after every step, and every live snapshot too.
fn run_sequence<B: Backend>(initial: &str, ops: &[Op]) {
    let mut buf = B::build(initial);
    let mut oracle = initial.to_owned();
    let mut snapshots = Vec::new();
    check_text(&buf, &oracle);

    for op in ops {
        let bounds = boundaries(&oracle);
        let span = |a: &Index, b: &Index| {
            let (x, y) = (*a.get(&bounds), *b.get(&bounds));
            x.min(y)..x.max(y)
        };
        match op {
            Op::Insert { at, text } => {
                let p = *at.get(&bounds);
                apply(&mut buf, &mut oracle, p..p, text);
            }
            Op::Delete { a, b } => apply(&mut buf, &mut oracle, span(a, b), ""),
            Op::Replace { a, b, text } => apply(&mut buf, &mut oracle, span(a, b), text),
            Op::Invalid {
                bad,
                a,
                b,
                past,
                text,
            } => {
                if let Some((range, err)) = invalid_range(&oracle, *bad, *a, *b, *past) {
                    reject(&mut buf, &oracle, range, text, err);
                }
            }
            Op::Snapshot => snapshots.push((buf.snapshot(), oracle.clone())),
        }
        check_text(&buf, &oracle);
        for (snap, then) in &snapshots {
            assert_eq!(snap.len_bytes(), then.len(), "snapshot length");
            assert_eq!(snap.slice(0..usize::MAX), then.as_str(), "snapshot text");
        }
    }
    for (snap, then) in &snapshots {
        check_text(&**snap, then);
    }
}

/// Every offset inside a multi-byte character, and ranges past the end or
/// reversed, are refused with the right error and change nothing.
fn invalid_edits<B: Backend>(initial: &str, past: usize) {
    let mut buf = B::build(initial);
    let len = initial.len();
    for x in interior_bytes(initial) {
        let nb = EditError::NotCharBoundary { offset: x };
        reject(&mut buf, initial, x..x, "x", nb.clone());
        reject(&mut buf, initial, x..len, "", nb.clone());
        reject(&mut buf, initial, 0..x, "\r\n", nb);
    }
    for range in [
        0..len + 1 + past,
        len + 1 + past..len + 1 + past,
        len..len + 1,
        usize::MAX..usize::MAX,
    ] {
        let err = EditError::OutOfBounds {
            range: range.clone(),
            len,
        };
        reject(&mut buf, initial, range, "x", err);
    }
    let bounds = boundaries(initial);
    if let [first, .., last] = bounds[..] {
        let range = last..first;
        let err = EditError::OutOfBounds {
            range: range.clone(),
            len,
        };
        reject(&mut buf, initial, range, "", err);
    }
}

/// `slice` and `slice_into` never panic, and return the oracle's text
/// clamped to the end and widened outward to char boundaries.
fn slices<B: Backend>(initial: &str, ops: &[Op], ranges: &[(Pos, Pos)]) {
    let mut buf = B::build(initial);
    let mut oracle = initial.to_owned();
    for op in ops {
        let bounds = boundaries(&oracle);
        let span = |a: &Index, b: &Index| {
            let (x, y) = (*a.get(&bounds), *b.get(&bounds));
            x.min(y)..x.max(y)
        };
        match op {
            Op::Insert { at, text } => {
                let p = *at.get(&bounds);
                apply(&mut buf, &mut oracle, p..p, text);
            }
            Op::Delete { a, b } => apply(&mut buf, &mut oracle, span(a, b), ""),
            Op::Replace { a, b, text } => apply(&mut buf, &mut oracle, span(a, b), text),
            Op::Invalid { .. } | Op::Snapshot => {}
        }
    }
    check_chunks(&buf, &oracle);
    let len = oracle.len();
    let mut into = String::new();
    for (a, b) in ranges {
        let range = a.resolve(len)..b.resolve(len);
        let want = oracle_slice(&oracle, &range);
        assert_eq!(
            buf.slice(range.clone()),
            want,
            "slice({range:?}) of {oracle:?}"
        );
        into.clear();
        into.push('>');
        buf.slice_into(range.clone(), &mut into);
        assert_eq!(into[1..], want, "slice_into({range:?}) of {oracle:?}");
    }
    // Small texts: every pair of offsets up to just past the end.
    if len <= 48 {
        for s in 0..=len + 1 {
            for e in 0..=len + 1 {
                assert_eq!(
                    buf.slice(s..e),
                    oracle_slice(&oracle, &(s..e)),
                    "slice({s}..{e}) of {oracle:?}"
                );
            }
        }
    }
}

/// Sorted, non-overlapping ranges at char boundaries of `oracle`, one per
/// item: the `2n` endpoints sorted and paired up, so neighbours may touch
/// and ranges may be empty.
fn sorted_items<'a>(
    oracle: &str,
    parts: &'a [(Index, Index, String)],
) -> Vec<(Range<usize>, &'a str)> {
    let bounds = boundaries(oracle);
    let mut ends: Vec<usize> = parts
        .iter()
        .flat_map(|(a, b, _)| [*a.get(&bounds), *b.get(&bounds)])
        .collect();
    ends.sort_unstable();
    parts
        .iter()
        .enumerate()
        .map(|(i, (_, _, t))| (ends[2 * i]..ends[2 * i + 1], t.as_str()))
        .collect()
}

/// `replace_sorted` gives the same text and the same edits as applying the
/// items one by one from the last, and those edits undo and redo.
#[expect(clippy::unwrap_used, reason = "test helper")]
fn replace_sorted_like_one_by_one<B: Backend>(
    initial: &str,
    ops: &[Op],
    parts: &[(Index, Index, String)],
) {
    // Insert first, so the items land on a text of several pieces.
    let mut all = B::build(initial);
    let mut each = B::build(initial);
    let mut oracle = initial.to_owned();
    for op in ops {
        let bounds = boundaries(&oracle);
        if let Op::Insert { at, text } = op {
            let p = *at.get(&bounds);
            all.replace(p..p, text).unwrap();
            apply(&mut each, &mut oracle, p..p, text);
        }
    }
    let before = oracle.clone();
    let items = sorted_items(&oracle, parts);
    let mut expected = Vec::new();
    for (range, text) in items.iter().rev() {
        expected.push(Edit {
            start: range.start,
            removed: oracle[range.clone()].into(),
            inserted: (*text).into(),
        });
        apply(&mut each, &mut oracle, range.clone(), text);
    }
    let got = all.replace_sorted(&items);
    assert!(
        got.is_ok(),
        "replace_sorted({items:?}) on {before:?}: {got:?}"
    );
    let got = got.unwrap();
    assert_eq!(got.len(), expected.len(), "edit count");
    for (i, (g, want)) in got.iter().zip(&expected).enumerate() {
        same_edit::<B>(
            &Ok(g.clone()),
            want,
            &format!("edit {i} of {items:?} on {before:?}"),
        );
    }
    all.check();
    check_text(&all, &oracle);
    assert_eq!(each.slice(0..usize::MAX), oracle);

    // Undo the group as History does: in reverse, putting back what each
    // edit removed. Then redo: forwards, putting back what each inserted.
    for e in got.iter().rev() {
        all.put(e.new_range(), &e.removed).unwrap();
        all.check();
    }
    check_text(&all, &before);
    for e in &got {
        all.put(e.old_range(), &e.inserted).unwrap();
    }
    check_text(&all, &oracle);

    // Out of order: refused, and nothing changes.
    let now = sorted_items(&oracle, parts);
    if let [first, second, ..] = &now[..]
        && first.0.end > first.0.start
    {
        let swapped = [second.clone(), first.clone()];
        assert_eq!(
            all.replace_sorted(&swapped),
            Err(EditError::Unsorted { index: 1 }),
            "{swapped:?}"
        );
        check_text(&all, &oracle);
    }
}

proptest! {
    #![proptest_config(config(256))]

    /// Valid inserts, deletes and replaces (with some invalid attempts and
    /// snapshots mixed in) keep the buffer equal to the oracle.
    #[test]
    fn edit_sequences_match_oracle(
        initial in text(32),
        ops in prop::collection::vec(op(), 0..40),
    ) {
        run_sequence::<RopeBuffer>(&initial, &ops);
        run_sequence::<MappedBuffer>(&initial, &ops);
    }

    /// Every offset inside a multi-byte character, and ranges past the end or
    /// reversed, are refused with the right error and change nothing.
    #[test]
    fn invalid_edits_are_rejected(initial in text(32), past in 0usize..4) {
        invalid_edits::<RopeBuffer>(&initial, past);
        invalid_edits::<MappedBuffer>(&initial, past);
    }

    /// A snapshot keeps the text it was taken with, however the buffer is
    /// edited afterwards.
    #[test]
    fn snapshots_ignore_later_edits(
        initial in text(32),
        before in prop::collection::vec(op(), 0..20),
        after in prop::collection::vec(op(), 1..20),
    ) {
        let mut ops = before;
        ops.push(Op::Snapshot);
        ops.extend(after);
        run_sequence::<RopeBuffer>(&initial, &ops);
        run_sequence::<MappedBuffer>(&initial, &ops);
    }

    /// `slice` and `slice_into` never panic, and return the oracle's text
    /// clamped to the end and widened outward to char boundaries.
    #[test]
    fn slice_clamps_and_widens(
        initial in text(32),
        ops in prop::collection::vec(op(), 0..8),
        ranges in prop::collection::vec((pos(), pos()), 1..64),
    ) {
        slices::<RopeBuffer>(&initial, &ops, &ranges);
        slices::<MappedBuffer>(&initial, &ops, &ranges);
    }

    /// Many replacements at once match applying them one at a time.
    #[test]
    fn replace_sorted_matches_one_by_one(
        initial in text(24),
        ops in prop::collection::vec(op(), 0..10),
        parts in prop::collection::vec((any::<Index>(), any::<Index>(), text(3)), 0..8),
    ) {
        replace_sorted_like_one_by_one::<RopeBuffer>(&initial, &ops, &parts);
        replace_sorted_like_one_by_one::<MappedBuffer>(&initial, &ops, &parts);
    }
}

proptest! {
    #![proptest_config(config(24))]

    /// The same edit sequences on texts spanning several rope chunks, so
    /// lines and slices that cross chunk boundaries are compared too.
    #[test]
    fn multi_chunk_edit_sequences_match_oracle(
        initial in long_text(),
        ops in prop::collection::vec(op(), 0..20),
    ) {
        run_sequence::<RopeBuffer>(&initial, &ops);
        run_sequence::<MappedBuffer>(&initial, &ops);
    }
}

proptest! {
    #![proptest_config(config(64))]

    /// The enum that holds either backend passes every call through.
    #[test]
    fn any_buffer_matches_oracle(
        initial in text(32),
        ops in prop::collection::vec(op(), 0..30),
    ) {
        run_sequence::<AnyBuffer>(&initial, &ops);
    }
}
