//! The project tree shown in the sidebar.
//!
//! Nodes live in one `Vec` and refer to each other by index (an *arena*):
//! a parent holds its children's indices, a child its parent's. That avoids
//! the shared, mutable pointers a tree of `Box`es or `Rc`s would need, which
//! Rust's borrow rules make awkward.
//!
//! A folder's contents load when it is first expanded, never up front, so a
//! huge repository costs nothing until you open its folders. Listing runs on
//! a worker ([`list_dir`]); the tree records that it is waiting and fills in
//! the children when the result arrives ([`Tree::loaded`]). Listing again
//! after a change on disk keeps the nodes that are still there, so expanded
//! folders stay expanded.
//!
//! A workspace can hold several root folders (M19). The first root is
//! node [`ROOT`]; each further root is a node of its own with no parent.
//! With one root the tree looks as it always did: the root's children are
//! the top rows. With more, each root is a row at the top, and its
//! contents are drawn one step in, so the folders read as separate
//! projects. Every root keeps its own `.gitignore`, because listing walks
//! up from the folder being listed, never from another root.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Index of a node in the tree's arena.
pub type NodeId = usize;

/// One file or folder.
#[derive(Debug, Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts about a node, not a state machine"
)]
pub struct Node {
    /// File name, for display.
    pub name: String,
    /// Full path.
    pub path: PathBuf,
    /// A folder.
    pub is_dir: bool,
    /// Steps below the root (the root's children are at 0).
    pub depth: usize,
    /// The folder's children, sorted; `None` until listed.
    pub children: Option<Vec<NodeId>>,
    /// Shown open.
    pub expanded: bool,
    /// A listing has been asked for and not arrived.
    pub loading: bool,
    /// The folder changed while its listing ran: list it again when the
    /// answer arrives, which may predate the change.
    relist: bool,
    parent: Option<NodeId>,
    /// Removed from the tree; the slot is reused.
    dead: bool,
}

/// One entry of a folder listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// File name.
    pub name: String,
    /// A folder.
    pub is_dir: bool,
}

/// Folders first, then case-insensitive name order (ties broken by the
/// exact name, so `a` and `A` keep a stable order).
fn order(a: &Entry, b: &Entry) -> Ordering {
    b.is_dir
        .cmp(&a.is_dir)
        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        .then_with(|| a.name.cmp(&b.name))
}

/// List one folder, sorted for display. `.gitignore`d entries are left out
/// unless `show_ignored`; `.git` itself always is. Dotfiles are shown.
///
/// # Errors
///
/// Returns the error reading the folder itself; unreadable entries inside
/// it are skipped.
pub fn list_dir(dir: &Path, show_ignored: bool) -> std::io::Result<Vec<Entry>> {
    // Fail early, with a real error, if the folder itself cannot be read.
    std::fs::read_dir(dir)?;
    let walk = ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false)
        .git_ignore(!show_ignored)
        .git_exclude(!show_ignored)
        .git_global(!show_ignored)
        .ignore(!show_ignored)
        .parents(true)
        .build();
    let mut entries: Vec<Entry> = walk
        .filter_map(Result::ok)
        .filter(|e| e.depth() == 1)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                return None;
            }
            // Follow symlinks for the kind, so a linked folder expands.
            let is_dir = e.file_type().is_some_and(|t| t.is_dir())
                || (e.path_is_symlink() && e.path().is_dir());
            Some(Entry { name, is_dir })
        })
        .collect();
    entries.sort_by(order);
    Ok(entries)
}

/// One visible row: a node and how deep it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// The node shown.
    pub node: NodeId,
    /// Indentation level.
    pub depth: usize,
}

/// A request for a worker: list `path` for node `node`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListRequest {
    /// The folder's node.
    pub node: NodeId,
    /// The folder.
    pub path: PathBuf,
    /// Matches the tree's generation when the answer arrives, or it is
    /// stale (the tree was rebuilt meanwhile).
    pub generation: u64,
    /// Include `.gitignore`d entries.
    pub show_ignored: bool,
}

