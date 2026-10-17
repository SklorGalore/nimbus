//! The compare view (M9 task 6): the live document beside another text,
//! with their differences coloured and aligned.
//!
//! A compare sits on an ordinary tab: the tab's own document stays the
//! editable right side, and the tab gains a read-only left side (the
//! saved file, another file, or a copy of another tab) plus the diff
//! between them. Everything the editor can do to the right side still
//! works, because nothing about that document changed; the compare only
//! watches its edits ([`Document::take_edits`]) and follows them.
//!
//! Three things keep it within the editor's budgets (M9 doc, Performance
//! budgets):
//!
//! - The first diff runs on a worker over snapshots of both texts; the
//!   view shows both sides unaligned with "comparing…" until it arrives.
//!   Texts within [`DIFF_LIMIT`] get a [`LiveDiff`], which follows each
//!   edit by re-diffing only the lines around it; larger ones get a
//!   layered [`compare`] and are compared again on the worker after a
//!   pause in typing, the find bar's pattern.
//! - Rows come from [`Rows`], a prefix sum over the hunks: no per-line
//!   arrays however long the files, and a row or a line is found in
//!   O(log hunks).
//! - Only the rows on screen are planned and drawn each frame, and the
//!   changed-word marks are worked out for those rows alone.

use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use nimbus_core::align::{Cell, Change, Folding, Rows};
use nimbus_core::compare::{Comparison, Options, compare};
use nimbus_core::diff::{DIFF_LIMIT, Hunk};
use nimbus_core::live_diff::{LiveDiff, Side};
use nimbus_core::{Buffer, Snapshot, Text};
use nimbus_render::Gpu;

use crate::document::Document;
use crate::theme::{Theme, Ui};
use crate::view::{Decorations, Layout, RowPlan, View};

/// What the left side is, for the status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Other {
    /// The file as saved on disk.
    Saved,
    /// Another file.
    File(PathBuf),
    /// A copy of another tab's text, by its name.
    Tab(String),
}

/// A finished diff from the worker.
#[derive(Debug)]
pub struct DiffResult {
    /// Which request it answers; stale ones are dropped.
    pub generation: u64,
    /// The document version the right side was snapshotted at.
    pub version: u64,
    /// The diff, or why there is none.
    pub outcome: Result<DiffKind, String>,
}

/// The two shapes a diff comes in.
#[derive(Debug)]
pub enum DiffKind {
    /// Both texts within the limit: follows edits locally.
    Live(Box<LiveDiff>),
    /// Over the limit: a layered comparison, redone after a pause.
    Fixed(Comparison),
}

/// How a diff is known.
#[derive(Debug)]
enum State {
    /// Waiting for the worker.
    Pending,
    Live(Box<LiveDiff>),
    Fixed {
        hunks: Vec<Hunk>,
        old_lines: usize,
        new_lines: usize,
        exact: bool,
    },
    Failed(String),
}

/// Sends a worker's result back to the event loop.
pub type Send = Arc<dyn Fn(DiffResult) -> bool + std::marker::Send + Sync>;

/// Generations are unique across every compare, so a result finds its
/// compare by generation alone.
static GENERATION: AtomicU64 = AtomicU64::new(1);

/// A local re-diff of more lines than this goes back to the worker as a
/// full diff instead of running on the main thread.
const REDIFF_BUDGET_LINES: usize = 20_000;

/// Typing pauses this long before an over-the-limit pair is recompared.
const RECOMPARE_AFTER: Duration = Duration::from_millis(300);

/// Changed-word marks are worked out for line pairs up to this long.
const MARK_LIMIT: usize = 4096;

/// What a click on the compare area means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    /// Nothing to do.
    None,
    /// Put the right side's cursor at this byte.
    Cursor(usize),
    /// A fold row was clicked; it has been toggled.
    Toggled,
}

