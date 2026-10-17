//! Property tests for the three-way merge (M9 task 4).
//!
//! The oracle is the definition of the chunks: they tile all three texts
//! in order, and each kind's claim holds line for line (a `Same` chunk has
//! the same lines in all three texts, an `Ours` chunk has theirs equal to
//! the base, and so on). From that follow the plan's properties: a side
//! that changed alone is taken, identical changes are taken once, a side
//! that did not change at all leaves the result equal to the other side,
//! and every conflict resolved one way gives that side's lines there.

use std::fmt::Write as _;
use std::sync::Arc;

use nimbus_core::live_diff::EditSpan;
use nimbus_core::merge::{Kind, Merge, Pick, lines_into};
use nimbus_core::regions::{Regions, Resolution};
use nimbus_core::{Buffer, RopeBuffer, Snapshot, Text};
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
    "fn f() {\n",
    "    x\n",
    "}\n",
];

fn text_strategy() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(PIECES), 0..20).prop_map(|v| v.concat())
}

/// A text with a few random splices applied.
fn edited(text: &str, splices: &[(prop::sample::Index, prop::sample::Index, String)]) -> String {
    let mut out = text.to_owned();
    for (a, b, insert) in splices {
        let bounds: Vec<usize> = out
            .char_indices()
            .map(|(i, _)| i)
            .chain([out.len()])
            .collect();
        let i = bounds[a.index(bounds.len())];
        let j = bounds[b.index(bounds.len())];
        out.replace_range(i.min(j)..i.max(j), insert);
    }
    out
}

fn splices() -> impl Strategy<Value = Vec<(prop::sample::Index, prop::sample::Index, String)>> {
    prop::collection::vec(
        (
            any::<prop::sample::Index>(),
            any::<prop::sample::Index>(),
            text_strategy(),
        ),
        0..3,
    )
}

fn rope(text: &str) -> Arc<dyn Snapshot> {
    RopeBuffer::from_text(text).snapshot().into()
}

fn lines(t: &dyn Text, r: std::ops::Range<usize>) -> String {
    let mut s = String::new();
    lines_into(t, r, &mut s);
    s
}

#[expect(clippy::unwrap_used, reason = "test helper")]
fn check(base: &str, ours: &str, theirs: &str) -> (Merge, String) {
    let (b, o, t) = (rope(base), rope(ours), rope(theirs));
    let m = Merge::new(&b, &o, &t).unwrap();
    let mut log = String::new();
    let _ = writeln!(
        log,
        "base {base:?} ours {ours:?} theirs {theirs:?}\n{:?}",
        m.chunks
    );
    // Tiling.
    let (mut pb, mut po, mut pt) = (0, 0, 0);
    let mut prev_same = false;
    for c in &m.chunks {
        assert_eq!(
            (c.base.start, c.ours.start, c.theirs.start),
            (pb, po, pt),
            "gap\n{log}"
        );
        assert!(
            c.base.start <= c.base.end
                && c.ours.start <= c.ours.end
                && c.theirs.start <= c.theirs.end,
            "{log}"
        );
        assert!(
            !(c.base.is_empty() && c.ours.is_empty() && c.theirs.is_empty()),
            "empty chunk\n{log}"
        );
        assert!(
            !(prev_same && c.kind == Kind::Same),
            "two Same in a row\n{log}"
        );
        prev_same = c.kind == Kind::Same;
        let (lb, lo, lt) = (
            lines(&*b, c.base.clone()),
            lines(&*o, c.ours.clone()),
            lines(&*t, c.theirs.clone()),
        );
        match c.kind {
            Kind::Same => assert!(
                lb == lo && lo == lt,
                "Same differs: {lb:?} {lo:?} {lt:?}\n{log}"
            ),
            Kind::Ours => assert!(lt == lb && lo != lb, "Ours: {lb:?} {lo:?} {lt:?}\n{log}"),
            Kind::Theirs => assert!(lo == lb && lt != lb, "Theirs: {lb:?} {lo:?} {lt:?}\n{log}"),
            Kind::Both => assert!(lo == lt && lo != lb, "Both: {lb:?} {lo:?} {lt:?}\n{log}"),
            Kind::Conflict => assert!(
                lo != lt && lo != lb && lt != lb,
                "Conflict: {lb:?} {lo:?} {lt:?}\n{log}"
            ),
        }
        (pb, po, pt) = (c.base.end, c.ours.end, c.theirs.end);
    }
    assert_eq!(
        (pb, po, pt),
        (m.lines[0], m.lines[1], m.lines[2]),
        "tail\n{log}"
    );
    assert_eq!(m.lines[0], base.split_inclusive('\n').count());
    let mut out = String::new();
    m.write(&*b, &*o, &*t, |_| Pick::Ours, &mut out);
    (m, out)
}

/// What the document reports for an edit, computed before applying it.
fn edit_span(before: &str, start: usize, removed: usize, inserted: &str) -> EditSpan {
    EditSpan {
        start,
        first_line: before[..start].matches('\n').count(),
        removed_lines: before[start..start + removed].matches('\n').count(),
        inserted: inserted.len(),
    }
}

