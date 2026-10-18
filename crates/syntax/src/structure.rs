//! What the tree says about a place in the text (M14): the node to grow
//! a selection to, and the regions that can fold.
//!
//! Both are read-only walks of the existing tree, asked for one byte or
//! one visible range at a time, so each call costs microseconds whatever
//! the file's size. Neither runs over the whole file.

use std::ops::Range;

use nimbus_core::Text;
use streaming_iterator::StreamingIterator;
use tree_sitter::QueryCursor;

use crate::registry;
use crate::state::SyntaxState;
use crate::text::NodeChunks;

/// Whether there is a tree that describes the text as it is now, so
/// that [`expand`] can be trusted over counting brackets. False while
/// the first parse runs, after an edit until the reparse, and for files
/// that get no tree.
#[must_use]
pub fn ready(state: &SyntaxState) -> bool {
    state.tree().is_some() && state.phase_name() == "ready"
}

/// The range of the smallest named syntax node that contains `range`
/// and is larger than it: the next step of a structural Expand
/// Selection (an identifier, then the expression, the statement, the
/// block, the function). `None` when there is no up-to-date tree, or
/// only the whole file is left, which the caller handles itself.
#[must_use]
pub fn expand(state: &SyntaxState, range: Range<usize>) -> Option<Range<usize>> {
    let (tree, _) = state.tree().filter(|_| state.phase_name() == "ready")?;
    let root = tree.root_node();
    let mut node = root.named_descendant_for_byte_range(range.start, range.end)?;
    loop {
        let found = node.start_byte()..node.end_byte();
        if found.start <= range.start && found.end >= range.end && found != range {
            return (node != root).then_some(found);
        }
        node = node.parent()?;
    }
}

/// The byte ranges within `range` that the language's fold query marks
/// as foldable and that span more than one line, appended to `out`,
/// sorted by start then by end descending (an outer region before the
/// ones inside it). Empty without a tree or a fold query.
///
/// A region is the node's whole range: the caller folds from the end of
/// its first line to the end of its last, so that the opening line stays
/// in view (M13's fold model).
pub fn fold_regions(
    state: &SyntaxState,
    text: &dyn Text,
    range: Range<usize>,
    out: &mut Vec<Range<usize>>,
) {
    let Some((tree, _)) = state.tree() else {
        return;
    };
    let Some(query) = state.language().and_then(registry::folds_query) else {
        return;
    };
    let first = out.len();
    let mut cursor = QueryCursor::new();
    crate::split::for_each_root(tree.root_node(), &range, &mut |root| {
        crate::split::prepare(&mut cursor, &root, &range);
        let mut matches = cursor.matches(query, root.0, |node: tree_sitter::Node<'_>| {
            NodeChunks::new(text, node)
        });
        while let Some(m) = matches.next() {
            for c in m.captures() {
                let (s, e) = (c.node.start_byte(), c.node.end_byte());
                if s < e && text.byte_to_line(s) < text.byte_to_line(e.saturating_sub(1)) {
                    out.push(s..e);
                }
            }
        }
    });
    let added = out.get_mut(first..).unwrap_or(&mut []);
    added.sort_unstable_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    // The same node can be captured by two patterns; keep one.
    let mut keep = first;
    for i in first..out.len() {
        if keep == first || out.get(keep - 1) != out.get(i) {
            out.swap(keep, i);
            keep += 1;
        }
    }
    out.truncate(keep);
}

#[cfg(all(test, feature = "rust"))]
mod tests {
    use super::*;
    use crate::LanguageId;
    use nimbus_core::RopeBuffer;

    fn parsed(src: &str, language: LanguageId) -> (RopeBuffer, SyntaxState) {
        let buf = RopeBuffer::from_text(src);
        let mut state = SyntaxState::new(Some(language), src.len());
        state.parse_now(&buf);
        (buf, state)
    }

    #[test]
    fn expand_climbs_from_identifier_to_function() {
        let src = "fn main() {\n    let total = count + 1;\n}\n";
        let (buf, state) = parsed(src, LanguageId::Rust);
        let at = src.find("count").unwrap_or(0);
        let steps = |mut range: Range<usize>| {
            let mut seen = Vec::new();
            while let Some(next) = expand(&state, range.clone()) {
                seen.push(buf.slice(next.clone()).into_owned());
                range = next;
            }
            seen
        };
        assert_eq!(
            steps(at..at),
            [
                "count",
                "count + 1",
                "let total = count + 1;",
                "{\n    let total = count + 1;\n}",
                "fn main() {\n    let total = count + 1;\n}",
            ]
        );
        // From a selection, the next node up.
        assert_eq!(
            expand(&state, at..at + 9).map(|r| buf.slice(r).into_owned()),
            Some("let total = count + 1;".to_owned())
        );
        // No tree: nothing to say.
        let fresh = SyntaxState::new(Some(LanguageId::Rust), src.len());
        assert_eq!(expand(&fresh, at..at), None);
    }

    #[test]
    fn fold_regions_span_lines_and_nest_outer_first() {
        let src = "fn a() {\n    if x {\n        y();\n    }\n}\nfn one_liner() { z() }\n";
        let (buf, state) = parsed(src, LanguageId::Rust);
        let mut out = Vec::new();
        fold_regions(&state, &buf, 0..src.len(), &mut out);
        let texts: Vec<String> = out
            .iter()
            .map(|r| buf.slice(r.clone()).into_owned())
            .collect();
        assert_eq!(texts[0], "fn a() {\n    if x {\n        y();\n    }\n}");
        assert_eq!(texts[1], "{\n    if x {\n        y();\n    }\n}");
        assert!(texts.iter().any(|t| t.starts_with("if x")), "{texts:?}");
        assert!(
            !texts.iter().any(|t| t.contains("one_liner")),
            "single-line regions are not folds: {texts:?}"
        );
        // Only what the range touches.
        let mut near = Vec::new();
        fold_regions(&state, &buf, src.len() - 5..src.len(), &mut near);
        assert!(near.is_empty(), "{near:?}");
    }

    #[cfg(feature = "markdown")]
    #[test]
    fn markdown_sections_fold() {
        let src = "# One\n\ntext\n\n## Two\n\nmore\n";
        let (buf, state) = parsed(src, LanguageId::Markdown);
        let mut out = Vec::new();
        fold_regions(&state, &buf, 0..src.len(), &mut out);
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0], 0..src.len());
        assert_eq!(buf.slice(out[1].clone()), "## Two\n\nmore\n");
    }
}