/// A compare attached to a tab.
pub struct Compare {
    /// The left side.
    pub other: Document,
    /// Its view: the highlight cache and horizontal scroll.
    pub other_view: View,
    /// What the left side is.
    pub what: Other,
    state: State,
    generation: u64,
    send: Send,
    /// `other` and the right side's snapshot versions the state describes.
    version: u64,
    rows: Rows<Change>,
    /// One column, changes stacked, instead of two side by side.
    pub unified: bool,
    /// The row at the top of the screen, and how far it is scrolled up.
    scroll_row: usize,
    scroll_px: f32,
    /// In the fixed state: when an edit last made the diff stale.
    dirty_since: Option<Instant>,
    /// Old-side line starts of folded runs the user opened.
    unfolded: HashSet<usize>,
    /// Row plans for the frame, left and right; reused.
    plans: [Vec<RowPlan>; 2],
    /// The change navigated to last.
    pub current: Option<usize>,
    status: String,
}

impl std::fmt::Debug for Compare {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compare")
            .field("what", &self.what)
            .field("state", &self.state)
            .field("generation", &self.generation)
            .field("rows", &self.rows.len())
            .finish_non_exhaustive()
    }
}

impl Compare {
    /// Start comparing `doc` (the right side) with `other`. Begins noting
    /// `doc`'s edits and starts the first diff on a worker.
    pub fn start(doc: &mut Document, other: Document, what: Other, send: Send) -> Self {
        doc.watch_edits(true);
        let mut c = Self {
            other,
            other_view: View::default(),
            what,
            state: State::Pending,
            generation: 0,
            send,
            version: doc.version,
            rows: Rows::two_way(&[], 0, 0, Folding::default()),
            unified: false,
            scroll_row: 0,
            scroll_px: 0.0,
            dirty_since: None,
            unfolded: HashSet::new(),
            plans: [Vec::new(), Vec::new()],
            current: None,
            status: String::new(),
        };
        c.other_view.wrap = false;
        c.restart(doc);
        c
    }

    /// Stop watching the document; the tab goes back to one column.
    #[expect(clippy::unused_self, reason = "consumes the compare: it is over")]
    pub fn finish(self, doc: &mut Document) {
        doc.watch_edits(false);
    }

    /// Like [`start`](Compare::start), but diffs on this thread before
    /// returning: for headless rendering and tests, which have no event
    /// loop to deliver a worker's result.
    pub fn start_sync(doc: &mut Document, other: Document, what: Other) -> Self {
        let send: Send = Arc::new(|_| false);
        let mut c = Self::start(doc, other, what, send);
        let old: Arc<dyn Snapshot> = c.other.buffer.snapshot().into();
        let new: Arc<dyn Snapshot> = doc.buffer.snapshot().into();
        let result = DiffResult {
            generation: c.generation,
            version: doc.version,
            outcome: diff_now(&old, &new),
        };
        c.receive(doc, result);
        c
    }

    /// Whether `result` answers this compare's latest request.
    #[must_use]
    pub fn wants(&self, result: &DiffResult) -> bool {
        result.generation == self.generation
    }

    /// Diff both texts from scratch on a worker.
    fn restart(&mut self, doc: &Document) {
        self.generation = GENERATION.fetch_add(1, Ordering::Relaxed);
        self.version = doc.version;
        self.state = State::Pending;
        self.dirty_since = None;
        let generation = self.generation;
        let version = doc.version;
        let old: Arc<dyn Snapshot> = self.other.buffer.snapshot().into();
        let new: Arc<dyn Snapshot> = doc.buffer.snapshot().into();
        let send = Arc::clone(&self.send);
        let work = move || {
            send(DiffResult {
                generation,
                version,
                outcome: diff_now(&old, &new),
            });
        };
        if let Err(e) = std::thread::Builder::new().name("diff".into()).spawn(work) {
            log::warn!("diff thread: {e}");
            self.state = State::Failed(e.to_string());
        }
    }