/// The project tree.
#[derive(Debug)]
pub struct Tree {
    root: PathBuf,
    /// Every root folder's node, the first being [`ROOT`].
    roots: Vec<NodeId>,
    nodes: Vec<Node>,
    free: Vec<NodeId>,
    /// What is on screen, top to bottom, rebuilt when the shape changes.
    rows: Vec<Row>,
    /// Show `.gitignore`d files.
    pub show_ignored: bool,
    generation: u64,
    /// Expand towards this file as its folders load.
    reveal: Option<PathBuf>,
    /// The row picked by the keyboard or a click.
    pub selected: Option<NodeId>,
    /// Folders to open as soon as they are listed: a restored session's
    /// expanded folders, which load one level at a time.
    expand_later: HashSet<PathBuf>,
    /// Select this path when a listing shows it: a file just created or
    /// renamed, which the tree learns about from the disk.
    select_later: Option<PathBuf>,
}

/// The root node's index.
pub const ROOT: NodeId = 0;

impl Tree {
    /// A tree for `root`, with nothing listed yet.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        let node = Node {
            name: folder_name(&root),
            path: root.clone(),
            is_dir: true,
            depth: 0,
            children: None,
            expanded: true,
            loading: false,
            relist: false,
            parent: None,
            dead: false,
        };
        Self {
            root,
            roots: vec![ROOT],
            nodes: vec![node],
            free: Vec::new(),
            rows: Vec::new(),
            show_ignored: false,
            generation: 0,
            reveal: None,
            selected: None,
            expand_later: HashSet::new(),
            select_later: None,
        }
    }

    /// The folder the tree shows: the first root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every root folder, the first one first.
    #[must_use]
    pub fn roots(&self) -> Vec<PathBuf> {
        self.roots
            .iter()
            .map(|&id| self.nodes[id].path.clone())
            .collect()
    }

    /// The root nodes, in order.
    #[must_use]
    pub fn root_ids(&self) -> &[NodeId] {
        &self.roots
    }

    /// Whether `id` is one of the root folders.
    #[must_use]
    pub fn is_root(&self, id: NodeId) -> bool {
        self.roots.contains(&id)
    }

    /// More than one root: each root is a row of its own.
    #[must_use]
    pub fn multi(&self) -> bool {
        self.roots.len() > 1
    }

    /// The root `path` is in, if any.
    #[must_use]
    pub fn root_of(&self, path: &Path) -> Option<NodeId> {
        self.roots
            .iter()
            .copied()
            .find(|&r| path.starts_with(&self.nodes[r].path))
    }

    /// Add `path` (canonical) as another root, after the others. It starts
    /// closed, as any folder does, so a second root costs nothing until it
    /// is opened; [`Tree::expand_later`] opens a restored one. A root inside
    /// another, or holding one, is refused, since the outer one already
    /// shows it and two nodes for one folder would disagree.
    ///
    /// # Errors
    ///
    /// Returns why it cannot be added, for the message line.
    pub fn add_root(&mut self, path: PathBuf) -> Result<(), String> {
        for &r in &self.roots {
            let have = &self.nodes[r].path;
            if *have == path {
                return Err(format!("{} is already in the workspace", path.display()));
            }
            if path.starts_with(have) {
                return Err(format!(
                    "{} is inside {}, which is already in the workspace",
                    path.display(),
                    have.display()
                ));
            }
            if have.starts_with(&path) {
                return Err(format!(
                    "{} holds {}, which is already in the workspace; remove that first",
                    path.display(),
                    have.display()
                ));
            }
        }
        let id = self.alloc(Node {
            name: folder_name(&path),
            path,
            is_dir: true,
            depth: 0,
            children: None,
            expanded: false,
            loading: false,
            relist: false,
            parent: None,
            dead: false,
        });
        self.roots.push(id);
        self.rebuild();
        Ok(())
    }

    /// Take root `id` out of the workspace (not the first, which the
    /// session is named after). Returns whether it was removed.
    pub fn remove_root(&mut self, id: NodeId) -> bool {
        if id == ROOT || !self.roots.contains(&id) {
            return false;
        }
        self.roots.retain(|&r| r != id);
        if self.selected.is_some_and(|s| {
            self.node(s)
                .is_none_or(|n| n.path.starts_with(&self.nodes[id].path))
        }) {
            self.selected = None;
        }
        self.remove(id);
        self.rebuild();
        true
    }

    /// A node.
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id).filter(|n| !n.dead)
    }

    /// The visible rows, root's children first.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Ask for `id`'s listing, unless it is already on the way.
    fn request(&mut self, id: NodeId, out: &mut Vec<ListRequest>) {
        let Some(node) = self.nodes.get_mut(id) else {
            return;
        };
        if node.dead || !node.is_dir || node.loading {
            return;
        }
        node.loading = true;
        out.push(ListRequest {
            node: id,
            path: node.path.clone(),
            generation: self.generation,
            show_ignored: self.show_ignored,
        });
    }

    /// Listings to start now: each root's, the first time.
    pub fn start(&mut self) -> Vec<ListRequest> {
        let mut out = Vec::new();
        for r in self.roots.clone() {
            if self.nodes[r].children.is_none() && self.nodes[r].expanded {
                self.request(r, &mut out);
            }
        }
        out
    }

    /// Show or hide ignored files: everything is listed again.
    pub fn set_show_ignored(&mut self, show: bool) -> Vec<ListRequest> {
        self.show_ignored = show;
        self.relist_all()
    }

    /// List every loaded folder again (keeping expansion), for example
    /// after the ignore rules changed.
    pub fn relist_all(&mut self) -> Vec<ListRequest> {
        self.generation += 1;
        let mut out = Vec::new();
        // Folders already listed, and ones whose first listing is still
        // on the way: that answer will be stale, so ask again.
        let ids: Vec<NodeId> = (0..self.nodes.len())
            .filter(|&i| {
                let n = &self.nodes[i];
                !n.dead && (n.children.is_some() || n.loading)
            })
            .collect();
        for id in ids {
            self.nodes[id].loading = false;
            self.nodes[id].relist = false;
            self.request(id, &mut out);
        }
        out
    }

    /// Open or close folder `id`, loading its contents the first time.
    pub fn toggle(&mut self, id: NodeId) -> Vec<ListRequest> {
        let open = self.node(id).is_some_and(|n| !n.expanded);
        self.set_expanded(id, open)
    }

    /// Open or close folder `id`.
    pub fn set_expanded(&mut self, id: NodeId, open: bool) -> Vec<ListRequest> {
        let mut out = Vec::new();
        let Some(node) = self.nodes.get_mut(id).filter(|n| n.is_dir && !n.dead) else {
            return out;
        };
        node.expanded = open;
        if open && node.children.is_none() {
            self.request(id, &mut out);
        }
        self.rebuild();
        out
    }

    /// Take a folder listing from a worker. Children that are still there
    /// keep their nodes (and so their expansion); new ones are added and
    /// missing ones removed. Returns listings to start next, for a file
    /// being revealed.
    pub fn loaded(&mut self, req: &ListRequest, entries: Vec<Entry>) -> Vec<ListRequest> {
        let fits = req.generation == self.generation
            && self
                .node(req.node)
                .is_some_and(|n| n.path == req.path && n.is_dir);
        if !fits {
            return Vec::new();
        }
        let depth = if self.is_root(req.node) {
            0
        } else {
            self.nodes[req.node].depth + 1
        };
        let old = self.nodes[req.node].children.take().unwrap_or_default();
        // Old children by name and kind, to find the ones still there.
        let mut kept: HashMap<(String, bool), NodeId> = old
            .iter()
            .map(|&id| ((self.nodes[id].name.clone(), self.nodes[id].is_dir), id))
            .collect();
        let mut children = Vec::with_capacity(entries.len());
        for entry in entries {
            let id = match kept.remove(&(entry.name.clone(), entry.is_dir)) {
                Some(id) => id,
                None => self.alloc(Node {
                    path: req.path.join(&entry.name),
                    name: entry.name,
                    is_dir: entry.is_dir,
                    depth,
                    children: None,
                    expanded: false,
                    loading: false,
                    relist: false,
                    parent: Some(req.node),
                    dead: false,
                }),
            };
            children.push(id);
        }
        for id in kept.into_values() {
            self.remove(id);
        }
        let mut out = Vec::new();
        if !self.expand_later.is_empty() {
            for &id in &children {
                let n = &self.nodes[id];
                if n.is_dir && self.expand_later.remove(&n.path) {
                    self.nodes[id].expanded = true;
                    if self.nodes[id].children.is_none() {
                        self.request(id, &mut out);
                    }
                }
            }
            // Ones this folder should have held are gone.
            self.expand_later
                .retain(|p| p.parent() != Some(req.path.as_path()));
        }
        if let Some(want) = &self.select_later
            && let Some(&id) = children.iter().find(|&&c| &self.nodes[c].path == want)
        {
            self.selected = Some(id);
            self.select_later = None;
        }
        let node = &mut self.nodes[req.node];
        node.children = Some(children);
        node.loading = false;
        let again = std::mem::take(&mut node.relist);
        if self.selected.is_some_and(|s| self.node(s).is_none()) {
            self.selected = None;
        }
        if again {
            self.request(req.node, &mut out);
        }
        self.continue_reveal(&mut out);
        self.rebuild();
        out
    }

    /// A listing failed (the folder is gone or unreadable): show it empty.
    pub fn failed(&mut self, req: &ListRequest) {
        if req.generation == self.generation
            && self.node(req.node).is_some_and(|n| n.path == req.path)
        {
            let node = &mut self.nodes[req.node];
            node.loading = false;
            if node.children.is_none() {
                node.children = Some(Vec::new());
            }
            self.rebuild();
        }
    }

    fn alloc(&mut self, node: Node) -> NodeId {
        if let Some(id) = self.free.pop() {
            self.nodes[id] = node;
            id
        } else {
            self.nodes.push(node);
            self.nodes.len() - 1
        }
    }

    /// Remove `id` and everything below it, freeing their slots.
    fn remove(&mut self, id: NodeId) {
        let mut stack = vec![id];
        while let Some(i) = stack.pop() {
            if let Some(children) = self.nodes[i].children.take() {
                stack.extend(children);
            }
            self.nodes[i].dead = true;
            self.free.push(i);
        }
    }

    /// Recompute the visible rows. With several roots, each root is a row
    /// and everything under it is drawn one step in.
    fn rebuild(&mut self) {
        self.rows.clear();
        let multi = self.multi();
        let mut stack: Vec<NodeId> = if multi {
            self.roots.iter().rev().copied().collect()
        } else {
            self.nodes[ROOT]
                .children
                .iter()
                .flatten()
                .rev()
                .copied()
                .collect()
        };
        while let Some(id) = stack.pop() {
            let node = &self.nodes[id];
            let depth = if !multi {
                node.depth
            } else if self.roots.contains(&id) {
                0
            } else {
                node.depth + 1
            };
            self.rows.push(Row { node: id, depth });
            if node.expanded
                && let Some(children) = &node.children
            {
                stack.extend(children.iter().rev().copied());
            }
        }
    }

    /// Loaded folders whose listing may have changed because `path`
    /// changed: its parent, and itself if it is a loaded folder.
    pub fn affected_by(&mut self, path: &Path) -> Vec<ListRequest> {
        let mut out = Vec::new();
        let ids: Vec<NodeId> = (0..self.nodes.len())
            .filter(|&i| {
                let n = &self.nodes[i];
                !n.dead
                    && (n.children.is_some() || n.loading)
                    && (n.path == path || path.parent() == Some(n.path.as_path()))
            })
            .collect();
        for id in ids {
            if self.nodes[id].loading {
                // Its listing may have read the folder before this change.
                self.nodes[id].relist = true;
            } else {
                self.request(id, &mut out);
            }
        }
        out
    }

    /// Open the folders at `paths` (absolute) once their parents are
    /// listed, as a restored session asks. A folder that is no longer
    /// there is never listed, so it is quietly left out.
    /// A root other than the first opens at once (listed by the next
    /// [`Tree::start`]).
    pub fn expand_later(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        for p in paths {
            if let Some(&r) = self.roots.iter().find(|&&r| self.nodes[r].path == p) {
                self.nodes[r].expanded = true;
            } else if self.root_of(&p).is_some() {
                self.expand_later.insert(p);
            }
        }
        self.rebuild();
    }

    /// Select `path` once a listing shows it, and list its folder again now
    /// (after our own change to it, so it need not wait for the watcher).
    pub fn select_when_listed(&mut self, path: &Path) -> Vec<ListRequest> {
        self.select_later = Some(path.to_path_buf());
        self.affected_by(path)
    }

    /// The folders shown open (not the root), and any still waiting to
    /// open, for saving the session: a sidebar never shown keeps the
    /// restored folders for next time.
    #[must_use]
    pub fn expanded_dirs(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|&(i, n)| i != ROOT && !n.dead && n.is_dir && n.expanded)
            .map(|(_, n)| n.path.clone())
            .chain(self.expand_later.iter().cloned())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Folders that are listed: the ones to watch for changes.
    pub fn loaded_dirs(&self) -> impl Iterator<Item = &Path> {
        self.nodes
            .iter()
            .filter(|n| !n.dead && n.children.is_some())
            .map(|n| n.path.as_path())
    }

    /// Expand the folders down to `path` and select it, loading them as
    /// needed. Returns listings to start.
    pub fn reveal(&mut self, path: &Path) -> Vec<ListRequest> {
        if self.root_of(path).is_none() {
            return Vec::new();
        }
        self.reveal = Some(path.to_path_buf());
        let mut out = Vec::new();
        self.continue_reveal(&mut out);
        self.rebuild();
        out
    }

    /// Walk down towards the file being revealed as far as the loaded
    /// folders allow, asking for the next listing where they stop.
    fn continue_reveal(&mut self, out: &mut Vec<ListRequest>) {
        let Some(target) = self.reveal.clone() else {
            return;
        };
        let Some(mut at) = self.root_of(&target) else {
            self.reveal = None;
            return;
        };
        loop {
            if self.nodes[at].path == target {
                self.selected = Some(at);
                self.reveal = None;
                return;
            }
            let Some(children) = self.nodes[at].children.clone() else {
                self.request(at, out);
                return;
            };
            self.nodes[at].expanded = true;
            let next = children
                .into_iter()
                .find(|&c| target.starts_with(&self.nodes[c].path));
            let Some(c) = next else {
                // Not in the listing (ignored, or gone): stop here.
                self.reveal = None;
                return;
            };
            at = c;
        }
    }

    /// The row index of `id`, if visible.
    #[must_use]
    pub fn row_of(&self, id: NodeId) -> Option<usize> {
        self.rows.iter().position(|r| r.node == id)
    }

    /// The parent folder of `id`: not the root when it is the only one,
    /// since it has no row then.
    #[must_use]
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        let multi = self.multi();
        self.node(id)?.parent.filter(|&p| multi || p != ROOT)
    }
}

