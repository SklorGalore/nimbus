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

use std::fmt::Write as _;
use std::ops::Range;
use std::sync::Arc;

use nimbus_core::compare::{Options, compare};
use nimbus_core::live_diff::{EditSpan, LiveDiff, Side};
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
        assert!(hunks.is_empty(), "{hunks:?}");
    }
    hunks
}

/// The layered comparison (`compare`) under `options`, checked the same
/// way: well-formed hunks that turn old into new, and line counts that
/// match the interner's.
#[expect(clippy::unwrap_used, reason = "test helper")]
fn check_compare(
    old: &str,
    new: &str,
    make: fn(&str) -> Arc<dyn Snapshot>,
    options: Options,
) -> Vec<Hunk> {
    let (os, ns) = (make(old), make(new));
    let c = compare(&os, &ns, options).unwrap();
    assert_eq!(
        c.old_lines,
        old.split_inclusive('\n').count(),
        "old lines of {old:?}"
    );
    assert_eq!(
        c.new_lines,
        new.split_inclusive('\n').count(),
        "new lines of {new:?}"
    );
    let (mut prev_old, mut prev_new) = (0, 0);
    for (i, h) in c.hunks.iter().enumerate() {
        assert!(
            h.old.start <= h.old.end && h.old.end <= c.old_lines,
            "old range {h:?}"
        );
        assert!(
            h.new.start <= h.new.end && h.new.end <= c.new_lines,
            "new range {h:?}"
        );
        assert!(!(h.old.is_empty() && h.new.is_empty()), "empty hunk {h:?}");
        assert!(
            h.old.start >= prev_old && h.new.start >= prev_new,
            "unsorted {:?}",
            c.hunks
        );
        assert!(
            i == 0 || h.old.start > prev_old || h.new.start > prev_new,
            "touching {:?}",
            c.hunks
        );
        prev_old = h.old.end;
        prev_new = h.new.end;
    }
    assert_eq!(
        apply(&*os, c.old_lines, &*ns, &c.hunks),
        new,
        "hunks {:?} from {old:?} with {options:?}",
        c.hunks
    );
    if old == new {
        assert!(c.hunks.is_empty(), "{:?}", c.hunks);
        assert_eq!(c.lines_diffed, 0);
    }
    if options.limit >= old.len().max(new.len()) {
        assert!(c.exact);
    }
    c.hunks
}

/// A random edit of `text`: a splice at char boundaries.
fn splice_strategy() -> impl Strategy<Value = (prop::sample::Index, prop::sample::Index, String)> {
    (
        any::<prop::sample::Index>(),
        any::<prop::sample::Index>(),
        text_strategy(),
    )
}

fn splice(
    text: &str,
    a: prop::sample::Index,
    b: prop::sample::Index,
    insert: &str,
) -> (usize, usize, String) {
    let bounds: Vec<usize> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain([text.len()])
        .collect();
    let i = bounds[a.index(bounds.len())];
    let j = bounds[b.index(bounds.len())];
    let (i, j) = (i.min(j), i.max(j));
    let mut out = text.to_owned();
    out.replace_range(i..j, insert);
    (i, j - i, out)
}

/// What the app computes from the document before applying an edit.
fn edit_span(before: &str, start: usize, removed: usize, inserted: &str) -> EditSpan {
    EditSpan {
        start,
        first_line: before[..start].matches('\n').count(),
        removed_lines: before[start..start + removed].matches('\n').count(),
        inserted: inserted.len(),
    }
}

/// The live diff's hunks must describe the current texts exactly.
fn check_live(live: &LiveDiff, old: &str, new: &str, log: &str) {
    let (os, ns) = (rope(old), rope(new));
    assert_eq!(live.lines(Side::Old), old.split_inclusive('\n').count());
    assert_eq!(live.lines(Side::New), new.split_inclusive('\n').count());
    let (mut prev_old, mut prev_new) = (0, 0);
    for (i, h) in live.hunks().iter().enumerate() {
        assert!(
            h.old.start <= h.old.end && h.old.end <= live.lines(Side::Old),
            "old range {h:?}"
        );
        assert!(
            h.new.start <= h.new.end && h.new.end <= live.lines(Side::New),
            "new range {h:?}"
        );
        assert!(!(h.old.is_empty() && h.new.is_empty()), "empty hunk {h:?}");
        assert!(
            h.old.start >= prev_old && h.new.start >= prev_new,
            "unsorted {:?}\n{log}",
            live.hunks()
        );
        assert!(
            i == 0 || h.old.start > prev_old || h.new.start > prev_new,
            "touching {:?}\n{log}",
            live.hunks()
        );
        prev_old = h.old.end;
        prev_new = h.new.end;
    }
    assert_eq!(
        apply(&*os, live.lines(Side::Old), &*ns, live.hunks()),
        new,
        "hunks {:?} for {old:?} -> {new:?}",
        live.hunks()
    );
}

