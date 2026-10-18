//! The sidebar: the project tree in a column left of the editor.
//!
//! This module draws the tree, works out what a click or a key does to it,
//! and returns any folder listings that need starting. The app runs those on
//! worker threads and passes the results back to the [`Tree`].

use std::path::{Path, PathBuf};

use nimbus_render::Gpu;

use crate::document::{Document, Solo};
use crate::input::Motion;
use crate::search_panel::{self, Panel};
use crate::theme::Ui;
use crate::tree::{ListRequest, NodeId, Tree};
use crate::tree_ops::{self, EditKind};
use crate::view::{self, Layout};

/// Default width in logical pixels (multiplied by the scale factor).
pub const DEFAULT_WIDTH: f32 = 240.0;
/// Narrowest the sidebar can be dragged, in logical pixels.
pub const MIN_WIDTH: f32 = 120.0;
/// How far either side of the divider a press starts a resize, in logical
/// pixels.
const GRAB: f32 = 4.0;

/// What the sidebar column shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The project tree.
    Files,
    /// Find in files.
    Search,
}

/// The sidebar's state.
#[derive(Debug)]
pub struct Sidebar {
    /// Shown.
    pub visible: bool,
    /// The tree, or the Search panel.
    pub mode: Mode,
    /// The Search panel; kept while the tree is shown, so its results
    /// survive switching.
    pub search: Panel,
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
    /// The inline field naming a new file or folder, or renaming one.
    pub edit: Option<TreeEdit>,
}

/// The tree's inline text field: New File, New Folder or Rename. The field
/// is a [`Document`], like the find bar's, so every editing key works in it.
#[derive(Debug)]
pub struct TreeEdit {
    /// What the name is for.
    pub kind: EditKind,
    /// The folder the name goes in.
    pub dir: PathBuf,
    /// Rename: the node renamed. New: the folder whose row the new row
    /// follows, `None` for the top of the tree.
    pub node: Option<NodeId>,
    /// The name being typed.
    pub field: Solo,
}

/// What a click or key in the sidebar asks the app to do.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Folder listings to start.
    pub requests: Vec<ListRequest>,
    /// A file to open in a tab.
    pub open: Option<PathBuf>,
    /// What the Search panel asks for.
    pub search: Option<search_panel::Reply>,
}

impl Sidebar {
    /// A sidebar for the project at `root`.
    #[must_use]
    pub fn new(root: PathBuf, visible: bool, width: f32) -> Self {
        Self {
            visible,
            mode: Mode::Files,
            search: Panel::default(),
            width: width.max(MIN_WIDTH),
            focused: false,
            resizing: false,
            scroll: 0,
            tree: Tree::new(root),
            wheel: 0.0,
            edit: None,
        }
    }

    /// Start naming a new file or folder in the selected folder (the
    /// selected file's folder, or the root), or renaming the selected node.
    /// Returns listings to start (the folder opens), or `None` when there
    /// is nothing to rename.
    pub fn begin_edit(&mut self, kind: EditKind) -> Option<Vec<ListRequest>> {
        let selected = self.tree.selected.filter(|&s| self.tree.node(s).is_some());
        let mut field = Solo {
            doc: Document::untitled(),
            carets: crate::document::Carets::default(),
        };
        let mut requests = Vec::new();
        let (dir, node) = if kind == EditKind::Rename {
            let id = selected?;
            let n = self.tree.node(id)?;
            let dir = n.path.parent()?.to_path_buf();
            let mut ed = field.ed();
            let _ = ed.insert(&n.name);
            ed.select(tree_ops::stem_range(&n.name, n.is_dir));
            (dir, Some(id))
        } else {
            match selected.and_then(|s| self.tree.node(s).map(|n| (s, n))) {
                Some((id, n)) if n.is_dir => {
                    let dir = n.path.clone();
                    requests = self.tree.set_expanded(id, true);
                    (dir, Some(id))
                }
                Some((id, n)) => {
                    let parent = self.tree.parent(id);
                    let dir = n
                        .path
                        .parent()
                        .map_or_else(|| self.tree.root().to_path_buf(), Path::to_path_buf);
                    (dir, parent)
                }
                None => (self.tree.root().to_path_buf(), None),
            }
        };
        self.edit = Some(TreeEdit {
            kind,
            dir,
            node,
            field,
        });
        self.focused = true;
        self.mode = Mode::Files;
        Some(requests)
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
        if self.mode == Mode::Search {
            self.search.scroll_by(rows, layout);
            return;
        }
        self.scroll = self.scroll.saturating_add_signed(rows);
        self.clamp_scroll(layout);
    }

