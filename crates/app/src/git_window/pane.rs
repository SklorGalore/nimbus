//! What the git window shows and how it reacts (M27), apart from the
//! window and the event loop (`git_window.rs`), so a headless render
//! draws exactly what a window does.
//!
//! The window has four panes: the sidebar on the left (Changes, then the
//! branches, remote-tracking branches, tags and stash entries), the
//! history in the middle, and on the right the selected commit's details
//! above the compare view of its selected file. Selecting Changes shows
//! the working tree's staged and unstaged files there instead.
//!
//! **Hit areas.** Each frame records the rectangles it drew that react to
//! the mouse (a row, a file, a toggle, a divider) with what they mean. A
//! click looks its point up in that list, so drawing and clicking can
//! never disagree about where something is.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, MutexGuard, PoisonError};
use std::time::Instant;

use nimbus_core::Whitespace as Ws;
use nimbus_core::diff::Normalize;
use nimbus_syntax::state::{Job, JobResult};

use super::history::{History, MAX_LANES};
use super::load::{self, Details, GitEvent, Loading, Refresh, Send, Side, Source, Texts};
use super::message::MessageBox;
use super::ops::{Discarded, Op, Prefill};
use crate::compare::{Compare, Other, Toggle};
use crate::document::{Carets, Document};
use crate::git::{Commit, FileChange, GitError, Kind, Ref, RefKind, Repo, Stash, Status};
use crate::input::GitCmd;
use crate::languages::Languages;
use crate::prompt::Prompt;
use crate::theme::Ui;
use crate::view::{Layout, View};

pub mod access;
pub mod draw;
mod staging;

/// Pixel count as `f32`.
#[expect(
    clippy::cast_precision_loss,
    reason = "counts on screen, far below 2^24"
)]
fn f(n: usize) -> f32 {
    n as f32
}

/// A non-negative pixel length as a whole count, rounding down.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to be non-negative; screen sizes"
)]
fn whole(v: f32) -> usize {
    v.max(0.0).floor() as usize
}

/// Where the history and the compare can be scrolled to, and which pane
/// keys go to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The sidebar's list.
    Sidebar,
    /// The history.
    History,
    /// The files of the selected commit or of Changes.
    Files,
    /// The compare, where rows are picked for Stage Lines (M28).
    Diff,
    /// The commit message box.
    Message,
}

/// What the right side shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selected {
    /// The working tree's changes.
    Changes,
    /// A commit, by its hash (rows move when the history reloads).
    Commit(String),
}

/// What a recorded hit area means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// A sidebar line.
    Side(usize),
    /// A history row.
    Row(usize),
    /// A file in the details.
    File(usize),
    /// A merge's parent to compare against.
    Parent(usize),
    /// A compare option.
    Toggle(Toggle),
    /// "Open in Editor".
    OpenInEditor,
    /// A pane divider: 0 sidebar|history, 1 history|details, 2 details
    /// above the compare.
    Divider(usize),
    /// A button that runs a git window command (M28).
    Git(GitCmd),
    /// A file's own Stage, Unstage or Discard button.
    FileGit(usize, GitCmd),
    /// The commit message box.
    Message,
    /// The compare's rows, for picking lines.
    Compare,
    /// Stop the commit that is running.
    StopCommit,
}

/// One line of the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SideItem {
    /// A section title.
    Section(&'static str, usize),
    /// The working tree's changes.
    Changes,
    /// A ref, by index into `GitWindow::refs`.
    Ref(usize),
    /// A stash entry, by index into `GitWindow::stashes`.
    Stash(usize),
    /// A note such as "none".
    Note(&'static str),
}

/// One file the right side lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileItem {
    /// How it changed.
    pub kind: Kind,
    /// Its path from the top folder.
    pub path: String,
    /// What the list shows: the path, or `old → new` for a rename.
    pub label: String,
    /// Where the diff's two sides come from.
    pub sides: (Source, Source),
    /// In Changes: staged (`Some(true)`) or not; `None` for a commit.
    pub staged: Option<bool>,
    /// A submodule: there is no text to compare.
    pub submodule: bool,
    /// The path before a staged rename.
    pub orig: Option<String>,
    /// In Changes: the file is in both lists, part staged (M28).
    pub partly: bool,
}

/// One line of the details pane, above the compare.
#[derive(Debug, Clone, PartialEq)]
enum InfoLine {
    /// Text in a colour.
    Text(String, [f32; 4]),
    /// A label and its value.
    Field(&'static str, String),
    /// A merge's parents, to pick the one compared against.
    Parents,
    /// A file, by index into `GitWindow::files`.
    File(usize),
    /// Changes' list heading: staged (`true`) or not, with its text.
    Heading(bool, String),
    /// Space.
    Blank,
}

/// Commits in the quick first load of HEAD's history.
const PREVIEW: u32 = 2_000;

/// HEAD and every ref's (name, target) a history was loaded for.
type RefsSeen = (Option<String>, Vec<(String, String)>);

/// The compare view of one file, or why there is none.
#[expect(
    clippy::large_enum_variant,
    reason = "one per window, swapped whole; boxing would only add a pointer chase"
)]
pub enum DiffPane {
    /// The two sides, compared.
    Compare {
        /// The new side, as an ordinary document (never edited).
        doc: Box<Document>,
        /// Its cursors, which stay at the start.
        carets: Carets,
        /// Its view.
        view: View,
        /// The compare, holding the old side.
        compare: Box<Compare>,
        /// Both sides' bytes as read (old, new), for the patches that
        /// stage part of the file (M28).
        raw: [Arc<Vec<u8>>; 2],
    },
    /// Instead of a compare: "binary file", "reading…", an error.
    Message(String),
}

