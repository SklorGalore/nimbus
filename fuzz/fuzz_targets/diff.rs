//! Fuzz the line diff and the layered comparison built on it.
//!
//! The oracle is the definition of a correct diff: taking the old text and
//! replacing each hunk's old lines with its new lines must give the new
//! text byte for byte. The hunks must also be well formed (sorted, apart,
//! in bounds, never empty on both sides), equal texts must have no hunks,
//! and the answer must not depend on the buffer backend holding the text.
//! The same holds for `compare` with default options, with tiny limits
//! (so the windowed layer runs on a few bytes), with long lines cut into
//! pieces (`split`, the path for minified files), and for the live diff
//! that follows edits.
//!
//! This is `crates/core/tests/diff_oracle.rs` with the fuzzer choosing the
//! texts and edits.

#![no_main]

use std::ops::Range;
use std::sync::Arc;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use nimbus_core::compare::{Options, compare};
use nimbus_core::live_diff::{EditSpan, LiveDiff, Side};
use nimbus_core::split::{SPLIT_AT, Splits};
use nimbus_core::{Hunk, Interner, Normalize, Snapshot, Text, Whitespace};
use nimbus_fuzz::{Pos, Txt, boundaries, mapped, rope, span};

const MAX_TEXT: usize = 1024;

#[derive(Arbitrary, Debug)]
struct Input {
    old: Txt,
    /// The new text is the old one with these splices...
    splices: Vec<(Pos, Pos, Txt)>,
    /// ...and its first line moved to the end...
    move_line: bool,
    /// ...or, if given, this text instead, unrelated to the old one.
    unrelated: Option<Txt>,
    /// Put a line longer than the split threshold at the start of the old
    /// text (before the splices), built from these words.
    long_line: Option<Vec<u8>>,
    /// Which normalisation to check: whitespace (mod 3) and case.
    whitespace: u8,
    case: bool,
    /// Edits for the live diff: which side, the splice, and whether to
    /// leave the rediff pending until the next edit.
    live: Vec<(bool, Pos, Pos, Txt, bool)>,
}

fuzz_target!(|input: Input| {
    let mut old = input.old.build(MAX_TEXT);
    if let Some(words) = &input.long_line {
        old.insert_str(0, &long_line(words));
    }
    let new = match &input.unrelated {
        Some(t) => t.build(MAX_TEXT),
        None => {
            let mut new = old.clone();
            for (a, b, insert) in input.splices.iter().take(4) {
                let bounds = boundaries(&new);
                new.replace_range(span(a.get(&bounds), b.get(&bounds)), &insert.build(64));
            }
            if input.move_line {
                let mut lines: Vec<&str> = new.split_inclusive('\n').collect();
                if lines.len() > 1 {
                    let l = lines.remove(0);
                    lines.push(l);
                    new = lines.concat();
                }
            }
            new
        }
    };

    let on_rope = check(&old, &new, rope);
    let on_mapped = check(&old, &new, mapped);
    assert_eq!(
        on_rope, on_mapped,
        "backends disagree on {old:?} -> {new:?}"
    );
    interning_order(&old, &new);

    check_compare(&old, &new, rope, Options::default());
    check_compare(&old, &new, mapped, Options::default());
    assert_eq!(
        check_compare(&old, &new, rope, TINY_OPTIONS),
        check_compare(&old, &new, mapped, TINY_OPTIONS),
        "windowed compare: backends disagree on {old:?} -> {new:?}"
    );
    split_compare(&old, &new);

    let whitespace =
        [Whitespace::Exact, Whitespace::Ends, Whitespace::All][usize::from(input.whitespace % 3)];
    normalised_diff(
        &old,
        &new,
        Normalize {
            whitespace,
            case: input.case,
            blank_lines: false,
        },
    );
    blank_lines(&old, &new);

    live_diff(old, new, &input.live);
});

