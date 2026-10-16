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
    key: Option<(u64, Range<usize>)>,
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
            key: None,
        }
    }

    /// Spans within `range` of `text`, sorted and not overlapping. Bytes in
    /// no span have no capture. Empty while there is no tree yet.
    ///
    /// The cost grows with the length of `range`, so pass the visible
    /// lines, not the file.
    pub fn spans(&mut self, state: &SyntaxState, text: &dyn Text, range: Range<usize>) -> &[Span] {
        let key = (state.rev(), range.clone());
        if self.key.as_ref() == Some(&key) {
            return &self.spans;
        }
        self.spans.clear();
        self.key = Some(key);
        let Some((tree, grammar)) = state.tree() else {
            return &self.spans;
        };
        let range = range.start.min(text.len_bytes())..range.end.min(text.len_bytes());
        if range.is_empty() {
            return &self.spans;
        }

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
        &self.spans
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
        h.spans(&state, &buf, 0..src.len())
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
        let spans = h.spans(&state, &buf, range.clone()).to_vec();
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
        assert_eq!(h.spans(&state, &buf, range).to_vec(), spans);
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