/// How `path` is shown in a workspace of `roots`: relative to the root it
/// is in, prefixed with that root's folder name when there is more than
/// one root, so `app/src/main.rs` and `lib/src/main.rs` can be told apart.
/// `None` when it is in no root.
#[must_use]
pub fn shown_path(roots: &[PathBuf], path: &Path) -> Option<PathBuf> {
    let root = roots.iter().find(|r| path.starts_with(r))?;
    let rel = path.strip_prefix(root).ok()?;
    Some(if roots.len() > 1 {
        PathBuf::from(folder_name(root)).join(rel)
    } else {
        rel.to_path_buf()
    })
}

/// A folder's name for display: its last component, or the whole path for
/// `/` or a drive.
#[must_use]
pub fn folder_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(list: &[(&str, bool)]) -> Vec<Entry> {
        let mut v: Vec<Entry> = list
            .iter()
            .map(|(n, d)| Entry {
                name: (*n).to_owned(),
                is_dir: *d,
            })
            .collect();
        v.sort_by(order);
        v
    }

    fn names(t: &Tree) -> Vec<String> {
        t.rows()
            .iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), t.node(r.node).unwrap().name))
            .collect()
    }

    #[test]
    fn folders_first_then_case_insensitive() {
        let e = entries(&[
            ("b.rs", false),
            ("Zeta", true),
            ("A.txt", false),
            ("alpha", true),
        ]);
        let n: Vec<&str> = e.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(n, ["alpha", "Zeta", "A.txt", "b.rs"]);
    }

    #[test]
    fn folders_load_on_expand_and_keep_state_when_relisted() {
        let mut t = Tree::new(PathBuf::from("/p"));
        let req = t.start();
        assert_eq!(req.len(), 1);
        assert!(t.start().is_empty(), "already loading");
        t.loaded(&req[0], entries(&[("src", true), ("README", false)]));
        assert_eq!(names(&t), ["src", "README"]);
        let src = t.rows()[0].node;
        let req = t.toggle(src);
        assert_eq!(req[0].path, PathBuf::from("/p/src"));
        assert_eq!(names(&t), ["src", "README"], "loading: no children yet");
        t.loaded(&req[0], entries(&[("main.rs", false)]));
        assert_eq!(names(&t), ["src", "  main.rs", "README"]);

        // The root changes on disk: src stays, and stays open.
        let req = t.affected_by(Path::new("/p/new.txt"));
        assert_eq!(req.len(), 1);
        t.loaded(&req[0], entries(&[("src", true), ("new.txt", false)]));
        assert_eq!(names(&t), ["src", "  main.rs", "new.txt"]);
        assert_eq!(t.rows()[0].node, src, "same node");

        // Collapse hides children without forgetting them.
        assert_eq!(t.toggle(src), Vec::new());
        assert_eq!(names(&t), ["src", "new.txt"]);
    }

    #[test]
    fn restored_folders_open_as_they_are_listed() {
        let mut t = Tree::new(PathBuf::from("/p"));
        t.expand_later([
            PathBuf::from("/p/src"),
            PathBuf::from("/p/src/view"),
            PathBuf::from("/p/gone"),
            PathBuf::from("/elsewhere"),
        ]);
        let mut expanded = t.expanded_dirs();
        assert_eq!(
            expanded.len(),
            3,
            "kept until listed; outside the root never"
        );
        assert!(expanded.contains(&PathBuf::from("/p/gone")));
        let req = t.start();
        let req = t.loaded(&req[0], entries(&[("src", true), ("README", false)]));
        assert_eq!(req.len(), 1, "src opens and is listed");
        let req = t.loaded(&req[0], entries(&[("view", true), ("lib.rs", false)]));
        t.loaded(&req[0], entries(&[("draw.rs", false)]));
        assert_eq!(
            names(&t),
            ["src", "  view", "    draw.rs", "  lib.rs", "README"]
        );
        expanded = t.expanded_dirs();
        assert!(expanded.contains(&PathBuf::from("/p/src/view")));
        assert!(
            !expanded.contains(&PathBuf::from("/p/gone")),
            "not in /p: dropped"
        );
    }

    #[test]
    fn removed_folders_free_their_nodes_and_stale_answers_are_dropped() {
        let mut t = Tree::new(PathBuf::from("/p"));
        let req = t.start();
        t.loaded(&req[0], entries(&[("a", true)]));
        let a = t.rows()[0].node;
        let req_a = t.toggle(a);
        t.loaded(&req_a[0], entries(&[("x", false), ("y", false)]));
        assert_eq!(t.rows().len(), 3);
        let req = t.affected_by(Path::new("/p/a"));
        assert_eq!(req.len(), 2, "the root and a itself");
        let root_req = req.iter().find(|r| r.node == ROOT).unwrap().clone();
        t.loaded(&root_req, entries(&[("b", false)]));
        assert_eq!(names(&t), ["b"]);
        assert!(t.node(a).is_none());
        // The answer for `a` arrives after it was removed: ignored.
        let late = req.iter().find(|r| r.node == a).unwrap().clone();
        t.loaded(&late, entries(&[("z", false)]));
        assert_eq!(names(&t), ["b"]);
        // Its slots are reused.
        let before = t.nodes.len();
        let req = t.affected_by(Path::new("/p/c"));
        t.loaded(
            &req[0],
            entries(&[("b", false), ("c", false), ("d", false)]),
        );
        assert_eq!(t.nodes.len(), before);
    }

    #[test]
    fn reveal_expands_down_to_a_file() {
        let mut t = Tree::new(PathBuf::from("/p"));
        let mut req = t.reveal(Path::new("/p/src/ui/view.rs"));
        let listings = [
            entries(&[("src", true), ("z", false)]),
            entries(&[("ui", true), ("lib.rs", false)]),
            entries(&[("view.rs", false)]),
        ];
        for listing in listings {
            assert_eq!(req.len(), 1);
            req = t.loaded(&req[0].clone(), listing);
        }
        assert_eq!(req, Vec::new());
        assert_eq!(names(&t), ["src", "  ui", "    view.rs", "  lib.rs", "z"]);
        let sel = t.selected.unwrap();
        assert_eq!(t.node(sel).unwrap().name, "view.rs");
        assert_eq!(t.row_of(sel), Some(2));
        assert_eq!(t.reveal(Path::new("/elsewhere/a")), Vec::new());
    }

    #[test]
    fn list_dir_hides_ignored_and_git() {
        let dir = std::env::temp_dir().join(format!("nimbus-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n*.log\n").unwrap();
        std::fs::write(dir.join("a.log"), "").unwrap();
        std::fs::write(dir.join("B.rs"), "").unwrap();
        let names = |show| {
            list_dir(&dir, show)
                .unwrap()
                .into_iter()
                .map(|e| e.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(false), ["src", ".gitignore", "B.rs"]);
        assert_eq!(
            names(true),
            ["src", "target", ".gitignore", "a.log", "B.rs"]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn toggling_ignored_files_during_a_first_listing_asks_again() {
        let mut t = Tree::new(PathBuf::from("/p"));
        let first = t.start();
        let again = t.set_show_ignored(true);
        assert_eq!(
            again.len(),
            1,
            "the root, still loading, is asked for again"
        );
        t.loaded(&first[0], entries(&[("old", false)]));
        assert_eq!(
            names(&t),
            Vec::<String>::new(),
            "the stale answer is dropped"
        );
        t.loaded(&again[0], entries(&[("new", false)]));
        assert_eq!(names(&t), ["new"]);
    }

    #[test]
    fn paths_show_their_root_when_there_are_several() {
        let one = [PathBuf::from("/p")];
        let two = [PathBuf::from("/p"), PathBuf::from("/w/q")];
        let file = Path::new("/w/q/src/a.rs");
        assert_eq!(shown_path(&one, file), None);
        assert_eq!(
            shown_path(&one, Path::new("/p/a.rs")),
            Some(PathBuf::from("a.rs"))
        );
        assert_eq!(
            shown_path(&two, file),
            Some(Path::new("q").join("src/a.rs"))
        );
    }

    #[test]
    fn several_roots_are_rows_of_their_own() {
        let mut t = Tree::new(PathBuf::from("/p"));
        let req = t.start();
        t.loaded(&req[0], entries(&[("src", true), ("a.rs", false)]));
        assert_eq!(names(&t), ["src", "a.rs"], "one root: as before");
        assert!(t.add_root(PathBuf::from("/p/src")).is_err(), "inside /p");
        assert!(t.add_root(PathBuf::from("/")).is_err(), "holds /p");
        assert!(t.add_root(PathBuf::from("/p")).is_err(), "already there");
        t.add_root(PathBuf::from("/q")).unwrap();
        assert!(t.start().is_empty(), "a new root starts closed");
        assert_eq!(names(&t), ["p", "  src", "  a.rs", "q"]);
        let q = t.root_of(Path::new("/q/b.rs")).unwrap();
        let req = t.toggle(q);
        assert_eq!(req.len(), 1);
        t.loaded(&req[0], entries(&[("b.rs", false)]));
        assert_eq!(names(&t), ["p", "  src", "  a.rs", "q", "  b.rs"]);
        assert_eq!(t.roots(), [PathBuf::from("/p"), PathBuf::from("/q")]);
        assert!(t.is_root(q) && t.multi());
        // Left from a top-level file goes to its root's row.
        let b = t.rows()[4].node;
        assert_eq!(t.parent(b), Some(q));
        // Revealing works in any root; roots are never "expanded folders".
        let src = t.rows()[1].node;
        let req = t.toggle(src);
        t.loaded(&req[0], entries(&[("lib.rs", false)]));
        assert_eq!(
            t.expanded_dirs(),
            [PathBuf::from("/p/src"), PathBuf::from("/q")],
            "an open second root is remembered"
        );
        t.toggle(q);
        assert_eq!(names(&t).len(), 5, "q closed: b.rs hidden");
        assert_eq!(t.reveal(Path::new("/q/b.rs")), Vec::new());
        assert_eq!(t.node(t.selected.unwrap()).unwrap().name, "b.rs");
        // The first root stays; another can go.
        assert!(!t.remove_root(ROOT));
        assert!(t.remove_root(q));
        assert!(t.selected.is_none());
        assert_eq!(names(&t), ["src", "  lib.rs", "a.rs"]);
        assert_eq!(t.parent(t.rows()[0].node), None);
        // A restored second root opens as it is added back.
        t.add_root(PathBuf::from("/q")).unwrap();
        t.expand_later([PathBuf::from("/q")]);
        assert_eq!(t.start().len(), 1);
    }

    #[test]
    fn a_change_during_a_listing_lists_again_after_it() {
        let mut t = Tree::new(PathBuf::from("/p"));
        let req = t.start();
        assert_eq!(
            t.affected_by(Path::new("/p/x")),
            Vec::new(),
            "already loading"
        );
        let next = t.loaded(&req[0], entries(&[("a", false)]));
        assert_eq!(next.len(), 1, "listed again");
        t.loaded(&next[0], entries(&[("a", false), ("x", false)]));
        assert_eq!(names(&t), ["a", "x"]);
        assert_eq!(t.affected_by(Path::new("/p/y")).len(), 1);
    }
}
