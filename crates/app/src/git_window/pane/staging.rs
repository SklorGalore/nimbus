//! Staging, unstaging, discarding and committing from the git window
//! (M28): what each button and action does with the selection, and what
//! happens when git answers.
//!
//! The window never edits its lists itself after a change: it reads the
//! status again and shows what git says, the rule M6 set for the file
//! tree (the disk is the truth, our own changes included).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::super::message::keep_draft;
use super::super::ops::{self, DiscardFile, Done, Op, PatchOp};
use super::{Ask, DiffPane, FileItem, Focus, GitWindow, Selected};
use crate::git::patch::{Apply, Pick};
use crate::git::{CommitOutput, GitError, Kind};
use crate::input::GitCmd;
use crate::prompt::{Answer, Button, Prompt};

/// Lines of a hook's output shown when a commit is refused.
const REPORT_LINES: usize = 40;

/// Part of a file: the change at the selection, or the picked rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Hunk,
    Lines,
}

/// What is done to a part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Act {
    Stage,
    Unstage,
    Discard,
}

impl Act {
    fn verb(self) -> &'static str {
        match self {
            Self::Stage => "Staged",
            Self::Unstage => "Unstaged",
            Self::Discard => "Discarded",
        }
    }
}

impl GitWindow {
    /// Say `text` in the status bar.
    pub(super) fn say(&mut self, text: impl Into<String>, is_error: bool) {
        self.said = Some((text.into(), is_error));
    }

    /// What to say in the status bar, taken once.
    pub fn take_said(&mut self) -> Option<(String, bool)> {
        self.said.take()
    }

    /// The window-wide layout of the last frame, for drawing the
    /// palette over it.
    #[must_use]
    pub fn window_layout(&self) -> Option<crate::view::Layout> {
        self.window_layout
    }

    /// Whether a commit is running.
    #[must_use]
    pub fn committing(&self) -> bool {
        self.committing.is_some()
    }

