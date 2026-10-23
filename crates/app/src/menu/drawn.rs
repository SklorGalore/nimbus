//! Menus Nimbus draws itself, in the theme's colours: the menu bar and its
//! drop-downs on Windows, and right-click menus on every system.
//!
//! A system menu (Windows' or macOS's) is a separate window the operating
//! system paints in its own colours, and while it is open it runs a loop
//! of its own that the app cannot draw into. A drawn menu is just more of
//! the frame: a strip along the top of the window ([`Menus::draw_bar`]) and
//! a box drawn over everything else while one is open
//! ([`Menus::draw_popup`]). While a menu is open, keys and clicks go to it
//! first, as with a prompt: the arrow keys move between items and menus,
//! Enter or a click runs the item's action, and Escape or a click elsewhere
//! closes it.
//!
//! The bar sits in the space the renderer keeps at the top of the window
//! (`Gpu::set_inset_top`, the same room the macOS title bar uses), so it is
//! drawn at negative y and the rest of the layout does not change.
//!
//! The items are the same lists the macOS menu bar shows ([`super::LAYOUT`]
//! and [`super::TREE_MENU`]), each an action with its title and shortcut.

use nimbus_render::Gpu;

use crate::action;
use crate::keymap::Keymap;
use crate::theme::Ui;
use crate::view::Layout;

/// Whether this system's menu bar is the one Nimbus draws. macOS keeps its
/// own at the top of the screen; Linux has none (the palette covers it).
pub const DRAWS_MENU_BAR: bool = cfg!(target_os = "windows");

/// One line of a menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// An action: its id, its title and its shortcut ("Ctrl+S", or empty).
    Item {
        /// The action's stable id, run when the item is chosen.
        id: &'static str,
        /// What the menu shows.
        title: &'static str,
        /// The shortcut, drawn on the right.
        keys: String,
    },
    /// A thin line between groups of items.
    Separator,
}

impl Entry {
    fn is_item(&self) -> bool {
        matches!(self, Entry::Item { .. })
    }
}

/// The entries for a list of action ids, `"-"` for a separator.
fn entries(ids: &[&str], keymap: &Keymap) -> Vec<Entry> {
    ids.iter()
        .filter_map(|id| {
            if *id == "-" {
                return Some(Entry::Separator);
            }
            let info = action::by_id(id)?;
            Some(Entry::Item {
                id: info.id,
                title: info.title,
                keys: keymap.label(info.id),
            })
        })
        .collect()
}

/// A key that means something to an open menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Up: the item above.
    Up,
    /// Down: the item below.
    Down,
    /// Left: the menu to the left on the bar.
    Left,
    /// Right: the menu to the right on the bar.
    Right,
    /// Enter or Space: run the highlighted item.
    Press,
    /// Escape: close the menu.
    Escape,
    /// A typed letter: run the first item starting with it.
    Letter(char),
}

/// What a click did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    /// Chose this action: run it (the menu is closed).
    Chose(&'static str),
    /// Landed on the menu or the bar but chose nothing, or opened a menu.
    Handled,
    /// Landed elsewhere: the menu closed, and the click goes no further,
    /// as with a system menu.
    Closed,
    /// No menu is open and the click is not on the bar.
    Ignored,
}

/// An open menu: a drop-down from the bar, or a right-click menu.
#[derive(Debug)]
struct Popup {
    entries: Vec<Entry>,
    /// Top-left corner, in layout coordinates (before keeping it on
    /// screen).
    at: [f32; 2],
    /// The highlighted entry, always an item.
    hover: Option<usize>,
    /// The bar menu it drops down from, if any.
    bar: Option<usize>,
    /// How far the entries are scrolled up, in pixels, when the menu is
    /// taller than the window.
    scroll: f32,
}

/// Where things are, in layout pixels.
struct Geometry {
    /// The box: x, y, width, height.
    frame: [f32; 4],
    /// Each entry's top and height, scrolled.
    rows: Vec<(f32, f32)>,
    pad: f32,
    /// The band the entries show in: top and bottom. The whole box, or
    /// the part between the scroll arrows.
    view: (f32, f32),
    /// When the menu is taller than the window: the scroll arrows' bands
    /// (top and bottom, each a top y and height) and the furthest it
    /// scrolls.
    arrows: Option<Arrows>,
    /// The scroll actually used: `Popup::scroll` kept in range.
    scroll: f32,
}

