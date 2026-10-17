//! The merge views (M9 tasks 7 and 8): two or three inputs beside an
//! editable result.
//!
//! A merge sits on a tab whose document is the **result**. The tab gains
//! the read-only inputs: for a two-way merge the left and right files
//! (the result starts as a copy of the left); for a three-way merge the
//! base (not shown), ours and theirs (the result starts as the merge's
//! output, with conflicts as marker blocks). The columns are ours, result,
//! theirs, so each change is taken by pulling from the side it sits on.
//!
//! The result is never diffed against anything: the [`Regions`] know
//! each stretch's lines in the result and follow every edit the document
//! reports (undo and redo included), so typing in the result costs what
//! typing anywhere costs, whatever the files' size. Taking a side is one
//! ordinary `replace`, so it undoes like any other edit.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use nimbus_core::align::{Folding, Row, Rows};
use nimbus_core::merge::{Kind, Merge, Pick};
use nimbus_core::regions::{RegionTag, Regions, Resolution};
use nimbus_core::{Buffer, Snapshot, Text};
use nimbus_render::Gpu;

use crate::columns::{Click, Columns, Live};
use crate::document::Document;
use crate::theme::Theme;
use crate::view::{Layout, Preedit, View};

/// Which merge this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Left and right, no common ancestor: every difference is a choice.
    TwoWay,
    /// Base, ours and theirs: only overlapping changes need a choice.
    ThreeWay,
}

/// A side of the merge as shown: ours (left) or theirs (right).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Take {
    /// The left column's lines.
    Left,
    /// The right column's lines.
    Right,
    /// Left, then right.
    Both,
    /// Right, then left.
    BothReversed,
}

impl Take {
    fn pick(self) -> Pick {
        match self {
            Self::Left => Pick::Ours,
            Self::Right => Pick::Theirs,
            Self::Both => Pick::OursThenTheirs,
            Self::BothReversed => Pick::TheirsThenOurs,
        }
    }
}

/// What a merge's worker computes: the chunks, and for a three-way merge
/// the result text with each chunk's lines in it.
pub type Outcome = Result<(Merge, Option<(String, Vec<Range<usize>>)>), String>;

/// A worker's answer to a merge request.
#[derive(Debug)]
pub struct Merged {
    /// Which request it answers.
    pub generation: u64,
    /// The merge, or why there is none.
    pub outcome: Outcome,
}

/// Sends the worker's result back to the event loop.
pub type Send = Arc<dyn Fn(Merged) -> bool + std::marker::Send + Sync>;

/// Generations are unique across every merge, so a result finds its merge
/// by generation alone.
static GENERATION: AtomicU64 = AtomicU64::new(1);

/// A merge attached to a tab.
pub struct MergeView {
    /// Two-way or three-way.
    pub shape: Shape,
    /// The common ancestor of a three-way merge; not shown.
    base: Option<Document>,
    /// The left input.
    pub ours: Document,
    /// The right input.
    pub theirs: Document,
    /// Names for the status line.
    names: (String, String),
    views: [View; 2],
    regions: Regions,
    /// The regions behind the rows' changes, by change number.
    change_regions: Vec<usize>,
    /// The aligned rows and everything drawn from them.
    pub columns: Columns<RegionTag>,
    /// `--merge`: the merge should report through the exit code.
    pub reports_exit: bool,
    /// Waiting for the worker's merge under this generation; the result
    /// cannot be edited until it arrives.
    pending: Option<u64>,
    /// Why there is no merge, when the worker failed.
    failed: Option<String>,
    status: String,
    scratch: String,
}

impl std::fmt::Debug for MergeView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MergeView")
            .field("shape", &self.shape)
            .field("regions", &self.regions.all().len())
            .field("open_conflicts", &self.regions.open_conflicts())
            .finish_non_exhaustive()
    }
}

