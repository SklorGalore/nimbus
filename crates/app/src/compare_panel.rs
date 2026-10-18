//! The sidebar's Compare mode (M18): the compare options as buttons, then
//! the files that differ between two folders, as a folder compare
//! ([`crate::folder_compare`]) finds them.
//!
//! The options (ignore whitespace, case, blank lines, unified) apply to
//! every compare view; each is also a palette command. The app copies
//! their current state into [`Panel::options`] before each draw, so the
//! buttons never disagree with what the compare shows.
//!
//! One row per file, sorted by path, with a mark for how it differs:
//! `−` only in the left folder, `+` only in the right, `≠` in both but
//! different, `!` unreadable. Identical files are counted, not listed.
//! Enter or a click opens the pair in the compare view; "Next Change"
//! past a file's last change steps to the next file.

use std::path::Path;
use std::sync::Arc;

use nimbus_core::search::Generation;
use nimbus_core::{Normalize, Whitespace};
use nimbus_render::Gpu;

use crate::compare::Toggle;
use crate::folder_compare::{Batch, Results, Status};
use crate::input::Motion;
use crate::theme::Ui;
use crate::view::Layout;

/// What a key did in the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Not a key the panel takes: it goes to the editor.
    Ignored,
    /// Taken (the selection moved, or nothing to do).
    Handled,
    /// Enter on this row: open it.
    Open(usize),
}

/// What a click in the panel asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    /// Nothing under the press.
    None,
    /// Flip this compare option.
    Toggle(Toggle),
    /// Open this row of the folder compare.
    Open(usize),
}

/// The compare options as the buttons show them.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// The diff options every compare uses.
    pub normalize: Normalize,
    /// Whether the active tab's compare is unified; `None` when the
    /// active tab is not comparing.
    pub unified: Option<bool>,
}

/// The Compare panel's state.
#[derive(Debug, Default)]
pub struct Panel {
    /// The comparison shown, once one was started.
    pub results: Option<Results>,
    /// Cancels an older comparison when a newer one starts.
    pub generation: Arc<Generation>,
    /// The highlighted row.
    pub selected: Option<usize>,
    /// The row opened in the compare view last, for stepping on.
    pub opened: Option<usize>,
    /// The option buttons' state, refreshed by the app before drawing.
    pub options: Options,
    /// The first visible row.
    scroll: usize,
    /// Scroll left over from trackpad deltas smaller than a row.
    wheel: f32,
}

/// The option buttons, in order, at the top of the panel.
const TOGGLES: [Toggle; 4] = [
    Toggle::Whitespace,
    Toggle::Case,
    Toggle::BlankLines,
    Toggle::Unified,
];

/// Rows above the list: a label, the option buttons, a gap, then the two
/// roots and the counts.
const OPTION_ROWS: usize = 1 + TOGGLES.len() + 1;
const SUMMARY_ROWS: usize = 2;

impl Options {
    /// A button's label and whether it is on.
    fn button(self, toggle: Toggle) -> (&'static str, bool) {
        let n = self.normalize;
        match toggle {
            Toggle::Whitespace => (
                match n.whitespace {
                    Whitespace::Exact => "Ignore whitespace",
                    Whitespace::Ends => "Ignore whitespace: ends",
                    Whitespace::All => "Ignore whitespace: all",
                },
                n.whitespace != Whitespace::Exact,
            ),
            Toggle::Case => ("Ignore case", n.case),
            Toggle::BlankLines => ("Ignore blank lines", n.blank_lines),
            Toggle::Unified => ("Unified", self.unified == Some(true)),
        }
    }
}

impl Panel {
    /// Begin showing comparison `generation` of `left` with `right`.
    pub fn begin(&mut self, generation: u64, left: &Path, right: &Path) {
        self.results = Some(Results::new(
            generation,
            left.to_path_buf(),
            right.to_path_buf(),
        ));
        self.selected = None;
        self.opened = None;
        self.scroll = 0;
    }

    /// Take a batch from the workers. Returns whether anything changed.
    pub fn receive(&mut self, batch: Batch) -> bool {
        self.results.as_mut().is_some_and(|r| r.receive(batch))
    }

    fn len(&self) -> usize {
        self.results.as_ref().map_or(0, |r| r.rows().len())
    }

    /// Where the folder summary starts, below the option buttons.
    fn summary_top(layout: &Layout, header: f32) -> f32 {
        #[expect(clippy::cast_precision_loss, reason = "a few rows")]
        let options = OPTION_ROWS as f32 * layout.metrics.line_height;
        header + options
    }

    /// Where the list starts, below the options and the summary.
    fn list_top(layout: &Layout, header: f32) -> f32 {
        #[expect(clippy::cast_precision_loss, reason = "two rows")]
        let summary = SUMMARY_ROWS as f32 * layout.metrics.line_height;
        Self::summary_top(layout, header) + summary
    }

