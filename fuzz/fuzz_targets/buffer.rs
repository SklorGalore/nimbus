//! Fuzz the text buffers: an arbitrary edit script, applied to a
//! [`RopeBuffer`], to a [`MappedBuffer`] with tiny limits (so its piece
//! table, add buffer and line index paths all run on a few bytes) and to a
//! plain `String`, the oracle. After every step everything the buffer can
//! answer (text, length, lines, line starts and ends, offset conversions,
//! storage chunks, snapshots) must agree with the oracle.
//!
//! The script mixes valid edits at char boundaries, edits at raw offsets
//! (which must be refused with the right error and change nothing when
//! they are not valid), batches through `replace_sorted`, and undo and
//! redo through [`History`]. Each edit is recorded a second after the one
//! before, past the coalescing window, so every edit is its own undo
//! step and the oracle for undo is a simple stack of texts.
//!
//! This is `crates/core/tests/buffer_oracle.rs` and `undo_oracle.rs` with
//! the fuzzer choosing the inputs instead of proptest strategies.

#![no_main]

use std::ops::Range;
use std::time::{Duration, Instant};

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use nimbus_core::{Buffer, Edit, EditError, History, Selection, Selections, Snapshot};
use nimbus_fuzz::{Pos, Txt, boundaries, check_text, mapped_buffer, rope_buffer, span};

/// Longest starting text and longest insertion, in bytes.
const MAX_INITIAL: usize = 512;
const MAX_INSERT: usize = 64;
/// Most steps per input.
const MAX_OPS: usize = 64;

#[derive(Arbitrary, Debug)]
struct Input {
    initial: Txt,
    /// Repeat the starting text this many times (mod 8, plus one), so some
    /// inputs span several rope chunks (about 1 KB each) and lines and
    /// slices that cross chunk boundaries get compared too.
    stretch: u8,
    ops: Vec<Op>,
}

#[derive(Arbitrary, Debug)]
enum Op {
    Insert {
        at: Pos,
        text: Txt,
    },
    Delete {
        a: Pos,
        b: Pos,
    },
    Replace {
        a: Pos,
        b: Pos,
        text: Txt,
    },
    /// A replacement at raw byte offsets: applied if valid, otherwise it
    /// must be refused with the right error and change nothing.
    Raw {
        start: u16,
        end: u16,
        text: Txt,
    },
    /// Several replacements at once, sorted and apart: Replace All
    /// (recorded with `record_group`) or typing at several cursors
    /// (`batch`, recorded with `record_batch`).
    Sorted {
        parts: Vec<(Pos, Pos, Txt)>,
        batch: bool,
    },
    /// `replace_sorted` with raw ranges: applied if valid and in order,
    /// otherwise refused with nothing changed.
    RawSorted(Vec<(u16, u16, Txt)>),
    Undo,
    Redo,
    Snapshot,
}

fuzz_target!(|input: Input| {
    let base = input.initial.build(MAX_INITIAL);
    let initial = base.repeat(usize::from(input.stretch % 8) + 1);
    let ops = &input.ops[..input.ops.len().min(MAX_OPS)];
    run(rope_buffer(&initial), &initial, ops);
    run(mapped_buffer(&initial), &initial, ops);
});

/// One undo step in the oracle: the text before and after it.
struct Step {
    before: String,
    after: String,
}

struct Session<B: Buffer> {
    buf: B,
    text: String,
    hist: History,
    undo: Vec<Step>,
    redo: Vec<Step>,
    now: Instant,
    snapshots: Vec<(Box<dyn Snapshot>, String)>,
}

fn run<B: Buffer>(buf: B, initial: &str, ops: &[Op]) {
    let mut s = Session {
        buf,
        text: initial.to_owned(),
        hist: History::new(),
        undo: Vec::new(),
        redo: Vec::new(),
        now: Instant::now(),
        snapshots: Vec::new(),
    };
    check_text(&s.buf, &s.text);
    for op in ops {
        s.step(op);
        check_text(&s.buf, &s.text);
        for (snap, then) in &s.snapshots {
            assert_eq!(snap.len_bytes(), then.len(), "snapshot length");
            assert_eq!(snap.slice(0..usize::MAX), then.as_str(), "snapshot text");
        }
    }
    for (snap, then) in &s.snapshots {
        check_text(&**snap, then);
    }
    // Undoing everything gives back the starting text.
    while !s.undo.is_empty() {
        s.step(&Op::Undo);
    }
    assert_eq!(s.text, initial, "undo all");
    assert_eq!(s.buf.slice(0..usize::MAX), initial, "undo all");
}

