//! The app's side of the accessibility tree (M22): the adapter, the scene
//! it describes, and what a screen reader asks the app to do.
//!
//! `accesskit_winit::Adapter` connects our tree to the platform. It must
//! exist before the window is first shown (see [`open_window`]), and it
//! stays inactive until a screen reader asks for the tree. Each frame
//! calls [`App::update_access`], which hands the adapter a closure that
//! builds the update; while no reader is running the adapter never calls
//! it, so nobody else pays for any of this.
//!
//! A reader's requests (focus this tab, put the caret here, open this
//! folder) arrive as [`AppEvent::Access`] events and go to
//! [`App::access_event`], which runs the same code the mouse and keyboard
//! do.

use accesskit::{Action, ActionData, ActionRequest, NodeId, TreeUpdate};
use accesskit_winit::{Adapter, WindowEvent as AccessEvent};
use nimbus_core::Text;
use nimbus_core::wrap::{self, WrapCache, WrapView};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::{Window, WindowAttributes};

use super::{App, AppEvent};
use crate::access::text::{Frame, Geometry, TextWindow};
use crate::access::{
    self, Field, Find, Menu, MenuBar, MenuEntry, Picker, Prompt, Scene, Sent, Sidebar, Tab,
    TreeRow, id,
};
use crate::docs::DocId;
use crate::document::{Editor, Solo};
use crate::find::Field as FindField;
use crate::input::Command;
use crate::menu::drawn::{self, DRAWS_MENU_BAR, Entry};
use crate::search_panel::Focus as SearchFocus;
use crate::sidebar::Mode;
use crate::view::{self, Layout};

/// Tree rows described either side of the selected one.
const TREE_WINDOW: usize = 200;

/// The adapter and what was last sent through it.
#[derive(Debug)]
pub struct Access {
    adapter: Option<AdapterBox>,
    /// The next update must describe the whole tree: nothing has been
    /// sent since a reader (re)started, or the platform asked again.
    need_full: bool,
    sent: Sent,
    editor: TextWindow,
    /// What the editor's window was built for: the tab, its document and
    /// the document's epoch; when these change the window starts over.
    editor_key: Option<(usize, DocId, u64)>,
    /// The editor document's journal end the window last followed.
    synced: u64,
    find: TextWindow,
    replace: TextWindow,
    picker: TextWindow,
    search: [TextWindow; 4],
    /// Row starts for the one-line fields (never wrapped).
    field_rows: WrapCache,
}

/// The adapter, which is not `Debug`.
struct AdapterBox(Adapter);

impl std::fmt::Debug for AdapterBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Adapter")
    }
}

impl Default for Access {
    fn default() -> Self {
        Self {
            adapter: None,
            need_full: true,
            sent: Sent::default(),
            editor: TextWindow::new(id::field::EDITOR),
            editor_key: None,
            synced: 0,
            find: TextWindow::new(id::field::FIND),
            replace: TextWindow::new(id::field::REPLACE),
            picker: TextWindow::new(id::field::PICKER),
            search: [
                TextWindow::new(id::field::SEARCH),
                TextWindow::new(id::field::SEARCH_REPLACE),
                TextWindow::new(id::field::SEARCH_INCLUDE),
                TextWindow::new(id::field::SEARCH_EXCLUDE),
            ],
            field_rows: WrapCache::default(),
        }
    }
}

impl Access {
    /// Forget everything sent: the next update is a full tree.
    fn start_over(&mut self) {
        self.need_full = true;
        self.sent.clear();
        self.editor.reset();
        self.editor_key = None;
        for w in [&mut self.find, &mut self.replace, &mut self.picker] {
            w.reset();
        }
        for w in &mut self.search {
            w.reset();
        }
    }

    /// The runs of the text field `node`.
    fn runs(&self, node: NodeId) -> &[NodeId] {
        match node {
            id::EDITOR => self.editor.children(),
            id::FIND_FIELD => self.find.children(),
            id::REPLACE_FIELD => self.replace.children(),
            id::PICKER_FIELD => self.picker.children(),
            id::SEARCH_FIELD => self.search[0].children(),
            id::SEARCH_REPLACE => self.search[1].children(),
            id::SEARCH_INCLUDE => self.search[2].children(),
            id::SEARCH_EXCLUDE => self.search[3].children(),
            _ => &[],
        }
    }

