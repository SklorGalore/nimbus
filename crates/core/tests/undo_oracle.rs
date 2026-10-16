//! Property tests for undo and redo (M3 task 3): random edit sessions on a
//! buffer with a [`History`] recording every edit, checked against a plain
//! `String` oracle. Every property runs on [`RopeBuffer`] and on
//! [`MappedBuffer`] (M7 task 7). The mapped buffer gets tiny [`Limits`], so
//! removals over 4 bytes are recorded as pieces ([`Content::Pieces`]) and
//! undo and redo put those pieces back; groups go through
//! [`Buffer::replace_sorted`], which the mapped buffer does in one pass.
//!
//! Coalescing makes the exact number of undo steps depend on timing, kinds
//! and positions, so most properties check *states* rather than counts: an
//! undo must land on a text that existed earlier on the current line of
//! history, a redo must give back exactly the text its undo took away, and
//! undoing everything must restore the starting text. One property
//! ([`undo_restores_each_groups_first_selections`]) goes further and keeps an
//! independent model of the grouping rules from `undo.rs`, so it can check
//! the group count and the selections every undo and redo returns.
//!
//! Time is simulated: each session starts at one `Instant` and every step
//! advances it by a generated delay of 0 to 1200 ms (with 499, 500 and 501
//! drawn often), so the 500 ms coalescing window is crossed both ways.

use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nimbus_core::line_index::{LineIndex, Spacing};
use nimbus_core::undo::COALESCE_WINDOW;
use nimbus_core::{
    Buffer, Content, Edit, EditError, History, Limits, MappedBuffer, RopeBuffer, Selection,
    Selections, Text, VecSource,
};
use proptest::prelude::*;
use proptest::sample::Index;
use proptest::test_runner::Config;
use unicode_segmentation::UnicodeSegmentation;

/// Building blocks for starting texts and replacements: line endings,
/// multi-byte characters, an emoji ZWJ sequence and a combining accent.
const PIECES: &[&str] = &[
    "a",
    "Zq",
    "hello ",
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

/// What a keystroke inserts. Mostly single characters, so runs of typing
/// coalesce; line breaks and multi-character strings appear too.
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
    "\n",
    "\r\n",
    "xy",
    "ab\ncd",
];

/// Selections carry a unique tag in their anchor, so a test can tell which
/// edit's selections an undo or redo handed back. History never interprets
/// selections, so the tags need not be valid offsets.
const TAG_BASE: usize = 1_000_000;

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

/// `got` records the same change as `want` (whose content is bytes), with
/// the removed text held as pieces exactly when over the copy limit.
fn same_edit<B: Backend>(got: &Edit, want: &Edit, what: &str) {
    assert_eq!(got.start, want.start, "{what}: start");
    assert_eq!(got.inserted, want.inserted, "{what}: inserted");
    assert_eq!(
        bytes_of(&got.removed),
        bytes_of(&want.removed),
        "{what}: removed"
    );
    let over = B::copy_limit().is_some_and(|limit| want.removed.len() > limit);
    assert_eq!(
        matches!(got.removed, Content::Pieces(_)),
        over,
        "{what}: removal of {} bytes held as pieces",
        want.removed.len()
    );
}

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
        1 => "[ -~]{0,6}",
    ]
}

fn text(max_pieces: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(piece(), 0..max_pieces).prop_map(|v| v.concat())
}

/// Milliseconds to advance the clock before a step. The window edges are
/// drawn often so `< COALESCE_WINDOW` is tested right at the boundary.
fn delay() -> impl Strategy<Value = u64> {
    prop_oneof![
        6 => 0u64..1200,
        2 => prop::sample::select(vec![0u64, 10, 499, 500, 501]),
    ]
}