impl std::fmt::Debug for DiffPane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Compare { compare, .. } => f.debug_tuple("Compare").field(compare).finish(),
            Self::Message(m) => f.debug_tuple("Message").field(m).finish(),
        }
    }
}

/// The parts of the window that need a frame's geometry.
#[derive(Debug, Clone, Copy)]
struct Geo {
    w: f32,
    lh: f32,
    cell: f32,
    margin: f32,
    header: f32,
    bottom: f32,
    side: (f32, f32),
    history: (f32, f32),
    details: (f32, f32),
    info_bottom: f32,
    /// Top of the commit area under Changes' list (`info_bottom` when
    /// there is none).
    commit_top: f32,
    bar_bottom: f32,
    /// The bar has a second row of staging buttons.
    bar_rows: usize,
    row_h: f32,
    line: f32,
}

/// Everything the git window shows.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent flags: what the mouse is doing, which side a compare reads"
)]
pub struct GitWindow {
    /// The repository, once found.
    pub repo: Option<Repo>,
    /// Why there is none (no git, not a repository), in plain words.
    pub error: Option<String>,
    /// Where the window was opened, for the error message.
    pub dir: PathBuf,
    status: Option<Status>,
    refs: Vec<Ref>,
    stashes: Vec<Stash>,
    /// Ref labels by commit hash: indices into `refs`.
    labels: HashMap<String, Vec<usize>>,
    side_items: Vec<SideItem>,
    collapsed: [bool; 4],
    /// The history shown, and a reload replacing it once it catches up.
    current: Option<Loading>,
    next: Option<Loading>,
    generation: u64,
    /// The refs the shown history was loaded for, to tell when to reload.
    loaded_for: Option<RefsSeen>,
    /// The selection, by hash, and the row it was last seen at.
    pub selected: Selected,
    selected_row: Option<usize>,
    /// Row to select once the history reaches it (a ref clicked early).
    pending_jump: Option<String>,
    /// Row to bring on screen at the next frame, which knows the height.
    pending_scroll: Option<usize>,
    /// The compare's layout at the last frame, for keys that step
    /// through its changes.
    compare_layout: Option<Layout>,
    /// History scroll, in rows (fractional while scrolling smoothly).
    scroll: f32,
    side_scroll: f32,
    info_scroll: f32,
    /// Keys go here.
    pub focus: Focus,
    /// The commit's object and files, or Changes' files.
    commit: Option<Result<Commit, GitError>>,
    parent: usize,
    pub files: Vec<FileItem>,
    files_error: Option<String>,
    file_sel: Option<usize>,
    info: Vec<InfoLine>,
    /// Width the info lines were wrapped for.
    info_width: f32,
    /// The info pane's line height and height at the last frame.
    info_view: (f32, f32),
    pub diff: Option<DiffPane>,
    /// Requests' tokens: results with an older one are dropped.
    details_token: u64,
    texts_token: u64,
    pub normalize: Normalize,
    /// Pane sizes: sidebar width in cells, the details' share of the rest,
    /// and the info's share of the details' height.
    side_cells: f32,
    details_share: f32,
    info_share: f32,
    hits: Vec<([f32; 4], Hit)>,
    /// The buttons the last frame drew: what each does, its label, and
    /// whether it is on and usable, for the screen reader's tree.
    drawn_buttons: Vec<(Hit, &'static str, bool, bool)>,
    /// A divider being dragged.
    pub dragging: Option<usize>,
    scratch: String,
    send: Send,
    languages: Arc<Languages>,
    /// A refresh is running; another was asked for meanwhile.
    refreshing: bool,
    refresh_again: bool,
    /// Now, in seconds since 1970, for relative dates (set per frame).
    now: i64,
    /// Rows picked in the compare for Stage Lines: anchor and head (M28).
    pub picked: Option<(usize, usize)>,
    /// The mouse is down picking rows.
    pub picking: bool,
    /// The mouse is down selecting in the message box.
    pub message_drag: bool,
    /// A change to the repository is running: what it is.
    busy: Option<String>,
    /// Discards Undo Discard can put back, newest last.
    undo: Vec<Discarded>,
    /// A question or report over the window.
    pub prompt: Option<Prompt<Ask>>,
    /// The commit message box.
    pub message: MessageBox,
    /// A commit is running: set this to stop it.
    committing: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Who commits are by, and the last message.
    prefill: Prefill,
    /// The message's version when its draft was last kept.
    kept_version: u64,
    /// Something to say in the status bar, for the window to take.
    said: Option<(String, bool)>,
    /// Columns per tab, from the config.
    pub tab_width: usize,
    /// The window's layout at the last frame, for prompts' clicks.
    window_layout: Option<Layout>,
}

/// What a prompt's answer goes on to do.
#[derive(Debug, Clone)]
pub enum Ask {
    /// Discard, with what to say when it is done.
    Discard(Box<Op>, String),
    /// Nothing: a report, such as a hook's output.
    Report,
}

/// A colour with its alpha scaled.
fn fade(c: [f32; 4], a: f32) -> [f32; 4] {
    [c[0], c[1], c[2], c[3] * a]
}

fn lane_color(ui: &Ui, lane: usize) -> [f32; 4] {
    match lane % 6 {
        0 => ui.graph_1,
        1 => ui.graph_2,
        2 => ui.graph_3,
        3 => ui.graph_4,
        4 => ui.graph_5,
        _ => ui.graph_6,
    }
}

fn kind_color(ui: &Ui, kind: Kind) -> [f32; 4] {
    match kind {
        Kind::Added | Kind::Untracked | Kind::Copied => ui.graph_3,
        Kind::Deleted | Kind::Conflicted => ui.error,
        Kind::Renamed => ui.graph_2,
        _ => ui.graph_1,
    }
}

/// Whether bytes look binary: a NUL in the first 8 KB, as git decides.
fn is_binary(bytes: &[u8]) -> bool {
    memchr::memchr(0, bytes.get(..8192).unwrap_or(bytes)).is_some()
}

impl GitWindow {
    /// A window for the repository holding `dir`, not yet opened.
    pub fn new(dir: PathBuf, send: Send, languages: Arc<Languages>, normalize: Normalize) -> Self {
        Self {
            repo: None,
            error: None,
            dir,
            status: None,
            refs: Vec::new(),
            stashes: Vec::new(),
            labels: HashMap::new(),
            side_items: Vec::new(),
            collapsed: [false, true, false, false],
            current: None,
            next: None,
            generation: 0,
            loaded_for: None,
            selected: Selected::Changes,
            selected_row: None,
            pending_jump: None,
            pending_scroll: None,
            compare_layout: None,
            scroll: 0.0,
            side_scroll: 0.0,
            info_scroll: 0.0,
            focus: Focus::History,
            commit: None,
            parent: 0,
            files: Vec::new(),
            files_error: None,
            file_sel: None,
            info: Vec::new(),
            info_width: 0.0,
            info_view: (0.0, 0.0),
            diff: None,
            details_token: 0,
            texts_token: 0,
            normalize,
            side_cells: 24.0,
            details_share: 0.48,
            info_share: 0.36,
            hits: Vec::new(),
            drawn_buttons: Vec::new(),
            dragging: None,
            scratch: String::new(),
            send,
            languages,
            refreshing: false,
            refresh_again: false,
            now: 0,
            picked: None,
            picking: false,
            message_drag: false,
            busy: None,
            undo: Vec::new(),
            prompt: None,
            message: MessageBox::default(),
            committing: None,
            prefill: Prefill::default(),
            kept_version: 0,
            said: None,
            tab_width: 4,
            window_layout: None,
        }
    }