    /// The text window describing field `field` (see [`id::field`]).
    fn window(&self, field: u8) -> Option<&TextWindow> {
        Some(match field {
            id::field::EDITOR => &self.editor,
            id::field::FIND => &self.find,
            id::field::REPLACE => &self.replace,
            id::field::PICKER => &self.picker,
            id::field::SEARCH => &self.search[0],
            id::field::SEARCH_REPLACE => &self.search[1],
            id::field::SEARCH_INCLUDE => &self.search[2],
            id::field::SEARCH_EXCLUDE => &self.search[3],
            _ => return None,
        })
    }
}

/// The window's attributes as given, but hidden: the adapter must be
/// created before the window is first shown.
pub fn hidden(attrs: WindowAttributes) -> WindowAttributes {
    attrs.with_visible(false)
}

/// Create the window hidden, attach the accessibility adapter, then show
/// it. The order matters: `accesskit_winit` panics if the window was ever
/// visible before the adapter existed (it has to replace the view's
/// accessibility methods first), and a release build aborts on a panic.
/// Generic over the steps so a test can check the order without a window.
pub fn open_window<W, E>(
    attrs: WindowAttributes,
    create: impl FnOnce(WindowAttributes) -> Result<W, E>,
    attach: impl FnOnce(&W),
    show: impl FnOnce(&W),
) -> Result<W, E> {
    let window = create(hidden(attrs))?;
    attach(&window);
    show(&window);
    Ok(window)
}

impl Access {
    /// Attach the adapter to `window`, which must not have been shown.
    /// Requests from screen readers come back through `proxy`.
    pub fn attach(
        &mut self,
        event_loop: &ActiveEventLoop,
        window: &Window,
        proxy: EventLoopProxy<AppEvent>,
    ) {
        if window.is_visible() == Some(true) {
            // Never on the platforms we ship (they report hidden windows),
            // but the adapter would panic: go without rather than abort.
            log::warn!("window already visible; screen reader support is off");
            return;
        }
        let adapter = Adapter::with_event_loop_proxy(event_loop, window, proxy);
        self.adapter = Some(AdapterBox(adapter));
    }

    /// Let the adapter see a window event before the app handles it (it
    /// tracks focus and, on some platforms, the window's position).
    pub fn window_event(&mut self, window: &Window, event: &winit::event::WindowEvent) {
        if let Some(a) = self.adapter.as_mut() {
            a.0.process_event(window, event);
        }
    }
}

impl From<accesskit_winit::Event> for AppEvent {
    fn from(event: accesskit_winit::Event) -> Self {
        AppEvent::Access(event.window_event)
    }
}

/// The grid font's geometry for runs.
fn geometry(layout: &Layout, tab_width: usize) -> Geometry {
    Geometry {
        cell: layout.metrics.cell_width,
        line_height: layout.metrics.line_height,
        tab_width,
    }
}

/// Describe a one-line field: its runs into `out`, the field itself back.
fn solo_field(
    win: &mut TextWindow,
    rows: &mut WrapCache,
    solo: &Solo,
    (label, placeholder): (&str, Option<&str>),
    bounds: [f32; 4],
    g: Geometry,
    out: &mut Vec<(NodeId, accesskit::Node)>,
) -> Field {
    let t = &solo.doc.buffer;
    let mut w = WrapView::new(rows, None, g.tab_width);
    let sel = solo.carets.selection(t);
    let frame = Frame {
        top: None,
        geometry: g,
        long_scroll: 0.0,
    };
    let placed = win.update(t, &mut w, sel, frame, out);
    Field {
        label: label.to_owned(),
        placeholder: placeholder.map(str::to_owned),
        bounds,
        origin: [f64::from(bounds[0]), f64::from(bounds[1])],
        selection: placed.selection,
        read_only: false,
    }
}

impl App {
    /// Send the screen reader what changed since the last update. Costs
    /// nothing while no reader is running: the adapter skips the closure.
    pub(super) fn update_access(&mut self) {
        let Some(mut adapter) = self.access.adapter.take() else {
            return;
        };
        adapter.0.update_if_active(|| self.access_tree());
        self.access.adapter = Some(adapter);
    }