    fn visible_rows(layout: &Layout, header: f32) -> usize {
        let h = layout.bottom - Self::list_top(layout, header);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a small non-negative row count"
        )]
        let rows = (h / layout.metrics.line_height).floor().max(1.0) as usize;
        rows
    }

    fn show_row(&mut self, row: usize, layout: &Layout, header: f32) {
        let rows = Self::visible_rows(layout, header);
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll + rows {
            self.scroll = row + 1 - rows;
        }
    }

    /// Scroll by `rows` (negative is up).
    pub fn scroll_by(&mut self, rows: isize, layout: &Layout, header: f32) {
        let max = self
            .len()
            .saturating_sub(Self::visible_rows(layout, header));
        self.scroll = self.scroll.saturating_add_signed(rows).min(max);
    }

    /// Scroll by a wheel movement of `dy` physical pixels.
    pub fn wheel(&mut self, dy: f32, layout: &Layout, header: f32) {
        self.wheel += dy / layout.metrics.line_height;
        let whole = self.wheel.trunc();
        self.wheel -= whole;
        #[expect(clippy::cast_possible_truncation, reason = "a small row count")]
        self.scroll_by(whole as isize, layout, header);
    }

    /// A click at physical `y`: an option button flips, a row of the
    /// folder compare is selected and opened.
    pub fn click(&mut self, layout: &Layout, header: f32, y: f32) -> Click {
        let lh = layout.metrics.line_height;
        let first = header + lh;
        if y >= first {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a small non-negative row index"
            )]
            let i = ((y - first) / lh).floor() as usize;
            if let Some(&t) = TOGGLES.get(i) {
                return Click::Toggle(t);
            }
        }
        let top = Self::list_top(layout, header);
        if y < top {
            return Click::None;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a small non-negative row index"
        )]
        let row = ((y - top) / layout.metrics.line_height).floor() as usize + self.scroll;
        if row < self.len() {
            self.selected = Some(row);
            Click::Open(row)
        } else {
            Click::None
        }
    }

    /// A key while the panel has focus: arrows move, Enter opens a row.
    pub fn key(
        &mut self,
        motion: Option<Motion>,
        enter: bool,
        layout: &Layout,
        header: f32,
    ) -> Key {
        let n = self.len();
        if enter {
            return self
                .selected
                .filter(|&s| s < n)
                .map_or(Key::Handled, Key::Open);
        }
        let Some(motion) = motion else {
            return Key::Ignored;
        };
        if n == 0 {
            return Key::Handled;
        }
        let page = Self::visible_rows(layout, header).saturating_sub(1).max(1);
        let cur = self.selected.unwrap_or(0);
        let next = match motion {
            Motion::Up => cur.saturating_sub(1),
            Motion::Down if self.selected.is_none() => 0,
            Motion::Down => cur + 1,
            Motion::PageUp => cur.saturating_sub(page),
            Motion::PageDown => cur + page,
            Motion::DocStart | Motion::LineStart => 0,
            Motion::DocEnd | Motion::LineEnd => n - 1,
            _ => cur,
        }
        .min(n - 1);
        self.selected = Some(next);
        self.show_row(next, layout, header);
        Key::Handled
    }

    /// The row to open after `row` for "Next Change" past the end of a
    /// file (or before it, going back), selected and shown.
    pub fn step(&mut self, forward: bool, layout: &Layout, header: f32) -> Option<usize> {
        let results = self.results.as_ref()?;
        let from = self.opened?;
        let next = if forward {
            results.next_after(from)
        } else {
            results.prev_before(from)
        }?;
        self.selected = Some(next);
        self.show_row(next, layout, header);
        Some(next)
    }

    /// A one-line summary for the status of a finished or running compare.
    #[must_use]
    pub fn summary(&self) -> String {
        let Some(r) = &self.results else {
            return "Run Compare Folders… from the palette".to_owned();
        };
        let mut s = format!(
            "{} differ · {} only left · {} only right · {} same",
            r.differs, r.only_left, r.only_right, r.same
        );
        if r.errors > 0 {
            use std::fmt::Write as _;
            let _ = write!(s, " · {} unreadable", r.errors);
        }
        if !r.done {
            s.push_str(" · comparing…");
        }
        s
    }
}