/// The scroll arrows of a menu taller than the window.
struct Arrows {
    up: (f32, f32),
    down: (f32, f32),
    max_scroll: f32,
    /// How far one press of an arrow scrolls: a few rows.
    step: f32,
}

/// The app's menus: whether one is open, and which.
#[derive(Debug, Default)]
pub struct Menus {
    open: Option<Popup>,
    /// The item a click chose, for the app to run once the click is
    /// handled.
    pub clicked: Option<&'static str>,
}

/// The height of the drawn menu bar for `line_height`: a line and a half.
#[must_use]
pub fn bar_height(line_height: f32) -> f32 {
    (line_height * 1.5).round()
}

/// Each bar title's left edge and width, in order.
fn bar_titles(layout: &Layout) -> impl Iterator<Item = (f32, f32)> + '_ {
    let pad = layout.metrics.cell_width;
    let mut x = (layout.margin / 2.0).round();
    super::LAYOUT.iter().map(move |(title, _)| {
        let w = (layout.ui.width(title) + 2.0 * pad).round();
        let at = (x, w);
        x += w;
        at
    })
}

/// Where the bar's last title ends: the window's own title and buttons
/// go to the right of it (`crate::caption`).
#[must_use]
pub fn titles_end(layout: &Layout) -> f32 {
    bar_titles(layout).last().map_or(0.0, |(x, w)| x + w)
}

impl Menus {
    /// Whether a menu is open (and so takes keys and clicks first).
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Close whatever menu is open.
    pub fn close(&mut self) {
        self.open = None;
    }

    /// Open bar menu `i` (wrapping), under its title, with its first item
    /// highlighted when `highlight` (opened from the keyboard).
    pub fn open_bar(&mut self, i: usize, layout: &Layout, keymap: &Keymap, highlight: bool) {
        let n = super::LAYOUT.len();
        let i = i % n;
        let Some((x, _)) = bar_titles(layout).nth(i) else {
            return;
        };
        let entries = entries(super::LAYOUT[i].1, keymap);
        let hover = highlight
            .then(|| entries.iter().position(Entry::is_item))
            .flatten();
        self.open = Some(Popup {
            entries,
            at: [x, 0.0],
            hover,
            bar: Some(i),
            scroll: 0.0,
        });
    }

    /// Open a right-click menu of `ids` with its corner at the pointer.
    pub fn open_context(&mut self, ids: &[&str], at: [f32; 2], keymap: &Keymap) {
        self.open = Some(Popup {
            entries: entries(ids, keymap),
            at,
            hover: None,
            bar: None,
            scroll: 0.0,
        });
    }

    /// Scroll the open menu by `dy` pixels (positive is down the list),
    /// when it is taller than the window. Returns whether it moved.
    pub fn scroll_by(&mut self, layout: &Layout, dy: f32) -> bool {
        let Some(popup) = self.open.as_mut() else {
            return false;
        };
        let geo = geometry(popup, layout);
        let Some(arrows) = geo.arrows else {
            return false;
        };
        let to = (geo.scroll + dy).clamp(0.0, arrows.max_scroll);
        let moved = (to - popup.scroll).abs() > f32::EPSILON;
        popup.scroll = to;
        moved
    }

    /// The bar menu that is open, if the open menu drops down from the bar.
    #[must_use]
    pub fn open_bar_menu(&self) -> Option<usize> {
        self.open.as_ref().and_then(|popup| popup.bar)
    }

