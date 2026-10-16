//! Property tests: a syntax tree kept up to date incrementally through
//! [`SyntaxState`] must be exactly the tree a plain incremental parse of a
//! `String` gives (the `Reference` below), and, when the text has no syntax
//! errors, the tree a fresh parse gives (DESIGN.md §4.2, M5). With errors,
//! tree-sitter may recover differently incrementally than from scratch, so
//! a fresh parse is only an oracle for error-free text.
//!
//! The fresh-parse oracle is a new `tree_sitter::Parser` that parses the
//! whole text from a plain `String`, with no old tree to reuse. Incremental parsing is
//! only correct if every edit was described to tree-sitter exactly: the
//! byte offsets *and* the (row, column) points of its start, old end and new
//! end. A wrong point does not always change the shape of the tree, so the
//! s-expression alone is not enough: the two trees are walked in lockstep
//! and every node's kind, field, byte range and point range are compared,
//! and each node's points are also checked against positions computed from
//! the oracle text by counting `\n`s.
//!
//! Edits go through a [`RopeBuffer`] and [`SyntaxState::edit`], then reach
//! the tree by every route the editor uses:
//! - a reparse after each edit, or after several (batched);
//! - edits made while the first parse runs on a "worker" (here: later on
//!   the same thread), which `finish` replays into the arriving tree;
//! - a reparse that runs over budget (`Duration::ZERO`) and moves to a job,
//!   with more edits made before the job's result comes back.
//!
//! The generated text is Rust-like (braces, strings, comments, `let`s) with
//! random printable ASCII, unbalanced quotes and comment openers mixed in,
//! so many parses contain `ERROR` and `MISSING` nodes. It also mixes `\r\n`,
//! a lone `\r`, multi-byte characters (columns count bytes, not chars), an
//! emoji ZWJ sequence and a combining accent. Edits land on char boundaries,
//! including offset 0 and the end, and some deletes span line breaks.

#![cfg(feature = "rust")]

use std::ops::Range;
use std::time::Duration;

use nimbus_core::{Buffer, RopeBuffer, Text};
use nimbus_syntax::LanguageId;
use nimbus_syntax::state::{Job, SyntaxState};
use proptest::prelude::*;
use proptest::sample::Index;
use proptest::test_runner::Config;
use tree_sitter::{InputEdit, Node, Parser, Point, Tree};

/// Building blocks for generated text. Line endings appear twice to make
/// multi-line texts common; lone delimiters make unbalanced input.
const PIECES: &[&str] = &[
    "fn f() {",
    "fn g(x: u8) -> u8 { x }",
    "}",
    "{",
    "(",
    ")",
    "let x = 1;",
    "struct S;",
    "\"str\"",
    "\"",
    "/* c */",
    "/*",
    "*/",
    "// c\n",
    "'a'",
    "'",
    "x",
    "é",
    "日本",
    "👩\u{200d}💻",
    "e\u{301}",
    "\n",
    "\n",
    "\r\n",
    "\r\n",
    "\r",
    "    ",
    "\t",
    "",
];

/// Whole, valid Rust items, so some texts parse without errors and the
/// trees compared are realistic, not only error recovery.
const ITEMS: &[&str] = &[
    "fn f() {\n    let x = 1;\n}\n",
    "fn g(a: u8) -> u8 {\r\n    a + 1\r\n}\r\n",
    "struct S {\n    s: &'static str,\n}\n",
    "const C: &str = \"日本 é\";\n",
    "static E: char = 'é';\n",
    "// comment ü 👩\u{200d}💻\n",
    "/* block\n   comment */\n",
    "enum E { A, B }\n",
    "impl S {\n    fn m(&self) {}\n}\n",
    "use std::io;\n",
    "\n",
];

/// Inserts biased towards line structure and multi-byte text, for the
/// property that targets point arithmetic.
const LINEY: &[&str] = &[
    "\n",
    "\r\n",
    "\n\n",
    "a\r\nb",
    "日本\n",
    "\né",
    "x\n    y",
    "// c\r\n",
    "/* a\nb */",
    "\"a\nb\"",
    "👩\u{200d}💻\n",
    "",
];

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