/// Where a merge's inputs came from, for building one.
pub struct Inputs {
    /// The base of a three-way merge.
    pub base: Option<Document>,
    /// Left, or local.
    pub ours: Document,
    /// Right, or remote.
    pub theirs: Document,
    /// Names shown in the status line.
    pub names: (String, String),
}

impl MergeView {
    /// Start a merge of `inputs` on a worker, through `send`. Returns the
    /// view, waiting, and the result document, which saves to `out`: a
    /// copy of the left text for a two-way merge, empty for a three-way
    /// one until the worker's text arrives.
    pub fn start(
        shape: Shape,
        inputs: Inputs,
        out: Option<PathBuf>,
        send: &Send,
    ) -> (Self, Document) {
        let (mut view, mut result) = Self::new(shape, inputs, out);
        let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
        view.pending = Some(generation);
        let job = view.job();
        let send = Arc::clone(send);
        let spawned = std::thread::Builder::new()
            .name("merge".into())
            .spawn(move || {
                send(Merged {
                    generation,
                    outcome: job(),
                });
            });
        if let Err(e) = spawned {
            log::warn!("merge thread: {e}");
            view.pending = None;
            view.failed = Some(e.to_string());
        }
        result.watch_edits(true);
        (view, result)
    }

    /// Like [`start`](MergeView::start), merging on this thread before
    /// returning: for headless rendering and tests.
    pub fn start_sync(
        shape: Shape,
        inputs: Inputs,
        out: Option<PathBuf>,
    ) -> Result<(Self, Document), String> {
        let (mut view, mut result) = Self::new(shape, inputs, out);
        let outcome = (view.job())();
        view.pending = Some(0);
        view.receive(
            &mut result,
            Merged {
                generation: 0,
                outcome,
            },
        );
        if let Some(e) = view.failed.take() {
            return Err(e);
        }
        result.watch_edits(true);
        Ok((view, result))
    }

    /// A two-way merge on this thread (tests and headless): the result is
    /// a copy of ours.
    #[cfg(test)]
    pub fn two_way(inputs: Inputs, out: Option<PathBuf>) -> Result<(Self, Document), String> {
        Self::start_sync(Shape::TwoWay, inputs, out)
    }

    /// A three-way merge on this thread (tests and headless).
    pub fn three_way(inputs: Inputs, out: Option<PathBuf>) -> Result<(Self, Document), String> {
        Self::start_sync(Shape::ThreeWay, inputs, out)
    }

    /// The view and its result document before any merge is known.
    fn new(shape: Shape, inputs: Inputs, out: Option<PathBuf>) -> (Self, Document) {
        let text = match shape {
            Shape::TwoWay => inputs
                .ours
                .buffer
                .slice(0..inputs.ours.buffer.len_bytes())
                .into_owned(),
            Shape::ThreeWay => String::new(),
        };
        let mut result = Document::with_text(out, &text);
        result.line_ending = inputs.ours.line_ending;
        let mut views = [View::default(), View::default()];
        for v in &mut views {
            v.wrap = false;
        }
        let view = Self {
            shape,
            base: inputs.base,
            ours: inputs.ours,
            theirs: inputs.theirs,
            names: inputs.names,
            views,
            regions: Regions::default(),
            change_regions: Vec::new(),
            columns: Columns::new(Rows::from_segments(3, Folding::default(), []), 3, 1),
            reports_exit: false,
            pending: None,
            failed: None,
            status: String::new(),
            scratch: String::new(),
        };
        (view, result)
    }