    /// A worker finished. Returns whether the screen changed.
    pub fn receive(&mut self, doc: &Document, result: DiffResult) -> bool {
        if result.generation != self.generation {
            return false;
        }
        if result.version != doc.version {
            // Edited while the worker ran: its answer describes old text.
            self.restart(doc);
            return false;
        }
        self.state = match result.outcome {
            Ok(DiffKind::Live(d)) => State::Live(d),
            Ok(DiffKind::Fixed(c)) => State::Fixed {
                hunks: c.hunks,
                old_lines: c.old_lines,
                new_lines: c.new_lines,
                exact: c.exact,
            },
            Err(e) => State::Failed(e),
        };
        self.rebuild_rows();
        true
    }

    /// Bring the diff up to date with the document's edits since the last
    /// frame. Called before drawing.
    pub fn sync(&mut self, doc: &mut Document) {
        let (spans, reset) = doc.take_edits();
        if reset {
            self.restart(doc);
            return;
        }
        if spans.is_empty() {
            if let (State::Fixed { .. }, Some(since)) = (&self.state, self.dirty_since)
                && since.elapsed() >= RECOMPARE_AFTER
            {
                self.restart(doc);
            }
            return;
        }
        match &mut self.state {
            State::Live(live) => {
                let after: Arc<dyn Snapshot> = doc.buffer.snapshot().into();
                let mut restart = false;
                for span in spans {
                    let Some(span) = span else {
                        restart = true;
                        break;
                    };
                    match live.edit(Side::New, &after, span) {
                        Ok(region) if region.lines() <= REDIFF_BUDGET_LINES => live.rediff(&region),
                        Ok(_) | Err(_) => {
                            restart = true;
                            break;
                        }
                    }
                }
                if restart {
                    self.restart(doc);
                } else {
                    self.version = doc.version;
                    self.rebuild_rows();
                }
            }
            State::Fixed { .. } => {
                self.dirty_since.get_or_insert_with(Instant::now);
            }
            State::Pending | State::Failed(_) => {}
        }
    }

    /// Whether a recompare is waiting on a typing pause, for the event
    /// loop to wake up for.
    #[must_use]
    pub fn wake_at(&self) -> Option<Instant> {
        self.dirty_since.map(|t| t + RECOMPARE_AFTER)
    }

    fn hunks(&self) -> Option<(&[Hunk], usize, usize)> {
        match &self.state {
            State::Live(d) => Some((d.hunks(), d.lines(Side::Old), d.lines(Side::New))),
            State::Fixed {
                hunks,
                old_lines,
                new_lines,
                ..
            } => Some((hunks, *old_lines, *new_lines)),
            State::Pending | State::Failed(_) => None,
        }
    }

    fn rebuild_rows(&mut self) {
        let Some((hunks, old_lines, new_lines)) = self.hunks() else {
            return;
        };
        let folding = Folding::default();
        let mut rows = if self.unified {
            Rows::unified(hunks, old_lines, new_lines, folding)
        } else {
            Rows::two_way(hunks, old_lines, new_lines, folding)
        };
        // Keep the runs the user opened open: find each by the old line
        // that starts it.
        for &start in &self.unfolded {
            let row = rows.row_of(0, start);
            if let Some(r) = rows.at(row)
                && matches!(r.cells[0], Cell::Fold { .. })
            {
                rows.toggle_fold(row);
            }
        }
        self.rows = rows;
        self.scroll_row = self.scroll_row.min(self.rows.len().saturating_sub(1));
    }

    /// Switch between two columns and one.
    pub fn toggle_unified(&mut self) {
        self.unified = !self.unified;
        self.rebuild_rows();
    }

