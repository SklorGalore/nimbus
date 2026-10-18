//! Sticky scroll (M20): the scopes around the top of the view, pinned
//! above the text so a reader knows which function, impl or heading they
//! are in.
//!
//! A scope is a fold region from the grammar's fold query (M14), the same
//! regions the gutter folds. The question asked is small: which regions
//! contain one byte, the start of the line just below the pinned rows.
//! The tree answers that by walking down to that byte, so it costs the
//! same on any file. Files over 16 MB have no tree (`DESIGN.md` §4.2),
//! so they get no sticky rows.

use std::ops::Range;

use nimbus_core::Text;
use nimbus_core::wrap::{self, RowPos, WrapView};
use nimbus_syntax::highlight::{Highlighter, Span};
use nimbus_syntax::structure;

use crate::document::Document;

/// The most rows pinned at once. Deeper nesting keeps the outermost
/// scopes, which say the most about where the reader is.
pub const MAX_ROWS: usize = 5;

/// Bytes of a pinned line that are coloured and drawn: enough for any
/// window, so a pinned long line costs no more than a short one.
pub const LINE_BYTES: usize = 4096;

/// What a view's sticky scroll was worked out for: the text's version,
/// the syntax revision, the top row, the wrap width and the number of
/// folds. The lines are asked for again only when one changes.
pub type Key = (u64, u64, usize, Option<usize>, usize);

/// One view's pinned lines and what drawing them needs.
#[derive(Debug, Default)]
pub struct Sticky {
    /// The pinned lines, by their starts, outermost first.
    lines: Vec<usize>,
    key: Option<Key>,
    /// Scratch for the fold regions looked at.
    regions: Vec<Range<usize>>,
    /// Syntax spans for the pinned lines, cached apart from the visible
    /// lines' so that neither evicts the other.
    highlighter: Highlighter,
    /// Reused for the byte ranges of the pinned lines.
    ranges: Vec<Range<usize>>,
}

impl Sticky {
    /// The lines pinned in the last frame.
    #[must_use]
    pub fn lines(&self) -> &[usize] {
        &self.lines
    }

    /// Work out the lines to pin for a view whose top row is `top`, unless
    /// `key` is what they were last worked out for. `None` turns sticky
    /// scroll off.
    pub fn update(&mut self, key: Option<Key>, doc: &Document, w: &mut WrapView<'_>, top: RowPos) {
        let Some(key) = key else {
            self.lines.clear();
            self.key = None;
            return;
        };
        if self.key != Some(key) {
            self.key = Some(key);
            lines_for(doc, w, top, &mut self.regions, &mut self.lines);
        }
    }

    /// The syntax spans of the pinned lines (none when `colour` is false)
    /// and the lines themselves.
    pub fn parts(&mut self, doc: &Document, colour: bool) -> (&[Span], &[usize]) {
        if self.lines.is_empty() || !colour {
            return (&[], &self.lines);
        }
        let t = &doc.buffer;
        self.ranges.clear();
        for &start in &self.lines {
            let span = t.line_span(start);
            self.ranges.push(start..span.end.min(start + LINE_BYTES));
        }
        let spans = self.highlighter.spans(&doc.syntax, t, &self.ranges);
        (spans, &self.lines)
    }
}

/// Append to `out` the first lines (their starts) of the scopes that
/// contain the line starting at `line`, outermost first, at most
/// [`MAX_ROWS`]. A scope counts when its first line is above `line` (so
/// it is scrolled out of view) and it has not ended before `line`. Two
/// scopes starting on the same line (`fn f() {` is a function and its
/// block) count once, as does a body whose line only closes its item's
/// signature. `regions` is scratch space.
pub fn scopes(doc: &Document, line: usize, regions: &mut Vec<Range<usize>>, out: &mut Vec<usize>) {
    let t = &doc.buffer;
    regions.clear();
    structure::fold_regions(&doc.syntax, t, line..line + 1, regions);
    // Outer regions come first (sorted by start, then by end descending).
    let mut last_end = None;
    for r in regions.iter() {
        if out.len() >= MAX_ROWS {
            break;
        }
        if r.start >= line || r.end <= line {
            continue;
        }
        let head = t.line_span(r.start).start;
        if head >= line || out.last() == Some(&head) {
            continue;
        }
        // A body ending with its item, on a line that only closes the
        // item's signature (`) {` under a long `fn`), adds nothing.
        if last_end == Some(r.end) && closes_signature(t, head) {
            continue;
        }
        out.push(head);
        last_end = Some(r.end);
    }
}