/// The regions must tile the result exactly, in order, whatever was done.
fn check_regions(r: &Regions, result: &str) {
    let lines = nimbus_core::merge::lines_in(result);
    assert_eq!(r.result_lines(), lines, "{r:?}");
    let mut at = 0;
    for region in r.all() {
        assert_eq!(region.result.start, at, "gap or overlap in {r:?}");
        assert!(region.result.end >= at);
        at = region.result.end;
    }
    assert_eq!(
        at, lines,
        "regions end at {at}, result has {lines} lines: {r:?}"
    );
}

proptest! {
    #![proptest_config(Config { cases: 500, ..Config::default() })]

    #[test]
    fn regions_follow_takes_and_edits(
        base in text_strategy(), s1 in splices(), s2 in splices(),
        steps in prop::collection::vec((any::<bool>(), any::<prop::sample::Index>(), any::<prop::sample::Index>(), any::<prop::sample::Index>(), text_strategy()), 0..6),
    ) {
        let ours = edited(&base, &s1);
        let theirs = edited(&base, &s2);
        let (b, o, t) = (rope(&base), rope(&ours), rope(&theirs));
        let m = Merge::new(&b, &o, &t).unwrap();
        let mut result = String::new();
        let mut ranges = Vec::new();
        m.write_tracked(&*b, &*o, &*t, |_| Pick::Markers, &mut result, &mut ranges);
        let mut regions = Regions::new(&m, &ranges);
        check_regions(&regions, &result);
        let picks = [Pick::Ours, Pick::Theirs, Pick::OursThenTheirs, Pick::Base, Pick::Markers];
        for (take, which, a, bb, insert) in steps {
            let doc = RopeBuffer::from_text(&result);
            if take && !regions.all().is_empty() {
                let i = which.index(regions.all().len());
                let pick = picks[a.index(picks.len())];
                let mut text = String::new();
                let lines = regions.take_text(i, pick, (&*b, &*o, &*t), &mut text).unwrap();
                let (start, end) = (doc.line_to_byte(lines.start), doc.line_to_byte(lines.end));
                result.replace_range(start..end, &text);
                regions.took(i, pick, nimbus_core::merge::lines_in(&text));
                prop_assert_eq!(regions.all()[i].resolution, Resolution::Taken(pick));
            } else {
                let (start, removed, after) = {
                    let bounds: Vec<usize> = result.char_indices().map(|(i, _)| i).chain([result.len()]).collect();
                    let i = bounds[a.index(bounds.len())];
                    let j = bounds[bb.index(bounds.len())];
                    let (i, j) = (i.min(j), i.max(j));
                    let mut s = result.clone();
                    s.replace_range(i..j, &insert);
                    (i, j - i, s)
                };
                let span = edit_span(&result, start, removed, &insert);
                result = after;
                regions.apply_edit(&RopeBuffer::from_text(&result), span);
            }
            check_regions(&regions, &result);
        }
    }

    #[test]
    fn chunks_tile_and_mean_what_they_say(base in text_strategy(), s1 in splices(), s2 in splices()) {
        let ours = edited(&base, &s1);
        let theirs = edited(&base, &s2);
        check(&base, &ours, &theirs);
    }

    #[test]
    fn an_unchanged_side_yields_the_other(base in text_strategy(), s in splices()) {
        let ours = edited(&base, &s);
        let (m, out) = check(&base, &ours, &base);
        prop_assert_eq!(out, ours.clone());
        prop_assert_eq!(m.conflicts(), 0);
        let (m, out) = check(&base, &base, &ours);
        prop_assert_eq!(out, ours);
        prop_assert_eq!(m.conflicts(), 0);
    }

    #[test]
    fn identical_sides_merge_to_themselves(base in text_strategy(), s in splices()) {
        let both = edited(&base, &s);
        let (m, out) = check(&base, &both, &both);
        prop_assert_eq!(out, both);
        prop_assert_eq!(m.conflicts(), 0);
    }

    #[test]
    fn conflicts_resolved_one_way_take_that_side(base in text_strategy(), s1 in splices(), s2 in splices()) {
        let ours = edited(&base, &s1);
        let theirs = edited(&base, &s2);
        let (m, _) = check(&base, &ours, &theirs);
        let (b, o, t) = (rope(&base), rope(&ours), rope(&theirs));
        // Taking ours everywhere: every chunk that is not purely theirs
        // comes from ours, so the result equals ours with only their
        // separate changes applied; in particular, where theirs made no
        // change at all, it is ours exactly.
        if m.chunks.iter().all(|c| c.kind != Kind::Theirs) {
            let mut out = String::new();
            m.write(&*b, &*o, &*t, |_| Pick::Ours, &mut out);
            prop_assert_eq!(out, ours);
        }
        if m.chunks.iter().all(|c| c.kind != Kind::Ours) {
            let mut out = String::new();
            m.write(&*b, &*o, &*t, |_| Pick::Theirs, &mut out);
            prop_assert_eq!(out, theirs);
        }
    }
}
