//! The git window's accessibility tree: what a screen reader reads in
//! `nimbus --git` (M27's window, described after M22).
//!
//! The git window is a process and a window of its own, so it has a tree
//! of its own, built the same way as the editor's (see the parent module):
//! the window gathers a [`GitScene`] of plain data, [`build`] turns it into
//! nodes, and [`super::Sent`] keeps only what changed. Node ids are this
//! tree's own; they never meet the editor's.
//!
//! The window's four panes become four landmarks a reader can jump
//! between: the sidebar is a tree (Changes, then the branch, remote, tag
//! and stash sections, each opening and closing), the history a list box
//! whose options read a commit's subject, author, date and branch labels,
//! the details a group of lines with the changed files as a second list
//! box, and the compare the new side of the diff as read-only text. The
//! compare options above it are buttons, on or off.

use accesskit::{Action, Live, Node, NodeId, Role, Toggled};

use super::{Bounds, Field, Runs, field_node, rect};

/// Node ids of the git window's tree.
pub mod id {
    use accesskit::NodeId;

    /// The window: the tree's root (the same id as the editor's root, so
    /// [`super::super::update`] serves both).
    pub const WINDOW: NodeId = super::super::id::WINDOW;
    /// The header: the repository, its branch, the commit count.
    pub const HEADER: NodeId = NodeId(2);
    /// "Opening the repository…", or why there is none.
    pub const NOTE: NodeId = NodeId(3);
    /// The sidebar's tree.
    pub const SIDEBAR: NodeId = NodeId(4);
    /// The history's list.
    pub const HISTORY: NodeId = NodeId(5);
    /// The details pane.
    pub const DETAILS: NodeId = NodeId(6);
    /// The details' list of files.
    pub const FILES: NodeId = NodeId(7);
    /// The compare options' toolbar.
    pub const OPTIONS: NodeId = NodeId(8);
    /// "Open in Editor".
    pub const OPEN_IN_EDITOR: NodeId = NodeId(9);
    /// The "Spaces" option.
    pub const WHITESPACE: NodeId = NodeId(10);
    /// The "Case" option.
    pub const CASE: NodeId = NodeId(11);
    /// The "Blank lines" option.
    pub const BLANK_LINES: NodeId = NodeId(12);
    /// The "Unified" option.
    pub const UNIFIED: NodeId = NodeId(13);
    /// The compare: its new side as text.
    pub const COMPARE: NodeId = NodeId(14);
    /// The status bar.
    pub const STATUS: NodeId = NodeId(15);
    /// The status bar's message ("Copied …").
    pub const MESSAGE: NodeId = NodeId(16);
    /// "Compare with" and a merge's parents.
    pub const PARENTS: NodeId = NodeId(17);

    /// The compare's text field, for [`super::super::id::run`].
    pub const COMPARE_FIELD: u8 = 0;

    const KIND_SHIFT: u32 = 56;
    const SIDE: u64 = 1;
    const ROW: u64 = 2;
    const FILE: u64 = 3;
    const PARENT: u64 = 4;
    const LINE: u64 = 5;
    /// Runs: the kinds `super::super::id::run` gives.
    const RUN: u64 = 16;
    const LOW: u64 = (1 << KIND_SHIFT) - 1;

    fn make(kind: u64, n: usize) -> NodeId {
        NodeId(kind << KIND_SHIFT | (n as u64 & LOW))
    }

    /// Sidebar line `n`.
    #[must_use]
    pub fn side(n: usize) -> NodeId {
        make(SIDE, n)
    }

    /// History row `n`, counted from the newest commit.
    #[must_use]
    pub fn row(n: usize) -> NodeId {
        make(ROW, n)
    }

    /// File `n` of the details' list.
    #[must_use]
    pub fn file(n: usize) -> NodeId {
        make(FILE, n)
    }

    /// A merge's parent `n`, counted from 0.
    #[must_use]
    pub fn parent(n: usize) -> NodeId {
        make(PARENT, n)
    }

    /// Line `n` of the details.
    #[must_use]
    pub fn line(n: usize) -> NodeId {
        make(LINE, n)
    }

    /// What a node id names.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Target {
        /// One of the fixed nodes.
        Fixed(NodeId),
        /// A sidebar line.
        Side(usize),
        /// A history row.
        Row(usize),
        /// A file.
        File(usize),
        /// A merge's parent.
        Parent(usize),
        /// A run of the compare's text.
        Run,
        /// Nothing that acts (a details line).
        Other,
    }

    /// Decode `id`.
    #[must_use]
    pub fn target(id: NodeId) -> Target {
        let n = usize::try_from(id.0 & LOW).unwrap_or(usize::MAX);
        match id.0 >> KIND_SHIFT {
            0 => Target::Fixed(id),
            SIDE => Target::Side(n),
            ROW => Target::Row(n),
            FILE => Target::File(n),
            PARENT => Target::Parent(n),
            k if k >= RUN => Target::Run,
            _ => Target::Other,
        }
    }
}