    /// How many rows fit on screen.
    fn visible_rows(layout: &Layout) -> usize {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a small non-negative row count"
        )]
        let n = ((layout.bottom - layout.top) / layout.metrics.line_height)
            .ceil()
            .max(0.0) as usize;
        n
    }

    /// Scroll by `dy` pixels (positive is down).
    pub fn scroll_by(&mut self, dy: f32, layout: &Layout) {
        let lh = layout.metrics.line_height;
        let total = self.rows.len();
        let max_row = total.saturating_sub(Self::visible_rows(layout).saturating_sub(1).max(1));
        let mut px = self.scroll_px + dy;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "whole rows scrolled, a small number"
        )]
        let whole = (px / lh).floor().abs() as usize;
        if px >= lh {
            self.scroll_row = (self.scroll_row + whole).min(max_row);
            px -= f(whole) * lh;
        } else if px < 0.0 {
            if self.scroll_row >= whole {
                self.scroll_row -= whole;
                px += f(whole) * lh;
            } else {
                self.scroll_row = 0;
                px = 0.0;
            }
        }
        if self.scroll_row >= max_row {
            self.scroll_row = max_row;
            px = 0.0;
        }
        self.scroll_px = px.max(0.0);
    }

    /// Scroll so `row` is on screen, with a little room.
    fn show_row(&mut self, row: usize, layout: &Layout) {
        let visible = Self::visible_rows(layout).max(1);
        if row < self.scroll_row + 1 {
            self.scroll_row = row.saturating_sub(1);
            self.scroll_px = 0.0;
        } else if row + 2 >= self.scroll_row + visible {
            self.scroll_row = (row + 2)
                .saturating_sub(visible)
                .min(self.rows.len().saturating_sub(1));
            self.scroll_px = 0.0;
        }
    }

    /// Keep the right side's cursor on screen, opening a fold it is in.
    pub fn ensure_cursor_visible(&mut self, doc: &Document, layout: &Layout) {
        if self.rows.is_empty() {
            return;
        }
        let line = doc.buffer.byte_to_line(doc.cursor());
        let mut row = self.rows.row_of(1, line);
        if let Some(r) = self.rows.at(row)
            && let Cell::Fold { first, .. } = r.cells[0]
        {
            self.unfolded.insert(first);
            self.rows.toggle_fold(row);
            row = self.rows.row_of(1, line);
        }
        self.show_row(row, layout);
    }

    /// Go to the next change after the cursor (or the current one).
    /// Returns the byte to put the cursor at.
    pub fn next_change(&mut self, doc: &Document, layout: &Layout) -> Option<usize> {
        let line = doc.buffer.byte_to_line(doc.cursor());
        let row = self.rows.row_of(1, line);
        let from = match self.current {
            Some(n)
                if self.rows.change_row(n) == Some(row)
                    || self.rows.change_row(n).is_some_and(|r| r <= row) =>
            {
                self.rows.change_row(n + 1)?
            }
            _ => row + 1,
        };
        let (n, start) = self.rows.next_change(from)?;
        self.go_to_change(doc, n, start, layout)
    }

    /// Go to the change before the cursor.
    pub fn prev_change(&mut self, doc: &Document, layout: &Layout) -> Option<usize> {
        let line = doc.buffer.byte_to_line(doc.cursor());
        let row = self.rows.row_of(1, line);
        let (n, start) = self.rows.prev_change(row)?;
        self.go_to_change(doc, n, start, layout)
    }

    fn go_to_change(
        &mut self,
        doc: &Document,
        n: usize,
        start: usize,
        layout: &Layout,
    ) -> Option<usize> {
        self.current = Some(n);
        self.show_row(start, layout);
        // The cursor goes to the change's first line on the right, or to
        // the line after a pure removal.
        let mut row = start;
        loop {
            let r = self.rows.at(row)?;
            if let Cell::Line(l) = r.cells[1] {
                return Some(doc.buffer.line_to_byte(l));
            }
            if r.change.is_none() {
                return Some(
                    doc.buffer
                        .line_to_byte(doc.buffer.len_lines().saturating_sub(1)),
                );
            }
            row += 1;
        }
    }

    /// The row and column under `(x, y)`, if over the text area.
    fn row_at(&self, layout: &Layout, x: f32, y: f32) -> Option<(usize, usize)> {
        if y < layout.top || y >= layout.bottom || x < layout.left || x >= layout.width {
            return None;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a non-negative row on screen"
        )]
        let r = ((y - layout.top + self.scroll_px) / layout.metrics.line_height).floor() as usize;
        let column = usize::from(!(self.unified || x < self.split(layout)));
        Some((self.scroll_row + r, column))
    }

    /// Where the two columns meet.
    fn split(&self, layout: &Layout) -> f32 {
        if self.unified {
            layout.width
        } else {
            f32::midpoint(layout.left, layout.width).round()
        }
    }

    /// The layouts of the left and right columns.
    fn columns(&self, layout: &Layout) -> (Layout, Layout) {
        if self.unified {
            (*layout, *layout)
        } else {
            let mid = self.split(layout);
            (
                layout.column(layout.left, mid),
                layout.column(mid, layout.width),
            )
        }
    }

    /// A click at `(x, y)`. `view` is the right side's view, which maps
    /// the x to a byte of a line.
    pub fn click(
        &mut self,
        doc: &Document,
        view: &mut View,
        layout: &Layout,
        tab_width: usize,
        x: f32,
        y: f32,
    ) -> Click {
        let Some((row, column)) = self.row_at(layout, x, y) else {
            return Click::None;
        };
        let Some(r) = self.rows.at(row) else {
            return Click::None;
        };
        match r.cells[column.min(1)] {
            Cell::Fold { first, .. } => {
                if self.unfolded.contains(&first) {
                    self.unfolded.remove(&first);
                } else {
                    self.unfolded.insert(first);
                }
                self.rows.toggle_fold(row);
                Click::Toggled
            }
            Cell::Line(_) | Cell::Filler if column == 1 || self.unified => {
                // The right side's line on this row, or the nearest below.
                let mut at = row;
                while let Some(r) = self.rows.at(at) {
                    if let Cell::Line(l) = r.cells[1] {
                        let (_, right) = self.columns(layout);
                        let start = doc.buffer.line_to_byte(l);
                        let pos = nimbus_core::wrap::RowPos { start, row: 0 };
                        return Click::Cursor(view.byte_at_x(x, pos, doc, &right, tab_width));
                    }
                    at += 1;
                }
                Click::None
            }
            _ => Click::None,
        }
    }

    /// The summary for the status bar, followed by the editor's own
    /// status `info`.
    pub fn status(&mut self, info: &str) -> &str {
        use std::fmt::Write as _;
        self.status.clear();
        match &self.what {
            Other::Saved => self.status.push_str("Compare with saved"),
            Other::File(p) => {
                let _ = write!(
                    self.status,
                    "Compare with {}",
                    p.file_name().and_then(|n| n.to_str()).unwrap_or("file")
                );
            }
            Other::Tab(name) => {
                let _ = write!(self.status, "Compare with {name}");
            }
        }
        match &self.state {
            State::Pending => self.status.push_str(" · comparing…"),
            State::Failed(e) => {
                let _ = write!(self.status, " · cannot compare: {e}");
            }
            State::Live(_) | State::Fixed { .. } => {
                let n = self.rows.changes();
                match (self.current, n) {
                    (_, 0) => self.status.push_str(" · no changes"),
                    (Some(c), _) => {
                        let _ = write!(self.status, " · change {} of {n}", c + 1);
                    }
                    (None, 1) => self.status.push_str(" · 1 change"),
                    (None, _) => {
                        let _ = write!(self.status, " · {n} changes");
                    }
                }
                if let State::Fixed { exact: false, .. } = &self.state {
                    self.status
                        .push_str(" · large file: changes may not be minimal");
                }
                if self.dirty_since.is_some() {
                    self.status.push_str(" · updating…");
                }
            }
        }
        if !info.is_empty() {
            let _ = write!(self.status, " · {info}");
        }
        &self.status
    }

    /// Plan the rows on screen for both columns. Returns how far the first
    /// row is scrolled up.
    fn plan(&mut self, doc: &Document, layout: &Layout, ui: &Ui) -> f32 {
        let visible = Self::visible_rows(layout) + 1;
        let [left, right] = &mut self.plans;
        left.clear();
        right.clear();
        if self.rows.is_empty() {
            // Nothing known yet: both texts from the top, unaligned.
            let (o, d) = (&self.other.buffer, &doc.buffer);
            for i in 0..visible {
                let row = self.scroll_row + i;
                left.push(RowPlan {
                    line: (row < lines_of(o)).then_some(row),
                    fold: None,
                    background: None,
                    mark: None,
                });
                right.push(RowPlan {
                    line: (row < lines_of(d)).then_some(row),
                    fold: None,
                    background: None,
                    mark: None,
                });
            }
            return self.scroll_px;
        }
        for i in 0..visible {
            let Some(r) = self.rows.at(self.scroll_row + i) else {
                break;
            };
            let (bg_left, bg_right) = match r.change {
                Some((Change::Added, _)) => (Some(ui.diff_filler), Some(ui.diff_added)),
                Some((Change::Removed, _)) => (Some(ui.diff_removed), Some(ui.diff_filler)),
                Some((Change::Changed, _)) => (Some(ui.diff_changed), Some(ui.diff_changed)),
                None => (None, None),
            };
            let mut l = plan_of(r.cells[0], bg_left);
            let mut rt = plan_of(r.cells[1], bg_right);
            if self.unified {
                // One column: a row shows one side; the other draws nothing.
                if l.line.is_none() && l.fold.is_none() {
                    l.background = None;
                }
                if rt.line.is_none() && rt.fold.is_none() {
                    rt.background = None;
                }
                if l.fold.is_some() {
                    rt.fold = None;
                }
            }
            if let (Some((Change::Changed, _)), Some(a), Some(b)) = (r.change, l.line, rt.line) {
                let (ma, mb) = word_marks(&self.other.buffer, a, &doc.buffer, b);
                l.mark = ma;
                rt.mark = mb;
            }
            left.push(l);
            right.push(rt);
        }
        self.scroll_px
    }

    /// Queue the compare area: both columns and their scrollbars. The
    /// caller started the frame and draws the bars after.
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
        preedit: Option<&crate::view::Preedit>,
        matches: (&[Range<usize>], Option<Range<usize>>),
    ) {
        let ui = theme.ui;
        let px = self.plan(doc, layout, &ui);
        let (left, right) = self.columns(layout);
        view.scroll_x = view.scroll_x.max(0.0);
        self.other_view.scroll_x = view.scroll_x;
        {
            let captures = match self.other.syntax.capture_names() {
                Some((key, names)) => theme.capture_looks(key, names),
                None => &[],
            };
            let deco = Decorations {
                cursor_visible: false,
                preedit: None,
                matches: &[],
                current: None,
                ui: &ui,
                captures,
            };
            self.other_view.draw_rows(
                gpu,
                &self.other,
                &left,
                tab_width,
                &deco,
                &self.plans[0],
                px,
                ui.diff_word,
            );
        }
        {
            let captures = match doc.syntax.capture_names() {
                Some((key, names)) => theme.capture_looks(key, names),
                None => &[],
            };
            let deco = Decorations {
                cursor_visible,
                preedit,
                matches: matches.0,
                current: matches.1,
                ui: &ui,
                captures,
            };
            view.draw_rows(
                gpu,
                doc,
                &right,
                tab_width,
                &deco,
                &self.plans[1],
                px,
                ui.diff_word,
            );
        }
        if !self.unified {
            // The divider between the columns.
            gpu.begin_layer(None);
            let x = self.split(layout);
            gpu.push_rect(
                [x - 1.0, layout.top],
                [
                    (1.0 * layout.scale).round().max(1.0),
                    layout.bottom - layout.top,
                ],
                ui.bar,
            );
        }
        self.draw_scrollbar(gpu, layout, &ui);
    }

    /// A scrollbar over the rows, at the right edge of the area.
    fn draw_scrollbar(&self, gpu: &mut Gpu, layout: &Layout, ui: &Ui) {
        let total = self.rows.len().max(1);
        let visible = Self::visible_rows(layout);
        if visible >= total {
            return;
        }
        let track = layout.bottom - layout.top;
        let (total, visible, scrolled) = (f(total), f(visible), f(self.scroll_row));
        let h = (track * visible / total)
            .max(2.0 * layout.metrics.line_height)
            .min(track);
        let y = layout.top + (track - h) * (scrolled / (total - visible).max(1.0));
        let w = (6.0 * layout.scale).round();
        gpu.begin_layer(None);
        gpu.push_rect(
            [layout.width - w - 2.0 * layout.scale, y],
            [w, h],
            ui.scrollbar,
        );
    }
}

