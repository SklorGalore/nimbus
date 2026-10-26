//! The git window's side of its accessibility tree: what each pane shows,
//! gathered into a [`GitScene`] for `crate::access::git` to describe, and
//! what a screen reader asks the window to do, run through the same
//! methods a click or a key uses.
//!
//! Where things are on screen comes from the hit areas the last frame
//! recorded (see `pane.rs`), so the tree and the mouse always agree; a line
//! that is scrolled away has no rectangle and is still read.

use std::fmt::Write as _;

use accesskit::{Action, ActionData, ActionRequest, NodeId};
use nimbus_core::Text as _;
use nimbus_core::wrap::{WrapCache, WrapView};
use nimbus_render::Gpu;

use super::draw::Outcome;
use super::{DiffPane, Focus, GitWindow, Hit, InfoLine, Selected, SideItem, lock, whole};
use crate::access::git::{Button, Compare, GitScene, HistoryList, Item, SideRow, id};
use crate::access::text::{Frame, Geometry, TextWindow};
use crate::access::{Bounds, Field};
use crate::compare::Toggle;
use crate::git::{Kind, RefKind};

/// History rows described either side of the selected one.
const HISTORY_WINDOW: usize = 200;

/// The text fields as the window last described them: the compare's new
/// side and the commit message box, each with its text window and the
/// rows of its lines, and what each was built for.
#[derive(Debug, Default)]
pub struct CompareText {
    window: Option<TextWindow>,
    rows: WrapCache,
    /// The texts' token the window was built for; another diff starts it
    /// over.
    key: Option<u64>,
    message: Option<TextWindow>,
    message_rows: WrapCache,
}

impl CompareText {
    fn window(&mut self) -> &mut TextWindow {
        self.window
            .get_or_insert_with(|| TextWindow::new(id::COMPARE_FIELD))
    }

    /// Forget what was sent: the next update describes the text again.
    pub fn reset(&mut self) {
        self.window().reset();
        self.key = None;
        if let Some(m) = self.message.as_mut() {
            m.reset();
        }
    }

    /// The runs of text field `node`.
    #[must_use]
    pub fn runs(&self, node: NodeId) -> &[NodeId] {
        let win = match node {
            id::COMPARE => self.window.as_ref(),
            id::MESSAGE_BOX => self.message.as_ref(),
            _ => None,
        };
        win.map_or(&[], TextWindow::children)
    }

    /// The text window of field `field`.
    fn field(&self, field: u8) -> Option<&TextWindow> {
        match field {
            id::COMPARE_FIELD => self.window.as_ref(),
            id::MESSAGE_FIELD => self.message.as_ref(),
            _ => None,
        }
    }
}

/// How a kind of change reads.
fn kind_word(kind: Kind) -> &'static str {
    match kind {
        Kind::Unmodified => "Unchanged",
        Kind::Modified => "Modified",
        Kind::TypeChanged => "Type changed",
        Kind::Added => "Added",
        Kind::Deleted => "Deleted",
        Kind::Renamed => "Renamed",
        Kind::Copied => "Copied",
        Kind::Conflicted => "Conflicted",
        Kind::Untracked => "Untracked",
    }
}

impl GitWindow {
    /// Where the last frame drew `hit`.
    fn hit_rect(&self, hit: Hit) -> Option<Bounds> {
        self.hits.iter().find(|(_, h)| *h == hit).map(|(r, _)| *r)
    }

