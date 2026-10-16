//! A document's syntax tree and how it is kept up to date.
//!
//! The first parse of a file can take tens of milliseconds, so it runs on a
//! worker against a snapshot ([`Job`]). Edits made while it runs are queued
//! and replayed into the tree when it arrives. After that, each frame
//! reparses incrementally on the main thread, which usually takes
//! microseconds, within a time budget; if a reparse runs over (opening a
//! comment in a big file re-lexes everything after it), the rest moves to a
//! worker, and the edited old tree keeps the old colours on screen.
//!
//! This crate starts no threads: it hands out jobs, and the app runs them
//! and passes the results back.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use nimbus_core::{Buffer, Edit, Snapshot, Text};
use tree_sitter::{InputEdit, ParseOptions, ParseState, Parser, Tree};

use crate::LanguageId;
use crate::edit::{input_edit, point_at};
use crate::lexical::{self, Lexical, Rules};
use crate::registry::{self, Grammar};
use crate::text::bytes_from;

/// Files larger than this are not parsed; they get the simpler line-by-line
/// highlighting in [`crate::lexical`] instead (DESIGN.md §4.2).
pub const MAX_HIGHLIGHT_BYTES: usize = 16 * 1024 * 1024;

static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

/// Why a document has no highlighting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Off {
    /// Not a language Nimbus knows.
    NoLanguage,
    /// The grammar failed to load or the parse failed.
    Failed,
}

enum Phase {
    Off(Off),
    /// Larger than [`MAX_HIGHLIGHT_BYTES`], a user-defined language, or a
    /// mapped document: line-by-line rules, no tree.
    Lexical(Lexical),
    /// The language is known; no parse has started.
    Waiting,
    /// A job is running. Edits since it started wait in `pending`; `stale`
    /// is an older tree (kept edited) to draw colours from meanwhile.
    Parsing {
        job: u64,
        pending: Vec<InputEdit>,
        stale: Option<(Tree, &'static Grammar)>,
    },
    /// The tree matches the text, or will after a reparse if `dirty`.
    Ready {
        tree: Tree,
        parser: Parser,
        grammar: &'static Grammar,
        dirty: bool,
    },
}

/// Work to run on another thread: a parse of a snapshot.
pub struct Job {
    id: u64,
    language: LanguageId,
    snapshot: Box<dyn Snapshot>,
    parser: Option<Parser>,
    old: Option<Tree>,
    cancel: Arc<AtomicBool>,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("id", &self.id)
            .field("language", &self.language)
            .finish_non_exhaustive()
    }
}

/// What a job produced: a tree for the snapshot, or nothing if it failed or
/// was cancelled.
pub struct JobResult {
    id: u64,
    outcome: Option<(Tree, Parser, &'static Grammar)>,
}

impl std::fmt::Debug for JobResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobResult")
            .field("id", &self.id)
            .field("ok", &self.outcome.is_some())
            .finish()
    }
}

impl JobResult {
    /// The job this result belongs to.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }
}

impl Job {
    /// Parse the snapshot. Loads the grammar first if needed, which may
    /// compile its query; that is why this runs on a worker.
    #[must_use]
    pub fn run(self) -> JobResult {
        let Self {
            id,
            language,
            snapshot,
            parser,
            old,
            cancel,
        } = self;
        let started = Instant::now();
        let outcome = (|| {
            let grammar = registry::grammar(language)?;
            let mut parser = if let Some(p) = parser {
                p
            } else {
                let mut p = Parser::new();
                p.set_language(&grammar.language).ok()?;
                p
            };
            let text: &dyn Text = &*snapshot;
            let mut stop = |_: &ParseState| {
                if cancel.load(Ordering::Relaxed) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            };
            let options = ParseOptions::new().progress_callback(&mut stop);
            let tree = parser.parse_with_options(
                &mut |i, _| bytes_from(text, i),
                old.as_ref(),
                Some(options),
            )?;
            Some((tree, parser, grammar))
        })();
        log::debug!(
            "syntax: {} parse job {id} took {:?}",
            language.name(),
            started.elapsed()
        );
        JobResult { id, outcome }
    }
}

