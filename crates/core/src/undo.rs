//! Undo and redo.
//!
//! The history is a list of *groups*. A group is the edits one undo step
//! reverses, plus the selections before and after, so undo also puts the
//! cursor back. Typing coalesces: each keystroke is an edit, but a word typed
//! in one go undoes in one step. A new group starts when any of these happen:
//! the cursor moved, the kind of edit changed (typing then deleting), the edit
//! is not contiguous with the last one, more than [`COALESCE_WINDOW`] passed,
//! or a line break was typed. Branching undo trees are a non-goal (DESIGN.md
//! §4.1): a new edit after undoing clears the redo list.

use std::time::{Duration, Instant};

use std::ops::Range;

use crate::buffer::{Buffer, Content, Edit, EditError, Text, report_sorted};
use crate::selection::Selections;

/// Pause after which typing starts a new undo step.
pub const COALESCE_WINDOW: Duration = Duration::from_millis(500);

/// One undo step.
#[derive(Debug, Clone)]
pub struct Group {
    /// Unique, increasing id. Lets a document remember which step was on
    /// top when it was saved, so undoing back to that point reads as clean.
    pub id: u64,
    /// The edits, in the order they were applied.
    pub edits: Vec<Edit>,
    /// Selections before the first edit.
    pub before: Selections,
    /// Selections after the last edit.
    pub after: Selections,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Inserted text without a line break, removing nothing.
    Typing,
    /// Removed text, inserting nothing.
    Deleting,
    /// Anything else: replacements, pastes, line breaks. Never coalesces.
    Other,
}

fn kind(edit: &Edit) -> Kind {
    if edit.removed.is_empty() && !edit.inserted.is_empty() && !edit.inserted.has_newline() {
        Kind::Typing
    } else if !edit.removed.is_empty() && edit.inserted.is_empty() {
        Kind::Deleting
    } else {
        Kind::Other
    }
}

/// Whether a group's edits were applied from the end of the text to the
/// start without overlapping, as Replace All applies them. Such a group can
/// be undone or redone with one [`Buffer::put_sorted`] instead of one put
/// per edit, which for a mapped buffer is the difference between linear and
/// quadratic time.
fn descending(edits: &[Edit]) -> bool {
    edits.len() > 1
        && edits
            .windows(2)
            .all(|w| matches!(w, [prev, next] if next.old_range().end <= prev.start))
}

/// Whether `next` continues `prev` without a gap.
fn contiguous(prev: &Edit, next: &Edit) -> bool {
    match kind(next) {
        // Typing continues where the previous insertion ended.
        Kind::Typing => next.start == prev.new_range().end,
        // Backspace removes text ending where the last deletion started;
        // Delete removes text starting at the same place.
        Kind::Deleting => next.old_range().end == prev.start || next.start == prev.start,
        Kind::Other => false,
    }
}

/// The undo and redo stacks for one document.
#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Group>,
    redo: Vec<Group>,
    /// Whether the newest undo group may still absorb edits, and when it
    /// last did.
    open_since: Option<Instant>,
    next_id: u64,
}

impl History {
    /// An empty history.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an edit that was just applied, at time `now`.
    pub fn record(&mut self, edit: Edit, before: &Selections, after: &Selections, now: Instant) {
        self.redo.clear();
        let joins = self
            .open_since
            .is_some_and(|t| now.duration_since(t) < COALESCE_WINDOW)
            && kind(&edit) != Kind::Other
            && self
                .undo
                .last()
                .and_then(|g| g.edits.last())
                .is_some_and(|prev| kind(prev) == kind(&edit) && contiguous(prev, &edit));
        let other = kind(&edit) == Kind::Other;
        if joins && let Some(group) = self.undo.last_mut() {
            group.edits.push(edit);
            group.after = after.clone();
        } else {
            let id = self.new_id();
            self.undo.push(Group {
                id,
                edits: vec![edit],
                before: before.clone(),
                after: after.clone(),
            });
        }
        // A paste or line break is its own step: nothing joins it.
        self.open_since = if other { None } else { Some(now) };
    }

    /// Record several edits applied together as one step, such as replace
    /// all. `edits` must be in the order they were applied.
    pub fn record_group(&mut self, edits: Vec<Edit>, before: &Selections, after: &Selections) {
        if edits.is_empty() {
            // Nothing changed, but this was still a distinct action: typing
            // after it must not join the typing before it.
            self.seal();
            return;
        }
        self.redo.clear();
        let id = self.new_id();
        self.undo.push(Group {
            id,
            edits,
            before: before.clone(),
            after: after.clone(),
        });
        self.open_since = None;
    }

    fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Id of the newest undo step, or 0 if there is none. Compare with the
    /// value saved at the last save to tell whether the text has changed.
    /// Call [`History::seal`] when saving, so later typing cannot join the
    /// saved step.
    #[must_use]
    pub fn top_id(&self) -> u64 {
        self.undo.last().map_or(0, |g| g.id)
    }

    /// End the current group, so the next edit starts a new undo step.
    /// Called when the cursor moves or the document is saved.
    pub fn seal(&mut self) {
        self.open_since = None;
    }

    /// Whether there is anything to undo.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether there is anything to redo.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Reverse the newest group in `buffer`. Returns the selections to
    /// restore, or `None` if there was nothing to undo.
    ///
    /// `on_edit` is called with each edit made and the text after it, for
    /// the syntax tree. A group of disjoint edits made from the end of the
    /// text back to the start (Replace All, typing with several cursors) is
    /// put back in one [`Buffer::put_sorted`]; its edits are then reported
    /// as [`report_sorted`] describes, all with the text after the whole
    /// group, which matches the text after each edit up to its new end.
    ///
    /// # Errors
    ///
    /// Returns an [`EditError`] if the buffer no longer matches the history,
    /// which means the buffer was edited without recording. The group is
    /// kept, and edits reversed before the error stay reversed.
    pub fn undo(
        &mut self,
        buffer: &mut dyn Buffer,
        on_edit: &mut dyn FnMut(&dyn Text, &Edit),
    ) -> Result<Option<Selections>, EditError> {
        self.seal();
        let Some(group) = self.undo.pop() else {
            return Ok(None);
        };
        if descending(&group.edits) {
            // Each edit's place in the text as it is now: shifted by every
            // edit before it in the text, which were applied after it.
            let mut shift = 0isize;
            let items: Vec<(Range<usize>, &Content)> = group
                .edits
                .iter()
                .rev()
                .map(|e| {
                    let r = e.new_range();
                    let start = r.start.saturating_add_signed(shift);
                    shift += e.growth();
                    (start..start + r.len(), &e.removed)
                })
                .collect();
            return match buffer.put_sorted(&items) {
                Ok(mut applied) => {
                    report_sorted(&mut applied, |edit| on_edit(&*buffer, edit));
                    let restore = group.before.clone();
                    self.redo.push(group);
                    Ok(Some(restore))
                }
                Err(e) => {
                    self.undo.push(group);
                    Err(e)
                }
            };
        }
        for edit in group.edits.iter().rev() {
            match buffer.put(edit.new_range(), &edit.removed) {
                Ok(applied) => on_edit(&*buffer, &applied),
                Err(e) => {
                    self.undo.push(group);
                    return Err(e);
                }
            }
        }
        let restore = group.before.clone();
        self.redo.push(group);
        Ok(Some(restore))
    }

    /// Re-apply the most recently undone group. Returns the selections to
    /// restore, or `None` if there was nothing to redo.
    ///
    /// # Errors
    ///
    /// As for [`History::undo`].
    pub fn redo(
        &mut self,
        buffer: &mut dyn Buffer,
        on_edit: &mut dyn FnMut(&dyn Text, &Edit),
    ) -> Result<Option<Selections>, EditError> {
        self.seal();
        let Some(group) = self.redo.pop() else {
            return Ok(None);
        };
        if descending(&group.edits) {
            // Every edit's old range is in the text before the group.
            let items: Vec<(Range<usize>, &Content)> = group
                .edits
                .iter()
                .rev()
                .map(|e| (e.old_range(), &e.inserted))
                .collect();
            return match buffer.put_sorted(&items) {
                Ok(mut applied) => {
                    report_sorted(&mut applied, |edit| on_edit(&*buffer, edit));
                    let restore = group.after.clone();
                    self.undo.push(group);
                    Ok(Some(restore))
                }
                Err(e) => {
                    self.redo.push(group);
                    Err(e)
                }
            };
        }
        for edit in &group.edits {
            match buffer.put(edit.old_range(), &edit.inserted) {
                Ok(applied) => on_edit(&*buffer, &applied),
                Err(e) => {
                    self.redo.push(group);
                    return Err(e);
                }
            }
        }
        let restore = group.after.clone();
        self.undo.push(group);
        Ok(Some(restore))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RopeBuffer, Selection, Text};

    struct Doc {
        buf: RopeBuffer,
        hist: History,
        cursor: usize,
        t: Instant,
    }

    impl Doc {
        fn new(text: &str) -> Self {
            Self {
                buf: RopeBuffer::from_text(text),
                hist: History::new(),
                cursor: text.len(),
                t: Instant::now(),
            }
        }

        fn sels(&self) -> Selections {
            Selections::single(Selection::cursor(self.cursor))
        }