/// One line of the sidebar.
#[derive(Debug, Clone, PartialEq)]
pub struct SideRow {
    /// Its index in the sidebar, which actions name.
    pub index: usize,
    /// What it reads: "Changes, 3", "main, current branch", "Tags".
    pub name: String,
    /// 1 for Changes and the section titles, 2 inside a section.
    pub level: usize,
    /// For a section title: whether it is open.
    pub expanded: Option<bool>,
    /// Whether it is the selected line.
    pub selected: bool,
    /// Where it is drawn, if on screen.
    pub bounds: Option<Bounds>,
}

/// A row of a list (a commit, a file, a merge's parent).
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// Its index, which actions name.
    pub index: usize,
    /// What it reads.
    pub label: String,
    /// Whether it is selected.
    pub selected: bool,
    /// Where it is drawn, if on screen.
    pub bounds: Option<Bounds>,
}

/// A button of the compare options.
#[derive(Debug, Clone, PartialEq)]
pub struct Button {
    /// Which ([`id::OPEN_IN_EDITOR`], [`id::CASE`], …).
    pub id: NodeId,
    /// What it reads.
    pub label: String,
    /// For an option: whether it is on.
    pub on: Option<bool>,
    /// It does nothing now (Open in Editor with no file).
    pub disabled: bool,
    /// Where it is drawn, if on screen.
    pub bounds: Option<Bounds>,
}

/// The history's list: a window of rows around the selection.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryList {
    /// The rows described.
    pub rows: Vec<Item>,
    /// How many commits the history has so far.
    pub total: usize,
    /// "1,234 commits", "Reading history…", "No commits yet".
    pub status: String,
}

/// What the right side's lower half shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Compare {
    /// The diff: its new side's text field, and its status ("change 2 of
    /// 5").
    Text(Field, String),
    /// Instead of a diff: "Binary file", "Reading…".
    Message(String),
}

/// Everything the git window's tree describes, for one update.
#[derive(Debug, Clone, PartialEq)]
pub struct GitScene {
    /// The window's title.
    pub title: String,
    /// The window's size.
    pub size: [f32; 2],
    /// The header's text, and where it is.
    pub header: (String, Bounds),
    /// Instead of the panes: "Opening the repository…" or an error.
    pub note: Option<String>,
    /// The sidebar's lines, and where the sidebar is.
    pub side: (Vec<SideRow>, Option<Bounds>),
    /// The history, and where it is.
    pub history: (HistoryList, Option<Bounds>),
    /// The details' lines of text (the message, author, date …), and
    /// where the details are.
    pub details: (Vec<String>, Option<Bounds>),
    /// A merge's parents to compare against.
    pub parents: Vec<Item>,
    /// The changed files, and what the list is called ("2 files
    /// changed").
    pub files: (Vec<Item>, String),
    /// Open in Editor and the compare options.
    pub buttons: Vec<Button>,
    /// The compare, and where it is.
    pub compare: (Option<Compare>, Option<Bounds>),
    /// The status bar's message.
    pub message: Option<String>,
    /// The node with the keyboard.
    pub focus: NodeId,
}

/// The nodes for `scene`; `runs` gives the compare field's runs.
#[must_use]
pub fn build(scene: &GitScene, runs: Runs<'_>) -> Vec<(NodeId, Node)> {
    let mut out = Vec::with_capacity(64);
    let mut window = Node::new(Role::Window);
    window.set_label(scene.title.as_str());
    window.set_bounds(rect([0.0, 0.0, scene.size[0], scene.size[1]]));

    window.push_child(id::HEADER);
    let mut header = Node::new(Role::Banner);
    header.set_bounds(rect(scene.header.1));
    header.set_label(scene.header.0.as_str());
    out.push((id::HEADER, header));

    if let Some(note) = &scene.note {
        window.push_child(id::NOTE);
        let mut n = Node::new(Role::Label);
        n.set_value(note.as_str());
        n.set_live(Live::Polite);
        out.push((id::NOTE, n));
    } else {
        window.push_child(id::SIDEBAR);
        side_nodes(&scene.side.0, scene.side.1, &mut out);
        window.push_child(id::HISTORY);
        history_nodes(&scene.history.0, scene.history.1, &mut out);
        window.push_child(id::DETAILS);
        details_nodes(scene, &mut out);
        window.push_child(id::OPTIONS);
        option_nodes(&scene.buttons, &mut out);
        if let Some(compare) = &scene.compare.0 {
            window.push_child(id::COMPARE);
            out.push((id::COMPARE, compare_node(compare, scene.compare.1, runs)));
        }
    }

    window.push_child(id::STATUS);
    let mut status = Node::new(Role::Status);
    status.set_label("Status");
    status.set_live(Live::Off);
    if let Some(m) = &scene.message {
        status.push_child(id::MESSAGE);
        let mut node = Node::new(Role::Label);
        node.set_value(m.as_str());
        node.set_live(Live::Polite);
        out.push((id::MESSAGE, node));
    }
    out.push((id::STATUS, status));

    out.push((id::WINDOW, window));
    out
}

