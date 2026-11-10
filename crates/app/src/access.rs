//! The accessibility tree (M22): what a screen reader reads.
//!
//! Nimbus draws everything itself on the GPU, so to the operating system
//! the window is one blank view: VoiceOver, NVDA and Orca find no buttons,
//! no text field, nothing to read. They learn what is on screen from an
//! *accessibility tree*, which native toolkits build for free. We build
//! one by hand with AccessKit: a tree of plain-data nodes, each with a
//! role (window, tab list, text field, list box), a name, a rectangle on
//! screen and a few states. `accesskit_winit` translates it into each
//! platform's own API (NSAccessibility, UI Automation, AT-SPI).
//!
//! This module only *describes*: it turns a [`Scene`], plain data the app
//! gathers each frame, into nodes, and remembers what it sent so the next
//! update carries only what changed ([`Sent`]). The app's glue
//! (`app/access.rs`) owns the adapter, fills in the scene, and maps what a
//! reader asks for (focus this tab, put the caret here) back onto the
//! same code the mouse and keyboard use. Nothing here needs a window or a
//! GPU, so the tree is tested like any other code; those tests are what
//! cover NVDA and Orca, which read the same tree.
//!
//! Nothing is built until a screen reader asks: the adapter calls back the
//! first time one wants the tree, and until then every update is skipped
//! before any of this runs.

pub mod git;
pub mod text;

use std::collections::{HashMap, HashSet};

use accesskit::{
    Action, Affine, Live, Node, NodeId, Rect, Role, TextDirection, TextSelection, TreeId, TreeInfo,
    TreeUpdate,
};

/// Node ids. Fixed parts of the window have fixed ids; repeated ones
/// (tabs, tree rows, list options, text runs) carry a kind in the top
/// byte and their index below it, so a reader's request names what it
/// acts on without a lookup table.
pub mod id {
    use accesskit::NodeId;

    /// The window: the tree's root.
    pub const WINDOW: NodeId = NodeId(1);
    /// The focused group's tab bar.
    pub const TABS: NodeId = NodeId(2);
    /// The editor's text.
    pub const EDITOR: NodeId = NodeId(3);
    /// The status bar.
    pub const STATUS: NodeId = NodeId(4);
    /// The status bar's details (line, column, encoding).
    pub const STATUS_INFO: NodeId = NodeId(5);
    /// The message line: "Saved", errors.
    pub const MESSAGE: NodeId = NodeId(6);
    /// The find bar.
    pub const FIND: NodeId = NodeId(7);
    /// The find bar's pattern field.
    pub const FIND_FIELD: NodeId = NodeId(8);
    /// The find bar's replacement field.
    pub const REPLACE_FIELD: NodeId = NodeId(9);
    /// The find bar's match count.
    pub const FIND_COUNT: NodeId = NodeId(10);
    /// The sidebar.
    pub const SIDEBAR: NodeId = NodeId(11);
    /// The sidebar's file tree.
    pub const TREE: NodeId = NodeId(12);
    /// The palette or a picker.
    pub const PICKER: NodeId = NodeId(13);
    /// The picker's query field.
    pub const PICKER_FIELD: NodeId = NodeId(14);
    /// The picker's list.
    pub const PICKER_LIST: NodeId = NodeId(15);
    /// The Search panel's pattern field.
    pub const SEARCH_FIELD: NodeId = NodeId(16);
    /// The Search panel's replacement field.
    pub const SEARCH_REPLACE: NodeId = NodeId(17);
    /// The Search panel's files-to-include field.
    pub const SEARCH_INCLUDE: NodeId = NodeId(18);
    /// The Search panel's files-to-leave-out field.
    pub const SEARCH_EXCLUDE: NodeId = NodeId(19);
    /// The Search panel's result count.
    pub const SEARCH_COUNT: NodeId = NodeId(20);
    /// The menu bar Nimbus draws (Windows).
    pub const MENU_BAR: NodeId = NodeId(21);
    /// The open drawn menu: a bar menu's drop-down or a right-click menu.
    pub const MENU: NodeId = NodeId(22);
    /// The open prompt ("Save changes?").
    pub const PROMPT: NodeId = NodeId(23);
    /// The prompt's text.
    pub const PROMPT_TEXT: NodeId = NodeId(24);

    /// Text fields, for [`run`]: each field's runs have ids of their own.
    pub mod field {
        /// The editor.
        pub const EDITOR: u8 = 0;
        /// The find bar's pattern.
        pub const FIND: u8 = 1;
        /// The find bar's replacement.
        pub const REPLACE: u8 = 2;
        /// The picker's query.
        pub const PICKER: u8 = 3;
        /// The Search panel's pattern.
        pub const SEARCH: u8 = 4;
        /// The Search panel's replacement.
        pub const SEARCH_REPLACE: u8 = 5;
        /// The Search panel's files to include.
        pub const SEARCH_INCLUDE: u8 = 6;
        /// The Search panel's files to leave out.
        pub const SEARCH_EXCLUDE: u8 = 7;
    }

    const KIND_SHIFT: u32 = 56;
    const TAB: u64 = 1;
    const TREE_ITEM: u64 = 2;
    const OPTION: u64 = 3;
    const MENU_TITLE: u64 = 4;
    const MENU_ITEM: u64 = 5;
    const BUTTON: u64 = 6;
    /// Runs take kinds `RUN..RUN + fields`.
    const RUN: u64 = 16;
    const LOW: u64 = (1 << KIND_SHIFT) - 1;

    fn make(kind: u64, n: usize) -> NodeId {
        NodeId(kind << KIND_SHIFT | (n as u64 & LOW))
    }

