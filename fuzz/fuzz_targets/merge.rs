//! Fuzz the three-way merge.
//!
//! The oracle is the definition of the chunks: they tile all three texts
//! in order, and each kind's claim holds line for line (a `Same` chunk has
//! the same lines in all three texts, an `Ours` chunk has theirs equal to
//! the base, and so on). A side that did not change leaves the result
//! equal to the other side, identical sides merge to themselves, and the
//! regions of the merge view tile the result exactly through any sequence
//! of takes and edits.
//!
//! This is `crates/core/tests/merge_oracle.rs` with the fuzzer choosing
//! the texts and the steps.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use nimbus_core::live_diff::EditSpan;
use nimbus_core::merge::{Kind, Merge, Pick, lines_in, lines_into};
use nimbus_core::regions::{Regions, Resolution};
use nimbus_core::{RopeBuffer, Text};
use nimbus_fuzz::{Pos, Txt, boundaries, rope, span};

const MAX_TEXT: usize = 512;

#[derive(Arbitrary, Debug)]
struct Input {
    base: Txt,
    ours: Side,
    theirs: Side,
    steps: Vec<Step>,
}

/// How a side is made from the base: by splices (so it shares lines with
/// the base, as real edits do), or as unrelated text.
#[derive(Arbitrary, Debug)]
enum Side {
    Edited(Vec<(Pos, Pos, Txt)>),
    Unrelated(Txt),
}

impl Side {
    fn build(&self, base: &str) -> String {
        match self {
            Side::Edited(splices) => {
                let mut out = base.to_owned();
                for (a, b, insert) in splices.iter().take(3) {
                    let bounds = boundaries(&out);
                    out.replace_range(span(a.get(&bounds), b.get(&bounds)), &insert.build(64));
                }
                out
            }
            Side::Unrelated(t) => t.build(MAX_TEXT),
        }
    }
}

/// A step in the merge view: take one side for a region, or edit the
/// result by hand.
#[derive(Arbitrary, Debug)]
enum Step {
    Take { region: u8, pick: u8 },
    Edit { a: Pos, b: Pos, text: Txt },
}

const PICKS: [Pick; 6] = [
    Pick::Ours,
    Pick::Theirs,
    Pick::OursThenTheirs,
    Pick::TheirsThenOurs,
    Pick::Base,
    Pick::Markers,
];

fuzz_target!(|input: Input| {
    let base = input.base.build(MAX_TEXT);
    let ours = input.ours.build(&base);
    let theirs = input.theirs.build(&base);

    check(&base, &ours, &theirs);

    let (m, out) = check(&base, &ours, &base);
    assert_eq!(out, ours, "theirs unchanged");
    assert_eq!(m.conflicts(), 0);
    let (m, out) = check(&base, &base, &ours);
    assert_eq!(out, ours, "ours unchanged");
    assert_eq!(m.conflicts(), 0);
    let (m, out) = check(&base, &ours, &ours);
    assert_eq!(out, ours, "identical sides");
    assert_eq!(m.conflicts(), 0);

    regions(&base, &ours, &theirs, &input.steps);
});

fn lines(t: &dyn Text, r: std::ops::Range<usize>) -> String {
    let mut s = String::new();
    lines_into(t, r, &mut s);
    s
}

/// The merge of the three, checked chunk by chunk; and the text written
/// taking ours for every conflict.
fn check(base: &str, ours: &str, theirs: &str) -> (Merge, String) {
    let (b, o, t) = (rope(base), rope(ours), rope(theirs));
    let m = match Merge::new(&b, &o, &t) {
        Ok(m) => m,
        Err(e) => panic!("merge failed: {e:?}"),
    };
    let log = format!(
        "base {base:?} ours {ours:?} theirs {theirs:?}\n{:?}",
        m.chunks
    );
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
    assert_eq!(m.lines[1], ours.split_inclusive('\n').count());
    assert_eq!(m.lines[2], theirs.split_inclusive('\n').count());
    let mut out = String::new();
    m.write(&*b, &*o, &*t, |_| Pick::Ours, &mut out);
    // Taking ours everywhere gives ours exactly when theirs changed
    // nothing on its own.
    if m.chunks.iter().all(|c| c.kind != Kind::Theirs) {
        assert_eq!(out, ours, "all ours\n{log}");
    }
    if m.chunks.iter().all(|c| c.kind != Kind::Ours) {
        let mut out = String::new();
        m.write(&*b, &*o, &*t, |_| Pick::Theirs, &mut out);
        assert_eq!(out, theirs, "all theirs\n{log}");
    }
    (m, out)
}

/// The regions tile the result exactly, in order.
fn check_regions(r: &Regions, result: &str) {
    let lines = lines_in(result);
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

/// The merge view's regions follow takes and hand edits of the result.
fn regions(base: &str, ours: &str, theirs: &str, steps: &[Step]) {
    let (b, o, t) = (rope(base), rope(ours), rope(theirs));
    let Ok(m) = Merge::new(&b, &o, &t) else {
        panic!("merge failed");
    };
    let mut result = String::new();
    let mut ranges = Vec::new();
    m.write_tracked(&*b, &*o, &*t, |_| Pick::Markers, &mut result, &mut ranges);
    let mut regions = Regions::new(&m, &ranges);
    check_regions(&regions, &result);
    for step in steps.iter().take(8) {
        let doc = RopeBuffer::from_text(&result);
        match step {
            Step::Take { region, pick } if !regions.all().is_empty() => {
                let i = usize::from(*region) % regions.all().len();
                let pick = PICKS[usize::from(*pick) % PICKS.len()];
                let mut text = String::new();
                let Some(lines) = regions.take_text(i, pick, (&*b, &*o, &*t), &mut text) else {
                    panic!(
                        "take_text({i}) refused with {} regions",
                        regions.all().len()
                    );
                };
                let (start, end) = (doc.line_to_byte(lines.start), doc.line_to_byte(lines.end));
                result.replace_range(start..end, &text);
                regions.took(i, pick, lines_in(&text));
                assert_eq!(regions.all()[i].resolution, Resolution::Taken(pick));
            }
            Step::Take { .. } => {}
            Step::Edit { a, b: bb, text } => {
                let bounds = boundaries(&result);
                let range = span(a.get(&bounds), bb.get(&bounds));
                let insert = text.build(32);
                let es = EditSpan {
                    start: range.start,
                    first_line: result[..range.start].matches('\n').count(),
                    removed_lines: result[range.clone()].matches('\n').count(),
                    inserted: insert.len(),
                };
                result.replace_range(range, &insert);
                regions.apply_edit(&RopeBuffer::from_text(&result), es);
            }
        }
        check_regions(&regions, &result);
    }
}