fn side_nodes(rows: &[SideRow], bounds: Option<Bounds>, out: &mut Vec<(NodeId, Node)>) {
    let mut tree = Node::new(Role::Tree);
    tree.set_label("Branches and changes");
    if let Some(b) = bounds {
        tree.set_bounds(rect(b));
    }
    for row in rows {
        let sid = id::side(row.index);
        tree.push_child(sid);
        let mut n = Node::new(Role::TreeItem);
        n.set_label(row.name.as_str());
        n.set_level(row.level);
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
        out.push((sid, n));
    }
    out.push((id::SIDEBAR, tree));
}

fn history_nodes(h: &HistoryList, bounds: Option<Bounds>, out: &mut Vec<(NodeId, Node)>) {
    let mut list = Node::new(Role::ListBox);
    list.set_label("History");
    list.set_description(h.status.as_str());
    if let Some(b) = bounds {
        list.set_bounds(rect(b));
    }
    list.set_size_of_set(h.total);
    for row in &h.rows {
        let rid = id::row(row.index);
        list.push_child(rid);
        out.push((rid, option(row, h.total)));
    }
    out.push((id::HISTORY, list));
}

/// A list box option: its label, place, selection and bounds.
fn option(item: &Item, total: usize) -> Node {
    let mut n = Node::new(Role::ListBoxOption);
    n.set_value(item.label.as_str());
    n.set_position_in_set(item.index + 1);
    n.set_size_of_set(total);
    n.set_selected(item.selected);
    if let Some(b) = item.bounds {
        n.set_bounds(rect(b));
    }
    n.add_action(Action::Click);
    n.add_action(Action::Focus);
    n
}

fn details_nodes(scene: &GitScene, out: &mut Vec<(NodeId, Node)>) {
    let (lines, bounds) = &scene.details;
    let mut group = Node::new(Role::Group);
    group.set_label("Details");
    if let Some(b) = bounds {
        group.set_bounds(rect(*b));
    }
    for (n, text) in lines.iter().enumerate() {
        let lid = id::line(n);
        group.push_child(lid);
        let mut node = Node::new(Role::Label);
        node.set_value(text.as_str());
        out.push((lid, node));
    }
    if !scene.parents.is_empty() {
        group.push_child(id::PARENTS);
        let mut parents = Node::new(Role::RadioGroup);
        parents.set_label("Compare with");
        for p in &scene.parents {
            let pid = id::parent(p.index);
            parents.push_child(pid);
            let mut b = Node::new(Role::RadioButton);
            b.set_label(p.label.as_str());
            b.set_toggled(Toggled::from(p.selected));
            if let Some(r) = p.bounds {
                b.set_bounds(rect(r));
            }
            b.add_action(Action::Click);
            out.push((pid, b));
        }
        out.push((id::PARENTS, parents));
    }
    let (files, label) = &scene.files;
    group.push_child(id::FILES);
    let mut list = Node::new(Role::ListBox);
    list.set_label(label.as_str());
    list.set_size_of_set(files.len());
    for f in files {
        let fid = id::file(f.index);
        list.push_child(fid);
        out.push((fid, option(f, files.len())));
    }
    out.push((id::FILES, list));
    out.push((id::DETAILS, group));
}

fn option_nodes(buttons: &[Button], out: &mut Vec<(NodeId, Node)>) {
    let mut bar = Node::new(Role::Toolbar);
    bar.set_label("Compare options");
    for b in buttons {
        bar.push_child(b.id);
        let mut n = Node::new(Role::Button);
        n.set_label(b.label.as_str());
        if let Some(on) = b.on {
            // A toggle button: read as "on" or "off" (pressed or not).
            n.set_toggled(Toggled::from(on));
        }
        if b.disabled {
            n.set_disabled();
        } else {
            n.add_action(Action::Click);
        }
        if let Some(r) = b.bounds {
            n.set_bounds(rect(r));
        }
        out.push((b.id, n));
    }
    out.push((id::OPTIONS, bar));
}

