//! Which lines of a document can fold, and the whole-file scan behind
//! Fold All and Fold Level N (M13).
//!
//! A region comes from the grammar's fold query when the file has an
//! up-to-date syntax tree and its language has one (M14), else from the
//! bracket that opens at the end of the line, else from indentation.
//! Asking about one line looks at a bounded number of lines or bytes, so
//! it is cheap on any file; the whole-file scan runs on a worker over a
//! snapshot, the way search does.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use nimbus_core::fold::{self, at_depth, bracket_region, indent_region, lines_range};
use nimbus_core::{Buffer, Snapshot, Text};
use nimbus_syntax::structure::{self, FoldSource};

use crate::document::Document;

/// Lines an indentation region may span before the gutter stops
/// offering it: past this, asking would cost a scan of the file.
pub const LINE_LIMIT: usize = 20_000;

/// Bytes a bracket may enclose before it stops counting as a region.
pub const BRACKET_LIMIT: usize = 1 << 20;

/// Lines above the cursor looked at for a region around it, when its own
/// line starts none.
const ENCLOSING_LINES: usize = 2_000;

/// The largest file Fold All scans: the compare view's limit, so the
/// whole-file scans of the editor share one bound.
pub const FOLD_ALL_LIMIT: usize = nimbus_core::DIFF_LIMIT;

/// The fold whose first line is the one holding `pos`, if that line
/// starts a region. `window` is scratch space the caller keeps.
pub fn region_at(
    doc: &Document,
    pos: usize,
    tab_width: usize,
    window: &mut Vec<u8>,
) -> Option<Range<usize>> {
    let t = &doc.buffer;
    let span = t.line_span(pos);
    if let Some(found) = grammar_regions(doc, span.start..span.end.max(span.start + 1)) {
        return found
            .into_iter()
            .filter(|r| t.line_span(r.start).start == span.start)
            .find_map(|r| grammar_fold(t, &r));
    }
    bracket_region(t, pos, BRACKET_LIMIT, window)
        .or_else(|| indent_region(t, pos, tab_width, LINE_LIMIT))
}

/// The innermost fold that would hide the line holding `pos`: the region
/// around the cursor, for Fold when the cursor's own line starts none.
pub fn enclosing(
    doc: &Document,
    pos: usize,
    tab_width: usize,
    window: &mut Vec<u8>,
) -> Option<Range<usize>> {
    let t = &doc.buffer;
    let line = t.line_span(pos).start;
    let hides = |f: &Range<usize>| f.start < line && line <= f.end;
    if let Some(found) = grammar_regions(doc, pos..pos + 1) {
        // Outer regions come first: the innermost is the last that hides
        // the line.
        return found
            .iter()
            .rev()
            .filter_map(|r| grammar_fold(t, r))
            .find(|f| hides(f));
    }
    // Walk up to a line indented less than this one whose region covers
    // it; only those can.
    let level = indent_of(t, line, tab_width);
    let mut at = line;
    for _ in 0..ENCLOSING_LINES {
        let Some(prev) = at.checked_sub(1).map(|p| t.line_span(p).start) else {
            break;
        };
        at = prev;
        let Some(width) = indent_of(t, at, tab_width) else {
            continue;
        };
        if level.is_none_or(|l| width < l)
            && let Some(f) = region_at(doc, at, tab_width, window).filter(|f| hides(f))
        {
            return Some(f);
        }
        if width == 0 {
            break; // nothing above encloses a top-level line
        }
    }
    None
}

/// The grammar's regions within `range` (outermost first), or `None`
/// when the grammar cannot say: no up-to-date tree, or no fold query
/// for the language. Bracket and indentation regions stand in then.
fn grammar_regions(doc: &Document, range: Range<usize>) -> Option<Vec<Range<usize>>> {
    let state = &doc.syntax;
    if !structure::ready(state) {
        return None;
    }
    nimbus_syntax::registry::folds_query(state.language()?)?;
    let mut out = Vec::new();
    structure::fold_regions(state, &doc.buffer, range, &mut out);
    Some(out)
}

/// A grammar region as a fold: from the end of its first line to the end
/// of its last, or of the line before when the last holds only closing
/// brackets, so `}` stays in view as it does for bracket regions.
fn grammar_fold(t: &dyn Text, r: &Range<usize>) -> Option<Range<usize>> {
    let last = t.line_span(r.end.saturating_sub(1).max(r.start));
    let peek = t.slice(last.start..last.end.min(last.start + 64));
    let rest = peek.trim();
    let closers = !rest.is_empty()
        && last.end - last.start <= 64
        && rest
            .chars()
            .all(|c| matches!(c, '}' | ')' | ']' | ';' | ','));
    let end = if closers {
        last.start.checked_sub(1)?
    } else {
        last.start
    };
    lines_range(t, r.start, end)
}

