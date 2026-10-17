//! Property tests for editing at several cursors (M8, multi-cursor): random
//! cursor sets and operations on both buffer backends, checked against a
//! plain `String` with a naive list of positions.
//!
//! Each step of a session works out one replacement per selection (typing,
//! Enter, Backspace, Delete, word-delete, paste, indent), then runs it
//! through the real path, exactly as `Document::apply_each` in the app does:
//! [`multi::apply_each`], [`report_sorted`] (replayed into a mirror string,
//! standing in for the syntax tree), [`Selections::map_through`] and
//! [`History::record_batch`]. Other steps add a cursor, collapse to the
//! primary, move every cursor, undo and redo.
//!
//! The oracle is written from the documented rules, not from the code:
//!
//! - **Edits**: replacements that change nothing are dropped, deletions that
//!   overlap are merged until none do, and the rest are applied to the
//!   `String` one at a time from the last to the first.
//! - **Mapping**: a position covered by an edit (the start of a selection by
//!   an edit whose old range holds it, `start <= p < end`; the end of one, or
//!   a cursor, by `start < p <= end`; either by an insertion exactly at it)
//!   goes to the start of that edit's new text with [`Bias::Before`] or its
//!   end with [`Bias::After`]. Any other position shifts by the growth of
//!   every edit that ends at or before it.
//! - **Merging**: sorted by (start, end, head), the first adjacent pair that
//!   overlaps (or touches where either is a cursor) merges, until no pair
//!   does. The merged selection points the way the primary did if the
//!   primary is a non-empty member, otherwise the way the first member did,
//!   or the second if the first is a bare cursor (which has no direction).
//! - **Undo grouping**: a model of the coalescing rules from `undo.rs`
//!   (same number of edits, all typing or all deleting, contiguous at every
//!   cursor, inside [`COALESCE_WINDOW`] and not sealed since), so every undo
//!   must restore exactly the text and the selections from before its group
//!   and every redo those from after it.
//!
//! Every property runs on [`RopeBuffer`] and on a [`MappedBuffer`] with tiny
//! [`Limits`], so removals over 4 bytes are held as pieces and both
//! `replace_sorted` and `put_sorted` go through the piece table.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nimbus_core::line_index::{LineIndex, Spacing};
use nimbus_core::undo::COALESCE_WINDOW;
use nimbus_core::{
    Bias, Buffer, Content, Edit, History, Limits, MappedBuffer, RopeBuffer, Selection, Selections,
    Text, VecSource, movement, multi, report_sorted,
};
use proptest::prelude::*;
use proptest::sample::Index;
use proptest::test_runner::Config;

/// Building blocks for starting texts and pasted text: line endings,
/// multi-byte characters, an emoji ZWJ sequence and a combining accent.
const PIECES: &[&str] = &[
    "a",
    "Zq",
    "hello ",
    "foo.bar",
    "  ",
    "\n",
    "\r\n",
    "\r",
    "é",
    "日本",
    "👩\u{200d}💻",
    "e\u{301}",
    "\t",
    "",
];

/// What a keystroke types: never a line break, so runs of it coalesce.
const TYPED: &[&str] = &[
    "a",
    "b",
    " ",
    "é",
    "日",
    "👩\u{200d}💻",
    "e\u{301}",
    "\t",
    "\r",
    "xy",
];

/// Limits for the mapped backend that make every path run on small texts.
const TINY: Limits = Limits {
    copy_limit: 4,
    segment: 16,
    max_piece: 6,
    scan: 3,
    pseudo_line: 8,
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
}

impl Backend for RopeBuffer {
    fn build(text: &str) -> Self {
        RopeBuffer::from_text(text)
    }
}

impl Backend for MappedBuffer {
    #[expect(clippy::unwrap_used, reason = "test helper")]
    fn build(text: &str) -> Self {
        let index = LineIndex::build(text.as_bytes(), Spacing::new(2, 5));
        MappedBuffer::with_index(Arc::new(VecSource::from(text)), index, TINY).unwrap()
    }

    fn copy_limit() -> Option<usize> {
        Some(TINY.copy_limit)
    }
}

/// The bytes of `content`, read from the buffer's storage for pieces.
#[expect(clippy::unwrap_used, reason = "test helper")]
fn bytes_of(content: &Content) -> Vec<u8> {
    let mut out = Vec::new();
    content.read_into(&mut out).unwrap();
    out
}