/// A document's syntax state.
pub struct SyntaxState {
    language: Option<LanguageId>,
    phase: Phase,
    /// Keep the simple rules even when the text is small enough to parse:
    /// set by [`SyntaxState::reset_lexical`] for a mapped document.
    lexical_only: bool,
    /// Set when the document goes away or is reset, so a running job stops.
    cancel: Arc<AtomicBool>,
    /// Increases whenever highlighting may have changed; highlight caches
    /// key on it.
    rev: u64,
}

impl std::fmt::Debug for SyntaxState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyntaxState")
            .field("language", &self.language)
            .field("rev", &self.rev)
            .finish_non_exhaustive()
    }
}

impl Default for SyntaxState {
    fn default() -> Self {
        Self::new(None, 0)
    }
}

impl Drop for SyntaxState {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl SyntaxState {
    /// State for a document of `len` bytes in `language`.
    #[must_use]
    pub fn new(language: Option<LanguageId>, len: usize) -> Self {
        let phase = match language {
            None => Phase::Off(Off::NoLanguage),
            Some(l) if len > MAX_HIGHLIGHT_BYTES => {
                Phase::Lexical(Lexical::new(lexical::builtin(l)))
            }
            Some(_) => Phase::Waiting,
        };
        Self {
            language,
            phase,
            lexical_only: false,
            cancel: Arc::new(AtomicBool::new(false)),
            rev: 0,
        }
    }

    /// Start over, for a new language or a reloaded file. A running job is
    /// cancelled and its result will be ignored.
    pub fn reset(&mut self, language: Option<LanguageId>, len: usize) {
        self.cancel.store(true, Ordering::Relaxed);
        let rev = self.rev + 1;
        *self = Self::new(language, len);
        self.rev = rev;
    }

    /// Start over with simple rules only, for a user-defined language:
    /// no tree-sitter grammar, at any size.
    pub fn reset_simple(&mut self, rules: &'static Rules) {
        self.reset(None, 0);
        self.phase = Phase::Lexical(Lexical::new(rules));
    }

    /// Start over with the simple rules of the built-in `language`, and
    /// keep them at any size: for a memory-mapped document (M7), whose text
    /// is read from disk as needed. A tree-sitter parse would read all of
    /// it, so even a small mapped file (one that is not valid UTF-8) is
    /// never parsed.
    pub fn reset_lexical(&mut self, language: LanguageId) {
        self.reset(None, 0);
        self.language = Some(language);
        self.phase = Phase::Lexical(Lexical::new(lexical::builtin(language)));
        self.lexical_only = true;
    }

    /// The built-in language, if one was detected (`None` for a
    /// user-defined language).
    #[must_use]
    pub fn language(&self) -> Option<LanguageId> {
        self.language
    }

    /// Why there is no highlighting, if there is none.
    #[must_use]
    pub fn off_reason(&self) -> Option<Off> {
        match self.phase {
            Phase::Off(r) => Some(r),
            _ => None,
        }
    }

    /// The simple highlighter, when the document is not parsed: too large,
    /// a user-defined language, or mapped.
    #[must_use]
    pub fn lexical(&self) -> Option<&Lexical> {
        match &self.phase {
            Phase::Lexical(l) => Some(l),
            _ => None,
        }
    }

    /// The capture names that spans index into, and a key to cache their
    /// colours under (distinct per grammar, and for the simple rules).
    #[must_use]
    pub fn capture_names(&self) -> Option<(usize, &'static [&'static str])> {
        if self.lexical().is_some() {
            return Some((LanguageId::ALL.len(), crate::lexical::CAPTURES));
        }
        let (_, grammar) = self.tree()?;
        Some((self.language?.index(), grammar.highlights.capture_names()))
    }

    /// Changes whenever the highlighting may have changed.
    #[must_use]
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// Whether `id` is the job this state is waiting for.
    #[must_use]
    pub fn waiting_for(&self, id: u64) -> bool {
        matches!(self.phase, Phase::Parsing { job, .. } if job == id)
    }