/// A sequence of valid items: error-free Rust.
fn valid_text(max_items: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(ITEMS), 0..max_items).prop_map(|v| v.concat())
}

/// A starting text: valid Rust or a jumble of pieces.
fn initial() -> impl Strategy<Value = String> {
    prop_oneof![text(24), valid_text(8)]
}

/// Text a few kilobytes long, so a reparse is big enough for a zero budget
/// to run out and hand the parse to a job.
fn long_text() -> impl Strategy<Value = String> {
    prop_oneof![
        (text(16), 20usize..80).prop_map(|(t, n)| t.repeat(n)),
        (valid_text(8), 10usize..60).prop_map(|(t, n)| t.repeat(n)),
    ]
}

/// What an edit inserts: a few pieces, or a whole item.
fn insert() -> BoxedStrategy<String> {
    prop_oneof![
        3 => text(6),
        1 => prop::sample::select(ITEMS).prop_map(str::to_owned),
    ]
    .boxed()
}

fn liney() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => prop::sample::select(LINEY).prop_map(str::to_owned),
        1 => text(4),
    ]
}

/// Where an edit endpoint lands: the very start, the very end, or any char
/// boundary in between.
#[derive(Debug, Clone)]
enum Pos {
    Start,
    End,
    At(Index),
}

impl Pos {
    fn resolve(&self, s: &str) -> usize {
        match self {
            Pos::Start => 0,
            Pos::End => s.len(),
            Pos::At(i) => *i.get(&boundaries(s)),
        }
    }
}

fn pos() -> impl Strategy<Value = Pos> {
    prop_oneof![
        1 => Just(Pos::Start),
        1 => Just(Pos::End),
        6 => any::<Index>().prop_map(Pos::At),
    ]
}

/// One edit. Positions are resolved against the oracle when it runs.
#[derive(Debug, Clone)]
enum Op {
    Insert {
        at: Pos,
        text: String,
    },
    Delete {
        a: Pos,
        b: Pos,
    },
    Replace {
        a: Pos,
        b: Pos,
        text: String,
    },
    /// Replace a range that contains a `\n`: from a boundary at or before
    /// the chosen line break to one after it.
    AcrossBreak {
        nl: Index,
        left: Index,
        right: Index,
        text: String,
    },
}

impl Op {
    /// The range and replacement this op means for `s`, or `None` if it
    /// cannot apply (a line-spanning edit in a text with no line break).
    fn resolve(&self, s: &str) -> Option<(Range<usize>, String)> {
        let span = |a: &Pos, b: &Pos| {
            let (x, y) = (a.resolve(s), b.resolve(s));
            x.min(y)..x.max(y)
        };
        match self {
            Op::Insert { at, text } => {
                let p = at.resolve(s);
                Some((p..p, text.clone()))
            }
            Op::Delete { a, b } => Some((span(a, b), String::new())),
            Op::Replace { a, b, text } => Some((span(a, b), text.clone())),
            Op::AcrossBreak {
                nl,
                left,
                right,
                text,
            } => {
                let breaks: Vec<usize> = s.match_indices('\n').map(|(i, _)| i).collect();
                if breaks.is_empty() {
                    return None;
                }
                let at = *nl.get(&breaks);
                let bounds = boundaries(s);
                let before: Vec<usize> = bounds.iter().copied().filter(|&b| b <= at).collect();
                let after: Vec<usize> = bounds.iter().copied().filter(|&b| b > at).collect();
                Some((*left.get(&before)..*right.get(&after), text.clone()))
            }
        }
    }
}

fn op_with(insert: BoxedStrategy<String>) -> BoxedStrategy<Op> {
    prop_oneof![
        4 => (pos(), insert.clone()).prop_map(|(at, text)| Op::Insert { at, text }),
        3 => (pos(), pos()).prop_map(|(a, b)| Op::Delete { a, b }),
        3 => (pos(), pos(), insert.clone()).prop_map(|(a, b, text)| Op::Replace { a, b, text }),
        2 => (any::<Index>(), any::<Index>(), any::<Index>(), insert)
            .prop_map(|(nl, left, right, text)| Op::AcrossBreak { nl, left, right, text }),
    ]
    .boxed()
}