/// Whether the line starting at `head` begins with a closing bracket or
/// an opening brace: the tail of a signature that began above.
fn closes_signature(t: &dyn Text, head: usize) -> bool {
    let peek = t.slice(head..t.line_span(head).end.min(head + 256));
    peek.trim_start().starts_with([')', ']', '}', '{', '>'])
}

/// The lines to pin above a view whose top row is `top`: the scopes of
/// the line just below the pinned rows. Pinning `n` rows covers the top
/// `n` rows of text, so the line that decides is the one `n` rows down;
/// a few rounds settle `n` (it grows by at least one each round, and at
/// most to [`MAX_ROWS`]).
pub fn lines_for(
    doc: &Document,
    w: &mut WrapView<'_>,
    top: RowPos,
    regions: &mut Vec<Range<usize>>,
    out: &mut Vec<usize>,
) {
    out.clear();
    if !has_scopes(doc) {
        return;
    }
    let t = &doc.buffer;
    let mut n = 0;
    loop {
        let (below, _) = wrap::step_rows(t, w, top, isize::try_from(n).unwrap_or(0));
        out.clear();
        scopes(doc, below.start, regions, out);
        if out.len() <= n || n >= MAX_ROWS {
            break;
        }
        n = out.len();
    }
}

/// Whether the document can have sticky rows: it has a tree (none above
/// 16 MB) and its language a fold query.
fn has_scopes(doc: &Document) -> bool {
    let state = &doc.syntax;
    state.tree().is_some()
        && state
            .language()
            .is_some_and(|l| nimbus_syntax::registry::folds_query(l).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Solo;
    use nimbus_core::wrap::WrapCache;

    fn parsed(text: &str, ext: &str) -> Solo {
        let mut d = Solo::from_text(text);
        d.doc.path = Some(std::path::PathBuf::from(format!("x.{ext}")));
        d.doc
            .detect_language(&crate::languages::Languages::default());
        d.doc.syntax.parse_now(&d.doc.buffer);
        d
    }

    /// The pinned lines' numbers when line `top` is at the top.
    fn pinned(d: &Solo, top: usize) -> Vec<usize> {
        let t = &d.doc.buffer;
        let mut cache = WrapCache::default();
        let mut w = WrapView::new(&mut cache, None, 4);
        let at = RowPos {
            start: t.line_to_byte(top),
            row: 0,
        };
        let (mut regions, mut out) = (Vec::new(), Vec::new());
        lines_for(&d.doc, &mut w, at, &mut regions, &mut out);
        out.iter().map(|&s| t.byte_to_line(s)).collect()
    }

    #[test]
    fn the_enclosing_impl_and_function_stay_pinned() {
        let src = "impl A {\n    fn f() {\n        a();\n        b();\n        c();\n        d();\n        e();\n    }\n}\n";
        let d = parsed(src, "rs");
        assert_eq!(pinned(&d, 0), Vec::<usize>::new(), "nothing is above");
        // Line 2 at the top: pinning two rows covers lines 2 and 3, so the
        // line that decides is 4, inside both.
        assert_eq!(pinned(&d, 2), vec![0, 1]);
        assert_eq!(pinned(&d, 4), vec![0, 1]);
        // Line 6 at the top: line 8 (the impl's `}`) is outside the
        // function, so only the impl stays.
        assert_eq!(pinned(&d, 6), vec![0]);
    }

    #[test]
    fn a_body_on_a_later_line_than_its_item_is_not_pinned_twice() {
        let src = "fn f(\n    a: u8,\n) {\n    one();\n    two();\n    three();\n    four();\n}\n";
        let d = parsed(src, "rs");
        assert_eq!(pinned(&d, 3), vec![0]);
    }

    #[test]
    fn markdown_headings_pin_their_sections() {
        let src = "# Top\n\nintro\n\n## Part\n\none\n\ntwo\n\nthree\n\nfour\n";
        let d = parsed(src, "md");
        assert_eq!(pinned(&d, 6), vec![0, 4]);
    }

    #[test]
    fn plain_text_has_none() {
        let d = Solo::from_text("a\n  b\n  c\n  d\n");
        assert_eq!(pinned(&d, 2), Vec::<usize>::new());
    }
}