    /// The tree to highlight from: the current one, or the stale one while
    /// a reparse runs on a worker.
    #[must_use]
    pub fn tree(&self) -> Option<(&Tree, &'static Grammar)> {
        match &self.phase {
            Phase::Ready { tree, grammar, .. } => Some((tree, grammar)),
            Phase::Parsing {
                stale: Some((tree, grammar)),
                ..
            } => Some((tree, *grammar)),
            _ => None,
        }
    }

    /// Record an edit just applied to `text` (the text after the edit).
    pub fn edit(&mut self, text: &dyn Text, edit: &Edit) {
        if let Phase::Lexical(lexical) = &self.phase {
            if self.language.is_some()
                && !self.lexical_only
                && text.len_bytes() <= MAX_HIGHLIGHT_BYTES
            {
                // A built-in language shrunk back under the limit (an
                // undone paste): parse.
                self.phase = Phase::Waiting;
            } else {
                lexical.edit(text, edit);
            }
            self.rev += 1;
            return;
        }
        if matches!(self.phase, Phase::Off(_) | Phase::Waiting) {
            return;
        }
        // The start of an edit is unchanged by it, so its position can be
        // read from the text after.
        let Some(ie) = input_edit(edit, point_at(text, edit.start)) else {
            // Positions unknown: start over with a fresh parse.
            self.phase = Phase::Waiting;
            self.rev += 1;
            return;
        };
        match &mut self.phase {
            Phase::Parsing { pending, stale, .. } => {
                pending.push(ie);
                if let Some((tree, _)) = stale {
                    tree.edit(&ie);
                }
            }
            Phase::Ready { tree, dirty, .. } => {
                tree.edit(&ie);
                *dirty = true;
            }
            Phase::Off(_) | Phase::Lexical(_) | Phase::Waiting => {}
        }
        self.rev += 1;
    }

    /// The first parse, if one should start now. The app runs it on a
    /// worker and passes the result to [`SyntaxState::finish`].
    pub fn take_job<B: Buffer>(&mut self, text: &B) -> Option<Job> {
        let language = self.language?;
        if !matches!(self.phase, Phase::Waiting) {
            return None;
        }
        if text.len_bytes() > MAX_HIGHLIGHT_BYTES {
            // Grew past the limit before the first parse began.
            self.phase = Phase::Lexical(Lexical::new(lexical::builtin(language)));
            self.rev += 1;
            return None;
        }
        let id = NEXT_JOB.fetch_add(1, Ordering::Relaxed);
        self.phase = Phase::Parsing {
            job: id,
            pending: Vec::with_capacity(64),
            stale: None,
        };
        Some(Job {
            id,
            language,
            snapshot: text.snapshot(),
            parser: None,
            old: None,
            cancel: Arc::clone(&self.cancel),
        })
    }

    /// Take a job's result. Edits made since it started are replayed into
    /// the tree, then one reparse (within `budget`) brings it up to date.
    /// Returns a follow-up job if that reparse ran over budget.
    pub fn finish<B: Buffer>(
        &mut self,
        result: JobResult,
        text: &B,
        budget: Duration,
    ) -> Option<Job> {
        if !self.waiting_for(result.id) {
            return None;
        }
        let Some((mut tree, parser, grammar)) = result.outcome else {
            self.phase = Phase::Off(Off::Failed);
            self.rev += 1;
            return None;
        };
        let pending = match &mut self.phase {
            Phase::Parsing { pending, .. } => std::mem::take(pending),
            _ => Vec::new(),
        };
        for ie in &pending {
            tree.edit(ie);
        }
        self.phase = Phase::Ready {
            tree,
            parser,
            grammar,
            dirty: !pending.is_empty(),
        };
        self.rev += 1;
        self.refresh(text, budget)
    }