    /// A key while a menu is open. Returns the action to run, if one was
    /// chosen (the menu is then closed).
    pub fn key(&mut self, key: Key, layout: &Layout, keymap: &Keymap) -> Option<&'static str> {
        let popup = self.open.as_mut()?;
        match key {
            Key::Up | Key::Down => {
                let n = popup.entries.len();
                let start = popup
                    .hover
                    .unwrap_or(if key == Key::Up { 0 } else { n - 1 });
                popup.hover = (1..=n)
                    .map(|k| {
                        if key == Key::Up {
                            (start + n * k - k) % n
                        } else {
                            (start + k) % n
                        }
                    })
                    .find(|&i| popup.entries[i].is_item());
                reveal(popup, layout);
                None
            }
            Key::Left | Key::Right => {
                if let Some(i) = popup.bar {
                    let n = super::LAYOUT.len();
                    let next = if key == Key::Left { i + n - 1 } else { i + 1 };
                    self.open_bar(next, layout, keymap, true);
                }
                None
            }
            Key::Press => {
                let chosen = popup.hover.and_then(|i| item_id(&popup.entries[i]));
                if chosen.is_some() {
                    self.open = None;
                }
                chosen
            }
            Key::Escape => {
                self.open = None;
                None
            }
            Key::Letter(c) => {
                let c = c.to_lowercase().next().unwrap_or(c);
                let chosen = popup.entries.iter().find_map(|e| match e {
                    Entry::Item { id, title, .. }
                        if title.chars().next().and_then(|t| t.to_lowercase().next())
                            == Some(c) =>
                    {
                        Some(*id)
                    }
                    _ => None,
                });
                if chosen.is_some() {
                    self.open = None;
                }
                chosen
            }
        }
    }

    /// The pointer moved to (`x`, `y`): highlight the item under it, or
    /// with a bar menu open, open the menu whose title it is over. Returns
    /// whether anything changed.
    pub fn hover(&mut self, layout: &Layout, (x, y): (f32, f32), keymap: &Keymap) -> bool {
        let Some(popup) = &self.open else {
            return false;
        };
        if popup.bar.is_some()
            && y < 0.0
            && let Some(i) = title_at(layout, x)
            && Some(i) != popup.bar
        {
            self.open_bar(i, layout, keymap, false);
            return true;
        }
        let geo = geometry(popup, layout);
        let hover = item_at(popup, &geo, x, y);
        let Some(popup) = self.open.as_mut() else {
            return false;
        };
        let changed = popup.hover != hover;
        popup.hover = hover;
        changed
    }

    /// A left click at (`x`, `y`) in layout pixels (negative y is the bar).
    pub fn click(&mut self, layout: &Layout, (x, y): (f32, f32), keymap: &Keymap) -> Click {
        let on_bar = DRAWS_MENU_BAR && y < 0.0;
        let Some(popup) = &self.open else {
            return match title_at(layout, x).filter(|_| on_bar) {
                Some(i) => {
                    self.open_bar(i, layout, keymap, false);
                    Click::Handled
                }
                None => Click::Ignored,
            };
        };
        if on_bar && let Some(i) = title_at(layout, x) {
            // A click on the open menu's title closes it, as Windows does.
            if popup.bar == Some(i) {
                self.open = None;
            } else {
                self.open_bar(i, layout, keymap, false);
            }
            return Click::Handled;
        }
        let geo = geometry(popup, layout);
        let [fx, fy, fw, fh] = geo.frame;
        if x < fx || x >= fx + fw || y < fy || y >= fy + fh {
            self.open = None;
            return Click::Closed;
        }
        if let Some(arrows) = &geo.arrows {
            let inside = |(top, h): (f32, f32)| y >= top && y < top + h;
            let dy = if inside(arrows.up) {
                -arrows.step
            } else if inside(arrows.down) {
                arrows.step
            } else {
                0.0
            };
            if dy != 0.0 {
                self.scroll_by(layout, dy);
                return Click::Handled;
            }
        }
        match item_at(popup, &geo, x, y).and_then(|i| item_id(&popup.entries[i])) {
            Some(id) => {
                self.open = None;
                Click::Chose(id)
            }
            None => Click::Handled,
        }
    }

    /// Draw the menu bar in the space kept above the layout, in the tab
    /// bar's colour so it runs on into the tabs below. Titles are dimmed
    /// while the window is in the background, and the open menu's title is
    /// highlighted.
    pub fn draw_bar(&self, gpu: &mut Gpu, ui: &Ui, layout: &Layout, active: bool) {
        let h = gpu.inset_top();
        if h <= 0.0 {
            return;
        }
        gpu.begin_layer(None);
        gpu.push_rect([0.0, -h], [layout.width, h], ui.tab_bar);
        let pad = layout.metrics.cell_width;
        let y = (-h + (h - layout.metrics.line_height) / 2.0).round();
        let open = self.open_bar_menu();
        for (i, ((title, _), (x, w))) in super::LAYOUT.iter().zip(bar_titles(layout)).enumerate() {
            if open == Some(i) {
                gpu.push_rect([x, -h], [w, h], ui.picker_selected);
            }
            let color = if active { ui.text } else { ui.gutter };
            gpu.push_ui_text([x + pad, y], title, color);
        }
    }

    /// Draw the open menu, if any, over everything drawn so far.
    pub fn draw_popup(&self, gpu: &mut Gpu, ui: &Ui, layout: &Layout) {
        let Some(popup) = &self.open else {
            return;
        };
        let geo = geometry(popup, layout);
        let [x, y, w, h] = geo.frame;
        let line = layout.scale.round().max(1.0);
        let clip = gpu.clip();
        gpu.begin_layer(Some(geo.frame));
        gpu.push_rect([x, y], [w, h], ui.picker_border);
        gpu.push_rect(
            [x + line, y + line],
            [w - 2.0 * line, h - 2.0 * line],
            ui.picker,
        );
        let lh = layout.metrics.line_height;
        let keys_right = x + w - geo.pad;
        if let Some(arrows) = &geo.arrows {
            // ▲ and ▼ in their bands, dimmed at the end they cannot go past.
            for ((top, ah), arrow, live) in [
                (arrows.up, "▲", geo.scroll > 0.0),
                (arrows.down, "▼", geo.scroll < arrows.max_scroll),
            ] {
                let aw = layout.ui.width(arrow);
                let at = [
                    (x + (w - aw) / 2.0).round(),
                    (top + (ah - lh) / 2.0).round(),
                ];
                gpu.push_ui_text(at, arrow, if live { ui.text } else { ui.gutter });
            }
        }
        let (view_top, view_bottom) = geo.view;
        gpu.begin_layer(Some([x, view_top, w, view_bottom - view_top]));
        for (i, (entry, &(top, rh))) in popup.entries.iter().zip(&geo.rows).enumerate() {
            if top + rh <= view_top || top >= view_bottom {
                continue;
            }
            match entry {
                Entry::Separator => {
                    let mid = (top + rh / 2.0).round();
                    gpu.push_rect(
                        [x + geo.pad, mid],
                        [w - 2.0 * geo.pad, line],
                        ui.picker_border,
                    );
                }
                Entry::Item { title, keys, .. } => {
                    if popup.hover == Some(i) {
                        gpu.push_rect([x + line, top], [w - 2.0 * line, rh], ui.picker_selected);
                    }
                    let ty = (top + (rh - lh) / 2.0).round();
                    gpu.push_ui_text([x + geo.pad, ty], title, ui.text);
                    if !keys.is_empty() {
                        let kx = (keys_right - layout.ui.width(keys)).round();
                        gpu.push_ui_text([kx, ty], keys, ui.gutter);
                    }
                }
            }
        }
        gpu.begin_layer(clip);
    }
}