/// The diff itself: live within the limit, layered past it.
fn diff_now(old: &Arc<dyn Snapshot>, new: &Arc<dyn Snapshot>) -> Result<DiffKind, String> {
    if old.len_bytes() <= DIFF_LIMIT && new.len_bytes() <= DIFF_LIMIT {
        LiveDiff::new(old, new)
            .map(|d| DiffKind::Live(Box::new(d)))
            .map_err(|e| e.to_string())
    } else {
        compare(old, new, Options::default())
            .map(DiffKind::Fixed)
            .map_err(|e| e.to_string())
    }
}

/// A small count as pixels.
#[expect(clippy::cast_precision_loss, reason = "row counts on screen are small")]
fn f(n: usize) -> f32 {
    n as f32
}

fn lines_of(t: &dyn Text) -> usize {
    let n = t.len_lines();
    // The diff's convention: no empty line after a final newline.
    if n > 0 && (t.len_bytes() == 0 || t.byte_at(t.len_bytes() - 1) == Some(b'\n')) {
        n - 1
    } else {
        n
    }
}

fn plan_of(cell: Cell, background: Option<[f32; 4]>) -> RowPlan {
    match cell {
        Cell::Line(l) => RowPlan {
            line: Some(l),
            fold: None,
            background,
            mark: None,
        },
        Cell::Filler => RowPlan {
            line: None,
            fold: None,
            background,
            mark: None,
        },
        Cell::Fold { hidden, .. } => RowPlan {
            line: None,
            fold: Some(hidden),
            background: None,
            mark: None,
        },
    }
}

