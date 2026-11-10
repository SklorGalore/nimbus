//! Turning a syntax tree into coloured spans for the visible lines.
//!
//! The highlight query names parts of the tree (`@keyword`, `@string`,
//! `@function.method`); the theme maps those names to colours. This module
//! finds which name covers each byte. When several captures cover the same
//! bytes, the innermost node wins, and for the same node the pattern that
//! comes later in the query wins, the convention the queries are written for.
//! A nested language's tree ([`crate::inject`]) is innermost of all: its
//! captures paint over the host's for the same bytes.

use std::cmp::Reverse;
use std::ops::Range;
use std::time::{Duration, Instant};

use nimbus_core::Text;
use streaming_iterator::StreamingIterator;
use tree_sitter::QueryCursor;

use crate::registry::{Grammar, LEXICAL_TABLE};
use crate::split;
use crate::state::SyntaxState;
use crate::text::NodeChunks;

/// A run of bytes highlighted as one capture of a grammar's query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Start byte.
    pub start: usize,
    /// End byte, exclusive.
    pub end: usize,
    /// Index into the capture names of `table`.
    pub capture: u32,
    /// Which capture table (`registry::capture_table`) names the capture:
    /// the document's grammar, a nested language's, or the simple rules'.
    pub table: usize,
}

/// No capture: default text colour.
const NONE: u32 = u32::MAX;

/// Captures are painted as `table << TABLE_SHIFT | capture`.
const TABLE_SHIFT: u32 = 24;

/// Time per frame for parsing nested languages that came into view, on
/// top of the host's reparse. What does not fit waits for the next frame.
pub const INJECTION_BUDGET: Duration = Duration::from_millis(2);

/// One capture to paint: start, end, nesting depth, pattern index,
/// encoded capture.
type Capture = (usize, usize, u8, usize, u32);

/// Computes spans, reusing its buffers and the last result while neither the
/// tree nor the range changed.
pub struct Highlighter {
    cursor: QueryCursor,
    captures: Vec<Capture>,
    paint: Vec<u32>,
    spans: Vec<Span>,
    /// What `spans` was computed for: the state's revision and the ranges.
    key_rev: Option<u64>,
    key_ranges: Vec<Range<usize>>,
    /// Time allowed per call for parsing nested languages.
    pub injection_budget: Duration,
}