    /// Tab `n` of the focused group, counted from the left.
    #[must_use]
    pub fn tab(n: usize) -> NodeId {
        make(TAB, n)
    }

    /// The tree row showing the sidebar's node `node`.
    #[must_use]
    pub fn tree_item(node: usize) -> NodeId {
        make(TREE_ITEM, node)
    }

    /// The picker's match `n`, counted from the first match.
    #[must_use]
    pub fn option(n: usize) -> NodeId {
        make(OPTION, n)
    }

    /// Title `n` of the drawn menu bar.
    #[must_use]
    pub fn menu_title(n: usize) -> NodeId {
        make(MENU_TITLE, n)
    }

    /// Entry `n` of the open menu, counted with its separators.
    #[must_use]
    pub fn menu_item(n: usize) -> NodeId {
        make(MENU_ITEM, n)
    }

    /// Button `n` of the open prompt, in the prompt's own order.
    #[must_use]
    pub fn button(n: usize) -> NodeId {
        make(BUTTON, n)
    }

    /// Run `index` of text field `field`'s window `generation`.
    #[must_use]
    pub fn run(field: u8, generation: u16, index: usize) -> NodeId {
        let low = u64::from(generation) << 32 | (index as u64 & 0xffff_ffff);
        NodeId((RUN + u64::from(field)) << KIND_SHIFT | low)
    }

    /// What a node id names.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Target {
        /// One of the fixed nodes.
        Fixed(NodeId),
        /// A tab, by position in the focused group.
        Tab(usize),
        /// A tree row, by the sidebar's node.
        TreeItem(usize),
        /// A picker match.
        Option(usize),
        /// A title of the drawn menu bar.
        MenuTitle(usize),
        /// An entry of the open menu.
        MenuItem(usize),
        /// A button of the open prompt.
        Button(usize),
        /// A run of a text field.
        Run(u8),
    }

    /// Decode `id`.
    #[must_use]
    pub fn target(id: NodeId) -> Target {
        let kind = id.0 >> KIND_SHIFT;
        let n = usize::try_from(id.0 & LOW).unwrap_or(usize::MAX);
        match kind {
            TAB => Target::Tab(n),
            TREE_ITEM => Target::TreeItem(n),
            OPTION => Target::Option(n),
            MENU_TITLE => Target::MenuTitle(n),
            MENU_ITEM => Target::MenuItem(n),
            BUTTON => Target::Button(n),
            k if k >= RUN => Target::Run(u8::try_from(k - RUN).unwrap_or(u8::MAX)),
            _ => Target::Fixed(id),
        }
    }
}

/// A rectangle in physical pixels, `[x, y, width, height]`, as the app's
/// layout code works them out.
pub type Bounds = [f32; 4];

fn rect([x, y, w, h]: Bounds) -> Rect {
    let (x, y) = (f64::from(x), f64::from(y));
    Rect::new(x, y, x + f64::from(w.max(0.0)), y + f64::from(h.max(0.0)))
}

/// A text field: its runs come from a [`text::TextWindow`].
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// What it is called, such as "Find" or the file's name.
    pub label: String,
    /// Shown while it is empty, such as "Type a command".
    pub placeholder: Option<String>,
    /// Where it is on screen.
    pub bounds: Bounds,
    /// Where its runs' coordinates start on screen: column 0 of the window's
    /// first row. Runs are placed relative to it, so scrolling moves only
    /// this, not every run.
    pub origin: [f64; 2],
    /// The caret and selection.
    pub selection: Option<TextSelection>,
    /// It cannot be edited.
    pub read_only: bool,
}

/// One tab.
#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    /// The file's name, with "(deleted)" or "(changed on disk)".
    pub name: String,
    /// Unsaved changes.
    pub dirty: bool,
    /// Where it is drawn.
    pub bounds: Bounds,
}

/// One row of the sidebar's file tree.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeRow {
    /// The sidebar's node shown.
    pub node: usize,
    /// File or folder name.
    pub name: String,
    /// Indentation level, 0 at the top.
    pub depth: usize,
    /// For a folder: whether it is open.
    pub expanded: Option<bool>,
    /// The selected row.
    pub selected: bool,
    /// Where it is drawn, if on screen.
    pub bounds: Option<Bounds>,
}

/// The sidebar.
#[derive(Debug, Clone, PartialEq)]
pub enum Sidebar {
    /// The file tree: a window of rows around the selection (or the top),
    /// so a huge expanded tree costs what is near the reader.
    Files {
        /// The workspace's name.
        name: String,
        /// The rows described.
        rows: Vec<TreeRow>,
    },
    /// The Search panel: its fields and the count of results.
    Search {
        /// Pattern, replacement (in replace mode), include and exclude.
        fields: Vec<(NodeId, Field)>,
        /// "12 results in 3 files" and the like.
        status: String,
    },
    /// The folder compare: only named for now (deferred, M22 Results).
    Compare,
}

/// The palette or a picker.
#[derive(Debug, Clone, PartialEq)]
pub struct Picker {
    /// "Command palette", "Go to file", …
    pub label: String,
    /// The query field.
    pub field: Field,
    /// The matches shown: index among all matches, text, detail, bounds.
    pub options: Vec<(usize, String, String, Bounds)>,
    /// The highlighted match's index.
    pub selected: Option<usize>,
    /// How many matches there are.
    pub total: usize,
    /// "42 files" and the like.
    pub status: String,
}

