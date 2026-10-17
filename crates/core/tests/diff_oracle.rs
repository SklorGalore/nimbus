//! Property tests for the line diff (DESIGN.md §6, M9 task 1).
//!
//! The oracle is the definition of a correct diff: taking the old text,
//! replacing each hunk's old lines with its new lines, must give the new
//! text byte for byte. The hunks must also be well formed (sorted, apart,
//! in bounds, never empty on both sides), equal texts must have no hunks,
//! and the result must not depend on which buffer backend holds the text
//! or on how its storage is chunked, since the interner reads chunks.
//!
//! Texts are built from pieces that break naive line handling: `\r\n`, a
//! lone `\r`, multi-byte characters, empty lines and lines without a
//! trailing newline. The new text is the old one with a few random splices
//! and a few whole-line moves, so hunks of every shape come up.

use std::ops::Range;
use std::sync::Arc;

use nimbus_core::{
    Buffer, Hunk, Interner, Limits, MappedBuffer, RopeBuffer, Snapshot, Text, VecSource,
};
use proptest::prelude::*;
use proptest::test_runner::Config;

const PIECES: &[&str] = &[
    "a\n",
    "b\n",
    "c\n",
    "a\n",
    "b\n",
    "\n",
    "\r\n",
    "x",
    "y",
    "\r",
    "é\n",
    "🙂\n",
    "\t",
    " ",
    "fn f() {\n",
    "    x\n",
    "}\n",
];

/// Mapped-buffer limits that split storage into tiny pieces, so the chunk
/// walk in the interner crosses a boundary every few bytes.
const TINY: Limits = Limits {
    copy_limit: 4,
    segment: 16,
    max_piece: 6,
    scan: 3,
    pseudo_line: 8,
};

fn text_strategy() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(PIECES), 0..24).prop_map(|v| v.concat())
}

/// `old`, and `new` made from it by a few splices and a line move.
fn pair_strategy() -> impl Strategy<Value = (String, String)> {
    (
        text_strategy(),
        prop::collection::vec(
            (
                any::<prop::sample::Index>(),
                any::<prop::sample::Index>(),
                text_strategy(),
            ),
            0..4,
        ),
        any::<bool>(),
    )
        .prop_map(|(old, splices, move_line)| {
            let mut new = old.clone();
            for (a, b, insert) in splices {
                let bounds: Vec<usize> = new
                    .char_indices()
                    .map(|(i, _)| i)
                    .chain([new.len()])
                    .collect();
                let i = bounds[a.index(bounds.len())];
                let j = bounds[b.index(bounds.len())];
                let (i, j) = (i.min(j), i.max(j));
                new.replace_range(i..j, &insert);
            }
            if move_line {
                let mut lines: Vec<&str> = new.split_inclusive('\n').collect();
                if lines.len() > 1 {
                    let l = lines.remove(0);
                    lines.push(l);
                    new = lines.concat();
                }
            }
            (old, new)
        })
}

fn rope(text: &str) -> Arc<dyn Snapshot> {
    RopeBuffer::from_text(text).snapshot().into()
}

#[expect(clippy::unwrap_used, reason = "test helper")]
fn mapped(text: &str) -> Arc<dyn Snapshot> {
    // Edit the mapped buffer a little so it has add-buffer pieces too, then
    // put the text back, so its storage is many small pieces of both kinds.
    let mut b = MappedBuffer::with_limits(Arc::new(VecSource::new(text.as_bytes().to_vec())), TINY);
    if text.len() >= 2 && text.is_char_boundary(1) {
        let first = text[..1].to_owned();
        b.replace(0..1, "").unwrap();
        b.replace(0..0, &first).unwrap();
    }
    assert_eq!(b.slice(0..b.len_bytes()), text);
    b.snapshot().into()
}

fn line_bytes(t: &dyn Text, lines: Range<usize>) -> String {
    t.slice(t.line_to_byte(lines.start)..t.line_to_byte(lines.end))
        .into_owned()
}

/// The oracle: old text with each hunk applied.
fn apply(old: &dyn Text, old_lines: usize, new: &dyn Text, hunks: &[Hunk]) -> String {
    let mut out = String::new();
    let mut at = 0;
    for h in hunks {
        out.push_str(&line_bytes(old, at..h.old.start));
        out.push_str(&line_bytes(new, h.new.clone()));
        at = h.old.end;
    }
    out.push_str(&line_bytes(old, at..old_lines));
    out
}

#[expect(clippy::unwrap_used, reason = "test helper")]
fn check(old: &str, new: &str, make: fn(&str) -> Arc<dyn Snapshot>) -> Vec<Hunk> {
    let (os, ns) = (make(old), make(new));
    let mut interner = Interner::new();
    let ol = interner.intern(&os).unwrap();
    let nl = interner.intern(&ns).unwrap();
    let expected_old = old.split_inclusive('\n').count();
    assert_eq!(ol.len(), expected_old, "old line count of {old:?}");
    assert_eq!(
        nl.len(),
        new.split_inclusive('\n').count(),
        "new line count of {new:?}"
    );
    let hunks = interner.diff(&ol, &nl);
    let mut prev_old = 0;
    let mut prev_new = 0;
    for h in &hunks {
        assert!(
            h.old.start <= h.old.end && h.old.end <= ol.len(),
            "old range {h:?}"
        );
        assert!(
            h.new.start <= h.new.end && h.new.end <= nl.len(),
            "new range {h:?}"
        );
        assert!(!(h.old.is_empty() && h.new.is_empty()), "empty hunk {h:?}");
        assert!(
            h.old.start >= prev_old && h.new.start >= prev_new,
            "unsorted {hunks:?}"
        );
        // Hunks that touch would be one hunk.
        assert!(
            hunks.len() == 1
                || h.old.start > prev_old
                || h.new.start > prev_new
                || (prev_old == 0 && prev_new == 0),
            "touching {hunks:?}"
        );
        prev_old = h.old.end;
        prev_new = h.new.end;
    }
    assert_eq!(
        apply(&*os, ol.len(), &*ns, &hunks),
        new,
        "hunks {hunks:?} from {old:?}"
    );
    if old == new {
        assert!(hunks.is_empty());
    }
    hunks
}

proptest! {
    #![proptest_config(Config { cases: 400, ..Config::default() })]

    #[test]
    fn hunks_turn_old_into_new_on_rope((old, new) in pair_strategy()) {
        check(&old, &new, rope);
    }

    #[test]
    fn hunks_turn_old_into_new_on_mapped((old, new) in pair_strategy()) {
        check(&old, &new, mapped);
    }

    #[test]
    fn backends_agree((old, new) in pair_strategy()) {
        prop_assert_eq!(check(&old, &new, rope), check(&old, &new, mapped));
    }

    #[test]
    fn interning_in_either_order_agrees((old, new) in pair_strategy()) {
        let mut a = Interner::new();
        let oa = a.intern(&rope(&old)).unwrap();
        let na = a.intern(&rope(&new)).unwrap();
        let mut b = Interner::new();
        let nb = b.intern(&rope(&new)).unwrap();
        let ob = b.intern(&rope(&old)).unwrap();
        prop_assert_eq!(a.diff(&oa, &na), b.diff(&ob, &nb));
        prop_assert_eq!(a.num_tokens(), b.num_tokens());
    }
}