/// Whether the line starting at `start` is only whitespace (judged by
/// its first bytes, as [`fold::indent_regions`] does).
fn blank(t: &dyn Text, start: usize) -> bool {
    let span = t.line_span(start);
    let peek = t.slice(span.start..span.end.min(span.start + fold::INDENT_PEEK));
    span.end - span.start <= fold::INDENT_PEEK && peek.trim().is_empty()
}

/// The indentation of the line starting at `start`, or `None` if blank.
fn indent_of(t: &dyn Text, start: usize, tab_width: usize) -> Option<usize> {
    if blank(t, start) {
        return None;
    }
    let span = t.line_span(start);
    let peek = t.slice(span.start..span.end.min(span.start + fold::INDENT_PEEK));
    let tab = tab_width.max(1);
    let mut col = 0;
    for b in peek.bytes() {
        match b {
            b' ' => col += 1,
            b'\t' => col += tab - col % tab,
            _ => break,
        }
    }
    Some(col)
}

/// A Fold All or Fold Level N waiting for its worker.
#[derive(Debug)]
pub struct FoldAll {
    /// Matches the result to the request.
    pub token: u64,
    /// The document's version when the scan started: a result for an
    /// older text is dropped.
    pub version: u64,
}

/// The regions a worker found, for the view that asked.
#[derive(Debug)]
pub struct FoldResult {
    /// [`FoldAll::token`] of the request.
    pub token: u64,
    /// The folds to make.
    pub regions: Vec<Range<usize>>,
}

/// Tokens for Fold All requests.
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

/// Start a whole-file scan of `doc` for Fold All (`level` `None`) or Fold
/// Level N, sending the folds to `send`. `Err` with a message for the
/// status bar when the file is too large, or no worker could start.
///
/// # Errors
///
/// Returns what to tell the user when the scan cannot run.
pub fn start(
    doc: &Document,
    tab_width: usize,
    level: Option<usize>,
    send: impl FnOnce(FoldResult) + Send + 'static,
) -> Result<FoldAll, String> {
    let len = doc.buffer.len_bytes();
    if len > FOLD_ALL_LIMIT {
        let mb = FOLD_ALL_LIMIT >> 20;
        return Err(format!(
            "Fold All works on files up to {mb} MB; fold regions one at a time here"
        ));
    }
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    let snapshot: Arc<dyn Snapshot> = doc.buffer.snapshot().into();
    let source = FoldSource::of(&doc.syntax);
    let work = move || {
        let regions = scan(&*snapshot, source.as_ref(), tab_width, level);
        send(FoldResult { token, regions });
    };
    std::thread::Builder::new()
        .name("nimbus-fold".to_owned())
        .spawn(work)
        .map_err(|e| format!("Cannot fold: {e}"))?;
    Ok(FoldAll {
        token,
        version: doc.version,
    })
}

/// Every fold of the text (or those `level` deep), from the grammar when
/// there is a tree, else by indentation.
fn scan(
    t: &dyn Text,
    source: Option<&FoldSource>,
    tab_width: usize,
    level: Option<usize>,
) -> Vec<Range<usize>> {
    let mut regions = Vec::new();
    if let Some(source) = source {
        let mut found = Vec::new();
        source.regions(t, 0..t.len_bytes(), &mut found);
        regions.extend(found.iter().filter_map(|r| grammar_fold(t, r)));
    } else {
        fold::indent_regions(t, tab_width, &mut regions, &|| false);
    }
    match level {
        Some(n) => at_depth(regions, n),
        None => regions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indentation_and_brackets_stand_in_without_a_grammar() {
        let src = "fn f() {\n    a\n    b\n}\nplain\n    x\n    y\nend\n";
        let doc = Document::from_text(src);
        let mut window = Vec::new();
        // A bracket region keeps the `}` line in view.
        let f = region_at(&doc, 0, 4, &mut window).unwrap();
        assert_eq!(&src[f.clone()], "\n    a\n    b");
        // An indentation region otherwise.
        let p = src.find("plain").unwrap();
        let g = region_at(&doc, p, 4, &mut window).unwrap();
        assert_eq!(&src[g.clone()], "\n    x\n    y");
        // From inside: the region around the cursor.
        let y = src.find("    y").unwrap();
        assert_eq!(enclosing(&doc, y, 4, &mut window), Some(g));
        assert_eq!(
            enclosing(&doc, src.find("    a").unwrap(), 4, &mut window),
            Some(f)
        );
        assert_eq!(enclosing(&doc, p, 4, &mut window), None);
    }

    #[test]
    fn the_whole_file_scan_finds_each_level() {
        let src = "a\n  b\n    c\n  d\ne\n  f\n";
        let t = nimbus_core::RopeBuffer::from_text(src);
        let all = scan(&t, None, 4, None);
        assert_eq!(all.len(), 3);
        let top = scan(&t, None, 4, Some(1));
        let text: Vec<&str> = top.iter().map(|r| &src[r.clone()]).collect();
        // The empty last line belongs to the last region, as blank lines do.
        assert_eq!(text, ["\n  b\n    c\n  d", "\n  f\n"]);
        assert_eq!(scan(&t, None, 4, Some(2)).len(), 1);
    }
}