/// The find bar.
#[derive(Debug, Clone, PartialEq)]
pub struct Find {
    /// The pattern field.
    pub pattern: Field,
    /// The replacement field, in replace mode.
    pub replace: Option<Field>,
    /// "3 of 120", "no matches", or a regex error.
    pub count: String,
    /// Where the bar is.
    pub bounds: Bounds,
}

/// The menu bar Nimbus draws (Windows): its titles and the open one.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuBar {
    /// Each title and where it is drawn.
    pub titles: Vec<(String, Bounds)>,
    /// The title whose menu is open.
    pub open: Option<usize>,
    /// Where the bar is.
    pub bounds: Bounds,
}

/// One entry of an open drawn menu.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuEntry {
    /// An item: its title, its shortcut ("Ctrl+S", or empty) and where it
    /// is drawn (`None` while scrolled out of the box).
    Item(String, String, Option<Bounds>),
    /// A line between groups; not described, but it keeps the entries'
    /// numbers the menu's own.
    Separator,
}

/// An open drawn menu: a bar menu's drop-down, or a right-click menu.
#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    /// The bar title it drops down from, or "Context menu".
    pub label: String,
    /// Its entries, in order.
    pub entries: Vec<MenuEntry>,
    /// The highlighted entry.
    pub hover: Option<usize>,
    /// Where the box is.
    pub bounds: Bounds,
}

/// An open prompt: a question the app waits on.
#[derive(Debug, Clone, PartialEq)]
pub struct Prompt {
    /// Its heading, such as "Unsaved changes".
    pub title: String,
    /// The question.
    pub text: String,
    /// Each button's label, where it is drawn, and whether it is the
    /// default (the one Enter presses when the prompt opens).
    pub buttons: Vec<(String, Bounds, bool)>,
    /// The button with the keyboard.
    pub focused: usize,
    /// Where the box is.
    pub bounds: Bounds,
}

/// Everything the tree describes, gathered by the app for one update.
#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    /// The window's title.
    pub title: String,
    /// The window's size, `[width, height]`, below the inset.
    pub size: [f32; 2],
    /// The space the renderer keeps above the layout for the drawn title
    /// bar or menu bar (`Gpu::inset_top`). Layout coordinates start below
    /// it, so the window's node moves everything down by it.
    pub inset: f32,
    /// The drawn menu bar (Windows).
    pub menu_bar: Option<MenuBar>,
    /// The open drawn menu.
    pub menu: Option<Menu>,
    /// The front prompt, when one is open.
    pub prompt: Option<Prompt>,
    /// The focused group's tabs and which is active.
    pub tabs: Vec<Tab>,
    /// The active tab's position in `tabs`.
    pub active_tab: usize,
    /// Where the tab bar is.
    pub tab_bar: Bounds,
    /// The editor's text field.
    pub editor: Field,
    /// The status bar's details and where the bar is.
    pub status: Option<(String, Bounds)>,
    /// The message line's text.
    pub message: Option<String>,
    /// The find bar, when open.
    pub find: Option<Find>,
    /// The sidebar, when shown, and where.
    pub sidebar: Option<(Sidebar, Bounds)>,
    /// The palette or a picker, when open.
    pub picker: Option<Picker>,
    /// The node with the keyboard.
    pub focus: NodeId,
}

/// Runs each text field shows, from the fields' [`text::TextWindow`]s.
pub type Runs<'a> = &'a dyn Fn(NodeId) -> &'a [NodeId];

/// The nodes for `scene`, every one of them; [`Sent`] drops the unchanged.
/// `runs` gives each text field's children.
#[must_use]
pub fn build(scene: &Scene, runs: Runs<'_>) -> Vec<(NodeId, Node)> {
    let mut out = Vec::with_capacity(32);
    let mut window = Node::new(Role::Window);
    window.set_label(scene.title.as_str());
    // Every position below is in layout pixels, which start under the
    // inset; the window's transform moves them to where they are on
    // screen, and its own rectangle (in the same moved coordinates)
    // starts above, at the window's top edge.
    let inset = scene.inset.max(0.0);
    window.set_transform(Affine::translate((0.0, f64::from(inset))));
    window.set_bounds(rect([0.0, -inset, scene.size[0], scene.size[1] + inset]));
    let mut children = Vec::with_capacity(8);

    if let Some(bar) = &scene.menu_bar {
        children.push(id::MENU_BAR);
        menu_bar_nodes(bar, &mut out);
    }

    if let Some((sidebar, bounds)) = &scene.sidebar {
        children.push(id::SIDEBAR);
        sidebar_nodes(sidebar, *bounds, runs, &mut out);
    }

    // The tab bar: a tab list whose tabs say their position ("2 of 5")
    // and whether they are selected; selecting one switches to it.
    children.push(id::TABS);
    let mut tabs = Node::new(Role::TabList);
    tabs.set_label("Open files");
    tabs.set_bounds(rect(scene.tab_bar));
    tabs.set_size_of_set(scene.tabs.len());
    for (n, tab) in scene.tabs.iter().enumerate() {
        tabs.push_child(id::tab(n));
        let mut node = Node::new(Role::Tab);
        let label = if tab.dirty {
            format!("{}, modified", tab.name)
        } else {
            tab.name.clone()
        };
        node.set_label(label);
        node.set_bounds(rect(tab.bounds));
        node.set_position_in_set(n + 1);
        node.set_size_of_set(scene.tabs.len());
        node.set_selected(n == scene.active_tab);
        node.add_action(Action::Click);
        node.add_action(Action::Focus);
        node.push_controlled(id::EDITOR);
        out.push((id::tab(n), node));
    }
    out.push((id::TABS, tabs));

    if let Some(find) = &scene.find {
        children.push(id::FIND);
        find_nodes(find, runs, &mut out);
    }

    children.push(id::EDITOR);
    let mut editor = field_node(Role::MultilineTextInput, &scene.editor, runs(id::EDITOR));
    editor.add_action(Action::ReplaceSelectedText);
    out.push((id::EDITOR, editor));

    if scene.status.is_some() || scene.message.is_some() {
        children.push(id::STATUS);
        let mut status = Node::new(Role::Status);
        status.set_label("Status");
        // Off, though a status region is polite by default: the details
        // change on every cursor move and would be spoken each time.
        status.set_live(Live::Off);
        if let Some((info, bounds)) = &scene.status {
            status.set_bounds(rect(*bounds));
            status.push_child(id::STATUS_INFO);
            let mut node = Node::new(Role::Label);
            node.set_value(info.as_str());
            out.push((id::STATUS_INFO, node));
        }
        if let Some(message) = &scene.message {
            // A live region: a reader speaks it when it appears or
            // changes, without moving focus.
            status.push_child(id::MESSAGE);
            let mut node = Node::new(Role::Label);
            node.set_value(message.as_str());
            node.set_live(Live::Polite);
            out.push((id::MESSAGE, node));
        }
        out.push((id::STATUS, status));
    }

    if let Some(picker) = &scene.picker {
        children.push(id::PICKER);
        picker_nodes(picker, runs, &mut out);
    }

    // Last, as they are drawn over everything else.
    if let Some(menu) = &scene.menu {
        children.push(id::MENU);
        menu_nodes(menu, &mut out);
    }
    if let Some(prompt) = &scene.prompt {
        children.push(id::PROMPT);
        prompt_nodes(prompt, &mut out);
    }

    window.set_children(children);
    out.push((id::WINDOW, window));
    out
}