/// The differing middle of two lines (common start and end trimmed), as
/// byte ranges in each text, for lines of ordinary length.
fn word_marks(
    a: &dyn Text,
    la: usize,
    b: &dyn Text,
    lb: usize,
) -> (Option<Range<usize>>, Option<Range<usize>>) {
    let sa = a.line_span(a.line_to_byte(la));
    let sb = b.line_span(b.line_to_byte(lb));
    if sa.end - sa.start > MARK_LIMIT || sb.end - sb.start > MARK_LIMIT {
        return (None, None);
    }
    let (ta, tb) = (a.slice(sa.start..sa.end), b.slice(sb.start..sb.end));
    let (xa, xb) = (ta.as_bytes(), tb.as_bytes());
    let prefix = xa.iter().zip(xb).take_while(|(p, q)| p == q).count();
    let max_suffix = xa.len().min(xb.len()) - prefix;
    let suffix = xa
        .iter()
        .rev()
        .zip(xb.iter().rev())
        .take(max_suffix)
        .take_while(|(p, q)| p == q)
        .count();
    // Widen to character boundaries so a mark never splits a character.
    let bounds = |s: &str, start: usize, end: usize| -> Range<usize> {
        let mut st = start;
        while st > 0 && !s.is_char_boundary(st) {
            st -= 1;
        }
        let mut en = end;
        while en < s.len() && !s.is_char_boundary(en) {
            en += 1;
        }
        st..en
    };
    let ra = bounds(&ta, prefix, xa.len() - suffix);
    let rb = bounds(&tb, prefix, xb.len() - suffix);
    (
        Some(sa.start + ra.start..sa.start + ra.end),
        Some(sb.start + rb.start..sb.start + rb.end),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_marks_trim_common_ends() {
        let a = Document::from_text("let x = compute(1, 2);\n");
        let b = Document::from_text("let x = compute(1, 3);\n");
        let (ma, mb) = word_marks(&a.buffer, 0, &b.buffer, 0);
        assert_eq!(ma, Some(19..20));
        assert_eq!(mb, Some(19..20));
        let a = Document::from_text("é\n");
        let b = Document::from_text("ê\n");
        let (ma, _) = word_marks(&a.buffer, 0, &b.buffer, 0);
        assert_eq!(ma, Some(0..2), "widened to the character");
    }

    fn layout() -> Layout {
        let metrics = nimbus_render::Metrics {
            size_px: 14.0,
            cell_width: 8.0,
            line_height: 16.0,
            baseline: 12.0,
        };
        Layout::new(metrics, 800, 400, 1.0, 100).with_chrome(true, 0, true)
    }

    #[test]
    fn a_compare_follows_edits_and_counts_changes() {
        let mut doc = Document::from_text("a\nb\nc\nd\n");
        let other = Document::from_text("a\nB\nc\nd\n");
        let mut c = Compare::start_sync(&mut doc, other, Other::Saved);
        assert_eq!(c.rows.changes(), 1);
        assert!(c.status("").starts_with("Compare with saved · 1 change"));
        // Type on the last line: a second change appears after a sync.
        doc.click(6, false);
        doc.insert("X").unwrap();
        c.sync(&mut doc);
        assert_eq!(c.rows.changes(), 2, "{:?}", c.hunks());
        assert!(c.status("Ln 1").contains("2 changes · Ln 1"));
        // Undo brings it back to one.
        doc.undo().unwrap();
        c.sync(&mut doc);
        assert_eq!(c.rows.changes(), 1);
        // Next change from the top lands on line 1 of the right side.
        doc.click(0, false);
        let layout = layout();
        assert_eq!(c.next_change(&doc, &layout), Some(2));
        assert_eq!(c.current, Some(0));
        assert!(c.status("").contains("change 1 of 1"));
        // Unified rows stack the old line over the new one.
        c.toggle_unified();
        assert_eq!(c.rows.len(), 5);
        c.toggle_unified();
        assert_eq!(c.rows.len(), 4);
    }

    #[test]
    fn plans_cover_the_screen_with_fillers_and_backgrounds() {
        let mut doc = Document::from_text("a\nx\ny\nb\n");
        let other = Document::from_text("a\nb\n");
        let mut c = Compare::start_sync(&mut doc, other, Other::Saved);
        let layout = layout();
        let ui = Ui::default();
        c.plan(&doc, &layout, &ui);
        let [left, right] = &c.plans;
        assert_eq!(left[1].line, None, "filler on the left for added lines");
        assert_eq!(right[1].line, Some(1));
        assert_eq!(right[1].background, Some(ui.diff_added));
        assert_eq!(left[1].background, Some(ui.diff_filler));
        assert_eq!(left[3].line, Some(1));
        assert_eq!(right[3].line, Some(3));
        assert_eq!(right[3].background, None);
    }

    #[test]
    fn line_counts_follow_the_diff_convention() {
        assert_eq!(lines_of(&Document::from_text("a\nb\n").buffer), 2);
        assert_eq!(lines_of(&Document::from_text("a\nb").buffer), 2);
        assert_eq!(lines_of(&Document::from_text("").buffer), 0);
    }
}