/// One step of a session. Positions are `Index`es, resolved against the
/// oracle's char boundaries when the step runs.
#[derive(Debug, Clone)]
enum Op {
    /// Insert at the cursor and move the cursor past the insertion.
    Type(String),
    /// Remove the grapheme before the cursor.
    Backspace,
    /// Remove the grapheme after the cursor.
    DeleteForward,
    /// Replace an arbitrary range (possibly empty, possibly with nothing).
    Replace {
        a: Index,
        b: Index,
        text: String,
    },
    /// Replace 1 to 4 non-overlapping ranges as one step, like replace all.
    Group(Vec<(Index, Index, String)>),
    /// Move the cursor without sealing, as the history sees a caller that
    /// forgets to seal.
    Move(Index),
    /// End the current group.
    Seal,
    Undo,
    Redo,
}

fn edit_op() -> BoxedStrategy<Op> {
    prop_oneof![
        8 => prop::sample::select(TYPED).prop_map(|t| Op::Type(t.to_owned())),
        4 => Just(Op::Backspace),
        2 => Just(Op::DeleteForward),
        2 => (any::<Index>(), any::<Index>(), text(3))
            .prop_map(|(a, b, text)| Op::Replace { a, b, text }),
        1 => prop::collection::vec((any::<Index>(), any::<Index>(), text(2)), 1..=4)
            .prop_map(Op::Group),
        1 => any::<Index>().prop_map(Op::Move),
        1 => Just(Op::Seal),
    ]
    .boxed()
}

fn any_op() -> BoxedStrategy<Op> {
    prop_oneof![
        8 => edit_op(),
        2 => Just(Op::Undo),
        2 => Just(Op::Redo),
    ]
    .boxed()
}

/// A callback for undo and redo that applies each reported edit to `mirror`
/// and checks it against the buffer after the edit. Inserted text held as
/// pieces is read from the buffer's storage; either way it must be what
/// the buffer now holds at the edit's new range.
#[expect(clippy::unwrap_used, reason = "test helper")]
fn replay_into(mirror: &mut String) -> impl FnMut(&dyn Text, &Edit) + '_ {
    |after, edit| {
        let bytes = bytes_of(&edit.inserted);
        let inserted = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(after.slice(edit.new_range()), inserted, "reported insert");
        assert_eq!(
            bytes_of(&edit.removed).len(),
            edit.removed.len(),
            "removed length"
        );
        // Not checked against `after`'s length: a group undone in one
        // batch reports every edit with the text after the whole group.
        mirror.replace_range(edit.old_range(), inserted);
    }
}

fn steps(op: BoxedStrategy<Op>, max: usize) -> impl Strategy<Value = Vec<(Op, u64)>> {
    prop::collection::vec((op, delay()), 0..max)
}

// ---------------------------------------------------------------------------
// Model of the grouping rules, written from the rules in undo.rs: a new edit
// joins the newest group only if it is typing (no line break, nothing
// removed) or deleting (nothing inserted), the group's last edit is the same
// kind, the two are contiguous, under COALESCE_WINDOW passed since the last
// joined edit, and nothing sealed the group since (seal, undo, redo, a
// non-coalescing edit, or record_group).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Typing,
    Deleting,
    Other,
}

fn kind(e: &Edit) -> Kind {
    if e.removed.is_empty() && !e.inserted.is_empty() && !e.inserted.has_newline() {
        Kind::Typing
    } else if !e.removed.is_empty() && e.inserted.is_empty() {
        Kind::Deleting
    } else {
        Kind::Other
    }
}

fn contiguous(prev: &Edit, next: &Edit) -> bool {
    match kind(next) {
        Kind::Typing => next.start == prev.start + prev.inserted.len(),
        // Backspace ends where the last removal started; Delete starts there.
        Kind::Deleting => next.start + next.removed.len() == prev.start || next.start == prev.start,
        Kind::Other => false,
    }
}

#[derive(Debug, Clone)]
struct ModelGroup {
    last_edit: Edit,
    before_text: String,
    after_text: String,
    before: Selections,
    after: Selections,
}

#[derive(Debug, Default)]
struct Model {
    undo: Vec<ModelGroup>,
    redo: Vec<ModelGroup>,
    open_since: Option<Instant>,
}