/// A text field's node: `role`, its label and selection, its runs.
fn field_node(role: Role, field: &Field, runs: &[NodeId]) -> Node {
    let mut node = Node::new(role);
    node.set_label(field.label.as_str());
    if let Some(p) = &field.placeholder {
        node.set_placeholder(p.as_str());
    }
    // Runs are placed from the field's origin; the field's own rectangle
    // is given in the same moved coordinates.
    let [ox, oy] = field.origin;
    node.set_transform(Affine::translate((ox, oy)));
    let r = rect(field.bounds);
    node.set_bounds(Rect::new(r.x0 - ox, r.y0 - oy, r.x1 - ox, r.y1 - oy));
    node.set_text_direction(TextDirection::LeftToRight);
    node.set_children(runs.to_vec());
    if let Some(sel) = field.selection {
        node.set_text_selection(sel);
    }
    if field.read_only {
        node.set_read_only();
    }
    node.add_action(Action::Focus);
    node.add_action(Action::SetTextSelection);
    node
}

fn find_nodes(find: &Find, runs: Runs<'_>, out: &mut Vec<(NodeId, Node)>) {
    let mut bar = Node::new(Role::Search);
    bar.set_label("Find");
    bar.set_bounds(rect(find.bounds));
    let mut kids = vec![id::FIND_FIELD];
    out.push((
        id::FIND_FIELD,
        field_node(Role::SearchInput, &find.pattern, runs(id::FIND_FIELD)),
    ));
    if let Some(replace) = &find.replace {
        kids.push(id::REPLACE_FIELD);
        let mut node = field_node(Role::TextInput, replace, runs(id::REPLACE_FIELD));
        node.add_action(Action::ReplaceSelectedText);
        out.push((id::REPLACE_FIELD, node));
    }
    kids.push(id::FIND_COUNT);
    let mut count = Node::new(Role::Label);
    count.set_value(find.count.as_str());
    count.set_live(Live::Polite);
    out.push((id::FIND_COUNT, count));
    bar.set_children(kids);
    out.push((id::FIND, bar));
}

fn sidebar_nodes(sidebar: &Sidebar, bounds: Bounds, runs: Runs<'_>, out: &mut Vec<(NodeId, Node)>) {
    let mut node = Node::new(Role::Complementary);
    node.set_bounds(rect(bounds));
    match sidebar {
        Sidebar::Files { name, rows } => {
            node.set_label("Files");
            node.push_child(id::TREE);
            let mut tree = Node::new(Role::Tree);
            tree.set_label(name.as_str());
            tree.set_bounds(rect(bounds));
            for row in rows {
                let item = id::tree_item(row.node);
                tree.push_child(item);
                let mut n = Node::new(Role::TreeItem);
                n.set_label(row.name.as_str());
                n.set_level(row.depth + 1);
                n.set_selected(row.selected);
                if let Some(b) = row.bounds {
                    n.set_bounds(rect(b));
                }
                if let Some(open) = row.expanded {
                    n.set_expanded(open);
                    n.add_action(if open {
                        Action::Collapse
                    } else {
                        Action::Expand
                    });
                }
                n.add_action(Action::Click);
                n.add_action(Action::Focus);
                out.push((item, n));
            }
            out.push((id::TREE, tree));
        }
        Sidebar::Search { fields, status } => {
            node.set_label("Search in files");
            for (field_id, field) in fields {
                node.push_child(*field_id);
                let role = if *field_id == id::SEARCH_FIELD {
                    Role::SearchInput
                } else {
                    Role::TextInput
                };
                out.push((*field_id, field_node(role, field, runs(*field_id))));
            }
            node.push_child(id::SEARCH_COUNT);
            let mut count = Node::new(Role::Label);
            count.set_value(status.as_str());
            count.set_live(Live::Polite);
            out.push((id::SEARCH_COUNT, count));
        }
        Sidebar::Compare => node.set_label("Compare folders"),
    }
    out.push((id::SIDEBAR, node));
}