    /// A request from the adapter: build the tree, act for a reader, or
    /// stop.
    pub(super) fn access_event(&mut self, event: AccessEvent, event_loop: &ActiveEventLoop) {
        match event {
            AccessEvent::InitialTreeRequested => {
                self.access.start_over();
                self.update_access();
            }
            AccessEvent::AccessibilityDeactivated => self.access.start_over(),
            AccessEvent::ActionRequested(req) => self.access_action(req, event_loop),
        }
    }

    /// The tree update: everything on a full one, else what changed.
    fn access_tree(&mut self) -> TreeUpdate {
        let full = std::mem::replace(&mut self.access.need_full, false);
        if full {
            self.access.start_over();
            self.access.need_full = false;
        }
        let mut nodes = Vec::new();
        let scene = self.access_scene(&mut nodes);
        let built = {
            let a = &self.access;
            access::build(&scene, &|n| a.runs(n))
        };
        self.access.sent.diff(built, &mut nodes);
        access::update(nodes, scene.focus, full)
    }

    /// Gather what the tree shows, adding changed text runs to `out`.
    fn access_scene(&mut self, out: &mut Vec<(NodeId, accesskit::Node)>) -> Scene {
        let window = self.window_layout();
        let layout = self.layout().or(window);
        let (width, height) = window.map_or((0.0, 0.0), |l| (l.width, l.height));
        let tab_width = self.tab_width();
        let g = layout.map_or(
            Geometry {
                cell: 1.0,
                line_height: 1.0,
                tab_width,
            },
            |l| geometry(&l, tab_width),
        );

        // Tabs of the focused group.
        let (labels, active_tab) = self.tab_labels(self.groups.focused);
        let extents = layout.map(|l| view::tab_extents(&l, &labels, active_tab));
        let tabs_h = layout.map_or(0.0, |l| l.tabs_h);
        let tabs = labels
            .iter()
            .enumerate()
            .map(|(n, label)| {
                let (x0, x1) = extents
                    .as_ref()
                    .and_then(|e| e.get(n).copied())
                    .unwrap_or((0.0, 0.0));
                Tab {
                    name: label.name.clone(),
                    dirty: label.dirty,
                    bounds: [x0, 0.0, x1 - x0, tabs_h],
                }
            })
            .collect();
        let tab_bar = layout.map_or([0.0; 4], |l| [l.left, 0.0, l.width - l.left, l.tabs_h]);

        let editor = self.access_editor(layout, g, out);
        let status = self.access_status(window);
        let message = self.message.as_ref().map(|m| m.text.clone());
        let find = self.access_find(layout, g, out);
        let picker = self.access_picker(window, g, out);
        let sidebar = self.access_sidebar(window, g, out);
        let inset = self.gpu.as_ref().map_or(0.0, nimbus_render::Gpu::inset_top);
        let (menu_bar, menu) = self.access_menus(window, inset);
        let prompt = self.access_prompt(window);

        // A prompt and an open menu take every key, so they have the
        // keyboard before anything else.
        let focus = if let Some(p) = &prompt {
            id::button(p.focused)
        } else if let Some(m) = &menu {
            m.hover.map_or(id::MENU, id::menu_item)
        } else if picker.is_some() {
            id::PICKER_FIELD
        } else if self.sidebar.focused && sidebar.is_some() {
            self.sidebar_focus(sidebar.as_ref())
        } else if self.find.focused && find.is_some() {
            if self.find.focus == FindField::Replace && self.find.replace_mode {
                id::REPLACE_FIELD
            } else {
                id::FIND_FIELD
            }
        } else {
            id::EDITOR
        };
        let dirty = if self.doc().is_dirty() { "● " } else { "" };
        Scene {
            title: format!("{dirty}{} — Nimbus", self.doc().display_name()),
            size: [width, height],
            inset,
            menu_bar,
            menu,
            prompt,
            tabs,
            active_tab,
            tab_bar,
            editor,
            status,
            message,
            find,
            sidebar,
            picker,
            focus,
        }
    }