impl Model {
    /// Record `step` (an edit's group as it would be if it started one);
    /// returns whether it started a new group.
    fn record(&mut self, step: ModelGroup, now: Instant) -> bool {
        self.redo.clear();
        let k = kind(&step.last_edit);
        let joins = k != Kind::Other
            && self
                .open_since
                .is_some_and(|t| now.duration_since(t) < COALESCE_WINDOW)
            && self.undo.last().is_some_and(|g| {
                kind(&g.last_edit) == k && contiguous(&g.last_edit, &step.last_edit)
            });
        match self.undo.last_mut() {
            Some(g) if joins => {
                g.last_edit = step.last_edit;
                g.after_text = step.after_text;
                g.after = step.after;
            }
            _ => self.undo.push(step),
        }
        self.open_since = if k == Kind::Other { None } else { Some(now) };
        !joins
    }

    fn record_group(&mut self, step: ModelGroup) {
        self.redo.clear();
        self.undo.push(step);
        self.open_since = None;
    }

    fn undo(&mut self) -> Option<ModelGroup> {
        self.open_since = None;
        let g = self.undo.pop()?;
        self.redo.push(g.clone());
        Some(g)
    }

    fn redo(&mut self) -> Option<ModelGroup> {
        self.open_since = None;
        let g = self.redo.pop()?;
        self.undo.push(g.clone());
        Some(g)
    }
}

// ---------------------------------------------------------------------------
// A session: buffer, history, oracle text, cursor, clock, and the model.

/// What one step did.
#[derive(Debug)]
enum Outcome {
    /// Nothing was recorded (a move, a seal, or a removal with nothing to
    /// remove).
    Nothing,
    /// An edit was recorded; `new_group` is the model's verdict.
    Recorded { new_group: bool },
    /// Undo or redo ran: what the history returned and what the model
    /// expected.
    Stepped {
        got: Option<Selections>,
        expected: Option<ModelGroup>,
    },
}

struct Session<B: Backend> {
    buf: B,
    text: String,
    hist: History,
    model: Model,
    now: Instant,
    cursor: usize,
    tag: usize,
}

impl<B: Backend> Session<B> {
    fn new(initial: &str) -> Self {
        Self {
            buf: B::build(initial),
            text: initial.to_owned(),
            hist: History::new(),
            model: Model::default(),
            now: Instant::now(),
            cursor: initial.len(),
            tag: 0,
        }
    }

    fn buffer_text(&self) -> String {
        self.buf.slice(0..usize::MAX).into_owned()
    }