fn op() -> BoxedStrategy<Op> {
    op_with(insert())
}

/// Edits whose inserts are mostly line breaks and multi-byte text, with
/// half the endpoints at the start or end of the text.
fn edge_op() -> BoxedStrategy<Op> {
    let end = || {
        prop_oneof![
            1 => Just(Pos::Start),
            1 => Just(Pos::End),
            2 => any::<Index>().prop_map(Pos::At),
        ]
    };
    prop_oneof![
        3 => (end(), liney()).prop_map(|(at, text)| Op::Insert { at, text }),
        2 => (end(), end()).prop_map(|(a, b)| Op::Delete { a, b }),
        2 => (end(), end(), liney()).prop_map(|(a, b, text)| Op::Replace { a, b, text }),
        3 => (any::<Index>(), any::<Index>(), any::<Index>(), liney())
            .prop_map(|(nl, left, right, text)| Op::AcrossBreak { nl, left, right, text }),
    ]
    .boxed()
}

/// One step of a session that exercises every route to the tree.
#[derive(Debug, Clone)]
enum Step {
    Edit(Op),
    /// `refresh` with no time limit.
    Refresh,
    /// `refresh` with a zero budget, which may hand the parse to a job.
    RefreshZero,
    /// Run the outstanding job, if any, and pass its result to `finish`
    /// with no limit (`false`) or a zero budget (`true`).
    Finish(bool),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        6 => op().prop_map(Step::Edit),
        1 => Just(Step::Refresh),
        2 => Just(Step::RefreshZero),
        2 => any::<bool>().prop_map(Step::Finish),
    ]
}

// ---------------------------------------------------------------------------
// Oracle helpers.

/// Every valid edit position, including the end.
fn boundaries(s: &str) -> Vec<usize> {
    s.char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(s.len()))
        .collect()
}

/// Positions in `s` computed the slow, obvious way, for checking the
/// points tree-sitter reports.
struct Points {
    /// Byte offset where each row starts: 0, then just past each `\n`.
    line_starts: Vec<usize>,
    len: usize,
}

