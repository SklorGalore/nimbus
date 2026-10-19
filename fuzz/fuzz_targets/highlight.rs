//! Fuzz highlighting in every bundled grammar (M26). The tree-sitter
//! parsers are C, generated from each grammar, and they read whatever file
//! the user opens, so a crash in one is a crash of the editor. This parses
//! arbitrary text in each language, applies a few edits the way typing
//! does (`SyntaxState::edit`, then an incremental reparse), and asks for
//! the highlight spans of the whole text and of a window into it. For
//! Markdown it also reads the block outline the preview lays out.
//!
//! The checks are the promises the view relies on: spans are sorted, do
//! not overlap, stay inside the ranges asked for and fall on character
//! boundaries, and every capture index names a capture of its table.
//! Comparing with a fresh parse is not one of them: tree-sitter's error
//! recovery may legitimately build a different tree for broken input
//! after an edit.
//!
//! Needs the grammars, so it sits behind the `syntax` feature:
//! `cargo fuzz run highlight --features syntax`.

#![no_main]

use std::ops::Range;
use std::time::Duration;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use nimbus_core::{Buffer, RopeBuffer, Text};
use nimbus_fuzz::{Pos, Txt, boundaries, span};
use nimbus_syntax::LanguageId;
use nimbus_syntax::highlight::{Highlighter, Span};
use nimbus_syntax::markdown;
use nimbus_syntax::registry;
use nimbus_syntax::state::SyntaxState;

const MAX_TEXT: usize = 4096;

#[derive(Arbitrary, Debug)]
struct Input {
    language: u8,
    text: Txt,
    edits: Vec<(Pos, Pos, Txt)>,
    window: (Pos, Pos),
}

fuzz_target!(|input: Input| {
    let language = LanguageId::ALL[usize::from(input.language) % LanguageId::ALL.len()];
    let text = input.text.build(MAX_TEXT);
    let buf = RopeBuffer::from_text(&text);
    let mut state = SyntaxState::new(Some(language), text.len());
    state.parse_now(&buf);
    let mut h = Highlighter::new();
    check(&mut h, &state, &buf, language);
    let mut buf = buf;
    for (a, b, insert) in input.edits.iter().take(8) {
        let now = buf.slice(0..buf.len_bytes()).into_owned();
        let bounds = boundaries(&now);
        let range = span(a.get(&bounds), b.get(&bounds));
        let insert = insert.build(256);
        if now.len() - range.len() + insert.len() > MAX_TEXT {
            continue;
        }
        let Ok(edit) = buf.replace(range, &insert) else {
            panic!("an edit at character boundaries was refused");
        };
        state.edit(&buf, &edit);
        let mut job = state.refresh(&buf, Duration::MAX);
        while let Some(j) = job {
            job = state.finish(j.run(), &buf, Duration::MAX);
        }
        check(&mut h, &state, &buf, language);
    }
    let all = buf.slice(0..buf.len_bytes()).into_owned();
    let bounds = boundaries(&all);
    let window = span(input.window.0.get(&bounds), input.window.1.get(&bounds));
    spans_ok(&all, std::slice::from_ref(&window), &mut h, &state, &buf);
});

/// The whole text's spans, and for Markdown its blocks.
fn check(h: &mut Highlighter, state: &SyntaxState, buf: &RopeBuffer, language: LanguageId) {
    let text = buf.slice(0..buf.len_bytes()).into_owned();
    spans_ok(&text, &[0..text.len()], h, state, buf);
    if language == LanguageId::Markdown
        && let Some(outline) = markdown::outline(state)
    {
        for block in markdown::blocks(&outline, buf) {
            let r = block.range.clone();
            assert!(r.start <= r.end && r.end <= text.len(), "block {r:?}");
        }
    }
}

fn spans_ok(
    text: &str,
    ranges: &[Range<usize>],
    h: &mut Highlighter,
    state: &SyntaxState,
    buf: &RopeBuffer,
) {
    // Nested languages may need a few rounds, as on screen.
    let mut spans: Vec<Span> = Vec::new();
    for _ in 0..4 {
        spans = h.spans(state, buf, ranges).to_vec();
        if !state.injections_pending() {
            break;
        }
    }
    for w in spans.windows(2) {
        assert!(w[0].end <= w[1].start, "overlapping spans {w:?}");
    }
    for s in &spans {
        assert!(s.start < s.end, "empty span {s:?}");
        assert!(
            ranges.iter().any(|r| r.start <= s.start && s.end <= r.end),
            "span {s:?} outside {ranges:?}"
        );
        assert!(
            text.is_char_boundary(s.start) && text.is_char_boundary(s.end),
            "span {s:?} splits a character"
        );
        let Some(names) = registry::capture_table(s.table) else {
            panic!("span {s:?} names no capture table");
        };
        assert!(
            (s.capture as usize) < names.len(),
            "span {s:?}: no such capture"
        );
    }
}