    fn has_head(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.head.is_some())
    }

    fn selected_file(&self) -> Option<FileItem> {
        self.files.get(self.file_sel?).cloned()
    }

    /// Run a git window command. Returns whether to draw again.
    pub fn git_command(&mut self, cmd: GitCmd) -> bool {
        if self.repo.is_none() {
            return false;
        }
        match cmd {
            GitCmd::Commit => self.commit(),
            GitCmd::ToggleAmend => self.toggle_amend(),
            GitCmd::FocusMessage => {
                if self.selected != Selected::Changes {
                    self.show_changes();
                }
                self.focus = Focus::Message;
            }
            GitCmd::UndoDiscard => self.undo_discard(),
            _ if self.busy.is_some() => {
                let what = self.busy.clone().unwrap_or_default();
                self.say(format!("{what} Wait for it to finish."), false);
            }
            GitCmd::StageAll => self.start(Op::StageAll, "Staged everything".to_owned()),
            GitCmd::UnstageAll => {
                let head = self.has_head();
                self.start(Op::UnstageAll(head), "Unstaged everything".to_owned());
            }
            GitCmd::StageFile => self.whole_file(Act::Stage),
            GitCmd::UnstageFile => self.whole_file(Act::Unstage),
            GitCmd::DiscardFile => self.whole_file(Act::Discard),
            GitCmd::StageHunk => self.part(Part::Hunk, Act::Stage),
            GitCmd::UnstageHunk => self.part(Part::Hunk, Act::Unstage),
            GitCmd::DiscardHunk => self.part(Part::Hunk, Act::Discard),
            GitCmd::StageLines => self.part(Part::Lines, Act::Stage),
            GitCmd::UnstageLines => self.part(Part::Lines, Act::Unstage),
            GitCmd::DiscardLines => self.part(Part::Lines, Act::Discard),
        }
        true
    }

    /// Run file `i`'s own button.
    pub fn file_button(&mut self, i: usize, cmd: GitCmd) {
        self.select_file(i);
        self.git_command(cmd);
    }

    /// Start `op` on a worker; `said` is what to say when it worked.
    fn start(&mut self, op: Op, said: String) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let doing = match &op {
            Op::Undo(_) => "Putting it back…",
            Op::Discard(_) => "Discarding…",
            Op::Unstage(..) | Op::UnstageAll(_) => "Unstaging…",
            Op::Patch(p) if !p.cached => "Discarding…",
            Op::Patch(p) if p.apply == Apply::Reverse => "Unstaging…",
            _ => "Staging…",
        };
        self.busy = Some(doing.to_owned());
        ops::run(repo, op, said, Arc::clone(&self.send));
    }

    /// The selected file must be in Changes' staged (`Some(true)`) or
    /// unstaged list; says why not otherwise.
    fn file_in(&mut self, staged: bool) -> Option<FileItem> {
        let Some(file) = self.selected_file().filter(|f| f.staged.is_some()) else {
            self.say("Select a file under Changes first", false);
            return None;
        };
        // A partly staged file is in both lists: use its other row.
        if file.staged != Some(staged)
            && let Some(other) = self
                .files
                .iter()
                .find(|f| f.path == file.path && f.staged == Some(staged))
        {
            return Some(other.clone());
        }
        if file.staged != Some(staged) {
            let where_ = if staged {
                "is not staged"
            } else {
                "has no unstaged changes"
            };
            self.say(format!("{} {where_}", file.path), false);
            return None;
        }
        Some(file)
    }

    fn whole_file(&mut self, act: Act) {
        match act {
            Act::Stage => {
                let Some(f) = self.file_in(false) else { return };
                let said = format!("Staged {}", f.path);
                self.start(Op::Stage(vec![f.path]), said);
            }
            Act::Unstage => {
                let Some(f) = self.file_in(true) else { return };
                let said = format!("Unstaged {}", f.path);
                let mut paths = vec![f.path];
                paths.extend(f.orig);
                let head = self.has_head();
                self.start(Op::Unstage(paths, head), said);
            }
            Act::Discard => {
                let Some(f) = self.file_in(false) else { return };
                let (what, text) = match f.kind {
                    Kind::Conflicted => {
                        self.say(
                            format!("{} has conflicts; resolve them or stage it", f.path),
                            false,
                        );
                        return;
                    }
                    Kind::Untracked if f.path.ends_with('/') => (
                        DiscardFile::UntrackedFolder(f.path.clone()),
                        format!(
                            "Move the folder {} and everything in it to the trash?",
                            f.path
                        ),
                    ),
                    Kind::Untracked => (
                        DiscardFile::Untracked(f.path.clone()),
                        format!("Move {}, which git does not track, to the trash?", f.path),
                    ),
                    Kind::Deleted => (
                        DiscardFile::Deleted(f.path.clone()),
                        format!("Bring back {}, deleted in the working tree?", f.path),
                    ),
                    _ => (
                        DiscardFile::Tracked(f.path.clone()),
                        format!(
                            "Discard every unstaged change to {}? It goes back to its staged version.",
                            f.path
                        ),
                    ),
                };
                let said = format!("Discarded the changes to {}", f.path);
                self.ask_discard(Op::Discard(what), said, &text);
            }
        }
    }

    /// Ask before a discard; the answer runs it.
    fn ask_discard(&mut self, op: Op, said: String, question: &str) {
        let text = format!("{question}\nUndo Discard puts it back until this window closes.");
        self.prompt = Some(Prompt::new(
            "Discard changes",
            text,
            &[
                Button {
                    label: "Cancel",
                    answer: Answer::Cancel,
                },
                Button {
                    label: "Discard",
                    answer: Answer::Yes,
                },
            ],
            Ask::Discard(Box::new(op), said),
        ));
    }

    /// The prompt was answered.
    pub fn answered(&mut self, answer: Answer) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        if let (Answer::Yes, Ask::Discard(op, said)) = (answer, prompt.then) {
            self.start(*op, said);
        }
    }

    /// Stage, unstage or discard part of the selected file.
    fn part(&mut self, part: Part, act: Act) {
        let Some(file) = self.file_in(act == Act::Unstage) else {
            return;
        };
        if self.selected_file().map(|f| f.staged) != Some(file.staged) {
            let row = if act == Act::Unstage {
                "staged"
            } else {
                "unstaged"
            };
            self.say(
                format!("Select {}'s {row} row to pick from its compare", file.path),
                false,
            );
            return;
        }
        let whole_only = match file.kind {
            Kind::Deleted => Some("a deleted file"),
            Kind::Conflicted => Some("a file with conflicts"),
            _ if file.submodule => Some("a folder or submodule"),
            _ => None,
        };
        if let Some(what) = whole_only {
            self.say(format!("Stage or discard {what} whole"), false);
            return;
        }
        let Some(DiffPane::Compare { compare, raw, .. }) = &self.diff else {
            self.say("There is no text to pick from: use the whole file", false);
            return;
        };
        if compare.hunks().is_none() {
            self.say("Still comparing; try again in a moment", false);
            return;
        }
        let rows = match part {
            Part::Lines => {
                let Some((a, b)) = self.picked else {
                    self.say("Click the rows to pick in the compare first", false);
                    return;
                };
                a.min(b)..a.max(b) + 1
            }
            Part::Hunk => {
                let at = self
                    .picked
                    .and_then(|(_, head)| compare.change_at(head))
                    .or_else(|| compare.current_change())
                    .unwrap_or(0);
                let Some(r) = compare.change_rows(at) else {
                    self.say("No changes in this file", false);
                    return;
                };
                r
            }
        };
        let [old, new] = compare.lines_in(rows);
        if old.is_empty() && new.is_empty() {
            self.say("No changed lines are picked", false);
            return;
        }
        let create = match (&file.sides.0, act) {
            (super::Source::Absent, Act::Stage) => self
                .repo
                .as_ref()
                .map(|r| ops::mode_of(&r.top.join(&file.path))),
            _ => None,
        };
        let op = PatchOp {
            path: file.path.clone(),
            old: Arc::clone(&raw[0]),
            new: Arc::clone(&raw[1]),
            new_is_disk: act != Act::Unstage,
            pick: Pick { old, new },
            apply: if act == Act::Stage {
                Apply::Forward
            } else {
                Apply::Reverse
            },
            cached: act != Act::Discard,
            create,
        };
        let noun = if part == Part::Hunk {
            "a change"
        } else {
            "the picked lines"
        };
        let said = format!("{} {noun} in {}", act.verb(), file.path);
        let op = Op::Patch(Box::new(op));
        if act == Act::Discard {
            let question = format!("Discard {noun} in {} from the working tree?", file.path);
            self.ask_discard(op, said, &question);
        } else {
            self.start(op, said);
        }
    }

    fn undo_discard(&mut self) {
        if self.busy.is_some() {
            self.say("Wait for the change running now to finish", false);
            return;
        }
        let Some(d) = self.undo.pop() else {
            self.say("Nothing to put back", false);
            return;
        };
        let said = format!("Put back {}", d.path());
        self.start(Op::Undo(d), said);
    }

    /// A change finished: say so, remember how to undo a discard, and
    /// read the status again.
    pub(super) fn done(&mut self, d: Done) {
        self.busy = None;
        match d.result {
            Ok(undo) => {
                self.undo.extend(undo);
                let more = if d.undid || self.undo.is_empty() {
                    String::new()
                } else {
                    " · Undo Discard puts it back".to_owned()
                };
                self.say(format!("{}{more}", d.said), false);
            }
            Err(e) => self.say(e, true),
        }
        self.refresh();
    }

    // ---- Committing ----------------------------------------------------

    fn toggle_amend(&mut self) {
        if !self.has_head() && !self.message.amend {
            self.say("There is no commit to amend yet", false);
            return;
        }
        let on = !self.message.amend;
        let last = self.prefill.last.clone();
        self.message.set_amend(on, last.as_deref());
        if on && last.is_none() {
            self.say("The last commit's message is still being read", false);
        }
    }

    fn commit(&mut self) {
        if let Some(stop) = &self.committing {
            stop.store(true, Ordering::Relaxed);
            return;
        }
        if self.message.is_blank() {
            self.focus = Focus::Message;
            self.say("Write a commit message first", false);
            return;
        }
        let staged = self.status.as_ref().is_some_and(|s| {
            s.entries
                .iter()
                .any(crate::git::parse::StatusEntry::is_staged)
        });
        if !staged && !self.message.amend {
            self.say("Nothing is staged to commit", false);
            return;
        }
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let stop = Arc::new(AtomicBool::new(false));
        self.committing = Some(Arc::clone(&stop));
        ops::commit(
            repo,
            self.message.text(),
            self.message.amend,
            stop,
            Arc::clone(&self.send),
        );
    }

    /// Stop a running commit (its hooks).
    pub fn stop_commit(&mut self) {
        if let Some(stop) = &self.committing {
            stop.store(true, Ordering::Relaxed);
        }
    }

    /// A commit finished.
    pub(super) fn committed(&mut self, result: Result<CommitOutput, GitError>) {
        self.committing = None;
        match result {
            Ok(out) if out.ok => {
                let amended = self.message.amend;
                let subject = self.message.text().lines().next().unwrap_or("").to_owned();
                self.message.clear();
                self.keep_draft_now();
                self.say(
                    format!(
                        "{} “{subject}”",
                        if amended { "Amended:" } else { "Committed:" }
                    ),
                    false,
                );
                if let Some(repo) = self.repo.clone() {
                    ops::prefill(repo, Arc::clone(&self.send));
                }
            }
            Ok(out) => {
                let mut text: Vec<&str> = out.text.lines().take(REPORT_LINES).collect();
                if text.is_empty() {
                    text.push("git gave no reason.");
                }
                let more = out.text.lines().count() > REPORT_LINES;
                let mut body = text.join("\n");
                if more {
                    body.push_str("\n…");
                }
                self.prompt = Some(Prompt::new(
                    "The commit did not go through",
                    body,
                    &[Button {
                        label: "OK",
                        answer: Answer::Yes,
                    }],
                    Ask::Report,
                ));
            }
            Err(GitError::Cancelled) => self.say("Commit stopped", false),
            Err(e) => self.say(format!("Cannot commit: {e}"), true),
        }
        self.refresh();
    }

    /// Keep the message as this repository's draft, if it changed since
    /// it was last kept.
    pub fn keep_draft_now(&mut self) {
        let Some(repo) = &self.repo else { return };
        if self.message.doc.version == self.kept_version && !self.message.is_blank() {
            return;
        }
        // An amend's text is the last commit's, not a draft.
        let text = if self.message.amend {
            String::new()
        } else {
            self.message.text()
        };
        keep_draft(&repo.top, &text);
        self.kept_version = self.message.doc.version;
    }

    // ---- Picking rows --------------------------------------------------

    /// Move the picked rows' head by `d` (Up and Down in the compare);
    /// `extend` keeps the anchor.
    pub fn step_pick(&mut self, d: isize, extend: bool) {
        let Some(DiffPane::Compare { compare, .. }) = &mut self.diff else {
            return;
        };
        let n = compare.row_count();
        if n == 0 {
            return;
        }
        let from = match self.picked {
            Some((_, head)) => head,
            None => compare
                .current_change()
                .and_then(|c| compare.change_rows(c))
                .map_or(0, |r| r.start),
        };
        let to = from.saturating_add_signed(d).min(n - 1);
        let anchor = match self.picked {
            Some((a, _)) if extend => a,
            _ => to,
        };
        self.picked = Some((anchor, to));
        if let Some(layout) = &self.compare_layout {
            compare.show_row(to, layout);
        }
    }

    /// The mouse at `(x, y)` over the compare: pick its row, or extend
    /// the pick to it.
    pub fn pick_at(&mut self, x: f32, y: f32, extend: bool) {
        let (Some(DiffPane::Compare { compare, .. }), Some(layout)) =
            (&self.diff, &self.compare_layout)
        else {
            return;
        };
        let Some(row) = compare.row_under(layout, x, y) else {
            return;
        };
        self.picked = match self.picked {
            Some((anchor, _)) if extend => Some((anchor, row)),
            _ => Some((row, row)),
        };
    }
}