/// A callback for undo and redo that applies each reported edit to `mirror`
/// and checks the inserted text against the buffer after the edit, as the
/// syntax tree would read it.
#[expect(clippy::unwrap_used, reason = "test helper")]
fn replay_into(mirror: &mut String) -> impl FnMut(&dyn Buffer, &Edit) + '_ {
    |after, edit| {
        let bytes = bytes_of(&edit.inserted);
        let inserted = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(after.slice(edit.new_range()), inserted, "reported insert");
        mirror.replace_range(edit.old_range(), inserted);
    }
}

/// `cases` cases, or `PROPTEST_CASES` when set, for longer local runs.
fn config(cases: u32) -> Config {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(cases);
    Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    }
}

fn piece() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => prop::sample::select(PIECES).prop_map(str::to_owned),
        1 => "[ -~]{0,6}",
    ]
}

fn text(max_pieces: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(piece(), 0..max_pieces).prop_map(|v| v.concat())
}

/// One line of a clipboard: no `\n`.
fn line() -> impl Strategy<Value = String> {
    text(3).prop_map(|s| s.replace('\n', ""))
}

/// Milliseconds before a step: mostly typing speed, so steps coalesce,
/// with the window's edge drawn often.
fn delay() -> impl Strategy<Value = u64> {
    prop_oneof![
        5 => 0u64..300,
        3 => 0u64..1200,
        2 => prop::sample::select(vec![0u64, 10, 499, 500, 501]),
    ]
}

/// One step of a session. Positions are `Index`es, resolved against the
/// oracle's char boundaries when the step runs.
#[derive(Debug, Clone)]
enum Op {
    /// Type at every selection, replacing it.
    Type(String),
    /// Enter: `\r\n` if true, else `\n`.
    Newline(bool),
    /// Each cursor deletes the grapheme before it; a selection itself.
    DeleteBack,
    /// Each cursor deletes the grapheme after it; a selection itself.
    DeleteForward,
    /// Each cursor deletes back to `word_left`; a selection itself.
    DeleteWordBack,
    /// The whole (multi-line) text at every selection, one undo step.
    Paste(String),
    /// One line per cursor, as pasting what several cursors copied.
    PasteLines(Vec<String>),
    /// A tab at the start of every line any selection touches, once each.
    Indent,
    /// Left or Right (`right`) at every cursor, extending or not. Without
    /// `seal` the history is not told, as with a caller that forgets to
    /// seal: then only the contiguity rule keeps the next keystroke from
    /// joining the step before the move.
    Move {
        right: bool,
        extend: bool,
        seal: bool,
    },
    /// A new primary selection from `a` to `b`, or a cursor at `a`.
    AddCursor {
        a: Index,
        b: Index,
        empty: bool,
    },
    /// A cursor `chars` characters from the primary's head (either way),
    /// so word-deletes from two cursors reach the same start.
    AddNear(isize),
    /// Escape: keep only the primary.
    Collapse,
    Undo,
    Redo,
}

/// Keystrokes that coalesce, and moves that do not seal: for the grouping
/// rules, which need several such steps in a row to be tested.
fn coalescing_op() -> BoxedStrategy<Op> {
    prop_oneof![
        6 => prop::sample::select(TYPED).prop_map(|t| Op::Type(t.to_owned())),
        3 => Just(Op::DeleteBack),
        2 => Just(Op::DeleteForward),
        2 => Just(Op::DeleteWordBack),
        3 => (any::<bool>(), any::<bool>())
            .prop_map(|(right, extend)| Op::Move { right, extend, seal: false }),
        2 => (-4isize..=4).prop_map(Op::AddNear),
        1 => Just(Op::Undo),
        1 => Just(Op::Redo),
    ]
    .boxed()
}

fn any_op() -> BoxedStrategy<Op> {
    prop_oneof![
        8 => prop::sample::select(TYPED).prop_map(|t| Op::Type(t.to_owned())),
        1 => any::<bool>().prop_map(Op::Newline),
        4 => Just(Op::DeleteBack),
        2 => Just(Op::DeleteForward),
        3 => Just(Op::DeleteWordBack),
        1 => text(5).prop_map(Op::Paste),
        1 => prop::collection::vec(line(), 8).prop_map(Op::PasteLines),
        1 => Just(Op::Indent),
        2 => (any::<bool>(), any::<bool>(), any::<bool>())
            .prop_map(|(right, extend, seal)| Op::Move { right, extend, seal }),
        2 => (any::<Index>(), any::<Index>(), any::<bool>())
            .prop_map(|(a, b, empty)| Op::AddCursor { a, b, empty }),
        3 => (-4isize..=4).prop_map(Op::AddNear),
        1 => Just(Op::Collapse),
        2 => Just(Op::Undo),
        2 => Just(Op::Redo),
    ]
    .boxed()
}

