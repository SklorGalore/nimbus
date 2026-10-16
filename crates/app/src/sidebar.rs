//! The sidebar: the project tree in a column left of the editor.
//!
//! This module draws the tree, works out what a click or a key does to it,
//! and returns any folder listings that need starting. The app runs those on
//! worker threads and passes the results back to the [`Tree`].

use std::path::{Path, PathBuf};

use nimbus_render::Gpu;

use crate::input::Motion;
use crate::theme::Ui;
use crate::tree::{ListRequest, NodeId, Tree};
use crate::view::Layout;

/// Default width in logical pixels (multiplied by the scale factor).
pub const DEFAULT_WIDTH: f32 = 240.0;
/// Narrowest the sidebar can be dragged, in logical pixels.
pub const MIN_WIDTH: f32 = 120.0;
/// How far either side of the divider a press starts a resize, in logical
/// pixels.
const GRAB: f32 = 4.0;

/// The sidebar's state.
#[derive(Debug)]
pub struct Sidebar {
    /// Shown.
    pub visible: bool,
    /// Width in logical pixels.
    pub width: f32,
    /// Has keyboard focus (arrows move in the tree, not the text).
    pub focused: bool,
    /// The divider is being dragged.
    pub resizing: bool,
    /// The first visible row.
    pub scroll: usize,
    /// The project tree.
    pub tree: Tree,
    /// Scroll left over from trackpad deltas smaller than a row.
    wheel: f32,
}

/// What a click or key in the sidebar asks the app to do.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Folder listings to start.
    pub requests: Vec<ListRequest>,
    /// A file to open in a tab.
    pub open: Option<PathBuf>,
}

impl Sidebar {
    /// A sidebar for the project at `root`.
    #[must_use]
    pub fn new(root: PathBuf, visible: bool, width: f32) -> Self {
        Self {
            visible,
            width: width.max(MIN_WIDTH),
            focused: false,
            resizing: false,
            scroll: 0,
            tree: Tree::new(root),
            wheel: 0.0,
        }
    }

    /// Width in physical pixels at `scale`, or 0 when hidden.
    #[must_use]
    pub fn px(&self, scale: f32) -> f32 {
        if self.visible {
            (self.width * scale).round()
        } else {
            0.0
        }
    }

    /// Height of the root's title row, aligned with the tab bar.
    fn header_h(layout: &Layout) -> f32 {
        layout.tabs_h.max(layout.metrics.line_height)
    }