    /// The drawn menu bar (Windows) and the open drawn menu.
    fn access_menus(&self, window: Option<Layout>, inset: f32) -> (Option<MenuBar>, Option<Menu>) {
        let Some(layout) = window else {
            return (None, None);
        };
        let open_bar = self.menus.open_bar_menu();
        let bar = (DRAWS_MENU_BAR && inset > 0.0).then(|| MenuBar {
            titles: drawn::bar_items(&layout, inset)
                .into_iter()
                .map(|(t, b)| (t.to_owned(), b))
                .collect(),
            open: open_bar,
            bounds: [0.0, -inset, layout.width, inset],
        });
        let menu = self.menus.describe(&layout).map(|d| Menu {
            label: d
                .bar
                .and_then(|i| crate::menu::LAYOUT.get(i))
                .map_or_else(|| "Context menu".to_owned(), |(t, _)| (*t).to_owned()),
            entries: d
                .entries
                .iter()
                .map(|(e, bounds)| match e {
                    Entry::Item { title, keys, .. } => {
                        MenuEntry::Item((*title).to_owned(), keys.clone(), *bounds)
                    }
                    Entry::Separator => MenuEntry::Separator,
                })
                .collect(),
            hover: d.hover,
            bounds: d.frame,
        });
        (bar, menu)
    }

    /// The front prompt, when one is open.
    fn access_prompt(&self, window: Option<Layout>) -> Option<Prompt> {
        let (p, layout) = (self.prompts.front()?, window?);
        let (bounds, rects) = p.rects(&layout);
        let default = p
            .buttons
            .iter()
            .position(|b| b.answer == crate::prompt::Answer::Yes);
        Some(Prompt {
            title: p.title.clone(),
            text: p.text.clone(),
            buttons: p
                .buttons
                .iter()
                .zip(rects)
                .enumerate()
                .map(|(i, (b, r))| (b.label.to_owned(), r, Some(i) == default))
                .collect(),
            focused: p.focused,
            bounds,
        })
    }

    /// The node with the keyboard while the sidebar has it.
    fn sidebar_focus(&self, sidebar: Option<&(Sidebar, [f32; 4])>) -> NodeId {
        match sidebar {
            Some((Sidebar::Files { rows, .. }, _)) => rows
                .iter()
                .find(|r| r.selected)
                .map_or(id::TREE, |r| id::tree_item(r.node)),
            Some((Sidebar::Search { fields, .. }, _)) => {
                let want = match self.sidebar.search.focus {
                    SearchFocus::Query => id::SEARCH_FIELD,
                    SearchFocus::Replace => id::SEARCH_REPLACE,
                    SearchFocus::Include => id::SEARCH_INCLUDE,
                    SearchFocus::Exclude => id::SEARCH_EXCLUDE,
                    SearchFocus::List => id::SEARCH_COUNT,
                };
                if want == id::SEARCH_COUNT || fields.iter().any(|(f, _)| *f == want) {
                    want
                } else {
                    id::SIDEBAR
                }
            }
            _ => id::SIDEBAR,
        }
    }