    /// Scroll by a wheel or trackpad movement of `dy` physical pixels
    /// (positive is down the list).
    pub fn wheel(&mut self, dy: f32, layout: &Layout) {
        if self.mode == Mode::Search {
            self.search.wheel(dy, layout);
            return;
        }
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

    /// Which mode's tab in the header is at `x`, if any.
    fn header_tab_at(layout: &Layout, x: f32, y: f32) -> Option<Mode> {
        if y >= Self::header_h(layout) {
            return None;
        }
        let cell = layout.metrics.cell_width;
        let files = layout.margin..layout.margin + 5.0 * cell;
        let search = files.end + cell..files.end + 7.0 * cell;
        if files.contains(&x) {
            Some(Mode::Files)
        } else if search.contains(&x) {
            Some(Mode::Search)
        } else {
            None
        }
    }

    /// A click at physical `(x, y)`: in the header, switch mode; in the
    /// tree, select the row (a folder opens or closes, a file opens in a
    /// tab); in the Search panel, whatever the panel says.
    pub fn click(&mut self, layout: &Layout, x: f32, y: f32) -> Outcome {
        self.focused = true;
        // A click anywhere drops an unfinished name.
        self.edit = None;
        if let Some(mode) = Self::header_tab_at(layout, x, y) {
            self.mode = mode;
            return Outcome::default();
        }
        if self.mode == Mode::Search {
            return Outcome {
                search: Some(self.search.click(layout, x, y)),
                ..Outcome::default()
            };
        }
        let Some(row) = self.row_at(layout, y) else {
            return Outcome::default();
        };
        let id = self.tree.rows()[row].node;
        self.tree.selected = Some(id);
        self.activate(id)
    }

    /// Select the tree row at physical `y` without opening it (a
    /// right-click), focusing the tree. Returns whether there was a row.
    pub fn select_at(&mut self, layout: &Layout, y: f32) -> bool {
        if self.mode != Mode::Files {
            return false;
        }
        self.edit = None;
        self.focused = true;
        let row = self.row_at(layout, y);
        self.tree.selected = row.map(|r| self.tree.rows()[r].node);
        true
    }

    /// Enter, or a click, on node `id`.
    fn activate(&mut self, id: NodeId) -> Outcome {
        let Some(node) = self.tree.node(id) else {
            return Outcome::default();
        };
        if node.is_dir {
            Outcome {
                requests: self.tree.toggle(id),
                ..Outcome::default()
            }
        } else {
            Outcome {
                open: Some(node.path.clone()),
                ..Outcome::default()
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
/// tab's file (canonical path), marked in the tree. `blink` is whether the
/// text cursor is in its visible phase, for the Search panel's fields.
pub fn draw(
    gpu: &mut Gpu,
    ui: &Ui,
    layout: &Layout,
    sidebar: &Sidebar,
    active: Option<&Path>,
    blink: bool,
) {
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
    // The header: the two modes as tabs, then the project's name.
    let y = ((header - lh) / 2.0).round();
    let accent = (2.0 * layout.scale).round().max(1.0);
    let mut x = pad;
    for (mode, label) in [(Mode::Files, "Files"), (Mode::Search, "Search")] {
        let on = sidebar.mode == mode;
        let color = if on { ui.text } else { ui.gutter };
        gpu.push_text_line([x, y], label, color);
        let cols = cols(label.len());
        if on {
            gpu.push_rect([x, header - accent], [cols * cell, accent], ui.tab_accent);
        }
        x += (cols + 1.0) * cell;
    }
    // The project's name sits against the right edge, apart from the
    // tabs, so it does not read as a third one; a name that would run
    // into the tabs is left out rather than clipped.
    let title = sidebar
        .tree
        .node(crate::tree::ROOT)
        .map_or("", |n| n.name.as_str());
    let title_w = cols(title.chars().count()) * cell;
    let title_x = w - pad - title_w;
    if !title.is_empty() && title_x >= x + cell {
        gpu.push_text_line([title_x, y], title, ui.gutter);
    }

    if sidebar.mode == Mode::Search {
        search_panel::draw(gpu, ui, layout, &sidebar.search, sidebar.focused, blink);
        let line = layout.scale.round().max(1.0);
        gpu.push_rect([w - line, 0.0], [line, bottom], ui.picker_border);
        gpu.begin_layer(None);
        return;
    }

    draw_tree(gpu, ui, layout, sidebar, active, blink);
    // The divider.
    let line = layout.scale.round().max(1.0);
    gpu.push_rect([w - line, 0.0], [line, bottom], ui.picker_border);
    gpu.begin_layer(None);
}

/// The tree's rows below the header, with the name field when one is open:
/// in place of the renamed row, or as a new row at the top of its folder.
fn draw_tree(
    gpu: &mut Gpu,
    ui: &Ui,
    layout: &Layout,
    sidebar: &Sidebar,
    active: Option<&Path>,
    blink: bool,
) {
    let (cell, lh) = (layout.metrics.cell_width, layout.metrics.line_height);
    let (w, pad, bottom) = (layout.left, layout.margin, layout.bottom);
    let accent = (2.0 * layout.scale).round().max(1.0);
    let cursor = sidebar.focused && blink;
    let edit = sidebar.edit.as_ref();
    let new_after = edit.filter(|e| e.kind != EditKind::Rename).map(|e| e.node);
    let new_mark = match edit.map(|e| e.kind) {
        Some(EditKind::NewFolder) => "▸",
        _ => "",
    };
    let mut y = Sidebar::header_h(layout);
    if let Some(e) = edit.filter(|_| new_after == Some(None) && sidebar.scroll == 0) {
        draw_edit_row(gpu, ui, layout, e, (y, 0), new_mark, cursor);
        y += lh;
    }
    for row in sidebar.tree.rows().iter().skip(sidebar.scroll) {
        if y >= bottom {
            break;
        }
        let Some(node) = sidebar.tree.node(row.node) else {
            continue;
        };
        let mark = match (node.is_dir, node.expanded) {
            (true, true) => "▾",
            (true, false) => "▸",
            (false, _) => "",
        };
        if let Some(e) = edit.filter(|e| e.kind == EditKind::Rename && e.node == Some(row.node)) {
            draw_edit_row(gpu, ui, layout, e, (y, row.depth), mark, cursor);
            y += lh;
            continue;
        }
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
        gpu.push_text_line([x, y], mark, ui.gutter);
        gpu.push_text_span([x + 2.0 * cell, y], &node.name, 0, ui.text, 0.0);
        y += lh;
        if let Some(e) = edit.filter(|_| new_after == Some(Some(row.node))) {
            draw_edit_row(gpu, ui, layout, e, (y, row.depth + 1), new_mark, cursor);
            y += lh;
        }
    }
}

/// One tree row holding the name field, at `(y, depth)`, after a folder
/// `mark` (or none).
fn draw_edit_row(
    gpu: &mut Gpu,
    ui: &Ui,
    layout: &Layout,
    edit: &TreeEdit,
    (y, depth): (f32, usize),
    mark: &str,
    cursor: bool,
) {
    let cell = layout.metrics.cell_width;
    let x = layout.margin + cols(depth * 2) * cell;
    gpu.push_text_line([x, y], mark, ui.gutter);
    let at = [x + 2.0 * cell, y];
    let width = (layout.left - at[0] - layout.margin).max(4.0 * cell);
    gpu.push_rect(at, [width, layout.metrics.line_height], ui.field);
    view::draw_field(gpu, ui, &edit.field, at, width, layout, cursor);
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
    fn name_fields_go_in_the_selected_folder() {
        let l = layout();
        let mut s = sidebar();
        assert!(s.begin_edit(EditKind::Rename).is_none(), "nothing selected");
        // Nothing selected: a new file goes at the top of the root.
        s.begin_edit(EditKind::NewFile).unwrap();
        let e = s.edit.take().unwrap();
        assert_eq!((e.dir, e.node), (PathBuf::from("/p"), None));
        // A folder selected: inside it, which opens.
        s.key(Some(Motion::Down), false, &l).unwrap();
        let req = s.begin_edit(EditKind::NewFolder).unwrap();
        assert_eq!(req.len(), 1, "src is listed");
        let e = s.edit.take().unwrap();
        assert_eq!(e.dir, PathBuf::from("/p/src"));
        // A file selected: beside it; Rename starts with its name, the
        // part before the extension selected.
        s.tree.loaded(&req[0], vec![entry("main.rs", false)]);
        s.key(Some(Motion::Down), false, &l).unwrap();
        s.begin_edit(EditKind::NewFile).unwrap();
        assert_eq!(s.edit.take().unwrap().dir, PathBuf::from("/p/src"));
        s.begin_edit(EditKind::Rename).unwrap();
        let e = s.edit.as_ref().unwrap();
        assert_eq!(
            e.field.carets.selected_text(&e.field.doc.buffer).as_deref(),
            Some("main")
        );
        assert_eq!(e.dir, PathBuf::from("/p/src"));
        assert!(s.focused);
    }

    #[test]
    fn clicks_select_rows_and_the_divider_resizes() {
        let l = layout();
        let mut s = sidebar();
        let row_y = |row: f32| Sidebar::header_h(&l) + row * l.metrics.line_height + 1.0;
        let out = s.click(&l, 50.0, row_y(1.0));
        assert_eq!(out.open, Some(PathBuf::from("/p/README")));
        assert!(s.focused);
        let out = s.click(&l, 50.0, row_y(0.0));
        assert_eq!(out.requests.len(), 1, "a folder opens");
        assert!(
            s.click(&l, 50.0, row_y(9.0)).open.is_none(),
            "below the rows"
        );
        // The header's tabs switch modes.
        s.click(&l, l.margin + 70.0, 1.0);
        assert_eq!(s.mode, Mode::Search);
        assert!(s.click(&l, 50.0, row_y(1.0)).search.is_some());
        s.click(&l, l.margin + 10.0, 1.0);
        assert_eq!(s.mode, Mode::Files);
        assert!(s.on_divider(&l, 201.0) && !s.on_divider(&l, 150.0));
        s.drag_to(&l, 10.0);
        assert_eq!(s.width, MIN_WIDTH);
        s.drag_to(&l, 700.0);
        assert!(s.width <= 800.0 * 2.0 / 3.0);
    }
}