    /// The window's title: "repo — Git — Nimbus".
    #[must_use]
    pub fn title(&self) -> String {
        match &self.repo {
            Some(r) => format!("{} — Git — Nimbus", r.name()),
            None => "Git — Nimbus".to_owned(),
        }
    }

    /// Folders to watch for changes: the git folder and its refs, the top
    /// folder, and the folders of the files the status lists.
    #[must_use]
    pub fn watched(&self) -> std::collections::HashSet<PathBuf> {
        let mut set = std::collections::HashSet::new();
        let Some(repo) = &self.repo else {
            return set;
        };
        set.insert(repo.git_dir.clone());
        set.insert(repo.top.clone());
        let refs = repo.git_dir.join("refs");
        let mut stack = vec![(refs, 0)];
        while let Some((dir, depth)) = stack.pop() {
            if set.len() > 400 {
                break;
            }
            if let Ok(read) = std::fs::read_dir(&dir) {
                for e in read.flatten() {
                    if depth < 4 && e.file_type().is_ok_and(|t| t.is_dir()) {
                        stack.push((e.path(), depth + 1));
                    }
                }
            }
            set.insert(dir);
        }
        if let Some(s) = &self.status {
            for e in s.entries.iter().take(1000) {
                let p = repo.top.join(e.path.trim_end_matches('/'));
                if let Some(parent) = p.parent() {
                    set.insert(parent.to_path_buf());
                }
            }
        }
        set
    }

    // ---- Events -------------------------------------------------------

    /// Handle a worker's result. Returns whether to draw again.
    pub fn event(&mut self, event: GitEvent) -> bool {
        match event {
            GitEvent::Opened(Ok(repo)) => {
                if let Some(draft) = super::message::draft(&repo.top) {
                    self.message = MessageBox::new(&draft);
                    self.kept_version = self.message.doc.version;
                }
                super::ops::prefill(repo.clone(), Arc::clone(&self.send));
                self.repo = Some(repo);
                self.error = None;
                self.refresh();
            }
            GitEvent::Opened(Err(e)) => self.error = Some(open_error(&e, &self.dir)),
            GitEvent::Refreshed(r) => self.refreshed(*r),
            GitEvent::History(generation) => return self.history_grew(generation),
            GitEvent::Details(d) => self.details_arrived(*d),
            GitEvent::Texts(t) => self.texts_arrived(*t),
            GitEvent::Diff(result) => {
                if let Some(DiffPane::Compare { doc, compare, .. }) = &mut self.diff
                    && compare.wants(&result)
                {
                    compare.receive(doc, result);
                }
            }
            GitEvent::Syntax(result) => self.parsed(result),
            GitEvent::Disk => self.refresh(),
            GitEvent::Done(d) => self.done(*d),
            GitEvent::Committed(r) => self.committed(*r),
            GitEvent::Prefill(p) => self.prefill = *p,
            GitEvent::FontLoaded
            | GitEvent::Say(..)
            | GitEvent::Fonts(_)
            | GitEvent::Redraw
            | GitEvent::Access(_) => {}
        }
        true
    }