fn picker_nodes(picker: &Picker, runs: Runs<'_>, out: &mut Vec<(NodeId, Node)>) {
    let mut dialog = Node::new(Role::Dialog);
    dialog.set_label(picker.label.as_str());
    dialog.set_modal();
    dialog.set_children(vec![id::PICKER_FIELD, id::PICKER_LIST]);
    // The query field is a combo box: typing filters the list below it,
    // and the highlighted match is its active descendant, which a reader
    // speaks as the arrows move it while the keyboard stays in the field.
    let mut field = field_node(
        Role::EditableComboBox,
        &picker.field,
        runs(id::PICKER_FIELD),
    );
    field.push_controlled(id::PICKER_LIST);
    field.set_description(picker.status.as_str());
    field.set_expanded(true);
    if let Some(sel) = picker.selected {
        field.set_active_descendant(id::option(sel));
    }
    out.push((id::PICKER_FIELD, field));
    let mut list = Node::new(Role::ListBox);
    list.set_label(picker.status.as_str());
    list.set_size_of_set(picker.total);
    for (n, text, detail, bounds) in &picker.options {
        let oid = id::option(*n);
        list.push_child(oid);
        let mut option = Node::new(Role::ListBoxOption);
        let label = if detail.is_empty() {
            text.clone()
        } else {
            format!("{text}, {detail}")
        };
        option.set_value(label);
        option.set_bounds(rect(*bounds));
        option.set_position_in_set(n + 1);
        option.set_size_of_set(picker.total);
        option.set_selected(picker.selected == Some(*n));
        option.add_action(Action::Click);
        out.push((oid, option));
    }
    out.push((id::PICKER_LIST, list));
    out.push((id::PICKER, dialog));
}

fn menu_bar_nodes(bar: &MenuBar, out: &mut Vec<(NodeId, Node)>) {
    let mut node = Node::new(Role::MenuBar);
    node.set_label("Menu bar");
    node.set_bounds(rect(bar.bounds));
    for (n, (title, bounds)) in bar.titles.iter().enumerate() {
        let tid = id::menu_title(n);
        node.push_child(tid);
        // A title is a menu item with a menu behind it: "File, menu",
        // open or closed. Pressing it opens its menu.
        let mut item = Node::new(Role::MenuItem);
        item.set_label(title.as_str());
        item.set_bounds(rect(*bounds));
        item.set_has_popup(accesskit::HasPopup::Menu);
        item.set_expanded(bar.open == Some(n));
        item.set_position_in_set(n + 1);
        item.set_size_of_set(bar.titles.len());
        item.add_action(Action::Click);
        item.add_action(if bar.open == Some(n) {
            Action::Collapse
        } else {
            Action::Expand
        });
        if bar.open == Some(n) {
            item.push_controlled(id::MENU);
        }
        out.push((tid, item));
    }
    out.push((id::MENU_BAR, node));
}

fn menu_nodes(menu: &Menu, out: &mut Vec<(NodeId, Node)>) {
    let mut node = Node::new(Role::Menu);
    node.set_label(menu.label.as_str());
    node.set_bounds(rect(menu.bounds));
    let items = menu
        .entries
        .iter()
        .filter(|e| matches!(e, MenuEntry::Item(..)))
        .count();
    let mut position = 0;
    for (n, entry) in menu.entries.iter().enumerate() {
        let MenuEntry::Item(title, keys, bounds) = entry else {
            continue;
        };
        position += 1;
        let iid = id::menu_item(n);
        node.push_child(iid);
        let mut item = Node::new(Role::MenuItem);
        item.set_label(title.as_str());
        if !keys.is_empty() {
            // Read after the title, as a native menu's shortcut is.
            item.set_keyboard_shortcut(keys.as_str());
        }
        if let Some(b) = bounds {
            item.set_bounds(rect(*b));
        }
        item.set_position_in_set(position);
        item.set_size_of_set(items);
        item.set_selected(menu.hover == Some(n));
        item.add_action(Action::Click);
        item.add_action(Action::Focus);
        out.push((iid, item));
    }
    out.push((id::MENU, node));
}

fn prompt_nodes(prompt: &Prompt, out: &mut Vec<(NodeId, Node)>) {
    // An alert dialog: a reader speaks its title and text when it opens,
    // and keeps its reading inside it until it is answered, as the prompt
    // keeps the keyboard.
    let mut dialog = Node::new(Role::AlertDialog);
    dialog.set_label(prompt.title.as_str());
    dialog.set_modal();
    dialog.set_bounds(rect(prompt.bounds));
    dialog.push_child(id::PROMPT_TEXT);
    dialog.push_described_by(id::PROMPT_TEXT);
    let mut text = Node::new(Role::Label);
    text.set_value(prompt.text.as_str());
    out.push((id::PROMPT_TEXT, text));
    for (n, (label, bounds, default)) in prompt.buttons.iter().enumerate() {
        let bid = id::button(n);
        dialog.push_child(bid);
        let mut b = Node::new(if *default {
            Role::DefaultButton
        } else {
            Role::Button
        });
        b.set_label(label.as_str());
        b.set_bounds(rect(*bounds));
        b.add_action(Action::Click);
        b.add_action(Action::Focus);
        out.push((bid, b));
    }
    out.push((id::PROMPT, dialog));
}

/// The nodes as last sent, so an update carries only what changed.
#[derive(Debug, Default)]
pub struct Sent {
    nodes: HashMap<NodeId, Node>,
    seen: HashSet<NodeId>,
}