    fn boundaries(&self) -> Vec<usize> {
        self.text
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(self.text.len()))
            .collect()
    }

    /// A selection with a fresh tag and `head` as the cursor.
    fn sel(&mut self, head: usize) -> Selections {
        self.tag += 1;
        Selections::single(Selection {
            anchor: TAG_BASE + self.tag,
            head,
        })
    }

    /// Apply a replacement to buffer and oracle; the buffer's record must
    /// match the oracle's. Returns the buffer's record, which is what the
    /// history keeps (for a mapped buffer, possibly with pieces in it).
    #[expect(clippy::unwrap_used, reason = "test helper")]
    fn replace(&mut self, range: Range<usize>, text: &str) -> Edit {
        let expected = Edit {
            start: range.start,
            removed: self.text[range.clone()].into(),
            inserted: text.into(),
        };
        let what = format!("replace({range:?}, {text:?}) on {:?}", self.text);
        let got = self.buf.replace(range.clone(), text);
        assert!(got.is_ok(), "{what}: {got:?}");
        let got = got.unwrap();
        same_edit::<B>(&got, &expected, &what);
        self.text.replace_range(range, text);
        got
    }

    /// Apply one edit and record it with `History::record`.
    fn single(&mut self, range: Range<usize>, text: &str, head_before: usize) -> Outcome {
        let before_text = self.text.clone();
        let before = self.sel(head_before);
        let edit = self.replace(range, text);
        self.cursor = edit.new_range().end;
        let after = self.sel(self.cursor);
        self.hist.record(edit.clone(), &before, &after, self.now);
        let step = ModelGroup {
            last_edit: edit,
            before_text,
            after_text: self.text.clone(),
            before,
            after,
        };
        let new_group = self.model.record(step, self.now);
        Outcome::Recorded { new_group }
    }

    fn step(&mut self, op: &Op, delay_ms: u64) -> Outcome {
        self.now += Duration::from_millis(delay_ms);
        let bounds = self.boundaries();
        // What undo and redo report, replayed onto the text before them.
        let mut mirror = self.text.clone();
        let outcome = match op {
            Op::Type(t) => {
                let c = self.cursor;
                self.single(c..c, t, c)
            }
            Op::Backspace => {
                let c = self.cursor;
                match self.text[..c].graphemes(true).next_back() {
                    Some(g) => self.single(c - g.len()..c, "", c),
                    None => Outcome::Nothing,
                }
            }
            Op::DeleteForward => {
                let c = self.cursor;
                match self.text[c..].graphemes(true).next() {
                    Some(g) => self.single(c..c + g.len(), "", c),
                    None => Outcome::Nothing,
                }
            }
            Op::Replace { a, b, text } => {
                let (x, y) = (*a.get(&bounds), *b.get(&bounds));
                self.single(x.min(y)..x.max(y), text, x.max(y))
            }
            Op::Group(parts) => self.group(&bounds, parts),
            Op::Move(at) => {
                self.cursor = *at.get(&bounds);
                Outcome::Nothing
            }
            Op::Seal => {
                self.hist.seal();
                self.model.open_since = None;
                Outcome::Nothing
            }
            Op::Undo => {
                let before = self.text.clone();
                let got = self.hist.undo(&mut self.buf, &mut replay_into(&mut mirror));
                let expected = self.model.undo();
                self.after_step(got, expected, &before)
            }
            Op::Redo => {
                let before = self.text.clone();
                let got = self.hist.redo(&mut self.buf, &mut replay_into(&mut mirror));
                let expected = self.model.redo();
                self.after_step(got, expected, &before)
            }
        };
        assert_eq!(self.buffer_text(), self.text, "buffer drifted from oracle");
        if matches!(op, Op::Undo | Op::Redo) {
            assert_eq!(
                mirror, self.text,
                "undo/redo reported edits that do not replay"
            );
        }
        outcome
    }

    /// Replace 1 to 4 non-overlapping ranges with `replace_sorted`, which
    /// applies them last first so earlier offsets stay valid, and record
    /// them in that application order.
    #[expect(clippy::unwrap_used, reason = "test helper")]
    fn group(&mut self, bounds: &[usize], parts: &[(Index, Index, String)]) -> Outcome {
        let mut ends: Vec<usize> = parts
            .iter()
            .flat_map(|(a, b, _)| [*a.get(bounds), *b.get(bounds)])
            .collect();
        ends.sort_unstable();
        let before_text = self.text.clone();
        let before = self.sel(self.cursor);
        let items: Vec<(Range<usize>, &str)> = parts
            .iter()
            .enumerate()
            .map(|(i, (_, _, t))| (ends[2 * i]..ends[2 * i + 1], t.as_str()))
            .collect();
        let got = self.buf.replace_sorted(&items);
        assert!(
            got.is_ok(),
            "replace_sorted({items:?}) on {before_text:?}: {got:?}"
        );
        let edits = got.unwrap();
        assert_eq!(edits.len(), items.len(), "one edit per item");
        for ((range, t), got) in items.iter().rev().zip(&edits) {
            let expected = Edit {
                start: range.start,
                removed: self.text[range.clone()].into(),
                inserted: (*t).into(),
            };
            same_edit::<B>(
                got,
                &expected,
                &format!("group item {range:?} on {before_text:?}"),
            );
            self.text.replace_range(range.clone(), t);
        }
        // Applied last, so its offsets hold in the final text.
        let Some(lowest) = edits.last().cloned() else {
            return Outcome::Nothing;
        };
        self.cursor = lowest.new_range().end;
        let after = self.sel(self.cursor);
        self.hist.record_group(edits, &before, &after);
        self.model.record_group(ModelGroup {
            last_edit: lowest,
            before_text,
            after_text: self.text.clone(),
            before,
            after,
        });
        Outcome::Recorded { new_group: true }
    }

    fn after_step(
        &mut self,
        got: Result<Option<Selections>, EditError>,
        expected: Option<ModelGroup>,
        before: &str,
    ) -> Outcome {
        assert!(got.is_ok(), "undo/redo failed on {before:?}: {got:?}");
        let got = got.ok().flatten();
        self.text = self.buffer_text();
        match &got {
            Some(s) => {
                let head = s.primary().head;
                assert!(
                    self.text.is_char_boundary(head),
                    "restored cursor {head} is not a boundary of {:?}",
                    self.text
                );
                self.cursor = head;
            }
            None => assert_eq!(self.text, before, "a no-op undo/redo changed the text"),
        }
        Outcome::Stepped { got, expected }
    }

    /// Undo until `undo` returns `None`; returns the text after each step.
    fn undo_all(&mut self) -> Vec<String> {
        let mut texts = Vec::new();
        while let Outcome::Stepped { got: Some(_), .. } = self.step(&Op::Undo, 0) {
            texts.push(self.text.clone());
        }
        assert!(!self.hist.can_undo(), "undo returned None with can_undo");
        texts
    }
}

