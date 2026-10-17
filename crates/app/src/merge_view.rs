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
    /// A two-way merge of `inputs.ours` and `inputs.theirs`. Returns the
    /// view and the result document: a copy of ours, saving to `out`.
    ///
    /// # Errors
    ///
    /// The diff's error when a text has too many distinct lines.
    pub fn two_way(inputs: Inputs, out: Option<PathBuf>) -> Result<(Self, Document), String> {
        let ours: Arc<dyn Snapshot> = inputs.ours.buffer.snapshot().into();
        let theirs: Arc<dyn Snapshot> = inputs.theirs.buffer.snapshot().into();
        let merge = Merge::new(&ours, &ours, &theirs).map_err(|e| e.to_string())?;
        let regions = Regions::from_ours(&merge);
        let text = ours.slice(0..ours.len_bytes());
        let mut result = Document::with_text(out, &text);
        result.line_ending = inputs.ours.line_ending;
        let view = Self::build(Shape::TwoWay, inputs, regions, &mut result);
        Ok((view, result))
    }

    /// A three-way merge. The result starts as the merge's output with
    /// conflicts as marker blocks, saving to `out`.
    ///
    /// # Errors
    ///
    /// The diff's error when a text has too many distinct lines, or a
    /// missing base.
    pub fn three_way(inputs: Inputs, out: Option<PathBuf>) -> Result<(Self, Document), String> {
        let Some(base_doc) = &inputs.base else {
            return Err("a three-way merge needs a base".to_owned());
        };
        let base: Arc<dyn Snapshot> = base_doc.buffer.snapshot().into();
        let ours: Arc<dyn Snapshot> = inputs.ours.buffer.snapshot().into();
        let theirs: Arc<dyn Snapshot> = inputs.theirs.buffer.snapshot().into();
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
        let regions = Regions::new(&merge, &ranges);
        let mut result = Document::with_text(out, &text);
        result.line_ending = inputs.ours.line_ending;
        let view = Self::build(Shape::ThreeWay, inputs, regions, &mut result);
        Ok((view, result))
    }

    /// A three-way merge rebuilt from a text with conflict markers: the
    /// common text with each block's parts split into base, ours and
    /// theirs, merged again. The result is the same text with the blocks
    /// written our way, saving to `out`. `Ok(None)` when the text has no
    /// markers.
    ///
    /// # Errors
    ///
    /// Markers out of order, or the merge's own error.
    pub fn from_markers(
        text: &str,
        out: Option<PathBuf>,
    ) -> Result<Option<(Self, Document)>, String> {
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
        Self::three_way(inputs, out).map(Some)
    }

    fn build(shape: Shape, inputs: Inputs, regions: Regions, result: &mut Document) -> Self {
        result.watch_edits(true);
        let mut views = [View::default(), View::default()];
        for v in &mut views {
            v.wrap = false;
        }
        let mut m = Self {
            shape,
            base: inputs.base,
            ours: inputs.ours,
            theirs: inputs.theirs,
            names: inputs.names,
            views,
            regions,
            change_regions: Vec::new(),
            columns: Columns::new(Rows::from_segments(3, Folding::default(), []), 3, 1),
            reports_exit: false,
            status: String::new(),
            scratch: String::new(),
        };
        m.rebuild_rows();
        m
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
        for span in spans.into_iter().flatten() {
            self.regions.apply_edit(&doc.buffer, span);
        }
        self.rebuild_rows();
        true
    }

    /// The region under the result's cursor.
    fn region_at_cursor(&self, doc: &Document) -> Option<usize> {
        let line = doc.buffer.byte_to_line(doc.cursor());
        self.regions.at_result_line(line)
    }

    /// Take a side for the change under the cursor: one undoable edit of
    /// the result. Returns what happened, for the message line.
    pub fn take(&mut self, doc: &mut Document, take: Take) -> Result<(), String> {
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
        self.scratch = text;
        // The replace was noted like any edit; apply it now, then record
        // the pick over the "edited" mark it left.
        let (spans, _) = doc.take_edits();
        for span in spans.into_iter().flatten() {
            self.regions.apply_edit(&doc.buffer, span);
        }
        if outcome.is_ok() {
            self.regions.took(i, pick);
            doc.click(start, false);
        }
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
        // Merges mark no words: the inputs' lines differ as wholes.
        self.columns
            .plan(layout, background, |_: &Row<RegionTag>| [None, None, None]);
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