    /// Bring the tree up to date after edits, spending at most `budget` on
    /// this thread. Returns a job to finish the parse on a worker if the
    /// budget ran out. Call once per frame, before highlighting.
    pub fn refresh<B: Buffer>(&mut self, text: &B, budget: Duration) -> Option<Job> {
        let Phase::Ready {
            tree,
            parser,
            dirty,
            ..
        } = &mut self.phase
        else {
            return None;
        };
        if !*dirty {
            return None;
        }
        if text.len_bytes() > MAX_HIGHLIGHT_BYTES {
            self.phase = Phase::Lexical(Lexical::new(lexical::builtin(self.language?)));
            self.rev += 1;
            return None;
        }
        // No deadline when the budget is too large to add to now.
        let deadline = Instant::now().checked_add(budget);
        let mut over = |_: &ParseState| {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let options = ParseOptions::new().progress_callback(&mut over);
        let as_text: &dyn Text = text;
        if let Some(new) = parser.parse_with_options(
            &mut |i, _| bytes_from(as_text, i),
            Some(tree),
            Some(options),
        ) {
            *tree = new;
            *dirty = false;
            self.rev += 1;
            return None;
        }
        // Over budget. Restart the parse on a worker from the edited tree;
        // keep a copy here so the old colours stay on screen meanwhile.
        let old = std::mem::replace(&mut self.phase, Phase::Waiting);
        let Phase::Ready {
            tree,
            mut parser,
            grammar,
            ..
        } = old
        else {
            return None;
        };
        // A cancelled parser would resume on its next parse; start clean so
        // it can never resume against different text.
        parser.reset();
        let id = NEXT_JOB.fetch_add(1, Ordering::Relaxed);
        let language = self.language?;
        self.phase = Phase::Parsing {
            job: id,
            pending: Vec::with_capacity(64),
            stale: Some((tree.clone(), grammar)),
        };
        Some(Job {
            id,
            language,
            snapshot: text.snapshot(),
            parser: Some(parser),
            old: Some(tree),
            cancel: Arc::clone(&self.cancel),
        })
    }

    /// Parse to completion on this thread: for headless rendering and tests.
    pub fn parse_now<B: Buffer>(&mut self, text: &B) {
        let mut job = self.take_job(text);
        while let Some(j) = job {
            let result = j.run();
            job = self.finish(result, text, Duration::MAX);
        }
    }
}

#[cfg(all(test, feature = "rust"))]
mod tests {
    use super::*;
    use nimbus_core::RopeBuffer;

    #[expect(clippy::unwrap_used, reason = "test helper")]
    fn fresh(src: &str) -> String {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        p.parse(src, None).unwrap().root_node().to_sexp()
    }

    #[expect(clippy::unwrap_used, reason = "test helper")]
    fn sexp(state: &SyntaxState) -> String {
        state.tree().unwrap().0.root_node().to_sexp()
    }

    #[expect(clippy::unwrap_used, reason = "test helper")]
    fn apply(
        buf: &mut RopeBuffer,
        state: &mut SyntaxState,
        range: std::ops::Range<usize>,
        text: &str,
    ) {
        let edit = buf.replace(range, text).unwrap();
        state.edit(buf, &edit);
    }

    #[test]
    fn unknown_and_huge_files_are_off() {
        assert_eq!(
            SyntaxState::new(None, 10).off_reason(),
            Some(Off::NoLanguage)
        );
        let s = SyntaxState::new(Some(LanguageId::Rust), MAX_HIGHLIGHT_BYTES + 1);
        assert!(s.lexical().is_some(), "huge files get the simple rules");
        assert_eq!(
            s.capture_names().map(|(_, n)| n),
            Some(crate::lexical::CAPTURES)
        );
    }

    #[test]
    fn highlighting_returns_when_a_file_shrinks_under_the_limit() {
        let mut buf = RopeBuffer::from_text("fn a() {}\n");
        let mut state = SyntaxState::new(Some(LanguageId::Rust), buf.len_bytes());
        state.parse_now(&buf);
        let big = "x".repeat(MAX_HIGHLIGHT_BYTES);
        apply(&mut buf, &mut state, 0..0, &big);
        assert!(state.refresh(&buf, Duration::MAX).is_none());
        assert!(state.lexical().is_some());
        apply(&mut buf, &mut state, 0..big.len(), "");
        assert!(state.lexical().is_none());
        state.parse_now(&buf);
        assert_eq!(sexp(&state), fresh("fn a() {}\n"));
    }