/// A line over the split threshold, from words chosen by the fuzzer.
fn long_line(words: &[u8]) -> String {
    const WORDS: &[&str] = &[
        "alpha", "beta", "gamma", "{\"k\":", "1,", "]", "é", " ", "\r",
    ];
    let mut out = String::new();
    let mut i = 0;
    while out.len() <= SPLIT_AT + 64 {
        let w = words.get(i % words.len().max(1)).copied().unwrap_or(0);
        out.push_str(WORDS[usize::from(w) % WORDS.len()]);
        i += 1;
    }
    out.push('\n');
    out
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

/// Hunks are in bounds, never empty on both sides, sorted, and apart.
fn well_formed(hunks: &[Hunk], old_lines: usize, new_lines: usize, what: &str) {
    let (mut prev_old, mut prev_new) = (0, 0);
    for (i, h) in hunks.iter().enumerate() {
        assert!(
            h.old.start <= h.old.end && h.old.end <= old_lines,
            "old range {h:?} of {old_lines} lines: {what}"
        );
        assert!(
            h.new.start <= h.new.end && h.new.end <= new_lines,
            "new range {h:?} of {new_lines} lines: {what}"
        );
        assert!(
            !(h.old.is_empty() && h.new.is_empty()),
            "empty hunk {h:?}: {what}"
        );
        assert!(
            h.old.start >= prev_old && h.new.start >= prev_new,
            "unsorted {hunks:?}: {what}"
        );
        assert!(
            i == 0 || h.old.start > prev_old || h.new.start > prev_new,
            "touching {hunks:?}: {what}"
        );
        prev_old = h.old.end;
        prev_new = h.new.end;
    }
}

fn check(old: &str, new: &str, make: fn(&str) -> Arc<dyn Snapshot>) -> Vec<Hunk> {
    let (os, ns) = (make(old), make(new));
    let mut interner = Interner::new();
    let (Ok(ol), Ok(nl)) = (interner.intern(&os), interner.intern(&ns)) else {
        panic!("interning failed for {old:?} / {new:?}");
    };
    assert_eq!(
        ol.len(),
        old.split_inclusive('\n').count(),
        "old line count of {old:?}"
    );
    assert_eq!(
        nl.len(),
        new.split_inclusive('\n').count(),
        "new line count of {new:?}"
    );
    let hunks = interner.diff(&ol, &nl);
    let what = format!("{old:?} -> {new:?}");
    well_formed(&hunks, ol.len(), nl.len(), &what);
    assert_eq!(
        apply(&*os, ol.len(), &*ns, &hunks),
        new,
        "hunks {hunks:?}: {what}"
    );
    if old == new {
        assert!(hunks.is_empty(), "{hunks:?}");
    }
    hunks
}

/// Interning old then new, or new then old, gives the same diff.
fn interning_order(old: &str, new: &str) {
    let mut a = Interner::new();
    let mut b = Interner::new();
    let (Ok(oa), Ok(na)) = (a.intern(&rope(old)), a.intern(&rope(new))) else {
        panic!("interning failed");
    };
    let (Ok(nb), Ok(ob)) = (b.intern(&rope(new)), b.intern(&rope(old))) else {
        panic!("interning failed");
    };
    assert_eq!(
        a.diff(&oa, &na),
        b.diff(&ob, &nb),
        "interning order: {old:?} -> {new:?}"
    );
    assert_eq!(a.num_tokens(), b.num_tokens());
}

/// Every layer on a few bytes: a full diff only under 12 bytes a side,
/// then 8-byte windows.
const TINY_OPTIONS: Options = Options {
    limit: 12,
    window: 8,
    normalize: Normalize {
        whitespace: Whitespace::Exact,
        case: false,
        blank_lines: false,
    },
    split: false,
};

fn check_compare(
    old: &str,
    new: &str,
    make: fn(&str) -> Arc<dyn Snapshot>,
    options: Options,
) -> Vec<Hunk> {
    let (os, ns) = (make(old), make(new));
    let c = match compare(&os, &ns, options) {
        Ok(c) => c,
        Err(e) => panic!("compare {old:?} -> {new:?}: {e:?}"),
    };
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
    let what = format!("{old:?} -> {new:?} with {options:?}");
    well_formed(&c.hunks, c.old_lines, c.new_lines, &what);
    assert_eq!(
        apply(&*os, c.old_lines, &*ns, &c.hunks),
        new,
        "hunks {:?}: {what}",
        c.hunks
    );
    if old == new {
        assert!(c.hunks.is_empty(), "{:?}", c.hunks);
        assert_eq!(c.lines_diffed, 0);
    }
    if options.limit >= old.len().max(new.len()) {
        assert!(c.exact, "{what}");
    }
    c.hunks
}

/// A text's units as strings: lines, with cut lines in their pieces (the
/// last piece keeps the line ending).
fn units(t: &dyn Text, splits: &Splits) -> Vec<String> {
    let all = t.slice(0..t.len_bytes()).into_owned();
    let mut out = Vec::new();
    let mut start = 0;
    for (line, l) in all.split_inclusive('\n').enumerate() {
        let span = t.line_span(start);
        let mut piece = 0;
        if splits.piece(line, 0, span).is_none() {
            out.push(l.to_owned());
        } else {
            while let Some((r, _)) = splits.piece(line, piece, span) {
                let last = r.end == span.end;
                let end = if last { start + l.len() } else { r.end };
                out.push(all[r.start..end].to_owned());
                piece += 1;
                if last {
                    break;
                }
            }
        }
        start += l.len();
    }
    out
}

/// `old` units with the hunks applied, taking replacement units from `new`.
fn apply_units(old: &[String], new: &[String], hunks: &[Hunk]) -> String {
    let mut out = String::new();
    let mut at = 0;
    for h in hunks {
        out.extend(old[at..h.old.start].iter().map(String::as_str));
        out.extend(new[h.new.clone()].iter().map(String::as_str));
        at = h.old.end;
    }
    out.extend(old[at..].iter().map(String::as_str));
    out
}

/// With long lines cut into pieces, the hunks are in units and turn the
/// old units into the new ones; the unit counts match the scans.
fn split_compare(old: &str, new: &str) {
    for make in [rope, mapped] {
        let (os, ns) = (make(old), make(new));
        let options = Options {
            split: true,
            ..Options::default()
        };
        let c = match compare(&os, &ns, options) {
            Ok(c) => c,
            Err(e) => panic!("split compare {old:?} -> {new:?}: {e:?}"),
        };
        let (ou, nu) = (units(&*os, &c.old_splits), units(&*ns, &c.new_splits));
        let what = format!("split {old:?} -> {new:?}");
        assert_eq!(c.old_lines, ou.len(), "{what}");
        assert_eq!(c.new_lines, nu.len(), "{what}");
        well_formed(&c.hunks, c.old_lines, c.new_lines, &what);
        assert_eq!(apply_units(&ou, &nu, &c.hunks), new, "{what}");
    }
}

/// Each line of `text`, normalised the way the options describe: an
/// independent model of what the interner compares (as in diff_oracle).
fn normalised(text: &str, n: Normalize) -> Vec<String> {
    text.split_inclusive('\n')
        .map(|l| {
            let ws = |c: char| c.is_ascii_whitespace() || c == '\x0B';
            let mut s: String = match n.whitespace {
                Whitespace::Exact => l.to_owned(),
                Whitespace::Ends => l.trim_matches(ws).to_owned(),
                Whitespace::All => l.chars().filter(|&c| !ws(c)).collect(),
            };
            if n.case {
                s.make_ascii_lowercase();
            }
            let mut s = s.replace('\n', "\u{2}");
            s.push('\u{1}');
            s.push('\n');
            s
        })
        .collect()
}

fn changed(hunks: &[Hunk]) -> usize {
    hunks.iter().map(|h| h.old.len() + h.new.len()).sum()
}

/// With normalisation, the hunks turn the old normalised lines into the
/// new ones and are as small as an exact diff of the normalised texts.
fn normalised_diff(old: &str, new: &str, n: Normalize) {
    if old.contains(['\u{1}', '\u{2}']) || new.contains(['\u{1}', '\u{2}']) {
        // The model marks line ends with these; texts holding them would
        // fool the model, not the code under test.
        return;
    }
    let (no, nn) = (normalised(old, n), normalised(new, n));
    let mut expected = Interner::new();
    let (Ok(eo), Ok(en)) = (
        expected.intern(&rope(&no.concat())),
        expected.intern(&rope(&nn.concat())),
    ) else {
        panic!("interning failed");
    };
    let exact = expected.diff(&eo, &en);
    for make in [rope, mapped] {
        let mut i = Interner::with_options(n, false);
        let (Ok(a), Ok(b)) = (i.intern(&make(old)), i.intern(&make(new))) else {
            panic!("interning failed");
        };
        assert_eq!(a.len(), no.len());
        let hunks = i.diff(&a, &b);
        let what = format!("{old:?} -> {new:?} with {n:?}");
        well_formed(&hunks, a.len(), b.len(), &what);
        assert_eq!(apply_units(&no, &nn, &hunks), nn.concat(), "{what}");
        assert_eq!(
            changed(&hunks),
            changed(&exact),
            "{hunks:?} vs {exact:?}: {what}"
        );
    }
}

/// Ignore Blank Lines drops exactly the hunks made of blank lines.
fn blank_lines(old: &str, new: &str) {
    let n = Normalize {
        blank_lines: true,
        ..Normalize::default()
    };
    let mut i = Interner::with_options(n, false);
    let (Ok(a), Ok(b)) = (i.intern(&rope(old)), i.intern(&rope(new))) else {
        panic!("interning failed");
    };
    let kept = i.diff(&a, &b);
    let mut e = Interner::new();
    let (Ok(ea), Ok(eb)) = (e.intern(&rope(old)), e.intern(&rope(new))) else {
        panic!("interning failed");
    };
    // Whitespace as the diff defines it: ASCII whitespace plus vertical
    // tab, which `u8::is_ascii_whitespace` leaves out.
    let blank = |s: &str| s.bytes().all(|b| b.is_ascii_whitespace() || b == 0x0B);
    let ol: Vec<&str> = old.split_inclusive('\n').collect();
    let nl: Vec<&str> = new.split_inclusive('\n').collect();
    let expected: Vec<Hunk> = e
        .diff(&ea, &eb)
        .into_iter()
        .filter(|h| {
            !(ol[h.old.clone()].iter().all(|l| blank(l))
                && nl[h.new.clone()].iter().all(|l| blank(l)))
        })
        .collect();
    assert_eq!(kept, expected, "blank lines: {old:?} -> {new:?}");
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
fn check_live(live: &LiveDiff, old: &str, new: &str) {
    let (os, ns) = (rope(old), rope(new));
    assert_eq!(live.lines(Side::Old), old.split_inclusive('\n').count());
    assert_eq!(live.lines(Side::New), new.split_inclusive('\n').count());
    let what = format!("live {old:?} -> {new:?}");
    well_formed(
        live.hunks(),
        live.lines(Side::Old),
        live.lines(Side::New),
        &what,
    );
    assert_eq!(
        apply(&*os, live.lines(Side::Old), &*ns, live.hunks()),
        new,
        "hunks {:?}: {what}",
        live.hunks()
    );
}

/// The live diff follows edits on either side, rediffed at once or left
/// pending until the next edit.
fn live_diff(mut old: String, mut new: String, edits: &[(bool, Pos, Pos, Txt, bool)]) {
    let mut live = match LiveDiff::new(&rope(&old), &rope(&new)) {
        Ok(l) => l,
        Err(e) => panic!("live diff of {old:?} -> {new:?}: {e:?}"),
    };
    check_live(&live, &old, &new);
    let mut pending = None;
    for (on_old, a, b, insert, defer) in edits.iter().take(8) {
        let side = if *on_old { Side::Old } else { Side::New };
        let text = if *on_old { &mut old } else { &mut new };
        let bounds = boundaries(text);
        let range = span(a.get(&bounds), b.get(&bounds));
        let insert = insert.build(64);
        let es = edit_span(text, range.start, range.len(), &insert);
        text.replace_range(range, &insert);
        let region = match live.edit(side, &rope(text), es) {
            Ok(r) => r,
            Err(e) => panic!("live edit {es:?}: {e:?}"),
        };
        assert!(region.lines() <= live.lines(Side::Old) + live.lines(Side::New));
        if *defer {
            pending = Some(region);
        } else {
            live.rediff(&region);
            assert!(live.pending().is_none());
            check_live(&live, &old, &new);
            pending = None;
        }
    }
    if let Some(region) = pending {
        live.rediff(&region);
        if let Some(current) = live.pending().cloned() {
            live.rediff(&current);
        }
        check_live(&live, &old, &new);
    }
}