impl std::fmt::Debug for Highlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Highlighter")
            .field("spans", &self.spans.len())
            .finish_non_exhaustive()
    }
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlighter {
    /// A highlighter with empty buffers.
    #[must_use]
    pub fn new() -> Self {
        let mut cursor = QueryCursor::new();
        // Bound the work on pathological input; highlighting a frame must
        // stay cheap.
        cursor.set_match_limit(256);
        Self {
            cursor,
            captures: Vec::new(),
            paint: Vec::new(),
            spans: Vec::new(),
            key_rev: None,
            key_ranges: Vec::new(),
            injection_budget: INJECTION_BUDGET,
        }
    }

    /// Spans within `ranges` of `text`, sorted and not overlapping. The
    /// ranges must be sorted and not overlap; bytes in no span have no
    /// capture. Empty while there is no tree yet.
    ///
    /// The cost grows with the length of the ranges, so pass the visible
    /// parts of the lines on screen, not the file. Nested languages in
    /// the ranges are parsed here, within [`Self::injection_budget`];
    /// [`SyntaxState::injections_pending`] says when another call is
    /// needed to finish.
    pub fn spans(
        &mut self,
        state: &SyntaxState,
        text: &dyn Text,
        ranges: &[Range<usize>],
    ) -> &[Span] {
        let len = text.len_bytes();
        let clamped: Vec<Range<usize>> = ranges
            .iter()
            .map(|r| r.start.min(len)..r.end.min(len))
            .filter(|r| !r.is_empty())
            .collect();
        let host = state.tree();
        let mut injected = state.injected().borrow_mut();
        // Nothing changed since the last call, and no nested region is
        // waiting for a parse: the cached spans stand. (Nested regions
        // depend on the view, the text and the host tree, all unchanged.)
        if self.key_rev == Some(state.rev()) && self.key_ranges == ranges && !injected.pending() {
            return &self.spans;
        }
        // Nested languages first: what the view shows decides which
        // regions have trees.
        match host {
            Some((tree, grammar)) if grammar.injections.is_some() => {
                let deadline = Instant::now().checked_add(self.injection_budget);
                injected.refresh(tree, grammar, text, &clamped, deadline);
            }
            _ => injected.clear(),
        }
        self.spans.clear();
        self.key_rev = Some(state.rev());
        self.key_ranges.clear();
        self.key_ranges.extend_from_slice(ranges);
        for range in clamped {
            if let Some(lexical) = state.lexical() {
                let first = self.spans.len();
                lexical.spans(text, range, &mut self.spans);
                for s in self.spans.get_mut(first..).unwrap_or(&mut []) {
                    s.table = LEXICAL_TABLE;
                }
            } else if let Some((tree, grammar)) = host {
                let table = state.language().map_or(0, crate::LanguageId::index);
                self.tree_spans(tree, grammar, table, &injected, text, range);
            }
        }
        &self.spans
    }

    /// Append the spans of one range from a syntax tree and the trees
    /// nested in it.
    fn tree_spans(
        &mut self,
        tree: &tree_sitter::Tree,
        grammar: &Grammar,
        table: usize,
        injected: &crate::inject::Injected,
        text: &dyn Text,
        range: Range<usize>,
    ) {
        self.captures.clear();
        self.collect(tree, grammar, table, 0, text, &range);
        injected.children.visit(&range, &mut |child, depth| {
            if let Some(tree) = &child.tree {
                self.collect(tree, child.grammar, child.key.table(), depth, text, &range);
            }
        });
        // Paint outer nodes first, then inner ones over them; a nested
        // language over its host; for the same node, earlier patterns
        // first so later ones win.
        self.captures
            .sort_unstable_by_key(|&(s, e, depth, pattern, _)| (s, Reverse(e), depth, pattern));

        self.paint.clear();
        self.paint.resize(range.len(), NONE);
        for &(s, e, _, _, capture) in &self.captures {
            let s = s.clamp(range.start, range.end) - range.start;
            let e = e.clamp(range.start, range.end) - range.start;
            if let Some(cells) = self.paint.get_mut(s..e) {
                cells.fill(capture);
            }
        }

        let mut run_start = 0;
        for i in 1..=self.paint.len() {
            let current = self.paint.get(run_start).copied();
            if self.paint.get(i).copied() != current {
                if let Some(encoded) = current.filter(|&c| c != NONE) {
                    self.spans.push(Span {
                        start: range.start + run_start,
                        end: range.start + i,
                        capture: encoded & ((1 << TABLE_SHIFT) - 1),
                        table: (encoded >> TABLE_SHIFT) as usize,
                    });
                }
                run_start = i;
            }
        }
    }

    /// Add the captures of one tree's highlight query within `range`.
    fn collect(
        &mut self,
        tree: &tree_sitter::Tree,
        grammar: &Grammar,
        table: usize,
        depth: u8,
        text: &dyn Text,
        range: &Range<usize>,
    ) {
        let Ok(table) = u32::try_from(table) else {
            return;
        };
        let (cursor, captures) = (&mut self.cursor, &mut self.captures);
        split::for_each_root(tree.root_node(), range, &mut |root| {
            split::prepare(cursor, &root, range);
            let mut matches = cursor.captures(
                &grammar.highlights,
                root.0,
                |node: tree_sitter::Node<'_>| NodeChunks::new(text, node),
            );
            while let Some((m, i)) = matches.next() {
                let Some(c) = m.captures().get(*i) else {
                    continue;
                };
                let (s, e) = (c.node.start_byte(), c.node.end_byte());
                if s < e {
                    let encoded = (table << TABLE_SHIFT) | (c.index & ((1 << TABLE_SHIFT) - 1));
                    captures.push((s, e, depth, m.pattern_index, encoded));
                }
            }
        });
    }
}

#[cfg(all(test, feature = "rust"))]
mod tests {
    use super::*;
    use crate::LanguageId;
    use nimbus_core::RopeBuffer;