/// Index of the last element of `states[..below]` equal to `text`.
/// Greedy latest-match is enough to test that a sequence of texts appears
/// in `states` in decreasing order.
fn latest_match(states: &[String], below: usize, text: &str) -> Option<usize> {
    states[..below].iter().rposition(|s| s == text)
}

// ---------------------------------------------------------------------------
// Properties.

/// Properties 1 and 2: after any edit sequence, undoing until `None`
/// restores the starting text through earlier states in reverse order, and
/// redoing until `None` walks the same states forwards to the final text.
fn undo_all_then_redo_all<B: Backend>(initial: &str, ops: &[(Op, u64)]) {
    let mut s = Session::<B>::new(initial);
    // The text after every recorded edit; undo can only land on these.
    let mut states = vec![initial.to_owned()];
    for (op, d) in ops {
        if let Outcome::Recorded { .. } = s.step(op, *d) {
            states.push(s.text.clone());
        }
    }
    let records = states.len() - 1;
    let final_text = s.text.clone();

    let undone = s.undo_all();
    let n = undone.len();
    let mut at = states.len() - 1;
    for (i, t) in undone.iter().enumerate() {
        let found = latest_match(&states, at, t);
        assert!(
            found.is_some(),
            "undo {} gave {t:?}, not an earlier state of {states:?}",
            i + 1
        );
        at = found.unwrap_or_default();
    }
    assert_eq!(s.text, initial, "undo-all must restore the initial text");
    assert!(n <= records, "{n} undo steps for {records} recorded edits");
    if records > 0 {
        assert!(n >= 1, "edits happened but nothing undid");
    }
    assert_eq!(s.hist.can_redo(), n > 0, "can_redo after undo-all");

    // Redo j must give back the text before undo n-j+1.
    let mut walk: Vec<&str> = vec![&final_text];
    walk.extend(undone.iter().map(String::as_str));
    for j in 1..=n {
        let r = s.step(&Op::Redo, 0);
        assert!(
            matches!(r, Outcome::Stepped { got: Some(_), .. }),
            "redo {j} of {n}: {r:?}"
        );
        assert_eq!(s.text, walk[n - j], "text after redo {j} of {n}");
    }
    let r = s.step(&Op::Redo, 0);
    assert!(
        matches!(r, Outcome::Stepped { got: None, .. }),
        "redo past the end: {r:?}"
    );
    assert_eq!(s.text, final_text, "redo-all must restore the final text");
}