/// The action id of an item entry.
fn item_id(entry: &Entry) -> Option<&'static str> {
    match entry {
        Entry::Item { id, .. } => Some(*id),
        Entry::Separator => None,
    }
}

/// The bar title at `x`, if any.
fn title_at(layout: &Layout, x: f32) -> Option<usize> {
    bar_titles(layout).position(|(left, w)| x >= left && x < left + w)
}

/// The item (not separator) under (`x`, `y`), if any.
fn item_at(popup: &Popup, geo: &Geometry, x: f32, y: f32) -> Option<usize> {
    let [fx, _, fw, _] = geo.frame;
    if x < fx || x >= fx + fw || y < geo.view.0 || y >= geo.view.1 {
        return None;
    }
    geo.rows
        .iter()
        .position(|&(top, h)| y >= top && y < top + h)
        .filter(|&i| popup.entries[i].is_item())
}

/// Lay out `popup`: rows a line and a half tall, separators under a line,
/// wide enough for the longest title and shortcut with three cells
/// between, and kept inside the window.
fn geometry(popup: &Popup, layout: &Layout) -> Geometry {
    let cell = layout.metrics.cell_width;
    let lh = layout.metrics.line_height;
    let pad = (cell * 1.5).round();
    let row_h = (lh * 1.5).round();
    let sep_h = (lh * 0.6).round();
    let inner = (lh * 0.25).round();
    let widest = popup
        .entries
        .iter()
        .map(|e| match e {
            Entry::Item { title, keys, .. } => {
                let gap = if keys.is_empty() { 0.0 } else { 3.0 * cell };
                layout.ui.width(title) + gap + layout.ui.width(keys)
            }
            Entry::Separator => 0.0,
        })
        .fold(0.0, f32::max);
    let w = (widest + 2.0 * pad)
        .max(16.0 * cell)
        .min(layout.width)
        .round();
    let h: f32 = popup
        .entries
        .iter()
        .map(|e| if e.is_item() { row_h } else { sep_h })
        .sum::<f32>()
        + 2.0 * inner;
    // Keep it in the window: shift left at the right edge, and up at the
    // bottom (a right-click low in the window opens upward instead). A
    // menu taller than the window fills it and scrolls.
    let x = popup.at[0].min(layout.width - w).max(0.0).round();
    let mut y = popup.at[1];
    if y + h > layout.height {
        y = if popup.bar.is_none() && popup.at[1] - h >= 0.0 {
            popup.at[1] - h
        } else if popup.bar.is_some() {
            y
        } else {
            (layout.height - h).max(0.0)
        };
    }
    let y = y.round();
    let shown = h.min(layout.height - y).max(0.0);
    let (arrows, view, scroll) = if h > shown {
        let ah = lh.round();
        let up = (y + 1.0, ah);
        let down = (y + shown - ah - 1.0, ah);
        let view = (up.0 + ah, down.0);
        // The rows alone, without the box's inner padding.
        let max_scroll = (h - 2.0 * inner - (view.1 - view.0)).max(0.0);
        let arrows = Arrows {
            up,
            down,
            max_scroll,
            step: 3.0 * row_h,
        };
        (Some(arrows), view, popup.scroll.clamp(0.0, max_scroll))
    } else {
        (None, (y, y + shown), 0.0)
    };
    // With arrows, the entries start under the top one (`view.0`), less
    // the inner padding the box keeps above the first row.
    let mut top = if arrows.is_some() { view.0 } else { y + inner } - scroll;
    let rows = popup
        .entries
        .iter()
        .map(|e| {
            let rh = if e.is_item() { row_h } else { sep_h };
            let row = (top, rh);
            top += rh;
            row
        })
        .collect();
    Geometry {
        frame: [x, y, w, shown],
        rows,
        pad,
        view,
        arrows,
        scroll,
    }
}