fn compare_node(compare: &Compare, bounds: Option<Bounds>, runs: Runs<'_>) -> Node {
    match compare {
        Compare::Text(field, status) => {
            let mut node = field_node(Role::MultilineTextInput, field, runs(id::COMPARE));
            node.set_description(status.as_str());
            node
        }
        Compare::Message(m) => {
            let mut node = Node::new(Role::Label);
            node.set_label("Compare");
            node.set_value(m.as_str());
            if let Some(b) = bounds {
                node.set_bounds(rect(b));
            }
            node
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::tests::find;
    use crate::access::update;
    use accesskit_consumer::Tree;

    fn item(index: usize, label: &str, selected: bool) -> Item {
        Item {
            index,
            label: label.to_owned(),
            selected,
            bounds: None,
        }
    }

    fn scene() -> GitScene {
        GitScene {
            title: "nimbus — Git — Nimbus".to_owned(),
            size: [1200.0, 800.0],
            header: (
                "nimbus, main, 3 commits".to_owned(),
                [0.0, 0.0, 1200.0, 30.0],
            ),
            note: None,
            side: (
                vec![
                    SideRow {
                        index: 0,
                        name: "Changes, 2".to_owned(),
                        level: 1,
                        expanded: None,
                        selected: false,
                        bounds: Some([0.0, 30.0, 200.0, 20.0]),
                    },
                    SideRow {
                        index: 1,
                        name: "Branches".to_owned(),
                        level: 1,
                        expanded: Some(true),
                        selected: false,
                        bounds: None,
                    },
                    SideRow {
                        index: 2,
                        name: "main, current branch".to_owned(),
                        level: 2,
                        expanded: None,
                        selected: true,
                        bounds: None,
                    },
                ],
                None,
            ),
            history: (
                HistoryList {
                    rows: vec![
                        item(0, "Merge side, Ada, 2 days ago", true),
                        item(1, "Fix, Ada, 3 days ago", false),
                    ],
                    total: 3,
                    status: "3 commits".to_owned(),
                },
                None,
            ),
            details: (vec!["Merge side".to_owned()], None),
            parents: vec![
                item(0, "parent 1 abc", true),
                item(1, "parent 2 def", false),
            ],
            files: (
                vec![item(0, "Renamed b.txt → c.txt", true)],
                "1 file changed".to_owned(),
            ),
            buttons: vec![
                Button {
                    id: id::OPEN_IN_EDITOR,
                    label: "Open in Editor".to_owned(),
                    on: None,
                    disabled: false,
                    bounds: None,
                },
                Button {
                    id: id::CASE,
                    label: "Ignore case".to_owned(),
                    on: Some(true),
                    disabled: false,
                    bounds: None,
                },
            ],
            compare: (Some(Compare::Message("Reading…".to_owned())), None),
            message: None,
            focus: id::row(0),
        }
    }

    fn no_runs(_: NodeId) -> &'static [NodeId] {
        &[]
    }

    fn tree(scene: &GitScene) -> Tree {
        Tree::new(update(build(scene, &no_runs), scene.focus, true), true)
    }

    #[test]
    fn ids_decode_to_what_they_name() {
        assert_eq!(id::target(id::side(4)), id::Target::Side(4));
        assert_eq!(id::target(id::row(70_000)), id::Target::Row(70_000));
        assert_eq!(id::target(id::file(2)), id::Target::File(2));
        assert_eq!(id::target(id::parent(1)), id::Target::Parent(1));
        assert_eq!(id::target(id::line(3)), id::Target::Other);
        assert_eq!(
            id::target(crate::access::id::run(id::COMPARE_FIELD, 3, 9)),
            id::Target::Run
        );
        assert_eq!(id::target(id::CASE), id::Target::Fixed(id::CASE));
    }

    #[test]
    fn the_panes_are_a_tree_lists_and_buttons() {
        let t = tree(&scene());
        let root = t.state().root();
        let get = |n| find(root, n).expect("the node");
        let side = get(id::side(2));
        assert_eq!(side.role(), Role::TreeItem);
        assert_eq!(side.level(), Some(2));
        assert_eq!(side.is_selected(), Some(true));
        assert_eq!(get(id::side(1)).data().is_expanded(), Some(true));
        let row = get(id::row(1));
        assert_eq!(row.role(), Role::ListBoxOption);
        assert_eq!(row.value().as_deref(), Some("Fix, Ada, 3 days ago"));
        assert_eq!(row.position_in_set(), Some(2));
        assert_eq!(get(id::CASE).toggled(), Some(Toggled::True));
        assert_eq!(get(id::OPEN_IN_EDITOR).toggled(), None);
        assert_eq!(get(id::parent(0)).role(), Role::RadioButton);
        assert_eq!(
            t.state().focus().map(|f| f.locate().0),
            Some(id::row(0)),
            "the selected commit has the keyboard"
        );
    }

    #[test]
    fn before_the_repository_opens_only_the_note_is_read() {
        let mut s = scene();
        s.note = Some("Opening the repository…".to_owned());
        s.focus = id::NOTE;
        let t = tree(&s);
        let root = t.state().root();
        assert!(find(root, id::NOTE).is_some());
        assert!(find(root, id::HISTORY).is_none());
    }
}