impl Points {
    fn new(s: &str) -> Self {
        let line_starts = std::iter::once(0)
            .chain(s.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        Self {
            line_starts,
            len: s.len(),
        }
    }

    /// The point of `byte`: its row is the number of `\n`s before it, its
    /// column the bytes since the last one. `None` past the end.
    fn at(&self, byte: usize) -> Option<Point> {
        if byte > self.len {
            return None;
        }
        let row = self.line_starts.partition_point(|&start| start <= byte) - 1;
        Some(Point {
            row,
            column: byte - self.line_starts[row],
        })
    }
}

/// Visit every node of `tree` in document order.
fn each_node(tree: &Tree, mut f: impl FnMut(Node<'_>)) {
    let mut c = tree.walk();
    loop {
        f(c.node());
        if c.goto_first_child() {
            continue;
        }
        loop {
            if c.goto_next_sibling() {
                break;
            }
            if !c.goto_parent() {
                return;
            }
        }
    }
}

/// Every node's points must agree with its bytes in `src`. Checked on the
/// edited tree before it is reparsed: `Tree::edit` shifts nodes using the
/// points it is given, and a reparse recomputes the edited region from the
/// text, so this is where a wrong point from the `InputEdit` conversion
/// shows. It matters because that edited tree colours the view while a
/// reparse runs on a worker.
fn assert_points_match(tree: &Tree, src: &str, context: &str) {
    let points = Points::new(src);
    each_node(tree, |n| {
        assert_eq!(
            Some(n.start_position()),
            points.at(n.start_byte()),
            "start of {} {:?} {context} in {src:?}",
            n.kind(),
            n.byte_range()
        );
        assert_eq!(
            Some(n.end_position()),
            points.at(n.end_byte()),
            "end of {} {:?} {context} in {src:?}",
            n.kind(),
            n.byte_range()
        );
    });
}

/// Everything compared about a node.
#[derive(Debug, PartialEq, Eq)]
struct NodeInfo<'t> {
    kind: &'t str,
    field: Option<&'t str>,
    named: bool,
    missing: bool,
    extra: bool,
    bytes: Range<usize>,
    start: Point,
    end: Point,
    children: u32,
}

fn info<'t>(node: Node<'t>, field: Option<&'t str>) -> NodeInfo<'t> {
    NodeInfo {
        kind: node.kind(),
        field,
        named: node.is_named(),
        missing: node.is_missing(),
        extra: node.is_extra(),
        bytes: node.byte_range(),
        start: node.start_position(),
        end: node.end_position(),
        children: node.child_count(),
    }
}

/// Walk `got` and `want` in lockstep and require identical nodes. `what`
/// names the tree `want` is, for failure messages.
fn assert_same_tree(got: &Tree, want: &Tree, src: &str, what: &str) {
    assert_eq!(
        got.root_node().to_sexp(),
        want.root_node().to_sexp(),
        "s-expression vs {what} of {src:?}"
    );
    let mut g = got.walk();
    let mut w = want.walk();
    let mut path: Vec<usize> = vec![0];
    loop {
        let (gn, wn) = (g.node(), w.node());
        assert_eq!(
            info(gn, g.field_name()),
            info(wn, w.field_name()),
            "node at child path {path:?} vs {what} of {src:?}"
        );
        let down = g.goto_first_child();
        assert_eq!(down, w.goto_first_child(), "children at {path:?}");
        if down {
            path.push(0);
            continue;
        }
        loop {
            let next = g.goto_next_sibling();
            assert_eq!(next, w.goto_next_sibling(), "siblings at {path:?}");
            if next {
                if let Some(last) = path.last_mut() {
                    *last += 1;
                }
                break;
            }
            let up = g.goto_parent();
            assert_eq!(up, w.goto_parent(), "parent of {path:?}");
            path.pop();
            if !up {
                return;
            }
        }
    }
}

#[expect(clippy::unwrap_used, reason = "test helper")]
fn rust_parser() -> Parser {
    let mut p = Parser::new();
    p.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
    p
}

// ---------------------------------------------------------------------------
// The reference: incremental parsing done the obvious way.

/// Incremental parsing of a plain `String`, with each [`InputEdit`] worked
/// out from the text before and after the edit (not by `edit.rs`), and a
/// reparse at the same moments the session's tree gets one.
///
/// Why it is needed: when the text has syntax errors, tree-sitter's error
/// recovery depends on which old subtrees it can reuse, so an incremental
/// parse of broken text may legitimately recover differently from a fresh
/// parse (tree-sitter only promises the same tree as a fresh parse for
/// error-free input). Parsing is deterministic, though, so for the same old
/// tree, edits and text it must give exactly the same tree. That makes this
/// an exact oracle for every input, broken or not.
///
/// The session's routes all reduce to this schedule. A job parses a
/// snapshot with the edited old tree, which is the same as reparsing at the
/// moment the job was made; edits made while it runs are applied to its
/// result in order, the same as editing the reference tree as they happen;
/// and `finish` reparses once if there were any.
struct Reference {
    parser: Parser,
    tree: Option<Tree>,
    dirty: bool,
}

impl Reference {
    #[expect(clippy::unwrap_used, reason = "test helper")]
    fn parse(&mut self, text: &str) {
        self.tree = Some(self.parser.parse(text, self.tree.as_ref()).unwrap());
        self.dirty = false;
    }