/// The error a single replacement of `range` must give on `text`, or
/// `None` if it is valid. `Err(())` when more than one thing is wrong in a
/// way the contract does not order, so only "some error" is checked.
fn expected_error(text: &str, range: &Range<usize>) -> Option<Result<EditError, ()>> {
    let len = text.len();
    if range.start > range.end || range.end > len {
        return Some(Ok(EditError::OutOfBounds {
            range: range.clone(),
            len,
        }));
    }
    let bad: Vec<usize> = [range.start, range.end]
        .into_iter()
        .filter(|&o| !text.is_char_boundary(o))
        .collect();
    match bad[..] {
        [] => None,
        [offset] => Some(Ok(EditError::NotCharBoundary { offset })),
        [a, b] if a == b => Some(Ok(EditError::NotCharBoundary { offset: a })),
        _ => Some(Err(())),
    }
}

impl<B: Buffer> Session<B> {
    fn sel(head: usize) -> Selections {
        Selections::single(Selection { anchor: head, head })
    }

    /// Apply a valid replacement to buffer and oracle, check the record
    /// the buffer returns, and record it as one undo step.
    fn replace(&mut self, range: Range<usize>, text: &str) {
        let before = self.text.clone();
        let got = self.buf.replace(range.clone(), text);
        let edit = match got {
            Ok(e) => e,
            Err(e) => panic!("replace({range:?}, {text:?}) on {before:?}: {e:?}"),
        };
        check_edit(&edit, range.start, &before[range.clone()], text);
        self.text.replace_range(range.clone(), text);
        assert_eq!(edit.old_range(), range, "old_range");
        assert_eq!(
            edit.new_range(),
            range.start..range.start + text.len(),
            "new_range"
        );
        self.now += Duration::from_secs(1);
        let (b, a) = (Self::sel(range.start), Self::sel(edit.new_range().end));
        self.hist.record(edit, &b, &a, self.now);
        self.recorded(before);
    }

    /// A new undo step was recorded: it clears the redo stack.
    fn recorded(&mut self, before: String) {
        self.redo.clear();
        self.undo.push(Step {
            before,
            after: self.text.clone(),
        });
    }

    /// `replace_sorted` of `items`, which must be valid; checked against
    /// applying them one by one from the last, and recorded as one group
    /// (with `record_batch` if `batch`, else `record_group`).
    fn sorted(&mut self, items: &[(Range<usize>, &str)], batch: bool) {
        let before = self.text.clone();
        let edits = match self.buf.replace_sorted(items) {
            Ok(e) => e,
            Err(e) => panic!("replace_sorted({items:?}) on {before:?}: {e:?}"),
        };
        assert_eq!(edits.len(), items.len(), "one edit per item");
        for ((range, t), edit) in items.iter().rev().zip(&edits) {
            check_edit(edit, range.start, &self.text[range.clone()], t);
            self.text.replace_range(range.clone(), t);
        }
        if edits.is_empty() {
            return;
        }
        let head = edits.last().map_or(0, |e| e.new_range().end);
        let (b, a) = (Self::sel(head), Self::sel(head));
        if batch {
            self.now += Duration::from_secs(1);
            self.hist.record_batch(edits, &b, &a, self.now);
        } else {
            self.hist.record_group(edits, &b, &a);
        }
        self.recorded(before);
    }