    /// Read the status and refs again, unless a read is already running,
    /// in which case one more follows it.
    pub fn refresh(&mut self) {
        let Some(repo) = &self.repo else { return };
        if self.refreshing {
            self.refresh_again = true;
            return;
        }
        self.refreshing = true;
        load::refresh(repo.clone(), Arc::clone(&self.send));
    }

    /// Take a refresh's result; reload the history when the refs moved.
    pub fn refreshed(&mut self, r: Refresh) {
        self.refreshing = false;
        if std::mem::take(&mut self.refresh_again) {
            self.refresh();
        }
        match r.status {
            Ok(s) => self.status = Some(s),
            Err(e) => {
                self.error = Some(format!("git status failed: {e}"));
                return;
            }
        }
        self.refs = r.refs.unwrap_or_else(|e| {
            log::warn!("git window: cannot list refs: {e}");
            Vec::new()
        });
        self.stashes = r.stashes;
        self.labels.clear();
        for (i, rf) in self.refs.iter().enumerate() {
            if rf.kind != RefKind::Stash {
                self.labels.entry(rf.target.clone()).or_default().push(i);
            }
        }
        self.build_side();
        let head = self.status.as_ref().and_then(|s| s.head.clone());
        let key: Vec<(String, String)> = self
            .refs
            .iter()
            .filter(|r| r.kind != RefKind::Stash)
            .map(|r| (r.name.clone(), r.target.clone()))
            .collect();
        let wanted = Some((head, key));
        if self.loaded_for != wanted {
            self.loaded_for = wanted;
            self.reload_history();
        }
        if self.selected == Selected::Changes {
            self.show_changes();
        }
    }

    fn reload_history(&mut self) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let head = self.status.as_ref().is_some_and(|s| s.head.is_some());
        if self.current.is_none() && head {
            // A first screen at once, from HEAD alone; the whole history
            // replaces it when it catches up.
            self.generation += 1;
            match load::history(&repo, (head, Some(PREVIEW)), self.generation, &self.send) {
                Ok(l) => self.current = Some(l),
                Err(e) => log::warn!("git window: no preview of the history: {e}"),
            }
        }
        self.generation += 1;
        match load::history(&repo, (head, None), self.generation, &self.send) {
            Ok(l) => {
                if self.current.is_none() {
                    self.current = Some(l);
                } else {
                    self.next = Some(l);
                }
            }
            Err(e) => self.error = Some(format!("git log failed: {e}")),
        }
    }

    fn history_grew(&mut self, generation: u64) -> bool {
        if let Some(n) = &self.next
            && n.generation == generation
        {
            n.news.store(false, std::sync::atomic::Ordering::Release);
            let (len, finished) = {
                let h = lock(&n.history);
                (h.len(), h.finished.is_some())
            };
            let shown = self.current.as_ref().map_or(0, |c| lock(&c.history).len());
            if finished || len >= shown {
                self.swap_history();
            }
            return false;
        }
        let Some(c) = &self.current else { return false };
        if c.generation != generation {
            return false;
        }
        c.news.store(false, std::sync::atomic::Ordering::Release);
        if self.selected_row.is_none() || self.pending_jump.is_some() {
            self.find_selection();
        }
        true
    }

    /// The reload caught up: show it, keeping the selection and the row
    /// at the top of the history where they were.
    fn swap_history(&mut self) {
        let top = self.current.as_ref().and_then(|c| {
            let h = lock(&c.history);
            (!h.is_empty()).then(|| h.oid(whole(self.scroll).min(h.len().saturating_sub(1))))
        });
        self.current = self.next.take();
        self.selected_row = None;
        self.find_selection();
        if let (Some(top), Some(c)) = (top, &self.current)
            && let Some(row) = lock(&c.history).find(&top)
        {
            self.scroll = f(row);
        }
    }

    /// Find the selected commit's row in the history.
    fn find_selection(&mut self) {
        let Some(c) = &self.current else { return };
        let h = lock(&c.history);
        if let Some(target) = self.pending_jump.clone() {
            if let Some(row) = h.find(&target) {
                drop(h);
                self.pending_jump = None;
                self.select_row(row, true);
            }
            return;
        }
        if let Selected::Commit(oid) = &self.selected {
            self.selected_row = h.find(oid);
        }
    }

    fn details_arrived(&mut self, d: Details) {
        if d.token != self.details_token {
            return;
        }
        self.parent = d.parent;
        self.files.clear();
        self.files_error = None;
        match d.files {
            Ok(files) => {
                self.files = files.into_iter().map(commit_file).collect();
            }
            Err(e) => self.files_error = Some(e.to_string()),
        }
        self.commit = Some(d.commit);
        self.info_width = 0.0;
        self.file_sel = None;
        self.diff = None;
        if !self.files.is_empty() {
            self.select_file(0);
        }
    }

    fn texts_arrived(&mut self, t: Texts) {
        if t.token != self.texts_token {
            return;
        }
        self.picked = None;
        self.diff = Some(self.make_diff(t));
    }