    /// Rows that fit below the title and above any message line.
    fn visible_rows(layout: &Layout) -> usize {
        let h = layout.bottom - Self::header_h(layout);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a small non-negative row count"
        )]
        let rows = (h / layout.metrics.line_height).floor().max(1.0) as usize;
        rows
    }

    /// Scroll so row `row` is on screen.
    fn show_row(&mut self, row: usize, layout: &Layout) {
        let rows = Self::visible_rows(layout);
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll + rows {
            self.scroll = row + 1 - rows;
        }
    }

    /// Keep the scroll within the rows there are.
    fn clamp_scroll(&mut self, layout: &Layout) {
        let max = self
            .tree
            .rows()
            .len()
            .saturating_sub(Self::visible_rows(layout));
        self.scroll = self.scroll.min(max);
    }

    /// Scroll by `rows` (negative is up).
    pub fn scroll_by(&mut self, rows: isize, layout: &Layout) {
        self.scroll = self.scroll.saturating_add_signed(rows);
        self.clamp_scroll(layout);
    }

    /// Scroll by a wheel or trackpad movement of `dy` physical pixels
    /// (positive is down the list).
    pub fn wheel(&mut self, dy: f32, layout: &Layout) {
        self.wheel += dy / layout.metrics.line_height;
        let whole = self.wheel.trunc();
        self.wheel -= whole;
        #[expect(clippy::cast_possible_truncation, reason = "a small row count")]
        self.scroll_by(whole as isize, layout);
    }

    /// Whether the point is over the sidebar (divider included).
    #[must_use]
    pub fn contains(&self, layout: &Layout, x: f32) -> bool {
        self.visible && x < layout.left + GRAB * layout.scale
    }

    /// Whether the point is on the divider, where a press resizes.
    #[must_use]
    pub fn on_divider(&self, layout: &Layout, x: f32) -> bool {
        self.visible && (x - layout.left).abs() <= GRAB * layout.scale
    }

    /// Resize so the divider is at physical `x`, within `MIN_WIDTH` and
    /// two thirds of the window.
    pub fn drag_to(&mut self, layout: &Layout, x: f32) {
        let max = (layout.width * 2.0 / 3.0 / layout.scale).max(MIN_WIDTH);
        self.width = (x / layout.scale).clamp(MIN_WIDTH, max).round();
    }

    /// The row index under physical `y`, if any.
    fn row_at(&self, layout: &Layout, y: f32) -> Option<usize> {
        let top = Self::header_h(layout);
        if y < top || y >= layout.bottom {
            return None;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a small non-negative row index"
        )]
        let row = ((y - top) / layout.metrics.line_height).floor() as usize + self.scroll;
        (row < self.tree.rows().len()).then_some(row)
    }

    /// A click at physical `(x, y)`: select the row; a folder opens or
    /// closes, a file opens in a tab.
    pub fn click(&mut self, layout: &Layout, y: f32) -> Outcome {
        self.focused = true;
        let Some(row) = self.row_at(layout, y) else {
            return Outcome::default();
        };
        let id = self.tree.rows()[row].node;
        self.tree.selected = Some(id);
        self.activate(id)
    }

    /// Enter, or a click, on node `id`.
    fn activate(&mut self, id: NodeId) -> Outcome {
        let Some(node) = self.tree.node(id) else {
            return Outcome::default();
        };
        if node.is_dir {
            Outcome {
                requests: self.tree.toggle(id),
                open: None,
            }
        } else {
            Outcome {
                requests: Vec::new(),
                open: Some(node.path.clone()),
            }
        }
    }

    /// A key while the sidebar has focus. Returns `None` for keys it does
    /// not handle, which then go to the editor as usual.
    pub fn key(&mut self, motion: Option<Motion>, enter: bool, layout: &Layout) -> Option<Outcome> {
        let rows = self.tree.rows().len();
        if rows == 0 {
            // Still loading, or an empty folder: the keys are still ours.
            return (motion.is_some() || enter).then(Outcome::default);
        }
        let Some(current) = self.tree.selected.and_then(|s| self.tree.row_of(s)) else {
            // Nothing selected (or hidden in a closed folder): any key
            // starts at the top.
            self.tree.selected = Some(self.tree.rows()[0].node);
            self.show_row(0, layout);
            return Some(Outcome::default());
        };
        let page = Self::visible_rows(layout).saturating_sub(1).max(1);
        let select = |row: usize| Some(row.min(rows - 1));
        let mut out = Outcome::default();
        let target = if enter {
            let id = self.tree.rows()[current].node;
            out = self.activate(id);
            None
        } else {
            let id = self.tree.rows()[current].node;
            let node = self.tree.node(id)?;
            match motion? {
                Motion::Up => select(current.saturating_sub(1)),
                Motion::Down => select(current + 1),
                Motion::PageUp => select(current.saturating_sub(page)),
                Motion::PageDown => select(current + page),
                Motion::DocStart | Motion::LineStart => select(0),
                Motion::DocEnd | Motion::LineEnd => select(rows - 1),
                // Right opens a closed folder, then steps into it.
                Motion::Right | Motion::WordRight if node.is_dir && !node.expanded => {
                    out.requests = self.tree.set_expanded(id, true);
                    None
                }
                Motion::Right | Motion::WordRight => select(current + usize::from(node.is_dir)),
                // Left closes an open folder, else goes to the parent.
                Motion::Left | Motion::WordLeft if node.is_dir && node.expanded => {
                    out.requests = self.tree.set_expanded(id, false);
                    None
                }
                Motion::Left | Motion::WordLeft => {
                    self.tree.parent(id).and_then(|p| self.tree.row_of(p))
                }
            }
        };
        let row = target
            .unwrap_or(current)
            .min(self.tree.rows().len().saturating_sub(1));
        if let Some(r) = self.tree.rows().get(row) {
            self.tree.selected = Some(r.node);
        }
        self.show_row(row, layout);
        Some(out)
    }

    /// Select and show `path` (the active tab's file, canonical), opening
    /// the folders down to it.
    pub fn reveal(&mut self, path: &Path, layout: Option<&Layout>) -> Vec<ListRequest> {
        let out = self.tree.reveal(path);
        self.after_change(layout);
        out
    }

    /// After the tree changed shape: keep the selection on screen.
    pub fn after_change(&mut self, layout: Option<&Layout>) {
        if let Some(layout) = layout {
            if let Some(row) = self.tree.selected.and_then(|s| self.tree.row_of(s)) {
                self.show_row(row, layout);
            }
            self.clamp_scroll(layout);
        }
    }
}

#[expect(clippy::cast_precision_loss, reason = "small column counts")]
fn cols(n: usize) -> f32 {
    n as f32
}