        fn edit(&mut self, range: std::ops::Range<usize>, text: &str, after_ms: u64) {
            self.t += Duration::from_millis(after_ms);
            let before = self.sels();
            let e = self.buf.replace(range, text).unwrap();
            self.cursor = e.new_range().end;
            let after = self.sels();
            self.hist.record(e, &before, &after, self.t);
        }

        fn ty(&mut self, s: &str) {
            for ch in s.chars() {
                let mut b = [0; 4];
                self.edit(self.cursor..self.cursor, ch.encode_utf8(&mut b), 50);
            }
        }

        fn text(&self) -> String {
            self.buf.slice(0..self.buf.len_bytes()).into_owned()
        }

        fn undo(&mut self) -> Option<usize> {
            let s = self.hist.undo(&mut self.buf, &mut |_, _| {}).unwrap()?;
            self.cursor = s.primary().head;
            Some(self.cursor)
        }

        fn redo(&mut self) -> Option<usize> {
            let s = self.hist.redo(&mut self.buf, &mut |_, _| {}).unwrap()?;
            self.cursor = s.primary().head;
            Some(self.cursor)
        }
    }

    #[test]
    fn a_typed_word_undoes_in_one_step_and_restores_the_cursor() {
        let mut d = Doc::new("x ");
        d.ty("hello");
        assert_eq!(d.text(), "x hello");
        assert_eq!(d.undo(), Some(2));
        assert_eq!(d.text(), "x ");
        assert_eq!(d.redo(), Some(7));
        assert_eq!(d.text(), "x hello");
    }

    #[test]
    fn a_pause_starts_a_new_step() {
        let mut d = Doc::new("");
        d.ty("ab");
        d.t += COALESCE_WINDOW;
        d.ty("cd");
        d.undo();
        assert_eq!(d.text(), "ab");
    }

    #[test]
    fn moving_the_cursor_seals_the_step() {
        let mut d = Doc::new("");
        d.ty("ab");
        d.hist.seal();
        d.ty("cd");
        d.undo();
        assert_eq!(d.text(), "ab");
    }

    #[test]
    fn switching_from_typing_to_deleting_starts_a_new_step() {
        let mut d = Doc::new("");
        d.ty("abc");
        d.edit(2..3, "", 50);
        d.edit(1..2, "", 50);
        assert_eq!(d.text(), "a");
        d.undo();
        assert_eq!(d.text(), "abc", "both backspaces undo together");
        d.undo();
        assert_eq!(d.text(), "");
    }

    #[test]
    fn non_contiguous_typing_does_not_join() {
        let mut d = Doc::new("0123456789");
        d.cursor = 2;
        d.ty("a");
        d.cursor = 8;
        d.ty("b");
        d.undo();
        assert_eq!(d.text(), "01a23456789");
    }

    #[test]
    fn line_breaks_and_pastes_are_their_own_steps() {
        let mut d = Doc::new("");
        d.ty("ab");
        d.edit(2..2, "\n", 50);
        d.ty("cd");
        d.undo();
        assert_eq!(d.text(), "ab\n");
        d.undo();
        assert_eq!(d.text(), "ab");
    }

    #[test]
    fn a_new_edit_clears_redo() {
        let mut d = Doc::new("");
        d.ty("ab");
        d.undo();
        assert!(d.hist.can_redo());
        d.ty("z");
        assert!(!d.hist.can_redo());
        assert_eq!(d.redo(), None);
    }

    #[test]
    fn grouped_edits_undo_together_in_reverse_order() {
        let mut buf = RopeBuffer::from_text("a-b-c");
        let mut hist = History::new();
        let before = Selections::default();
        let e1 = buf.replace(1..2, "+").unwrap();
        let e2 = buf.replace(3..4, "+").unwrap();
        hist.record_group(vec![e1, e2], &before, &before);
        assert_eq!(buf.slice(0..9), "a+b+c");
        hist.undo(&mut buf, &mut |_, _| {}).unwrap();
        assert_eq!(buf.slice(0..9), "a-b-c");
        hist.redo(&mut buf, &mut |_, _| {}).unwrap();
        assert_eq!(buf.slice(0..9), "a+b+c");
    }

    #[test]
    fn top_id_returns_to_a_saved_point_after_undo() {
        let mut d = Doc::new("");
        d.ty("ab");
        d.hist.seal();
        let saved = d.hist.top_id();
        d.ty("c");
        assert_ne!(d.hist.top_id(), saved);
        d.undo();
        assert_eq!(d.hist.top_id(), saved);
    }

    #[test]
    fn empty_history_is_a_no_op() {
        let mut buf = RopeBuffer::from_text("x");
        let mut hist = History::new();
        assert!(hist.undo(&mut buf, &mut |_, _| {}).unwrap().is_none());
        assert!(hist.redo(&mut buf, &mut |_, _| {}).unwrap().is_none());
    }
}