    /// The editor's text field: a window of the active document's lines.
    fn access_editor(
        &mut self,
        layout: Option<Layout>,
        geom: Geometry,
        out: &mut Vec<(NodeId, accesskit::Node)>,
    ) -> Field {
        let index = self.active;
        let tab = &mut self.tabs[index];
        let doc = &self.docs[tab.doc];
        let access = &mut self.access;
        // A different tab or a text swapped wholesale starts the window
        // over; edits move its start along with the text.
        let key = (index, tab.doc, doc.epoch);
        let end = doc.journal().end();
        if access.editor_key == Some(key) {
            match doc.journal().since(access.synced) {
                Some(shifts) => access
                    .editor
                    .follow(|pos| shifts.iter().fold(pos, |pos, shift| shift.map(pos))),
                None => access.editor.reset(),
            }
        } else {
            access.editor.reset();
            access.editor_key = Some(key);
        }
        access.synced = end;
        let text = &doc.buffer;
        let sel = tab.carets.selections.primary();
        let label = doc.display_name();
        let read_only = doc.read_only.is_some();
        let Some(layout) = layout else {
            return Field {
                label,
                placeholder: None,
                bounds: [0.0; 4],
                origin: [0.0; 2],
                selection: None,
                read_only,
            };
        };
        let view = &mut tab.view;
        let (wrapping, sideways, partial_row) = (view.wrap, view.scroll_x, view.scroll_px);
        let scroll_top = view.scroll_top.min(text.len_bytes());
        let mut rows = view.wrap_view(&layout, geom.tab_width);
        let top = wrap::row_pos_of(text, &mut rows, scroll_top);
        // Long lines scroll sideways even while wrapping; the field's own
        // origin scrolls only when not wrapping.
        let frame = Frame {
            top: Some(top),
            geometry: geom,
            long_scroll: if wrapping { sideways } else { 0.0 },
        };
        let placed = access.editor.update(text, &mut rows, sel, frame, out);
        let line_height = f64::from(geom.line_height);
        let top_y = f64::from(layout.top - partial_row);
        let first_y = match placed.top_row {
            Some(row) => top_y - f64_of(row) * line_height,
            // The screen shows text outside the window (scrolled away
            // from the cursor): put the window just off screen.
            None if sel.head < scroll_top => top_y - f64_of(placed.rows) * line_height,
            None => f64::from(layout.bottom),
        };
        let x0 = f64::from(layout.text_left) - if wrapping { 0.0 } else { sideways };
        Field {
            label,
            placeholder: None,
            bounds: [
                layout.text_left,
                layout.top,
                layout.text_right() - layout.text_left,
                layout.bottom - layout.top,
            ],
            origin: [x0, first_y],
            selection: placed.selection,
            read_only,
        }
    }

    /// The status bar's details.
    fn access_status(&mut self, window: Option<Layout>) -> Option<(String, [f32; 4])> {
        let layout = window?;
        let tab_width = self.tab_width();
        let i = self.active;
        let tab = &mut self.tabs[i];
        let info = tab
            .view
            .status_info(&self.docs[tab.doc], &tab.carets, tab_width)
            .to_owned();
        Some((
            info,
            [
                0.0,
                layout.bottom,
                layout.width,
                layout.height - layout.bottom,
            ],
        ))
    }

    /// The find bar, when open.
    fn access_find(
        &mut self,
        layout: Option<Layout>,
        g: Geometry,
        out: &mut Vec<(NodeId, accesskit::Node)>,
    ) -> Option<Find> {
        let layout = layout?;
        if !self.find.open {
            self.access.find.reset();
            self.access.replace.reset();
            return None;
        }
        let count = self.find.status();
        let a = &mut self.access;
        let find = solo_field(
            &mut a.find,
            &mut a.field_rows,
            &self.find.find,
            ("Find", None),
            view::find_field_rect(&layout, &count, 0),
            g,
            out,
        );
        let replace = if self.find.replace_mode {
            Some(solo_field(
                &mut a.replace,
                &mut a.field_rows,
                &self.find.replace,
                ("Replace", None),
                view::find_field_rect(&layout, &count, 1),
                g,
                out,
            ))
        } else {
            a.replace.reset();
            None
        };
        Some(Find {
            pattern: find,
            replace,
            count,
            bounds: [
                layout.left,
                layout.tabs_h,
                layout.width - layout.left,
                layout.top - layout.margin - layout.tabs_h,
            ],
        })
    }

    /// The palette or a picker, when open.
    fn access_picker(
        &mut self,
        window: Option<Layout>,
        g: Geometry,
        out: &mut Vec<(NodeId, accesskit::Node)>,
    ) -> Option<Picker> {
        let (Some(p), Some(layout)) = (self.picker.as_ref(), window) else {
            self.access.picker.reset();
            return None;
        };
        let (rows, selected) = p.rows(&self.keymap);
        let options = rows
            .into_iter()
            .enumerate()
            .map(|(r, row)| {
                let (_, bounds) = view::picker_rects(&layout, r);
                (p.scroll + r, row.text, row.detail, bounds)
            })
            .collect();
        let label = super::picker_title(p.kind);
        let (field_rect, _) = view::picker_rects(&layout, 0);
        let a = &mut self.access;
        let field = solo_field(
            &mut a.picker,
            &mut a.field_rows,
            &p.input,
            (label, Some(super::picker_placeholder(p.kind))),
            field_rect,
            g,
            out,
        );
        Some(Picker {
            label: label.to_owned(),
            field,
            options,
            selected: selected.map(|s| p.scroll + s),
            total: p.match_count(),
            status: super::picker_status(p),
        })
    }