    /// Describe replacing `range` of `before` to give `after`.
    #[expect(clippy::unwrap_used, reason = "test helper")]
    fn edit(&mut self, before: &str, after: &str, range: &Range<usize>, inserted: usize) {
        let (old, new) = (Points::new(before), Points::new(after));
        let ie = InputEdit {
            start_byte: range.start,
            old_end_byte: range.end,
            new_end_byte: range.start + inserted,
            start_position: old.at(range.start).unwrap(),
            old_end_position: old.at(range.end).unwrap(),
            new_end_position: new.at(range.start + inserted).unwrap(),
        };
        if let Some(tree) = &mut self.tree {
            tree.edit(&ie);
            self.dirty = true;
        }
    }

    fn refresh(&mut self, text: &str) {
        if self.dirty {
            self.parse(text);
        }
    }
}

// ---------------------------------------------------------------------------
// The session under test.

/// A buffer, its oracle string, its syntax state, the job (if any) that a
/// worker would be running, and the reference following the same schedule.
struct Session {
    buf: RopeBuffer,
    oracle: String,
    state: SyntaxState,
    job: Option<Job>,
    reference: Reference,
    fresh: Parser,
}

impl Session {
    fn new(initial: &str) -> Self {
        let buf = RopeBuffer::from_text(initial);
        let state = SyntaxState::new(Some(LanguageId::Rust), buf.len_bytes());
        let mut reference = Reference {
            parser: rust_parser(),
            tree: None,
            dirty: false,
        };
        reference.parse(initial);
        Self {
            buf,
            oracle: initial.to_owned(),
            state,
            job: None,
            reference,
            fresh: rust_parser(),
        }
    }

    /// Parse to completion now, so the session starts in the Ready state.
    fn ready(initial: &str) -> Self {
        let mut s = Self::new(initial);
        s.state.parse_now(&s.buf);
        s
    }

    /// Start the first parse as a job and leave it outstanding.
    fn first_parse_pending(initial: &str) -> Self {
        let mut s = Self::new(initial);
        s.job = s.state.take_job(&s.buf);
        assert!(s.job.is_some(), "a known language starts a first parse");
        s
    }

    #[expect(clippy::unwrap_used, reason = "test helper")]
    fn edit(&mut self, op: &Op) {
        let Some((range, text)) = op.resolve(&self.oracle) else {
            return;
        };
        let edit = self.buf.replace(range.clone(), &text).unwrap();
        let before = self.oracle.clone();
        self.oracle.replace_range(range.clone(), &text);
        assert_eq!(self.buf.len_bytes(), self.oracle.len(), "buffer length");
        self.state.edit(&self.buf, &edit);
        self.reference
            .edit(&before, &self.oracle, &range, text.len());
        if let Some((tree, _)) = self.state.tree() {
            let context = format!("after {edit:?}, before reparse");
            assert_points_match(tree, &self.oracle, &context);
        }
    }

    fn refresh(&mut self, budget: Duration) {
        let job = self.state.refresh(&self.buf, budget);
        if self.job.is_some() {
            assert!(job.is_none(), "no second job while one is outstanding");
        } else {
            // Inline or handed to a job, the parse is of this text.
            self.reference.refresh(&self.oracle);
            self.job = job;
        }
    }

    fn finish(&mut self, budget: Duration) {
        if let Some(job) = self.job.take() {
            let result = job.run();
            assert!(self.state.waiting_for(result.id()), "result is current");
            self.job = self.state.finish(result, &self.buf, budget);
            self.reference.refresh(&self.oracle);
        }
    }

    fn step(&mut self, step: &Step) {
        match step {
            Step::Edit(op) => self.edit(op),
            Step::Refresh => self.refresh(Duration::MAX),
            Step::RefreshZero => self.refresh(Duration::ZERO),
            Step::Finish(zero) => self.finish(if *zero { Duration::ZERO } else { Duration::MAX }),
        }
    }

    /// Run outstanding jobs and reparse with no limit until nothing is
    /// left to do, as the app does over the following frames. With no
    /// limit a finish or refresh completes, so two rounds are enough; a few
    /// more are allowed before calling it a hang.
    fn settle(&mut self) {
        let mut rounds = 0;
        loop {
            if self.job.is_some() {
                self.finish(Duration::MAX);
            } else {
                self.refresh(Duration::MAX);
                if self.job.is_none() {
                    return;
                }
            }
            rounds += 1;
            assert!(rounds < 16, "parse did not settle");
        }
    }

