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
//! - Only the rows on screen are planned and drawn ([`Columns`]), and the
//!   changed-word marks are worked out for those rows alone.
//!
//! M18 adds three things. The diff options (ignore whitespace, case,
//! blank lines), toggled in the sidebar's Compare tab, re-diff on the
//! worker like a fresh compare. A text with lines over 4 KB is diffed in
//! pieces ([`nimbus_core::split`]): such a pair always takes the layered
//! [`compare`] path, recomputed after a pause, because the live diff
//! follows edits line by line. And the left side can be a revision from
//! git ([`Other::Revision`]).

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use nimbus_core::align::{Change, Folding, MAX_COLUMNS, Row, Rows};
use nimbus_core::compare::{Comparison, Options, compare};
use nimbus_core::diff::{DIFF_LIMIT, Hunk, Normalize};
use nimbus_core::live_diff::{LiveDiff, Side};
use nimbus_core::split::{Splits, has_long_lines};
use nimbus_core::{Buffer, Snapshot, Text};
use nimbus_render::Gpu;

use crate::columns::{Bytes, Click, Columns, Live};
use crate::document::{Carets, Document};
use crate::theme::Theme;
use crate::view::{Layout, Preedit, View};

/// What the left side is, for the status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Other {
    /// The file as saved on disk.
    Saved,
    /// Another file.
    File(PathBuf),
    /// A copy of another tab's text, by its name.
    Tab(String),
    /// The file at a git revision (M18), by the revision's name.
    Revision(String),
    /// Nothing: a folder compare's file that only the right folder has.
    Missing,
}

/// A compare option, toggled from the sidebar's Compare tab or the palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    /// Ignore whitespace: off, at the ends, everywhere.
    Whitespace,
    /// Ignore ASCII case.
    Case,
    /// Drop changes of blank lines only.
    BlankLines,
    /// One column instead of two.
    Unified,
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
        /// Cut lines per side; when any, the above count units.
        splits: Box<[Splits; 2]>,
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

/// Text versions a live diff may keep alive (one per edit followed) before
/// it is redone from scratch at the next pause: bounds the memory a long
/// typing session pins.
const MAX_PINNED_TEXTS: usize = 512;

/// Changed-word marks are worked out for line pairs up to this long.
const MARK_LIMIT: usize = 4096;

/// A compare attached to a tab.
pub struct Compare {
    /// The left side.
    pub other: Document,
    /// Its view: the highlight cache and horizontal scroll.
    pub other_view: View,
    /// The other side's cursors, which stay at the start.
    still: Carets,
    /// What the left side is.
    pub what: Other,
    state: State,
    generation: u64,
    send: Send,
    /// The right side's version the state describes.
    version: u64,
    /// The aligned rows and everything drawn from them.
    pub columns: Columns<Change>,
    /// One column, changes stacked, instead of two side by side.
    pub unified: bool,
    /// In the fixed state: when an edit last made the diff stale.
    dirty_since: Option<Instant>,
    status: String,
    /// What counts as an equal line (M18).
    normalize: Normalize,
}

impl std::fmt::Debug for Compare {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compare")
            .field("what", &self.what)
            .field("state", &self.state)
            .field("generation", &self.generation)
            .field("columns", &self.columns)
            .finish_non_exhaustive()
    }
}