/// Scroll `popup` just enough to show its highlighted entry.
fn reveal(popup: &mut Popup, layout: &Layout) {
    let geo = geometry(popup, layout);
    let (Some(arrows), Some(i)) = (&geo.arrows, popup.hover) else {
        return;
    };
    let Some(&(top, h)) = geo.rows.get(i) else {
        return;
    };
    let (view_top, view_bottom) = geo.view;
    let dy = if top < view_top {
        top - view_top
    } else if top + h > view_bottom {
        top + h - view_bottom
    } else {
        return;
    };
    popup.scroll = (geo.scroll + dy).clamp(0.0, arrows.max_scroll);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        let metrics = nimbus_render::Metrics {
            size_px: 14.0,
            line_height: 20.0,
            baseline: 15.0,
            cell_width: 10.0,
        };
        Layout::new(metrics, 1000, 600, 1.0, 10)
    }

    fn keymap() -> Keymap {
        Keymap::new(&std::collections::HashMap::new()).0
    }

    #[test]
    fn arrows_skip_separators_and_wrap() {
        let (layout, keymap) = (layout(), keymap());
        let mut menus = Menus::default();
        menus.open_context(&["tree.newFile", "-", "tree.rename"], [10.0, 10.0], &keymap);
        assert_eq!(menus.key(Key::Down, &layout, &keymap), None);
        assert_eq!(menus.open.as_ref().unwrap().hover, Some(0));
        menus.key(Key::Down, &layout, &keymap);
        assert_eq!(menus.open.as_ref().unwrap().hover, Some(2));
        menus.key(Key::Down, &layout, &keymap);
        assert_eq!(menus.open.as_ref().unwrap().hover, Some(0));
        menus.key(Key::Up, &layout, &keymap);
        assert_eq!(menus.open.as_ref().unwrap().hover, Some(2));
        assert_eq!(menus.key(Key::Press, &layout, &keymap), Some("tree.rename"));
        assert!(!menus.is_open());
    }

    #[test]
    fn left_and_right_walk_the_bar() {
        let (layout, keymap) = (layout(), keymap());
        let mut menus = Menus::default();
        menus.open_bar(0, &layout, &keymap, true);
        menus.key(Key::Left, &layout, &keymap);
        assert_eq!(menus.open_bar_menu(), Some(super::super::LAYOUT.len() - 1));
        menus.key(Key::Right, &layout, &keymap);
        assert_eq!(menus.open_bar_menu(), Some(0));
        assert_eq!(menus.open.as_ref().unwrap().hover, Some(0));
        assert_eq!(menus.key(Key::Escape, &layout, &keymap), None);
        assert!(!menus.is_open());
    }

    #[test]
    fn clicks_choose_items_and_close_outside() {
        let (layout, keymap) = (layout(), keymap());
        let mut menus = Menus::default();
        menus.open_context(&["tree.newFile", "-", "tree.rename"], [10.0, 10.0], &keymap);
        let geo = geometry(menus.open.as_ref().unwrap(), &layout);
        let (top, h) = geo.rows[2];
        let x = geo.frame[0] + 5.0;
        // The separator chooses nothing and keeps the menu open.
        let (sep_top, _) = geo.rows[1];
        assert_eq!(
            menus.click(&layout, (x, sep_top + 1.0), &keymap),
            Click::Handled
        );
        assert_eq!(
            menus.click(&layout, (x, top + h / 2.0), &keymap),
            Click::Chose("tree.rename")
        );
        menus.open_context(&["tree.newFile"], [10.0, 10.0], &keymap);
        assert_eq!(menus.click(&layout, (900.0, 500.0), &keymap), Click::Closed);
        assert!(!menus.is_open());
    }

    #[test]
    fn menus_stay_inside_the_window() {
        let (layout, keymap) = (layout(), keymap());
        let mut menus = Menus::default();
        menus.open_context(
            &["tree.newFile", "tree.rename"],
            [layout.width - 5.0, layout.height - 5.0],
            &keymap,
        );
        let [x, y, w, h] = geometry(menus.open.as_ref().unwrap(), &layout).frame;
        assert!(x >= 0.0 && x + w <= layout.width);
        assert!(y >= 0.0 && y + h <= layout.height);
    }

    #[test]
    fn a_menu_taller_than_the_window_scrolls() {
        let metrics = nimbus_render::Metrics {
            size_px: 14.0,
            line_height: 20.0,
            baseline: 15.0,
            cell_width: 10.0,
        };
        let layout = Layout::new(metrics, 1000, 200, 1.0, 10);
        let keymap = keymap();
        let mut menus = Menus::default();
        menus.open_bar(1, &layout, &keymap, true);
        let geo = geometry(menus.open.as_ref().unwrap(), &layout);
        let [_, y, _, h] = geo.frame;
        assert!(y + h <= layout.height);
        let arrows = geo.arrows.as_ref().expect("scroll arrows");
        assert!(arrows.max_scroll > 0.0);
        // Down to the last item: it is scrolled into view.
        let last = menus.open.as_ref().unwrap().entries.len() - 1;
        for _ in 0..200 {
            menus.key(Key::Down, &layout, &keymap);
            if menus.open.as_ref().unwrap().hover == Some(last) {
                break;
            }
        }
        let geo = geometry(menus.open.as_ref().unwrap(), &layout);
        let (top, rh) = geo.rows[last];
        assert!(top >= geo.view.0 && top + rh <= geo.view.1 + 0.5);
        // The wheel goes back to the top and no further.
        assert!(menus.scroll_by(&layout, -10_000.0));
        assert!(!menus.scroll_by(&layout, -10.0));
        // A click on the down arrow scrolls; it chooses nothing.
        let geo = geometry(menus.open.as_ref().unwrap(), &layout);
        let (down, dh) = geo.arrows.as_ref().unwrap().down;
        let x = geo.frame[0] + 5.0;
        assert_eq!(
            menus.click(&layout, (x, down + dh / 2.0), &keymap),
            Click::Handled
        );
        assert!(menus.open.as_ref().unwrap().scroll > 0.0);
    }

    #[test]
    fn letters_choose_by_first_letter() {
        let (layout, keymap) = (layout(), keymap());
        let mut menus = Menus::default();
        menus.open_context(&["tree.newFile", "tree.rename"], [0.0, 0.0], &keymap);
        assert_eq!(
            menus.key(Key::Letter('R'), &layout, &keymap),
            Some("tree.rename")
        );
    }
}