/// Property 3: a random mix of edits, undos and redos, checked without any
/// knowledge of grouping. `line` is the current line of history: every text
/// recorded since the session began, minus branches abandoned by editing
/// after an undo. Undo must move to an earlier entry of `line`; redo must
/// return to exactly where its undo started; a new edit forgets the redo.
fn interleaved<B: Backend>(initial: &str, ops: &[(Op, u64)]) {
    let mut s = Session::<B>::new(initial);
    let mut line = vec![initial.to_owned()];
    let mut pos = 0;
    let mut redo_to: Vec<usize> = Vec::new();
    for (i, (op, d)) in ops.iter().enumerate() {
        let before = s.text.clone();
        match s.step(op, *d) {
            Outcome::Nothing => {}
            Outcome::Recorded { .. } => {
                line.truncate(pos + 1);
                line.push(s.text.clone());
                pos = line.len() - 1;
                redo_to.clear();
                assert!(!s.hist.can_redo(), "step {i}: an edit must clear redo");
            }
            Outcome::Stepped { got: Some(_), .. } if matches!(op, Op::Undo) => {
                redo_to.push(pos);
                let found = latest_match(&line, pos, &s.text);
                assert!(
                    found.is_some(),
                    "step {i}: undo from {before:?} gave {:?}, not an earlier state of {line:?}",
                    s.text
                );
                pos = found.unwrap_or_default();
            }
            Outcome::Stepped { got: Some(_), .. } => {
                let to = redo_to.pop();
                assert!(
                    to.is_some(),
                    "step {i}: redo gave {:?} but nothing was undone",
                    s.text
                );
                let to = to.unwrap_or_default();
                assert_eq!(s.text, line[to], "step {i}: redo must undo its undo");
                pos = to;
            }
            Outcome::Stepped { got: None, .. } if matches!(op, Op::Undo) => {
                assert_eq!(
                    s.text, initial,
                    "step {i}: nothing to undo but text differs"
                );
                let found = latest_match(&line, pos + 1, &s.text);
                assert!(found.is_some(), "step {i}: initial text not on {line:?}");
                pos = found.unwrap_or_default();
            }
            Outcome::Stepped { got: None, .. } => {
                assert!(
                    redo_to.is_empty(),
                    "step {i}: redo returned None, expected {:?}",
                    redo_to.last().map(|&p| &line[p])
                );
            }
        }
    }
    let before_undo_all = s.text.clone();
    let undone = s.undo_all();
    assert_eq!(s.text, initial, "undo-all must restore the initial text");
    for _ in &undone {
        assert!(matches!(
            s.step(&Op::Redo, 0),
            Outcome::Stepped { got: Some(_), .. }
        ));
    }
    assert_eq!(s.text, before_undo_all, "redo-all after undo-all");
    // A new edit after undoing makes redo return None.
    if !undone.is_empty() {
        s.step(&Op::Undo, 0);
        s.step(&Op::Type("z".to_owned()), 0);
        assert!(matches!(
            s.step(&Op::Redo, 0),
            Outcome::Stepped { got: None, .. }
        ));
    }
}