    /// Gather what the tree shows. `gpu` gives the window's size and font
    /// (`None` in tests: nothing then has a rectangle). The compare's
    /// changed runs go to `out`.
    pub fn access_scene(
        &mut self,
        gpu: Option<&Gpu>,
        scale: f32,
        tab_width: usize,
        text: &mut CompareText,
        out: &mut Vec<(NodeId, accesskit::Node)>,
    ) -> GitScene {
        let g = gpu.map(|gpu| self.geo(gpu, scale));
        let window_size = g.map_or([0.0; 2], |g| [g.w, g.bottom + g.lh]);
        let pane = |x: (f32, f32), top: f32, bottom: f32| [x.0, top, x.1 - x.0, bottom - top];
        let header = (
            self.header_text(),
            g.map_or([0.0; 4], |g| [0.0, 0.0, g.w, g.header]),
        );
        let note = self.repo.is_none().then(|| {
            self.error
                .clone()
                .unwrap_or_else(|| "Opening the repository…".to_owned())
        });
        let side = self.access_side();
        let history = self.access_history();
        let (details, parents, files) = self.access_details();
        let buttons = self.access_buttons();
        let compare = self.access_compare(gpu, tab_width, text, out);
        let staging = self.access_staging();
        let message_box = self.access_message(gpu, tab_width, text, out);
        let focus = if note.is_some() {
            id::NOTE
        } else {
            match self.focus {
                Focus::Sidebar => side
                    .iter()
                    .find(|r| r.selected)
                    .map_or(id::SIDEBAR, |r| id::side(r.index)),
                Focus::History => self
                    .selected_row
                    .filter(|r| history.rows.iter().any(|i| i.index == *r))
                    .map_or(id::HISTORY, id::row),
                Focus::Files => self.file_sel.map_or(id::FILES, id::file),
                Focus::Diff => id::COMPARE,
                Focus::Message if message_box.is_some() => id::MESSAGE_BOX,
                Focus::Message => id::FILES,
            }
        };
        GitScene {
            title: self.title(),
            size: window_size,
            header,
            note,
            side: (side, g.map(|g| pane(g.side, g.header, g.bottom))),
            history: (history, g.map(|g| pane(g.history, g.header, g.bottom))),
            details: (details, g.map(|g| pane(g.details, g.header, g.info_bottom))),
            parents,
            files,
            buttons,
            staging,
            message_box,
            picker: None,
            compare: (compare, g.map(|g| pane(g.details, g.bar_bottom, g.bottom))),
            message: None,
            focus,
        }
    }

    /// The header as one line: the repository, its branch, the count.
    fn header_text(&self) -> String {
        let mut parts = Vec::with_capacity(3);
        if let Some(repo) = &self.repo {
            parts.push(repo.name());
        }
        if let Some(s) = &self.status {
            let mut b = s
                .branch
                .clone()
                .unwrap_or_else(|| "detached HEAD".to_owned());
            if let Some((ahead, behind)) = s.ahead_behind
                && (ahead, behind) != (0, 0)
            {
                let _ = write!(b, ", {ahead} ahead, {behind} behind");
            }
            if let Some(u) = &s.upstream {
                let _ = write!(b, ", tracking {u}");
            }
            parts.push(b);
        }
        parts.push(self.history_status());
        parts.retain(|p| !p.is_empty());
        parts.join(", ")
    }

    /// "1,234 commits", "Reading history…", or why it could not be read.
    fn history_status(&self) -> String {
        let Some(c) = &self.current else {
            return String::new();
        };
        let h = lock(&c.history);
        let n = h.len();
        match &h.finished {
            Some(Ok(())) if c.preview || self.next.is_some() => "Reading history…".to_owned(),
            None => "Reading history…".to_owned(),
            Some(Ok(())) if n == 0 => "No commits yet".to_owned(),
            Some(Ok(())) => format!("{} commit{}", n, if n == 1 { "" } else { "s" }),
            Some(Err(e)) => format!("git log: {e}"),
        }
    }