    fn make_diff(&self, t: Texts) -> DiffPane {
        let bytes = |side: &Side| match side {
            Side::Text(b) => Arc::new(b.clone()),
            _ => Arc::new(Vec::new()),
        };
        let raw = [bytes(&t.left), bytes(&t.right)];
        let text = |side: Side| -> Result<Option<String>, String> {
            match side {
                Side::Text(bytes) if is_binary(&bytes) => Err("binary".to_owned()),
                Side::Text(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
                Side::Absent => Ok(None),
                Side::Failed(e) => Err(e),
            }
        };
        let (left, right) = match (text(t.left), text(t.right)) {
            (Err(b), _) | (_, Err(b)) if b == "binary" => {
                return DiffPane::Message("Binary file: no text to compare".to_owned());
            }
            (Err(e), _) | (_, Err(e)) => return DiffPane::Message(format!("Cannot read: {e}")),
            (Ok(l), Ok(r)) => (l, r),
        };
        let path = self.repo.as_ref().map(|r| r.top.join(&t.path));
        let mut doc = Document::with_text(path.clone(), right.as_deref().unwrap_or(""));
        let mut other = Document::with_text(path, left.as_deref().unwrap_or(""));
        doc.detect_language(&self.languages);
        other.detect_language(&self.languages);
        for d in [&mut doc, &mut other] {
            if let Some(job) = d.syntax.take_job(&d.buffer) {
                spawn_syntax(&self.send, job);
            }
        }
        let name = t.path.rsplit('/').next().unwrap_or(&t.path).to_owned();
        let send = Arc::clone(&self.send);
        let compare = Compare::start(
            &mut doc,
            other,
            Other::Revision(name),
            self.normalize,
            Arc::new(move |r| send(GitEvent::Diff(r))),
        );
        let mut view = View::default();
        view.set_wrap(false);
        DiffPane::Compare {
            doc: Box::new(doc),
            carets: Carets::default(),
            view,
            compare: Box::new(compare),
            raw,
        }
    }

    fn parsed(&mut self, result: JobResult) {
        let Some(DiffPane::Compare { doc, compare, .. }) = &mut self.diff else {
            return;
        };
        let id = result.id();
        let target = if doc.syntax.waiting_for(id) {
            &mut **doc
        } else if compare.other.syntax.waiting_for(id) {
            &mut compare.other
        } else {
            return;
        };
        let budget = std::time::Duration::from_millis(2);
        if let Some(job) = target.syntax.finish(result, &target.buffer, budget) {
            spawn_syntax(&self.send, job);
        }
    }

    /// The window changed size or font: wrap the details again.
    pub fn relayout(&mut self) {
        self.info_width = 0.0;
    }

    /// Load everything a headless render shows, on this thread: the
    /// repository, its status and refs, the whole history, then the
    /// selection (`changes`, a hash prefix, or the newest commit) and its
    /// first file's compare. `rx` is where this window's `send` delivers.
    pub fn headless_load(
        &mut self,
        rx: &std::sync::mpsc::Receiver<GitEvent>,
        select: Option<&str>,
    ) {
        let repo = match Repo::open(&self.dir) {
            Ok(r) => r,
            Err(e) => {
                self.error = Some(open_error(&e, &self.dir));
                return;
            }
        };
        self.repo = Some(repo.clone());
        self.refreshed(load::read_refresh(&repo));
        let pump = |me: &mut Self, until: &dyn Fn(&Self) -> bool, quiet: std::time::Duration| {
            let deadline = Instant::now() + std::time::Duration::from_secs(60);
            while Instant::now() < deadline {
                match rx.recv_timeout(quiet) {
                    Ok(e) => {
                        me.event(e);
                    }
                    Err(_) if until(me) => return,
                    Err(_) => {}
                }
            }
        };
        let finished = |me: &Self| {
            me.next.is_none()
                && me
                    .current
                    .as_ref()
                    .is_none_or(|c| !c.preview && lock(&c.history).finished.is_some())
        };
        pump(self, &finished, std::time::Duration::from_millis(50));
        match select {
            Some("changes") => self.show_changes(),
            Some(prefix) => {
                let row = self.current.as_ref().and_then(|c| {
                    let h = lock(&c.history);
                    (0..h.len()).find(|&i| h.oid(i).starts_with(prefix))
                });
                self.select_row(row.unwrap_or(0), true);
            }
            None => self.select_row(0, true),
        }
        let settled = |me: &Self| match &me.diff {
            Some(DiffPane::Message(m)) => m != "Reading…",
            Some(DiffPane::Compare { compare, .. }) => compare.hunks().is_some(),
            None => me.commit.is_some() || me.selected == Selected::Changes,
        };
        pump(self, &settled, std::time::Duration::from_millis(300));
    }

    // ---- Selection ----------------------------------------------------

    /// Show the working tree's changes on the right.
    pub fn show_changes(&mut self) {
        let keep = self.file_sel.and_then(|i| self.files.get(i)).cloned();
        let was_changes = self.selected == Selected::Changes;
        self.selected = Selected::Changes;
        self.selected_row = None;
        self.commit = None;
        self.files_error = None;
        self.info_width = 0.0;
        self.files.clear();
        if let Some(s) = &self.status {
            for e in s.entries.iter().filter(|e| e.is_staged()) {
                self.files.push(FileItem {
                    kind: e.staged,
                    path: e.path.clone(),
                    label: rename_label(e.orig.as_deref(), &e.path),
                    sides: (
                        e.head_oid.clone().map_or(Source::Absent, Source::Blob),
                        e.index_oid.clone().map_or(Source::Absent, Source::Blob),
                    ),
                    staged: Some(true),
                    submodule: false,
                    orig: e.orig.clone(),
                    partly: e.is_unstaged(),
                });
            }
            for e in s.entries.iter().filter(|e| e.is_unstaged()) {
                let kind = if e.staged == Kind::Conflicted {
                    Kind::Conflicted
                } else {
                    e.unstaged
                };
                let left = match kind {
                    Kind::Untracked => Source::Absent,
                    _ => e.index_oid.clone().map_or(Source::Absent, Source::Blob),
                };
                let right = if kind == Kind::Deleted {
                    Source::Absent
                } else {
                    Source::Disk(e.path.clone())
                };
                self.files.push(FileItem {
                    kind,
                    path: e.path.clone(),
                    label: e.path.clone(),
                    sides: (left, right),
                    staged: Some(false),
                    submodule: e.path.ends_with('/'),
                    orig: None,
                    partly: e.is_staged(),
                });
            }
        }
        let again = keep.and_then(|k| {
            self.files
                .iter()
                .position(|f| f.path == k.path && f.staged == k.staged)
        });
        match again {
            Some(i) if was_changes => {
                // The same file again: read it again, its text may have
                // changed.
                self.file_sel = None;
                self.select_file(i);
            }
            _ if !self.files.is_empty() => {
                self.file_sel = None;
                self.select_file(0);
            }
            _ => {
                self.file_sel = None;
                self.diff = None;
            }
        }
    }

    /// Select history row `row` and read its details.
    pub fn select_row(&mut self, row: usize, scroll_to: bool) {
        let Some(c) = &self.current else { return };
        let h = lock(&c.history);
        if row >= h.len() {
            return;
        }
        let oid = h.oid(row);
        drop(h);
        self.selected_row = Some(row);
        if scroll_to {
            self.pending_scroll = Some(row);
        }
        if self.selected == Selected::Commit(oid.clone()) {
            return;
        }
        self.selected = Selected::Commit(oid.clone());
        self.choose_parent(0);
    }

    /// Compare the selected commit against its parent number `parent`.
    pub fn choose_parent(&mut self, parent: usize) {
        let (Some(repo), Selected::Commit(oid)) = (&self.repo, &self.selected) else {
            return;
        };
        self.details_token += 1;
        self.texts_token += 1;
        self.parent = parent;
        self.files.clear();
        self.file_sel = None;
        self.info_width = 0.0;
        self.diff = Some(DiffPane::Message("Reading…".to_owned()));
        load::details(
            repo.clone(),
            self.details_token,
            oid.clone(),
            parent,
            Arc::clone(&self.send),
        );
    }

    /// Select file `i` of the list and read its two sides.
    pub fn select_file(&mut self, i: usize) {
        if self.file_sel == Some(i) {
            return;
        }
        let (Some(repo), Some(file)) = (&self.repo, self.files.get(i)) else {
            return;
        };
        self.file_sel = Some(i);
        self.picked = None;
        self.texts_token += 1;
        if file.submodule {
            self.diff = Some(DiffPane::Message(
                "A submodule or folder: no text to compare".to_owned(),
            ));
            return;
        }
        self.diff = Some(DiffPane::Message("Reading…".to_owned()));
        load::texts(
            repo.clone(),
            self.texts_token,
            file.path.clone(),
            file.sides.clone(),
            Arc::clone(&self.send),
        );
    }

    /// The selected file's working copy, for Open in Editor.
    #[must_use]
    pub fn selected_path(&self) -> Option<PathBuf> {
        let file = self.files.get(self.file_sel?)?;
        Some(self.repo.as_ref()?.top.join(&file.path))
    }

    /// Activate sidebar line `i`: Changes, or scroll to a ref's commit.
    pub fn side_click(&mut self, i: usize) {
        match self.side_items.get(i).cloned() {
            Some(SideItem::Changes) => self.show_changes(),
            Some(SideItem::Ref(r)) => {
                if let Some(target) = self.refs.get(r).map(|r| r.target.clone()) {
                    self.jump_to(target);
                }
            }
            Some(SideItem::Stash(s)) => {
                if let Some(oid) = self.stashes.get(s).map(|s| s.oid.clone()) {
                    // Stash commits are not in the history; show them
                    // directly.
                    self.selected_row = None;
                    self.selected = Selected::Commit(oid);
                    self.choose_parent(0);
                }
            }
            Some(SideItem::Section(_, n)) => {
                if let Some(c) = self.collapsed.get_mut(n) {
                    *c = !*c;
                }
                self.build_side();
            }
            _ => {}
        }
    }

    fn jump_to(&mut self, oid: String) {
        let row = self
            .current
            .as_ref()
            .and_then(|c| lock(&c.history).find(&oid));
        match row {
            Some(row) => self.select_row(row, true),
            None => self.pending_jump = Some(oid),
        }
    }

    fn build_side(&mut self) {
        self.side_items.clear();
        self.side_items.push(SideItem::Changes);
        let sections = [
            ("Branches", RefKind::Branch),
            ("Remotes", RefKind::Remote),
            ("Tags", RefKind::Tag),
        ];
        for (n, (title, kind)) in sections.into_iter().enumerate() {
            self.side_items.push(SideItem::Section(title, n));
            if self.collapsed.get(n).copied().unwrap_or(false) {
                continue;
            }
            let before = self.side_items.len();
            for (i, r) in self.refs.iter().enumerate() {
                if r.kind == kind {
                    self.side_items.push(SideItem::Ref(i));
                }
            }
            if self.side_items.len() == before {
                self.side_items.push(SideItem::Note("none"));
            }
        }
        self.side_items.push(SideItem::Section("Stashes", 3));
        if !self.collapsed[3] {
            for i in 0..self.stashes.len() {
                self.side_items.push(SideItem::Stash(i));
            }
            if self.stashes.is_empty() {
                self.side_items.push(SideItem::Note("none"));
            }
        }
    }

    /// Flip a compare option.
    pub fn toggle(&mut self, t: Toggle) {
        match t {
            Toggle::Whitespace => {
                self.normalize.whitespace = match self.normalize.whitespace {
                    Ws::Exact => Ws::Ends,
                    Ws::Ends => Ws::All,
                    Ws::All => Ws::Exact,
                };
            }
            Toggle::Case => self.normalize.case = !self.normalize.case,
            Toggle::BlankLines => self.normalize.blank_lines = !self.normalize.blank_lines,
            Toggle::Unified => {
                if let Some(DiffPane::Compare { compare, .. }) = &mut self.diff {
                    compare.toggle_unified();
                }
                return;
            }
        }
        if let Some(DiffPane::Compare { doc, compare, .. }) = &mut self.diff {
            compare.set_normalize(doc, self.normalize);
        }
    }
}

/// Plain words for why the window has no repository.
fn open_error(e: &GitError, dir: &std::path::Path) -> String {
    match e {
        GitError::NotARepo => format!("{} is not inside a git repository", dir.display()),
        GitError::NotFound => {
            "git is not installed or not on PATH; the git window runs your own git".to_owned()
        }
        other => format!("Cannot open the repository: {other}"),
    }
}

fn rename_label(orig: Option<&str>, path: &str) -> String {
    match orig {
        Some(o) => format!("{o} → {path}"),
        None => path.to_owned(),
    }
}

fn commit_file(fc: FileChange) -> FileItem {
    let blob = |o: Option<String>| o.map_or(Source::Absent, Source::Blob);
    FileItem {
        kind: fc.kind,
        label: rename_label(fc.old_path.as_deref(), &fc.path),
        path: fc.path,
        sides: (blob(fc.old_oid), blob(fc.new_oid)),
        staged: None,
        submodule: fc.submodule,
        orig: None,
        partly: false,
    }
}

fn lock(h: &std::sync::Mutex<History>) -> MutexGuard<'_, History> {
    h.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Run a parse on its own thread, as the editor's `spawn_syntax` does.
fn spawn_syntax(send: &Send, job: Job) {
    let send = Arc::clone(send);
    let slot = Arc::new(std::sync::Mutex::new(Some(job)));
    let theirs = Arc::clone(&slot);
    let mine = Arc::clone(&send);
    let spawned = std::thread::Builder::new()
        .name("nimbus-parse".to_owned())
        .spawn(move || {
            if let Some(job) = theirs.lock().ok().and_then(|mut j| j.take()) {
                mine(GitEvent::Syntax(job.run()));
            }
        });
    if let Err(e) = spawned {
        log::error!("could not start a parse thread ({e}); parsing here");
        if let Some(job) = slot.lock().ok().and_then(|mut j| j.take()) {
            send(GitEvent::Syntax(job.run()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{TestDir, test_repo};
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::Duration;

    pub(super) fn window(dir: &Path) -> (GitWindow, mpsc::Receiver<GitEvent>) {
        let (tx, rx) = mpsc::channel::<GitEvent>();
        let tx = std::sync::Mutex::new(tx);
        let send: Send = Arc::new(move |e| tx.lock().is_ok_and(|t| t.send(e).is_ok()));
        let languages = Languages::load(&crate::config::Config::default().languages).0;
        let w = GitWindow::new(
            dir.to_path_buf(),
            send,
            Arc::new(languages),
            Normalize::default(),
        );
        (w, rx)
    }

    /// Feed events to the window until `done` holds.
    pub(super) fn pump(
        w: &mut GitWindow,
        rx: &mpsc::Receiver<GitEvent>,
        done: impl Fn(&GitWindow) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done(w) {
            assert!(Instant::now() < deadline, "timed out");
            if let Ok(e) = rx.recv_timeout(Duration::from_millis(50)) {
                w.event(e);
            }
        }
    }

    #[test]
    fn a_merge_shows_its_files_against_either_parent() {
        let Some(dir) = test_repo("git-pane-merge") else {
            return;
        };
        let (mut w, rx) = window(&dir.0);
        w.headless_load(&rx, None);
        assert_eq!(w.error, None);
        let len = w.current.as_ref().map(|c| lock(&c.history).len());
        assert_eq!(len, Some(4));
        // Row 0 is the merge, against its first parent: side's rename.
        assert_eq!(w.selected_row, Some(0));
        assert_eq!(w.kinds(), [Kind::Renamed]);
        assert_eq!(w.files[0].label, "b.txt → c.txt");
        // Against the second parent: main's change to a.txt.
        w.choose_parent(1);
        pump(&mut w, &rx, |w| w.parent == 1 && !w.files.is_empty());
        assert_eq!(w.kinds(), [Kind::Modified]);
        assert_eq!(w.files[0].path, "a.txt");
    }

    #[test]
    fn changes_list_staged_unstaged_and_untracked_files() {
        let Some(dir) = test_repo("git-pane-changes") else {
            return;
        };
        let (mut w, rx) = window(&dir.0);
        w.headless_load(&rx, Some("changes"));
        assert_eq!(w.selected, Selected::Changes);
        let got: Vec<_> = w
            .files
            .iter()
            .map(|f| (f.kind, f.path.as_str(), f.staged))
            .collect();
        assert_eq!(
            got,
            [
                (Kind::Modified, "a.txt", Some(true)),
                (Kind::Modified, "a.txt", Some(false)),
                (Kind::Untracked, "new.txt", Some(false)),
            ]
        );
        // The unstaged side reads the file from disk.
        assert_eq!(w.files[1].sides.1, Source::Disk("a.txt".to_owned()));
    }

    /// The index's copy of `path`.
    fn staged(dir: &Path, path: &str) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["show", &format!(":{path}")])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    }

    /// Wait for the change running to finish and the status read after it.
    fn settle(w: &mut GitWindow, rx: &mpsc::Receiver<GitEvent>) {
        pump(w, rx, |w| w.busy.is_none() && w.committing.is_none());
        // The status read the change started.
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            if let Ok(e) = rx.recv_timeout(Duration::from_millis(50)) {
                w.event(e);
            }
        }
    }

    #[test]
    fn stage_discard_undo_and_commit_from_the_window() {
        let Some(dir) = test_repo("git-pane-staging") else {
            return;
        };
        let (mut w, rx) = window(&dir.0);
        w.headless_load(&rx, Some("changes"));
        // Stage new.txt with its own button.
        let new = w.files.iter().position(|f| f.path == "new.txt").unwrap();
        w.file_button(new, GitCmd::StageFile);
        settle(&mut w, &rx);
        assert_eq!(staged(&dir.0, "new.txt"), "new\n");
        assert!(
            w.take_said()
                .is_some_and(|(t, e)| !e && t.contains("Staged"))
        );
        // Discard a.txt's unstaged change, after the prompt says yes.
        let a = w
            .files
            .iter()
            .position(|f| f.path == "a.txt" && f.staged == Some(false))
            .unwrap();
        w.file_button(a, GitCmd::DiscardFile);
        assert!(w.prompt.is_some(), "discarding asks first");
        w.answered(crate::prompt::Answer::Yes);
        settle(&mut w, &rx);
        assert_eq!(
            std::fs::read_to_string(dir.0.join("a.txt")).unwrap(),
            "staged\n"
        );
        // Undo Discard puts the working-tree text back.
        w.git_command(GitCmd::UndoDiscard);
        settle(&mut w, &rx);
        assert_eq!(
            std::fs::read_to_string(dir.0.join("a.txt")).unwrap(),
            "unstaged\n"
        );
        // Commit what is staged.
        w.message.set_text("Add new.txt");
        w.git_command(GitCmd::Commit);
        settle(&mut w, &rx);
        assert!(w.message.is_blank(), "a commit clears the box");
        let log = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir.0)
            .args(["log", "-1", "--format=%s"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "Add new.txt");
        assert!(
            w.files.iter().all(|f| f.staged != Some(true)),
            "nothing staged now"
        );
    }

    /// Whether the compare has its changes.
    fn compared(w: &GitWindow) -> bool {
        matches!(&w.diff, Some(DiffPane::Compare { compare, .. }) if compare.hunks().is_some())
    }

    #[test]
    fn a_hunk_and_picked_lines_stage_from_the_compare() {
        let Some(dir) = test_repo("git-pane-hunk") else {
            return;
        };
        dir.write("b.txt", "1\n2\n3\n4\n5\n6\n7\n8\n9\n");
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&dir.0)
                .args(args)
                .output()
                .unwrap()
        };
        git(&["reset", "-q"]);
        git(&["add", "b.txt"]);
        git(&["commit", "-q", "-m", "numbers"]);
        dir.write("b.txt", "1\nTWO\n3\n4\n5\n6\n7\nEIGHT\n9\n");
        let (mut w, rx) = window(&dir.0);
        w.headless_load(&rx, Some("changes"));
        let b = w
            .files
            .iter()
            .position(|f| f.path == "b.txt" && f.staged == Some(false))
            .unwrap();
        w.select_file(b);
        pump(&mut w, &rx, compared);
        // The first change is the current one: stage only it.
        w.git_command(GitCmd::StageHunk);
        settle(&mut w, &rx);
        assert_eq!(staged(&dir.0, "b.txt"), "1\nTWO\n3\n4\n5\n6\n7\n8\n9\n");
        // The rest is still unstaged.
        let b = w
            .files
            .iter()
            .position(|f| f.path == "b.txt" && f.staged == Some(false))
            .unwrap();
        w.select_file(b);
        pump(&mut w, &rx, compared);
        // Nothing picked: Stage Lines says so and changes nothing.
        w.take_said();
        w.git_command(GitCmd::StageLines);
        assert!(w.take_said().is_some());
        assert_eq!(w.busy, None);
        // Pick the remaining change's rows and stage them.
        let rows = match &w.diff {
            Some(DiffPane::Compare { compare, .. }) => compare.change_rows(0).unwrap(),
            _ => unreachable!(),
        };
        w.picked = Some((rows.start, rows.end - 1));
        w.git_command(GitCmd::StageLines);
        settle(&mut w, &rx);
        assert_eq!(staged(&dir.0, "b.txt"), "1\nTWO\n3\n4\n5\n6\n7\nEIGHT\n9\n");
        // And unstage the first change again from the staged row.
        let s = w
            .files
            .iter()
            .position(|f| f.path == "b.txt" && f.staged == Some(true))
            .unwrap();
        w.select_file(s);
        pump(&mut w, &rx, compared);
        w.git_command(GitCmd::UnstageHunk);
        settle(&mut w, &rx);
        assert_eq!(staged(&dir.0, "b.txt"), "1\n2\n3\n4\n5\n6\n7\nEIGHT\n9\n");
    }

    #[test]
    fn outside_a_repository_says_so() {
        let dir = TestDir::new("git-pane-outside");
        let (mut w, rx) = window(&dir.0);
        w.headless_load(&rx, None);
        assert!(w.error.is_some());
    }
}