impl Sent {
    /// Forget everything: the next update sends every node.
    pub fn clear(&mut self) {
        self.nodes.clear();
    }

    /// Add the nodes of `built` that differ from what was sent to `out`,
    /// and remember them. Nodes not in `built` are forgotten, so one that
    /// comes back later is sent again.
    pub fn diff(&mut self, built: Vec<(NodeId, Node)>, out: &mut Vec<(NodeId, Node)>) {
        self.seen.clear();
        for (node_id, node) in built {
            self.seen.insert(node_id);
            if self.nodes.get(&node_id) != Some(&node) {
                self.nodes.insert(node_id, node.clone());
                out.push((node_id, node));
            }
        }
        let seen = &self.seen;
        self.nodes.retain(|k, _| seen.contains(k));
    }
}

/// A tree update from changed nodes: the tree's identity on a full
/// update, and the focus, which every update must carry.
#[must_use]
pub fn update(nodes: Vec<(NodeId, Node)>, focus: NodeId, full: bool) -> TreeUpdate {
    TreeUpdate {
        nodes,
        tree: full.then(|| {
            let mut info = TreeInfo::new(id::WINDOW);
            info.toolkit_name = Some("Nimbus".to_owned());
            info.toolkit_version = Some(env!("CARGO_PKG_VERSION").to_owned());
            info
        }),
        tree_id: TreeId::ROOT,
        focus,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use accesskit_consumer::{NodeRef, Tree};

    fn field(label: &str) -> Field {
        Field {
            label: label.to_owned(),
            placeholder: None,
            bounds: [100.0, 50.0, 800.0, 500.0],
            origin: [100.0, 50.0],
            selection: None,
            read_only: false,
        }
    }

    fn scene() -> Scene {
        Scene {
            title: "a.rs — Nimbus".to_owned(),
            size: [1000.0, 600.0],
            inset: 0.0,
            menu_bar: None,
            menu: None,
            prompt: None,
            tabs: vec![
                Tab {
                    name: "a.rs".to_owned(),
                    dirty: true,
                    bounds: [0.0, 0.0, 80.0, 30.0],
                },
                Tab {
                    name: "b.rs".to_owned(),
                    dirty: false,
                    bounds: [80.0, 0.0, 80.0, 30.0],
                },
            ],
            active_tab: 0,
            tab_bar: [0.0, 0.0, 1000.0, 30.0],
            editor: field("a.rs"),
            status: Some(("Ln 1, Col 1".to_owned(), [0.0, 580.0, 1000.0, 20.0])),
            message: None,
            find: None,
            sidebar: None,
            picker: None,
            focus: id::EDITOR,
        }
    }

    fn no_runs(_: NodeId) -> &'static [NodeId] {
        &[]
    }

    /// Build `scene` into a full tree that AccessKit's own consumer
    /// accepts; it panics on a malformed one (an orphan, a bad focus).
    fn tree_of(scene: &Scene) -> Tree {
        let nodes = build(scene, &no_runs);
        Tree::new(update(nodes, scene.focus, true), true)
    }

    /// The node with local id `target`, found by walking from the root.
    pub(crate) fn find(n: NodeRef<'_>, target: NodeId) -> Option<NodeRef<'_>> {
        if n.locate().0 == target {
            return Some(n);
        }
        n.children().find_map(|c| find(c, target))
    }

    fn get(tree: &Tree, node: NodeId) -> NodeRef<'_> {
        find(tree.state().root(), node).expect("the node")
    }

    fn label(tree: &Tree, node: NodeId) -> Option<String> {
        find(tree.state().root(), node).and_then(|n| n.label().or_else(|| n.value()))
    }

    #[test]
    fn ids_decode_to_what_they_name() {
        assert_eq!(id::target(id::tab(3)), id::Target::Tab(3));
        assert_eq!(id::target(id::tree_item(70)), id::Target::TreeItem(70));
        assert_eq!(id::target(id::option(0)), id::Target::Option(0));
        assert_eq!(
            id::target(id::run(id::field::FIND, 9, 4)),
            id::Target::Run(id::field::FIND)
        );
        assert_eq!(id::target(id::EDITOR), id::Target::Fixed(id::EDITOR));
        assert_ne!(id::run(0, 1, 4), id::run(0, 2, 4), "generations differ");
    }

    #[test]
    fn tabs_are_a_tab_list_with_names_positions_and_modified_state() {
        let tree = tree_of(&scene());
        assert_eq!(label(&tree, id::tab(0)).as_deref(), Some("a.rs, modified"));
        assert_eq!(label(&tree, id::tab(1)).as_deref(), Some("b.rs"));
        let tab = get(&tree, id::tab(1));
        assert_eq!(tab.role(), Role::Tab);
        assert_eq!(tab.position_in_set(), Some(2));
        assert_eq!(
            tab.size_of_set_from_container(&accesskit_consumer::common_filter),
            Some(2)
        );
        assert_eq!(tab.is_selected(), Some(false));
        let focus = tree.state().focus().map(|f| f.locate());
        assert_eq!(focus.map(|f| f.0), Some(id::EDITOR));
    }

    #[test]
    fn the_message_is_a_polite_live_region_and_the_details_are_quiet() {
        let mut s = scene();
        s.message = Some("Saved".to_owned());
        let tree = tree_of(&s);
        assert_eq!(get(&tree, id::MESSAGE).live(), Live::Polite);
        assert_eq!(get(&tree, id::STATUS_INFO).live(), Live::Off);
        assert_eq!(get(&tree, id::STATUS).role(), Role::Status);
        assert_eq!(label(&tree, id::MESSAGE).as_deref(), Some("Saved"));
    }

    #[test]
    fn the_sidebar_is_a_tree_and_the_picker_a_list_with_its_highlight() {
        let mut s = scene();
        s.sidebar = Some((
            Sidebar::Files {
                name: "nimbus".to_owned(),
                rows: vec![
                    TreeRow {
                        node: 4,
                        name: "src".to_owned(),
                        depth: 0,
                        expanded: Some(true),
                        selected: false,
                        bounds: None,
                    },
                    TreeRow {
                        node: 9,
                        name: "main.rs".to_owned(),
                        depth: 1,
                        expanded: None,
                        selected: true,
                        bounds: Some([0.0, 40.0, 200.0, 20.0]),
                    },
                ],
            },
            [0.0, 0.0, 200.0, 600.0],
        ));
        s.picker = Some(Picker {
            label: "Command palette".to_owned(),
            field: field("Command palette"),
            options: vec![
                (5, "Save".to_owned(), "⌘S".to_owned(), [0.0; 4]),
                (6, "Save As…".to_owned(), String::new(), [0.0; 4]),
            ],
            selected: Some(6),
            total: 40,
            status: "40 commands".to_owned(),
        });
        s.focus = id::PICKER_FIELD;
        let tree = tree_of(&s);
        let get = |n| get(&tree, n);
        let row = get(id::tree_item(9));
        assert_eq!(row.role(), Role::TreeItem);
        assert_eq!(row.level(), Some(2));
        assert_eq!(row.is_selected(), Some(true));
        assert_eq!(get(id::tree_item(4)).data().is_expanded(), Some(true));
        assert_eq!(label(&tree, id::option(5)).as_deref(), Some("Save, ⌘S"));
        let option = get(id::option(6));
        assert_eq!(option.position_in_set(), Some(7));
        assert_eq!(option.is_selected(), Some(true));
        assert_eq!(
            get(id::PICKER_FIELD).data().active_descendant(),
            Some(id::option(6))
        );
    }

    #[test]
    fn drawn_menus_read_as_a_menu_bar_and_a_menu_without_separators() {
        let mut s = scene();
        s.inset = 30.0;
        s.menu_bar = Some(MenuBar {
            titles: vec![
                ("File".to_owned(), [0.0, -30.0, 50.0, 30.0]),
                ("Edit".to_owned(), [50.0, -30.0, 50.0, 30.0]),
            ],
            open: Some(1),
            bounds: [0.0, -30.0, 1000.0, 30.0],
        });
        s.menu = Some(Menu {
            label: "Edit".to_owned(),
            entries: vec![
                MenuEntry::Item("Undo".to_owned(), "Ctrl+Z".to_owned(), None),
                MenuEntry::Separator,
                MenuEntry::Item(
                    "Cut".to_owned(),
                    String::new(),
                    Some([50.0, 40.0, 200.0, 30.0]),
                ),
            ],
            hover: Some(2),
            bounds: [50.0, 0.0, 200.0, 80.0],
        });
        s.focus = id::menu_item(2);
        let tree = tree_of(&s);
        let get = |n| get(&tree, n);
        assert_eq!(get(id::MENU_BAR).role(), Role::MenuBar);
        let edit = get(id::menu_title(1));
        assert_eq!(edit.data().is_expanded(), Some(true));
        assert_eq!(get(id::menu_title(0)).data().is_expanded(), Some(false));
        assert_eq!(
            get(id::menu_item(0)).data().keyboard_shortcut(),
            Some("Ctrl+Z")
        );
        let cut = get(id::menu_item(2));
        assert_eq!(cut.role(), Role::MenuItem);
        assert_eq!(
            cut.position_in_set(),
            Some(2),
            "the separator is not counted"
        );
        assert_eq!(cut.is_selected(), Some(true));
        assert_eq!(get(id::MENU).children().count(), 2);
        // Layout pixels start under the inset: on screen, the bar is at the
        // window's top and the item 30 pixels lower than its layout y.
        let bar = get(id::menu_title(0)).bounding_box().expect("a box");
        assert_eq!((bar.y0, bar.y1), (0.0, 30.0));
        let item = cut.bounding_box().expect("a box");
        assert_eq!(item.y0, 70.0);
        let focus = tree.state().focus().map(|f| f.locate().0);
        assert_eq!(focus, Some(id::menu_item(2)));
    }

    #[test]
    fn a_prompt_is_an_alert_dialog_with_its_default_button() {
        let mut s = scene();
        s.prompt = Some(Prompt {
            title: "Unsaved changes".to_owned(),
            text: "Save changes to a.rs before closing?".to_owned(),
            buttons: vec![
                ("Don't Save".to_owned(), [0.0; 4], false),
                ("Cancel".to_owned(), [0.0; 4], false),
                ("Save".to_owned(), [0.0; 4], true),
            ],
            focused: 2,
            bounds: [200.0, 100.0, 500.0, 200.0],
        });
        s.focus = id::button(2);
        let tree = tree_of(&s);
        let dialog = get(&tree, id::PROMPT);
        assert_eq!(dialog.role(), Role::AlertDialog);
        assert_eq!(dialog.label().as_deref(), Some("Unsaved changes"));
        assert_eq!(get(&tree, id::button(2)).role(), Role::DefaultButton);
        assert_eq!(get(&tree, id::button(0)).role(), Role::Button);
        assert_eq!(
            label(&tree, id::PROMPT_TEXT).as_deref(),
            Some("Save changes to a.rs before closing?")
        );
        assert_eq!(id::target(id::button(1)), id::Target::Button(1));
        assert_eq!(id::target(id::menu_item(7)), id::Target::MenuItem(7));
        assert_eq!(id::target(id::menu_title(3)), id::Target::MenuTitle(3));
    }

    #[test]
    fn an_update_sends_only_what_changed_and_forgets_what_left() {
        let mut sent = Sent::default();
        let mut out = Vec::new();
        let s = scene();
        sent.diff(build(&s, &no_runs), &mut out);
        let all = out.len();
        assert!(all >= 5);
        out.clear();
        sent.diff(build(&s, &no_runs), &mut out);
        assert!(out.is_empty(), "nothing changed");
        let mut t = s.clone();
        t.message = Some("Saved".to_owned());
        sent.diff(build(&t, &no_runs), &mut out);
        let mut ids: Vec<NodeId> = out.iter().map(|(i, _)| *i).collect();
        ids.sort();
        assert_eq!(
            ids,
            [id::STATUS, id::MESSAGE],
            "the new node and its parent"
        );
        out.clear();
        sent.diff(build(&s, &no_runs), &mut out);
        out.clear();
        sent.diff(build(&t, &no_runs), &mut out);
        assert!(
            out.iter().any(|(i, _)| *i == id::MESSAGE),
            "a message that comes back is sent again, and spoken again"
        );
    }

    #[test]
    fn incremental_updates_apply_cleanly_to_the_consumers_tree() {
        let mut sent = Sent::default();
        let mut out = Vec::new();
        let s = scene();
        sent.diff(build(&s, &no_runs), &mut out);
        let mut tree = Tree::new(update(std::mem::take(&mut out), s.focus, true), true);
        let mut t = s.clone();
        t.find = Some(Find {
            pattern: field("Find"),
            replace: None,
            count: "3 of 7".to_owned(),
            bounds: [0.0, 30.0, 1000.0, 30.0],
        });
        t.focus = id::FIND_FIELD;
        t.tabs.pop();
        sent.diff(build(&t, &no_runs), &mut out);
        tree.update_and_process_changes(
            update(std::mem::take(&mut out), t.focus, false),
            &mut Ignore,
        );
        assert_eq!(label(&tree, id::FIND_COUNT).as_deref(), Some("3 of 7"));
        assert_eq!(label(&tree, id::tab(1)), None, "the closed tab is gone");
    }

    #[test]
    fn a_reader_moves_through_the_editor_by_character_word_and_line() {
        use crate::access::text::{Geometry, TextWindow};
        use nimbus_core::wrap::{WrapCache, WrapView};
        use nimbus_core::{RopeBuffer, Selection};

        let text = "fn main() {\n    let x = 1;\n}\n";
        let buffer = RopeBuffer::from_text(text);
        let mut win = TextWindow::new(id::field::EDITOR);
        let mut cache = WrapCache::default();
        let mut rows = WrapView::new(&mut cache, None, 4);
        let geometry = Geometry {
            cell: 10.0,
            line_height: 20.0,
            tab_width: 4,
        };
        let mut runs = Vec::new();
        let sel = Selection {
            anchor: 16,
            head: 19,
        };
        let frame = crate::access::text::Frame {
            top: None,
            geometry,
            long_scroll: 0.0,
        };
        let placed = win.update(&buffer, &mut rows, sel, frame, &mut runs);
        let mut s = scene();
        s.editor.selection = placed.selection;
        let children = win.children().to_vec();
        let mut nodes = build(&s, &|n| if n == id::EDITOR { &children } else { &[] });
        nodes.extend(runs);
        let tree = Tree::new(update(nodes, id::EDITOR, true), true);
        let editor = get(&tree, id::EDITOR);
        assert!(editor.supports_text_ranges());
        assert_eq!(editor.document_range().text(), text);
        let selected = editor.text_selection().expect("a selection");
        assert_eq!(selected.text(), "let", "Shift-selected text is read");
        // From the caret: the next character, the rest of the word, the line.
        let caret = editor.text_selection_focus().expect("a caret");
        let next = caret.forward_to_character_end();
        let mut r = caret.to_degenerate_range();
        r.set_end(next);
        assert_eq!(r.text(), " ");
        // The caret is in the word "let " (spaces belong to the word
        // before them), so the word ends after the space; the next is "x ".
        let word = caret.forward_to_word_end();
        let mut r = word.to_degenerate_range();
        r.set_end(word.forward_to_word_end());
        assert_eq!(r.text(), "x ");
        let start = caret.backward_to_line_start();
        let end = caret.forward_to_line_end();
        let mut r = start.to_degenerate_range();
        r.set_end(end);
        assert_eq!(r.text(), "    let x = 1;\n");
        // The selection's box is where "let" is drawn: columns 4 to 7 of
        // the second row, moved by the field's origin.
        let boxes = selected.bounding_boxes();
        assert_eq!(boxes.len(), 1);
        assert_eq!(
            (boxes[0].x0, boxes[0].x1, boxes[0].y0),
            (140.0, 170.0, 70.0)
        );
    }

    struct Ignore;
    impl accesskit_consumer::TreeChangeHandler for Ignore {
        fn node_added(&mut self, _: &NodeRef<'_>) {}
        fn node_updated(&mut self, _: &NodeRef<'_>, _: &NodeRef<'_>) {}
        fn focus_moved(&mut self, _: Option<&NodeRef<'_>>, _: Option<&NodeRef<'_>>) {}
        fn node_removed(&mut self, _: &NodeRef<'_>) {}
    }
}
