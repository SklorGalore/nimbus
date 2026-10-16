//! Property test: the simple highlighter for large files keeps checkpoints
//! of its line-to-line state, and an edit must discard exactly the ones it
//! could have changed.
//!
//! The oracle is a new [`Lexical`] with no checkpoints, which scans from
//! the first line. The texts stay under the lookback distance, so that scan
//! is exact; they have many short lines, so checkpoints are used. After
//! every edit, the spans of a random range must be the oracle's.

use nimbus_core::{Buffer, RopeBuffer, Text};
use nimbus_syntax::LanguageId;
use nimbus_syntax::highlight::Span;
use nimbus_syntax::lexical::{self, CHECKPOINT_LINES, Lexical};
use proptest::prelude::*;
use proptest::sample::Index;
use proptest::test_runner::Config;

/// Pieces that open and close comments and strings, across lines.
const PIECES: &[&str] = &[
    "let x = 1;",
    "fn f() {}",
    "/*",
    "*/",
    "\"",
    "'a'",
    "// c",
    "\"\"\"",
    "'''",
    "#",
    "```",
    "`",
    "é",
    "\n",
    "\n",
    "\n",
    "\n",
];

fn config(cases: u32) -> Config {
    Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    }
}

fn text(pieces: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(PIECES), 0..pieces).prop_map(|v| v.concat())
}

fn spans(lex: &Lexical, buf: &RopeBuffer, range: std::ops::Range<usize>) -> Vec<Span> {
    let mut out = Vec::new();
    lex.spans(buf, range, &mut out);
    out
}

/// A char boundary of `s` picked by `i`.
fn boundary(s: &str, i: Index) -> usize {
    let all: Vec<usize> = s.char_indices().map(|(b, _)| b).chain([s.len()]).collect();
    all[i.index(all.len())]
}

proptest! {
    #![proptest_config(config(96))]

    #[test]
    fn checkpointed_spans_match_a_fresh_scan(
        language in prop::sample::select(LanguageId::ALL.to_vec()),
        initial in text(12 * CHECKPOINT_LINES),
        edits in prop::collection::vec((any::<Index>(), any::<Index>(), text(40)), 1..12),
        view in (any::<Index>(), any::<Index>()),
    ) {
        let mut buf = RopeBuffer::from_text(&initial);
        let mut oracle = initial;
        let lex = Lexical::new(lexical::builtin(language));
        for (a, b, insert) in edits {
            // Look at the bottom first, so checkpoints exist to invalidate.
            let len = oracle.len();
            let _ = spans(&lex, &buf, len.saturating_sub(64)..len);
            let (x, y) = (boundary(&oracle, a), boundary(&oracle, b));
            let range = x.min(y)..x.max(y);
            let edit = buf.replace(range.clone(), &insert).unwrap();
            oracle.replace_range(range, &insert);
            lex.edit(&buf, &edit);
            let fresh = Lexical::new(lexical::builtin(language));
            let (p, q) = (boundary(&oracle, view.0), boundary(&oracle, view.1));
            let shown = p.min(q)..p.max(q);
            prop_assert_eq!(
                spans(&lex, &buf, shown.clone()),
                spans(&fresh, &buf, shown),
                "{} after edits", language.name()
            );
            // A range only consults the checkpoint before its first line,
            // so also start a range just after every checkpoint.
            for line in (CHECKPOINT_LINES..buf.len_lines()).step_by(CHECKPOINT_LINES) {
                let start = buf.line_to_byte(line + 1).min(buf.len_bytes());
                let end = buf.line_to_byte(line + 2).min(buf.len_bytes());
                prop_assert_eq!(
                    spans(&lex, &buf, start..end),
                    spans(&fresh, &buf, start..end),
                    "{} at line {}", language.name(), line + 1
                );
            }
        }
        prop_assert_eq!(buf.slice(0..buf.len_bytes()), oracle);
    }
}