    /// The sidebar, when shown.
    fn access_sidebar(
        &mut self,
        window: Option<Layout>,
        geom: Geometry,
        out: &mut Vec<(NodeId, accesskit::Node)>,
    ) -> Option<(Sidebar, [f32; 4])> {
        let layout = window?;
        if !self.sidebar.visible || layout.left <= 0.0 {
            for w in &mut self.access.search {
                w.reset();
            }
            return None;
        }
        let bounds = [0.0, 0.0, layout.left, layout.bottom];
        let sidebar = match self.sidebar.mode {
            Mode::Files => self.access_files(&layout),
            Mode::Search => self.access_search(&layout, geom, out),
            Mode::Compare => Sidebar::Compare,
        };
        Some((sidebar, bounds))
    }

    /// The file tree: the rows within [`TREE_WINDOW`] of the selected one
    /// (or of the top of the list when nothing is selected).
    fn access_files(&self, layout: &Layout) -> Sidebar {
        let tree = &self.sidebar.tree;
        let all = tree.rows();
        let centre = tree
            .selected
            .and_then(|s| tree.row_of(s))
            .unwrap_or(self.sidebar.scroll);
        let from = centre.saturating_sub(TREE_WINDOW);
        let to = (centre + TREE_WINDOW + 1).min(all.len());
        let rows = all
            .get(from..to)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .filter_map(|(k, row)| {
                let node = tree.node(row.node)?;
                Some(TreeRow {
                    node: row.node,
                    name: node.name.clone(),
                    depth: row.depth,
                    expanded: node.is_dir.then_some(node.expanded),
                    selected: tree.selected == Some(row.node),
                    bounds: self.sidebar.row_rect(layout, from + k),
                })
            })
            .collect();
        let name = match &self.sidebar.workspace {
            Some(name) => name.clone(),
            None => tree
                .node(crate::tree::ROOT)
                .map_or_else(|| "Files".to_owned(), |n| n.name.clone()),
        };
        Sidebar::Files { name, rows }
    }

    /// The Search panel's fields and its count of results.
    fn access_search(
        &mut self,
        layout: &Layout,
        geom: Geometry,
        out: &mut Vec<(NodeId, accesskit::Node)>,
    ) -> Sidebar {
        let panel = &self.sidebar.search;
        let access = &mut self.access;
        let [search, replace, include, exclude] = &mut access.search;
        let rows = &mut access.field_rows;
        // Approximate rows down the panel: the fields' order and size.
        let row = |n: f32| {
            let lh = layout.metrics.line_height;
            let y = layout.tabs_h + layout.margin + n * (lh + layout.margin);
            [layout.margin, y, layout.left - 2.0 * layout.margin, lh]
        };
        let mut fields = Vec::with_capacity(4);
        let mut add = |node, win: &mut TextWindow, solo, label, n| {
            let field = solo_field(win, rows, solo, (label, None), row(n), geom, out);
            fields.push((node, field));
        };
        add(id::SEARCH_FIELD, search, &panel.query, "Search", 0.0);
        if panel.replace_mode {
            add(id::SEARCH_REPLACE, replace, &panel.replace, "Replace", 1.0);
        } else {
            replace.reset();
        }
        add(
            id::SEARCH_INCLUDE,
            include,
            &panel.include,
            "Files to include",
            2.0,
        );
        add(
            id::SEARCH_EXCLUDE,
            exclude,
            &panel.exclude,
            "Files to leave out",
            3.0,
        );
        Sidebar::Search {
            fields,
            status: panel.status(),
        }
    }