    /// The work a merge needs, over snapshots, runnable on any thread.
    fn job(&self) -> impl FnOnce() -> Outcome + std::marker::Send + 'static {
        let ours: Arc<dyn Snapshot> = self.ours.buffer.snapshot().into();
        let theirs: Arc<dyn Snapshot> = self.theirs.buffer.snapshot().into();
        let base: Option<Arc<dyn Snapshot>> =
            self.base.as_ref().map(|b| b.buffer.snapshot().into());
        let shape = self.shape;
        move || match shape {
            Shape::TwoWay => {
                let merge = Merge::new(&ours, &ours, &theirs).map_err(|e| e.to_string())?;
                Ok((merge, None))
            }
            Shape::ThreeWay => {
                let Some(base) = base else {
                    return Err("a three-way merge needs a base".to_owned());
                };
                let merge = Merge::new(&base, &ours, &theirs).map_err(|e| e.to_string())?;
                let mut text = String::new();
                let mut ranges = Vec::new();
                merge.write_tracked(
                    &*base,
                    &*ours,
                    &*theirs,
                    |_| Pick::Markers,
                    &mut text,
                    &mut ranges,
                );
                Ok((merge, Some((text, ranges))))
            }
        }
    }

    /// Whether `merged` answers this merge's request.
    #[must_use]
    pub fn wants(&self, merged: &Merged) -> bool {
        self.pending == Some(merged.generation)
    }

    /// The worker finished: take its merge, and for a three-way merge put
    /// its text into the result. The caller re-detects the result's
    /// language after a three-way merge (its text was empty before).
    pub fn receive(&mut self, result: &mut Document, merged: Merged) {
        if !self.wants(&merged) {
            return;
        }
        self.pending = None;
        match merged.outcome {
            Ok((merge, None)) => self.regions = Regions::from_ours(&merge),
            Ok((merge, Some((text, ranges)))) => {
                result.set_text(&text);
                self.regions = Regions::new(&merge, &ranges);
            }
            Err(e) => self.failed = Some(e),
        }
        // Whatever the log noted while waiting (nothing: edits were held
        // off) or `set_text` marked is moot now.
        result.watch_edits(true);
        self.rebuild_rows();
    }

    /// Whether the merge is still being computed; the result cannot be
    /// edited meanwhile.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// The inputs of a three-way merge rebuilt from a text with conflict
    /// markers (the common text with each block's parts split into base,
    /// ours and theirs), and how many blocks there were. `Ok(None)` when
    /// the text has no markers.
    ///
    /// # Errors
    ///
    /// Markers out of order.
    pub fn inputs_from_markers(text: &str) -> Result<Option<(Inputs, usize)>, String> {
        let Some(parts) = nimbus_core::markers::split(text).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        let name = |label: &str, fallback: &str| {
            if label.is_empty() {
                fallback.to_owned()
            } else {
                label.to_owned()
            }
        };
        let inputs = Inputs {
            base: Some(Document::with_text(None, &parts.base)),
            ours: Document::with_text(None, &parts.ours),
            theirs: Document::with_text(None, &parts.theirs),
            names: (
                name(&parts.labels.0, "ours"),
                name(&parts.labels.1, "theirs"),
            ),
        };
        Ok(Some((inputs, parts.conflicts)))
    }

    /// A three-way merge rebuilt from a text with conflict markers, on
    /// this thread (tests). `Ok(None)` without markers.
    ///
    /// # Errors
    ///
    /// Markers out of order, or the merge's own error.
    #[cfg(test)]
    pub fn from_markers(
        text: &str,
        out: Option<PathBuf>,
    ) -> Result<Option<(Self, Document)>, String> {
        match Self::inputs_from_markers(text)? {
            Some((inputs, _)) => Self::three_way(inputs, out).map(Some),
            None => Ok(None),
        }
    }

    /// Why the worker could not merge, if it could not.
    #[must_use]
    pub fn failure(&self) -> Option<&str> {
        self.failed.as_deref()
    }

    /// Stop watching the result; the tab goes back to one column.
    #[expect(clippy::unused_self, reason = "consumes the merge: it is over")]
    pub fn finish(self, doc: &mut Document) {
        doc.watch_edits(false);
    }

    /// The inputs' documents, for parsing and drawing.
    pub fn inputs_mut(&mut self) -> [&mut Document; 2] {
        [&mut self.ours, &mut self.theirs]
    }

    /// How many conflicts are still open.
    #[must_use]
    pub fn open_conflicts(&self) -> usize {
        self.regions.open_conflicts()
    }

    fn rebuild_rows(&mut self) {
        self.change_regions = self
            .regions
            .all()
            .iter()
            .enumerate()
            .filter(|(_, r)| r.tag().is_some())
            .map(|(i, _)| i)
            .collect();
        let rows = Rows::from_segments(3, Folding::default(), self.regions.segments());
        self.columns.set_rows(rows);
    }

    /// Follow the result's edits since the last frame. Returns `false`
    /// when the result was replaced wholesale (a reload) and the merge no
    /// longer describes it.
    pub fn sync(&mut self, doc: &mut Document) -> bool {
        let (spans, reset) = doc.take_edits();
        if reset {
            return false;
        }
        if spans.is_empty() {
            return true;
        }
        for (span, after) in spans.into_iter().flatten() {
            self.regions.apply_edit(&*after, span);
        }
        self.rebuild_rows();
        true
    }

    /// The region under the result's cursor.
    fn region_at_cursor(&self, doc: &Document) -> Option<usize> {
        let line = doc.buffer.byte_to_line(doc.cursor());
        self.regions.change_at_result_line(line)
    }

    /// Take a side for the change under the cursor: one undoable edit of
    /// the result. Returns what happened, for the message line.
    pub fn take(&mut self, doc: &mut Document, take: Take) -> Result<(), String> {
        if self.pending.is_some() {
            return Err("still merging".to_owned());
        }
        let Some(i) = self.region_at_cursor(doc) else {
            return Err("no change at the cursor".to_owned());
        };
        if self.regions.all()[i].tag().is_none() {
            return Err("the cursor is not on a change".to_owned());
        }
        let pick = take.pick();
        let base: &dyn nimbus_core::Text = match &self.base {
            Some(b) => &b.buffer,
            None => &self.ours.buffer,
        };
        let mut text = std::mem::take(&mut self.scratch);
        let lines = self.regions.take_text(
            i,
            pick,
            (base, &self.ours.buffer, &self.theirs.buffer),
            &mut text,
        );
        let Some(lines) = lines else {
            self.scratch = text;
            return Err("no such change".to_owned());
        };
        let (start, end) = (
            doc.buffer.line_to_byte(lines.start),
            doc.buffer.line_to_byte(lines.end),
        );
        let outcome = doc
            .replace(start..end, &text)
            .map(|_| ())
            .map_err(|e| e.to_string());
        // The replace was noted like any edit, but the regions learn of it
        // from the take itself, which knows its region where a span at a
        // line start would not (an empty region there, or the holder).
        let _ = doc.take_edits();
        if outcome.is_ok() {
            self.regions
                .took(i, pick, nimbus_core::merge::lines_in(&text));
            doc.click(start, false);
        }
        self.scratch = text;
        self.rebuild_rows();
        outcome
    }

    /// Scroll by `dy` pixels (positive is down).
    pub fn scroll_by(&mut self, dy: f32, layout: &Layout) {
        self.columns.scroll_by(dy, layout);
    }

    /// Keep the result's cursor on screen.
    pub fn ensure_cursor_visible(&mut self, doc: &Document, layout: &Layout) {
        let line = doc.buffer.byte_to_line(doc.cursor());
        self.columns.ensure_line_visible(line, layout);
    }

    /// Go to the next or previous change; returns the byte for the cursor.
    pub fn step_change(&mut self, forward: bool, doc: &Document, layout: &Layout) -> Option<usize> {
        let line = doc.buffer.byte_to_line(doc.cursor());
        let start = self.columns.step_change(forward, line, layout)?;
        let line = self
            .columns
            .live_line_from(start)
            .unwrap_or_else(|| doc.buffer.len_lines().saturating_sub(1));
        Some(doc.buffer.line_to_byte(line))
    }

    /// Go to the next or previous open conflict; returns the byte for the
    /// cursor, or `None` when there is none that way.
    pub fn step_conflict(
        &mut self,
        forward: bool,
        doc: &Document,
        layout: &Layout,
    ) -> Option<usize> {
        let line = doc.buffer.byte_to_line(doc.cursor());
        let at = self.regions.at_result_line(line).unwrap_or(0);
        let regions = self.regions.all();
        let found = if forward {
            (at + 1..regions.len()).find(|&i| regions[i].is_open_conflict())
        } else {
            (0..at).rev().find(|&i| regions[i].is_open_conflict())
        }?;
        let number = self.change_regions.iter().position(|&r| r == found)?;
        self.columns.current = Some(number);
        let row = self.columns.rows.change_row(number)?;
        self.columns.show_row(row, layout);
        Some(doc.buffer.line_to_byte(regions[found].result.start))
    }

    /// A click at `(x, y)`.
    pub fn click(
        &mut self,
        doc: &Document,
        view: &mut View,
        layout: &Layout,
        tab_width: usize,
        x: f32,
        y: f32,
    ) -> Click {
        self.columns.click(doc, view, layout, tab_width, x, y)
    }

    /// The summary for the status bar, followed by the editor's own `info`.
    pub fn status(&mut self, doc: &Document, info: &str) -> &str {
        use std::fmt::Write as _;
        self.status.clear();
        let (l, r) = &self.names;
        match self.shape {
            Shape::TwoWay => {
                let _ = write!(self.status, "Merge {l} ⇄ {r}");
            }
            Shape::ThreeWay => {
                let _ = write!(self.status, "Merge {l} + {r}");
            }
        }
        if self.pending.is_some() {
            self.status.push_str(" · merging…");
        }
        if let Some(e) = &self.failed {
            let _ = write!(self.status, " · cannot merge: {e}");
        }
        let changes = self.columns.rows.changes();
        let conflicts = self.open_conflicts();
        match self.region_at_cursor(doc).and_then(|i| {
            self.change_regions
                .iter()
                .position(|&r| r == i)
                .map(|n| (n, self.regions.all()[i].resolution))
        }) {
            Some((n, resolution)) => {
                let _ = write!(self.status, " · change {} of {changes}", n + 1);
                match resolution {
                    Resolution::Pending => {}
                    Resolution::Taken(Pick::Ours) => self.status.push_str(" (left taken)"),
                    Resolution::Taken(Pick::Theirs) => self.status.push_str(" (right taken)"),
                    Resolution::Taken(_) => self.status.push_str(" (both taken)"),
                    Resolution::Edited => self.status.push_str(" (edited)"),
                }
            }
            None => {
                let _ = write!(self.status, " · {changes} changes");
            }
        }
        if self.shape == Shape::ThreeWay {
            match conflicts {
                0 => self.status.push_str(" · no conflicts left"),
                1 => self.status.push_str(" · 1 conflict left"),
                n => {
                    let _ = write!(self.status, " · {n} conflicts left");
                }
            }
        }
        if !info.is_empty() {
            let _ = write!(self.status, " · {info}");
        }
        &self.status
    }

    /// Plan the rows on screen for the three columns: unaligned while the
    /// merge is pending, else from the aligned rows with each region's
    /// background.
    fn plan(&mut self, doc: &Document, layout: &Layout, theme: &Theme) {
        let ui = theme.ui;
        let background = |r: &Row<RegionTag>, c: usize| -> Option<[f32; 4]> {
            let (tag, _) = r.change?;
            // The result column shows what it holds; the inputs show
            // where they differ from it.
            match (tag.kind, tag.resolution, c) {
                (Kind::Conflict, Resolution::Pending, _) => Some(ui.merge_conflict),
                (_, Resolution::Taken(_) | Resolution::Edited, 1) => Some(ui.diff_added),
                (Kind::Same, _, _) => None,
                (Kind::Ours | Kind::Both, _, 0) | (Kind::Theirs | Kind::Both, _, 2) => {
                    Some(ui.diff_changed)
                }
                (_, _, 1) => Some(ui.diff_changed),
                _ => Some(ui.diff_filler),
            }
        };
        if self.columns.rows.is_empty() {
            let lines = [
                nimbus_core::diff_lines(&self.ours.buffer),
                nimbus_core::diff_lines(&doc.buffer),
                nimbus_core::diff_lines(&self.theirs.buffer),
            ];
            self.columns.plan_unaligned(layout, &lines);
        } else {
            // Merges mark no words: the inputs' lines differ as wholes.
            self.columns
                .plan(layout, background, |_: &Row<RegionTag>| [None, None, None]);
        }
    }

    /// Queue the merge area: three columns and the scrollbar.
    #[expect(
        clippy::too_many_arguments,
        reason = "a frame's inputs, as the scene passes them"
    )]
    pub fn draw(
        &mut self,
        gpu: &mut Gpu,
        layout: &Layout,
        doc: &Document,
        view: &mut View,
        theme: &mut Theme,
        tab_width: usize,
        cursor_visible: bool,
        preedit: Option<&Preedit>,
        matches: (&[Range<usize>], Option<Range<usize>>),
    ) {
        self.plan(doc, layout, theme);
        let [ours_view, theirs_view] = &mut self.views;
        let live = Live {
            cursor_visible,
            preedit,
            matches,
            tab_width,
        };
        self.columns.draw(
            gpu,
            layout,
            &[&self.ours, doc, &self.theirs],
            &mut [ours_view, view, theirs_view],
            &live,
            theme,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A merge started on a worker shows the inputs unaligned and refuses
    /// edits until its result arrives, then aligns like a synchronous one.
    #[test]
    fn worker_merge_waits_then_aligns() {
        let (tx, rx) = std::sync::mpsc::channel();
        let send: Send = Arc::new(move |m| tx.send(m).is_ok());
        let (mut m, mut result) = MergeView::start(
            Shape::ThreeWay,
            inputs(Some("a\nb\nc\n"), "x\nb\nc\n", "a\nb\ny\n"),
            None,
            &send,
        );
        assert!(m.is_pending());
        assert!(m.columns.rows.is_empty());
        assert_eq!(result.buffer.len_bytes(), 0);
        assert_eq!(
            m.take(&mut result, Take::Left),
            Err("still merging".to_owned())
        );
        let merged = rx.recv().unwrap();
        assert!(m.wants(&merged));
        m.receive(&mut result, merged);
        assert!(!m.is_pending());
        assert!(m.failure().is_none());
        assert_eq!(
            result.buffer.slice(0..result.buffer.len_bytes()),
            "x\nb\ny\n"
        );
        assert!(!m.columns.rows.is_empty());
        assert_eq!(m.open_conflicts(), 0);
        // A stale answer is ignored.
        m.pending = Some(7);
        m.receive(
            &mut result,
            Merged {
                generation: 1,
                outcome: Err("late".to_owned()),
            },
        );
        assert!(m.failure().is_none());
    }

    fn inputs(base: Option<&str>, ours: &str, theirs: &str) -> Inputs {
        Inputs {
            base: base.map(Document::from_text),
            ours: Document::from_text(ours),
            theirs: Document::from_text(theirs),
            names: ("left".into(), "right".into()),
        }
    }

    fn text(d: &Document) -> String {
        d.buffer.slice(0..d.buffer.len_bytes()).into_owned()
    }

    fn layout() -> Layout {
        crate::compare::tests::layout()
    }

    #[test]
    fn two_way_merge_starts_as_the_left_and_takes_sides() {
        let (mut m, mut result) =
            MergeView::two_way(inputs(None, "a\nb\nc\n", "a\nB\nc\nd\n"), None).unwrap();
        assert_eq!(text(&result), "a\nb\nc\n");
        assert_eq!(m.columns.rows.changes(), 2);
        assert!(m.status(&result, "").starts_with("Merge left ⇄ right"));
        // Take the right side for the change on line 1.
        result.click(2, false);
        m.take(&mut result, Take::Right).unwrap();
        assert_eq!(text(&result), "a\nB\nc\n");
        assert!(
            m.status(&result, "").contains("(right taken)"),
            "{}",
            m.status(&result, "")
        );
        // The second change is a pure addition on the right: taking both
        // appends their line (ours has nothing there).
        let layout = layout();
        let pos = m.step_change(true, &result, &layout).unwrap();
        result.click(pos, false);
        m.take(&mut result, Take::Both).unwrap();
        assert_eq!(text(&result), "a\nB\nc\nd\n");
        // Undo is an ordinary edit the regions follow.
        result.undo().unwrap();
        assert!(m.sync(&mut result));
        assert_eq!(text(&result), "a\nB\nc\n");
        assert_eq!(m.columns.rows.changes(), 2);
    }

    /// Lines the right side adds in the middle have no result line to put
    /// the cursor on; Next Change lands on the line after them, and taking
    /// there inserts them.
    #[test]
    fn right_only_lines_in_the_middle_can_be_taken() {
        let (mut m, mut result) =
            MergeView::two_way(inputs(None, "a\nb\nc\n", "a\nX\nb\nc\n"), None).unwrap();
        assert_eq!(m.columns.rows.changes(), 1);
        let layout = layout();
        result.click(0, false);
        let pos = m.step_change(true, &result, &layout).unwrap();
        result.click(pos, false);
        assert_eq!(result.buffer.byte_to_line(pos), 1);
        m.take(&mut result, Take::Right).unwrap();
        assert_eq!(text(&result), "a\nX\nb\nc\n");
        assert!(m.status(&result, "").contains("(right taken)"));
        // The equal run after it followed: typing on its last line is an
        // edit of that run, not of the take.
        result.click(result.buffer.line_to_byte(3), false);
        result.insert("!").unwrap();
        assert!(m.sync(&mut result));
        assert_eq!(text(&result), "a\nX\nb\n!c\n");
        assert!(matches!(
            m.regions.all()[1].resolution,
            Resolution::Taken(_)
        ));
        assert_eq!(m.regions.all()[1].result, 1..2);
        assert_eq!(m.columns.rows.changes(), 2, "the take, and the edited run");
    }

    #[test]
    fn three_way_merge_marks_conflicts_and_counts_them_down() {
        let (mut m, mut result) = MergeView::three_way(
            inputs(Some("a\nb\nc\nd\n"), "a\nX\nc\nd\n", "a\nY\nc\nD\n"),
            None,
        )
        .unwrap();
        assert_eq!(
            text(&result),
            "a\n<<<<<<< ours\nX\n=======\nY\n>>>>>>> theirs\nc\nD\n",
            "their change to d is taken; the clash on b is a conflict"
        );
        assert_eq!(m.open_conflicts(), 1);
        assert!(m.status(&result, "").contains("1 conflict left"));
        let layout = layout();
        result.click(0, false);
        let pos = m.step_conflict(true, &result, &layout).unwrap();
        assert_eq!(pos, 2, "the conflict block starts on line 1");
        result.click(pos, false);
        m.take(&mut result, Take::Left).unwrap();
        assert_eq!(text(&result), "a\nX\nc\nD\n");
        assert_eq!(m.open_conflicts(), 0);
        assert!(m.status(&result, "").contains("no conflicts left"));
        assert!(m.step_conflict(true, &result, &layout).is_none());
    }

    #[test]
    fn a_file_with_markers_reopens_as_a_merge() {
        let source = "a\n<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> other\nc\n";
        let (mut m, mut result) = MergeView::from_markers(source, None).unwrap().unwrap();
        assert_eq!(
            text(&result),
            "a\n<<<<<<< ours\nx\n=======\ny\n>>>>>>> theirs\nc\n"
        );
        assert_eq!(m.open_conflicts(), 1);
        assert!(m.status(&result, "").starts_with("Merge HEAD + other"));
        result.click(4, false);
        m.take(&mut result, Take::Right).unwrap();
        assert_eq!(text(&result), "a\ny\nc\n");
        assert!(MergeView::from_markers("plain\n", None).unwrap().is_none());
    }

    /// `NIMBUS_DIFF_FILES=OLD:NEW cargo test --release -p nimbus -- --ignored --nocapture merge_typing_bench`
    ///
    /// M9 task 11: how long a three-way merge of the bench pair takes to
    /// build (base OLD, ours NEW, theirs OLD with every 400th line
    /// changed), and what a keystroke in its result costs: the edit, the
    /// region tracking and the row planning for one screen (drawing
    /// needs a GPU; its cost is the ordinary editor's).
    #[test]
    #[ignore = "benchmark: needs NIMBUS_DIFF_FILES and a release build"]
    fn merge_typing_bench() {
        use crate::compare::tests::{bench_pair, layout, report, type_at};
        let Some((old, new)) = bench_pair() else {
            return;
        };
        let base = Document::open(&old).unwrap();
        let ours = Document::open(&new).unwrap();
        let mut theirs_text = String::new();
        for (i, line) in base
            .buffer
            .slice(0..base.buffer.len_bytes())
            .lines()
            .enumerate()
        {
            if i % 400 == 0 {
                theirs_text.push_str("// theirs changed this line\n");
            } else {
                theirs_text.push_str(line);
                theirs_text.push('\n');
            }
        }
        let theirs = Document::with_text(None, &theirs_text);
        let t = std::time::Instant::now();
        let (mut m, mut result) = MergeView::three_way(
            Inputs {
                base: Some(base),
                ours,
                theirs,
                names: ("ours".into(), "theirs".into()),
            },
            None,
        )
        .unwrap();
        println!(
            "three-way merge built in {:?}: {} rows, {} changes, {} conflicts",
            t.elapsed(),
            m.columns.rows.len(),
            m.columns.rows.changes(),
            m.open_conflicts()
        );
        let layout = layout();
        let theme = Theme::default();
        let len = result.buffer.len_bytes();
        let mut samples = Vec::new();
        for (i, at) in type_at(len).enumerate() {
            let ch = if i % 7 == 6 { "\n" } else { "x" };
            result.click(at, false);
            let t = std::time::Instant::now();
            result.insert(ch).unwrap();
            m.sync(&mut result);
            m.plan(&result, &layout, &theme);
            samples.push(t.elapsed());
        }
        report("merge result keystroke (edit + regions + plan)", samples);
        // Taking a side: one replace of the region's lines, then a sync.
        let mut samples = Vec::new();
        for i in 0..m.change_regions.len().min(100) {
            let Some(r) = m.change_regions.get(i).copied() else {
                break;
            };
            let line = m.regions.all()[r].result.start;
            result.click(result.buffer.line_to_byte(line), false);
            let t = std::time::Instant::now();
            if m.take(&mut result, Take::Right).is_ok() {
                m.sync(&mut result);
                m.plan(&result, &layout, &theme);
                samples.push(t.elapsed());
            }
        }
        report("take right (replace + regions + plan)", samples);
    }

    #[test]
    fn typing_in_the_result_is_followed() {
        let (mut m, mut result) =
            MergeView::two_way(inputs(None, "a\nb\n", "a\nB\n"), None).unwrap();
        result.click(2, false);
        result.insert("x\n").unwrap();
        assert!(m.sync(&mut result));
        assert_eq!(text(&result), "a\nx\nb\n");
        assert!(
            m.status(&result, "").contains("(edited)"),
            "{}",
            m.status(&result, "")
        );
    }
}