    fn step(&mut self, op: &Op) {
        let bounds = boundaries(&self.text);
        match op {
            Op::Insert { at, text } => {
                let p = at.get(&bounds);
                self.replace(p..p, &text.build(MAX_INSERT));
            }
            Op::Delete { a, b } => self.replace(span(a.get(&bounds), b.get(&bounds)), ""),
            Op::Replace { a, b, text } => {
                self.replace(
                    span(a.get(&bounds), b.get(&bounds)),
                    &text.build(MAX_INSERT),
                );
            }
            Op::Raw { start, end, text } => {
                let range = usize::from(*start)..usize::from(*end);
                let text = text.build(MAX_INSERT);
                match expected_error(&self.text, &range) {
                    None => self.replace(range, &text),
                    Some(want) => {
                        let got = self.buf.replace(range.clone(), &text);
                        match want {
                            Ok(err) => assert_eq!(
                                got,
                                Err(err),
                                "replace({range:?}, {text:?}) on {:?}",
                                self.text
                            ),
                            Err(()) => assert!(
                                got.is_err(),
                                "replace({range:?}, {text:?}) on {:?}: {got:?}",
                                self.text
                            ),
                        }
                    }
                }
            }
            Op::Sorted { parts, batch } => {
                let parts = &parts[..parts.len().min(8)];
                let mut ends: Vec<usize> = parts
                    .iter()
                    .flat_map(|(a, b, _)| [a.get(&bounds), b.get(&bounds)])
                    .collect();
                ends.sort_unstable();
                let texts: Vec<String> = parts.iter().map(|(_, _, t)| t.build(16)).collect();
                let items: Vec<(Range<usize>, &str)> = texts
                    .iter()
                    .enumerate()
                    .map(|(i, t)| (ends[2 * i]..ends[2 * i + 1], t.as_str()))
                    .collect();
                self.sorted(&items, *batch);
            }
            Op::RawSorted(parts) => {
                let parts = &parts[..parts.len().min(8)];
                let texts: Vec<String> = parts.iter().map(|(_, _, t)| t.build(16)).collect();
                let items: Vec<(Range<usize>, &str)> = parts
                    .iter()
                    .zip(&texts)
                    .map(|((a, b, _), t)| (usize::from(*a)..usize::from(*b), t.as_str()))
                    .collect();
                let mut prev_end = 0;
                let valid = items.iter().all(|(r, _)| {
                    let ok = expected_error(&self.text, r).is_none() && r.start >= prev_end;
                    prev_end = r.end;
                    ok
                });
                if valid {
                    self.sorted(&items, false);
                } else {
                    let got = self.buf.replace_sorted(&items);
                    assert!(
                        got.is_err(),
                        "replace_sorted({items:?}) on {:?}: {got:?}",
                        self.text
                    );
                }
            }
            Op::Undo => {
                let mut mirror = self.text.clone();
                let got = self.hist.undo(&mut self.buf, &mut replay_into(&mut mirror));
                let got = match got {
                    Ok(g) => g,
                    Err(e) => panic!("undo on {:?}: {e:?}", self.text),
                };
                match self.undo.pop() {
                    Some(step) => {
                        assert!(got.is_some(), "undo had a step but returned None");
                        assert_eq!(self.buf.slice(0..usize::MAX), step.before, "undo");
                        self.text.clone_from(&step.before);
                        self.redo.push(step);
                    }
                    None => assert!(got.is_none(), "undo with nothing to undo"),
                }
                assert_eq!(mirror, self.text, "undo reported edits that do not replay");
            }
            Op::Redo => {
                let mut mirror = self.text.clone();
                let got = self.hist.redo(&mut self.buf, &mut replay_into(&mut mirror));
                let got = match got {
                    Ok(g) => g,
                    Err(e) => panic!("redo on {:?}: {e:?}", self.text),
                };
                match self.redo.pop() {
                    Some(step) => {
                        assert!(got.is_some(), "redo had a step but returned None");
                        assert_eq!(self.buf.slice(0..usize::MAX), step.after, "redo");
                        self.text.clone_from(&step.after);
                        self.undo.push(step);
                    }
                    None => assert!(got.is_none(), "redo with nothing to redo"),
                }
                assert_eq!(mirror, self.text, "redo reported edits that do not replay");
            }
            Op::Snapshot => {
                if self.snapshots.len() < 4 {
                    self.snapshots
                        .push((self.buf.snapshot(), self.text.clone()));
                }
            }
        }
    }
}

/// The bytes of an edit's content, read from the buffer's storage when it
/// is held as pieces.
fn bytes_of(content: &nimbus_core::Content) -> Vec<u8> {
    let mut out = Vec::new();
    assert!(content.read_into(&mut out).is_ok(), "read content");
    out
}

/// `edit` records replacing `removed` at `start` with `inserted`.
fn check_edit(edit: &Edit, start: usize, removed: &str, inserted: &str) {
    assert_eq!(edit.start, start, "edit start");
    assert_eq!(
        bytes_of(&edit.inserted),
        inserted.as_bytes(),
        "edit inserted"
    );
    assert_eq!(bytes_of(&edit.removed), removed.as_bytes(), "edit removed");
    assert_eq!(edit.removed.len(), removed.len(), "edit removed length");
    assert_eq!(
        edit.removed.has_newline(),
        removed.contains('\n'),
        "edit removed line breaks"
    );
}

/// A callback for undo and redo that applies each reported edit to
/// `mirror`; the edits must replay to the buffer's new text.
fn replay_into(mirror: &mut String) -> impl FnMut(&dyn Buffer, &Edit) + '_ {
    move |after, edit| {
        let bytes = bytes_of(&edit.inserted);
        let inserted = match std::str::from_utf8(&bytes) {
            Ok(s) => s,
            Err(e) => panic!("undo/redo inserted invalid UTF-8 {bytes:?}: {e}"),
        };
        assert_eq!(after.slice(edit.new_range()), inserted, "reported insert");
        assert_eq!(
            bytes_of(&edit.removed).len(),
            edit.removed.len(),
            "removed length"
        );
        let range = edit.old_range();
        assert!(
            range.end <= mirror.len()
                && mirror.is_char_boundary(range.start)
                && mirror.is_char_boundary(range.end),
            "reported edit {range:?} does not fit {mirror:?}"
        );
        mirror.replace_range(range, inserted);
    }
}