    /// Act on a screen reader's request with the code the mouse and
    /// keyboard use.
    fn access_action(&mut self, req: ActionRequest, event_loop: &ActiveEventLoop) {
        log::debug!(
            "screen reader asks: {:?} on {:?}",
            req.action,
            req.target_node
        );
        // While a prompt is open it alone answers, as it takes every key
        // and click.
        if !self.prompts.is_empty() {
            if let id::Target::Button(n) = id::target(req.target_node) {
                self.access_button(n, req.action, event_loop);
            }
            return;
        }
        match id::target(req.target_node) {
            id::Target::MenuTitle(n) => self.access_menu_title(n, req.action),
            id::Target::MenuItem(n) => self.access_menu_item(n, req.action, event_loop),
            id::Target::Button(_) => {}
            id::Target::Tab(n) => {
                if matches!(req.action, Action::Click | Action::Focus)
                    && let Some(&i) = self.group_tabs(self.groups.focused).get(n)
                {
                    self.picker = None;
                    self.sidebar.focused = false;
                    self.find.focused = false;
                    self.switch_to(i);
                }
            }
            id::Target::TreeItem(node) => self.access_tree_item(node, req.action),
            id::Target::Option(n) => {
                if req.action == Action::Click {
                    self.picker_choose(n);
                    if let Some(cmd) = self.pending_action.take() {
                        self.run(cmd, event_loop);
                    }
                    self.changed(false);
                }
            }
            id::Target::Run(field) => self.access_text(field, req, event_loop),
            id::Target::Fixed(node) => {
                let field = match node {
                    id::EDITOR => id::field::EDITOR,
                    id::FIND_FIELD => id::field::FIND,
                    id::REPLACE_FIELD => id::field::REPLACE,
                    id::PICKER_FIELD => id::field::PICKER,
                    id::SEARCH_FIELD => id::field::SEARCH,
                    id::SEARCH_REPLACE => id::field::SEARCH_REPLACE,
                    id::SEARCH_INCLUDE => id::field::SEARCH_INCLUDE,
                    id::SEARCH_EXCLUDE => id::field::SEARCH_EXCLUDE,
                    _ => return,
                };
                self.access_text(field, req, event_loop);
            }
        }
    }

    /// A request on title `n` of the drawn menu bar: open its menu
    /// (Click, Expand), or close it (Click on the open one, Collapse), as
    /// a click on the title does.
    fn access_menu_title(&mut self, n: usize, action: Action) {
        let open = self.menus.open_bar_menu() == Some(n);
        let Some(layout) = self.window_layout() else {
            return;
        };
        match action {
            Action::Click | Action::Collapse if open => self.menus.close(),
            Action::Click | Action::Expand if !open => {
                self.menus.open_bar(n, &layout, &self.keymap, true);
            }
            _ => return,
        }
        self.changed(false);
    }

    /// A request on entry `n` of the open menu: highlight it (Focus) or
    /// run it (Click).
    fn access_menu_item(&mut self, n: usize, action: Action, event_loop: &ActiveEventLoop) {
        match action {
            Action::Focus => {
                if let Some(layout) = self.window_layout() {
                    self.menus.set_hover(n, &layout);
                }
                self.changed(false);
            }
            Action::Click => {
                if let Some(id) = self.menus.choose(n) {
                    self.run_menu_item(id, event_loop);
                }
                self.changed(false);
            }
            _ => {}
        }
    }

    /// A request on button `n` of the front prompt: focus it, or press it.
    fn access_button(&mut self, n: usize, action: Action, event_loop: &ActiveEventLoop) {
        let Some(p) = self.prompts.front_mut() else {
            return;
        };
        let Some(answer) = p.buttons.get(n).map(|b| b.answer) else {
            return;
        };
        match action {
            Action::Focus => {
                p.focused = n;
                self.changed(false);
            }
            Action::Click => {
                self.prompt_answered(answer, event_loop);
                self.changed(false);
            }
            _ => {}
        }
    }