/// Every layer on a few bytes: a full diff only under 12 bytes a side,
/// then 8-byte windows, which the widening and the one-block fallback
/// both hit on these texts.
const TINY_OPTIONS: Options = Options {
    limit: 12,
    window: 8,
};

proptest! {
    #![proptest_config(Config { cases: 400, ..Config::default() })]

    #[test]
    fn compare_is_exact_under_the_limit((old, new) in pair_strategy()) {
        check_compare(&old, &new, rope, Options::default());
        check_compare(&old, &new, mapped, Options::default());
    }

    #[test]
    fn compare_in_windows_is_correct((old, new) in pair_strategy()) {
        prop_assert_eq!(
            check_compare(&old, &new, rope, TINY_OPTIONS),
            check_compare(&old, &new, mapped, TINY_OPTIONS)
        );
    }

    #[test]
    fn live_diff_follows_edits(
        (old0, new0) in pair_strategy(),
        edits in prop::collection::vec((any::<bool>(), splice_strategy(), any::<bool>()), 1..8),
    ) {
        let mut live = LiveDiff::new(&rope(&old0), &rope(&new0)).unwrap();
        let mut log = String::new();
        let _ = writeln!(log, "start old={old0:?} new={new0:?} hunks={:?}", live.hunks());
        check_live(&live, &old0, &new0, &log);
        let (mut old, mut new) = (old0, new0);
        let mut pending = None;
        for (on_old, (a, b, insert), defer) in edits {
            let side = if on_old { Side::Old } else { Side::New };
            let text = if on_old { &mut old } else { &mut new };
            let (start, removed, after) = splice(text, a, b, &insert);
            let span = edit_span(text, start, removed, &insert);
            *text = after;
            let region = live.edit(side, &rope(text), span).unwrap();
            let _ = writeln!(log, "edit {side:?} {span:?} -> {text:?}; region {region:?}; hunks {:?}; defer {defer}", live.hunks());
            prop_assert!(region.lines() <= live.lines(Side::Old) + live.lines(Side::New));
            if defer {
                // Leave it pending; the next edit must merge into it.
                pending = Some(region);
            } else {
                live.rediff(&region);
                let _ = writeln!(log, "rediff -> {:?}", live.hunks());
                prop_assert!(live.pending().is_none());
                check_live(&live, &old, &new, &log);
                pending = None;
            }
        }
        if let Some(region) = pending {
            // A stale region is ignored; the current one finishes the job.
            live.rediff(&region);
            let current = live.pending().cloned();
            if let Some(current) = current {
                live.rediff(&current);
            }
            let _ = writeln!(log, "final rediff -> {:?}", live.hunks());
            check_live(&live, &old, &new, &log);
        }
    }

    #[test]
    fn trimming_diffs_only_the_middle((old, new) in pair_strategy(), pad in 1..40usize) {
        // The same common text on both ends of each side: the lines that
        // go through the diff must not grow with it.
        let padding: String = (0..pad).fold(String::new(), |mut s, i| { use std::fmt::Write as _; let _ = writeln!(s, "same {i}"); s });
        let (po, pn) = (format!("{padding}{old}{padding}"), format!("{padding}{new}{padding}"));
        let plain = compare(&rope(&old), &rope(&new), Options::default()).unwrap();
        let padded = compare(&rope(&po), &rope(&pn), Options::default()).unwrap();
        check_compare(&po, &pn, rope, Options::default());
        // Trimming can only cut lines off the diff's input; never add. The
        // join between the texts may make one more line each side.
        prop_assert!(padded.lines_diffed <= plain.lines_diffed + 2, "{} vs {}", padded.lines_diffed, plain.lines_diffed);
    }

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