/// Property 4: an independent model of the grouping rules agrees with the
/// history on every step. `top_id` changes exactly when the model starts a
/// group; every undo returns the selections from before the group's first
/// edit and restores the text from before it; every redo returns the
/// selections after the group's last edit; and undo-all takes exactly as
/// many steps as the model has groups.
fn grouping_matches_model<B: Backend>(initial: &str, ops: &[(Op, u64)]) {
    let mut s = Session::<B>::new(initial);
    for (i, (op, d)) in ops.iter().enumerate() {
        let top = s.hist.top_id();
        match s.step(op, *d) {
            Outcome::Nothing => {}
            Outcome::Recorded { new_group } => {
                let changed = s.hist.top_id() != top;
                assert_eq!(
                    changed, new_group,
                    "step {i} {op:?} (+{d} ms): history started a new group = {changed}, model = {new_group}"
                );
            }
            Outcome::Stepped { got, expected } => {
                assert_eq!(
                    got.is_some(),
                    expected.is_some(),
                    "step {i} {op:?}: history returned {got:?}, model expected {expected:?}"
                );
                if let (Some(sel), Some(g)) = (got, expected) {
                    let (want_sel, want_text) = if matches!(op, Op::Undo) {
                        (g.before, g.before_text)
                    } else {
                        (g.after, g.after_text)
                    };
                    assert_eq!(sel, want_sel, "step {i} {op:?}: selections");
                    assert_eq!(s.text, want_text, "step {i} {op:?}: text");
                }
            }
        }
    }
    let groups = s.model.undo.len();
    let undone = s.undo_all();
    assert_eq!(undone.len(), groups, "undo steps vs model groups");
    assert_eq!(s.text, initial);
}

/// Property 5: after `seal()` the saved `top_id` is left by any further
/// edit, and undoing exactly the steps made since brings it back, along
/// with the saved text.
fn top_id_tracks_save_point<B: Backend>(initial: &str, prefix: &[(Op, u64)], after: &[(Op, u64)]) {
    let mut s = Session::<B>::new(initial);
    for (op, d) in prefix {
        s.step(op, *d);
    }
    s.hist.seal();
    s.model.open_since = None;
    let saved = s.hist.top_id();
    let saved_text = s.text.clone();
    let mut records = 0;
    for (i, (op, d)) in after.iter().enumerate() {
        if let Outcome::Recorded { .. } = s.step(op, *d) {
            records += 1;
        }
        if records > 0 {
            assert_ne!(
                s.hist.top_id(),
                saved,
                "edit {i} after the save left top_id at saved"
            );
        }
    }
    let final_top = s.hist.top_id();
    let mut undos = 0;
    while s.hist.top_id() != saved {
        assert!(
            undos < records,
            "{records} edits since the save, but {undos} undos did not reach it"
        );
        assert!(matches!(
            s.step(&Op::Undo, 0),
            Outcome::Stepped { got: Some(_), .. }
        ));
        undos += 1;
    }
    assert_eq!(s.text, saved_text, "text at the save point");
    assert_eq!(undos > 0, records > 0, "undos {undos} for {records} edits");
    for _ in 0..undos {
        s.step(&Op::Redo, 0);
    }
    assert_eq!(s.hist.top_id(), final_top, "redo back to the newest step");
}

proptest! {
    #![proptest_config(config(256))]

    #[test]
    fn undo_all_restores_initial_and_redo_all_restores_final(
        initial in text(12),
        ops in steps(edit_op(), 40),
    ) {
        undo_all_then_redo_all::<RopeBuffer>(&initial, &ops);
        undo_all_then_redo_all::<MappedBuffer>(&initial, &ops);
    }

    #[test]
    fn interleaved_undo_redo_follow_the_state_stack(
        initial in text(12),
        ops in steps(any_op(), 40),
    ) {
        interleaved::<RopeBuffer>(&initial, &ops);
        interleaved::<MappedBuffer>(&initial, &ops);
    }

    #[test]
    fn undo_restores_each_groups_first_selections(
        initial in text(12),
        ops in steps(any_op(), 40),
    ) {
        grouping_matches_model::<RopeBuffer>(&initial, &ops);
        grouping_matches_model::<MappedBuffer>(&initial, &ops);
    }

    #[test]
    fn top_id_returns_to_the_save_point(
        initial in text(12),
        prefix in steps(any_op(), 20),
        after in steps(edit_op(), 20),
    ) {
        top_id_tracks_save_point::<RopeBuffer>(&initial, &prefix, &after);
        top_id_tracks_save_point::<MappedBuffer>(&initial, &prefix, &after);
    }
}