/// The name of a folder for the summary line.
fn folder_name(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The option buttons: a label row, then one full-width button per
/// option. On is a bordered box in the theme's "on" colour with an
/// underline; off is a quieter solid fill (the border colour at half
/// strength), so both read as buttons on the dark and the light theme.
fn draw_options(
    gpu: &mut Gpu,
    ui: &Ui,
    layout: &Layout,
    options: Options,
    (header, width): (f32, f32),
) {
    let (cell, lh) = (layout.metrics.cell_width, layout.metrics.line_height);
    let pad = layout.margin;
    gpu.push_text_line([pad, header], "Compare options", ui.gutter);
    let line = layout.scale.round().max(1.0);
    let accent = (2.0 * layout.scale).round().max(1.0);
    let inset = (0.1 * lh).round();
    let bw = (width - 2.0 * pad).max(0.0);
    let mut row_y = header + lh;
    for toggle in TOGGLES {
        let (label, on) = options.button(toggle);
        let (top, bh) = (row_y + inset, lh - 2.0 * inset);
        if on {
            gpu.push_rect([pad, top], [bw, bh], ui.picker_border);
            gpu.push_rect(
                [pad + line, top + line],
                [bw - 2.0 * line, bh - 2.0 * line],
                ui.toggle_on,
            );
            gpu.push_rect([pad, top + bh - accent], [bw, accent], ui.tab_accent);
        } else {
            let [r, g, b, a] = ui.picker_border;
            gpu.push_rect([pad, top], [bw, bh], [r, g, b, a * 0.5]);
        }
        // Unified only means something while the active tab compares.
        let color = if toggle == Toggle::Unified && options.unified.is_none() {
            ui.gutter
        } else {
            ui.text
        };
        gpu.push_text_line([pad + cell, row_y], label, color);
        row_y += lh;
    }
}

/// Draw the panel below the sidebar's header, `w` pixels wide.
pub fn draw(
    gpu: &mut Gpu,
    ui: &Ui,
    layout: &Layout,
    panel: &Panel,
    (header, w): (f32, f32),
    focused: bool,
) {
    let m = layout.metrics;
    let (cell, lh) = (m.cell_width, m.line_height);
    let pad = layout.margin;
    let bottom = layout.bottom;
    draw_options(gpu, ui, layout, panel.options, (header, w));
    let mut y = Panel::summary_top(layout, header);
    let Some(results) = &panel.results else {
        gpu.push_text_line([pad, y], &panel.summary(), ui.gutter);
        return;
    };
    let title = format!(
        "{} ↔ {}",
        folder_name(&results.left),
        folder_name(&results.right)
    );
    gpu.push_text_line([pad, y], &title, ui.text);
    y += lh;
    gpu.push_text_line([pad, y], &panel.summary(), ui.gutter);
    y += lh;
    for (i, entry) in results.rows().iter().enumerate().skip(panel.scroll) {
        if y >= bottom {
            break;
        }
        if panel.selected == Some(i) {
            let color = if focused {
                ui.picker_selected
            } else {
                ui.selection
            };
            gpu.push_rect([0.0, y], [w, lh], color);
        }
        let mark = match entry.status {
            Status::OnlyLeft => "−",
            Status::OnlyRight => "+",
            Status::Differs | Status::Same => "≠",
            Status::Error(_) => "!",
        };
        gpu.push_text_line([pad, y], mark, ui.gutter);
        let rel = entry.rel.to_string_lossy();
        gpu.push_text_span([pad + 2.0 * cell, y], &rel, 0, ui.text, 0.0);
        y += lh;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folder_compare::Entry;
    use nimbus_render::Metrics;
    use std::path::PathBuf;

    fn layout() -> Layout {
        let metrics = Metrics {
            size_px: 14.0,
            line_height: 20.0,
            baseline: 15.0,
            cell_width: 10.0,
        };
        Layout::new(metrics, 800, 400, 1.0, 10).with_chrome(true, 0, false)
    }

    fn panel() -> Panel {
        let mut p = Panel::default();
        p.begin(1, Path::new("/a"), Path::new("/b"));
        let entries = ["x.rs", "y.rs", "z.rs"]
            .iter()
            .zip([Status::OnlyLeft, Status::Differs, Status::OnlyRight])
            .map(|(rel, status)| Entry {
                rel: PathBuf::from(rel),
                status,
            })
            .collect();
        assert!(p.receive(Batch {
            generation: 1,
            entries,
            same: 5,
            done: true,
        }));
        p
    }

    #[test]
    fn keys_select_and_enter_opens() {
        let l = layout();
        let mut p = panel();
        assert_eq!(p.key(Some(Motion::Down), false, &l, 20.0), Key::Handled);
        assert_eq!(p.selected, Some(0));
        p.key(Some(Motion::DocEnd), false, &l, 20.0);
        assert_eq!(p.key(None, true, &l, 20.0), Key::Open(2));
        assert!(
            p.summary()
                .starts_with("1 differ · 1 only left · 1 only right · 5 same")
        );
        let r = p.results.as_ref().unwrap();
        assert_eq!(r.pair(0), (Some(PathBuf::from("/a/x.rs")), None));
    }

    #[test]
    fn clicks_pick_rows_below_the_summary_and_step_moves_on() {
        let l = layout();
        let mut p = panel();
        let top = Panel::list_top(&l, 20.0);
        assert_eq!(p.click(&l, 20.0, top + 21.0), Click::Open(1));
        let summary = Panel::summary_top(&l, 20.0);
        assert_eq!(p.click(&l, 20.0, summary + 5.0), Click::None, "the summary");
        assert_eq!(p.click(&l, 20.0, 25.0), Click::None, "the options label");
        assert_eq!(p.click(&l, 20.0, 45.0), Click::Toggle(Toggle::Whitespace));
        assert_eq!(p.click(&l, 20.0, 105.0), Click::Toggle(Toggle::Unified));
        p.opened = Some(1);
        assert_eq!(p.step(true, &l, 20.0), Some(2));
        assert_eq!(p.step(false, &l, 20.0), Some(0));
    }
}