/// Starting selections: positions as `Index`es, and whether each is a bare
/// cursor. Non-empty ones point either way.
fn start_sels() -> impl Strategy<Value = (Vec<(Index, Index, bool)>, Index)> {
    (
        prop::collection::vec((any::<Index>(), any::<Index>(), any::<bool>()), 1..=8),
        any::<Index>(),
    )
}

fn boundaries(text: &str) -> Vec<usize> {
    text.char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .collect()
}

fn resolve(bounds: &[usize], (a, b, empty): &(Index, Index, bool)) -> Selection {
    let anchor = *a.get(bounds);
    if *empty {
        Selection::cursor(anchor)
    } else {
        Selection {
            anchor,
            head: *b.get(bounds),
        }
    }
}

// ---------------------------------------------------------------------------
// The oracle.

/// A selection in the oracle, with the primary marked on it rather than
/// held as an index, so merging carries it along without bookkeeping.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OSel {
    anchor: usize,
    head: usize,
    primary: bool,
}

impl OSel {
    fn start(&self) -> usize {
        self.anchor.min(self.head)
    }
    fn end(&self) -> usize {
        self.anchor.max(self.head)
    }
    fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
    fn backward(&self) -> bool {
        self.head < self.anchor
    }
    fn span(start: usize, end: usize, backward: bool, primary: bool) -> Self {
        let (anchor, head) = if backward { (end, start) } else { (start, end) };
        Self {
            anchor,
            head,
            primary,
        }
    }
}

/// Whether `b`, sorted after `a`, merges into it.
fn o_overlaps(a: &OSel, b: &OSel) -> bool {
    b.start() < a.end() || (b.start() == a.end() && (a.is_empty() || b.is_empty()))
}

/// Sort once, then merge the first overlapping adjacent pair until none is
/// left. A merged selection keeps its first member's place (it is not
/// sorted again), so "the first member" is first in the original order.
fn o_normalize(v: &mut Vec<OSel>) {
    v.sort_by_key(|s| (s.start(), s.end(), s.head));
    loop {
        let Some(i) = (0..v.len().saturating_sub(1)).find(|&i| o_overlaps(&v[i], &v[i + 1])) else {
            return;
        };
        let b = v.remove(i + 1);
        let a = &v[i];
        // A bare cursor has no direction to keep.
        let backward = if (b.primary && !b.is_empty()) || a.is_empty() {
            b.backward()
        } else {
            a.backward()
        };
        v[i] = OSel::span(
            a.start(),
            a.end().max(b.end()),
            backward,
            a.primary || b.primary,
        );
    }
}

/// One replacement per selection: a range and its new text.
type Items = Vec<(Range<usize>, String)>;

/// `(anchor, head)` pairs and the primary's index.
type View = (Vec<(usize, usize)>, usize);

fn o_view(v: &[OSel]) -> View {
    let pairs = v.iter().map(|s| (s.anchor, s.head)).collect();
    let primary = v.iter().position(|s| s.primary);
    assert!(primary.is_some(), "oracle lost its primary: {v:?}");
    (pairs, primary.unwrap_or_default())
}

fn lib_view(s: &Selections) -> View {
    (
        s.as_slice().iter().map(|s| (s.anchor, s.head)).collect(),
        s.primary_index(),
    )
}

/// One replacement, with offsets in the text before its batch.
#[derive(Debug, Clone)]
struct OEdit {
    start: usize,
    end: usize,
    removed: String,
    inserted: String,
}

impl OEdit {
    fn growth(&self) -> isize {
        isize::try_from(self.inserted.len()).unwrap_or(isize::MAX)
            - isize::try_from(self.removed.len()).unwrap_or(isize::MAX)
    }
}

fn shifted(p: usize, by: isize) -> usize {
    p.checked_add_signed(by).unwrap_or(usize::MAX)
}

/// Whether `e` covers position `p`, the start of a selection if `start`,
/// otherwise its end (or a bare cursor).
fn covers(e: &OEdit, p: usize, start: bool) -> bool {
    let insertion_here = e.start == e.end && e.start == p;
    insertion_here
        || if start {
            e.start <= p && p < e.end
        } else {
            e.start < p && p <= e.end
        }
}