    /// The state's tree must be exactly the reference's, have points that
    /// agree with the text, and, when the text has no syntax errors, equal
    /// a fresh parse.
    #[expect(clippy::unwrap_used, reason = "test helper")]
    fn check(&mut self) {
        assert_eq!(self.job.as_ref().map(|_| ()), None, "check after settling");
        assert_eq!(
            self.buf.slice(0..self.buf.len_bytes()),
            self.oracle,
            "buffer text"
        );
        let (got, _) = self.state.tree().unwrap();
        assert_points_match(got, &self.oracle, "after reparse");
        let reference = self.reference.tree.as_ref().unwrap();
        assert_same_tree(got, reference, &self.oracle, "reference");
        let fresh = self.fresh.parse(&self.oracle, None).unwrap();
        if !fresh.root_node().has_error() {
            assert_same_tree(got, &fresh, &self.oracle, "fresh parse");
        }
    }
}

proptest! {
    #![proptest_config(config(128))]

    /// A reparse after every edit keeps the tree equal to a fresh parse.
    #[test]
    fn reparse_after_each_edit_matches_fresh_parse(
        initial in initial(),
        ops in prop::collection::vec(op(), 0..24),
    ) {
        let mut s = Session::ready(&initial);
        s.check();
        for op in &ops {
            s.edit(op);
            s.refresh(Duration::MAX);
            s.check();
        }
    }

    /// Several edits recorded before one reparse are applied together, as
    /// when the user types faster than frames are drawn.
    #[test]
    fn batched_reparses_match_fresh_parse(
        initial in initial(),
        batches in prop::collection::vec(prop::collection::vec(op(), 1..8), 1..6),
    ) {
        let mut s = Session::ready(&initial);
        for batch in &batches {
            for op in batch {
                s.edit(op);
            }
            s.refresh(Duration::MAX);
            s.check();
        }
    }

    /// Edits made while the first parse is outstanding are replayed into
    /// its tree when it arrives, then later edits go the normal way.
    #[test]
    fn edits_during_first_parse_are_replayed(
        initial in initial(),
        during in prop::collection::vec(op(), 0..12),
        after in prop::collection::vec(op(), 0..8),
    ) {
        let mut s = Session::first_parse_pending(&initial);
        for op in &during {
            s.edit(op);
        }
        s.finish(Duration::MAX);
        s.check();
        for op in &after {
            s.edit(op);
            s.refresh(Duration::MAX);
            s.check();
        }
    }

    /// Edits concentrated on line breaks, multi-byte characters and the two
    /// ends of the text, where point arithmetic goes wrong.
    #[test]
    fn line_break_and_multibyte_edits_match_fresh_parse(
        initial in initial(),
        ops in prop::collection::vec(edge_op(), 1..24),
        batched in any::<bool>(),
    ) {
        let mut s = Session::ready(&initial);
        for op in &ops {
            s.edit(op);
            if !batched {
                s.refresh(Duration::MAX);
                s.check();
            }
        }
        s.settle();
        s.check();
    }
}

proptest! {
    #![proptest_config(config(128))]

    /// Zero-budget reparses that move to a job, with edits made while the
    /// job runs, first parses that are still outstanding, and finishes that
    /// run over budget again: once everything settles the tree matches.
    #[test]
    fn over_budget_reparses_match_fresh_parse(
        initial in long_text(),
        first_pending in any::<bool>(),
        steps in prop::collection::vec(step(), 0..32),
    ) {
        let mut s = if first_pending {
            Session::first_parse_pending(&initial)
        } else {
            Session::ready(&initial)
        };
        for step in &steps {
            s.step(step);
            // A full-budget refresh with nothing outstanding leaves the tree
            // up to date, so it can be compared mid-session.
            if matches!(step, Step::Refresh) && s.job.is_none() {
                s.check();
            }
        }
        s.settle();
        s.check();
    }
}