    /// Focus, caret and typing requests on text field `field`.
    fn access_text(&mut self, field: u8, req: ActionRequest, event_loop: &ActiveEventLoop) {
        match req.action {
            Action::Focus | Action::Click => self.access_focus(field),
            Action::SetTextSelection => {
                let Some(ActionData::SetTextSelection(sel)) = req.data else {
                    return;
                };
                let Some(win) = self.access.window(field) else {
                    return;
                };
                let (Some(anchor), Some(head)) = (win.byte_of(sel.anchor), win.byte_of(sel.focus))
                else {
                    return;
                };
                self.access_focus(field);
                if field == id::field::EDITOR {
                    let tab = &mut self.tabs[self.active];
                    Editor::new(&mut self.docs[tab.doc], &mut tab.carets).select_span(anchor, head);
                    self.changed(true);
                } else {
                    if let Some(solo) = self.access_solo(field) {
                        solo.ed().select_span(anchor, head);
                    }
                    self.changed(false);
                }
            }
            Action::ReplaceSelectedText => {
                if let Some(ActionData::Value(text)) = req.data {
                    self.access_focus(field);
                    self.run(Command::Insert(&text), event_loop);
                }
            }
            _ => {}
        }
    }

    /// Give text field `field` the keyboard, as a click on it does.
    fn access_focus(&mut self, field: u8) {
        match field {
            id::field::EDITOR => {
                self.picker = None;
                self.sidebar.focused = false;
                self.find.focused = false;
            }
            id::field::FIND | id::field::REPLACE => {
                self.picker = None;
                self.sidebar.focused = false;
                self.find.focused = true;
                self.find.focus = if field == id::field::REPLACE {
                    FindField::Replace
                } else {
                    FindField::Find
                };
            }
            id::field::PICKER => {}
            _ => {
                self.picker = None;
                self.find.focused = false;
                self.sidebar.focused = true;
                self.sidebar.search.focus = match field {
                    id::field::SEARCH_REPLACE => SearchFocus::Replace,
                    id::field::SEARCH_INCLUDE => SearchFocus::Include,
                    id::field::SEARCH_EXCLUDE => SearchFocus::Exclude,
                    _ => SearchFocus::Query,
                };
            }
        }
        self.changed(false);
    }

    /// The one-line field behind text field `field`.
    fn access_solo(&mut self, field: u8) -> Option<&mut Solo> {
        Some(match field {
            id::field::FIND => &mut self.find.find,
            id::field::REPLACE => &mut self.find.replace,
            id::field::PICKER => &mut self.picker.as_mut()?.input,
            id::field::SEARCH => &mut self.sidebar.search.query,
            id::field::SEARCH_REPLACE => &mut self.sidebar.search.replace,
            id::field::SEARCH_INCLUDE => &mut self.sidebar.search.include,
            id::field::SEARCH_EXCLUDE => &mut self.sidebar.search.exclude,
            _ => return None,
        })
    }

    /// A request on a tree row: select it (Focus), open it as Enter does
    /// (Click), or open and close a folder.
    fn access_tree_item(&mut self, node: usize, action: Action) {
        if self.sidebar.tree.node(node).is_none() {
            return;
        }
        self.picker = None;
        self.find.focused = false;
        self.sidebar.focused = true;
        self.sidebar.tree.selected = Some(node);
        match action {
            Action::Expand | Action::Collapse => {
                let requests = self
                    .sidebar
                    .tree
                    .set_expanded(node, action == Action::Expand);
                self.start_listings(requests);
            }
            Action::Click => {
                if let Some(layout) = self.window_layout()
                    && let Some(outcome) = self.sidebar.key(None, true, &layout)
                {
                    self.start_listings(outcome.requests);
                    if let Some(path) = outcome.open {
                        self.sidebar.focused = false;
                        self.open_path(&path);
                    }
                }
            }
            _ => {}
        }
        let layout = self.window_layout();
        self.sidebar.after_change(layout.as_ref());
        self.changed(false);
    }
}

#[expect(clippy::cast_precision_loss, reason = "a row count, for pixels")]
fn f64_of(n: usize) -> f64 {
    n as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn the_window_is_created_hidden_and_shown_only_after_the_adapter() {
        let steps = RefCell::new(Vec::new());
        let window: Result<(), ()> = open_window(
            Window::default_attributes(),
            |attrs| {
                steps.borrow_mut().push(if attrs.visible {
                    "create shown"
                } else {
                    "create hidden"
                });
                Ok(())
            },
            |()| steps.borrow_mut().push("attach"),
            |()| steps.borrow_mut().push("show"),
        );
        assert!(window.is_ok());
        assert_eq!(*steps.borrow(), ["create hidden", "attach", "show"]);
    }
}