impl Compare {
    /// Start comparing `doc` (the right side) with `other`. Begins noting
    /// `doc`'s edits and starts the first diff on a worker.
    pub fn start(
        doc: &mut Document,
        other: Document,
        what: Other,
        normalize: Normalize,
        send: Send,
    ) -> Self {
        doc.watch_edits(true);
        let mut c = Self {
            other,
            other_view: View::default(),
            still: Carets::default(),
            what,
            state: State::Pending,
            generation: 0,
            send,
            version: doc.version,
            columns: Columns::new(Rows::two_way(&[], 0, 0, Folding::default()), 2, 1),
            unified: false,
            dirty_since: None,
            status: String::new(),
            normalize,
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
    pub fn start_sync(
        doc: &mut Document,
        other: Document,
        what: Other,
        normalize: Normalize,
    ) -> Self {
        let send: Send = Arc::new(|_| false);
        let mut c = Self::start(doc, other, what, normalize, send);
        let old: Arc<dyn Snapshot> = c.other.buffer.snapshot().into();
        let new: Arc<dyn Snapshot> = doc.buffer.snapshot().into();
        let result = DiffResult {
            generation: c.generation,
            version: doc.version,
            outcome: diff_now(&old, &new, normalize),
        };
        c.receive(doc, result);
        c
    }

    /// Diff with other options: a fresh compare on the worker.
    pub fn set_normalize(&mut self, doc: &Document, normalize: Normalize) {
        if normalize != self.normalize {
            self.normalize = normalize;
            self.restart(doc);
        }
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
        let normalize = self.normalize;
        let work = move || {
            send(DiffResult {
                generation,
                version,
                outcome: diff_now(&old, &new, normalize),
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
                splits: Box::new([c.old_splits, c.new_splits]),
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
            if let (State::Fixed { .. } | State::Live(_), Some(since)) =
                (&self.state, self.dirty_since)
                && since.elapsed() >= RECOMPARE_AFTER
            {
                self.restart(doc);
            }
            return;
        }
        match &mut self.state {
            State::Live(live) => {
                let mut restart = false;
                for span in spans {
                    let Some((span, after)) = span else {
                        restart = true;
                        break;
                    };
                    match live.edit(Side::New, &after, span) {
                        Ok(region) if region.lines() <= REDIFF_BUDGET_LINES => {
                            live.rediff(&region);
                        }
                        Ok(_) | Err(_) => {
                            restart = true;
                            break;
                        }
                    }
                }
                if restart {
                    self.restart(doc);
                } else {
                    if live.pinned_texts() > MAX_PINNED_TEXTS {
                        // Every edit pins its text version in the interner;
                        // a fresh diff at the next pause lets them go.
                        self.dirty_since.get_or_insert_with(Instant::now);
                    }
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

    /// The current hunks, when known.
    #[must_use]
    pub fn hunks(&self) -> Option<(&[Hunk], usize, usize)> {
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
        let units = match &self.state {
            State::Fixed { splits, .. } => {
                let [a, b] = (**splits).clone();
                [a, b, Splits::default()]
            }
            _ => Default::default(),
        };
        let Some((hunks, old_lines, new_lines)) = self.hunks() else {
            return;
        };
        let folding = Folding::default();
        let rows = if self.unified {
            Rows::unified(hunks, old_lines, new_lines, folding)
        } else {
            Rows::two_way(hunks, old_lines, new_lines, folding)
        };
        self.columns.set_rows(rows);
        self.columns.set_units(units);
    }

    /// How many rows the compare has.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.columns.rows.len()
    }

    /// The row under `(x, y)`, if any (M28: picking lines to stage).
    #[must_use]
    pub fn row_under(&self, layout: &Layout, x: f32, y: f32) -> Option<usize> {
        self.columns.row_under(layout, x, y)
    }

    /// The top of `row` on screen.
    #[must_use]
    pub fn row_y(&self, row: usize, layout: &Layout) -> f32 {
        self.columns.row_y(row, layout)
    }

    /// Scroll so `row` is on screen.
    pub fn show_row(&mut self, row: usize, layout: &Layout) {
        self.columns.show_row(row, layout);
    }

    /// The change `row` is part of, by number.
    #[must_use]
    pub fn change_at(&self, row: usize) -> Option<usize> {
        self.columns
            .rows
            .at(row)
            .and_then(|r| r.change)
            .map(|(_, n)| n)
    }

    /// The change stepped to last (Next Change), if any.
    #[must_use]
    pub fn current_change(&self) -> Option<usize> {
        self.columns.current
    }

    /// The rows of change `n`.
    #[must_use]
    pub fn change_rows(&self, n: usize) -> Option<Range<usize>> {
        let start = self.columns.rows.change_row(n)?;
        let mut end = start;
        while self.change_at(end) == Some(n) {
            end += 1;
        }
        Some(start..end)
    }

    /// The changed lines on `rows`, per side (old, new), as sorted ranges:
    /// what staging those rows picks. Equal rows and fillers give none.
    #[must_use]
    pub fn lines_in(&self, rows: Range<usize>) -> [Vec<Range<usize>>; 2] {
        let mut out: [Vec<Range<usize>>; 2] = [Vec::new(), Vec::new()];
        for row in rows {
            let Some(r) = self.columns.rows.at(row) else {
                break;
            };
            if r.change.is_none() {
                continue;
            }
            for (c, side) in out.iter_mut().enumerate() {
                if let nimbus_core::Cell::Line(unit) = r.cells[c] {
                    let line = self.columns.locate(c, unit).0;
                    match side.last_mut() {
                        Some(last) if last.end == line => last.end = line + 1,
                        Some(last) if last.contains(&line) => {}
                        _ => side.push(line..line + 1),
                    }
                }
            }
        }
        for side in &mut out {
            side.sort_by_key(|r| r.start);
        }
        out
    }

    /// Switch between two columns and one.
    pub fn toggle_unified(&mut self) {
        self.unified = !self.unified;
        self.columns.set_count(if self.unified { 1 } else { 2 });
        self.rebuild_rows();
    }

    /// Scroll by `dy` pixels (positive is down).
    pub fn scroll_by(&mut self, dy: f32, layout: &Layout) {
        self.columns.scroll_by(dy, layout);
    }

    /// Keep the right side's cursor on screen, opening a fold it is in.
    pub fn ensure_cursor_visible(&mut self, doc: &Document, cursor: usize, layout: &Layout) {
        let line = doc.buffer.byte_to_line(cursor);
        self.columns.ensure_line_visible(line, layout);
    }

    /// Go to the next change after the cursor (or the current one).
    /// Returns the byte to put the cursor at.
    pub fn next_change(&mut self, doc: &Document, cursor: usize, layout: &Layout) -> Option<usize> {
        self.step(true, doc, cursor, layout)
    }

    /// Go to the change before the cursor.
    pub fn prev_change(&mut self, doc: &Document, cursor: usize, layout: &Layout) -> Option<usize> {
        self.step(false, doc, cursor, layout)
    }

    fn step(
        &mut self,
        forward: bool,
        doc: &Document,
        cursor: usize,
        layout: &Layout,
    ) -> Option<usize> {
        let line = doc.buffer.byte_to_line(cursor);
        let start = self.columns.step_change(forward, line, layout)?;
        // The cursor goes to the change's first line on the right, or to
        // the line after a pure removal; at the very end, the last line.
        let line = self
            .columns
            .live_line_from(start)
            .unwrap_or_else(|| doc.buffer.len_lines().saturating_sub(1));
        Some(doc.buffer.line_to_byte(line))
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
        self.columns.click(doc, view, layout, tab_width, x, y)
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
            Other::Tab(name) | Other::Revision(name) => {
                let _ = write!(self.status, "Compare with {name}");
            }
            Other::Missing => self.status.push_str("Only in the right folder"),
        }
        match &self.state {
            State::Pending => self.status.push_str(" · comparing…"),
            State::Failed(e) => {
                let _ = write!(self.status, " · cannot compare: {e}");
            }
            State::Live(_) | State::Fixed { .. } => {
                let n = self.columns.rows.changes();
                match (self.columns.current, n) {
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
                if let State::Fixed { splits, .. } = &self.state
                    && splits.iter().any(|s| !s.is_empty())
                {
                    self.status.push_str(" · long lines compared in pieces");
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
    fn plan(&mut self, doc: &Document, layout: &Layout, theme: &Theme) -> f32 {
        if self.columns.rows.is_empty() {
            // Nothing known yet: both texts from the top, unaligned.
            let lines = [
                nimbus_core::diff_lines(&self.other.buffer),
                nimbus_core::diff_lines(&doc.buffer),
            ];
            return self.columns.plan_unaligned(layout, &lines);
        }
        let ui = theme.ui;
        let background = |r: &Row<Change>, c: usize| match (r.change, c) {
            (Some((Change::Added, _)), 0) | (Some((Change::Removed, _)), 1) => Some(ui.diff_filler),
            (Some((Change::Added, _)), _) => Some(ui.diff_added),
            (Some((Change::Removed, _)), _) => Some(ui.diff_removed),
            (Some((Change::Changed, _)), _) => Some(ui.diff_changed),
            (None, _) => None,
        };
        let (other, this): (&dyn Text, &dyn Text) = (&self.other.buffer, &doc.buffer);
        let marks = |r: &Row<Change>, bytes: &Bytes| -> [Option<Range<usize>>; MAX_COLUMNS] {
            if let (Some((Change::Changed, _)), Some(a), Some(b)) =
                (r.change, bytes[0].clone(), bytes[1].clone())
            {
                let (ma, mb) = word_marks(other, a, this, b);
                [ma, mb, None]
            } else {
                [None, None, None]
            }
        };
        self.columns.plan(layout, &[other, this], background, marks)
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
        carets: &Carets,
        view: &mut View,
        theme: &mut Theme,
        tab_width: usize,
        cursor_visible: bool,
        preedit: Option<&Preedit>,
        matches: (&[Range<usize>], Option<Range<usize>>),
    ) {
        self.plan(doc, layout, theme);
        let live = Live {
            cursor_visible,
            preedit,
            matches,
            tab_width,
        };
        self.columns.draw(
            gpu,
            layout,
            &[(&self.other, &self.still), (doc, carets)],
            &mut [&mut self.other_view, view],
            &live,
            theme,
        );
    }
}

/// The diff itself: live within the limit, layered past it.
///
/// A pair with a line over the split threshold takes the layered path
/// with splitting on, even within the limit: the live diff follows edits
/// line by line and knows nothing of pieces.
fn diff_now(
    old: &Arc<dyn Snapshot>,
    new: &Arc<dyn Snapshot>,
    normalize: Normalize,
) -> Result<DiffKind, String> {
    let within = old.len_bytes() <= DIFF_LIMIT && new.len_bytes() <= DIFF_LIMIT;
    if within && !has_long_lines(&**old) && !has_long_lines(&**new) {
        LiveDiff::with_normalize(old, new, normalize)
            .map(|d| DiffKind::Live(Box::new(d)))
            .map_err(|e| e.to_string())
    } else {
        let options = Options {
            normalize,
            split: within,
            ..Options::default()
        };
        compare(old, new, options)
            .map(DiffKind::Fixed)
            .map_err(|e| e.to_string())
    }
}

/// The differing middle of two lines or pieces `sa` and `sb` (common start
/// and end trimmed), as byte ranges in each text, for ones of ordinary
/// length.
fn word_marks(
    a: &dyn Text,
    sa: Range<usize>,
    b: &dyn Text,
    sb: Range<usize>,
) -> (Option<Range<usize>>, Option<Range<usize>>) {
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
pub(crate) mod tests {
    use super::*;
    use crate::document::Solo;

    #[test]
    fn word_marks_trim_common_ends() {
        let a = Solo::from_text("let x = compute(1, 2);\n");
        let b = Solo::from_text("let x = compute(1, 3);\n");
        let (ma, mb) = word_marks(&a.buffer, 0..22, &b.buffer, 0..22);
        assert_eq!(ma, Some(19..20));
        assert_eq!(mb, Some(19..20));
        let a = Solo::from_text("é\n");
        let b = Solo::from_text("ê\n");
        let (ma, _) = word_marks(&a.buffer, 0..2, &b.buffer, 0..2);
        assert_eq!(ma, Some(0..2), "widened to the character");
    }

    /// The bench pair `NIMBUS_DIFF_FILES=OLD:NEW` names, for the ignored
    /// benchmarks; `None` (skip) when unset.
    pub(crate) fn bench_pair() -> Option<(PathBuf, PathBuf)> {
        let spec = std::env::var("NIMBUS_DIFF_FILES").ok()?;
        let (a, b) = spec.split_once(':').expect("OLD:NEW");
        Some((PathBuf::from(a), PathBuf::from(b)))
    }

    /// 300 keystroke positions spread over a text of `len` bytes: a run of
    /// 50 at each of six places, so each run types into one place the way
    /// a person does, and the places cover the file.
    pub(crate) fn type_at(len: usize) -> impl Iterator<Item = usize> {
        (0..300).map(move |i| {
            let place = i / 50;
            (len / 12)
                .saturating_mul(2 * place + 1)
                .min(len.saturating_sub(1))
        })
    }

    /// Print `samples` as the typing bench does: p50, p99, max.
    pub(crate) fn report(label: &str, mut samples: Vec<std::time::Duration>) {
        samples.sort();
        let pick = |q: usize| {
            samples
                .get((samples.len() * q / 100).min(samples.len().saturating_sub(1)))
                .copied()
                .unwrap_or_default()
        };
        println!(
            "{label}: {} samples, p50 {:?}, p99 {:?}, max {:?}",
            samples.len(),
            pick(50),
            pick(99),
            samples.last().copied().unwrap_or_default()
        );
    }

    /// `NIMBUS_DIFF_FILES=OLD:NEW cargo test --release -p nimbus -- --ignored --nocapture compare_typing_bench`
    ///
    /// M9 task 11: the compare of the bench pair built on this thread
    /// (diff plus rows), then what a keystroke in the right side costs:
    /// the edit, the live diff's local re-diff and the row planning for
    /// one screen (drawing needs a GPU; its cost is the ordinary
    /// editor's).
    #[test]
    #[ignore = "benchmark: needs NIMBUS_DIFF_FILES and a release build"]
    fn compare_typing_bench() {
        let Some((old, new)) = bench_pair() else {
            return;
        };
        let mut doc = Solo::of(Document::open(&new).unwrap());
        let other = Document::open(&old).unwrap();
        let t = std::time::Instant::now();
        let mut c = Compare::start_sync(&mut doc, other, Other::File(old), Normalize::default());
        println!(
            "compare built in {:?}: {} rows, {} changes",
            t.elapsed(),
            c.columns.rows.len(),
            c.columns.rows.changes()
        );
        let layout = layout();
        let theme = Theme::default();
        let len = doc.buffer.len_bytes();
        let mut samples = Vec::new();
        for (i, at) in type_at(len).enumerate() {
            let ch = if i % 7 == 6 { "\n" } else { "x" };
            doc.ed().click(at, false);
            let t = std::time::Instant::now();
            doc.ed().insert(ch).unwrap();
            c.sync(&mut doc);
            c.plan(&doc, &layout, &theme);
            samples.push(t.elapsed());
        }
        report("compare keystroke (edit + live diff + plan)", samples);
        println!("after typing: {} changes", c.columns.rows.changes());
    }

    pub(crate) fn layout() -> Layout {
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
        let mut doc = Solo::from_text("a\nb\nc\nd\n");
        let other = Document::from_text("a\nB\nc\nd\n");
        let mut c = Compare::start_sync(&mut doc, other, Other::Saved, Normalize::default());
        assert_eq!(c.columns.rows.changes(), 1);
        assert!(c.status("").starts_with("Compare with saved · 1 change"));
        // Type on the last line: a second change appears after a sync.
        doc.ed().click(6, false);
        doc.ed().insert("X").unwrap();
        c.sync(&mut doc);
        assert_eq!(c.columns.rows.changes(), 2, "{:?}", c.hunks());
        assert!(c.status("Ln 1").contains("2 changes · Ln 1"));
        // Undo brings it back to one.
        doc.ed().undo().unwrap();
        c.sync(&mut doc);
        assert_eq!(c.columns.rows.changes(), 1);
        // Next change from the top lands on line 1 of the right side.
        doc.ed().click(0, false);
        let layout = layout();
        assert_eq!(
            c.next_change(&doc, doc.carets.cursor(&doc.buffer), &layout),
            Some(2)
        );
        assert_eq!(c.columns.current, Some(0));
        assert!(c.status("").contains("change 1 of 1"));
        // Unified rows stack the old line over the new one.
        c.toggle_unified();
        assert_eq!(c.columns.rows.len(), 5);
        c.toggle_unified();
        assert_eq!(c.columns.rows.len(), 4);
    }

    /// Several edits in one frame (here an undo of typed lines, which
    /// history replays batch by batch) each come with their own text, so
    /// the live diff re-reads the right lines for each. Before, every span
    /// was read against the final text and the diff drifted.
    #[test]
    fn many_edits_in_one_frame_keep_the_live_diff_right() {
        let base: String = (0..40).fold(String::new(), |mut s, i| {
            use std::fmt::Write as _;
            let _ = writeln!(s, "line {i}");
            s
        });
        let mut doc = Solo::from_text(&base);
        let other = Document::from_text(&base);
        let mut c = Compare::start_sync(&mut doc, other, Other::Saved, Normalize::default());
        assert_eq!(c.columns.rows.changes(), 0);
        let at = doc.buffer.line_to_byte(10);
        doc.ed().click(at, false);
        for i in 0..10 {
            doc.ed().insert(&format!("typed {i}\n")).unwrap();
        }
        c.sync(&mut doc);
        assert_eq!(c.columns.rows.changes(), 1);
        while doc.ed().undo().unwrap() && doc.is_dirty() {}
        assert_eq!(doc.buffer.slice(0..doc.buffer.len_bytes()), base);
        c.sync(&mut doc);
        let (hunks, old_lines, new_lines) = c.hunks().unwrap();
        assert_eq!((hunks, old_lines, new_lines), (&[][..], 40, 40));
        assert_eq!(c.columns.rows.changes(), 0);
        // And the same the other way: redo everything in one frame.
        while doc.ed().redo().unwrap() {}
        c.sync(&mut doc);
        assert_eq!(c.columns.rows.changes(), 1);
        assert_eq!(c.hunks().unwrap().2, 50);
    }

    #[test]
    fn plans_cover_the_screen_with_fillers_and_backgrounds() {
        let mut doc = Solo::from_text("a\nx\ny\nb\n");
        let other = Document::from_text("a\nb\n");
        let mut c = Compare::start_sync(&mut doc, other, Other::Saved, Normalize::default());
        let layout = layout();
        let theme = Theme::default();
        let ui = theme.ui;
        c.plan(&doc, &layout, &theme);
        let plans = c.columns.plans();
        let (left, right) = (&plans[0], &plans[1]);
        assert_eq!(left[1].line, None, "filler on the left for added lines");
        assert_eq!(right[1].line, Some(1));
        assert_eq!(right[1].background, Some(ui.diff_added));
        assert_eq!(left[1].background, Some(ui.diff_filler));
        assert_eq!(left[3].line, Some(1));
        assert_eq!(right[3].line, Some(3));
        assert_eq!(right[3].background, None);
    }

    #[test]
    fn options_re_diff() {
        let mut doc = Solo::from_text("fn a() {\n    x\n}\nY\n");
        let other = Document::from_text("fn a() {\n  x  \n}\ny\n");
        let mut c = Compare::start_sync(&mut doc, other, Other::Saved, Normalize::default());
        assert_eq!(c.columns.rows.changes(), 2);
        let n = Normalize {
            whitespace: nimbus_core::Whitespace::Ends,
            case: true,
            blank_lines: false,
        };
        c.set_normalize(&doc, n);
        // The re-diff runs on a worker; here, on this thread instead.
        let old: Arc<dyn Snapshot> = c.other.buffer.snapshot().into();
        let new: Arc<dyn Snapshot> = doc.buffer.snapshot().into();
        let result = DiffResult {
            generation: c.generation,
            version: doc.version,
            outcome: diff_now(&old, &new, n),
        };
        assert!(c.receive(&doc, result));
        assert_eq!(c.columns.rows.changes(), 0, "{:?}", c.hunks());
    }

    #[test]
    fn long_lines_compare_in_pieces_with_continuation_rows() {
        // Varied words, like real minified data (a short repeating cycle
        // would give the content-defined cuts nothing to find).
        let mut x = 7u64;
        let words: String = (0..3000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                [
                    "alpha ",
                    "beta ",
                    "{\"k\": 1}, ",
                    "gamma ",
                    "delta, ",
                    "[2] ",
                ][(x % 6) as usize]
            })
            .collect();
        let at = words.len() / 2;
        let edited = format!("{}CHANGED{}", &words[..at], &words[at..]);
        let mut doc = Solo::from_text(&format!("head\n{edited}\n"));
        let other = Document::from_text(&format!("head\n{words}\n"));
        let mut c = Compare::start_sync(&mut doc, other, Other::Saved, Normalize::default());
        // One small change among many pieces, not the whole line.
        assert_eq!(c.columns.rows.changes(), 1, "{:?}", c.hunks());
        let (hunks, old_units, _) = c.hunks().unwrap();
        assert!(old_units > 3, "the long line is cut: {old_units} units");
        assert_eq!(hunks[0].old.len(), 1, "{hunks:?}");
        assert!(c.status("").contains("long lines compared in pieces"));
        let layout = layout();
        let theme = Theme::default();
        c.plan(&doc, &layout, &theme);
        let right = &c.columns.plans()[1];
        assert_eq!(right[0].line, Some(0));
        assert!(right[0].piece.is_none(), "short lines draw whole");
        assert_eq!(right[1].line, Some(1));
        let (first, continued) = right[1].piece.clone().unwrap();
        assert!(!continued);
        assert_eq!(first.start, 5);
        let (second, continued) = right[2].piece.clone().unwrap();
        assert!(continued, "the second piece continues the line");
        assert_eq!(second.start, first.end);
        assert_eq!(right[2].line, Some(1));
    }

    #[test]
    fn line_counts_follow_the_diff_convention() {
        assert_eq!(
            nimbus_core::diff_lines(&Solo::from_text("a\nb\n").buffer),
            2
        );
        assert_eq!(nimbus_core::diff_lines(&Solo::from_text("a\nb").buffer), 2);
        assert_eq!(nimbus_core::diff_lines(&Solo::from_text("").buffer), 0);
    }
}