    fn access_side(&self) -> Vec<SideRow> {
        let selected = self.side_selected();
        let head = self.status.as_ref().and_then(|s| s.branch.as_deref());
        self.side_items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let (name, level, expanded) = match item {
                    SideItem::Section(title, n) => (
                        (*title).to_owned(),
                        1,
                        Some(!self.collapsed.get(*n).copied().unwrap_or(false)),
                    ),
                    SideItem::Changes => {
                        let n = self.status.as_ref().map_or(0, |s| s.entries.len());
                        (format!("Changes, {n}"), 1, None)
                    }
                    SideItem::Ref(r) => {
                        let name = self.refs.get(*r).map_or_else(String::new, |rf| {
                            if rf.kind == RefKind::Branch && head == Some(rf.name.as_str()) {
                                format!("{}, current branch", rf.name)
                            } else {
                                rf.name.clone()
                            }
                        });
                        (name, 2, None)
                    }
                    SideItem::Stash(s) => {
                        let name = self
                            .stashes
                            .get(*s)
                            .map_or_else(String::new, |st| format!("{}, {}", st.name, st.subject));
                        (name, 2, None)
                    }
                    SideItem::Note(n) => ((*n).to_owned(), 2, None),
                };
                SideRow {
                    index: i,
                    name,
                    level,
                    expanded,
                    selected: selected == Some(i),
                    bounds: self.hit_rect(Hit::Side(i)),
                }
            })
            .collect()
    }

    /// The history rows within [`HISTORY_WINDOW`] of the selected one, or
    /// of the top of the pane.
    fn access_history(&mut self) -> HistoryList {
        let status = self.history_status();
        let Some(c) = &self.current else {
            return HistoryList {
                rows: Vec::new(),
                total: 0,
                status,
            };
        };
        let h = lock(&c.history);
        let len = h.len();
        let centre = self.selected_row.unwrap_or_else(|| whole(self.scroll));
        let from = centre.saturating_sub(HISTORY_WINDOW);
        let to = (centre + HISTORY_WINDOW + 1).min(len);
        let head = self.status.as_ref().and_then(|s| s.head.clone());
        let mut ago = String::new();
        let rows = (from..to)
            .map(|i| {
                let oid = h.oid(i);
                let mut label = h.subject(i).to_owned();
                if let Some(refs) = self.labels.get(&oid) {
                    let names: Vec<&str> = refs
                        .iter()
                        .filter_map(|&r| self.refs.get(r).map(|rf| rf.name.as_str()))
                        .collect();
                    if !names.is_empty() {
                        label.push_str(", ");
                        label.push_str(&names.join(", "));
                    }
                }
                if head.as_deref() == Some(oid.as_str()) {
                    label.push_str(", HEAD");
                }
                if h.parents(i).len() > 1 {
                    label.push_str(", merge");
                }
                crate::git::ago(&mut ago, h.time(i), self.now);
                label.push_str(", ");
                label.push_str(h.author(i));
                label.push_str(", ");
                label.push_str(&ago);
                Item {
                    index: i,
                    label,
                    selected: self.selected_row == Some(i),
                    bounds: None,
                }
            })
            .collect::<Vec<_>>();
        drop(h);
        let rows = rows
            .into_iter()
            .map(|mut r| {
                r.bounds = self.hit_rect(Hit::Row(r.index));
                r
            })
            .collect();
        HistoryList {
            rows,
            total: len,
            status,
        }
    }

    /// The details' text lines, a merge's parents, and the files.
    fn access_details(&self) -> (Vec<String>, Vec<Item>, (Vec<Item>, String)) {
        let mut lines = Vec::with_capacity(self.info.len());
        for line in &self.info {
            match line {
                InfoLine::Text(t, _) | InfoLine::Heading(_, t) => lines.push(t.clone()),
                InfoLine::Field(label, value) => lines.push(format!("{label}: {value}")),
                InfoLine::Parents | InfoLine::File(_) | InfoLine::Blank => {}
            }
        }
        let parents = match &self.commit {
            Some(Ok(c)) if c.parents.len() > 1 => c
                .parents
                .iter()
                .enumerate()
                .map(|(k, p)| Item {
                    index: k,
                    label: format!("parent {} {}", k + 1, p.get(..8).unwrap_or("")),
                    selected: k == self.parent,
                    bounds: self.hit_rect(Hit::Parent(k)),
                })
                .collect(),
            _ => Vec::new(),
        };
        let files = self
            .files
            .iter()
            .enumerate()
            .map(|(i, file)| {
                let mut label = format!("{} {}", kind_word(file.kind), file.label);
                match file.staged {
                    Some(true) => label.push_str(", staged"),
                    Some(false) => label.push_str(", not staged"),
                    None => {}
                }
                Item {
                    index: i,
                    label,
                    selected: self.file_sel == Some(i),
                    bounds: self.hit_rect(Hit::File(i)),
                }
            })
            .collect();
        let n = self.files.len();
        let name = match (&self.selected, &self.files_error) {
            (_, Some(e)) => format!("Cannot list the files: {e}"),
            (Selected::Changes, None) => {
                format!("{n} changed file{}", if n == 1 { "" } else { "s" })
            }
            (Selected::Commit(_), None) => {
                format!("{n} file{} changed", if n == 1 { "" } else { "s" })
            }
        };
        (lines, parents, (files, name))
    }

    /// Open in Editor and the compare options.
    fn access_buttons(&self) -> Vec<Button> {
        let unified =
            matches!(&self.diff, Some(DiffPane::Compare { compare, .. }) if compare.unified);
        let ws = match self.normalize.whitespace {
            nimbus_core::Whitespace::Exact => "Spaces: count",
            nimbus_core::Whitespace::Ends => "Spaces: ignore at line ends",
            nimbus_core::Whitespace::All => "Spaces: ignore",
        };
        let options: [(NodeId, &str, Option<bool>, Hit); 5] = [
            (
                id::OPEN_IN_EDITOR,
                "Open in Editor",
                None,
                Hit::OpenInEditor,
            ),
            (
                id::UNIFIED,
                "Unified",
                Some(unified),
                Hit::Toggle(Toggle::Unified),
            ),
            (
                id::BLANK_LINES,
                "Ignore blank lines",
                Some(self.normalize.blank_lines),
                Hit::Toggle(Toggle::BlankLines),
            ),
            (
                id::CASE,
                "Ignore case",
                Some(self.normalize.case),
                Hit::Toggle(Toggle::Case),
            ),
            // Three states, so a button that says which, not a toggle.
            (id::WHITESPACE, ws, None, Hit::Toggle(Toggle::Whitespace)),
        ];
        options
            .into_iter()
            .map(|(node, label, on, hit)| Button {
                id: node,
                label: label.to_owned(),
                on,
                disabled: hit == Hit::OpenInEditor && self.file_sel.is_none(),
                bounds: self.hit_rect(hit),
            })
            .collect()
    }

    /// The staging and commit buttons the last frame drew, in order. A
    /// file's own button says which file.
    fn access_staging(&self) -> Vec<Button> {
        self.drawn_buttons
            .iter()
            .filter(|(hit, ..)| matches!(hit, Hit::Git(_) | Hit::FileGit(..) | Hit::StopCommit))
            .enumerate()
            .map(|(n, &(hit, label, on, usable))| {
                let label = match hit {
                    Hit::FileGit(i, _) => self
                        .files
                        .get(i)
                        .map_or_else(|| label.to_owned(), |f| format!("{label} {}", f.label)),
                    _ => label.to_owned(),
                };
                let toggle = matches!(hit, Hit::Git(crate::input::GitCmd::ToggleAmend));
                Button {
                    id: id::button(n),
                    label,
                    on: toggle.then_some(on),
                    disabled: !usable,
                    bounds: self.hit_rect(hit),
                }
            })
            .collect()
    }

    /// The hit of staging button `n`, as [`Self::access_staging`] numbers
    /// them.
    fn staging_hit(&self, n: usize) -> Option<(Hit, bool)> {
        self.drawn_buttons
            .iter()
            .filter(|(hit, ..)| matches!(hit, Hit::Git(_) | Hit::FileGit(..) | Hit::StopCommit))
            .nth(n)
            .map(|&(hit, _, _, usable)| (hit, usable))
    }

    /// The commit message box, while it is drawn (Changes is shown).
    fn access_message(
        &mut self,
        gpu: Option<&Gpu>,
        tab_width: usize,
        text: &mut CompareText,
        out: &mut Vec<(NodeId, accesskit::Node)>,
    ) -> Option<Field> {
        if self.selected != Selected::Changes || self.hit_rect(Hit::Message).is_none() {
            if let Some(m) = text.message.as_mut() {
                m.reset();
            }
            return None;
        }
        let bounds = self.hit_rect(Hit::Message)?;
        let geometry = gpu.map_or(
            Geometry {
                cell: 1.0,
                line_height: 1.0,
                tab_width,
            },
            |g| Geometry {
                cell: g.metrics().cell_width,
                line_height: g.metrics().line_height,
                tab_width,
            },
        );
        let m = &mut self.message;
        let sel = m.carets.selection(&m.doc.buffer);
        let win = text
            .message
            .get_or_insert_with(|| TextWindow::new(id::MESSAGE_FIELD));
        let mut w = WrapView::new(&mut text.message_rows, None, tab_width);
        let frame = Frame {
            top: None,
            geometry,
            long_scroll: 0.0,
        };
        let placed = win.update(&m.doc.buffer, &mut w, sel, frame, out);
        let label = if m.amend {
            "Commit message, amending the last commit"
        } else {
            "Commit message"
        };
        Some(Field {
            label: label.to_owned(),
            placeholder: Some("Describe the change".to_owned()),
            bounds,
            origin: [f64::from(bounds[0]), f64::from(bounds[1])],
            selection: placed.selection,
            read_only: false,
        })
    }

    /// The compare: the new side's text, or the message shown instead.
    fn access_compare(
        &mut self,
        gpu: Option<&Gpu>,
        tab_width: usize,
        text: &mut CompareText,
        out: &mut Vec<(NodeId, accesskit::Node)>,
    ) -> Option<Compare> {
        let layout = self.compare_layout;
        match &mut self.diff {
            None => {
                text.reset();
                None
            }
            Some(DiffPane::Message(m)) => {
                text.reset();
                Some(Compare::Message(m.clone()))
            }
            Some(DiffPane::Compare {
                doc,
                carets,
                compare,
                ..
            }) => {
                if text.key != Some(self.texts_token) {
                    text.window().reset();
                    text.key = Some(self.texts_token);
                }
                let geometry = gpu.map_or(
                    Geometry {
                        cell: 1.0,
                        line_height: 1.0,
                        tab_width,
                    },
                    |g| Geometry {
                        cell: g.metrics().cell_width,
                        line_height: g.metrics().line_height,
                        tab_width,
                    },
                );
                let sel = carets.selection(&doc.buffer);
                let CompareText { window, rows, .. } = text;
                let win = window.get_or_insert_with(|| TextWindow::new(id::COMPARE_FIELD));
                let mut w = WrapView::new(rows, None, tab_width);
                let frame = Frame {
                    top: None,
                    geometry,
                    long_scroll: 0.0,
                };
                let placed = win.update(&doc.buffer, &mut w, sel, frame, out);
                // The runs are placed as if the new side were drawn alone
                // from the pane's top-left corner: close enough for a
                // reader's highlight, not the compare's exact columns.
                let bounds = layout.map_or([0.0; 4], |l| {
                    [
                        l.text_left,
                        l.top,
                        l.text_right() - l.text_left,
                        l.bottom - l.top,
                    ]
                });
                let name = doc.display_name();
                let status = compare.status("").to_owned();
                let label = if doc.buffer.len_bytes() == 0 {
                    format!("{name}, now empty")
                } else {
                    name
                };
                Some(Compare::Text(
                    Field {
                        label,
                        placeholder: None,
                        bounds,
                        origin: [f64::from(bounds[0]), f64::from(bounds[1])],
                        selection: placed.selection,
                        read_only: true,
                    },
                    status,
                ))
            }
        }
    }

    /// Act on a screen reader's request with the code a click or a key
    /// runs.
    pub fn access_action(&mut self, req: &ActionRequest, text: &CompareText) -> Outcome {
        let action = req.action;
        let click = matches!(action, Action::Click);
        match id::target(req.target_node) {
            id::Target::Side(i) => {
                let section = matches!(self.side_items.get(i), Some(SideItem::Section(..)));
                if section {
                    let open = matches!(self.side_items.get(i), Some(SideItem::Section(_, n))
                        if !self.collapsed.get(*n).copied().unwrap_or(false));
                    let flip = match action {
                        Action::Click => true,
                        Action::Expand => !open,
                        Action::Collapse => open,
                        _ => false,
                    };
                    self.focus = Focus::Sidebar;
                    if flip {
                        self.side_click(i);
                    }
                } else if click || action == Action::Focus {
                    self.focus = Focus::Sidebar;
                    self.side_click(i);
                }
            }
            id::Target::Row(i) if click || action == Action::Focus => {
                self.focus = Focus::History;
                self.select_row(i, true);
            }
            id::Target::File(i) if click || action == Action::Focus => {
                self.focus = Focus::Files;
                self.select_file(i);
                self.reveal_file(i);
            }
            id::Target::Parent(k) if click => self.choose_parent(k),
            id::Target::Button(n) if click => {
                // Pressed where it is drawn, so it runs what a click runs.
                let Some((hit, true)) = self.staging_hit(n) else {
                    return Outcome::Done(false);
                };
                let Some([bx, by, bw, bh]) = self.hit_rect(hit) else {
                    return Outcome::Done(false);
                };
                return self.click(bx + bw / 2.0, by + bh / 2.0, false, false);
            }
            id::Target::Fixed(id::MESSAGE_BOX) | id::Target::Run(id::MESSAGE_FIELD) => {
                return self.access_message_action(req, text);
            }
            id::Target::Fixed(node) if click => match node {
                id::OPEN_IN_EDITOR => {
                    if let Some(p) = self.selected_path() {
                        return Outcome::OpenInEditor(p);
                    }
                }
                id::UNIFIED => self.toggle(Toggle::Unified),
                id::BLANK_LINES => self.toggle(Toggle::BlankLines),
                id::CASE => self.toggle(Toggle::Case),
                id::WHITESPACE => self.toggle(Toggle::Whitespace),
                _ => return Outcome::Done(false),
            },
            id::Target::Run(id::COMPARE_FIELD) | id::Target::Fixed(id::COMPARE)
                if action == Action::SetTextSelection =>
            {
                let Some(ActionData::SetTextSelection(sel)) = &req.data else {
                    return Outcome::Done(false);
                };
                let Some(win) = text.field(id::COMPARE_FIELD) else {
                    return Outcome::Done(false);
                };
                let (Some(anchor), Some(head)) = (win.byte_of(sel.anchor), win.byte_of(sel.focus))
                else {
                    return Outcome::Done(false);
                };
                if let Some(DiffPane::Compare { doc, carets, .. }) = &mut self.diff {
                    crate::document::Editor::new(doc, carets).select_span(anchor, head);
                }
            }
            _ => return Outcome::Done(false),
        }
        Outcome::Done(true)
    }

    /// A request on the commit message box: focus it, type into it, or
    /// move its caret.
    fn access_message_action(&mut self, req: &ActionRequest, text: &CompareText) -> Outcome {
        self.focus = Focus::Message;
        match (req.action, &req.data) {
            (Action::ReplaceSelectedText, Some(ActionData::Value(t))) => {
                let tab = self.tab_width;
                self.message
                    .command(crate::input::Command::Insert(t), None, tab);
            }
            (Action::SetTextSelection, Some(ActionData::SetTextSelection(sel))) => {
                let Some(win) = text.field(id::MESSAGE_FIELD) else {
                    return Outcome::Done(false);
                };
                let (Some(anchor), Some(head)) = (win.byte_of(sel.anchor), win.byte_of(sel.focus))
                else {
                    return Outcome::Done(false);
                };
                let m = &mut self.message;
                crate::document::Editor::new(&mut m.doc, &mut m.carets).select_span(anchor, head);
            }
            _ => {}
        }
        Outcome::Done(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{git, update};
    use crate::git::test_repo;
    use accesskit::Role;
    use accesskit_consumer::{NodeRef, Tree};

    fn find(n: NodeRef<'_>, target: NodeId) -> Option<NodeRef<'_>> {
        if n.locate().0 == target {
            return Some(n);
        }
        n.children().find_map(|c| find(c, target))
    }

    #[test]
    fn a_repository_reads_as_a_tree_a_history_and_its_files() {
        let Some(dir) = test_repo("git-access-tree") else {
            return;
        };
        let (mut w, rx) = super::super::tests::window(&dir.0);
        w.headless_load(&rx, None);
        assert_eq!(w.error, None);
        let mut text = CompareText::default();
        let mut runs = Vec::new();
        let scene = w.access_scene(None, 1.0, 4, &mut text, &mut runs);
        let mut nodes = git::build(&scene, &|n| text.runs(n));
        nodes.extend(runs);
        let tree = Tree::new(update(nodes, scene.focus, true), true);
        let root = tree.state().root();
        // Four commits, the merge selected and with the keyboard.
        let history = find(root, id::HISTORY).expect("the history");
        assert_eq!(history.role(), Role::ListBox);
        assert_eq!(history.children().count(), 4);
        let first = find(root, id::row(0)).expect("the merge");
        assert_eq!(first.is_selected(), Some(true));
        assert!(
            first.value().is_some_and(|v| v.contains("merge")),
            "a merge says so: {:?}",
            first.value()
        );
        assert_eq!(tree.state().focus().map(|f| f.locate().0), Some(id::row(0)));
        // The merge's rename, against its first parent.
        let file = find(root, id::file(0)).expect("the file");
        assert_eq!(file.value().as_deref(), Some("Renamed b.txt → c.txt"));
        assert!(find(root, id::parent(1)).is_some(), "both parents offered");
        // The sidebar's Changes line comes first.
        let changes = find(root, id::side(0)).expect("Changes");
        assert!(changes.label().is_some_and(|l| l.starts_with("Changes")));
    }

    #[test]
    fn a_reader_selects_a_commit_and_flips_an_option() {
        let Some(dir) = test_repo("git-access-actions") else {
            return;
        };
        let (mut w, rx) = super::super::tests::window(&dir.0);
        w.headless_load(&rx, None);
        let text = CompareText::default();
        let req = |node, action| ActionRequest {
            action,
            target_tree: accesskit::TreeId::ROOT,
            target_node: node,
            data: None,
        };
        w.access_action(&req(id::row(2), Action::Click), &text);
        assert_eq!(w.selected_row, Some(2));
        assert_eq!(w.focus, Focus::History);
        let case = w.normalize.case;
        w.access_action(&req(id::CASE, Action::Click), &text);
        assert_eq!(w.normalize.case, !case);
        w.access_action(&req(id::side(0), Action::Click), &text);
        assert_eq!(w.selected, Selected::Changes);
        assert_eq!(w.focus, Focus::Sidebar);
    }

    #[test]
    fn changes_read_their_message_box_and_a_reader_stages_a_file() {
        let Some(dir) = test_repo("git-access-staging") else {
            return;
        };
        let (mut w, rx) = super::super::tests::window(&dir.0);
        w.headless_load(&rx, Some("changes"));
        let Ok(font) = nimbus_render::Font::bundled() else {
            return;
        };
        // A frame on the CPU painter records what was drawn where.
        let mut gpu = Gpu::compat_headless(font, 14.0, (1400, 900));
        let (mut theme, _) = crate::theme::Theme::load("dark");
        w.draw(&mut gpu, &mut theme, 1.0, None);
        let mut text = CompareText::default();
        let mut runs = Vec::new();
        let scene = w.access_scene(Some(&gpu), 1.0, 4, &mut text, &mut runs);
        assert!(scene.message_box.is_some(), "Changes shows the message box");
        let stage = scene
            .staging
            .iter()
            .find(|b| b.label == "Stage new.txt")
            .expect("new.txt's own Stage button");
        assert!(!stage.disabled);
        let labels: Vec<&str> = scene.staging.iter().map(|b| b.label.as_str()).collect();
        assert!(labels.contains(&"Commit"), "{labels:?}");
        let mut nodes = git::build(&scene, &|n| text.runs(n));
        nodes.extend(runs);
        let tree = Tree::new(update(nodes, scene.focus, true), true);
        let root = tree.state().root();
        let field = find(root, id::MESSAGE_BOX).expect("the message box");
        assert_eq!(field.role(), Role::MultilineTextInput);
        // Pressing it stages the file, as a click does.
        let req = ActionRequest {
            action: Action::Click,
            target_tree: accesskit::TreeId::ROOT,
            target_node: stage.id,
            data: None,
        };
        w.access_action(&req, &text);
        super::super::tests::pump(&mut w, &rx, |w| {
            w.files
                .iter()
                .any(|f| f.path == "new.txt" && f.staged == Some(true))
        });
    }
}