/// Where `p` lands after `edits` (in text order, old offsets).
fn o_map(edits: &[OEdit], p: usize, start: bool, bias: Bias) -> usize {
    if let Some(j) = edits.iter().position(|e| covers(e, p, start)) {
        let e = &edits[j];
        let new_start = shifted(e.start, edits[..j].iter().map(OEdit::growth).sum());
        return match bias {
            Bias::After => new_start + e.inserted.len(),
            Bias::Before => new_start,
        };
    }
    shifted(
        p,
        edits.iter().filter(|e| e.end <= p).map(OEdit::growth).sum(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Typing,
    Deleting,
    Other,
}

fn o_kind(e: &OEdit) -> Kind {
    if e.removed.is_empty() && !e.inserted.is_empty() && !e.inserted.contains('\n') {
        Kind::Typing
    } else if !e.removed.is_empty() && e.inserted.is_empty() {
        Kind::Deleting
    } else {
        Kind::Other
    }
}

/// Text and selections at one point.
#[derive(Debug, Clone)]
struct Snap {
    text: String,
    sels: Vec<OSel>,
}

#[derive(Debug, Clone)]
struct OGroup {
    before: Snap,
    after: Snap,
    /// Recorded as a batch of several edits (else one edit at a time).
    batch: bool,
    /// The newest batch, in text order.
    last: Vec<OEdit>,
}

#[derive(Debug)]
struct Oracle {
    text: String,
    sels: Vec<OSel>,
    undo: Vec<OGroup>,
    redo: Vec<OGroup>,
    open_since: Option<Instant>,
}

impl Oracle {
    fn new(text: &str, sels: &[Selection], primary: usize) -> Self {
        let primary = primary.min(sels.len() - 1);
        let mut v: Vec<OSel> = sels
            .iter()
            .enumerate()
            .map(|(i, s)| OSel {
                anchor: s.anchor,
                head: s.head,
                primary: i == primary,
            })
            .collect();
        o_normalize(&mut v);
        Self {
            text: text.to_owned(),
            sels: v,
            undo: Vec::new(),
            redo: Vec::new(),
            open_since: None,
        }
    }

    fn snap(&self) -> Snap {
        Snap {
            text: self.text.clone(),
            sels: self.sels.clone(),
        }
    }

    fn seal(&mut self) {
        self.open_since = None;
    }

    /// Apply one replacement per selection. Returns the edits made, in text
    /// order, and whether the history should start a new undo step; `None`
    /// if nothing changed.
    fn edit(
        &mut self,
        items: &[(Range<usize>, String)],
        bias: (Bias, Bias),
        now: Instant,
    ) -> Option<(Vec<OEdit>, bool)> {
        let mut items: Vec<(Range<usize>, String)> = items
            .iter()
            .filter(|(r, t)| !(r.is_empty() && t.is_empty()))
            .cloned()
            .collect();
        items.sort_by_key(|(r, _)| (r.start, r.end));
        // Merge overlapping deletions until no two items overlap.
        'merge: loop {
            for i in 0..items.len() {
                for j in i + 1..items.len() {
                    if items[j].0.start < items[i].0.end {
                        assert!(
                            items[i].1.is_empty() && items[j].1.is_empty(),
                            "test bug: overlapping items with text: {items:?}"
                        );
                        let end = items[i].0.end.max(items[j].0.end);
                        items[i].0.end = end;
                        items.remove(j);
                        continue 'merge;
                    }
                }
            }
            break;
        }
        if items.is_empty() {
            return None;
        }
        let edits: Vec<OEdit> = items
            .iter()
            .map(|(r, t)| OEdit {
                start: r.start,
                end: r.end,
                removed: self.text[r.clone()].to_owned(),
                inserted: t.clone(),
            })
            .collect();
        let before = self.snap();
        for e in edits.iter().rev() {
            self.text.replace_range(e.start..e.end, &e.inserted);
        }
        for s in &mut self.sels {
            let backward = s.backward();
            let (a, b) = if s.is_empty() {
                let p = o_map(&edits, s.head, false, bias.1);
                (p, p)
            } else {
                (
                    o_map(&edits, s.start(), true, bias.0),
                    o_map(&edits, s.end(), false, bias.1),
                )
            };
            *s = if backward {
                OSel {
                    anchor: b,
                    head: a,
                    primary: s.primary,
                }
            } else {
                OSel {
                    anchor: a,
                    head: b,
                    primary: s.primary,
                }
            };
        }
        o_normalize(&mut self.sels);
        let new_group = self.record(&edits, before, now);
        Some((edits, new_group))
    }

    /// Whether `edits` continue the newest group.
    fn joins(&self, edits: &[OEdit], now: Instant) -> bool {
        let open = self
            .open_since
            .is_some_and(|t| now.duration_since(t) < COALESCE_WINDOW);
        if !open || edits.iter().any(|e| o_kind(e) == Kind::Other) {
            return false;
        }
        let Some(top) = self.undo.last() else {
            return false;
        };
        if top.batch != (edits.len() > 1) || top.last.len() != edits.len() {
            return false;
        }
        // Each cursor's previous edit, moved to the text after its batch.
        let mut shift = 0isize;
        for (prev, next) in top.last.iter().zip(edits) {
            let at = shifted(prev.start, shift);
            shift += prev.growth();
            let k = o_kind(next);
            let contiguous = match k {
                Kind::Typing => next.start == at + prev.inserted.len(),
                Kind::Deleting => next.end == at || next.start == at,
                Kind::Other => false,
            };
            if o_kind(prev) != k || !contiguous {
                return false;
            }
        }
        true
    }

    fn record(&mut self, edits: &[OEdit], before: Snap, now: Instant) -> bool {
        self.redo.clear();
        let joins = self.joins(edits, now);
        let after = self.snap();
        match self.undo.last_mut() {
            Some(g) if joins => {
                g.last = edits.to_vec();
                g.after = after;
            }
            _ => self.undo.push(OGroup {
                before,
                after,
                batch: edits.len() > 1,
                last: edits.to_vec(),
            }),
        }
        let other = edits.iter().any(|e| o_kind(e) == Kind::Other);
        self.open_since = if other { None } else { Some(now) };
        !joins
    }

    fn undo(&mut self) -> Option<Snap> {
        self.seal();
        let g = self.undo.pop()?;
        let snap = g.before.clone();
        self.redo.push(g);
        Some(snap)
    }

    fn redo(&mut self) -> Option<Snap> {
        self.seal();
        let g = self.redo.pop()?;
        let snap = g.after.clone();
        self.undo.push(g);
        Some(snap)
    }

    fn line_starts(&self) -> Vec<usize> {
        std::iter::once(0)
            .chain(self.text.match_indices('\n').map(|(i, _)| i + 1))
            .collect()
    }

    /// Every line touched by a selection, once, as their starts. A
    /// selection ending at the very start of a later line does not touch it.
    fn touched_line_starts(&self) -> Vec<usize> {
        let starts = self.line_starts();
        let line_of = |p: usize| starts.iter().rposition(|&s| s <= p).unwrap_or(0);
        let mut lines: Vec<usize> = Vec::new();
        for s in &self.sels {
            let first = line_of(s.start());
            let mut last = line_of(s.end());
            if last > first && starts[last] == s.end() {
                last -= 1;
            }
            lines.extend(first..=last);
        }
        lines.sort_unstable();
        lines.dedup();
        lines.into_iter().map(|l| starts[l]).collect()
    }
}

// ---------------------------------------------------------------------------
// A session: buffer, selections and history, run beside the oracle.

struct Session<B: Backend> {
    buf: B,
    sels: Selections,
    hist: History,
    oracle: Oracle,
    now: Instant,
}

impl<B: Backend> Session<B> {
    fn new(initial: &str, sels: &[(Index, Index, bool)], primary: Index) -> Self {
        let bounds = boundaries(initial);
        let list: Vec<Selection> = sels.iter().map(|s| resolve(&bounds, s)).collect();
        let p = primary.index(list.len());
        let oracle = Oracle::new(initial, &list, p);
        let s = Self {
            buf: B::build(initial),
            sels: Selections::from_vec(list, p),
            hist: History::new(),
            oracle,
            now: Instant::now(),
        };
        s.check("start");
        s
    }

    fn buffer_text(&self) -> String {
        self.buf.slice(0..usize::MAX).into_owned()
    }

    /// The replacement each selection makes for `op`, with the biases to
    /// map selections through it and whether it is its own undo step.
    fn items(&self, op: &Op) -> Option<(Items, (Bias, Bias), bool)> {
        let t: &dyn Text = &self.buf;
        let after = (Bias::After, Bias::After);
        let each = |f: &dyn Fn(&OSel) -> (Range<usize>, String)| -> Vec<_> {
            self.oracle.sels.iter().map(f).collect()
        };
        let range = |s: &OSel| s.start()..s.end();
        let delete = |f: &dyn Fn(usize) -> Range<usize>| {
            each(&|s| {
                let r = if s.is_empty() { f(s.head) } else { range(s) };
                (r, String::new())
            })
        };
        Some(match op {
            Op::Type(text) => (each(&|s| (range(s), text.clone())), after, false),
            Op::Newline(crlf) => {
                let nl = if *crlf { "\r\n" } else { "\n" };
                (each(&|s| (range(s), nl.to_owned())), after, false)
            }
            Op::DeleteBack => (delete(&|h| movement::prev_grapheme(t, h)..h), after, false),
            Op::DeleteForward => (delete(&|h| h..movement::next_grapheme(t, h)), after, false),
            Op::DeleteWordBack => (
                delete(&|h| {
                    let to = movement::word_left(t, h);
                    to.min(h)..to.max(h)
                }),
                after,
                false,
            ),
            Op::Paste(text) => (each(&|s| (range(s), text.clone())), after, true),
            Op::PasteLines(lines) => {
                let n = self.oracle.sels.len();
                let items = if n > 1 {
                    self.oracle
                        .sels
                        .iter()
                        .zip(lines)
                        .map(|(s, l)| (range(s), l.clone()))
                        .collect()
                } else {
                    let whole = lines.join("\n");
                    each(&|s| (range(s), whole.clone()))
                };
                (items, after, true)
            }
            Op::Indent => (
                self.oracle
                    .touched_line_starts()
                    .into_iter()
                    .map(|p| (p..p, "\t".to_owned()))
                    .collect(),
                (Bias::Before, Bias::After),
                true,
            ),
            _ => return None,
        })
    }

    /// Run `items` through the library and the oracle and compare.
    fn apply(&mut self, items: &[(Range<usize>, String)], bias: (Bias, Bias), seal: bool) {
        if seal {
            self.hist.seal();
            self.oracle.seal();
        }
        let what = format!("{items:?} on {:?}", self.oracle.text);
        let text_before = self.oracle.text.clone();
        let before = self.sels.clone();
        let expected = self.oracle.edit(items, bias, self.now);

        let mut cows: Vec<(Range<usize>, Cow<'_, str>)> = items
            .iter()
            .map(|(r, t)| (r.clone(), Cow::Borrowed(t.as_str())))
            .collect();
        let got = multi::apply_each(&mut self.buf, &mut cows);
        assert!(got.is_ok(), "apply_each({what}): {got:?}");
        let mut edits = got.unwrap_or_default();

        let Some((want, new_group)) = expected else {
            assert!(
                edits.is_empty(),
                "{what}: oracle changed nothing: {edits:?}"
            );
            return;
        };
        // Last first, offsets in the text before the batch.
        assert_eq!(edits.len(), want.len(), "{what}: edit count");
        for (got, want) in edits.iter().zip(want.iter().rev()) {
            assert_eq!(got.start, want.start, "{what}: start");
            assert_eq!(got.inserted, *want.inserted, "{what}: inserted");
            assert_eq!(
                bytes_of(&got.removed),
                want.removed.as_bytes(),
                "{what}: removed"
            );
            let over = B::copy_limit().is_some_and(|limit| want.removed.len() > limit);
            assert_eq!(
                matches!(got.removed, Content::Pieces(_)),
                over,
                "{what}: removal held as pieces"
            );
        }

        // The syntax tree's view: each edit in text order, read against the
        // text after the whole batch, must replay to that text.
        let mut mirror = text_before;
        let buf = &self.buf;
        report_sorted(&mut edits, |e| {
            let bytes = bytes_of(&e.inserted);
            let inserted = String::from_utf8_lossy(&bytes);
            assert_eq!(buf.slice(e.new_range()), inserted, "{what}: reported edit");
            mirror.replace_range(e.old_range(), &inserted);
        });
        assert_eq!(mirror, self.oracle.text, "{what}: report_sorted replay");

        self.sels.map_through(&edits, bias.0, bias.1);
        let top = self.hist.top_id();
        self.hist.record_batch(edits, &before, &self.sels, self.now);
        assert_eq!(
            self.hist.top_id() != top,
            new_group,
            "{what}: history started a new undo step (model: {new_group})"
        );
        if seal {
            self.hist.seal();
            self.oracle.seal();
        }
    }

    /// Undo (or redo) in both; compare what comes back.
    fn undo_redo(&mut self, undo: bool) {
        let mut mirror = self.oracle.text.clone();
        let text_before = self.oracle.text.clone();
        let got = if undo {
            self.hist.undo(&mut self.buf, &mut replay_into(&mut mirror))
        } else {
            self.hist.redo(&mut self.buf, &mut replay_into(&mut mirror))
        };
        assert!(got.is_ok(), "undo={undo} on {text_before:?}: {got:?}");
        let got = got.ok().flatten();
        let want = if undo {
            self.oracle.undo()
        } else {
            self.oracle.redo()
        };
        assert_eq!(
            got.is_some(),
            want.is_some(),
            "undo={undo}: history returned {got:?}, model {want:?}"
        );
        if let (Some(sels), Some(snap)) = (got, want) {
            self.sels = sels;
            self.oracle.text = snap.text;
            self.oracle.sels = snap.sels;
        }
        assert_eq!(mirror, self.buffer_text(), "undo={undo}: reported edits");
    }

    fn step(&mut self, op: &Op, delay_ms: u64) {
        self.now += Duration::from_millis(delay_ms);
        if let Some((items, bias, seal)) = self.items(op) {
            self.apply(&items, bias, seal);
        } else {
            match op {
                Op::Move {
                    right,
                    extend,
                    seal,
                } => {
                    let t: &dyn Text = &self.buf;
                    let f = |s: Selection| {
                        if !extend && !s.is_empty() {
                            let r = s.range();
                            return Selection::cursor(if *right { r.end } else { r.start });
                        }
                        let to = if *right {
                            movement::next_grapheme(t, s.head)
                        } else {
                            movement::prev_grapheme(t, s.head)
                        };
                        s.moved_to(to, *extend)
                    };
                    self.sels.transform(|_, s| f(s));
                    for s in &mut self.oracle.sels {
                        let m = f(Selection {
                            anchor: s.anchor,
                            head: s.head,
                        });
                        (s.anchor, s.head) = (m.anchor, m.head);
                    }
                    o_normalize(&mut self.oracle.sels);
                    if *seal {
                        self.hist.seal();
                        self.oracle.seal();
                    }
                }
                Op::AddCursor { .. } | Op::AddNear(_) => {
                    let bounds = boundaries(&self.oracle.text);
                    let sel = match op {
                        Op::AddCursor { a, b, empty } => resolve(&bounds, &(*a, *b, *empty)),
                        Op::AddNear(chars) => {
                            let head = self.sels.primary().head;
                            let at = bounds.iter().position(|&b| b == head).unwrap_or(0);
                            let to = at.saturating_add_signed(*chars).min(bounds.len() - 1);
                            Selection::cursor(bounds[to])
                        }
                        _ => return,
                    };
                    self.sels.push_primary(sel);
                    for s in &mut self.oracle.sels {
                        s.primary = false;
                    }
                    self.oracle.sels.push(OSel {
                        anchor: sel.anchor,
                        head: sel.head,
                        primary: true,
                    });
                    o_normalize(&mut self.oracle.sels);
                    self.hist.seal();
                    self.oracle.seal();
                }
                Op::Collapse => {
                    self.sels.collapse_to_primary();
                    self.oracle.sels.retain(|s| s.primary);
                    self.hist.seal();
                    self.oracle.seal();
                }
                Op::Undo => self.undo_redo(true),
                Op::Redo => self.undo_redo(false),
                _ => {}
            }
        }
        self.check(&format!("{op:?}"));
    }

    /// Everything the library holds must match the oracle.
    fn check(&self, what: &str) {
        let text = &self.oracle.text;
        assert_eq!(&self.buffer_text(), text, "after {what}: text");
        assert_eq!(
            lib_view(&self.sels),
            o_view(&self.oracle.sels),
            "after {what}: selections (anchor, head) and primary, on {text:?}"
        );
        let s = self.sels.as_slice();
        assert!(self.sels.primary_index() < s.len(), "after {what}: primary");
        for sel in s {
            assert!(
                text.is_char_boundary(sel.anchor) && text.is_char_boundary(sel.head),
                "after {what}: {sel:?} not on char boundaries of {text:?}"
            );
        }
        for w in s.windows(2) {
            let (a, b) = (w[0], w[1]);
            assert!(
                a.range().end < b.range().start
                    || (a.range().end == b.range().start && !a.is_empty() && !b.is_empty()),
                "after {what}: {a:?} and {b:?} overlap or are out of order"
            );
        }
        assert_eq!(self.hist.can_undo(), !self.oracle.undo.is_empty(), "{what}");
        assert_eq!(self.hist.can_redo(), !self.oracle.redo.is_empty(), "{what}");
    }
}

/// The main property: run `ops`, checking every step, then undo everything
/// (each step checked against the model) back to the initial text, and
/// redo everything back to the final one.
fn session<B: Backend>(
    initial: &str,
    sels: &(Vec<(Index, Index, bool)>, Index),
    ops: &[(Op, u64)],
) {
    let mut s = Session::<B>::new(initial, &sels.0, sels.1);
    for (op, d) in ops {
        s.step(op, *d);
    }
    let final_text = s.oracle.text.clone();
    let groups = s.oracle.undo.len();
    for _ in 0..groups {
        s.step(&Op::Undo, 0);
    }
    assert!(!s.hist.can_undo(), "{groups} undos left more to undo");
    assert_eq!(s.oracle.text, initial, "undo-all restores the initial text");
    for _ in 0..groups {
        s.step(&Op::Redo, 0);
    }
    // Steps undone before the session ended are still there to redo.
    assert_eq!(s.hist.can_redo(), !s.oracle.redo.is_empty(), "redo left");
    assert_eq!(
        s.oracle.text, final_text,
        "redo-all restores the final text"
    );
}

/// Typing `keys` at a stable set of cursors, each key under the window
/// after the last, is one undo step, which restores the text and every
/// cursor.
fn typing_run<B: Backend>(initial: &str, cursors: &[Index], keys: &[(&str, u64)]) {
    let bounds = boundaries(initial);
    let list: Vec<Selection> = cursors
        .iter()
        .map(|i| Selection::cursor(*i.get(&bounds)))
        .collect();
    let mut sels = Selections::from_vec(list, 0);
    let start = sels.clone();
    let mut buf = B::build(initial);
    let mut hist = History::new();
    let mut now = Instant::now();
    let mut top = None;
    for (key, d) in keys {
        now += Duration::from_millis(*d);
        let before = sels.clone();
        let mut items: Vec<(Range<usize>, Cow<'_, str>)> = sels
            .iter()
            .map(|s| (s.range(), Cow::Borrowed(*key)))
            .collect();
        let edits = multi::apply_each(&mut buf, &mut items);
        assert!(edits.is_ok(), "{edits:?}");
        let edits = edits.unwrap_or_default();
        assert_eq!(edits.len(), start.count(), "one edit per cursor");
        sels.map_through(&edits, Bias::After, Bias::After);
        assert_eq!(sels.count(), start.count(), "cursors stay apart");
        hist.record_batch(edits, &before, &sels, now);
        assert_eq!(
            *top.get_or_insert(hist.top_id()),
            hist.top_id(),
            "typing {key:?} started a new undo step"
        );
    }
    let restored = hist.undo(&mut buf, &mut |_, _| {});
    assert!(
        matches!(&restored, Ok(Some(s)) if *s == start),
        "{restored:?}"
    );
    assert_eq!(buf.slice(0..usize::MAX), initial);
    assert!(!hist.can_undo(), "one step for the whole run");
}

proptest! {
    #![proptest_config(config(128))]

    #[test]
    fn multi_cursor_sessions_match_the_oracle(
        initial in text(14),
        sels in start_sels(),
        ops in prop::collection::vec((any_op(), delay()), 0..40),
    ) {
        session::<RopeBuffer>(&initial, &sels, &ops);
        session::<MappedBuffer>(&initial, &sels, &ops);
    }

    #[test]
    fn coalescing_follows_the_grouping_rules(
        initial in text(10),
        sels in start_sels(),
        ops in prop::collection::vec((coalescing_op(), 0u64..600), 0..30),
    ) {
        session::<RopeBuffer>(&initial, &sels, &ops);
        session::<MappedBuffer>(&initial, &sels, &ops);
    }

    #[test]
    fn typing_within_the_window_is_one_undo_step(
        initial in text(10),
        cursors in prop::collection::vec(any::<Index>(), 1..=6),
        keys in prop::collection::vec((prop::sample::select(TYPED), 0u64..500), 1..12),
    ) {
        typing_run::<RopeBuffer>(&initial, &cursors, &keys);
        typing_run::<MappedBuffer>(&initial, &cursors, &keys);
    }
}
