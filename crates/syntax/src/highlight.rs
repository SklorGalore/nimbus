//! Turning a syntax tree into coloured spans for the visible lines.
//!
//! The highlight query names parts of the tree (`@keyword`, `@string`,
//! `@function.method`); the theme maps those names to colours. This module
//! finds which name covers each byte. When several captures cover the same
//! bytes, the innermost node wins, and for the same node the pattern that
//! comes later in the query wins, the convention the queries are written for.

use std::cmp::Reverse;
use std::ops::Range;

use nimbus_core::Text;
use streaming_iterator::StreamingIterator;
use tree_sitter::QueryCursor;

use crate::state::SyntaxState;
use crate::text::NodeChunks;

/// A run of bytes highlighted as one capture of the grammar's query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Start byte.
    pub start: usize,
    /// End byte, exclusive.
    pub end: usize,
    /// Index into the query's capture names.
    pub capture: u32,
}

/// No capture: default text colour.
const NONE: u32 = u32::MAX;

/// Computes spans, reusing its buffers and the last result while neither the
/// tree nor the range changed.
pub struct Highlighter {
    cursor: QueryCursor,
    captures: Vec<(usize, usize, usize, u32)>,
    paint: Vec<u32>,
    spans: Vec<Span>,
    /// What `spans` was computed for: the state's revision and the ranges.
    key_rev: Option<u64>,
    key_ranges: Vec<Range<usize>>,
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
        }
    }

    /// Spans within `ranges` of `text`, sorted and not overlapping. The
    /// ranges must be sorted and not overlap; bytes in no span have no
    /// capture. Empty while there is no tree yet.
    ///
    /// The cost grows with the length of the ranges, so pass the visible
    /// parts of the lines on screen, not the file.
    pub fn spans(
        &mut self,
        state: &SyntaxState,
        text: &dyn Text,
        ranges: &[Range<usize>],
    ) -> &[Span] {
        if self.key_rev == Some(state.rev()) && self.key_ranges == ranges {
            return &self.spans;
        }
        self.spans.clear();
        self.key_rev = Some(state.rev());
        self.key_ranges.clear();
        self.key_ranges.extend_from_slice(ranges);
        let len = text.len_bytes();
        for range in ranges {
            let range = range.start.min(len)..range.end.min(len);
            if range.is_empty() {
                continue;
            }
            if let Some(lexical) = state.lexical() {
                lexical.spans(text, range, &mut self.spans);
            } else if let Some((tree, grammar)) = state.tree() {
                self.tree_spans(tree, grammar, text, range);
            }
        }
        &self.spans
    }

    /// Append the spans of one range from a syntax tree.
    fn tree_spans(
        &mut self,
        tree: &tree_sitter::Tree,
        grammar: &crate::registry::Grammar,
        text: &dyn Text,
        range: Range<usize>,
    ) {
        self.captures.clear();
        self.cursor.set_byte_range(range.clone());
        let mut matches = self.cursor.captures(
            &grammar.highlights,
            tree.root_node(),
            |node: tree_sitter::Node<'_>| NodeChunks::new(text, node),
        );
        while let Some((m, i)) = matches.next() {
            let Some(c) = m.captures().get(*i) else {
                continue;
            };
            let (s, e) = (c.node.start_byte(), c.node.end_byte());
            if s < e {
                self.captures.push((s, e, m.pattern_index, c.index));
            }
        }
        // Paint outer nodes first, then inner ones over them; for the same
        // node, earlier patterns first so later ones win.
        self.captures
            .sort_unstable_by_key(|&(s, e, pattern, _)| (s, Reverse(e), pattern));

        self.paint.clear();
        self.paint.resize(range.len(), NONE);
        for &(s, e, _, capture) in &self.captures {
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
                if let Some(capture) = current.filter(|&c| c != NONE) {
                    self.spans.push(Span {
                        start: range.start + run_start,
                        end: range.start + i,
                        capture,
                    });
                }
                run_start = i;
            }
        }
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
        let grammar = state.tree().unwrap().1;
        let mut h = Highlighter::new();
        h.spans(&state, &buf, std::slice::from_ref(&(0..src.len())))
            .iter()
            .map(|s| {
                let name = grammar.highlights.capture_names()[s.capture as usize];
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
}