/// Draw the sidebar left of the editor column. `active` is the active
/// tab's file (canonical path), marked in the tree.
pub fn draw(gpu: &mut Gpu, ui: &Ui, layout: &Layout, sidebar: &Sidebar, active: Option<&Path>) {
    if !sidebar.visible || layout.left <= 0.0 {
        return;
    }
    let m = layout.metrics;
    let (cell, lh) = (m.cell_width, m.line_height);
    let w = layout.left;
    let header = Sidebar::header_h(layout);
    // Down to the message line, which spans the window.
    let bottom = layout.bottom;
    gpu.begin_layer(Some([0.0, 0.0, w, bottom]));
    gpu.push_rect([0.0, 0.0], [w, bottom], ui.tab_bar);
    let pad = layout.margin;
    let title = sidebar
        .tree
        .node(crate::tree::ROOT)
        .map_or("", |n| n.name.as_str());
    gpu.push_text_line([pad, ((header - lh) / 2.0).round()], title, ui.gutter);

    let rows = sidebar.tree.rows();
    let accent = (2.0 * layout.scale).round().max(1.0);
    let mut y = header;
    for row in rows.iter().skip(sidebar.scroll) {
        if y >= bottom {
            break;
        }
        let Some(node) = sidebar.tree.node(row.node) else {
            continue;
        };
        if sidebar.tree.selected == Some(row.node) {
            let color = if sidebar.focused {
                ui.picker_selected
            } else {
                ui.selection
            };
            gpu.push_rect([0.0, y], [w, lh], color);
        }
        if active == Some(node.path.as_path()) {
            gpu.push_rect([0.0, y], [accent, lh], ui.tab_accent);
        }
        let x = pad + cols(row.depth * 2) * cell;
        if node.is_dir {
            let mark = if node.expanded { "▾" } else { "▸" };
            gpu.push_text_line([x, y], mark, ui.gutter);
        }
        gpu.push_text_span([x + 2.0 * cell, y], &node.name, 0, ui.text, 0.0);
        y += lh;
    }
    // The divider.
    let line = layout.scale.round().max(1.0);
    gpu.push_rect([w - line, 0.0], [line, bottom], ui.picker_border);
    gpu.begin_layer(None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Entry;
    use nimbus_render::Metrics;

    fn layout() -> Layout {
        let metrics = Metrics {
            size_px: 14.0,
            line_height: 20.0,
            baseline: 15.0,
            cell_width: 10.0,
        };
        Layout::new(metrics, 800, 400, 1.0, 10)
            .with_chrome(true, 0, false)
            .with_sidebar(200.0)
    }

    fn entry(name: &str, is_dir: bool) -> Entry {
        Entry {
            name: name.to_owned(),
            is_dir,
        }
    }

    /// A sidebar showing `/p` with a folder `src` (holding `main.rs`) and
    /// a file `README`.
    fn sidebar() -> Sidebar {
        let mut s = Sidebar::new(PathBuf::from("/p"), true, 200.0);
        let req = s.tree.start();
        s.tree
            .loaded(&req[0], vec![entry("src", true), entry("README", false)]);
        s
    }

    fn selected(s: &Sidebar) -> String {
        s.tree.node(s.tree.selected.unwrap()).unwrap().name.clone()
    }

    #[test]
    fn keys_move_open_and_close() {
        let l = layout();
        let mut s = sidebar();
        s.key(Some(Motion::Down), false, &l).unwrap();
        assert_eq!(selected(&s), "src", "nothing selected: the first row");
        // Right on a closed folder opens it (and asks for its listing).
        let out = s.key(Some(Motion::Right), false, &l).unwrap();
        assert_eq!(out.requests.len(), 1);
        s.tree
            .loaded(&out.requests[0], vec![entry("main.rs", false)]);
        // Right again steps into it; Enter on a file opens it.
        s.key(Some(Motion::Right), false, &l).unwrap();
        assert_eq!(selected(&s), "main.rs");
        let out = s.key(None, true, &l).unwrap();
        assert_eq!(out.open, Some(PathBuf::from("/p/src/main.rs")));
        // Left goes to the parent, Left again closes it.
        s.key(Some(Motion::Left), false, &l).unwrap();
        assert_eq!(selected(&s), "src");
        s.key(Some(Motion::Left), false, &l).unwrap();
        assert_eq!(s.tree.rows().len(), 2);
        s.key(Some(Motion::DocEnd), false, &l).unwrap();
        assert_eq!(selected(&s), "README");
    }

    #[test]
    fn clicks_select_rows_and_the_divider_resizes() {
        let l = layout();
        let mut s = sidebar();
        let row_y = |row: f32| Sidebar::header_h(&l) + row * l.metrics.line_height + 1.0;
        let out = s.click(&l, row_y(1.0));
        assert_eq!(out.open, Some(PathBuf::from("/p/README")));
        assert!(s.focused);
        let out = s.click(&l, row_y(0.0));
        assert_eq!(out.requests.len(), 1, "a folder opens");
        assert!(s.click(&l, row_y(9.0)).open.is_none(), "below the rows");
        assert!(s.on_divider(&l, 201.0) && !s.on_divider(&l, 150.0));
        s.drag_to(&l, 10.0);
        assert_eq!(s.width, MIN_WIDTH);
        s.drag_to(&l, 700.0);
        assert!(s.width <= 800.0 * 2.0 / 3.0);
    }
}