    #[test]
    fn a_mapped_document_keeps_the_simple_rules_when_small() {
        let mut buf = nimbus_core::MappedBuffer::from_bytes(b"fn a() {}\n".to_vec());
        let mut state = SyntaxState::default();
        let rev = state.rev();
        state.reset_lexical(LanguageId::Rust);
        assert!(state.rev() > rev);
        assert!(state.lexical().is_some());
        assert_eq!(state.language(), Some(LanguageId::Rust));
        assert!(state.take_job(&buf).is_none(), "never parsed");
        let edit = buf.replace(0..0, "// x\n").unwrap();
        state.edit(&buf, &edit);
        assert!(state.lexical().is_some(), "an edit does not start a parse");
        assert!(state.take_job(&buf).is_none());
        assert!(state.refresh(&buf, Duration::MAX).is_none());

        // A reset (a reload that opened as a rope) parses again.
        state.reset(Some(LanguageId::Rust), buf.len_bytes());
        assert!(state.lexical().is_none());
        assert!(state.take_job(&buf).is_some());
    }

    #[test]
    fn edits_during_the_first_parse_are_replayed() {
        let mut buf = RopeBuffer::from_text("fn a() {}\n");
        let mut state = SyntaxState::new(Some(LanguageId::Rust), buf.len_bytes());
        let job = state.take_job(&buf).unwrap();
        assert!(state.take_job(&buf).is_none(), "one job at a time");
        // The worker parses the snapshot while the user types.
        let result = job.run();
        apply(&mut buf, &mut state, 0..0, "struct S;\n");
        let end = buf.len_bytes();
        apply(&mut buf, &mut state, end..end, "const X: u8 = 1;\n");
        assert!(state.finish(result, &buf, Duration::MAX).is_none());
        assert_eq!(sexp(&state), fresh(&buf.slice(0..buf.len_bytes())));
    }

    #[test]
    fn incremental_edits_match_a_fresh_parse() {
        let mut buf = RopeBuffer::from_text("fn main() {\n    let x = 1;\n}\n");
        let mut state = SyntaxState::new(Some(LanguageId::Rust), buf.len_bytes());
        state.parse_now(&buf);
        let rev = state.rev();
        apply(&mut buf, &mut state, 16..17, "renamed");
        apply(&mut buf, &mut state, 0..0, "/* ü */\n");
        assert!(state.refresh(&buf, Duration::MAX).is_none());
        assert!(state.rev() > rev);
        assert_eq!(sexp(&state), fresh(&buf.slice(0..buf.len_bytes())));
    }

    #[test]
    fn an_over_budget_reparse_moves_to_a_worker() {
        let mut buf = RopeBuffer::from_text("fn main() {}\n");
        let mut state = SyntaxState::new(Some(LanguageId::Rust), buf.len_bytes());
        state.parse_now(&buf);
        // A big paste: new text the old tree cannot help with.
        let paste = "fn f() { let s = \"x\"; }\n".repeat(20_000);
        apply(&mut buf, &mut state, 0..0, &paste);
        let job = state.refresh(&buf, Duration::ZERO).expect("over budget");
        assert!(
            state.tree().is_some(),
            "the stale tree still colours the view"
        );
        let result = job.run();
        apply(&mut buf, &mut state, 2..2, " note");
        assert!(state.finish(result, &buf, Duration::MAX).is_none());
        assert_eq!(sexp(&state), fresh(&buf.slice(0..buf.len_bytes())));
    }

    #[test]
    fn a_reset_ignores_the_old_job() {
        let buf = RopeBuffer::from_text("fn a() {}\n");
        let mut state = SyntaxState::new(Some(LanguageId::Rust), buf.len_bytes());
        let job = state.take_job(&buf).unwrap();
        let rev = state.rev();
        state.reset(Some(LanguageId::Rust), buf.len_bytes());
        assert!(state.rev() > rev);
        let result = job.run();
        assert!(!state.waiting_for(result.id()));
        assert!(state.finish(result, &buf, Duration::MAX).is_none());
        assert!(state.tree().is_none());
        state.parse_now(&buf);
        assert_eq!(sexp(&state), fresh("fn a() {}\n"));
    }
}