    #[expect(clippy::unwrap_used, reason = "test helper")]
    fn names(src: &str, language: LanguageId) -> Vec<(String, String)> {
        let buf = RopeBuffer::from_text(src);
        let mut state = SyntaxState::new(Some(language), src.len());
        state.parse_now(&buf);
        let mut h = Highlighter::new();
        let mut spans = Vec::new();
        // Nested languages may need a frame or two.
        for _ in 0..10 {
            spans = h
                .spans(&state, &buf, std::slice::from_ref(&(0..src.len())))
                .to_vec();
            if !state.injections_pending() {
                break;
            }
        }
        spans
            .iter()
            .map(|s| {
                let names = crate::registry::capture_table(s.table).unwrap();
                let name = names[s.capture as usize];
                (src[s.start..s.end].to_owned(), name.to_owned())
            })
            .collect()
    }

    fn name_of<'a>(spans: &'a [(String, String)], text: &str) -> Option<&'a str> {
        spans
            .iter()
            .find(|(t, _)| t == text)
            .map(|(_, n)| n.as_str())
    }

    #[test]
    fn rust_highlights_resolve_by_precedence() {
        let src = "const FOO: u32 = 1;\nfn main() { let s = \"hi\"; Foo::new(); } // done\n";
        let spans = names(src, LanguageId::Rust);
        assert_eq!(name_of(&spans, "fn"), Some("keyword"));
        assert_eq!(name_of(&spans, "main"), Some("function"));
        assert_eq!(name_of(&spans, "\"hi\""), Some("string"));
        assert_eq!(name_of(&spans, "u32"), Some("type.builtin"));
        assert_eq!(name_of(&spans, "// done"), Some("comment"));
        // Needs the vendored query's fixed pattern, ordered after @constructor.
        assert_eq!(name_of(&spans, "FOO"), Some("constant"));
        assert_eq!(name_of(&spans, "Foo"), Some("type"));
        assert_eq!(name_of(&spans, "new"), Some("function"));
    }

    #[test]
    fn spans_are_sorted_disjoint_and_clipped() {
        let src = "fn a() {}\nfn b() {}\nfn c() {}\n";
        let buf = RopeBuffer::from_text(src);
        let mut state = SyntaxState::new(Some(LanguageId::Rust), src.len());
        state.parse_now(&buf);
        let mut h = Highlighter::new();
        let range = 12..20;
        let spans = h.spans(&state, &buf, std::slice::from_ref(&range)).to_vec();
        assert_ne!(spans.len(), 0);
        for w in spans.windows(2) {
            assert!(w[0].end <= w[1].start, "{spans:?}");
        }
        assert!(
            spans
                .iter()
                .all(|s| s.start >= range.start && s.end <= range.end)
        );
        // Same key: the cached result.
        assert_eq!(h.spans(&state, &buf, &[range]).to_vec(), spans);
    }

    #[cfg(feature = "markdown")]
    #[test]
    fn markdown_inline_and_fenced_code_colour_with_the_inner_language() {
        let src = "# Title *em*\n\nSome **bold** and `code` here.\n\n```rust\nfn main() {}\n```\n\n```nope\nfn x\n```\n";
        let spans = names(src, LanguageId::Markdown);
        assert_eq!(name_of(&spans, "em"), Some("text.emphasis"), "{spans:?}");
        assert_eq!(name_of(&spans, "bold"), Some("text.strong"));
        assert_eq!(name_of(&spans, "code"), Some("text.literal"));
        assert_eq!(name_of(&spans, "`"), Some("punctuation.delimiter"));
        assert_eq!(name_of(&spans, "fn"), Some("keyword"), "the Rust fence");
        assert_eq!(name_of(&spans, "main"), Some("function"));
        assert!(
            spans.iter().all(|(t, n)| t != "x" || n != "keyword"),
            "an unknown fence language is coloured by the host: {spans:?}"
        );
        // Spans never overlap, whichever tree they come from.
        let buf = RopeBuffer::from_text(src);
        let mut state = SyntaxState::new(Some(LanguageId::Markdown), src.len());
        state.parse_now(&buf);
        let mut h = Highlighter::new();
        let all = h
            .spans(&state, &buf, std::slice::from_ref(&(0..src.len())))
            .to_vec();
        for w in all.windows(2) {
            assert!(w[0].end <= w[1].start, "{all:?}");
        }
        assert_eq!(
            state.injected().borrow().children.count(),
            3,
            "two inlines and the Rust fence; the unknown fence is none: {:?}",
            state.injected()
        );
    }

    #[cfg(feature = "markdown")]
    #[test]
    fn injected_trees_follow_edits_and_scrolling() {
        use nimbus_core::Buffer;
        let mut buf = RopeBuffer::from_text("plain *em*\n");
        let mut state = SyntaxState::new(Some(LanguageId::Markdown), buf.len_bytes());
        state.parse_now(&buf);
        let mut h = Highlighter::new();
        let full = |h: &mut Highlighter, state: &SyntaxState, buf: &RopeBuffer| {
            h.spans(state, buf, std::slice::from_ref(&(0..buf.len_bytes())))
                .iter()
                .map(|s| (buf.slice(s.start..s.end).into_owned(), s.table))
                .collect::<Vec<_>>()
        };
        let before = full(&mut h, &state, &buf);
        assert!(before.iter().any(|(t, _)| t == "em"), "{before:?}");
        // Typing before the paragraph shifts it; the child tree follows.
        let edit = buf.replace(0..0, "# Head\n\n").unwrap();
        state.edit(&buf, &edit);
        assert!(state.refresh(&buf, Duration::MAX).is_none());
        let after = full(&mut h, &state, &buf);
        assert!(after.iter().any(|(t, _)| t == "em"), "{after:?}");
        // Editing inside it reparses it.
        let at = buf.slice(0..buf.len_bytes()).find("*em*").unwrap();
        let edit = buf.replace(at..at + 4, "**strong**").unwrap();
        state.edit(&buf, &edit);
        assert!(state.refresh(&buf, Duration::MAX).is_none());
        let after = full(&mut h, &state, &buf);
        assert!(after.iter().any(|(t, _)| t == "strong"), "{after:?}");
        // A view far away drops the child.
        let far = "\n".repeat(100_000);
        let end = buf.len_bytes();
        let edit = buf.replace(end..end, &far).unwrap();
        state.edit(&buf, &edit);
        assert!(state.refresh(&buf, Duration::MAX).is_none());
        let tail = buf.len_bytes() - 10..buf.len_bytes();
        let _ = h.spans(&state, &buf, std::slice::from_ref(&tail));
        assert_eq!(state.injected().borrow().children.count(), 0);
        let _ = h.spans(&state, &buf, std::slice::from_ref(&(0..20)));
        assert_eq!(
            state.injected().borrow().children.count(),
            2,
            "heading and paragraph"
        );
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn typescript_inherits_javascript_highlights() {
        let spans = names(
            "function f(x: number) { return \"s\"; }\n",
            LanguageId::TypeScript,
        );
        assert_eq!(name_of(&spans, "function"), Some("keyword"));
        assert_eq!(name_of(&spans, "\"s\""), Some("string"));
        assert_eq!(name_of(&spans, "number"), Some("type.builtin"));
    }

    #[cfg(feature = "powershell")]
    #[test]
    fn powershell_highlights() {
        let src = "<# help #>\n[CmdletBinding()]\nparam([string]$Path)\n\
                   function Get-Thing {\n    If ($Path -eq $null) { return \"at $Path\" }\n\
                   \x20   Write-Host -ForegroundColor Red $_.Length\n\
                   \x20   $h = @{ Key = 0x1F }\n    [IO.File]::ReadAllText($env:HOME) # done\n}\n";
        let spans = names(src, LanguageId::PowerShell);
        let is = |text, name| assert_eq!(name_of(&spans, text), Some(name), "{text}: {spans:?}");
        is("<# help #>", "comment");
        is("# done", "comment");
        is("CmdletBinding", "attribute");
        is("param", "keyword");
        is("function", "keyword");
        // Keywords match in any case.
        is("If", "keyword");
        is("return", "keyword");
        is("-eq", "keyword.operator");
        is("$null", "constant.builtin");
        is("Get-Thing", "function");
        is("Write-Host", "function");
        is("-ForegroundColor", "label");
        is("$_", "variable.builtin");
        is("Length", "property");
        is("Key", "property");
        is("0x1F", "number");
        is("ReadAllText", "function.method");
        is("$env:HOME", "variable.builtin");
        // An interpolated variable breaks the string's colour.
        is("\"at ", "string");
        is("$Path", "variable");
    }
}
