//! The application: windows events in, editor commands out.
//!
//! `winit` drives the program through an [`ApplicationHandler`]: we hand it a
//! struct, and it calls back into us when the OS has events. Windows may only
//! be created once the app is `resumed`, which is why creation lives there and
//! not in `main`. Nothing redraws on a timer except the cursor blink (and
//! short retries), so an idle editor uses no CPU.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rfd::{FileDialog, MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{CursorIcon, Window, WindowId};

use nimbus_core::line_index::IndexChunk;
use nimbus_core::{Buffer, LineEnding, Text};
use nimbus_render::{Font, FrameStatus, Gpu, GpuContext, GpuError};
use nimbus_syntax::state::{Job, JobResult, MAX_HIGHLIGHT_BYTES, Off};

use crate::columns::Click;
use crate::compare::{Compare, DiffResult, Other, Toggle};
use crate::compare_panel;
use crate::config::{self, Config, Whitespace};
use crate::debug::Debug;
use crate::docs::{DocId, Docs};
use crate::document::{Carets, DiskChange, Document, Editor, MAX_COPY_BYTES, Occurrence, Solo};
use crate::editing::Outcome;
use crate::encoding::{Choice, Encoding};
use crate::find::{self, Field, FindBar, SearchBatch};
use crate::find_files::{self, Batch as FilesBatch};
use crate::folder_compare;
use crate::folding;
use crate::git;
use crate::groups::{self, Groups};
use crate::input::{self, Command, Motion, Resolved};
use crate::io;
use crate::keymap::{Chord, Keymap, chord_label};
use crate::languages::Languages;
use crate::menu::{self, MenuBar};
use crate::merge_view::{Inputs, MergeView, Merged, Shape, Take};
use crate::minimap;
use crate::picker::{self, EncodingFor, Item, Kind, Picker};
use crate::replace_files::{self, Replacement};
use crate::saver::{self, ReplacePolicy, SaveEvent, SaveStep};
use crate::search_panel::{Focus, Reply};
use crate::session;
use crate::sidebar::{self, Mode, Sidebar};
use crate::state::State;
use crate::theme::{Theme, Ui};
use crate::tree::{self, Entry, ListRequest};
use crate::update::{self, UpdateError};
use crate::view::{self, Decorations, Layout, PickerView, Preedit, StatusItem, TabLabel, View};
use crate::watch::{Change, Watch};
use crate::{MergeArgs, Options, effective_scale, print_err, print_out, save_screenshot, to_f32};

mod geometry;
mod tail;
mod tree_ops;
mod workspace;
mod workspaces;

pub use workspace::Startup;
use workspace::{Pending, Workspace, configure_tabs, startup_message};

/// Events sent to the event loop from other threads.
#[derive(Debug)]
pub enum AppEvent {
    /// Results from a background search.
    Search(SearchBatch),
    /// A system font for a character the bundled font lacks has been
    /// loaded: draw again to use it.
    FontLoaded,
    /// The worker looking up the config's `font` finished: the name it
    /// looked for, and the font if found.
    EditorFont(String, Option<nimbus_render::EditorFont>),
    /// The worker looking up the config's `ui_font` finished (M20).
    UiFont(String, Option<nimbus_render::EditorFont>),
    /// The picker's matcher has new results.
    PickerChanged,
    /// A menu item was chosen; carries its action id.
    Menu(String),
    /// A background parse finished.
    Syntax(JobResult),
    /// A sidebar folder listing finished.
    Listing(ListRequest, Result<Vec<Entry>, String>),
    /// Files or folders changed on disk.
    Disk(Vec<Change>),
    /// The file watcher could not watch everything asked of it.
    WatchWarning(String),
    /// A chunk of a huge file's line index, for the document with that
    /// indexing token, and why counting stopped if a read failed (the
    /// file was cut short).
    Index(u64, IndexChunk, Option<String>),
    /// Progress or the end of saving a huge file ([`crate::saver`]).
    Save(SaveEvent),
    /// Hits from a find-in-files worker.
    FindFiles(FilesBatch),
    /// A Replace in Files finished writing the files on disk.
    Replaced(replace_files::Report),
    /// A compare's diff finished on a worker (M9).
    Diff(DiffResult),
    /// A merge's chunks (and result text) finished on a worker (M9).
    Merged(Merged),
    /// A restored tab's file was read on a worker: the document for the
    /// placeholder with this path, or why it could not be read.
    Restored(PathBuf, Result<Box<Document>, String>),
    /// Recent files found missing on a worker.
    RecentMissing(Vec<PathBuf>),
    /// A file operation from the tree finished on its worker.
    FileOp(crate::tree_ops::Op, Result<(), String>),
    /// A check for updates finished; `true` when it was the startup
    /// check, which stays quiet unless a newer release exists.
    Update(Result<update::Outcome, UpdateError>, bool),
    /// A Fold All or Fold Level N scan finished on a worker (M13).
    Folded(folding::FoldResult),
    /// `git` read a file at a revision for Compare with HEAD (M18).
    Revision(Revision),
    /// Rows from a folder compare's workers (M18).
    FolderCompare(folder_compare::Batch),
}

/// A file read at a git revision on a worker (M18).
#[derive(Debug)]
pub struct Revision {
    /// The file (the tab's path) it was read for.
    path: PathBuf,
    /// The revision, as asked for.
    rev: String,
    /// Its contents, or why git could not give them.
    outcome: Result<Vec<u8>, git::GitError>,
}

/// Ask GitHub for the latest release on its own thread.
fn spawn_update_check(proxy: &EventLoopProxy<AppEvent>, at_startup: bool) {
    let proxy = proxy.clone();
    let spawned = std::thread::Builder::new()
        .name("nimbus-update".to_owned())
        .spawn(move || {
            let result = update::check(env!("CARGO_PKG_VERSION"));
            let _ = proxy.send_event(AppEvent::Update(result, at_startup));
        });
    if let Err(e) = spawned {
        log::error!("could not start the update check: {e}");
    }
}

/// List a folder for the sidebar on its own thread.
fn spawn_listing(proxy: &EventLoopProxy<AppEvent>, req: ListRequest) {
    let proxy = proxy.clone();
    let spawned = std::thread::Builder::new()
        .name("nimbus-list".to_owned())
        .spawn(move || {
            let result = tree::list_dir(&req.path, req.show_ignored).map_err(|e| e.to_string());
            let _ = proxy.send_event(AppEvent::Listing(req, result));
        });
    if let Err(e) = spawned {
        log::error!("could not start a listing thread: {e}");
    }
}

/// The least time between automatic reloads of one file changed on disk.
const RELOAD_GAP: Duration = Duration::from_secs(1);

/// Time a reparse may take on the main thread before it moves to a worker
/// (DESIGN.md §4.2). Typing reparses take microseconds.
const SYNTAX_BUDGET: Duration = Duration::from_millis(2);

/// Run a parse job on its own thread and send the result back. Jobs are
/// rare (the first parse of a file, or a reparse that ran over budget), so
/// a thread each is simpler than a pool and costs nothing while idle.
fn spawn_syntax(proxy: &EventLoopProxy<AppEvent>, job: Job) {
    // The job waits in a slot the thread takes it from, so that if the
    // thread cannot start, it is still here to run.
    let slot = Arc::new(Mutex::new(Some(job)));
    let theirs = Arc::clone(&slot);
    let sender = proxy.clone();
    let spawned = std::thread::Builder::new()
        .name("nimbus-parse".to_owned())
        .spawn(move || {
            if let Some(job) = theirs.lock().ok().and_then(|mut j| j.take()) {
                let _ = sender.send_event(AppEvent::Syntax(job.run()));
            }
        });
    if let Err(e) = spawned {
        // The document is waiting for this result; without it, it would
        // never be coloured again. Parse here instead, once.
        log::error!("could not start a parse thread ({e}); parsing on the main thread");
        if let Some(job) = slot.lock().ok().and_then(|mut j| j.take()) {
            let _ = proxy.send_event(AppEvent::Syntax(job.run()));
        }
    }
}

/// A short note at the bottom of the window that disappears by itself.
pub struct Message {
    text: String,
    is_error: bool,
    until: Instant,
}

/// Pause in typing after which the find bar searches the edited text again.
const RESEARCH_DELAY: Duration = Duration::from_millis(150);

/// Pause in typing in the Search panel before the project is searched.
const SEARCH_FILES_DELAY: Duration = Duration::from_millis(300);

/// What the last Replace in Files can put back.
#[derive(Debug, Default)]
struct ReplaceUndo {
    /// Files on disk, with their old contents.
    saved: Vec<replace_files::Saved>,
    /// Open tabs, by canonical path, with the history id their replace
    /// left on top: undone only while it is still there.
    tabs: Vec<(PathBuf, u64)>,
}

/// How long `--smoke` and `--screenshot` wait for a first frame.
const UNATTENDED_TIMEOUT: Duration = Duration::from_secs(10);

/// Menu events for the same item closer together than this are key
/// repeats from a held shortcut. Auto-repeat runs at about 30 ms; a
/// deliberate second press is rarely this fast.
const MENU_REPEAT_GAP: Duration = Duration::from_millis(250);

/// How long a message stays on screen.
const MESSAGE_TIME: Duration = Duration::from_secs(4);

/// The smallest and largest editor font sizes the zoom reaches, in points:
/// the same range the config's `font_size` accepts.
const MIN_ZOOM: f64 = 6.0;
const MAX_ZOOM: f64 = 72.0;

/// The file watcher, reporting changes and warnings to the event loop.
fn disk_watch(proxy: &EventLoopProxy<AppEvent>) -> Watch {
    let disk_proxy = proxy.clone();
    let warn_proxy = proxy.clone();
    Watch::new(
        move |changes| {
            let _ = disk_proxy.send_event(AppEvent::Disk(changes));
        },
        move |warning| {
            let _ = warn_proxy.send_event(AppEvent::WatchWarning(warning));
        },
    )
}

/// The font size a session starts at: the zoom the last one left in
/// `state.toml`, unless `remember_zoom = false`, else the config's.
fn starting_zoom(config: &Config, state: &State) -> f64 {
    state
        .font_size
        .filter(|_| config.remember_zoom)
        .map_or(config.font_size, |s| s.clamp(MIN_ZOOM, MAX_ZOOM))
}

/// How long the first chord of a two-chord binding waits for the second.
const CHORD_TIMEOUT: Duration = Duration::from_secs(2);

/// One open file: its text and history, and where it is scrolled to.
pub struct Tab {
    /// The document it shows, in [`App::docs`].
    pub doc: DocId,
    /// The editor group whose strip it is in (M13).
    pub group: usize,
    /// A Fold All or Fold Level N waiting for its worker.
    fold_job: Option<folding::FoldAll>,
    /// This tab's cursors and selections in it.
    pub carets: Carets,
    /// Its scroll position and wrap setting.
    pub view: View,
    /// The "too large to highlight" note has been shown.
    noted_size: bool,
    /// What to do when the save of a huge file under way ends.
    after_save: AfterSave,
    /// The answer "Don't Save" during a quit that is waiting for another
    /// tab's save: the history's top id then, so the question is not asked
    /// again when the quit goes on, unless the text changed since.
    discard_at: Option<u64>,
    /// A compare view on this tab (M9): the other side and the diff.
    pub compare: Option<Box<Compare>>,
    /// A merge view on this tab (M9): the inputs and the regions of the
    /// result, which is this tab's document.
    pub merge: Option<Box<MergeView>>,
    /// Where a restored tab's cursor and view go once its file is read.
    pending: Option<Pending>,
}

/// The cursors of every tab showing `doc`.
fn carets_of(tabs: &mut [Tab], doc: DocId) -> Vec<&mut Carets> {
    tabs.iter_mut()
        .filter(|t| t.doc == doc)
        .map(|t| &mut t.carets)
        .collect()
}

/// Whether a tab for `doc` opens with soft wrap: the language's setting
/// (`[editor.markdown] wrap`, on for Markdown by default) over the
/// top-level `soft_wrap`.
fn wrap_for(config: &Config, doc: &Document) -> bool {
    config
        .editor
        .for_language(doc.language_key())
        .wrap
        .unwrap_or(config.soft_wrap)
}

impl Tab {
    /// A tab showing `doc`, wrapped if `wrap`.
    #[must_use]
    pub fn new(doc: DocId, wrap: bool) -> Self {
        let mut view = View::default();
        view.set_wrap(wrap);
        Self {
            doc,
            group: 0,
            fold_job: None,
            carets: Carets::default(),
            view,
            noted_size: false,
            after_save: AfterSave::default(),
            discard_at: None,
            compare: None,
            merge: None,
            pending: None,
        }
    }

    /// Put the cursor and the view where a restored session left them,
    /// once `doc`, this tab's document, is read.
    fn apply_pending(&mut self, doc: &mut Document) {
        if doc.is_placeholder() {
            return;
        }
        let Some(p) = self.pending.take() else { return };
        let t = &doc.buffer;
        let (cursor, scroll) = (
            session::clamp_offset(t, p.cursor),
            session::clamp_offset(t, p.scroll),
        );
        // Saved folds are byte ranges into the file as it was: kept only
        // when its size and time say it is the same file, and then only
        // the ones that still run from a line's end to a later line's end.
        let folds: Vec<std::ops::Range<usize>> = if p.disk.is_some() && doc.disk_stamp() == p.disk {
            p.folds
                .into_iter()
                .filter(|f| f.end <= t.len_bytes())
                .filter(|f| nimbus_core::fold::lines_range(t, f.start, f.end) == Some(f.clone()))
                .collect()
        } else {
            Vec::new()
        };
        Editor::new(doc, &mut self.carets).click(cursor, false);
        self.view.scroll_top = scroll;
        if !folds.is_empty() {
            self.view.follow(doc);
            self.view.folds.fold_many(folds);
            self.view.folds.reveal(cursor);
        }
    }
}

/// What to do when a huge file's save, which runs on a worker, ends.
#[derive(Debug, Default)]
struct AfterSave {
    /// Set for Save As.
    save_as: Option<SaveAs>,
    /// Close the tab once saved (Close was chosen while saving).
    close: bool,
}

/// A Save As under way.
#[derive(Debug)]
struct SaveAs {
    /// The name before, to go back to if the save fails.
    old_path: Option<PathBuf>,
}

/// Saves of huge files, which run on workers, and what waits for them.
#[derive(Debug, Default)]
struct Saves {
    /// Quit once the save under way has finished (Quit was chosen while a
    /// huge file was being saved).
    quit_after: bool,
    /// Check again at this time whether a written save can replace its
    /// file (Windows waits for the old file to be released).
    poll_at: Option<Instant>,
    /// A search was stopped so a save could replace its file; search again
    /// when the save ends.
    search_stopped: bool,
}

/// What became of a request to save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Saved {
    /// Saved.
    Yes,
    /// A huge file's save started on a worker; it reports when done.
    Started,
    /// Not saved: failed, or the user cancelled the dialog.
    No,
}

/// The answer to "save changes before closing?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Discard {
    /// Go ahead: saved, nothing to save, or "Don't Save".
    Yes,
    /// Keep the tab: cancelled, or the save failed.
    Cancel,
    /// A save is under way; go ahead once it has finished.
    Wait,
}

/// Cursor blink timing.
///
/// The cursor shows solid on any input and toggles every 530 ms after that.
/// After 10 s without input it stays solid and stops scheduling wakeups, so
/// an idle editor costs no CPU at all.
pub(crate) struct Blink {
    visible: bool,
    last_input: Instant,
    next: Option<Instant>,
}

const BLINK_PERIOD: Duration = Duration::from_millis(530);
const BLINK_IDLE: Duration = Duration::from_secs(10);

impl Blink {
    fn new(now: Instant) -> Self {
        Self {
            visible: true,
            last_input: now,
            next: Some(now + BLINK_PERIOD),
        }
    }

    fn reset(&mut self, now: Instant) {
        self.visible = true;
        self.last_input = now;
        self.next = Some(now + BLINK_PERIOD);
    }

    /// Stop blinking, showing the cursor solid (window in the background).
    fn stop(&mut self) {
        self.visible = true;
        self.next = None;
    }

    /// Advance the blink; returns whether visibility changed.
    fn tick(&mut self, now: Instant) -> bool {
        let Some(due) = self.next else { return false };
        if now < due {
            return false;
        }
        if now.duration_since(self.last_input) >= BLINK_IDLE {
            self.next = None;
            let changed = !self.visible;
            self.visible = true;
            return changed;
        }
        self.visible = !self.visible;
        self.next = Some(now + BLINK_PERIOD);
        true
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "unrelated flags of the running editor, not one state"
)]
pub struct App {
    options: Options,
    started: Instant,
    config: Config,
    theme: Theme,
    /// Which language each file is, from the config and config directory.
    languages: Languages,
    keymap: Keymap,
    /// Folder the file opener searches.
    root: PathBuf,
    /// GPU setup running on a background thread since the start of `main`.
    /// Taken (left `None`) when the window joins it in `resumed`.
    gpu_init: Option<JoinHandle<Result<GpuContext, GpuError>>>,
    /// Declared before the window so it is dropped first: fields drop in
    /// declaration order, and the menu is attached to the window's handle.
    menu_bar: Option<MenuBar>,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    /// Modifier keys currently held, tracked from `ModifiersChanged`.
    modifiers: ModifiersState,
    /// Process exit status once the event loop ends; non-zero when a
    /// requested `--screenshot` could not be saved.
    pub exit_code: i32,
    /// `--merge`: whether the merge's result has been saved with no
    /// conflicts left, which decides the exit code (0 then, else 1).
    merge_saved_clean: Option<bool>,
    frames: u64,
    /// The open documents, which tabs name.
    docs: Docs,
    /// The editor groups side by side; tabs say which one they are in.
    groups: Groups,
    /// The tab, document version and line the fold marker was last worked
    /// out for under the pointer.
    fold_hover_key: Option<(usize, u64, usize)>,
    tabs: Vec<Tab>,
    active: usize,
    blink: Blink,
    preedit: Option<Preedit>,
    /// The debug overlay and the timings it shows.
    debug: Debug,
    /// Last mouse position in physical pixels; clicks carry no position.
    mouse: PhysicalPosition<f64>,
    /// Last rectangle sent to the IME, to avoid resending it every frame.
    ime_area: Option<[i32; 4]>,
    /// What the window title was last built from: active tab and dirty
    /// state. The title is rebuilt only when these change.
    title_key: Option<(usize, bool)>,
    /// While the window reports itself hidden, retry drawing at this time.
    /// macOS can report "occluded" for the first frames of a new window
    /// without ever sending the matching "visible again" event, so waiting
    /// for that event alone could leave the window blank.
    hidden_retry: Option<(Instant, Duration)>,
    /// When a scrollbar shown by a recent scroll hides (M16): the one
    /// wakeup that draws the frame without it.
    scrollbar_hide_at: Option<Instant>,
    /// Connected on first copy or paste, not at startup.
    clipboard: Option<arboard::Clipboard>,
    clicks: Clicks,
    find: FindBar,
    /// Where the editor's selection started when the query last changed:
    /// incremental search shows the first match from here.
    find_origin: usize,
    /// Jump to the first match once background results arrive.
    find_pending_jump: bool,
    picker: Option<Picker>,
    message: Option<Message>,
    proxy: EventLoopProxy<AppEvent>,
    /// Search again at this time: edits happened while the find bar was
    /// open.
    research_at: Option<Instant>,
    /// A palette action chosen by mouse, run once the click is handled.
    pending_action: Option<Command<'static>>,
    /// The first chord of a two-chord binding, pressed and waiting for
    /// the second until the deadline (M11).
    pending_chord: Option<(Chord, Instant)>,
    /// The last menu event and when it arrived, to drop auto-repeats.
    last_menu: Option<(String, Instant)>,
    /// The project tree.
    sidebar: Sidebar,
    /// Changes on disk to the sidebar's folders and open files.
    watch: Watch,
    /// The editor font size in points for this session: the config's
    /// `font_size`, changed by the zoom actions (M16) and remembered in
    /// `state.toml` unless `remember_zoom = false`.
    font_size: f64,
    /// Where whitespace shows: the config's `render_whitespace`, cycled
    /// from the palette.
    whitespace: Whitespace,
    /// Indent guides on: the config's `indent_guides`, toggled from the
    /// palette.
    indent_guides: bool,
    /// Sticky scroll on: the config's `sticky_scroll`, toggled from the
    /// palette (M20).
    sticky_scroll: bool,
    /// The minimap shown: the config's `minimap`, toggled from the
    /// palette (M20).
    minimap: bool,
    /// When the last zoom step started, until its first frame is drawn:
    /// that frame refills the atlas, and its time is logged.
    zoom_started: Option<Instant>,
    /// What is remembered between runs, and the session's bookkeeping.
    ws: Workspace,
    /// The pointer shows the resize cursor.
    resize_cursor: bool,
    /// Watching has started (after the first frame).
    watching: bool,
    /// A go-to-line waiting for a huge file's lines to be counted.
    pending_line: Option<usize>,
    /// Open files that changed on disk too soon after their last reload,
    /// to look at again at `recheck_at`.
    recheck: Vec<PathBuf>,
    recheck_at: Option<Instant>,
    /// Saves of huge files under way, and what waits for them.
    saves: Saves,
    /// Search the project at this time: the Search panel's query changed.
    search_files_at: Option<Instant>,
    /// What "Undo Replace in Files" would put back.
    replace_undo: Option<ReplaceUndo>,
    /// The open-tab part of a Replace in Files, until its files on disk
    /// are written and the summary can be shown.
    replace_summary: ReplaceSummary,
}

/// Multi-click detection and drag state. winit reports presses, not
/// double-clicks, so the count is worked out from timing and distance.
#[derive(Default)]
struct Clicks {
    count: u8,
    last: Option<(Instant, (f32, f32))>,
    /// What the initial click selected (a point, word or line); dragging
    /// extends from it in whole units.
    anchor: std::ops::Range<usize>,
    dragging: bool,
    /// The press held Alt: it added a cursor, and dragging extends that
    /// one instead of replacing every selection.
    adding: bool,
    /// Dragging the scrollbar's thumb, held this far below its top.
    scrollbar_grab: Option<f32>,
    /// Dragging the minimap's mark (M20), held this far below its top.
    minimap_grab: Option<f32>,
    /// A Shift+Alt press started a column selection here: the line's first
    /// byte and the display column.
    column: Option<(usize, usize)>,
}

const MULTI_CLICK: Duration = Duration::from_millis(500);

impl Clicks {
    /// Register a press and return the click count: 1, 2 or 3 (and back
    /// to 1 after a triple-click).
    fn press(&mut self, now: Instant, at: (f32, f32), slop: f32) -> u8 {
        let near = |p: (f32, f32)| (p.0 - at.0).abs() <= slop && (p.1 - at.1).abs() <= slop;
        self.count = match self.last {
            Some((t, p)) if now.duration_since(t) < MULTI_CLICK && near(p) && self.count < 3 => {
                self.count + 1
            }
            _ => 1,
        };
        self.last = Some((now, at));
        self.count
    }
}

impl App {
    /// The app before its window exists. `docs` become tabs (at least one);
    /// `warnings` are shown once the window is up.
    pub fn new(
        options: Options,
        started: Instant,
        gpu_init: JoinHandle<Result<GpuContext, GpuError>>,
        (config, languages, docs, root): (Config, Languages, Vec<Document>, PathBuf),
        startup: Startup,
        proxy: EventLoopProxy<AppEvent>,
        mut warnings: Vec<String>,
    ) -> Self {
        let Startup {
            mut state,
            restore,
            session_root,
            more_roots,
            workspace,
        } = startup;
        let session_root = session_root.filter(|_| !options.unattended());
        let debug_overlay = options.debug_overlay;
        let (keymap, key_warnings) = Keymap::new(&config.keys);
        let (theme, theme_warnings) = Theme::load(&config.theme);
        let watch = disk_watch(&proxy);
        let mut open = Docs::default();
        let (mut sidebar, (mut tabs, groups, active, note)) = workspaces::startup_tabs_and_roots(
            (&root, docs, restore),
            (more_roots, workspace, session_root.as_deref()),
            (&mut state, &mut open, &mut warnings),
            options.wrap || config.soft_wrap,
        );
        configure_tabs(&mut tabs, &mut open, &config, options.wrap);
        // `--search`: the Search panel open, searching once the window is
        // up (the first frame schedules it like a typed query).
        let search_files_at = options.search.as_ref().map(|pattern| {
            sidebar.visible = true;
            sidebar.mode = Mode::Search;
            sidebar.search.focus_query(Some(pattern.clone()));
            started
        });
        let all_warnings: Vec<String> = warnings
            .into_iter()
            .chain(key_warnings)
            .chain(theme_warnings)
            .collect();
        let message = startup_message(all_warnings, note);
        Self {
            options,
            started,
            font_size: starting_zoom(&config, &state),
            whitespace: config.render_whitespace,
            indent_guides: config.indent_guides,
            sticky_scroll: config.sticky_scroll,
            minimap: config.minimap,
            config,
            theme,
            languages,
            keymap,
            root,
            gpu_init: Some(gpu_init),
            window: None,
            gpu: None,
            menu_bar: None,
            modifiers: ModifiersState::empty(),
            exit_code: 0,
            merge_saved_clean: None,
            frames: 0,
            docs: open,
            groups,
            fold_hover_key: None,
            tabs,
            active,
            blink: Blink::new(started),
            preedit: None,
            debug: Debug::new(debug_overlay),
            mouse: PhysicalPosition::new(0.0, 0.0),
            ime_area: None,
            title_key: None,
            hidden_retry: None,
            scrollbar_hide_at: None,
            clipboard: None,
            clicks: Clicks::default(),
            find: FindBar::default(),
            find_origin: 0,
            find_pending_jump: false,
            picker: None,
            message,
            proxy,
            research_at: None,
            pending_action: None,
            pending_chord: None,
            last_menu: None,
            sidebar,
            watch,
            zoom_started: None,
            ws: Workspace::new(state, session_root),
            resize_cursor: false,
            watching: false,
            pending_line: None,
            recheck: Vec::new(),
            recheck_at: None,
            saves: Saves::default(),
            search_files_at,
            replace_undo: None,
            replace_summary: ReplaceSummary::default(),
        }
    }

    fn tab_width(&self) -> usize {
        self.config.tab_width
    }

    /// Set a document's indentation from the config and, when it says to
    /// detect, from the file: after opening, renaming or reloading the
    /// config.
    fn configure_indent(&self, doc: &mut Document) {
        doc.configure_indent(self.config.indent.for_language(doc.language_key()));
    }

    /// The editing settings for the active document's language.
    fn editing(&self) -> config::Editing {
        self.config.editor.for_language(self.doc().language_key())
    }

    /// Tell the user what an M11 operation refused, if it did.
    fn report(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Done | Outcome::Nothing => {}
            Outcome::TooLarge => self.say("Selection too large for this operation", true),
            Outcome::NoComment => self.say("This language has no comment syntax", true),
        }
    }

    fn doc(&self) -> &Document {
        &self.docs[self.tabs[self.active].doc]
    }

    fn doc_mut(&mut self) -> &mut Document {
        &mut self.docs[self.tabs[self.active].doc]
    }

    /// Reload the active tab's file from disk, on request.
    fn reload_active(&mut self) {
        let id = self.tabs[self.active].doc;
        let mut carets = carets_of(&mut self.tabs, id);
        match self.docs[id].reload(&self.languages, &mut carets) {
            Ok(()) => self.say("Reloaded from disk", false),
            Err(e) => self.say(format!("Cannot reload: {e}"), true),
        }
    }

    /// The active tab's document and cursors, to edit.
    fn ed(&mut self) -> Editor<'_> {
        let tab = &mut self.tabs[self.active];
        Editor::new(&mut self.docs[tab.doc], &mut tab.carets)
    }

    /// The active tab's cursors.
    fn carets(&self) -> &Carets {
        &self.tabs[self.active].carets
    }

    /// The active tab's cursor, snapped to its text.
    fn cursor(&self) -> usize {
        let tab = &self.tabs[self.active];
        tab.carets.cursor(&self.docs[tab.doc].buffer)
    }

    /// The active tab's primary selection, snapped to its text.
    fn selection(&self) -> nimbus_core::Selection {
        let tab = &self.tabs[self.active];
        tab.carets.selection(&self.docs[tab.doc].buffer)
    }

    fn font_px(&self, scale: f64) -> f32 {
        to_f32(self.font_size * scale)
    }

    /// Settle a newly opened GPU: tab width, ligatures, and a redraw when
    /// a fallback font arrives.
    fn gpu_ready(&mut self, mut gpu: Gpu) {
        gpu.set_tab_width(self.tab_width());
        gpu.set_ligatures(self.config.ligatures);
        let proxy = self.proxy.clone();
        gpu.set_font_notify(Arc::new(move || {
            let _ = proxy.send_event(AppEvent::FontLoaded);
        }));
        self.gpu = Some(gpu);
    }

    /// Look up the config's `font` on a worker (listing the system's fonts
    /// takes too long for the main thread); [`App::editor_font_arrived`]
    /// swaps it in. An empty setting goes back to the bundled font at once.
    fn load_editor_font(&mut self) {
        let name = self.config.font.clone();
        if name.is_empty() {
            if let (Some(gpu), Ok(regular)) = (self.gpu.as_mut(), Font::bundled()) {
                gpu.set_font(nimbus_render::EditorFont {
                    regular,
                    bold: Font::bundled_bold().ok(),
                });
            }
            return;
        }
        let proxy = self.proxy.clone();
        let spawned = std::thread::Builder::new()
            .name("nimbus-font".to_owned())
            .spawn(move || {
                let found = nimbus_render::find_editor_font(&name);
                let _ = proxy.send_event(AppEvent::EditorFont(name, found));
            });
        if let Err(e) = spawned {
            log::warn!("cannot look up the font: {e}");
        }
    }

    /// Look up the config's `ui_font` on a worker, as the editor font is
    /// (the system's font list takes tens of milliseconds); an empty name
    /// goes back to the grid at once.
    fn load_ui_font(&mut self) {
        let name = self.config.ui_font.clone();
        if name.is_empty() {
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.set_ui_font(None);
            }
            return;
        }
        let proxy = self.proxy.clone();
        let spawned = std::thread::Builder::new()
            .name("nimbus-ui-font".to_owned())
            .spawn(move || {
                let found = nimbus_render::find_editor_font(&name);
                let _ = proxy.send_event(AppEvent::UiFont(name, found));
            });
        if let Err(e) = spawned {
            log::warn!("cannot look up the interface font: {e}");
        }
    }

    /// The configured interface font was looked up: draw the chrome with
    /// it, unless the config changed again meanwhile.
    fn ui_font_arrived(&mut self, name: &str, found: Option<nimbus_render::EditorFont>) {
        if name != self.config.ui_font {
            return;
        }
        match (found, self.gpu.as_mut()) {
            (Some(font), Some(gpu)) => {
                gpu.set_ui_font(Some(font.regular));
                self.changed(false);
            }
            (None, _) => self.say(
                format!("Interface font {name:?} not found; using the editor font"),
                true,
            ),
            (Some(_), None) => {}
        }
    }

    /// The configured font was looked up: draw with it, unless the config
    /// changed again meanwhile.
    fn editor_font_arrived(&mut self, name: &str, found: Option<nimbus_render::EditorFont>) {
        if name != self.config.font {
            return;
        }
        match (found, self.gpu.as_mut()) {
            (Some(font), Some(gpu)) => {
                gpu.set_font(font);
                self.changed(false);
            }
            (None, _) => self.say(format!("Font {name:?} not found; using Hack"), true),
            (Some(_), None) => {}
        }
    }

    /// Zoom the editor to `size` points (clamped to 6–72): the atlas is
    /// refilled for the new size, and the view keeps its top line because
    /// its anchor is a byte, not a pixel offset. Remembered in
    /// `state.toml` unless `remember_zoom = false`.
    fn zoom_to(&mut self, size: f64) {
        let size = size.clamp(MIN_ZOOM, MAX_ZOOM);
        if (size - self.font_size).abs() < f64::EPSILON {
            return;
        }
        let ratio = size / self.font_size;
        self.font_size = size;
        if let (Some(gpu), Some(window)) = (self.gpu.as_mut(), self.window.as_ref()) {
            let started = Instant::now();
            gpu.set_font_size(to_f32(size * effective_scale(window.scale_factor())));
            log::debug!("zoom to {size} pt: atlas reset in {:?}", started.elapsed());
        }
        // The anchor byte stays; the part-row offset and the sideways
        // scroll are in pixels of the old size.
        for tab in &mut self.tabs {
            tab.view.scroll_px = 0.0;
            tab.view.scroll_x *= ratio;
        }
        self.zoom_started = Some(Instant::now());
        self.say(format!("Zoom: {size} pt"), false);
        self.remember();
        self.changed(false);
    }

    /// Geometry for the whole window, if it exists, sized for the focused
    /// tab. The sidebar, the status bar and the overlays use it; the text
    /// of each group uses its own column, [`App::group_layout`].
    fn window_layout(&self) -> Option<Layout> {
        let (window, gpu) = (self.window.as_ref()?, self.gpu.as_ref()?);
        let scale = to_f32(effective_scale(window.scale_factor()));
        let lines = self.tab_lines(&self.tabs[self.active]);
        Some(layout_for_lines(gpu, scale, lines, &self.find).with_sidebar(self.sidebar.px(scale)))
    }

    /// The most lines any document on `tab` has: the gutter's width.
    fn tab_lines(&self, tab: &Tab) -> usize {
        let mut lines = line_estimate(&self.docs[tab.doc].buffer);
        if let Some(c) = &tab.compare {
            lines = lines.max(line_estimate(&c.other.buffer));
        }
        if let Some(m) = &tab.merge {
            lines = lines
                .max(line_estimate(&m.ours.buffer))
                .max(line_estimate(&m.theirs.buffer));
        }
        lines
    }

    /// Geometry for group `g`'s column: its tab strip, find bar and text,
    /// between the dividers. With one group it is the window's layout.
    fn group_layout(&self, g: usize) -> Option<Layout> {
        let (window, gpu) = (self.window.as_ref()?, self.gpu.as_ref()?);
        let scale = to_f32(effective_scale(window.scale_factor()));
        let tab = &self.tabs[self.group_active(g)];
        let lines = self.tab_lines(tab);
        let full = layout_for_lines(gpu, scale, lines, self.group_find(g))
            .with_sidebar(self.sidebar.px(scale));
        let column = if self.groups.len() == 1 {
            full
        } else {
            let &(a, b) = self.groups.columns(full.left, full.width).get(g)?;
            full.column(a, b)
        };
        // The minimap is for one document's text, not a compare's columns.
        if self.minimap && tab.compare.is_none() && tab.merge.is_none() {
            let width = to_f32(self.config.minimap_width) * scale;
            return Some(column.with_minimap(width));
        }
        Some(column)
    }

    /// Geometry for the focused group, where the keyboard's edits show.
    fn layout(&self) -> Option<Layout> {
        self.group_layout(self.groups.focused)
    }

    /// The left and right edges of every group's column.
    fn group_columns(&self, window: &Layout) -> Vec<(f32, f32)> {
        self.groups.columns(window.left, window.width)
    }

    /// Group `g`'s active tab, as an index into `tabs`.
    fn group_active(&self, g: usize) -> usize {
        if g == self.groups.focused {
            self.active
        } else {
            self.groups.list.get(g).map_or(self.active, |x| x.active)
        }
        .min(self.tabs.len() - 1)
    }

    /// Group `g`'s find bar.
    fn group_find(&self, g: usize) -> &FindBar {
        if g == self.groups.focused {
            &self.find
        } else {
            self.groups.list.get(g).map_or(&self.find, |x| &x.find)
        }
    }

    /// The tabs in group `g`'s strip, in order, as indices into `tabs`.
    fn group_tabs(&self, g: usize) -> Vec<usize> {
        (0..self.tabs.len())
            .filter(|&i| self.tabs[i].group == g)
            .collect()
    }

    /// Put the focused group's active tab and find bar in its slot, ready
    /// for another group to take the focus.
    fn park_focus(&mut self) {
        let f = self.groups.focused;
        if let Some(slot) = self.groups.list.get_mut(f) {
            slot.active = self.active;
            // The keyboard leaves the group, its find bar included.
            self.find.focused = false;
            std::mem::swap(&mut self.find, &mut slot.find);
        }
    }

    /// Take the focused group's active tab and find bar from its slot.
    fn unpark_focus(&mut self) {
        let f = self.groups.focused;
        if let Some(slot) = self.groups.list.get_mut(f) {
            self.active = slot.active.min(self.tabs.len() - 1);
            std::mem::swap(&mut self.find, &mut slot.find);
        }
    }

    /// Give group `g` the keyboard: its active tab and its find bar
    /// become the app's.
    fn focus_group(&mut self, g: usize) {
        if g == self.groups.focused || g >= self.groups.len() {
            return;
        }
        // Typing in the other group starts a new undo step, even when it
        // shows the same document.
        self.doc_mut().history.seal();
        self.park_focus();
        self.groups.focused = g;
        self.unpark_focus();
        self.after_focus_moved();
    }

    /// The focused tab changed: forget what belonged to the old one.
    fn after_focus_moved(&mut self) {
        self.preedit = None;
        self.title_key = None;
        if self.find.open {
            self.find_origin = self.cursor();
            self.search(false);
        }
        self.reveal_active();
        self.changed(false);
    }

    /// What the debug overlay reports about the active document and the
    /// work running for it.
    fn debug_facts(&self) -> crate::debug::Facts {
        debug_facts(self.doc(), &self.find)
    }

    /// Show a short message at the bottom of the window.
    fn say(&mut self, text: impl Into<String>, is_error: bool) {
        self.message = Some(Message {
            text: text.into(),
            is_error,
            until: Instant::now() + MESSAGE_TIME,
        });
        self.changed(false);
    }

    fn update_title(&mut self) {
        let Some(window) = &self.window else { return };
        let key = (self.active, self.doc().is_dirty());
        if self.title_key == Some(key) {
            return;
        }
        let dirty = if key.1 { "● " } else { "" };
        window.set_title(&format!("{dirty}{} — Nimbus", self.doc().display_name()));
        self.title_key = Some(key);
    }

    /// Something changed what is on screen.
    fn changed(&mut self, keep_cursor_visible: bool) {
        // The tree's name field goes away with the keyboard.
        if self.sidebar.edit.is_some() && !self.sidebar.focused {
            self.sidebar.edit = None;
        }
        self.sync_panes();
        if keep_cursor_visible {
            // A cursor put inside a fold (a search match, Go to Line, an
            // edit elsewhere) opens it.
            let tab = &mut self.tabs[self.active];
            let cursor = tab.carets.cursor(&self.docs[tab.doc].buffer);
            tab.view.folds.reveal(cursor);
        }
        let tab = &self.tabs[self.active];
        self.docs[tab.doc].check_following(&tab.carets);
        if keep_cursor_visible && let Some(layout) = self.layout() {
            let tab_width = self.tab_width();
            let tab = &mut self.tabs[self.active];
            match (tab.compare.as_mut(), tab.merge.as_mut()) {
                (Some(c), _) => {
                    c.ensure_cursor_visible(
                        &self.docs[tab.doc],
                        tab.carets.cursor(&self.docs[tab.doc].buffer),
                        &layout,
                    );
                }
                (None, Some(m)) => {
                    m.ensure_cursor_visible(
                        &self.docs[tab.doc],
                        tab.carets.cursor(&self.docs[tab.doc].buffer),
                        &layout,
                    );
                }
                (None, None) => {
                    tab.view.ensure_cursor_visible(
                        &self.docs[tab.doc],
                        &tab.carets,
                        &layout,
                        tab_width,
                    );
                }
            }
        }
        self.update_title();
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// Run a command from a key, menu, the palette or the IME.
    fn run(&mut self, cmd: Command<'_>, event_loop: &ActiveEventLoop) {
        // Edits made from another group since the last frame move this
        // tab's cursors and folds first.
        self.sync_panes();
        if self.picker.is_some() && self.run_picker(cmd, event_loop) {
            self.changed(false);
            return;
        }
        if self.find.focused && self.run_find(cmd) {
            self.changed(false);
            return;
        }
        if self.sidebar.focused
            && self.picker.is_none()
            && self.sidebar.mode == Mode::Search
            && self.run_search_panel(cmd)
        {
            self.changed(false);
            return;
        }
        if self.sidebar.focused
            && self.picker.is_none()
            && self.sidebar.edit.is_some()
            && self.run_tree_edit(cmd)
        {
            self.changed(false);
            return;
        }
        if self.sidebar.focused && self.picker.is_none() && self.run_sidebar(cmd) {
            self.changed(false);
            return;
        }
        if self.run_app_command(cmd, event_loop) {
            return;
        }
        let result = self.run_edit(cmd);
        if let Err(e) = result {
            // A bug, not a user error; the document is unchanged.
            log::error!("edit refused: {e}");
        }
        // Edits invalidate search results. Search again once typing pauses,
        // not on every keystroke; until then stale highlights are hidden.
        if self.find.open && self.doc().version != self.find.version {
            self.research_at = Some(Instant::now() + RESEARCH_DELAY);
        }
        self.changed(!matches!(
            cmd,
            Command::Find | Command::Replace | Command::ToggleWrap
        ));
    }

    /// Commands about files, tabs, windows and pickers. Returns `false` for
    /// editing commands, which [`App::run_edit`] handles.
    fn run_app_command(&mut self, cmd: Command<'_>, event_loop: &ActiveEventLoop) -> bool {
        match cmd {
            Command::Save => {
                self.save(self.active);
            }
            Command::SaveAs => {
                self.save_as(self.active);
            }
            Command::OpenFile => self.open_dialog(),
            Command::NewFile => {
                let tab = Tab::new(
                    self.docs.insert(Document::untitled()),
                    self.config.soft_wrap,
                );
                let i = self.add_tab(tab);
                self.switch_to(i);
            }
            Command::CloseTab => self.close_tab(self.active, event_loop),
            Command::NextTab => self.step_tab(1),
            Command::PrevTab => self.step_tab(-1),
            Command::GoToTab(i) => {
                let ids = self.group_tabs(self.groups.focused);
                if let Some(&t) = ids.get(i.min(ids.len().saturating_sub(1))) {
                    self.switch_to(t);
                }
            }
            _ if cmd.is_layout() => self.run_layout_command(cmd, event_loop),
            Command::QuickOpen => self.open_picker(Kind::Files),
            Command::GoToLine => self.open_picker(Kind::Line),
            Command::GoToSymbol => self.open_picker(Kind::Symbols),
            Command::Palette => self.open_picker(Kind::Palette),
            Command::ReloadConfig => self.reload_config(),
            Command::ToggleTheme => {
                let next = if self.theme.base == "light" {
                    "dark"
                } else {
                    "light"
                };
                self.theme = Theme::built_in(next).unwrap_or_default();
                self.say(format!("Theme: {next}"), false);
            }
            Command::ToggleSidebar => self.toggle_sidebar(),
            Command::FocusSidebar => self.focus_sidebar(),
            Command::FindInFiles
            | Command::ReplaceInFiles
            | Command::NextResult
            | Command::PrevResult
            | Command::SearchToggleCase
            | Command::SearchToggleRegex
            | Command::SearchToggleWord
            | Command::ReplaceInFilesAll
            | Command::UndoReplaceInFiles
            | Command::ClearSearch => self.run_search_command(cmd),
            Command::ToggleDebugOverlay => {
                self.debug.visible = !self.debug.visible;
                self.changed(false);
            }
            Command::ToggleIgnored => {
                let show = !self.sidebar.tree.show_ignored;
                let requests = self.sidebar.tree.set_show_ignored(show);
                self.start_listings(requests);
                self.say(
                    if show {
                        "Showing ignored files"
                    } else {
                        "Hiding ignored files"
                    },
                    false,
                );
            }
            Command::ReloadFromDisk if self.waits_for_save() => {}
            Command::ReloadFromDisk => self.reload_active(),
            Command::ReopenWithEncoding => self.open_picker(Kind::Encoding(EncodingFor::Reopen)),
            Command::SaveWithEncoding => self.open_picker(Kind::Encoding(EncodingFor::Save)),
            Command::KeepMine => {
                self.doc_mut().keep_mine();
                self.say(
                    "Keeping your version; saving overwrites the file on disk",
                    false,
                );
            }
            _ if cmd.is_setup() => self.run_setup(cmd),
            Command::Close => {
                self.quit(event_loop);
                return true;
            }
            _ => return self.run_workspace_command(cmd),
        }
        self.changed(false);
        true
    }

    /// Show or hide the sidebar, keeping focus out of a hidden one.
    fn toggle_sidebar(&mut self) {
        self.sidebar.visible = !self.sidebar.visible;
        if self.sidebar.visible {
            self.show_sidebar();
        } else {
            self.sidebar.focused = false;
        }
        self.remember();
    }

    /// Show the file tree and move keyboard focus into it.
    fn focus_sidebar(&mut self) {
        self.sidebar.visible = true;
        self.sidebar.focused = true;
        self.sidebar.mode = Mode::Files;
        self.find.focused = false;
        self.show_sidebar();
        self.remember();
    }

    /// The compare view's commands (M9). Returns `false` for any other.
    fn run_compare_command(&mut self, cmd: Command<'_>) -> bool {
        match cmd {
            Command::CompareSaved => self.compare_with_saved(),
            Command::CompareFile => self.compare_with_file(),
            Command::CompareTab => self.compare_with_tab(),
            Command::CompareHead => self.compare_with_revision("HEAD".to_owned()),
            Command::CompareRevision => {
                if self.doc().path.is_some() {
                    self.open_picker(Kind::Revision);
                } else {
                    self.say("This tab has no file to compare with a revision", true);
                }
            }
            Command::CompareFolders => self.compare_folders(),
            Command::DiffWhitespace => self.toggle_diff_option(Toggle::Whitespace),
            Command::DiffCase => self.toggle_diff_option(Toggle::Case),
            Command::DiffBlankLines => self.toggle_diff_option(Toggle::BlankLines),
            Command::DiffNext => self.diff_step(true),
            Command::DiffPrev => self.diff_step(false),
            Command::DiffUnified => self.toggle_diff_option(Toggle::Unified),
            Command::DiffClose => {
                let tab = &mut self.tabs[self.active];
                if let Some(c) = tab.compare.take() {
                    c.finish(&mut self.docs[tab.doc]);
                }
                if let Some(m) = tab.merge.take() {
                    m.finish(&mut self.docs[tab.doc]);
                }
            }
            Command::MergeFiles => self.merge_files(),
            Command::MergeWithBase => self.merge_with_base(),
            Command::TakeLeft => self.merge_take(Take::Left),
            Command::TakeRight => self.merge_take(Take::Right),
            Command::TakeBoth => self.merge_take(Take::Both),
            Command::TakeBothReversed => self.merge_take(Take::BothReversed),
            Command::NextConflict => self.conflict_step(true),
            Command::PrevConflict => self.conflict_step(false),
            Command::ResolveMarkers => self.resolve_markers(),
            Command::MergeWithDisk => self.merge_with_disk(),
            _ => return false,
        }
        self.changed(false);
        true
    }

    /// A file picked in a dialog titled `title`, opened as a document.
    fn pick_document(&mut self, title: &str) -> Option<(Document, PathBuf)> {
        let mut dialog = FileDialog::new().set_title(title).set_directory(&self.root);
        if let Some(window) = &self.window {
            dialog = dialog.set_parent(window.as_ref());
        }
        let path = dialog.pick_file()?;
        match Document::open(&path) {
            Ok(mut doc) => {
                doc.detect_language(&self.languages);
                self.configure_indent(&mut doc);
                Some((doc, path))
            }
            Err(e) => {
                self.say(format!("Cannot read {}: {e}", path.display()), true);
                None
            }
        }
    }

    /// "Merge Files…": the left and right files, merged into a new tab.
    fn merge_files(&mut self) {
        let Some((ours, left)) = self.pick_document("Merge: the left file") else {
            return;
        };
        let Some((theirs, right)) = self.pick_document("Merge: the right file") else {
            return;
        };
        let inputs = Inputs {
            base: None,
            ours,
            theirs,
            names: (short_name(&left), short_name(&right)),
        };
        self.start_merge(Shape::TwoWay, inputs, None, false);
    }

    /// "Merge with Base…": the active tab is ours; the base and theirs
    /// are picked. The result opens in a new tab.
    fn merge_with_base(&mut self) {
        let Some(ours) = self.doc().copy_of() else {
            self.say("This tab is too large to merge here", true);
            return;
        };
        let our_name = self.doc().display_name();
        let Some((base, _)) = self.pick_document("Merge: the common base") else {
            return;
        };
        let Some((theirs, their_path)) = self.pick_document("Merge: their version") else {
            return;
        };
        let inputs = Inputs {
            base: Some(base),
            ours,
            theirs,
            names: (our_name, short_name(&their_path)),
        };
        self.start_merge(Shape::ThreeWay, inputs, None, false);
    }

    /// Open a merge tab for `inputs`; `out` is where its result saves, and
    /// `reports` whether `--merge` asked for the exit code.
    fn start_merge(&mut self, shape: Shape, inputs: Inputs, out: Option<PathBuf>, reports: bool) {
        let send = self.merge_sender();
        let (mut merge, mut result) = MergeView::start(shape, inputs, out, &send);
        result.detect_language(&self.languages);
        for doc in merge.inputs_mut() {
            if let Some(job) = doc.syntax.take_job(&doc.buffer) {
                spawn_syntax(&self.proxy, job);
            }
        }
        merge.reports_exit = reports;
        if reports {
            self.merge_saved_clean = Some(false);
        }
        let mut tab = Tab::new(self.docs.insert(result), false);
        tab.merge = Some(Box::new(merge));
        // An untouched empty tab, the one a bare start makes, gives way.
        let only_blank = self.tabs.len() == 1
            && self.docs[self.tabs[0].doc].path.is_none()
            && !self.docs[self.tabs[0].doc].is_dirty()
            && self.docs[self.tabs[0].doc].buffer.len_bytes() == 0;
        if only_blank {
            let old = std::mem::replace(&mut self.tabs[0], tab);
            self.docs.remove(old.doc);
            self.active = 0;
        } else {
            self.active = self.add_tab(tab);
        }
        self.start_syntax();
        self.title_key = None;
        self.changed(true);
    }

    /// The sender a merge's worker reports through.
    fn merge_sender(&self) -> crate::merge_view::Send {
        let proxy = self.proxy.clone();
        Arc::new(move |m| proxy.send_event(AppEvent::Merged(m)).is_ok())
    }

    /// A merge's worker finished: hand it to the merge it is for, and give
    /// the result its language now that it has text.
    fn merged_arrived(&mut self, merged: Merged) {
        let Some(tab) = self
            .tabs
            .iter_mut()
            .find(|t| t.merge.as_ref().is_some_and(|m| m.wants(&merged)))
        else {
            return;
        };
        let Some(m) = tab.merge.as_mut() else { return };
        m.receive(&mut self.docs[tab.doc], merged);
        self.docs[tab.doc].detect_language(&self.languages);
        let n = m.open_conflicts();
        let noun = if n == 1 { "conflict" } else { "conflicts" };
        let (note, is_error) = match m.failure() {
            Some(e) => (format!("Cannot merge: {e}"), true),
            None if n == 0 => ("Merged cleanly".to_owned(), false),
            None => (format!("Merged: {n} {noun} to resolve"), false),
        };
        self.say(note, is_error);
        self.start_syntax();
        self.changed(true);
    }

    /// Edits wait while the active tab's merge is still being computed.
    fn waits_for_merge(&mut self) -> bool {
        if !self.tabs[self.active]
            .merge
            .as_ref()
            .is_some_and(|m| m.is_pending())
        {
            return false;
        }
        self.say("Still merging; edits wait until the result is in", false);
        true
    }

    /// The active tab's whole text, when it is small enough to merge here.
    fn whole_text(&mut self) -> Option<String> {
        let doc = self.doc();
        if doc.buffer.len_bytes() > nimbus_core::DIFF_LIMIT || doc.buffer.is_mapped() {
            self.say("This file is too large to merge here", true);
            return None;
        }
        Some(doc.buffer.slice(0..doc.buffer.len_bytes()).into_owned())
    }

    /// Put a merge on the active tab in place of its document: the result
    /// takes the tab (same file, same slot), the old text's undo history
    /// is gone.
    fn replace_active_with_merge(&mut self, inputs: Inputs, out: Option<PathBuf>) {
        let send = self.merge_sender();
        let (mut merge, mut result) = MergeView::start(Shape::ThreeWay, inputs, out, &send);
        result.detect_language(&self.languages);
        for doc in merge.inputs_mut() {
            if let Some(job) = doc.syntax.take_job(&doc.buffer) {
                spawn_syntax(&self.proxy, job);
            }
        }
        let tab = &mut self.tabs[self.active];
        if let Some(c) = tab.compare.take() {
            c.finish(&mut self.docs[tab.doc]);
        }
        self.docs[tab.doc] = result;
        tab.view = View::default();
        tab.merge = Some(Box::new(merge));
        self.start_syntax();
        self.title_key = None;
        self.changed(true);
    }

    /// "Resolve Conflicts": the active tab's text, which holds conflict
    /// markers, reopened as a three-way merge of what the markers split.
    fn resolve_markers(&mut self) {
        let Some(text) = self.whole_text() else {
            return;
        };
        let out = self.doc().path.clone();
        match MergeView::inputs_from_markers(&text) {
            Ok(Some((inputs, n))) => {
                self.replace_active_with_merge(inputs, out);
                let noun = if n == 1 { "conflict" } else { "conflicts" };
                self.say(
                    format!("Resolving {n} {noun}; undo history starts here"),
                    false,
                );
            }
            Ok(None) => self.say("No conflict markers in this file", false),
            Err(e) => self.say(format!("Cannot resolve: {e}"), true),
        }
    }

    /// "Merge with Disk": the file changed on disk under unsaved edits
    /// (M6); a three-way merge of the text as last loaded, these edits,
    /// and the file as it is now, in place of the tab's document.
    fn merge_with_disk(&mut self) {
        let doc = self.doc();
        let (Some(path), Some(base)) = (doc.path.clone(), doc.disk_text().cloned()) else {
            self.say("Merge with Disk needs a file that was read from disk", true);
            return;
        };
        let Some(ours) = doc.copy_of() else {
            self.say("This file is too large to merge here", true);
            return;
        };
        let theirs = match Document::open(&path) {
            Ok(d) => d,
            Err(e) => {
                self.say(format!("Cannot read {}: {e}", path.display()), true);
                return;
            }
        };
        let base_text = base.slice(0..base.len_bytes());
        let inputs = Inputs {
            base: Some(Document::with_text(None, &base_text)),
            ours,
            theirs,
            names: ("mine".to_owned(), "disk".to_owned()),
        };
        self.replace_active_with_merge(inputs, Some(path));
        self.say("Merging with the file on disk…", false);
    }

    /// Say so when the active document holds conflict markers.
    fn note_conflict_markers(&mut self) {
        let doc = self.doc();
        let head = doc.buffer.slice(0..doc.buffer.len_bytes().min(256 * 1024));
        if nimbus_core::markers::has_markers(&head) {
            self.say(
                "Conflict markers found; run Resolve Conflicts from the command palette",
                false,
            );
        }
    }

    /// Take a side for the change under the cursor of the active merge.
    fn merge_take(&mut self, take: Take) {
        let tab = &mut self.tabs[self.active];
        let Some(m) = tab.merge.as_mut() else {
            self.say("Not merging; run Merge Files… first", false);
            return;
        };
        if let Err(e) = m.take(
            &mut Editor::new(&mut self.docs[tab.doc], &mut tab.carets),
            take,
        ) {
            self.say(e, false);
        }
        self.changed(true);
    }

    /// Move to the next or previous open conflict of the active merge.
    fn conflict_step(&mut self, forward: bool) {
        let Some(layout) = self.layout() else { return };
        let tab = &mut self.tabs[self.active];
        let Some(m) = tab.merge.as_mut() else {
            self.say("Not merging", false);
            return;
        };
        match m.step_conflict(
            forward,
            &self.docs[tab.doc],
            tab.carets.cursor(&self.docs[tab.doc].buffer),
            &layout,
        ) {
            Some(p) => {
                Editor::new(&mut self.docs[tab.doc], &mut tab.carets).click(p, false);
                self.changed(true);
            }
            None => self.say("No more conflicts that way", false),
        }
    }

    /// A compare's worker finished: hand the diff to the compare it is for.
    fn diff_arrived(&mut self, result: DiffResult) {
        for tab in &mut self.tabs {
            if let Some(c) = tab.compare.as_mut()
                && c.wants(&result)
            {
                c.receive(&self.docs[tab.doc], result);
                self.changed(false);
                return;
            }
        }
    }

    /// A parse finished: if a compare's left side was waiting for it, that
    /// side takes it and the frame is redrawn; otherwise it is handed back.
    fn other_side_parsed(&mut self, result: JobResult) -> Option<JobResult> {
        let id = result.id();
        let Some(other) = self.tabs.iter_mut().find_map(|t| {
            if let Some(c) = t.compare.as_deref_mut()
                && c.other.syntax.waiting_for(id)
            {
                return Some(&mut c.other);
            }
            t.merge.as_deref_mut().and_then(|m| {
                m.inputs_mut()
                    .into_iter()
                    .find(|d| d.syntax.waiting_for(id))
            })
        }) else {
            return Some(result);
        };
        if let Some(job) = other.syntax.finish(result, &other.buffer, SYNTAX_BUDGET) {
            spawn_syntax(&self.proxy, job);
        }
        self.changed(false);
        None
    }

    /// The sender a compare's worker reports through.
    fn diff_sender(&self) -> crate::compare::Send {
        let proxy = self.proxy.clone();
        Arc::new(move |r| proxy.send_event(AppEvent::Diff(r)).is_ok())
    }

    /// Put the active tab side by side with `other`.
    fn start_compare(&mut self, mut other: Document, what: Other) {
        let send = self.diff_sender();
        // The options live in the sidebar's Compare tab: bring it forward.
        self.sidebar.mode = sidebar::Mode::Compare;
        other.detect_language(&self.languages);
        if let Some(job) = other.syntax.take_job(&other.buffer) {
            spawn_syntax(&self.proxy, job);
        }
        let tab = &mut self.tabs[self.active];
        if let Some(old) = tab.compare.take() {
            old.finish(&mut self.docs[tab.doc]);
        }
        tab.view.set_wrap(false);
        tab.compare = Some(Box::new(Compare::start(
            &mut self.docs[tab.doc],
            other,
            what,
            self.ws.state.diff_options(),
            send,
        )));
        self.changed(true);
    }

    /// Flip one of the compare options (the sidebar's Compare tab). The diff options apply to
    /// every compare and are remembered in `state.toml`; Unified is the
    /// active compare's alone.
    fn toggle_diff_option(&mut self, toggle: Toggle) {
        use nimbus_core::Whitespace;
        if toggle == Toggle::Unified {
            if let Some(c) = self.tabs[self.active].compare.as_mut() {
                c.toggle_unified();
                self.changed(false);
            } else {
                self.say("Not comparing; run Compare with Saved first", false);
            }
            return;
        }
        let mut n = self.ws.state.diff_options();
        let said = match toggle {
            Toggle::Whitespace => {
                n.whitespace = match n.whitespace {
                    Whitespace::Exact => Whitespace::Ends,
                    Whitespace::Ends => Whitespace::All,
                    Whitespace::All => Whitespace::Exact,
                };
                match n.whitespace {
                    Whitespace::Exact => "Compare: whitespace counts",
                    Whitespace::Ends => "Compare: ignoring whitespace at line ends",
                    Whitespace::All => "Compare: ignoring all whitespace",
                }
            }
            Toggle::Case => {
                n.case = !n.case;
                if n.case {
                    "Compare: ignoring case"
                } else {
                    "Compare: case counts"
                }
            }
            Toggle::BlankLines | Toggle::Unified => {
                n.blank_lines = !n.blank_lines;
                if n.blank_lines {
                    "Compare: ignoring blank-line changes"
                } else {
                    "Compare: blank-line changes count"
                }
            }
        };
        self.ws.state.set_diff_options(n);
        self.save_state_soon();
        for tab in &mut self.tabs {
            if let Some(c) = tab.compare.as_mut() {
                c.set_normalize(&self.docs[tab.doc], n);
            }
        }
        self.say(said, false);
        self.changed(false);
    }

    /// Compare the active tab's file with `rev` from git, read on a worker
    /// ([`git::show`]); the result arrives as [`AppEvent::Revision`].
    fn compare_with_revision(&mut self, rev: String) {
        let Some(path) = self.doc().path.clone() else {
            self.say("This tab has no file to compare with a revision", true);
            return;
        };
        let proxy = self.proxy.clone();
        self.say(format!("Reading {rev} from git…"), false);
        let spawned = std::thread::Builder::new()
            .name("nimbus-git".to_owned())
            .spawn(move || {
                let outcome = git::show(&path, &rev);
                let _ = proxy.send_event(AppEvent::Revision(Revision { path, rev, outcome }));
            });
        if let Err(e) = spawned {
            self.say(format!("Cannot start git: {e}"), true);
        }
    }

    /// git finished reading a revision: open it beside its tab, or say
    /// why not. A missing git or a file outside a repository is a message,
    /// never a dialog.
    fn revision_read(&mut self, r: Revision) {
        let text = match r.outcome {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => {
                self.say(format!("Cannot compare with {}: {e}", r.rev), true);
                return;
            }
        };
        let found = self
            .tabs
            .iter()
            .position(|t| self.docs[t.doc].path.as_deref() == Some(r.path.as_path()));
        let Some(i) = found else {
            return;
        };
        if i != self.active {
            self.switch_to(i);
        }
        let other = Document::with_text(Some(r.path.clone()), &text);
        let name = r.path.file_name().map_or_else(
            || r.rev.clone(),
            |n| format!("{} @ {}", n.to_string_lossy(), r.rev),
        );
        self.message = None;
        self.start_compare(other, Other::Revision(name));
    }

    /// Ask for two folders and compare them in the sidebar.
    fn compare_folders(&mut self) {
        let pick = |title: &str| {
            let mut dialog = FileDialog::new().set_directory(&self.root).set_title(title);
            if let Some(window) = &self.window {
                dialog = dialog.set_parent(window.as_ref());
            }
            dialog.pick_folder()
        };
        let Some(left) = pick("Left folder") else {
            return;
        };
        let Some(right) = pick("Right folder") else {
            return;
        };
        self.start_folder_compare(&left, &right);
    }

    /// A batch of folder-compare results from the workers.
    fn folder_compare_arrived(&mut self, batch: folder_compare::Batch) {
        if self.sidebar.compare.receive(batch) {
            self.changed(false);
        }
    }

    /// Compare `left` with `right` on workers, listing what differs in the
    /// sidebar's Compare mode as it is found (M18).
    fn start_folder_compare(&mut self, left: &Path, right: &Path) {
        let (include, exclude) = self.sidebar.search.globs();
        let params = folder_compare::Params {
            left: io::canonical(left),
            right: io::canonical(right),
            include,
            exclude,
            show_ignored: self.sidebar.tree.show_ignored,
        };
        let proxy = self.proxy.clone();
        let generation = Arc::clone(&self.sidebar.compare.generation);
        match folder_compare::start(&params, &generation, move |b| {
            proxy.send_event(AppEvent::FolderCompare(b)).is_ok()
        }) {
            Ok(g) => {
                self.sidebar.compare.begin(g, &params.left, &params.right);
                self.sidebar.visible = true;
                self.sidebar.mode = Mode::Compare;
                self.sidebar.focused = true;
                self.changed(true);
            }
            Err(e) => self.say(format!("Cannot compare folders: {e}"), true),
        }
    }

    /// Open row `row` of the folder compare: the right file in a tab beside
    /// the left one. A file on one side only opens alone (left only) or
    /// against nothing (right only).
    fn open_folder_pair(&mut self, row: usize) {
        let Some(results) = &self.sidebar.compare.results else {
            return;
        };
        let (left, right) = results.pair(row);
        self.sidebar.compare.opened = Some(row);
        match (left, right) {
            (Some(l), Some(r)) => {
                self.open_path(&r);
                match Document::open(&l) {
                    Ok(other) => self.start_compare(other, Other::File(l)),
                    Err(e) => self.say(format!("Cannot read {}: {e}", l.display()), true),
                }
            }
            (None, Some(r)) => {
                self.open_path(&r);
                self.start_compare(Document::with_text(None, ""), Other::Missing);
            }
            (Some(l), None) => {
                self.open_path(&l);
                self.say("Only in the left folder", false);
            }
            (None, None) => {}
        }
    }

    fn compare_with_saved(&mut self) {
        let Some(path) = self.doc().path.clone() else {
            self.say("This tab has no file on disk to compare with", true);
            return;
        };
        match Document::open(&path) {
            Ok(other) => self.start_compare(other, Other::Saved),
            Err(e) => self.say(format!("Cannot read {}: {e}", path.display()), true),
        }
    }

    fn compare_with_file(&mut self) {
        let mut dialog = FileDialog::new().set_directory(&self.root);
        if let Some(window) = &self.window {
            dialog = dialog.set_parent(window.as_ref());
        }
        let Some(path) = dialog.pick_file() else {
            return;
        };
        match Document::open(&path) {
            Ok(other) => self.start_compare(other, Other::File(path)),
            Err(e) => self.say(format!("Cannot read {}: {e}", path.display()), true),
        }
    }

    /// Compare with another tab: the only other one, or one picked.
    fn compare_with_tab(&mut self) {
        let others: Vec<usize> = (0..self.tabs.len()).filter(|&i| i != self.active).collect();
        match others.as_slice() {
            [] => self.say("No other tab to compare with", true),
            [only] => self.compare_with_tab_index(*only),
            _ => self.open_picker(Kind::Tabs),
        }
    }

    /// Compare with tab `i`: a copy of its text as it is now.
    fn compare_with_tab_index(&mut self, i: usize) {
        if i == self.active || i >= self.tabs.len() {
            return;
        }
        // A restored tab not read yet: compare with its file.
        let src = &self.docs[self.tabs[i].doc];
        let name = src.display_name();
        if src.is_placeholder()
            && let Some(path) = src.path.clone()
        {
            match Document::open(&path) {
                Ok(other) => self.start_compare(other, Other::File(path)),
                Err(e) => self.say(format!("Cannot read {}: {e}", path.display()), true),
            }
            return;
        }
        match (src.copy_of(), src.path.clone()) {
            (Some(copy), _) => self.start_compare(copy, Other::Tab(name)),
            (None, Some(path)) => match Document::open(&path) {
                Ok(other) => self.start_compare(other, Other::File(path)),
                Err(e) => self.say(format!("Cannot read {}: {e}", path.display()), true),
            },
            (None, None) => self.say("That tab is too large to copy for comparing", true),
        }
    }

    /// Move to the next or previous change of the active compare.
    fn diff_step(&mut self, forward: bool) {
        let Some(layout) = self.layout() else { return };
        let tab = &mut self.tabs[self.active];
        let cursor = tab.carets.cursor(&self.docs[tab.doc].buffer);
        let pos = match (tab.compare.as_mut(), tab.merge.as_mut()) {
            (Some(c), _) if forward => c.next_change(&self.docs[tab.doc], cursor, &layout),
            (Some(c), _) => c.prev_change(&self.docs[tab.doc], cursor, &layout),
            (None, Some(m)) => m.step_change(forward, &self.docs[tab.doc], cursor, &layout),
            (None, None) => {
                self.say("Not comparing; run Compare with Saved first", false);
                return;
            }
        };
        if let Some(p) = pos {
            Editor::new(&mut self.docs[tab.doc], &mut tab.carets).click(p, false);
            self.changed(true);
        } else {
            // At the end of a folder compare's file: on to the next.
            let is_compare = tab.compare.is_some();
            let next = self
                .window_layout()
                .filter(|_| is_compare)
                .and_then(|l| self.sidebar.step_compare(forward, &l));
            match next {
                Some(row) => self.open_folder_pair(row),
                None => self.say("No more changes", false),
            }
        }
    }

    /// Editing, movement and find commands on the active tab.
    fn run_edit(&mut self, cmd: Command<'_>) -> Result<(), nimbus_core::EditError> {
        if cmd.edits_text() && (self.refuses_edits() || self.waits_for_merge()) {
            return Ok(());
        }
        let page = self.layout().map_or(1, |l| l.full_lines());
        let tab_width = self.tab_width();
        match cmd {
            Command::Move(motion, extend) => {
                self.move_cursor(motion, extend, page);
                Ok(())
            }
            Command::Insert(text) => {
                let auto_pairs = self.editing().auto_pairs;
                self.ed().type_text(text, auto_pairs)
            }
            Command::Newline => self.ed().insert_newline(),
            Command::Tab => match self.ed().indent_lines() {
                Ok(true) => Ok(()),
                Ok(false) => self.ed().insert_indent(),
                Err(e) => Err(e),
            },
            Command::Outdent => self.ed().outdent_lines(),
            Command::Undo => self.ed().undo().map(drop),
            Command::Redo => self.ed().redo().map(drop),
            Command::SelectAll => {
                self.ed().select_all();
                Ok(())
            }
            Command::AddNextOccurrence => {
                self.add_next_occurrence();
                Ok(())
            }
            Command::AddCursorAbove | Command::AddCursorBelow => {
                let lines = if cmd == Command::AddCursorAbove {
                    -1
                } else {
                    1
                };
                self.ed().add_cursor_vertical(lines, tab_width);
                Ok(())
            }
            Command::Copy | Command::Cut | Command::Paste => self.clipboard_command(cmd),
            Command::Escape => {
                if self.find.open {
                    self.find.close();
                } else if !self.ed().collapse_cursors() {
                    let head = self.selection().head;
                    self.ed().click(head, false);
                }
                Ok(())
            }
            Command::Find | Command::Replace => {
                self.open_find(cmd == Command::Replace);
                Ok(())
            }
            Command::ToggleCase(_) | Command::ToggleRegex(_) | Command::ReplaceAll => {
                if self.find.open {
                    self.run_find(cmd);
                }
                Ok(())
            }
            Command::ToggleWrap => {
                let tab = &mut self.tabs[self.active];
                let on = !tab.view.wrap;
                tab.view.set_wrap(on);
                tab.carets.preferred_column = None;
                self.say(if on { "Soft wrap on" } else { "Soft wrap off" }, false);
                Ok(())
            }
            Command::DeleteBack => self.ed().backspace().map(drop),
            Command::DeleteForward => self.ed().delete_forward().map(drop),
            Command::DeleteWordBack => self.ed().delete_to(Motion::WordLeft).map(drop),
            Command::DeleteWordForward => self.ed().delete_to(Motion::WordRight).map(drop),
            Command::DeleteToLineStart => self.ed().delete_to(Motion::LineStart).map(drop),
            _ => self.run_everyday_edit(cmd),
        }
    }

    /// The everyday editing commands of M11 (comments, lines, selection,
    /// text, indentation), on the active tab.
    #[expect(
        clippy::redundant_closure_for_method_calls,
        reason = "a method path fixes the editor's lifetime; the closure is general over it"
    )]
    fn run_everyday_edit(&mut self, cmd: Command<'_>) -> Result<(), nimbus_core::EditError> {
        match cmd {
            Command::ToggleLineComment => self.edit_op(|e| e.toggle_line_comment()),
            Command::ToggleBlockComment => self.edit_op(|e| e.toggle_block_comment()),
            Command::Duplicate => self.edit_op(|e| e.duplicate()),
            Command::MoveLineUp => self.edit_op(|d| d.move_lines(false)),
            Command::MoveLineDown => self.edit_op(|d| d.move_lines(true)),
            Command::DeleteLine => self.edit_op(|e| e.delete_lines()),
            Command::JoinLines => self.edit_op(|e| e.join_lines()),
            Command::InsertLineBelow => self.edit_op(|d| d.insert_line(false)),
            Command::InsertLineAbove => self.edit_op(|d| d.insert_line(true)),
            Command::SelectLine => {
                self.ed().select_line();
                Ok(())
            }
            Command::ExpandSelection => {
                self.ed().expand_selection();
                Ok(())
            }
            Command::ShrinkSelection => {
                self.ed().shrink_selection();
                Ok(())
            }
            Command::MatchingBracket(select) => {
                if self.ed().matching_bracket(select) == Outcome::Nothing {
                    self.say("No bracket at the cursor", false);
                }
                Ok(())
            }
            Command::ChangeCase(case) => self.edit_op(|d| d.change_case(case)),
            Command::RewriteLines(op) => self.edit_op(|d| d.rewrite_lines(op)),
            Command::TrimWhitespace => self.edit_op(|e| e.trim_whitespace()),
            Command::ChangeLineEndings(to) => {
                let outcome = self.ed().change_line_endings(to)?;
                if outcome == Outcome::TooLarge {
                    self.say("Too large to change every line ending at once", true);
                } else {
                    let name = match to {
                        LineEnding::Lf => "LF",
                        LineEnding::CrLf => "CRLF",
                    };
                    self.say(format!("Line endings: {name}"), false);
                }
                Ok(())
            }
            Command::IndentUsingSpaces | Command::IndentUsingTabs => {
                let settings = self.config.indent.for_language(self.doc().language_key());
                let indent = nimbus_core::indent::Indent {
                    width: settings.indent.width,
                    tabs: cmd == Command::IndentUsingTabs,
                };
                self.doc_mut().indent = indent;
                self.say(
                    format!("Indenting with {}", indent.label().to_lowercase()),
                    false,
                );
                Ok(())
            }
            Command::DetectIndent => {
                let settings = config::Indenting {
                    detect: true,
                    ..self.config.indent.for_language(self.doc().language_key())
                };
                let found = self.doc_mut().configure_indent(settings);
                let label = self.doc().indent.label().to_lowercase();
                self.say(
                    if found {
                        format!("Detected {label}")
                    } else {
                        format!("No indentation found; using {label}")
                    },
                    false,
                );
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Run an M11 operation on the active document and report a refusal.
    fn edit_op(
        &mut self,
        op: impl FnOnce(&mut Editor<'_>) -> Result<Outcome, nimbus_core::EditError>,
    ) -> Result<(), nimbus_core::EditError> {
        let outcome = op(&mut self.ed())?;
        self.report(outcome);
        Ok(())
    }

    fn move_cursor(&mut self, motion: Motion, extend: bool, page: usize) {
        let tab_width = self.tab_width();
        let layout = self.layout();
        let tab = &mut self.tabs[self.active];
        match &layout {
            // Row by row through soft wrap, and over folded lines.
            Some(layout) if tab.view.wrap || !tab.view.folds.is_empty() => {
                let mut w = tab.view.wrap_view(layout, tab_width);
                Editor::new(&mut self.docs[tab.doc], &mut tab.carets)
                    .move_cursor_wrapped(motion, extend, &mut w, page);
            }
            _ => Editor::new(&mut self.docs[tab.doc], &mut tab.carets)
                .move_cursor(motion, extend, tab_width, page),
        }
        if !tab.view.folds.is_empty() {
            // Left and right step over a fold rather than into it.
            let back = matches!(
                motion,
                Motion::Left | Motion::WordLeft | Motion::LineStart | Motion::DocStart
            );
            let t = &self.docs[tab.doc].buffer;
            let folds = &tab.view.folds;
            tab.carets.selections.transform(|_, sel| {
                let head = match folds.hiding(sel.head) {
                    Some(f) if back => f.start,
                    Some(f) => t.line_span(f.end).next.unwrap_or(sel.head),
                    None => sel.head,
                };
                let anchor = if extend { sel.anchor } else { head };
                nimbus_core::Selection { anchor, head }
            });
        }
        if matches!(motion, Motion::PageUp | Motion::PageDown)
            && let Some(layout) = layout
        {
            // Scroll by the same amount so the cursor keeps its row.
            let dir = if motion == Motion::PageUp { -1.0 } else { 1.0 };
            #[expect(clippy::cast_precision_loss, reason = "page is a few hundred lines")]
            let dy = dir * page as f32 * layout.metrics.line_height;
            tab.view
                .scroll_by(dy, &self.docs[tab.doc], &layout, tab_width);
        }
    }

    /// Make tab `i` the active one, focusing its group.
    fn switch_to(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        let g = self.tabs[i].group;
        if i == self.active && g == self.groups.focused {
            return;
        }
        if !self.ensure_loaded(i) {
            return;
        }
        if g != self.groups.focused {
            self.doc_mut().history.seal();
            self.park_focus();
            self.groups.focused = g;
            self.unpark_focus();
        }
        self.active = i;
        self.sidebar.focused = false;
        self.after_focus_moved();
        self.session_changed();
    }

    /// The next (`step` 1) or previous (-1) tab in the focused group's
    /// strip, wrapping around.
    fn step_tab(&mut self, step: isize) {
        let ids = self.group_tabs(self.groups.focused);
        let Some(at) = ids.iter().position(|&i| i == self.active) else {
            return;
        };
        let n = ids.len().cast_signed();
        let next = (at.cast_signed() + step).rem_euclid(n).cast_unsigned();
        if let Some(&i) = ids.get(next) {
            self.switch_to(i);
        }
    }

    /// Add `tab` to the focused group, returning its index.
    fn add_tab(&mut self, mut tab: Tab) -> usize {
        tab.group = self.groups.focused;
        self.tabs.push(tab);
        self.tabs.len() - 1
    }

    /// Move the cursor to the start of line `n` (counting from 1). Past the
    /// last line it goes to the last line; in a huge file whose lines are
    /// still being counted, the jump waits until counting reaches it.
    fn go_to_line(&mut self, n: usize) {
        let t = &self.docs[self.tabs[self.active].doc].buffer;
        let line = n.saturating_sub(1);
        if line >= t.len_lines() && self.docs[self.tabs[self.active].doc].counting_lines() {
            self.pending_line = Some(n);
            self.say(
                format!("Going to line {n} once lines are counted that far"),
                false,
            );
            return;
        }
        self.pending_line = None;
        let pos = t.line_to_byte(line.min(t.len_lines().saturating_sub(1)));
        let mut ed = self.ed();
        ed.click(pos, false);
        ed.carets.preferred_column = None;
        self.changed(true);
    }

    /// Put the cursor at byte `pos` of the active document (a symbol
    /// chosen in the picker) and scroll it into view.
    fn go_to_byte(&mut self, pos: usize) {
        let mut ed = self.ed();
        let pos = pos.min(ed.doc.buffer.len_bytes());
        ed.click(pos, false);
        ed.carets.preferred_column = None;
        self.changed(true);
    }

    /// Start line counting for any mapped document that has not started.
    fn start_indexing(&mut self) {
        for tab in &mut self.tabs {
            if let Some(job) = self.docs[tab.doc].indexing.take() {
                let proxy = self.proxy.clone();
                job.spawn(move |token, chunk, error| {
                    proxy
                        .send_event(AppEvent::Index(token, chunk, error))
                        .is_ok()
                });
            }
        }
    }

    /// A chunk of line index arrived: merge it, report progress, and finish
    /// a go-to-line that was waiting for it.
    fn index_arrived(&mut self, token: u64, chunk: IndexChunk, error: Option<String>) {
        let Some(i) = self
            .tabs
            .iter_mut()
            .position(|t| self.docs[t.doc].index_token == Some(token))
        else {
            return; // closed or reloaded meanwhile
        };
        let merged = self.docs[self.tabs[i].doc].merge_index(token, chunk);
        // A grown file's count may go on in another run.
        self.start_indexing();
        if error.is_some() {
            // Nothing more comes: stop waiting for it.
            self.docs[self.tabs[i].doc].stop_indexing();
        }
        if !merged || i != self.active {
            return;
        }
        let t = &self.docs[self.tabs[i].doc].buffer;
        if let Some(e) = error {
            #[expect(clippy::cast_precision_loss, reason = "a percentage for display")]
            let pct = t.indexed_bytes() as f64 * 100.0 / t.len_bytes().max(1) as f64;
            self.say(format!("Stopped counting lines at {pct:.0}%: {e}"), true);
        } else if t.is_fully_indexed() {
            let lines = t.len_lines();
            self.say(format!("{lines} lines"), false);
        }
        // Progress meanwhile shows in the status bar.
        if let Some(n) = self.pending_line {
            let t = &self.docs[self.tabs[i].doc].buffer;
            if n <= t.len_lines() || !self.docs[self.tabs[i].doc].counting_lines() {
                self.go_to_line(n);
            }
        }
        self.changed(false);
    }

    /// Start folder listings for the sidebar on worker threads.
    fn start_listings(&self, requests: Vec<ListRequest>) {
        for req in requests {
            spawn_listing(&self.proxy, req);
        }
    }

    /// The sidebar became visible: list the root the first time, and show
    /// the active file.
    fn show_sidebar(&mut self) {
        let requests = self.sidebar.tree.start();
        self.start_listings(requests);
        self.reveal_active();
    }

    /// Select the active tab's file in the sidebar, opening folders to it.
    fn reveal_active(&mut self) {
        if !self.sidebar.visible {
            return;
        }
        let Some(path) = self.doc().canonical.clone() else {
            return;
        };
        let layout = self.window_layout();
        let requests = self.sidebar.reveal(&path, layout.as_ref());
        self.start_listings(requests);
    }

    /// Remember the sidebar's visibility and width, and the zoom, for the
    /// next run.
    fn remember(&mut self) {
        let (visible, width) = (self.sidebar.visible, self.sidebar.width);
        let font_size = if self.config.remember_zoom {
            ((self.font_size - self.config.font_size).abs() > f64::EPSILON)
                .then_some(self.font_size)
        } else {
            self.ws.state.font_size
        };
        let state = &self.ws.state;
        if (visible, width, font_size)
            != (state.sidebar_visible, state.sidebar_width, state.font_size)
        {
            self.ws.state.sidebar_visible = visible;
            self.ws.state.sidebar_width = width;
            self.ws.state.font_size = font_size;
            self.save_state_soon();
        }
    }

    /// Watch the sidebar's listed folders and the folders of open files.
    /// Does nothing until the first frame is up.
    fn sync_watch(&mut self) {
        if !self.watching {
            return;
        }
        let mut wanted: HashSet<PathBuf> = self
            .sidebar
            .tree
            .loaded_dirs()
            .map(Path::to_path_buf)
            .collect();
        for tab in &self.tabs {
            if let Some(parent) = self.docs[tab.doc]
                .canonical
                .as_deref()
                .and_then(Path::parent)
            {
                wanted.insert(parent.to_path_buf());
            }
        }
        self.watch.sync(&wanted);
    }

    /// Paths changed on disk: list affected sidebar folders again, and
    /// reload, or warn about, open files that changed.
    fn disk_changed(&mut self, changes: &[Change]) {
        let mut requests = Vec::new();
        // Only creating, removing or renaming changes a folder's listing;
        // a file being written to does not.
        for change in changes.iter().filter(|c| c.structural) {
            requests.extend(self.sidebar.tree.affected_by(&change.path));
        }
        self.start_listings(requests);
        let now = Instant::now();
        let mut notes = Vec::new();
        let mut is_error = false;
        let mut later = Vec::new();
        let on_disk: Vec<DocId> = self
            .docs
            .iter()
            .filter(|(_, d)| {
                d.canonical.as_deref().is_some_and(|p| {
                    changes.iter().any(|c| c.path == p)
                        && !self.ws.moving.iter().any(|m| p.starts_with(m))
                })
            })
            .map(|(id, _)| id)
            .collect();
        for id in on_disk {
            let Some(path) = self.docs[id].canonical.as_deref() else {
                continue;
            };
            // A file written to over and over (a growing log) reloads at
            // most once per RELOAD_GAP; the rest wait for the timer. Not a
            // mapped one: it never reloads by itself unless cut short, and
            // that must be handled at once.
            if let Some(at) = self.docs[id].reloaded_at
                && !self.docs[id].is_file_mapped()
                && now.duration_since(at) < RELOAD_GAP
            {
                let due = at + RELOAD_GAP;
                self.recheck_at = Some(self.recheck_at.map_or(due, |t| t.min(due)));
                if !self.recheck.iter().any(|p| p == path) {
                    self.recheck.push(path.to_path_buf());
                }
                continue;
            }
            let name = self.docs[id].display_name();
            // Every tab showing it keeps its place in the new text.
            let mut carets = carets_of(&mut self.tabs, id);
            let note = match self.docs[id].disk_changed(&self.languages, &mut carets) {
                DiskChange::None | DiskChange::Appended => continue,
                DiskChange::Later(due) => {
                    later.extend(self.docs[id].canonical.clone().map(|p| (due, p)));
                    continue;
                }
                DiskChange::Reloaded => format!("{name} changed on disk; reloaded"),
                DiskChange::Conflict => {
                    is_error = true;
                    format!(
                        "{name} changed on disk. From the command palette: Reload from Disk, \
                         Keep My Version, Compare with Saved, or Merge with Disk"
                    )
                }
                DiskChange::NotReloaded => format!(
                    "{name} changed on disk; huge files do not reload by themselves. \
                     Run Reload from Disk from the command palette to see the changes, \
                     or Follow Tail for a file being added to"
                ),
                DiskChange::Truncated { reloaded: true } => {
                    is_error = true;
                    format!("{name} was truncated on disk; reloaded it")
                }
                DiskChange::Truncated { reloaded: false } => {
                    is_error = true;
                    format!(
                        "{name} was truncated on disk. Reload it now (Reload from Disk in the \
                         command palette): reading the missing part can crash Nimbus"
                    )
                }
                DiskChange::Deleted => {
                    is_error = true;
                    format!("{name} was deleted on disk; saving recreates it")
                }
                DiskChange::Failed(e) => {
                    is_error = true;
                    format!("{name} changed on disk but cannot be read: {e}")
                }
            };
            notes.push(note);
        }
        if !notes.is_empty() {
            self.say(notes.join("; "), is_error);
        }
        for (due, path) in later {
            self.recheck_later(due, path);
        }
        let follow = self.follow_tails();
        self.changed(follow);
    }

    /// A command while the sidebar has focus. Movement and Enter work in
    /// the tree, Escape gives focus back to the text, and typing is
    /// ignored. Returns `false` for commands that should run as usual.
    fn run_sidebar(&mut self, cmd: Command<'_>) -> bool {
        let Some(layout) = self.window_layout() else {
            return false;
        };
        let mac = cfg!(target_os = "macos");
        let (motion, enter) = match cmd {
            // Finder's keys on macOS: Return renames, Command+Down (and
            // Space) opens. Enter opens elsewhere, F2 renames everywhere.
            Command::Newline if mac => return self.run_tree_command(Command::TreeRename),
            Command::Move(Motion::DocEnd, false) | Command::Insert(" ") if mac => (None, true),
            Command::Move(m, _) => (Some(m), false),
            Command::Newline => (None, true),
            Command::Escape => {
                self.sidebar.focused = false;
                return true;
            }
            // Delete, or Command+Backspace on macOS, trashes.
            Command::DeleteForward => return self.run_tree_command(Command::TreeDelete),
            Command::DeleteToLineStart if mac => {
                return self.run_tree_command(Command::TreeDelete);
            }
            Command::Insert(_)
            | Command::Tab
            | Command::Outdent
            | Command::DeleteBack
            | Command::DeleteWordBack
            | Command::DeleteWordForward
            | Command::DeleteToLineStart
            | Command::Cut
            | Command::Paste
            | Command::SelectAll => return true,
            _ => return false,
        };
        let Some(outcome) = self.sidebar.key(motion, enter, &layout) else {
            return false;
        };
        self.start_listings(outcome.requests);
        if let Some(path) = outcome.open {
            self.sidebar.focused = false;
            self.open_path(&path);
        }
        if let Some(row) = outcome.pair {
            self.open_folder_pair(row);
        }
        true
    }

    /// Open `path` in a tab, or switch to it if it is already open.
    fn open_path(&mut self, path: &Path) {
        // Whatever asked (the sidebar, the picker, a dialog), the file
        // gets the keyboard.
        self.sidebar.focused = false;
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let showing: Vec<usize> = (0..self.tabs.len())
            .filter(|&i| {
                self.docs[self.tabs[i].doc].path.as_ref().is_some_and(|p| {
                    std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()) == canonical
                })
            })
            .collect();
        // Already open in this group: go to it. Open in another group
        // only: a tab here on the same document (M13).
        let here = showing
            .iter()
            .find(|&&i| self.tabs[i].group == self.groups.focused);
        if let Some(&i) = here {
            self.switch_to(i);
            return;
        }
        if let Some(&other) = showing.first() {
            let tab = Tab::new(self.tabs[other].doc, self.tabs[other].view.wrap);
            let i = self.add_tab(tab);
            self.switch_to(i);
            return;
        }
        match Document::open(path) {
            Ok(mut doc) => {
                doc.detect_language(&self.languages);
                self.configure_indent(&mut doc);
                // Replace a lone untouched untitled tab rather than keeping it.
                let replace = self.tabs.len() == 1
                    && self.docs[self.tabs[0].doc].path.is_none()
                    && !self.docs[self.tabs[0].doc].is_dirty();
                if let Some(c) = &doc.canonical {
                    self.ws.state.note_recent(c);
                }
                let wrap = wrap_for(&self.config, &doc);
                let tab = Tab::new(self.docs.insert(doc), wrap);
                if replace {
                    let old = std::mem::replace(&mut self.tabs[0], tab);
                    self.docs.remove(old.doc);
                    self.active = 0;
                    self.title_key = None;
                    self.reveal_active();
                } else {
                    let i = self.add_tab(tab);
                    self.switch_to(i);
                }
                self.save_state_soon();
                self.session_changed();
                self.sync_watch();
                self.changed(false);
                if !self.note_encoding() {
                    self.note_conflict_markers();
                }
            }
            Err(e) => self.say(format!("Cannot open {}: {e}", path.display()), true),
        }
    }

    fn open_dialog(&mut self) {
        let mut dialog = FileDialog::new().set_directory(&self.root);
        if let Some(window) = &self.window {
            dialog = dialog.set_parent(window.as_ref());
        }
        if let Some(path) = dialog.pick_file() {
            self.open_path(&path);
        }
    }

    /// Save tab `i`; an untitled tab asks for a name. A huge (mapped) file
    /// saves on a worker: this starts it and returns [`Saved::Started`],
    /// and [`App::save_ended`] finishes.
    fn save(&mut self, i: usize) -> Saved {
        if self.tabs[i].merge.as_ref().is_some_and(|m| m.is_pending()) {
            self.say("Still merging; save once the result is in", false);
            return Saved::No;
        }
        if !self.confirm_conflicts(i) {
            return Saved::No;
        }
        if self.docs[self.tabs[i].doc].path.is_none() {
            return self.save_as(i);
        }
        if self.docs[self.tabs[i].doc].is_saving() {
            self.say("Already saving; edits wait until it is done", false);
            return Saved::Started;
        }
        if let Some(why) = self.docs[self.tabs[i].doc].read_only.clone() {
            self.dialog(MessageLevel::Error, "Could not save", &why);
            return Saved::No;
        }
        if !self.confirm_lossy(i) {
            return Saved::No;
        }
        if self.docs[self.tabs[i].doc].saves_on_worker() {
            return self.start_save(i);
        }
        let editing = self
            .config
            .editor
            .for_language(self.docs[self.tabs[i].doc].language_key());
        if let Err(e) = {
            let tab = &mut self.tabs[i];
            Editor::new(&mut self.docs[tab.doc], &mut tab.carets)
        }
        .tidy_for_save(editing.trim_trailing_whitespace, editing.final_newline)
        {
            log::error!("tidying before save refused: {e}");
        }
        match self.docs[self.tabs[i].doc].save() {
            Ok(()) => {
                let name = self.docs[self.tabs[i].doc].display_name();
                log::info!("saved {name}");
                self.title_key = None;
                self.note_merge_saved(i);
                self.say(format!("Saved {name}"), false);
                Saved::Yes
            }
            Err(e) => {
                log::error!("{e}");
                if let Some(u) = e.unencodable().copied() {
                    return self.offer_utf8(i, u);
                }
                self.dialog(MessageLevel::Error, "Could not save", &e.to_string());
                Saved::No
            }
        }
    }

    /// Saving a file whose encoding has no bytes for one of its characters
    /// stopped there: show it, and offer to save as UTF-8, which holds
    /// every character.
    fn offer_utf8(&mut self, i: usize, u: io::Unencodable) -> Saved {
        let doc = &self.docs[self.tabs[i].doc];
        let line = doc
            .buffer
            .byte_to_line(u.offset.min(doc.buffer.len_bytes()))
            + 1;
        let name = doc.display_name();
        if i == self.active {
            self.go_to_byte(u.offset);
        }
        let text = format!(
            "Line {line} of {name} has {:?} (U+{:04X}), which {} cannot hold. \
             Nothing was saved.\n\nSave as UTF-8 instead?",
            u.ch,
            u32::from(u.ch),
            u.encoding
        );
        self.say(
            format!("{} cannot hold {:?} on line {line}", u.encoding, u.ch),
            true,
        );
        if self.options.smoke || self.options.screenshot.is_some() {
            return Saved::No;
        }
        let mut dialog = MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title("Character not in the encoding")
            .set_description(text)
            .set_buttons(MessageButtons::OkCancelCustom(
                "Save as UTF-8".into(),
                "Cancel".into(),
            ));
        if let Some(window) = &self.window {
            dialog = dialog.set_parent(window.as_ref());
        }
        let utf8 = match dialog.show() {
            MessageDialogResult::Ok => true,
            MessageDialogResult::Custom(label) => label == "Save as UTF-8",
            _ => false,
        };
        if !utf8 {
            return Saved::No;
        }
        let doc = &mut self.docs[self.tabs[i].doc];
        doc.encoding = Encoding::Utf8;
        doc.bom = false;
        doc.chosen_encoding = Some(Encoding::Utf8);
        self.save(i)
    }

    /// A file opened with bytes its encoding could not read (shown as
    /// U+FFFD) asks before its first save, which writes U+FFFD in their
    /// place. `true` to go on.
    fn confirm_lossy(&mut self, i: usize) -> bool {
        let doc = &self.docs[self.tabs[i].doc];
        if !doc.lossy || self.options.smoke || self.options.screenshot.is_some() {
            return true;
        }
        let text = format!(
            "{} had bytes that are not valid {}; they show as �. \
             Saving writes that character in their place. Save anyway?",
            doc.display_name(),
            doc.encoding.name()
        );
        let mut dialog = MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title("Replace unreadable bytes?")
            .set_description(text)
            .set_buttons(MessageButtons::YesNo);
        if let Some(window) = &self.window {
            dialog = dialog.set_parent(window.as_ref());
        }
        matches!(dialog.show(), MessageDialogResult::Yes)
    }

    /// An encoding was picked for the active document: read the file again
    /// in it, or save it in it.
    fn encoding_chosen(&mut self, purpose: EncodingFor, choice: Choice) {
        match purpose {
            EncodingFor::Reopen => self.reopen_with(choice.encoding),
            EncodingFor::Save => self.save_with(choice),
        }
    }

    /// Read the active file again in `encoding`, asking first if that loses
    /// unsaved changes. The choice sticks: reloading after a change on
    /// disk keeps it.
    fn reopen_with(&mut self, encoding: Encoding) {
        if self.doc().path.is_none() {
            self.say("Not saved yet: nothing to reopen", false);
            return;
        }
        if self.waits_for_save() {
            return;
        }
        if self.doc().is_dirty() && !self.options.smoke && self.options.screenshot.is_none() {
            let text = format!(
                "Reopening {} as {} discards its unsaved changes. Reopen?",
                self.doc().display_name(),
                encoding.name()
            );
            let mut dialog = MessageDialog::new()
                .set_level(MessageLevel::Warning)
                .set_title("Unsaved changes")
                .set_description(text)
                .set_buttons(MessageButtons::YesNo);
            if let Some(window) = &self.window {
                dialog = dialog.set_parent(window.as_ref());
            }
            if !matches!(dialog.show(), MessageDialogResult::Yes) {
                return;
            }
        }
        let id = self.tabs[self.active].doc;
        let before = self.docs[id].chosen_encoding.replace(encoding);
        let mut carets = carets_of(&mut self.tabs, id);
        match self.docs[id].reload(&self.languages, &mut carets) {
            Ok(()) => {
                if !self.note_encoding() {
                    self.say(format!("Reopened as {}", encoding.name()), false);
                }
            }
            Err(e) => {
                self.docs[id].chosen_encoding = before;
                self.say(format!("Cannot reopen: {e}"), true);
            }
        }
        self.changed(false);
    }

    /// Save the active file in `choice`'s encoding from now on. If the save
    /// fails, the file keeps its old encoding.
    fn save_with(&mut self, choice: Choice) {
        let i = self.active;
        let doc = self.doc();
        if doc.saves_on_worker() && !choice.encoding.is_utf8() {
            self.say(
                format!(
                    "Files over {} MB save only as UTF-8",
                    io::MAX_ROPE_BYTES / (1024 * 1024)
                ),
                true,
            );
            return;
        }
        if doc.buffer.is_mapped() && !doc.saves_on_worker() && !choice.encoding.is_utf8() {
            // Its bytes are not text in any known encoding.
            self.say("This file is not text; it saves only as it is", true);
            return;
        }
        let before = (doc.encoding, doc.bom, doc.chosen_encoding);
        let doc = self.doc_mut();
        doc.encoding = choice.encoding;
        doc.bom = choice.bom && choice.encoding.bom().is_some();
        let saved = self.save(i);
        let doc = &mut self.docs[self.tabs[i].doc];
        match saved {
            Saved::No if (doc.encoding, doc.bom) == (choice.encoding, choice.bom) => {
                (doc.encoding, doc.bom, doc.chosen_encoding) = before;
            }
            Saved::No => {}
            Saved::Yes | Saved::Started => doc.chosen_encoding = Some(doc.encoding),
        }
    }

    /// Say what the active document's encoding means for it, when it means
    /// something: read-only, or unreadable bytes shown as U+FFFD. Returns
    /// whether it said anything.
    fn note_encoding(&mut self) -> bool {
        let doc = self.doc();
        if let Some(why) = doc.read_only.clone() {
            self.say(why, true);
            return true;
        }
        if doc.lossy {
            let text = format!(
                "Some bytes are not valid {}; they show as �",
                doc.encoding.name()
            );
            self.say(text, true);
            return true;
        }
        false
    }

    /// Saving a merge with conflicts left asks first. `true` to go on.
    fn confirm_conflicts(&mut self, i: usize) -> bool {
        let Some(open) = self.tabs[i].merge.as_ref().map(|m| m.open_conflicts()) else {
            return true;
        };
        if open == 0 || self.options.smoke || self.options.screenshot.is_some() {
            return true;
        }
        let noun = if open == 1 { "conflict" } else { "conflicts" };
        let mut dialog = MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title("Conflicts left")
            .set_description(format!(
                "{open} {noun} not resolved. Save anyway, with conflict markers?"
            ))
            .set_buttons(MessageButtons::YesNo);
        if let Some(window) = &self.window {
            dialog = dialog.set_parent(window.as_ref());
        }
        matches!(dialog.show(), MessageDialogResult::Yes)
    }

    /// A `--merge` tab was saved: the exit code follows its conflicts.
    fn note_merge_saved(&mut self, i: usize) {
        if let Some(m) = self.tabs[i].merge.as_ref()
            && m.reports_exit
        {
            self.merge_saved_clean = Some(m.open_conflicts() == 0);
        }
    }

    /// Start saving the mapped document in tab `i` on a worker.
    fn start_save(&mut self, i: usize) -> Saved {
        let doc = &mut self.docs[self.tabs[i].doc];
        let name = doc.display_name();
        let job = match doc.begin_save(ReplacePolicy::default()) {
            Ok(job) => job,
            Err(e) => {
                log::error!("{e}");
                self.dialog(MessageLevel::Error, "Could not save", &e.to_string());
                return Saved::No;
            }
        };
        let token = job.token;
        let proxy = self.proxy.clone();
        if let Err(e) = job.spawn(move |event| proxy.send_event(AppEvent::Save(event)).is_ok()) {
            log::error!("could not start the save thread: {e}");
            // Nothing was written; end the save so edits go on.
            let failed = Err(io::SaveError {
                step: "starting the save",
                source: e,
            });
            let step = self.docs[self.tabs[i].doc].save_written(token, failed, Instant::now());
            log::debug!("save of {name} ended before it began: {step:?}");
            self.dialog(
                MessageLevel::Error,
                "Could not save",
                "Could not start saving; nothing was written",
            );
            return Saved::No;
        }
        log::info!("saving {name} on a worker");
        self.say(format!("Saving {name}…"), false);
        Saved::Started
    }

    /// Progress or the end of a huge file's save.
    fn save_event(&mut self, event: SaveEvent, event_loop: &ActiveEventLoop) {
        match event {
            SaveEvent::Progress {
                token,
                written,
                total,
            } => {
                let Some(i) = self.tab_saving(token) else {
                    return;
                };
                if i == self.active {
                    let name = self.docs[self.tabs[i].doc].display_name();
                    let pct = written.saturating_mul(100) / total.max(1);
                    self.say(format!("Saving {name}… {pct}%"), false);
                }
            }
            SaveEvent::Done { token, result } => {
                // No tab: it went away, and dropping the result deletes the
                // temp file.
                let Some(i) = self.tab_saving(token) else {
                    return;
                };
                let step = self.docs[self.tabs[i].doc].save_written(token, *result, Instant::now());
                self.save_step(i, step, event_loop);
            }
        }
    }

    /// The tab whose save is `token`.
    fn tab_saving(&self, token: u64) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| self.docs[t.doc].save_token() == Some(token))
    }

    /// A save of tab `i` moved on to `step`.
    fn save_step(&mut self, i: usize, step: SaveStep, event_loop: &ActiveEventLoop) {
        match step {
            SaveStep::Waiting => {
                // Windows: a search may be reading the old file, which keeps
                // it mapped. Stop it; it runs again once the save is done.
                self.saves.search_stopped |= self.find.stop();
                let due = Instant::now() + saver::POLL;
                self.saves.poll_at = Some(self.saves.poll_at.map_or(due, |t| t.min(due)));
            }
            SaveStep::Stale => {}
            step => self.save_ended(i, step, event_loop),
        }
    }

    /// Check whether written saves can replace their files yet (Windows).
    fn poll_saves(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        // From the end: finishing a save may close its tab.
        for i in (0..self.tabs.len()).rev() {
            if i < self.tabs.len() && self.docs[self.tabs[i].doc].is_replacing() {
                let step = self.docs[self.tabs[i].doc].poll_replace(now);
                self.save_step(i, step, event_loop);
            }
        }
    }

    /// A save of tab `i` that ran on a worker ended: report it, and close
    /// the tab or quit if that was waiting for it.
    fn save_ended(&mut self, i: usize, step: SaveStep, event_loop: &ActiveEventLoop) {
        let after = std::mem::take(&mut self.tabs[i].after_save);
        let name = self.docs[self.tabs[i].doc].display_name();
        self.title_key = None;
        if self.saves.search_stopped && !self.tabs.iter().any(|t| self.docs[t.doc].is_replacing()) {
            self.saves.search_stopped = false;
            if self.find.open {
                self.search(false);
            }
        }
        let failure = match step {
            SaveStep::Saved => None,
            SaveStep::NotReplaced(message) => Some(message),
            SaveStep::Failed(e) => Some(e.to_string()),
            SaveStep::Waiting | SaveStep::Stale => return,
        };
        let saved = failure.is_none();
        if let Some(message) = failure {
            log::error!("{message}");
            if let Some(save_as) = &after.save_as {
                self.docs[self.tabs[i].doc]
                    .path
                    .clone_from(&save_as.old_path);
            }
            // Whatever was waiting for this save must not go ahead.
            self.saves.quit_after = false;
            for tab in &mut self.tabs {
                tab.discard_at = None;
            }
            self.say(message.clone(), true);
            self.dialog(MessageLevel::Error, "Could not save", &message);
        } else {
            log::info!("saved {name}");
            self.say(format!("Saved {name}"), false);
            if after.save_as.is_some() {
                // Saved under a new name (see App::save_as).
                self.docs[self.tabs[i].doc].detect_language(&self.languages);
                let mut doc = std::mem::take(&mut self.docs[self.tabs[i].doc]);
                self.configure_indent(&mut doc);
                self.docs[self.tabs[i].doc] = doc;
                self.sync_watch();
                self.reveal_active();
            }
        }
        // Changes on disk while it ran were set aside; look now, after the
        // report so that any warning stays on screen.
        if let Some(path) = self.docs[self.tabs[i].doc].canonical.clone() {
            self.disk_changed(&[Change {
                path,
                structural: false,
            }]);
        }
        if saved && after.close {
            self.close_tab(i, event_loop);
        } else if saved && self.saves.quit_after {
            self.quit(event_loop);
        }
    }

    /// Say why the active document cannot be edited now, if it cannot: it
    /// is read-only, or a save is under way. Returns whether it cannot.
    fn refuses_edits(&mut self) -> bool {
        if let Some(why) = self.doc().read_only.clone() {
            self.say(why, true);
            return true;
        }
        self.waits_for_save()
    }

    /// Say that edits to the active document wait for its save, if one is
    /// under way. Returns whether it is.
    fn waits_for_save(&mut self) -> bool {
        if !self.doc().is_saving() {
            return false;
        }
        let name = self.doc().display_name();
        self.say(format!("Saving {name}… edits wait until it is done"), false);
        true
    }

    /// Ask for a file name and save tab `i` there.
    fn save_as(&mut self, i: usize) -> Saved {
        if self.docs[self.tabs[i].doc].is_saving() {
            self.say("Already saving; edits wait until it is done", false);
            return Saved::Started;
        }
        let doc = &self.docs[self.tabs[i].doc];
        let dir = doc
            .path
            .as_ref()
            .and_then(|p| p.parent())
            .map_or_else(|| self.root.clone(), Path::to_path_buf);
        let mut dialog = FileDialog::new()
            .set_directory(dir)
            .set_file_name(doc.display_name());
        if let Some(window) = &self.window {
            dialog = dialog.set_parent(window.as_ref());
        }
        let Some(path) = dialog.save_file() else {
            return Saved::No;
        };
        let old = self.docs[self.tabs[i].doc].path.replace(path);
        self.title_key = None;
        let saved = self.save(i);
        match saved {
            Saved::Yes => {
                // A new name can mean a new language: untitled to `notes.md`.
                self.docs[self.tabs[i].doc].detect_language(&self.languages);
                let mut doc = std::mem::take(&mut self.docs[self.tabs[i].doc]);
                self.configure_indent(&mut doc);
                self.docs[self.tabs[i].doc] = doc;
                self.sync_watch();
                self.reveal_active();
            }
            // A huge file: App::save_ended does the above, or goes back to
            // the old name.
            Saved::Started => self.tabs[i].after_save.save_as = Some(SaveAs { old_path: old }),
            Saved::No => {
                // Keep pointing at the old file, so the next save does not
                // silently retry the path that failed.
                self.docs[self.tabs[i].doc].path = old;
            }
        }
        saved
    }

    fn dialog(&self, level: MessageLevel, title: &str, text: &str) {
        let mut dialog = MessageDialog::new()
            .set_level(level)
            .set_title(title)
            .set_description(text)
            .set_buttons(MessageButtons::Ok);
        if let Some(window) = &self.window {
            dialog = dialog.set_parent(window.as_ref());
        }
        dialog.show();
    }

    /// Offer to save tab `i` if it has unsaved changes. Returns
    /// [`Discard::Cancel`] if the user cancelled, or the save failed, and
    /// the tab must stay open; [`Discard::Wait`] while a save runs.
    fn confirm_discard(&mut self, i: usize) -> Discard {
        let unattended = self.options.smoke || self.options.screenshot.is_some();
        let doc = &self.docs[self.tabs[i].doc];
        if doc.is_saving() {
            return Discard::Wait;
        }
        // A deleted file's tab holds the only copy left, edited or not.
        if !(doc.is_dirty() || doc.deleted) || unattended {
            return Discard::Yes;
        }
        if self.tabs[i].discard_at == Some(doc.history.top_id()) {
            return Discard::Yes; // answered earlier in this quit
        }
        self.switch_to(i);
        let doc = &self.docs[self.tabs[i].doc];
        let name = doc.display_name();
        let question = if doc.deleted {
            format!("{name} was deleted on disk. Save it before closing?")
        } else {
            format!("Save changes to {name} before closing?")
        };
        let mut dialog = MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title("Unsaved changes")
            .set_description(question)
            .set_buttons(MessageButtons::YesNoCancelCustom(
                "Save".into(),
                "Don't Save".into(),
                "Cancel".into(),
            ));
        if let Some(window) = &self.window {
            dialog = dialog.set_parent(window.as_ref());
        }
        let save = match dialog.show() {
            MessageDialogResult::Yes => true,
            MessageDialogResult::Custom(label) if label == "Save" => true,
            MessageDialogResult::No => false,
            MessageDialogResult::Custom(label) if label == "Don't Save" => false,
            _ => return Discard::Cancel, // Cancel or dismissed: keep editing.
        };
        if !save {
            self.tabs[i].discard_at = Some(self.docs[self.tabs[i].doc].history.top_id());
            return Discard::Yes;
        }
        match self.save(i) {
            Saved::Yes => Discard::Yes,
            Saved::Started => Discard::Wait,
            Saved::No => Discard::Cancel,
        }
    }

    /// Close tab `i`, offering to save it. Closing the last tab quits. A
    /// tab whose huge file is being saved closes once that is done.
    fn close_tab(&mut self, i: usize, event_loop: &ActiveEventLoop) {
        // Another tab still shows the document: nothing to ask.
        let discard = if self.shown_elsewhere(i) {
            Discard::Yes
        } else {
            self.confirm_discard(i)
        };
        match discard {
            Discard::Yes => {}
            Discard::Cancel => return,
            Discard::Wait => {
                self.tabs[i].after_save.close = true;
                let name = self.docs[self.tabs[i].doc].display_name();
                self.say(format!("Saving {name}; it closes when that is done"), false);
                return;
            }
        }
        if self.tabs.len() == 1 {
            self.save_state();
            event_loop.exit();
            return;
        }
        self.remember_closed(i);
        self.remove_tab(i);
        while !self.ensure_loaded(self.active) {}
        self.session_changed();
        self.title_key = None;
        self.preedit = None;
        if self.find.open {
            self.search(false);
        }
        self.sync_watch();
        self.reveal_active();
        self.changed(false);
    }

    /// Whether another tab shows tab `i`'s document, so closing `i` loses
    /// nothing.
    fn shown_elsewhere(&self, i: usize) -> bool {
        let doc = self.tabs[i].doc;
        self.tabs
            .iter()
            .enumerate()
            .any(|(j, t)| j != i && t.doc == doc)
    }

    /// Let every tab catch up with the edits other tabs made to its
    /// document: cursors and the top of the view move through them, so
    /// both stay on the same text. Then the edits are dropped, since every
    /// tab has followed them. Runs before each frame.
    fn sync_panes(&mut self) {
        for tab in &mut self.tabs {
            tab.view.follow(&self.docs[tab.doc]);
            let shifts = tab.carets.fit(&self.docs[tab.doc]);
            for shift in shifts {
                tab.view.scroll_top = shift.map(tab.view.scroll_top);
            }
        }
        for (_, doc) in self.docs.iter_mut() {
            doc.journal_mut().trim();
        }
    }

    /// The groups and folds commands ([`Command::is_layout`]).
    fn run_layout_command(&mut self, cmd: Command<'_>, event_loop: &ActiveEventLoop) {
        match cmd {
            Command::SplitRight => self.split_right(),
            Command::CloseGroup => self.close_group(event_loop),
            Command::FocusLeftGroup => {
                if let Some(g) = self.groups.focused.checked_sub(1) {
                    self.focus_group(g);
                }
            }
            Command::FocusRightGroup => self.focus_group(self.groups.focused + 1),
            Command::MoveTabToNextGroup => self.move_tab_to_next_group(),
            Command::Fold => self.fold_at_cursor(false),
            Command::ToggleFold => self.fold_at_cursor(true),
            Command::Unfold => self.unfold_at_cursor(),
            Command::FoldAll => self.fold_all(None),
            Command::FoldLevel(n) => self.fold_all(Some(n)),
            Command::UnfoldAll => {
                self.tabs[self.active].view.folds.clear();
                self.changed(true);
            }
            Command::ZoomIn => self.zoom_to(self.font_size.round() + 1.0),
            Command::ZoomOut => self.zoom_to(self.font_size.round() - 1.0),
            Command::ResetZoom => self.zoom_to(self.config.font_size),
            Command::ToggleWhitespace => {
                self.whitespace = self.whitespace.next();
                self.say(
                    format!("Render whitespace: {}", self.whitespace.name()),
                    false,
                );
                self.changed(false);
            }
            Command::ToggleIndentGuides => {
                self.indent_guides = !self.indent_guides;
                let state = if self.indent_guides { "on" } else { "off" };
                self.say(format!("Indent guides {state}"), false);
                self.changed(false);
            }
            Command::ToggleStickyScroll => {
                self.sticky_scroll = !self.sticky_scroll;
                let state = if self.sticky_scroll { "on" } else { "off" };
                self.say(format!("Sticky scroll {state}"), false);
                self.changed(false);
            }
            Command::ToggleMinimap => {
                self.minimap = !self.minimap;
                let state = if self.minimap { "shown" } else { "hidden" };
                self.say(format!("Minimap {state}"), false);
                self.changed(false);
            }
            _ => {}
        }
    }

    /// Fold the region the cursor's line starts, or else the one around
    /// the cursor (M13). With `toggle`, a folded line unfolds instead.
    fn fold_at_cursor(&mut self, toggle: bool) {
        let tab_width = self.tab_width();
        let tab = &mut self.tabs[self.active];
        if tab.compare.is_some() || tab.merge.is_some() {
            return;
        }
        let doc = &self.docs[tab.doc];
        let cursor = tab.carets.cursor(&doc.buffer);
        let span = doc.buffer.line_span(cursor);
        if toggle && tab.view.folds.unfold_line(span).is_some() {
            self.changed(true);
            return;
        }
        let mut window = Vec::new();
        let folded = |r: &Range<usize>| tab.view.folds.active().contains(r);
        let region = folding::region_at(doc, cursor, tab_width, &mut window)
            .filter(|r| !folded(r))
            .or_else(|| folding::enclosing(doc, cursor, tab_width, &mut window))
            .filter(|r| !folded(r));
        let Some(region) = region else {
            self.say("Nothing to fold here", false);
            return;
        };
        tab.view.folds.fold(region);
        self.cursor_out_of_folds(self.active);
        self.changed(false);
    }

    /// Unfold the fold on the cursor's line.
    fn unfold_at_cursor(&mut self) {
        let tab = &mut self.tabs[self.active];
        let t = &self.docs[tab.doc].buffer;
        let cursor = tab.carets.cursor(t);
        if tab.view.folds.unfold_line(t.line_span(cursor)).is_some()
            || tab.view.folds.reveal(cursor)
        {
            self.changed(true);
        }
    }

    /// A fold just made may hide the cursor: put it at the end of the
    /// fold's first line, where it stays in view.
    fn cursor_out_of_folds(&mut self, i: usize) {
        let tab = &mut self.tabs[i];
        let cursor = tab.carets.cursor(&self.docs[tab.doc].buffer);
        if let Some(f) = tab.view.folds.hiding(cursor) {
            Editor::new(&mut self.docs[tab.doc], &mut tab.carets).click(f.start, false);
        }
    }

    /// Fold All (`level` `None`) or Fold Level N: the regions are found on
    /// a worker, and fold when it reports ([`App::folded`]).
    fn fold_all(&mut self, level: Option<usize>) {
        let tab_width = self.tab_width();
        let tab = &self.tabs[self.active];
        if tab.compare.is_some() || tab.merge.is_some() {
            return;
        }
        let proxy = self.proxy.clone();
        let started = folding::start(&self.docs[tab.doc], tab_width, level, move |r| {
            let _ = proxy.send_event(AppEvent::Folded(r));
        });
        match started {
            Ok(job) => self.tabs[self.active].fold_job = Some(job),
            Err(e) => self.say(e, true),
        }
    }

    /// A Fold All scan finished: fold what it found, unless the text
    /// changed meanwhile.
    fn folded(&mut self, result: folding::FoldResult) {
        let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.fold_job.as_ref().is_some_and(|j| j.token == result.token))
        else {
            return; // closed meanwhile
        };
        let tab = &mut self.tabs[i];
        let Some(job) = tab.fold_job.take() else {
            return;
        };
        let doc = &self.docs[tab.doc];
        if doc.version != job.version {
            self.say("The file changed while folding; fold again", false);
            return;
        }
        if result.regions.is_empty() {
            self.say("Nothing to fold", false);
            return;
        }
        tab.view.follow(doc);
        tab.view.folds.fold_many(result.regions);
        self.cursor_out_of_folds(i);
        self.changed(false);
    }

    /// Split Right: a new group right of the focused one, showing the
    /// current document in a tab of its own, with the same cursors and
    /// scroll to start from. Its find bar starts closed.
    fn split_right(&mut self) {
        if self.groups.len() >= groups::MAX_GROUPS {
            self.say(
                format!("At most {} groups side by side", groups::MAX_GROUPS),
                false,
            );
            return;
        }
        let at = self.groups.insert_after(self.groups.focused);
        for tab in &mut self.tabs {
            if tab.group >= at {
                tab.group += 1;
            }
        }
        self.groups.list[at].find = FindBar::sharing(&self.find);
        let from = &self.tabs[self.active];
        let mut tab = Tab::new(from.doc, from.view.wrap);
        tab.group = at;
        tab.carets = from.carets.clone();
        tab.view.scroll_top = from.view.scroll_top;
        tab.view.scroll_px = from.view.scroll_px;
        self.tabs.push(tab);
        self.switch_to(self.tabs.len() - 1);
    }

    /// Close Group: close the focused group's tabs, asking about unsaved
    /// changes as Close Tab does; the group goes with its last tab. The
    /// last group stays.
    fn close_group(&mut self, event_loop: &ActiveEventLoop) {
        if self.groups.len() == 1 {
            return;
        }
        let g = self.groups.focused;
        for i in self.group_tabs(g).into_iter().rev() {
            let before = self.tabs.len();
            self.close_tab(i, event_loop);
            if self.tabs.len() == before {
                return; // cancelled, or waiting on a save
            }
        }
    }

    /// Move Tab to Next Group: the current tab moves to the group on the
    /// right, a new one if this is the rightmost and there is room, else
    /// the first. A group left empty closes.
    fn move_tab_to_next_group(&mut self) {
        let i = self.active;
        let from = self.groups.focused;
        let mut to = from + 1;
        if to == self.groups.len() {
            if self.groups.len() < groups::MAX_GROUPS {
                to = self.groups.insert_after(from);
                self.groups.list[to].find = FindBar::sharing(&self.find);
            } else {
                to = 0;
            }
        }
        if to == from {
            return;
        }
        self.doc_mut().history.seal();
        self.park_focus();
        let mut tabs: Vec<usize> = self.tabs.iter().map(|t| t.group).collect();
        self.groups.tab_moved(&mut tabs, i, to);
        for (tab, g) in self.tabs.iter_mut().zip(tabs) {
            tab.group = g;
        }
        self.unpark_focus();
        self.after_focus_moved();
    }

    /// Take tab `i` out of the list, keeping every group's active tab
    /// pointing at the same tab, or at a neighbour in its strip when it
    /// was the one removed. A group left with no tabs closes. Needs
    /// another tab to remain.
    fn remove_tab(&mut self, i: usize) {
        let g = self.tabs[i].group;
        let last_view = !self.shown_elsewhere(i);
        self.park_focus();
        let closed = self.tabs.remove(i);
        if last_view {
            // Its last tab: the document closes.
            self.docs.remove(closed.doc);
        }
        let mut tabs: Vec<usize> = self.tabs.iter().map(|t| t.group).collect();
        self.groups.tab_removed(&mut tabs, i, g);
        for (tab, g) in self.tabs.iter_mut().zip(tabs) {
            tab.group = g;
        }
        self.unpark_focus();
    }

    /// Quit, offering to save every tab with unsaved changes. If a huge
    /// file is being saved, quitting waits for it and then goes on
    /// ([`App::save_ended`]), without asking again about tabs already
    /// answered.
    fn quit(&mut self, event_loop: &ActiveEventLoop) {
        for i in 0..self.tabs.len() {
            // A document in two tabs is asked about once.
            if (0..i).any(|j| self.tabs[j].doc == self.tabs[i].doc) {
                continue;
            }
            match self.confirm_discard(i) {
                Discard::Yes => {}
                Discard::Cancel => {
                    self.saves.quit_after = false;
                    for tab in &mut self.tabs {
                        tab.discard_at = None;
                    }
                    return;
                }
                Discard::Wait => {
                    self.saves.quit_after = true;
                    let name = self.docs[self.tabs[i].doc].display_name();
                    self.say(format!("Saving {name}; quitting when that is done"), false);
                    return;
                }
            }
        }
        if let Some(clean) = self.merge_saved_clean {
            // `--merge`: git's mergetool reads the exit code.
            self.exit_code = i32::from(!clean);
        }
        self.save_state();
        event_loop.exit();
    }

    fn reload_config(&mut self) {
        let (mut config, mut warnings) = Config::load();
        if let Some(theme) = &self.options.theme {
            config.theme.clone_from(theme);
        }
        let (keymap, key_warnings) = Keymap::new(&config.keys);
        warnings.extend(key_warnings);
        let (theme, theme_warnings) = Theme::load(&config.theme);
        warnings.extend(theme_warnings);
        self.theme = theme;
        // Language mappings and user languages may have changed; files
        // whose language did not change keep their state.
        let (languages, language_warnings) = Languages::load(&config.languages);
        warnings.extend(language_warnings);
        warnings.extend(io::set_encoding_rules(&config.encodings));
        self.languages = languages;
        for (_, doc) in self.docs.iter_mut().filter(|(_, d)| !d.is_placeholder()) {
            doc.detect_language(&self.languages);
            doc.configure_indent(config.indent.for_language(doc.language_key()));
        }
        self.keymap = keymap;
        let old = std::mem::replace(&mut self.config, config);
        // A setting changed in the file wins over what the session did to
        // it; one left alone keeps the session's zoom and toggles.
        if (old.font_size - self.config.font_size).abs() > f64::EPSILON {
            self.font_size = self.config.font_size;
            self.remember();
        }
        if old.render_whitespace != self.config.render_whitespace {
            self.whitespace = self.config.render_whitespace;
        }
        if old.indent_guides != self.config.indent_guides {
            self.indent_guides = self.config.indent_guides;
        }
        if old.sticky_scroll != self.config.sticky_scroll {
            self.sticky_scroll = self.config.sticky_scroll;
        }
        if old.minimap != self.config.minimap {
            self.minimap = self.config.minimap;
        }
        if let Some(menu) = &self.menu_bar {
            menu.update(&self.keymap);
        }
        if let (Some(gpu), Some(window)) = (self.gpu.as_mut(), self.window.as_ref()) {
            gpu.set_font_size(to_f32(
                self.font_size * effective_scale(window.scale_factor()),
            ));
            gpu.set_tab_width(self.config.tab_width);
            gpu.set_ligatures(self.config.ligatures);
        }
        if old.font != self.config.font {
            self.load_editor_font();
        }
        if old.ui_font != self.config.ui_font {
            self.load_ui_font();
        }
        if warnings.is_empty() {
            self.say("Config reloaded", false);
        } else {
            for w in &warnings {
                log::warn!("{w}");
            }
            self.say(warnings.join("; "), true);
        }
    }

    /// The config file and update commands, which touch nothing in the
    /// documents.
    fn run_setup(&mut self, cmd: Command<'_>) {
        match cmd {
            Command::OpenConfig => self.open_config(),
            Command::CheckUpdates => {
                self.say("Checking GitHub for the latest release…", false);
                spawn_update_check(&self.proxy, false);
            }
            Command::OpenRelease => self.open_release(),
            _ => {}
        }
    }

    /// Open the releases page in the browser. It is the list rather than
    /// one release, so it needs no state from an earlier check and the
    /// newest release is at the top.
    fn open_release(&mut self) {
        let url = update::RELEASES_URL;
        match update::open_url(url) {
            Ok(()) => self.say(format!("Opening {url}"), false),
            Err(e) => self.say(format!("Cannot open the browser: {e}"), true),
        }
    }

    /// Tell the person what a check for updates found. The startup check
    /// says nothing unless a newer release exists: a tool should not
    /// greet every launch with "up to date" or "no network".
    fn update_checked(&mut self, result: Result<update::Outcome, UpdateError>, at_startup: bool) {
        let current = env!("CARGO_PKG_VERSION");
        match result {
            Ok(update::Outcome::Newer(release)) => {
                self.say(
                    format!(
                        "Nimbus {} is available (you have {current}); run Open Latest Release Page to get it",
                        release.version
                    ),
                    false,
                );
            }
            Ok(update::Outcome::UpToDate(release)) => {
                if !at_startup {
                    let note = if release.version == current {
                        String::new()
                    } else {
                        format!(" (latest release: {})", release.version)
                    };
                    self.say(format!("Nimbus {current} is up to date{note}"), false);
                }
            }
            Err(e) => {
                if at_startup {
                    log::info!("update check: {e}");
                } else {
                    self.say(e.to_string(), true);
                }
            }
        }
    }

    /// Open the config file in a tab, creating it from the template first.
    fn open_config(&mut self) {
        let Some(path) = config::path() else {
            self.say("No config directory on this system", true);
            return;
        };
        if !path.exists() {
            let created = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, config::TEMPLATE));
            if let Err(e) = created {
                self.say(format!("Cannot create {}: {e}", path.display()), true);
                return;
            }
        }
        self.open_path(&path);
        self.say("Edit, save, then run Reload Config from the palette", false);
    }

    fn open_picker(&mut self, kind: Kind) {
        let proxy = self.proxy.clone();
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = proxy.send_event(AppEvent::PickerChanged);
        });
        self.picker = Some(match kind {
            Kind::Palette => Picker::palette(notify),
            Kind::Files => Picker::files(
                &self.sidebar.tree.roots(),
                self.recent_in_workspace(),
                notify,
            ),
            Kind::Recent => {
                let paths: Vec<PathBuf> = self
                    .ws
                    .state
                    .recent
                    .iter()
                    .map(|r| r.path.clone())
                    .collect();
                self.check_recent(paths.clone());
                Picker::recent(paths, &self.root, notify)
            }
            Kind::Line => {
                let t = &self.doc().buffer;
                Picker::go_to_line(notify, t.is_fully_indexed().then(|| t.len_lines()))
            }
            Kind::Revision => Picker::revision(notify),
            Kind::Symbols => {
                let doc = self.doc();
                let symbols = symbol_items(doc);
                if symbols.is_empty() {
                    let why = match doc.syntax.phase_name() {
                        "off" | "simple" => "No symbols: this file has no syntax tree",
                        "waiting" | "parsing" => "No symbols yet: the file is still being parsed",
                        _ => "No symbols found in this file",
                    };
                    self.say(why, false);
                }
                Picker::symbols(notify, symbols)
            }
            Kind::Name(naming) => Picker::name(naming, notify),
            Kind::Workspaces => Picker::workspaces(crate::workspace_file::names(), notify),
            Kind::Tabs => {
                let names = self
                    .tabs
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != self.active)
                    .map(|(i, t)| (i, self.docs[t.doc].display_name()))
                    .collect();
                Picker::tabs(names, notify)
            }
            Kind::Encoding(purpose) => {
                let doc = self.doc();
                let current = Choice {
                    encoding: doc.encoding,
                    bom: doc.bom,
                };
                Picker::encodings(purpose, current, notify)
            }
        });
        self.find.focused = false;
        self.tick_picker();
    }

    /// Let the matcher work; redraw while results are coming in.
    fn tick_picker(&mut self) {
        let Some(p) = self.picker.as_mut() else {
            return;
        };
        p.tick();
        self.changed(false);
    }

    /// Handle a command while the picker is open. Returns `false` for
    /// commands that close it and should still run (none so far).
    fn run_picker(&mut self, cmd: Command<'_>, event_loop: &ActiveEventLoop) -> bool {
        let Some(p) = self.picker.as_mut() else {
            return false;
        };
        match cmd {
            Command::Escape => self.picker = None,
            Command::Move(Motion::Up, _) => p.move_selection(-1),
            Command::Move(Motion::Down, _) => p.move_selection(1),
            Command::Move(Motion::PageUp, _) => {
                p.move_selection(-isize::try_from(picker::VISIBLE_ROWS).unwrap_or(1));
            }
            Command::Move(Motion::PageDown, _) => {
                p.move_selection(isize::try_from(picker::VISIBLE_ROWS).unwrap_or(1));
            }
            Command::Newline => {
                let chosen = p.chosen();
                let kind = p.kind;
                self.picker = None;
                match chosen {
                    Some(Item::Action(a)) => self.run(a.command, event_loop),
                    Some(Item::Name(name)) => self.name_chosen(kind, &name),
                    Some(Item::Workspace(name)) => self.open_named_workspace(&name),
                    Some(Item::File(path) | Item::Recent(path)) => self.open_path(&path),
                    Some(Item::Line(n)) => self.go_to_line(n),
                    Some(Item::Symbol(s, _)) => self.go_to_byte(s.start),
                    Some(Item::Tab(i, _)) => self.compare_with_tab_index(i),
                    Some(Item::Revision(rev)) => self.compare_with_revision(rev),
                    Some(Item::Encoding(purpose, choice, _)) => {
                        self.encoding_chosen(purpose, choice);
                    }
                    None => {}
                }
            }
            Command::Palette => {
                if p.kind == Kind::Palette {
                    self.picker = None;
                } else {
                    self.open_picker(Kind::Palette);
                }
            }
            Command::QuickOpen => {
                if p.kind == Kind::Files {
                    self.picker = None;
                } else {
                    self.open_picker(Kind::Files);
                }
            }
            Command::Close => return false,
            _ => {
                let paste = (cmd == Command::Paste)
                    .then(|| self.clipboard_get())
                    .flatten();
                let tab_width = self.tab_width();
                let Some(p) = self.picker.as_mut() else {
                    return true;
                };
                if let Some(text) = edit_field(&mut p.input, cmd, paste.as_deref(), tab_width) {
                    self.clipboard_set(text);
                }
                self.tick_picker();
            }
        }
        true
    }
    fn open_find(&mut self, replace_mode: bool) {
        self.sidebar.focused = false;
        let selection = self.carets().selected_text(&self.doc().buffer);
        self.find.open(replace_mode, selection);
        self.find_origin = self.selection().range().start;
        self.search(true);
    }

    /// Search the document for the current query. With `jump`, select the
    /// first match from where the search started, as incremental search does.
    fn search(&mut self, jump: bool) {
        let proxy = self.proxy.clone();
        let doc = &self.docs[self.tabs[self.active].doc];
        let complete = self
            .find
            .search(doc, move |b| proxy.send_event(AppEvent::Search(b)).is_ok());
        self.find_pending_jump = jump && !complete;
        if jump && complete {
            self.jump_to_match(self.find_origin, false);
        }
    }

    /// Select the match at or after `from` (or before, `backwards`).
    fn jump_to_match(&mut self, from: usize, backwards: bool) {
        if let Some(range) = self.find.select_from(from, backwards) {
            self.ed().select(range);
            if let Some(layout) = self.layout() {
                let tab_width = self.tab_width();
                let tab = &mut self.tabs[self.active];
                tab.view.ensure_cursor_visible(
                    &self.docs[tab.doc],
                    &tab.carets,
                    &layout,
                    tab_width,
                );
            }
        }
    }

    /// Handle a command while the find bar has focus. Returns `false` for
    /// commands the editor should handle instead (save, close, undo of the
    /// document is not among them: undo applies to the focused field).
    fn run_find(&mut self, cmd: Command<'_>) -> bool {
        let backwards = self.modifiers.shift_key();
        match cmd {
            Command::Escape => self.find.close(),
            Command::Find | Command::Replace => {
                self.find.replace_mode = cmd == Command::Replace;
                self.find.focus = Field::Find;
            }
            Command::Tab | Command::Outdent if self.find.replace_mode => {
                self.find.focus = match self.find.focus {
                    Field::Find => Field::Replace,
                    Field::Replace => Field::Find,
                };
                self.find.field_mut().ed().select_all();
            }
            Command::Tab | Command::Outdent => {}
            Command::Newline if self.find.focus == Field::Replace => self.replace_one(),
            Command::Newline => {
                let sel = self.selection().range();
                let from = if backwards {
                    sel.start
                } else {
                    sel.end.max(sel.start + 1)
                };
                self.jump_to_match(from, backwards);
            }
            Command::ReplaceAll => self.replace_all(),
            Command::ToggleCase(_) => {
                self.find.case_sensitive = !self.find.case_sensitive;
                self.search(true);
            }
            Command::ToggleRegex(_) => {
                self.find.regex = !self.find.regex;
                self.search(true);
            }
            Command::Save
            | Command::SaveAs
            | Command::Close
            | Command::CloseTab
            | Command::ToggleWrap
            | Command::ToggleTheme
            | Command::ToggleSidebar
            | Command::GoToLine
            | Command::GoToSymbol
            | Command::FocusSidebar
            | Command::ToggleIgnored
            | Command::ToggleDebugOverlay
            | Command::ReloadFromDisk
            | Command::ReopenWithEncoding
            | Command::SaveWithEncoding
            | Command::ChangeLineEndings(_)
            | Command::KeepMine
            | Command::OpenFile
            | Command::NewFile
            | Command::NextTab
            | Command::PrevTab
            | Command::GoToTab(_)
            | Command::QuickOpen
            | Command::Palette
            | Command::ReloadConfig
            | Command::OpenConfig
            | Command::CompareSaved
            | Command::CompareFile
            | Command::CompareTab
            | Command::DiffNext
            | Command::DiffPrev
            | Command::DiffUnified
            | Command::DiffClose
            | Command::MergeFiles
            | Command::MergeWithBase
            | Command::TakeLeft
            | Command::TakeRight
            | Command::TakeBoth
            | Command::TakeBothReversed
            | Command::NextConflict
            | Command::PrevConflict
            | Command::ResolveMarkers
            | Command::MergeWithDisk
            | Command::CheckUpdates
            | Command::OpenRelease => return false,
            _ if cmd.is_find_in_files() || cmd.is_layout() || cmd.is_compare_extra() => {
                return false;
            }
            _ => {
                let paste = (cmd == Command::Paste)
                    .then(|| self.clipboard_get())
                    .flatten();
                let tab_width = self.tab_width();
                let field = self.find.field_mut();
                let before = field.doc.version;
                let copied = edit_field(field, cmd, paste.as_deref(), tab_width);
                let changed = field.doc.version != before;
                if let Some(text) = copied {
                    self.clipboard_set(text);
                }
                if changed && self.find.focus == Field::Find {
                    self.search(true);
                }
            }
        }
        true
    }

    /// Replace the current match (if the editor's selection is one) and go
    /// to the next.
    fn replace_one(&mut self) {
        if self.refuses_edits() {
            return;
        }
        let sel = self.selection().range();
        let is_match = self.find.current.and_then(|i| self.find.matches.get(i)) == Some(&sel);
        if is_match && !sel.is_empty() {
            if let Some(replacement) = self.find.replacement_for(self.doc(), sel.clone())
                && let Err(e) = self.ed().replace(sel, &replacement)
            {
                log::error!("replace refused: {e}");
            }
            self.doc_mut().history.seal();
            // Search the changed text, then go to the next match after the
            // replacement; on large files that waits for the results.
            self.find_origin = self.cursor();
            self.search(true);
            return;
        }
        let from = self.cursor();
        self.jump_to_match(from, false);
    }

    /// Replace every match as one undo step.
    fn replace_all(&mut self) {
        if self.refuses_edits() {
            return;
        }
        if !self.find.complete && !self.find.capped {
            let doc = &self.docs[self.tabs[self.active].doc];
            if doc.buffer.is_mapped() {
                // Finishing the search here could read gigabytes on the
                // main thread.
                self.say("Still searching; Replace All once the count is final", true);
                return;
            }
            // A background search is still running: finish it here so no
            // match is missed.
            self.find.search_now(doc);
        }
        if self.find.capped {
            self.say(
                format!(
                    "Too many matches to replace all (over {}); narrow the search",
                    find::MAX_KEPT
                ),
                true,
            );
            return;
        }
        let items: Vec<(std::ops::Range<usize>, String)> = self
            .find
            .matches
            .iter()
            .filter_map(|r| {
                // A byte-level pattern can match inside a character;
                // replacing there would be refused, and with it the rest.
                let t = &self.doc().buffer;
                if !t.is_char_boundary(r.start) || !t.is_char_boundary(r.end) {
                    return None;
                }
                let rep = self.find.replacement_for(self.doc(), r.clone())?;
                Some((r.clone(), rep))
            })
            .collect();
        match self.ed().replace_many(&items) {
            Ok(n) => {
                self.search(false);
                self.say(
                    format!("Replaced {n} match{}", if n == 1 { "" } else { "es" }),
                    false,
                );
            }
            Err(e) => {
                log::error!("replace all refused: {e}");
                self.say("Replace all failed; nothing was lost", true);
            }
        }
    }

    /// Primary+D, saying when there is nothing more to add.
    fn add_next_occurrence(&mut self) {
        let note = match self.ed().add_next_occurrence() {
            Occurrence::SelectedWord | Occurrence::Added | Occurrence::NoWord => return,
            Occurrence::AllSelected => "Every occurrence is selected",
            Occurrence::NotFound => "No other occurrence",
            Occurrence::TooLong => "The selection is too long to look for",
            Occurrence::GaveUp => "None in the next 32 MB; press again to look further",
        };
        self.say(note, false);
    }

    /// Copy, cut or paste in the editor. Cut and paste are their own undo
    /// steps.
    fn clipboard_command(&mut self, cmd: Command<'_>) -> Result<(), nimbus_core::EditError> {
        if cmd == Command::Paste {
            return match self.clipboard_get() {
                Some(text) => self.ed().paste(&text).map(drop),
                None => Ok(()),
            };
        }
        let len = self.carets().selected_len();
        if len > MAX_COPY_BYTES {
            // Refused before reading anything, and Cut deletes nothing.
            self.say(
                format!(
                    "The selection is {} MB; copy and cut take at most {} MB",
                    len >> 20,
                    MAX_COPY_BYTES >> 20
                ),
                true,
            );
            return Ok(());
        }
        let Some(text) = self.carets().selected_text(&self.doc().buffer) else {
            return Ok(());
        };
        self.clipboard_set(text);
        if cmd != Command::Cut {
            return Ok(());
        }
        let mut ed = self.ed();
        ed.doc.history.seal();
        let result = ed.delete_selections().map(drop);
        ed.doc.history.seal();
        result
    }

    /// The system clipboard, connected on first use rather than at startup.
    fn clipboard(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(c) => self.clipboard = Some(c),
                Err(e) => log::warn!("clipboard unavailable: {e}"),
            }
        }
        self.clipboard.as_mut()
    }

    fn clipboard_set(&mut self, text: String) {
        if let Some(c) = self.clipboard()
            && let Err(e) = c.set_text(text)
        {
            log::warn!("could not copy: {e}");
        }
    }

    fn clipboard_get(&mut self) -> Option<String> {
        match self.clipboard()?.get_text() {
            Ok(t) => Some(t),
            Err(e) => {
                log::debug!("nothing to paste: {e}");
                None
            }
        }
    }

    /// A left-button press: place the cursor, or select a word or line on a
    /// double- or triple-click, and start dragging.
    fn mouse_down(&mut self, now: Instant) {
        let Some(window) = self.window_layout() else {
            return;
        };
        let (x, y) = (to_f32(self.mouse.x), to_f32(self.mouse.y));
        if self.picker.is_some() {
            self.picker_click(&window, x, y);
            return;
        }
        if self.sidebar.on_divider(&window, x) {
            self.sidebar.resizing = true;
            return;
        }
        if self.sidebar.contains(&window, x) {
            self.sidebar_click(&window, x, y);
            return;
        }
        self.sidebar.focused = false;
        if y >= window.bottom {
            return self.status_bar_click(&window, x);
        }
        let columns = self.group_columns(&window);
        if let Some(d) = Groups::on_divider(&columns, x, window.scale) {
            self.groups.dragging = Some(d);
            return;
        }
        // A press in another group gives it the keyboard first.
        if let Some(g) = Groups::at(&columns, x) {
            self.focus_group(g);
        }
        let Some(layout) = self.layout() else { return };
        let (labels, active) = self.tab_labels(self.groups.focused);
        if let Some(n) = view::tab_at(&layout, &labels, active, x, y) {
            if let Some(&i) = self.group_tabs(self.groups.focused).get(n) {
                self.switch_to(i);
            }
            return;
        }
        let rows =
            usize::from(self.find.open) + usize::from(self.find.open && self.find.replace_mode);
        if let Some(row) = view::find_bar_row(&layout, y, rows) {
            // A click in the bar focuses the field on that row.
            self.find.focused = true;
            self.find.focus = if row == 1 {
                Field::Replace
            } else {
                Field::Find
            };
            self.changed(false);
            return;
        }
        if y < layout.top {
            return;
        }
        self.text_press(&layout, (x, y), now);
    }

    /// A left-button press on the focused group's text: the scrollbar, a
    /// compare or merge view's columns, or the text itself.
    fn text_press(&mut self, layout: &Layout, (x, y): (f32, f32), now: Instant) {
        let layout = *layout;
        self.find.focused = false;
        let tab_width = self.tab_width();
        let tab = &mut self.tabs[self.active];
        if tab.compare.is_some() || tab.merge.is_some() {
            if let Some(hit) = self.columns_scrollbar_hit(&layout, x, y) {
                self.grab_columns_scrollbar(hit, y, &layout);
            } else {
                self.columns_click(&layout, (x, y), now);
            }
            return;
        }
        if let Some(press) = tab.view.minimap_press(x, y) {
            self.grab_minimap(press, &layout);
            return;
        }
        if let Some(hit) = tab.view.scrollbar_hit(x, y) {
            self.grab_scrollbar(hit, y, &layout);
            return;
        }
        // A pinned line of sticky scroll (M20): jump to it.
        if let Some(line) = tab.view.sticky_at(&layout, y) {
            tab.view.scroll_to_line(line);
            let doc = &mut self.docs[tab.doc];
            let start = nimbus_core::movement::smart_home(&doc.buffer, line);
            Editor::new(doc, &mut tab.carets).click(start, false);
            doc.stop_following();
            self.blink.reset(now);
            self.changed(false);
            return;
        }
        if self.fold_click(&layout, x, y) {
            return;
        }
        let tab = &mut self.tabs[self.active];
        let pos = tab.view.hit(x, y, &self.docs[tab.doc], &layout, tab_width);
        let count = self.clicks.press(now, (x, y), layout.metrics.cell_width);
        // Shift+Alt+press: a column selection, grown by dragging.
        self.clicks.column = (count == 1 && self.modifiers.alt_key() && self.modifiers.shift_key())
            .then(|| {
                tab.view
                    .hit_column(x, y, &self.docs[tab.doc], &layout, tab_width)
            });
        if let Some(at) = self.clicks.column {
            Editor::new(&mut self.docs[tab.doc], &mut tab.carets).column_select(at, at, tab_width);
            self.clicks.adding = false;
            self.clicks.dragging = true;
            self.blink.reset(now);
            self.changed(false);
            return;
        }
        let mut doc = Editor::new(&mut self.docs[tab.doc], &mut tab.carets);
        self.clicks.adding = count == 1 && self.modifiers.alt_key();
        match count {
            1 if self.clicks.adding => {
                // Alt+click: add a cursor, or remove the one there. Only a
                // new cursor can be dragged out into a selection.
                self.clicks.adding = doc.click_add(pos);
                self.clicks.anchor = pos..pos;
            }
            1 => {
                doc.click(pos, self.modifiers.shift_key());
                // Dragging extends from the selection's anchor: the old one
                // after a Shift+click, the clicked point otherwise.
                let anchor = doc.selection().anchor;
                self.clicks.anchor = anchor..anchor;
            }
            2 => {
                let word = nimbus_core::movement::word_at(&doc.doc.buffer, pos);
                self.clicks.anchor = word.clone();
                doc.select(word);
            }
            _ => {
                let line = nimbus_core::movement::line_range(&doc.doc.buffer, pos);
                self.clicks.anchor = line.clone();
                doc.select(line);
            }
        }
        self.clicks.dragging = !(count == 1 && self.modifiers.alt_key() && !self.clicks.adding);
        self.blink.reset(now);
        self.changed(false);
    }

    /// A press on a fold chip (it unfolds) or on the gutter's fold marker
    /// column (the line folds or unfolds). Returns whether it was one.
    fn fold_click(&mut self, layout: &Layout, x: f32, y: f32) -> bool {
        let tab_width = self.tab_width();
        let tab = &mut self.tabs[self.active];
        let doc = &self.docs[tab.doc];
        if let Some(start) = tab.view.chip_at(x, y, layout.metrics.line_height) {
            tab.view.folds.unfold_line(doc.buffer.line_span(start));
            self.changed(false);
            return true;
        }
        if !View::on_fold_marker(layout, x) {
            return false;
        }
        let pos = tab.view.hit(layout.text_left, y, doc, layout, tab_width);
        let span = doc.buffer.line_span(pos);
        if tab.view.folds.unfold_line(span).is_none() {
            let mut window = Vec::new();
            let Some(region) = folding::region_at(doc, span.start, tab_width, &mut window) else {
                return false;
            };
            tab.view.folds.fold(region);
            self.cursor_out_of_folds(self.active);
        }
        self.changed(false);
        true
    }

    /// Show the fold marker on the line under the pointer while it is in
    /// the gutter, if that line can fold. Asks about a line only when the
    /// pointer moves to another one.
    fn update_fold_hover(&mut self) {
        let Some(layout) = self.layout() else { return };
        let (x, y) = (to_f32(self.mouse.x), to_f32(self.mouse.y));
        let tab_width = self.tab_width();
        let active = self.active;
        let tab = &mut self.tabs[active];
        let doc = &self.docs[tab.doc];
        let in_gutter = x >= layout.left
            && x < layout.text_left
            && y >= layout.top
            && y < layout.bottom
            && tab.compare.is_none()
            && tab.merge.is_none();
        let line = in_gutter.then(|| {
            let pos = tab.view.hit(layout.text_left, y, doc, &layout, tab_width);
            doc.buffer.line_span(pos).start
        });
        let key = line.map(|l| (active, doc.version, l));
        if key == self.fold_hover_key {
            return;
        }
        self.fold_hover_key = key;
        let mut window = Vec::new();
        let foldable = line.filter(|&l| {
            tab.view.folds.on_line(doc.buffer.line_span(l)).is_none()
                && folding::region_at(doc, l, tab_width, &mut window).is_some()
        });
        if tab.view.fold_hover != foldable {
            tab.view.fold_hover = foldable;
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }

    /// A press in the sidebar: the tree, or the Search panel.
    fn sidebar_click(&mut self, layout: &Layout, x: f32, y: f32) {
        self.find.focused = false;
        let outcome = self.sidebar.click(layout, x, y);
        self.start_listings(outcome.requests);
        if let Some(path) = outcome.open {
            self.sidebar.focused = false;
            self.open_path(&path);
        }
        if let Some(reply) = outcome.search {
            self.act_on_search_reply(&reply);
        }
        if let Some(row) = outcome.pair {
            self.open_folder_pair(row);
        }
        if let Some(toggle) = outcome.toggle {
            self.toggle_diff_option(toggle);
        }
        self.changed(false);
    }

    /// A press on the scrollbar: grab the thumb where pressed, or jump to
    /// centre it there.
    fn grab_scrollbar(&mut self, hit: view::ScrollbarHit, y: f32, layout: &Layout) {
        let tab_width = self.tab_width();
        let tab = &mut self.tabs[self.active];
        let grab = match hit {
            view::ScrollbarHit::Thumb(offset) => offset,
            view::ScrollbarHit::Track => {
                let half = tab.view.thumb_height() / 2.0;
                tab.view
                    .drag_scrollbar(y - half, &self.docs[tab.doc], layout, tab_width);
                half
            }
        };
        self.clicks.scrollbar_grab = Some(grab);
        self.docs[tab.doc].stop_following();
        self.changed(false);
    }

    /// A press on the minimap (M20): grab its mark where pressed, or
    /// centre the line pressed and grab the mark by its middle.
    fn grab_minimap(&mut self, press: minimap::Press, layout: &Layout) {
        let tab_width = self.tab_width();
        let tab = &mut self.tabs[self.active];
        let doc = &self.docs[tab.doc];
        let grab = match press {
            minimap::Press::Slider(offset) => offset,
            minimap::Press::Line(start) => {
                tab.view.centre_line(start, doc, layout, tab_width);
                tab.view.minimap_slider_height() / 2.0
            }
        };
        self.clicks.minimap_grab = Some(grab);
        self.docs[tab.doc].stop_following();
        self.changed(false);
    }

    /// The pointer moved with the minimap's mark grabbed: put its top at
    /// `y`.
    fn drag_minimap_to(&mut self, y: f32) {
        let Some(layout) = self.layout() else { return };
        let tab_width = self.tab_width();
        let tab = &mut self.tabs[self.active];
        tab.view
            .drag_minimap(y, &self.docs[tab.doc], &layout, tab_width);
        self.changed(false);
    }

    /// What a press at `(x, y)` hits on the active compare or merge tab's
    /// scrollbar, if anything.
    fn columns_scrollbar_hit(&self, layout: &Layout, x: f32, y: f32) -> Option<view::ScrollbarHit> {
        let tab = &self.tabs[self.active];
        match (tab.compare.as_ref(), tab.merge.as_ref()) {
            (Some(c), _) => c.columns.scrollbar_hit(layout, x, y),
            (None, Some(m)) => m.columns.scrollbar_hit(layout, x, y),
            (None, None) => None,
        }
    }

    /// Start dragging the compare or merge tab's scrollbar, as
    /// [`grab_scrollbar`](Self::grab_scrollbar) does for one document.
    fn grab_columns_scrollbar(&mut self, hit: view::ScrollbarHit, y: f32, layout: &Layout) {
        let grab = match hit {
            view::ScrollbarHit::Thumb(offset) => offset,
            view::ScrollbarHit::Track => {
                let half = self.columns_thumb_height(layout) / 2.0;
                self.drag_columns_scrollbar(y - half, layout);
                half
            }
        };
        self.clicks.scrollbar_grab = Some(grab);
        self.changed(false);
    }

    fn columns_thumb_height(&self, layout: &Layout) -> f32 {
        let tab = &self.tabs[self.active];
        match (tab.compare.as_ref(), tab.merge.as_ref()) {
            (Some(c), _) => c.columns.thumb_height(layout),
            (None, Some(m)) => m.columns.thumb_height(layout),
            (None, None) => 0.0,
        }
    }

    /// The pointer moved with a scrollbar grabbed: put the thumb's top at
    /// `y`, on whichever scrollbar the active tab shows.
    fn drag_scrollbar_to(&mut self, y: f32) {
        let Some(layout) = self.layout() else { return };
        let tab_width = self.tab_width();
        let tab = &mut self.tabs[self.active];
        if tab.compare.is_some() || tab.merge.is_some() {
            self.drag_columns_scrollbar(y, &layout);
        } else {
            tab.view
                .drag_scrollbar(y, &self.docs[tab.doc], &layout, tab_width);
        }
        self.changed(false);
    }

    /// Put the compare or merge tab's thumb at `y`.
    fn drag_columns_scrollbar(&mut self, y: f32, layout: &Layout) {
        let tab = &mut self.tabs[self.active];
        match (tab.compare.as_mut(), tab.merge.as_mut()) {
            (Some(c), _) => c.columns.drag_scrollbar(y, layout),
            (None, Some(m)) => m.columns.drag_scrollbar(y, layout),
            (None, None) => {}
        }
    }

    /// A click on a compare view: a fold toggles, a line places the cursor.
    fn columns_click(&mut self, layout: &Layout, (x, y): (f32, f32), now: Instant) {
        let tab_width = self.tab_width();
        let shift = self.modifiers.shift_key();
        let tab = &mut self.tabs[self.active];
        let click = match (tab.compare.as_mut(), tab.merge.as_mut()) {
            (Some(c), _) => c.click(&self.docs[tab.doc], &mut tab.view, layout, tab_width, x, y),
            (None, Some(m)) => m.click(&self.docs[tab.doc], &mut tab.view, layout, tab_width, x, y),
            (None, None) => return,
        };
        match click {
            Click::Cursor(pos) => {
                Editor::new(&mut self.docs[tab.doc], &mut tab.carets).click(pos, shift);
            }
            Click::Toggled | Click::None => {}
        }
        self.clicks.dragging = false;
        self.blink.reset(now);
        self.changed(false);
    }

    /// A key press or release: run what it is bound to, or type it. The
    /// first chord of a two-chord binding waits for the second (M11).
    fn key_input(
        &mut self,
        event: &winit::event::KeyEvent,
        now: Instant,
        event_loop: &ActiveEventLoop,
    ) {
        let pending = self.pending_chord.map(|(c, _)| c);
        let resolved = input::resolve(
            &self.keymap,
            input::KeyPress {
                key: &event.logical_key,
                physical: event.physical_key,
                text: event.text.as_deref(),
                pressed: event.state == ElementState::Pressed,
                repeat: event.repeat,
                mods: self.modifiers,
            },
            self.find.open,
            pending,
        );
        let was_pending = pending.is_some() && event.state == ElementState::Pressed;
        match resolved {
            Resolved::Command(cmd) => {
                self.pending_chord = None;
                self.blink.reset(now);
                self.debug.latency.input(now);
                self.run(cmd, event_loop);
            }
            Resolved::Prefix(chord) => {
                // Wait for the second chord; the status bar shows the
                // first meanwhile.
                self.pending_chord = Some((chord, now + CHORD_TIMEOUT));
                self.changed(false);
            }
            Resolved::Nothing => {
                if was_pending {
                    // Any other key cancels the chord.
                    self.pending_chord = None;
                    self.changed(false);
                }
            }
        }
    }

    /// A click on the status bar: the indentation item ("Spaces: 4") and
    /// the line ending ("LF") offer the choices for this file through the
    /// palette; the encoding item offers Reopen and Save with Encoding.
    fn status_bar_click(&mut self, layout: &Layout, x: f32) {
        let query = match self.tabs[self.active].view.status_item_at(layout, x) {
            Some(StatusItem::Indent) => "indent",
            Some(StatusItem::LineEnding) => "line endings",
            Some(StatusItem::Encoding) => "with encoding",
            Some(StatusItem::Follow) => {
                self.toggle_follow();
                return;
            }
            None => return,
        };
        self.open_picker(Kind::Palette);
        if let Some(p) = self.picker.as_mut()
            && let Err(e) = p.input.ed().insert(query)
        {
            log::debug!("picker query: {e}");
        }
        self.changed(false);
    }

    /// Scroll the sidebar or the text under the pointer.
    fn mouse_wheel(&mut self, delta: MouseScrollDelta) {
        let Some(window) = self.window_layout() else {
            return;
        };
        let x = to_f32(self.mouse.x);
        // The group under the pointer scrolls, focused or not.
        let g = Groups::at(&self.group_columns(&window), x).unwrap_or(self.groups.focused);
        let Some(layout) = self.group_layout(g) else {
            return;
        };
        let i = self.group_active(g);
        let lh = layout.metrics.line_height;
        let cell = layout.metrics.cell_width;
        // Trackpads send pixels, with momentum from the OS; wheels send
        // lines, already accelerated on macOS.
        let lines_per_notch = if cfg!(target_os = "macos") { 1.0 } else { 3.0 };
        let (dx, dy) = match delta {
            MouseScrollDelta::PixelDelta(p) => (to_f32(p.x), to_f32(p.y)),
            MouseScrollDelta::LineDelta(x, y) => {
                (x * cell * lines_per_notch, y * lh * lines_per_notch)
            }
        };
        if self.sidebar.contains(&window, x) {
            self.sidebar.wheel(-dy, &window);
        } else {
            let tab_width = self.tab_width();
            let tab = &mut self.tabs[i];
            match (tab.compare.as_mut(), tab.merge.as_mut()) {
                (Some(c), _) => c.scroll_by(-dy, &layout),
                (None, Some(m)) => m.scroll_by(-dy, &layout),
                (None, None) => tab
                    .view
                    .scroll_by(-dy, &self.docs[tab.doc], &layout, tab_width),
            }
            if dy > 0.0 {
                self.docs[tab.doc].stop_following();
            }
            tab.view.scroll_x_by(-dx);
        }
        self.changed(false);
    }

    /// The pointer moved while dragging divider `d`: resize the groups
    /// either side, none narrower than [`groups::MIN_CELLS`] cells.
    fn drag_group_divider(&mut self, d: usize, x: f32) {
        let Some(window) = self.window_layout() else {
            return;
        };
        let columns = self.group_columns(&window);
        let min = groups::MIN_CELLS * window.metrics.cell_width;
        self.groups.drag_divider(&columns, d, x, min);
        self.changed(false);
    }

    /// Widen the scrollbar while the pointer is over it.
    fn update_scrollbar_hover(&mut self) {
        let (x, y) = (to_f32(self.mouse.x), to_f32(self.mouse.y));
        let on_columns = self
            .layout()
            .is_some_and(|l| self.columns_scrollbar_hit(&l, x, y).is_some());
        let view = &mut self.tabs[self.active].view;
        let hover = self.clicks.scrollbar_grab.is_some()
            || on_columns
            || view.scrollbar_hit(x, y).is_some();
        if hover != view.scrollbar_hover {
            view.scrollbar_hover = hover;
            self.changed(false);
        }
    }

    /// Show or hide the search toggles' tooltip as the pointer moves.
    fn update_search_hover(&mut self) {
        let Some(layout) = self.window_layout() else {
            return;
        };
        let (x, y) = (to_f32(self.mouse.x), to_f32(self.mouse.y));
        let changed = if self.sidebar.visible && self.sidebar.mode == Mode::Search {
            self.sidebar.search.hover_at(&layout, x, y, Instant::now())
        } else {
            self.sidebar.search.hover_gone()
        };
        if changed {
            self.changed(false);
        }
    }

    /// Show the resize cursor over the sidebar's divider, the usual one
    /// elsewhere. Only tells the window when it changes.
    fn update_cursor_icon(&mut self) {
        let Some(layout) = self.window_layout() else {
            return;
        };
        let x = to_f32(self.mouse.x);
        let resize = self.sidebar.resizing
            || self.groups.dragging.is_some()
            || self.sidebar.on_divider(&layout, x)
            || (self.groups.len() > 1
                && Groups::on_divider(&self.group_columns(&layout), x, layout.scale).is_some());
        if resize != self.resize_cursor {
            self.resize_cursor = resize;
            if let Some(window) = &self.window {
                window.set_cursor(if resize {
                    CursorIcon::ColResize
                } else {
                    CursorIcon::Default
                });
            }
        }
    }

    /// The mouse moved with the button held: extend the selection by the
    /// unit of the click that started it (character, word or line).
    fn mouse_drag(&mut self) {
        let Some(layout) = self.layout() else { return };
        let (x, y) = (to_f32(self.mouse.x), to_f32(self.mouse.y));
        let tab_width = self.tab_width();
        let tab = &mut self.tabs[self.active];
        if tab.compare.is_some() || tab.merge.is_some() {
            return;
        }
        if let Some(from) = self.clicks.column {
            let to = tab
                .view
                .hit_column(x, y, &self.docs[tab.doc], &layout, tab_width);
            Editor::new(&mut self.docs[tab.doc], &mut tab.carets)
                .column_select(from, to, tab_width);
            self.changed(true);
            return;
        }
        let pos = tab.view.hit(x, y, &self.docs[tab.doc], &layout, tab_width);
        let t = &self.docs[tab.doc].buffer;
        let unit = match self.clicks.count {
            1 => pos..pos,
            2 => nimbus_core::movement::word_at(t, pos),
            _ => nimbus_core::movement::line_range(t, pos),
        };
        let anchor = self.clicks.anchor.clone();
        let (from, to) = if unit.start < anchor.start {
            (anchor.end, unit.start)
        } else {
            (anchor.start, unit.end.max(anchor.end))
        };
        if self.clicks.adding {
            // Alt+drag: the added selection grows; the others stay.
            Editor::new(&mut self.docs[tab.doc], &mut tab.carets).drag_added(from, to);
        } else {
            Editor::new(&mut self.docs[tab.doc], &mut tab.carets).select_span(from, to);
        }
        self.changed(true);
    }

    /// Labels for group `g`'s tab strip, and which of them is its active
    /// tab.
    fn tab_labels(&self, g: usize) -> (Vec<TabLabel>, usize) {
        let active = self.group_active(g);
        let mut at = 0;
        let labels = self
            .group_tabs(g)
            .into_iter()
            .enumerate()
            .map(|(n, i)| {
                if i == active {
                    at = n;
                }
                let d = &self.docs[self.tabs[i].doc];
                let name = d.display_name();
                let name = if d.deleted {
                    format!("{name} (deleted)")
                } else if d.conflict {
                    format!("{name} (changed on disk)")
                } else {
                    name
                };
                TabLabel {
                    name,
                    dirty: d.is_dirty(),
                }
            })
            .collect();
        (labels, at)
    }

    /// A click while the picker is open: choose a row, or close it.
    fn picker_click(&mut self, layout: &Layout, x: f32, y: f32) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let (rows, _) = picker.rows(&self.keymap);
        match view::picker_row_at(layout, rows.len(), x, y) {
            Some(row) => {
                picker.selected = picker.scroll + row;
                let chosen = picker.chosen();
                let kind = picker.kind;
                self.picker = None;
                match chosen {
                    Some(Item::Name(name)) => self.name_chosen(kind, &name),
                    Some(Item::Workspace(name)) => self.open_named_workspace(&name),
                    Some(Item::File(path) | Item::Recent(path)) => self.open_path(&path),
                    Some(Item::Action(a)) => self.pending_action = Some(a.command),
                    Some(Item::Line(n)) => self.go_to_line(n),
                    Some(Item::Symbol(s, _)) => self.go_to_byte(s.start),
                    Some(Item::Tab(i, _)) => self.compare_with_tab_index(i),
                    Some(Item::Revision(rev)) => self.compare_with_revision(rev),
                    Some(Item::Encoding(purpose, choice, _)) => {
                        self.encoding_chosen(purpose, choice);
                    }
                    None => {}
                }
            }
            None => {
                if !view::picker_contains(layout, rows.len(), x, y) {
                    self.picker = None;
                }
            }
        }
        self.changed(false);
    }

    /// Start the active tab's first parse if it has not started, and say
    /// once if its file is too large to highlight.
    fn start_syntax(&mut self) {
        let tab = &mut self.tabs[self.active];
        let d = &mut self.docs[tab.doc];
        if let Some(job) = d.syntax.take_job(&d.buffer) {
            spawn_syntax(&self.proxy, job);
        }
        if self.docs[tab.doc].syntax.lexical().is_some() && !tab.noted_size {
            tab.noted_size = true;
            let mb = MAX_HIGHLIGHT_BYTES / (1024 * 1024);
            self.say(format!("Over {mb} MB: simpler syntax colours"), false);
        }
    }

    /// Bring tab `i`'s syntax tree, compare or merge up to date before it
    /// is drawn, and snap its cursors to a text swapped wholesale.
    fn prepare_tab(&mut self, i: usize) {
        let tab = &mut self.tabs[i];
        // Bring the tree up to date with this frame's edits, or hand the
        // rest to a worker if that would take too long.
        let d = &mut self.docs[tab.doc];
        if let Some(job) = d.syntax.refresh(&d.buffer, SYNTAX_BUDGET) {
            spawn_syntax(&self.proxy, job);
        }
        if let Some(c) = tab.compare.as_mut() {
            c.sync(&mut self.docs[tab.doc]);
            if let Some(job) = c.other.syntax.refresh(&c.other.buffer, SYNTAX_BUDGET) {
                spawn_syntax(&self.proxy, job);
            }
        }
        if let Some(m) = tab.merge.as_mut() {
            if m.sync(&mut self.docs[tab.doc]) {
                for doc in m.inputs_mut() {
                    if let Some(job) = doc.syntax.refresh(&doc.buffer, SYNTAX_BUDGET) {
                        spawn_syntax(&self.proxy, job);
                    }
                }
            } else {
                // The result was reloaded from disk: the regions no longer
                // describe it.
                if let Some(m) = tab.merge.take() {
                    m.finish(&mut self.docs[tab.doc]);
                }
                self.message = Some(Message {
                    text: "Merge ended: the file was reloaded".into(),
                    is_error: false,
                    until: Instant::now() + MESSAGE_TIME,
                });
            }
        }
        // A text swapped wholesale (a save, a merge result) leaves the
        // cursors to be snapped to it before they are drawn.
        tab.carets.fit(&self.docs[tab.doc]);
    }

    /// Queue the whole frame: each group's text, tab strip and find bar,
    /// the dividers between groups, the status bar, then the overlays.
    /// `panes` holds each group's layout, `layout` the window's.
    fn paint(&mut self, gpu: &mut Gpu, layout: &Layout, panes: &[Layout]) {
        let tab_width = self.tab_width();
        let chord_message = self.pending_chord.map(|(chord, until)| Message {
            text: format!("{} …", chord_label(chord)),
            is_error: false,
            until,
        });
        view::begin_frame(gpu, &self.theme.ui);
        let focused = self.groups.focused;
        let many = self.groups.len() > 1;
        let now = Instant::now();
        let mut hide_at: Option<Instant> = None;
        for (g, pane) in panes.iter().enumerate() {
            let i = self.group_active(g);
            self.prepare_tab(i);
            let (labels, active_label) = self.tab_labels(g);
            let is_focused = g == focused;
            let tab = &mut self.tabs[i];
            let find = if is_focused {
                &self.find
            } else {
                &self.groups.list[g].find
            };
            let mut scene = Scene {
                doc: &self.docs[tab.doc],
                carets: &tab.carets,
                view: &mut tab.view,
                compare: tab.compare.as_deref_mut(),
                merge: tab.merge.as_deref_mut(),
                theme: &mut self.theme,
                find,
                message: chord_message.as_ref().or(self.message.as_ref()),
                // Only the focused group's cursor blinks; another group
                // showing the same file shows its cursor still.
                cursor_visible: !is_focused
                    || (self.blink.visible && self.picker.is_none() && !self.sidebar.focused),
                preedit: if is_focused {
                    self.preedit.as_ref()
                } else {
                    None
                },
                tab_width,
                whitespace: self.whitespace,
                guides: self.indent_guides,
                sticky: self.sticky_scroll,
            };
            paint_pane(gpu, pane, &mut scene);
            hide_at = hide_at
                .into_iter()
                .chain(scene.view.scrollbar_hide_at().filter(|&t| t > now))
                .min();
            if is_focused {
                paint_status(gpu, layout, &mut scene);
            }
            // Nested languages the budget left unparsed colour on the
            // next frame.
            if self.docs[tab.doc].syntax.injections_pending()
                && let Some(window) = &self.window
            {
                window.request_redraw();
            }
            view::draw_tabs(
                gpu,
                &self.theme.ui,
                pane,
                &labels,
                active_label,
                is_focused || !many,
            );
        }
        self.scrollbar_hide_at = hide_at;
        if many {
            view::draw_dividers(gpu, &self.theme.ui, layout, panes);
        }
        self.sidebar.compare.options = compare_panel::Options {
            normalize: self.ws.state.diff_options(),
            unified: self.tabs[self.active].compare.as_ref().map(|c| c.unified),
        };
        let active = self.docs[self.tabs[self.active].doc].canonical.as_deref();
        sidebar::draw(
            gpu,
            &self.theme.ui,
            layout,
            &self.sidebar,
            active,
            self.blink.visible,
        );
        let facts = self.debug_facts();
        self.debug.draw(gpu, &self.theme.ui, layout, facts);
        if let Some(p) = &self.picker {
            let ui = &self.theme.ui;
            draw_picker_overlay(gpu, ui, layout, p, &self.keymap, self.blink.visible);
        }
    }

    /// Tell the IME where the cursor is, so its candidate window appears
    /// next to it. Only when it moved.
    fn tell_ime(&mut self, window: &Window, pane: &Layout) {
        let tab_width = self.config.tab_width;
        let r = self.tabs[self.active].view.cursor_rect(
            pane,
            self.preedit
                .as_ref()
                .map_or(0, |p| nimbus_core::column::line_width(&p.text, tab_width)),
        );
        #[expect(
            clippy::cast_possible_truncation,
            reason = "on-screen pixel coordinates fit in i32"
        )]
        let area = [r[0] as i32, r[1] as i32, r[2] as i32, r[3] as i32];
        if self.ime_area != Some(area) {
            window.set_ime_cursor_area(
                PhysicalPosition::new(area[0], area[1]),
                PhysicalSize::new(area[2].max(1).unsigned_abs(), area[3].max(1).unsigned_abs()),
            );
            self.ime_area = Some(area);
        }
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let frame_started = Instant::now();
        // The front tab is always read: a closed tab can bring a
        // placeholder to the front.
        while !self.ensure_loaded(self.active) {}
        // The first parse runs on a worker while this frame shows the
        // text plain; colours follow when it finishes. Before the layout,
        // which leaves room for any message this adds.
        self.start_syntax();
        if self.frames == 0 && self.sidebar.visible {
            // Listing runs on a worker; this only starts it.
            self.show_sidebar();
        }
        // Edits another group made to a document shown here too.
        self.sync_panes();
        let Some(layout) = self.window_layout() else {
            return;
        };
        let panes: Vec<Layout> = (0..self.groups.len())
            .filter_map(|g| self.group_layout(g))
            .collect();
        // Painting reads the whole app; take the GPU out while it does.
        if let Some(mut gpu) = self.gpu.take() {
            self.paint(&mut gpu, &layout, &panes);
            self.gpu = Some(gpu);
        }
        let focused_pane = panes.get(self.groups.focused).copied().unwrap_or(layout);
        // Picker results arriving later wake the loop through the
        // matcher's notify callback (AppEvent::PickerChanged), not by
        // redrawing continuously.
        if let Some(p) = self.picker.as_mut() {
            p.tick();
        }
        let (Some(window), Some(gpu)) = (self.window.clone(), self.gpu.as_mut()) else {
            return;
        };
        if self.frames == 0 && self.options.screenshot.is_some() && !gpu.request_capture() {
            print_err("nimbus: this surface cannot be captured");
            self.exit_code = 1;
            event_loop.exit();
            return;
        }
        match gpu.render() {
            Ok(FrameStatus::Presented) => {
                self.frames += 1;
                self.hidden_retry = None;
            }
            Ok(FrameStatus::Incomplete) => {
                self.frames += 1;
                window.request_redraw();
            }
            Ok(FrameStatus::Retry) => {
                log::debug!("frame skipped; retrying");
                window.request_redraw();
                return;
            }
            Ok(FrameStatus::Hidden) => {
                // Drawn again on Occluded(false) or Resized, or by the
                // backing-off retry, in case that event never comes.
                // It backs off and gives up after about 4 s, so a window
                // that really is hidden goes back to zero CPU.
                let next = match self.hidden_retry {
                    None => Some(Duration::from_millis(100)),
                    Some((_, d)) if d < Duration::from_secs(2) => Some(d * 2),
                    Some(_) => None,
                };
                self.hidden_retry = next.map(|d| (Instant::now() + d, d));
                log::debug!("frame skipped; window hidden, retry in {next:?}");
                return;
            }
            Err(e) => {
                log::error!("{e}");
                event_loop.exit();
                return;
            }
        }
        self.debug.latency.presented();
        self.debug.frames.push(frame_started.elapsed());
        if let Some(at) = self.zoom_started.take() {
            // The first frame at a new size rasterizes every glyph again.
            log::info!(
                "zoom {} pt: refilled and drawn in {:?} (frame {:?})",
                self.font_size,
                at.elapsed(),
                frame_started.elapsed()
            );
        }

        self.tell_ime(&window, &focused_pane);

        if self.options.bench_typing > 0 && self.frames >= 2 {
            self.bench_step(event_loop);
            return;
        }

        if self.frames == 1 {
            if let Some(path) = &self.options.screenshot {
                let capture = self.gpu.as_mut().and_then(Gpu::take_capture);
                if !save_screenshot(path, capture.as_ref()) {
                    self.exit_code = 1;
                }
                event_loop.exit();
                return;
            }
            let elapsed = self.started.elapsed();
            log::info!("first frame in {elapsed:?}");
            if self.options.smoke {
                print_out(&format!(
                    "first-frame-ms={:.1}",
                    elapsed.as_secs_f64() * 1000.0
                ));
                event_loop.exit();
            }
        }
    }

    /// One step of `--bench-typing`: type a character, or report and exit.
    fn bench_step(&mut self, event_loop: &ActiveEventLoop) {
        let samples = self.debug.latency.samples.get_or_insert_with(Vec::new);
        if samples.len() >= self.options.bench_typing {
            let mut s = std::mem::take(samples);
            s.sort();
            let pick = |q: usize| s.get((s.len() * q / 100).min(s.len() - 1)).copied();
            print_out(&format!(
                "typing latency over {} keystrokes: p50 {:?}, p99 {:?}, max {:?}",
                s.len(),
                pick(50).unwrap_or_default(),
                pick(99).unwrap_or_default(),
                s.last().copied().unwrap_or_default()
            ));
            event_loop.exit();
            return;
        }
        let ch = ["a", "b", "c", " ", "x", "y"][samples.len() % 6];
        self.debug.latency.input(Instant::now());
        self.run(Command::Insert(ch), event_loop);
    }

    fn ime(&mut self, ime: Ime, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        self.blink.reset(now);
        match ime {
            Ime::Enabled => {}
            Ime::Preedit(text, cursor) => {
                self.preedit = (!text.is_empty()).then_some(Preedit { text, cursor });
                self.debug.latency.input(now);
                self.changed(false);
            }
            Ime::Commit(text) => {
                self.preedit = None;
                self.debug.latency.input(now);
                // Routed like typing: into the find bar if it has focus.
                self.run(Command::Insert(&text), event_loop);
            }
            Ime::Disabled => {
                self.preedit = None;
                self.changed(false);
            }
        }
    }
}

/// What a Replace in Files did so far, for the summary once the files on
/// disk are written too.
#[derive(Debug, Default)]
struct ReplaceSummary {
    /// Replacements made in open tabs, and the tabs changed.
    tab_replaced: usize,
    tab_files: usize,
    /// Files whose ticked text had changed since the search.
    changed: usize,
    /// Tabs busy saving, left alone.
    busy: usize,
    /// Tabs in another encoding than UTF-8 or read-only, left alone: the
    /// search read the file's bytes, whose offsets are not the tab's.
    encoded: usize,
}

impl App {
    /// The find-in-files commands, from wherever they come.
    fn run_search_command(&mut self, cmd: Command<'_>) {
        match cmd {
            Command::FindInFiles | Command::ReplaceInFiles => {
                self.open_search_panel(cmd == Command::ReplaceInFiles);
            }
            Command::NextResult => self.step_result(false),
            Command::PrevResult => self.step_result(true),
            Command::SearchToggleCase | Command::SearchToggleRegex | Command::SearchToggleWord => {
                let which = match cmd {
                    Command::SearchToggleCase => crate::search_panel::Toggle::Case,
                    Command::SearchToggleRegex => crate::search_panel::Toggle::Regex,
                    _ => crate::search_panel::Toggle::Word,
                };
                if self.sidebar.search.toggle(which) {
                    self.search_files();
                }
            }
            Command::ReplaceInFilesAll => self.replace_in_files(),
            Command::UndoReplaceInFiles => self.undo_replace_in_files(),
            Command::ClearSearch => {
                self.sidebar.search.clear();
                self.search_files_at = None;
            }
            Command::SaveSearch => {
                if self.sidebar.search.query().pattern.is_empty() {
                    self.say("Type a search in the Search panel first", false);
                } else {
                    self.open_picker(Kind::Name(crate::picker::Naming::Search));
                }
            }
            _ => {}
        }
    }

    /// Show the Search panel with the keyboard in its query, which starts
    /// as the editor's selection when that is one short line.
    fn open_search_panel(&mut self, replace: bool) {
        self.find.focused = false;
        self.sidebar.visible = true;
        self.sidebar.mode = Mode::Search;
        self.sidebar.focused = true;
        let selection = self.carets().selected_text(&self.doc().buffer);
        let panel = &mut self.sidebar.search;
        let before = panel.query.doc.version;
        panel.focus_query(selection);
        if replace {
            panel.replace_mode = true;
        }
        if panel.query.doc.version != before {
            self.search_files_at = Some(Instant::now());
        }
        self.show_sidebar();
        self.remember();
    }

    /// A command while the Search panel has the keyboard. Fields take
    /// typing and movement; the list takes movement, Enter and Space;
    /// Tab moves between them; Escape gives the editor the keyboard.
    /// Returns `false` for commands that run as usual.
    fn run_search_panel(&mut self, cmd: Command<'_>) -> bool {
        let Some(layout) = self.window_layout() else {
            return false;
        };
        let in_list = self.sidebar.search.focus == Focus::List;
        match cmd {
            Command::Escape => self.sidebar.focused = false,
            Command::Tab | Command::Outdent => {
                self.sidebar.search.cycle_focus(cmd == Command::Outdent);
            }
            Command::Newline if in_list => {
                if let Some(hit) = self.sidebar.search.selected_hit() {
                    self.open_result(hit, true);
                } else if let Some(i) = self.sidebar.search.selected_saved()
                    && self.sidebar.search.load_saved(i)
                {
                    self.search_files();
                }
            }
            Command::Newline => {
                self.search_files();
                self.note_search_history(false);
            }
            Command::Move(motion, _) if in_list => {
                self.sidebar.search.move_selection(motion, &layout);
            }
            Command::DeleteForward | Command::DeleteBack if in_list => {
                if let Some(i) = self.sidebar.search.selected_saved() {
                    self.sidebar.search.forget_saved(i);
                    self.session_changed();
                }
            }
            // Up and Down in the query or replacement walk its history.
            Command::Move(m @ (Motion::Up | Motion::Down), false)
                if matches!(self.sidebar.search.focus, Focus::Query | Focus::Replace) =>
            {
                let panel = &mut self.sidebar.search;
                if panel.history_step(m == Motion::Up) && panel.focus == Focus::Query {
                    self.search_files_at = Some(Instant::now() + SEARCH_FILES_DELAY);
                }
            }
            Command::Insert(" ") if in_list => {
                if let Some(row) = self.sidebar.search.selected {
                    self.sidebar.search.toggle_tick(row);
                }
            }
            Command::Find
            | Command::Replace
            | Command::Save
            | Command::SaveAs
            | Command::Close
            | Command::CloseTab
            | Command::ToggleWrap
            | Command::ToggleTheme
            | Command::ToggleSidebar
            | Command::GoToLine
            | Command::FocusSidebar
            | Command::ToggleIgnored
            | Command::ToggleDebugOverlay
            | Command::ReloadFromDisk
            | Command::ReopenWithEncoding
            | Command::SaveWithEncoding
            | Command::ChangeLineEndings(_)
            | Command::KeepMine
            | Command::OpenFile
            | Command::NewFile
            | Command::NextTab
            | Command::PrevTab
            | Command::GoToTab(_)
            | Command::QuickOpen
            | Command::Palette
            | Command::ReloadConfig
            | Command::OpenConfig
            | Command::CompareSaved
            | Command::CompareFile
            | Command::CompareTab
            | Command::DiffNext
            | Command::DiffPrev
            | Command::DiffUnified
            | Command::DiffClose
            | Command::MergeFiles
            | Command::MergeWithBase
            | Command::TakeLeft
            | Command::TakeRight
            | Command::TakeBoth
            | Command::TakeBothReversed
            | Command::NextConflict
            | Command::PrevConflict
            | Command::ResolveMarkers
            | Command::MergeWithDisk => return false,
            _ if cmd.is_find_in_files() || cmd.is_layout() || cmd.is_workspace() => return false,
            _ if in_list => {}
            _ => self.edit_search_field(cmd),
        }
        true
    }

    /// An editing key in one of the Search panel's fields: edit it, and
    /// search again once typing pauses (not for the replacement).
    fn edit_search_field(&mut self, cmd: Command<'_>) {
        let paste = (cmd == Command::Paste)
            .then(|| self.clipboard_get())
            .flatten();
        let tab_width = self.tab_width();
        let panel = &mut self.sidebar.search;
        let searches = panel.focus != Focus::Replace;
        let Some(field) = panel.field_mut() else {
            return;
        };
        let before = field.doc.version;
        let copied = edit_field(field, cmd, paste.as_deref(), tab_width);
        let changed = field.doc.version != before;
        if let Some(text) = copied {
            self.clipboard_set(text);
        }
        if changed {
            self.sidebar.search.typed();
        }
        if changed && searches {
            self.search_files_at = Some(Instant::now() + SEARCH_FILES_DELAY);
        }
    }

    /// Do what a click in the Search panel asked for.
    fn act_on_search_reply(&mut self, reply: &Reply) {
        if reply.search {
            self.search_files();
        }
        if reply.replace_all {
            self.replace_in_files();
        }
        if let Some(hit) = reply.open {
            self.open_result(hit, false);
        }
        if reply.saved {
            self.search_files();
        }
    }

    /// Pin the Search panel's search as `name`, at the top of its list.
    fn save_search(&mut self, name: &str) {
        let panel = &mut self.sidebar.search;
        let saved = panel.to_saved(name);
        panel.history.save(saved);
        panel.rebuild_rows();
        self.session_changed();
        self.say(format!("Saved the search as {name}"), false);
    }

    /// Note the Search panel's query (and replacement) in its history, as
    /// one the user meant rather than a prefix typed on the way.
    fn note_search_history(&mut self, replaced: bool) {
        self.sidebar.search.note_history(replaced);
        self.session_changed();
    }

    /// Snapshots of the open tabs with unsaved edits, for the search to
    /// read instead of their files.
    fn snapshots(&self) -> find_files::Snapshots {
        let mut map = find_files::Snapshots::new();
        for tab in &self.tabs {
            if let Some(path) = &self.docs[tab.doc].canonical
                && self.docs[tab.doc].is_dirty()
            {
                map.insert(
                    path.clone(),
                    Arc::from(self.docs[tab.doc].buffer.snapshot()),
                );
            }
        }
        map
    }

    /// Search the project for the Search panel's query, cancelling any
    /// search under way.
    fn search_files(&mut self) {
        self.search_files_at = None;
        let roots = self.sidebar.tree.roots();
        let params = self
            .sidebar
            .search
            .params(&roots, self.sidebar.tree.show_ignored);
        let snapshots = self.snapshots();
        let proxy = self.proxy.clone();
        self.sidebar.search.start(params, snapshots, move |b| {
            proxy.send_event(AppEvent::FindFiles(b)).is_ok()
        });
    }

    /// Open the file of hit `(file, hit)` and select the match. The byte
    /// range is from the file as searched; if the text there no longer
    /// matches (the tab was edited since), the line is searched again and
    /// its first match selected, or its start when none is left. With
    /// `focus_editor`, the keyboard goes to the text.
    fn open_result(&mut self, (file, hit): (usize, usize), focus_editor: bool) {
        if self.sidebar.search.results.is_some() {
            self.note_search_history(false);
        }
        let Some(results) = &self.sidebar.search.results else {
            return;
        };
        let Some((path, hit)) = results
            .files
            .get(file)
            .and_then(|f| Some((f.path.clone(), f.hits.get(hit)?)))
        else {
            return;
        };
        let (line, range) = (hit.line, hit.range.clone());
        let re = self
            .sidebar
            .search
            .params
            .as_ref()
            .and_then(|p| p.query.compile().ok().flatten());
        let keep_focus = self.sidebar.focused && !focus_editor;
        self.open_path(&path);
        self.sidebar.focused = keep_focus;
        if self.doc().canonical.as_deref() != Some(io::canonical(&path).as_path()) {
            return; // it could not be opened; the message says so
        }
        let doc = self.doc();
        let t = &doc.buffer;
        let select = re.as_ref().and_then(|re| {
            // Still a match at the same place?
            if range.end <= t.len_bytes() && t.is_char_boundary(range.start) {
                let span = t.line_span(range.start);
                let text = t.slice(span.start..span.end);
                if let Some(m) = re
                    .find_iter(text.as_bytes())
                    .find(|m| span.start + m.start() == range.start)
                {
                    return Some(span.start + m.start()..span.start + m.end());
                }
            }
            // The line, then: its first match, or its start.
            (line < t.len_lines()).then(|| {
                let span = t.line_span(t.line_to_byte(line));
                let text = t.slice(span.start..span.end);
                re.find(text.as_bytes())
                    .map_or(span.start..span.start, |m| {
                        span.start + m.start()..span.start + m.end()
                    })
            })
        });
        let select = select.unwrap_or_else(|| {
            let at = nimbus_core::movement::clamp(t, range.start);
            at..at
        });
        self.ed().select(select);
        self.changed(true);
    }

    /// F4 and Shift+F4: open the next or previous hit across files.
    fn step_result(&mut self, backwards: bool) {
        let Some(layout) = self.window_layout() else {
            return;
        };
        self.sidebar.visible = true;
        self.sidebar.mode = Mode::Search;
        match self.sidebar.search.step(backwards, &layout) {
            Some(hit) => self.open_result(hit, true),
            None => self.say("No search results", false),
        }
    }

    /// Replace every ticked hit: open tabs as one undoable edit each, the
    /// rest on a worker through the save path. The summary follows once
    /// the worker is done.
    fn replace_in_files(&mut self) {
        let panel = &mut self.sidebar.search;
        if panel.replacing {
            self.say("Still replacing; wait for it to finish", true);
            return;
        }
        let Some(results) = &panel.results else {
            self.say("Nothing to replace: search first", true);
            return;
        };
        if !results.done {
            self.say(
                "Still searching; Replace All once the search is complete",
                true,
            );
            return;
        }
        if results.capped {
            self.say(
                "Too many matches to replace in files; narrow the search",
                true,
            );
            return;
        }
        let ticked = results.ticked();
        if ticked.is_empty() {
            self.say("Nothing ticked to replace", true);
            return;
        }

        let Some(replacement) = replacement_of(panel) else {
            return;
        };
        let mut summary = ReplaceSummary::default();
        let mut undo = ReplaceUndo::default();
        let mut jobs = Vec::new();
        for (fi, hits) in ticked {
            let file = &results.files[fi];
            let canonical = io::canonical(&file.path);
            let tab = self
                .tabs
                .iter()
                .position(|t| self.docs[t.doc].canonical.as_deref() == Some(canonical.as_path()));
            let Some(ti) = tab else {
                jobs.push(replace_files::Job {
                    path: file.path.clone(),
                    ranges: hits.iter().map(|&j| file.hits[j].range.clone()).collect(),
                });
                continue;
            };
            let ranges: Vec<std::ops::Range<usize>> =
                hits.iter().map(|&j| file.hits[j].range.clone()).collect();
            let tab = &mut self.tabs[ti];
            if self.docs[tab.doc].is_saving() {
                summary.busy += 1;
                continue;
            }
            if !self.docs[tab.doc].encoding.is_utf8() || self.docs[tab.doc].read_only.is_some() {
                summary.encoded += 1;
                continue;
            }
            let mut ed = Editor::new(&mut self.docs[tab.doc], &mut tab.carets);
            match replace_in_tab(&mut ed, &replacement, &ranges) {
                Some(0) => summary.changed += 1,
                Some(n) => {
                    summary.tab_replaced += n;
                    summary.tab_files += 1;
                    undo.tabs.push((canonical, ed.doc.history.top_id()));
                }
                None => log::error!("replace in {} refused", file.rel),
            }
        }
        self.replace_undo = Some(undo);
        self.replace_summary = summary;
        if jobs.is_empty() {
            self.replaced(replace_files::Report::default());
            return;
        }
        let proxy = self.proxy.clone();
        let spawned = replace_files::spawn(replacement, jobs, move |report| {
            let _ = proxy.send_event(AppEvent::Replaced(report));
        });
        match spawned {
            Ok(()) => self.sidebar.search.replacing = true,
            Err(e) => {
                log::error!("could not start the replace thread: {e}");
                self.say(
                    "Replace in Files failed to start; files on disk are unchanged",
                    true,
                );
            }
        }
    }

    /// The files on disk are written: finish the summary and search again
    /// so the results show the text as it is now.
    fn replaced(&mut self, report: replace_files::Report) {
        self.sidebar.search.replacing = false;
        self.note_search_history(true);
        let summary = std::mem::take(&mut self.replace_summary);
        let (disk_replaced, disk_files) = report.totals();
        let plural =
            |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let names = |paths: &[PathBuf]| {
            paths
                .iter()
                .map(|p| {
                    p.file_name().map_or_else(
                        || p.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let replaced = summary.tab_replaced + disk_replaced;
        let files = summary.tab_files + disk_files;
        let mut text = format!(
            "Replaced {} in {}",
            plural(replaced, "match", "matches"),
            plural(files, "file", "files")
        );
        if summary.tab_files > 0 {
            let _ = write!(text, " ({} open)", plural(summary.tab_files, "tab", "tabs"));
        }
        let changed = summary.changed
            + report
                .files
                .iter()
                .filter(|(_, o)| *o == replace_files::Outcome::Changed)
                .count();
        if changed > 0 {
            let _ = write!(
                text,
                "; {} changed since the search, left alone",
                plural(changed, "file", "files")
            );
        }
        if summary.busy > 0 {
            let _ = write!(
                text,
                "; {} still saving, left alone",
                plural(summary.busy, "tab", "tabs")
            );
        }
        if summary.encoded > 0 {
            let _ = write!(
                text,
                "; {} not in UTF-8, left alone",
                plural(summary.encoded, "tab", "tabs")
            );
        }
        let failed: Vec<PathBuf> = report
            .files
            .iter()
            .filter(|(_, o)| matches!(o, replace_files::Outcome::Failed(_)))
            .map(|(p, _)| p.clone())
            .collect();
        for (path, outcome) in &report.files {
            if let replace_files::Outcome::Failed(why) = outcome {
                log::error!("replace in files: {}: {why}", path.display());
            }
        }
        if !failed.is_empty() {
            let _ = write!(text, "; failed: {}", names(&failed));
        }
        if !report.unkept.is_empty() {
            let _ = write!(text, "; undo not available for {}", names(&report.unkept));
        }
        let is_error = !failed.is_empty();
        if let Some(undo) = &mut self.replace_undo {
            undo.saved = report.saved;
            if undo.saved.is_empty() && undo.tabs.is_empty() {
                self.replace_undo = None;
            }
        }
        self.say(text, is_error);
        self.search_files();
    }

    /// Put back what the last Replace in Files wrote: tabs whose replace
    /// is still their last edit undo it; files on disk get their old
    /// contents if they have not changed since.
    fn undo_replace_in_files(&mut self) {
        let Some(undo) = self.replace_undo.take() else {
            self.say("Nothing to undo: no Replace in Files since the start", true);
            return;
        };
        let mut restored = 0;
        let mut problems = Vec::new();
        for (path, top) in &undo.tabs {
            let Some(tab) = self
                .tabs
                .iter_mut()
                .find(|t| self.docs[t.doc].canonical.as_deref() == Some(path.as_path()))
            else {
                problems.push(format!("{} (tab closed)", name_of(path)));
                continue;
            };
            if self.docs[tab.doc].history.top_id() != *top || self.docs[tab.doc].is_saving() {
                problems.push(format!("{} (edited since)", name_of(path)));
                continue;
            }
            match Editor::new(&mut self.docs[tab.doc], &mut tab.carets).undo() {
                Ok(true) => restored += 1,
                Ok(false) => problems.push(name_of(path)),
                Err(e) => {
                    log::error!("undo refused: {e}");
                    problems.push(name_of(path));
                }
            }
        }
        let (files, failed) = replace_files::undo(&undo.saved);
        restored += files.len();
        for (path, why) in failed {
            problems.push(format!("{} ({why})", name_of(&path)));
        }
        let mut text = format!(
            "Restored {} file{}",
            restored,
            if restored == 1 { "" } else { "s" }
        );
        if !problems.is_empty() {
            let _ = write!(text, "; could not restore {}", problems.join(", "));
        }
        self.say(text, !problems.is_empty());
        self.search_files();
    }
}

/// Replace `ranges` in an open tab's text as one undo step, each only
/// where the text still matches. Returns how many were replaced, or `None`
/// if the buffer refused the edit (nothing is changed then).
fn replace_in_tab(
    ed: &mut Editor<'_>,
    replacement: &Replacement,
    ranges: &[std::ops::Range<usize>],
) -> Option<usize> {
    let t = &ed.doc.buffer;
    let items: Vec<(std::ops::Range<usize>, String)> = ranges
        .iter()
        .filter_map(|range| {
            if !t.is_char_boundary(range.start) || !t.is_char_boundary(range.end) {
                return None;
            }
            let text = replacement.at_in(t.len_bytes(), range.clone(), |r| {
                let mut v = Vec::new();
                t.read_into(r, &mut v).ok().map(|()| v)
            })?;
            Some((range.clone(), String::from_utf8_lossy(&text).into_owned()))
        })
        .collect();
    if items.is_empty() {
        return Some(0);
    }
    let n = ed.replace_many(&items).ok()?;
    // Typing after must start a new undo step, so undoing the replace
    // later puts back exactly the replace.
    ed.doc.history.seal();
    Some(n)
}

/// A path's file name, for messages.
fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The number of lines, or while a huge file is still being counted, an
/// estimate from the part counted so far (a little high), so the gutter is
/// wide enough from the start instead of widening as counting goes on.
fn line_estimate(t: &dyn Text) -> usize {
    let (counted, len) = (t.indexed_bytes(), t.len_bytes());
    let lines = t.len_lines();
    if counted >= len || counted == 0 {
        return lines;
    }
    let before = t.line_number(counted).unwrap_or(lines);
    let estimate =
        u128::from(before as u64) * u128::from(len as u64) * 11 / (u128::from(counted as u64) * 10);
    usize::try_from(estimate).unwrap_or(usize::MAX).max(lines)
}

/// The tab labels for headless rendering: the given ones, or the one
/// document's own.
fn labels_or_own(labels: Vec<TabLabel>, doc: &Document) -> Vec<TabLabel> {
    if labels.is_empty() {
        vec![TabLabel {
            name: doc.display_name(),
            dirty: false,
        }]
    } else {
        labels
    }
}

/// The document's definitions with their line numbers, for Go to Symbol.
fn symbol_items(doc: &Document) -> Vec<(nimbus_syntax::symbols::Symbol, Option<usize>)> {
    let t = &doc.buffer;
    nimbus_syntax::symbols::symbols(&doc.syntax, t)
        .into_iter()
        .map(|s| {
            let line = t.line_number(s.start).map(|l| l + 1);
            (s, line)
        })
        .collect()
}

/// `--picker KIND[:QUERY]` with `--headless`: open the palette, file
/// opener, go-to-line or symbol picker with `QUERY` typed, let it settle,
/// and draw it.
fn draw_headless_picker(
    gpu: &mut Gpu,
    ui: &Ui,
    layout: &Layout,
    (spec, root): (&str, &Path),
    doc: &Document,
    config: &Config,
) {
    // "palette" or "files", optionally followed by ":query".
    let (kind, query) = spec.split_once(':').unwrap_or((spec, ""));
    let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    let mut p = match kind {
        "files" => Picker::files(&[root.to_path_buf()], Vec::new(), notify),
        "recent" => {
            let paths = State::load().recent.into_iter().map(|r| r.path).collect();
            Picker::recent(paths, root, notify)
        }
        "line" => Picker::go_to_line(notify, Some(doc.buffer.len_lines())),
        "symbols" => Picker::symbols(notify, symbol_items(doc)),
        _ => Picker::palette(notify),
    };
    let _ = p.input.ed().insert(query);
    // Let the walk and matcher finish, up to two seconds.
    let deadline = Instant::now() + Duration::from_secs(2);
    while p.tick() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let (keymap, _) = Keymap::new(&config.keys);
    draw_picker_overlay(gpu, ui, layout, &p, &keymap, true);
}

/// Give the window the app icon where the window itself carries one.
///
/// On Windows the icon embedded in `nimbus.exe` (by `build.rs`, resource
/// 1 in `assets/icon/nimbus.rc`) shows in Explorer, but a window starts
/// with none: without this its title bar and Alt-Tab entry show a blank
/// generic icon. Windows picks the right size from the resource, so
/// nothing is decoded here. macOS takes the icon from `Nimbus.app`, and
/// Linux from the `.desktop` file, so they need nothing.
fn with_app_icon(attrs: winit::window::WindowAttributes) -> winit::window::WindowAttributes {
    #[cfg(target_os = "windows")]
    {
        use winit::platform::windows::{IconExtWindows, WindowAttributesExtWindows};
        use winit::window::Icon;
        match Icon::from_resource(1, None) {
            Ok(icon) => attrs
                .with_window_icon(Some(icon.clone()))
                .with_taskbar_icon(Some(icon)),
            // Built without the resource (build.rs warned): no icon.
            Err(e) => {
                log::debug!("app icon not loaded: {e}");
                attrs
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    attrs
}

/// What the debug overlay reports about `doc` and the work running for it.
fn debug_facts(doc: &Document, find: &FindBar) -> crate::debug::Facts {
    let t = &doc.buffer;
    crate::debug::Facts {
        backend: match (t.is_mapped(), doc.saves_on_worker()) {
            (false, _) => "rope",
            (true, false) => "bytes in memory",
            (true, true) => "mapped file",
        },
        len: t.len_bytes(),
        syntax: doc.syntax.phase_name(),
        searching: find.open && !find.complete,
        #[expect(clippy::cast_precision_loss, reason = "a percentage for display")]
        counting: doc
            .counting_lines()
            .then(|| t.indexed_bytes() as f64 * 100.0 / t.len_bytes().max(1) as f64),
        saving: doc.is_saving(),
    }
}

/// Geometry for the window `gpu` draws to, with room for the find bar when
/// it is shown, and the status bar.
pub fn layout_for(gpu: &Gpu, scale: f32, doc: &Document, find: &FindBar) -> Layout {
    layout_for_lines(gpu, scale, line_estimate(&doc.buffer), find)
}

/// As [`layout_for`], for a gutter wide enough for `lines` lines.
pub fn layout_for_lines(gpu: &Gpu, scale: f32, lines: usize, find: &FindBar) -> Layout {
    let (w, h) = gpu.size();
    let bar_rows = match (find.open, find.replace_mode) {
        (false, _) => 0,
        (true, false) => 1,
        (true, true) => 2,
    };
    Layout::new(gpu.metrics(), w, h, scale, lines)
        .with_chrome(true, bar_rows, true)
        .with_ui(gpu.ui_font())
}

/// Everything one frame shows.
pub struct Scene<'a> {
    doc: &'a Document,
    /// The cursors drawn in it.
    carets: &'a Carets,
    view: &'a mut View,
    /// A compare on the tab: two columns instead of the view alone.
    compare: Option<&'a mut Compare>,
    /// A merge on the tab: three columns, the result in the middle.
    merge: Option<&'a mut MergeView>,
    theme: &'a mut Theme,
    find: &'a FindBar,
    message: Option<&'a Message>,
    /// Blink phase: the cursor is in its visible half.
    cursor_visible: bool,
    preedit: Option<&'a Preedit>,
    tab_width: usize,
    /// Where whitespace shows.
    whitespace: Whitespace,
    /// Indent guides on.
    guides: bool,
    /// Sticky scroll on (M20).
    sticky: bool,
}

/// The config's tab width, ligatures and font for a headless render. With
/// no later frame to swap a font into, a configured font is looked up
/// before drawing.
fn headless_fonts(gpu: &mut Gpu, config: &Config) {
    gpu.set_tab_width(config.tab_width);
    gpu.set_ligatures(config.ligatures);
    if !config.font.is_empty() {
        match nimbus_render::find_editor_font(&config.font) {
            Some(f) => gpu.set_font(f),
            None => log::warn!("font {:?} not found; using the bundled font", config.font),
        }
    }
    if !config.ui_font.is_empty() {
        match nimbus_render::find_editor_font(&config.ui_font) {
            Some(f) => gpu.set_ui_font(Some(f.regular)),
            None => log::warn!(
                "interface font {:?} not found; using the editor font",
                config.ui_font
            ),
        }
    }
}

/// The GPU for a headless render at `scale` and `size`, with the
/// config's fonts; `None` after saying why the bundled font could not be
/// read.
fn headless_gpu(
    context: GpuContext,
    config: &Config,
    (scale, size): (f64, (u32, u32)),
    linear_blend: bool,
) -> Option<Gpu> {
    let font = Font::bundled()
        .map_err(|e| print_err(&format!("nimbus: {e}")))
        .ok()?;
    let mut gpu = Gpu::headless(
        context,
        font,
        to_f32(config.font_size * scale),
        size,
        linear_blend,
    );
    headless_fonts(&mut gpu, config);
    Some(gpu)
}

/// `layout` with room for the minimap when the config shows one and the
/// tab is a plain document (`plain`: no compare or merge columns).
fn headless_minimap(layout: Layout, config: &Config, scale: f64, plain: bool) -> Layout {
    if config.minimap && plain {
        layout.with_minimap(to_f32(config.minimap_width * scale))
    } else {
        layout
    }
}

/// Queue a whole frame of one tab: editor text, then the find bar and
/// status bar. Headless rendering uses it; the window paints each group
/// with [`paint_pane`] and the status bar once.
pub fn paint_scene(gpu: &mut Gpu, layout: &Layout, scene: &mut Scene<'_>) {
    view::begin_frame(gpu, &scene.theme.ui);
    paint_pane(gpu, layout, scene);
    paint_status(gpu, layout, scene);
}

/// Queue one group's text and find bar inside `layout`'s column. The
/// frame was started with [`view::begin_frame`].
pub fn paint_pane(gpu: &mut Gpu, layout: &Layout, scene: &mut Scene<'_>) {
    let find = scene.find;
    let current = find.current.and_then(|i| find.matches.get(i)).cloned();
    let ui = scene.theme.ui;
    let syntax = &scene.doc.syntax;
    // Hide highlights from before an edit until the search reruns.
    let matches: &[Range<usize>] = if find.open && find.version == scene.doc.version {
        &find.matches
    } else {
        &[]
    };
    if let Some(c) = scene.compare.as_deref_mut() {
        c.draw(
            gpu,
            layout,
            scene.doc,
            scene.carets,
            scene.view,
            scene.theme,
            scene.tab_width,
            scene.cursor_visible && !find.focused,
            scene.preedit,
            (matches, current),
        );
    } else if let Some(m) = scene.merge.as_deref_mut() {
        m.draw(
            gpu,
            layout,
            scene.doc,
            scene.carets,
            scene.view,
            scene.theme,
            scene.tab_width,
            scene.cursor_visible && !find.focused,
            scene.preedit,
            (matches, current),
        );
    } else {
        let looks = if syntax.capture_names().is_some() {
            scene.theme.resolve_tables();
            Some(&*scene.theme)
        } else {
            None
        };
        scene.view.draw(
            gpu,
            scene.doc,
            scene.carets,
            layout,
            scene.tab_width,
            &Decorations {
                cursor_visible: scene.cursor_visible && !find.focused,
                preedit: scene.preedit,
                matches,
                current,
                ui: &ui,
                looks,
                whitespace: scene.whitespace,
                guides: scene.guides.then(|| {
                    let indent = scene.doc.indent;
                    if indent.tabs {
                        scene.tab_width
                    } else {
                        indent.width
                    }
                }),
                sticky: scene.sticky,
            },
        );
        // A nested language that came into view loads its grammar or was
        // parsed this frame: its table may be new to the theme, and its
        // spans may be waiting on the next frame's budget.
        scene.theme.resolve_tables();
        if layout.minimap > 0.0 {
            let looks = syntax.capture_names().is_some().then_some(&*scene.theme);
            scene
                .view
                .draw_minimap(gpu, scene.doc, layout, (&ui, looks), scene.tab_width);
        }
        scene.view.draw_scrollbar(gpu, scene.doc, layout, &ui);
    }
    if find.open {
        view::draw_find_bar(gpu, &ui, find, layout, scene.cursor_visible);
    }
}

/// Queue the status bar across the bottom of the window, for the scene's
/// tab.
pub fn paint_status(gpu: &mut Gpu, layout: &Layout, scene: &mut Scene<'_>) {
    let ui = scene.theme.ui;
    let message = scene.message.map(|m| (m.text.as_str(), m.is_error));
    let info = scene
        .view
        .status_info(scene.doc, scene.carets, scene.tab_width);
    let info = match (scene.compare.as_deref_mut(), scene.merge.as_deref_mut()) {
        (Some(c), _) => c.status(info),
        (None, Some(m)) => m.status(scene.doc, scene.carets.cursor(&scene.doc.buffer), info),
        (None, None) => info,
    };
    view::draw_status(gpu, &ui, layout, message, info);
}

/// `--headless WxH --screenshot FILE`: render the first frame into an
/// off-screen texture, with no window, and save it. Works with the screen
/// locked or no display attached. Returns the process exit code.
pub fn run_headless(
    options: &Options,
    config: &Config,
    (docs, root, more_roots): (Vec<Document>, PathBuf, Vec<PathBuf>),
    size: (u32, u32),
    context: GpuContext,
) -> i32 {
    let labels = headless_labels(&docs);
    let mut doc = docs.into_iter().next().unwrap_or_else(Document::untitled);
    let mut compare = headless_compare(options, config, &mut doc);
    let mut merge = headless_merge(options, config, &mut doc);
    let Some(path) = &options.screenshot else {
        print_err("nimbus: --headless needs --screenshot FILE.bmp");
        return 2;
    };
    // No window to ask: default to a Retina scale, or NIMBUS_SCALE.
    let scale = effective_scale(2.0);
    let Some(mut gpu) = headless_gpu(context, config, (scale, size), options.linear_blend) else {
        return 1;
    };
    let mut view = View::default();
    view.set_wrap(options.wrap || wrap_for(config, &doc));
    if options.wait_index {
        index_now(&mut doc);
    }
    let mut carets = Carets::default();
    let find = headless_find(
        options.find.as_deref(),
        &mut Editor::new(&mut doc, &mut carets),
    );
    headless_cursors(options, &doc, &mut carets);
    let plain = compare.is_none() && merge.is_none();
    let (sidebar, layout) = headless_sidebar(
        (&root, more_roots),
        headless_minimap(
            layout_for(&gpu, to_f32(scale), &doc, &find),
            config,
            scale,
            plain,
        ),
        doc.canonical.as_deref(),
        options.search.as_deref(),
        folder_diff(options),
    );
    view.ensure_cursor_visible(&doc, &carets, &layout, config.tab_width);
    // One frame only: parse here rather than on a worker.
    doc.syntax.parse_now(&doc.buffer);
    print_warnings(nimbus_syntax::registry::take_warnings());
    let (mut theme, warnings) = Theme::load(&config.theme);
    print_warnings(warnings);
    let labels = labels_or_own(labels, &doc);
    // Characters the bundled font lacks are looked for in system fonts on
    // a worker; with no later frame to show them in, wait for them once
    // everything has been painted, and paint again if any arrived.
    let mut fonts_waited = false;
    for _ in 0..8 {
        paint_scene(
            &mut gpu,
            &layout,
            &mut Scene {
                doc: &doc,
                carets: &carets,
                view: &mut view,
                compare: compare.as_deref_mut(),
                merge: merge.as_deref_mut(),
                theme: &mut theme,
                find: &find,
                message: None,
                cursor_visible: true,
                preedit: None,
                tab_width: config.tab_width,
                whitespace: config.render_whitespace,
                guides: config.indent_guides,
                sticky: config.sticky_scroll,
            },
        );
        let ui = &theme.ui;
        view::draw_tabs(&mut gpu, ui, &layout, &labels, 0, true);
        sidebar::draw(
            &mut gpu,
            ui,
            &layout,
            &sidebar,
            doc.canonical.as_deref(),
            true,
        );
        if options.debug_overlay {
            Debug::new(true).draw(&mut gpu, ui, &layout, debug_facts(&doc, &find));
        }
        if let Some(spec) = &options.picker {
            draw_headless_picker(&mut gpu, ui, &layout, (spec, &root), &doc, config);
        }
        // Nested languages are parsed a few per frame; keep painting
        // until none is left over, as the window would on redraws.
        if doc.syntax.injections_pending() {
            continue;
        }
        if fonts_waited || !gpu.wait_for_fonts(Duration::from_secs(5)) {
            break;
        }
        fonts_waited = true;
    }
    gpu.request_capture();
    if let Err(e) = gpu.render() {
        print_err(&format!("nimbus: {e}"));
        return 1;
    }
    i32::from(!save_screenshot(path, gpu.take_capture().as_ref()))
}

/// Warnings on stderr, for headless runs with no message line to show them.
fn print_warnings(warnings: impl IntoIterator<Item = String>) {
    for w in warnings {
        print_err(&format!("nimbus: {w}"));
    }
}

/// Tab labels for the headless frame: one per document opened.
fn headless_labels(docs: &[Document]) -> Vec<TabLabel> {
    docs.iter()
        .map(|d| TabLabel {
            name: d.display_name(),
            dirty: false,
        })
        .collect()
}

/// `--cursors a,b,c`: several cursors at those byte offsets, the last
/// primary.
fn headless_cursors(options: &Options, doc: &Document, carets: &mut Carets) {
    if options.cursors.is_empty() {
        return;
    }
    let t = &doc.buffer;
    let sels = options
        .cursors
        .iter()
        .map(|&p| nimbus_core::Selection::cursor(nimbus_core::movement::clamp(t, p)))
        .collect();
    carets.selections = nimbus_core::Selections::from_vec(sels, options.cursors.len() - 1);
}

/// `--diff A B` headless: the compare, diffed on this thread.
fn headless_compare(
    options: &Options,
    config: &Config,
    doc: &mut Document,
) -> Option<Box<Compare>> {
    let (a, _) = options.diff.as_ref()?;
    if folder_diff(options).is_some() {
        return None;
    }
    match Document::open(a) {
        Ok(mut other) => {
            other.detect_language(&Languages::load(&config.languages).0);
            other.syntax.parse_now(&other.buffer);
            Some(Box::new(Compare::start_sync(
                doc,
                other,
                Other::File(a.clone()),
                State::load().diff_options(),
            )))
        }
        Err(e) => {
            print_err(&format!("nimbus: cannot read {}: {e}", a.display()));
            None
        }
    }
}

/// `--merge` headless: the merge tab's view, with `doc` replaced by the
/// result.
fn headless_merge(
    options: &Options,
    config: &Config,
    doc: &mut Document,
) -> Option<Box<MergeView>> {
    let args = options.merge.as_ref()?;
    let languages = Languages::load(&config.languages).0;
    match open_merge_inputs(args, &languages) {
        Ok(inputs) => match MergeView::three_way(inputs, Some(args.out.clone())) {
            Ok((mut merge, mut result)) => {
                result.detect_language(&languages);
                result.syntax.parse_now(&result.buffer);
                for d in merge.inputs_mut() {
                    d.syntax.parse_now(&d.buffer);
                }
                *doc = result;
                Some(Box::new(merge))
            }
            Err(e) => {
                print_err(&format!("nimbus: cannot merge: {e}"));
                None
            }
        },
        Err(e) => {
            print_err(&format!("nimbus: {e}"));
            None
        }
    }
}

/// The three documents of `--merge`.
fn open_merge_inputs(args: &MergeArgs, languages: &Languages) -> Result<Inputs, String> {
    let open = |p: &Path| -> Result<Document, String> {
        let mut d = Document::open(p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
        d.detect_language(languages);
        Ok(d)
    };
    Ok(Inputs {
        base: Some(open(&args.base)?),
        ours: open(&args.local)?,
        theirs: open(&args.remote)?,
        names: (short_name(&args.local), short_name(&args.remote)),
    })
}

/// A path's file name for the status line.
fn short_name(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// `--wait-index`: count all of a huge file's lines on this thread.
fn index_now(doc: &mut Document) {
    if let Some(job) = doc.indexing.take() {
        let started = Instant::now();
        job.run(|token, chunk, _| doc.merge_index(token, chunk));
        log::info!(
            "indexed {} lines in {:?}",
            doc.buffer.len_lines(),
            started.elapsed()
        );
    }
}

/// `--find PATTERN`: the find bar searching, with the first match selected.
fn headless_find(pattern: Option<&str>, ed: &mut Editor<'_>) -> FindBar {
    let mut find = FindBar::default();
    if let Some(pattern) = pattern {
        find.open(false, None);
        let _ = find.find.ed().insert(pattern);
        find.search_now(ed.doc);
        if let Some(range) = find.select_from(0, false) {
            ed.select(range);
        }
    }
    find
}

/// `--search PATTERN`: the Search panel showing the project searched to
/// the end, its first hit selected.
fn headless_search(sidebar: &mut Sidebar, pattern: &str, layout: &Layout) {
    sidebar.visible = true;
    sidebar.mode = Mode::Search;
    sidebar.focused = true;
    let panel = &mut sidebar.search;
    panel.focus_query(Some(pattern.to_owned()));
    let params = panel.params(&sidebar.tree.roots(), sidebar.tree.show_ignored);
    let (tx, rx) = std::sync::mpsc::channel();
    panel.start(params, find_files::Snapshots::new(), move |b| {
        tx.send(b).is_ok()
    });
    while panel.results.as_ref().is_some_and(|r| !r.done) {
        let Ok(batch) = rx.recv() else { break };
        panel.receive(batch);
    }
    panel.focus = Focus::List;
    panel.step(false, layout);
}

/// The sidebar as the window would show it, listed here in place, and the
/// layout with room for it; with `search`, showing the Search panel.
fn headless_sidebar(
    (root, more_roots): (&Path, Vec<PathBuf>),
    layout: Layout,
    file: Option<&Path>,
    search: Option<&str>,
    folders: Option<(&Path, &Path)>,
) -> (Sidebar, Layout) {
    let state = State::load();
    let mut sidebar = Sidebar::new(
        io::canonical(root),
        state.sidebar_visible || folders.is_some(),
        state.sidebar_width,
    );
    for r in more_roots {
        if let Err(e) = sidebar.tree.add_root(r) {
            print_err(&format!("nimbus: {e}"));
        }
    }
    // Every root shown open, so a screenshot shows what each holds.
    let roots = sidebar.tree.roots();
    sidebar.tree.expand_later(roots);
    let layout = layout.with_sidebar(sidebar.px(layout.scale));
    if sidebar.visible {
        let mut requests = sidebar.tree.start();
        if let Some(p) = file {
            requests.extend(sidebar.reveal(p, Some(&layout)));
        }
        while let Some(req) = requests.pop() {
            match tree::list_dir(&req.path, req.show_ignored) {
                Ok(entries) => requests.extend(sidebar.tree.loaded(&req, entries)),
                Err(_) => sidebar.tree.failed(&req),
            }
        }
        sidebar.after_change(Some(&layout));
    }
    if let Some(pattern) = search {
        headless_search(&mut sidebar, pattern, &layout);
    }
    if let Some((left, right)) = folders {
        headless_folder_compare(&mut sidebar, left, right);
    }
    (sidebar, layout)
}

/// `--diff DIR_A DIR_B`: the two folders, when both are folders (M18).
fn folder_diff(options: &Options) -> Option<(&Path, &Path)> {
    let (a, b) = options.diff.as_ref()?;
    (a.is_dir() && b.is_dir()).then_some((a.as_path(), b.as_path()))
}

/// The folder compare run to the end on this thread, for a headless frame.
fn headless_folder_compare(sidebar: &mut Sidebar, left: &Path, right: &Path) {
    let params = folder_compare::Params {
        left: io::canonical(left),
        right: io::canonical(right),
        ..folder_compare::Params::default()
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let generation = Arc::clone(&sidebar.compare.generation);
    match folder_compare::start(&params, &generation, move |b| tx.send(b).is_ok()) {
        Ok(g) => sidebar.compare.begin(g, &params.left, &params.right),
        Err(e) => {
            print_err(&format!("nimbus: {e}"));
            return;
        }
    }
    sidebar.mode = Mode::Compare;
    while sidebar.compare.results.as_ref().is_some_and(|r| !r.done) {
        let Ok(batch) = rx.recv() else { break };
        sidebar.compare.receive(batch);
    }
}

/// Draw an open picker over everything else.
fn draw_picker_overlay(
    gpu: &mut Gpu,
    ui: &Ui,
    layout: &Layout,
    p: &Picker,
    keymap: &Keymap,
    blink: bool,
) {
    let (rows, selected) = p.rows(keymap);
    let rows: Vec<(String, String)> = rows.into_iter().map(|r| (r.text, r.detail)).collect();
    let n = p.match_count();
    let noun = match (p.kind, n == 1) {
        (Kind::Palette, true) => "command",
        (Kind::Palette, false) => "commands",
        (Kind::Files, true) => "file",
        (Kind::Files, false) => "files",
        (Kind::Symbols, true) => "symbol",
        (Kind::Symbols, false) => "symbols",
        (Kind::Line | Kind::Name(_) | Kind::Revision, _) => "",
        (Kind::Workspaces, true) => "workspace",
        (Kind::Workspaces, false) => "workspaces",
        (Kind::Tabs, true) => "tab",
        (Kind::Tabs, false) => "tabs",
        (Kind::Encoding(_), true) => "encoding",
        (Kind::Encoding(_), false) => "encodings",
        (Kind::Recent, true) => "recent file",
        (Kind::Recent, false) => "recent files",
    };
    let more = if p.loading() { "…" } else { "" };
    let status = if matches!(p.kind, Kind::Line | Kind::Name(_) | Kind::Revision) {
        String::new()
    } else {
        format!("{n} {noun}{more}")
    };
    let placeholder = match p.kind {
        Kind::Palette => "Type a command",
        Kind::Files => "Type a file name",
        Kind::Line => "Type a line number",
        Kind::Symbols => "Type a function, type or heading name",
        Kind::Tabs => "Type a tab name",
        Kind::Revision => "Type a git revision: HEAD~1, a branch, a tag or a hash",
        Kind::Encoding(EncodingFor::Reopen) => "Reopen with encoding",
        Kind::Encoding(EncodingFor::Save) => "Save with encoding",
        Kind::Recent => "Type a file name or folder",
        Kind::Name(crate::picker::Naming::Workspace) => "Name the workspace",
        Kind::Name(crate::picker::Naming::Search) => "Name the search",
        Kind::Workspaces => "Type a workspace name",
    };
    let pv = PickerView {
        input: &p.input,
        placeholder,
        rows: &rows,
        selected,
        status,
    };
    view::draw_picker(gpu, ui, layout, &pv, blink);
}

/// Apply a text-editing command to a one-line field (the find bar). Returns
/// text to put on the clipboard for Copy and Cut. Line breaks are not typed
/// into fields; pasted ones are kept, so a multi-line literal can be found.
fn edit_field(
    field: &mut Solo,
    cmd: Command<'_>,
    paste: Option<&str>,
    tab_width: usize,
) -> Option<String> {
    let mut field = field.ed();
    let result = match cmd {
        Command::Move(motion, extend) => {
            field.move_cursor(motion, extend, tab_width, 1);
            Ok(())
        }
        Command::Insert(text) => field.insert(text).map(drop),
        Command::DeleteBack => field.delete_back().map(drop),
        Command::DeleteForward => field.delete_forward().map(drop),
        Command::Undo => field.undo().map(drop),
        Command::Redo => field.redo().map(drop),
        Command::SelectAll => {
            field.select_all();
            Ok(())
        }
        Command::Copy => return field.selected_text(),
        Command::Cut => {
            let text = field.selected_text();
            if text.is_some()
                && let Err(e) = field.delete_back()
            {
                log::error!("cut refused: {e}");
            }
            return text;
        }
        Command::Paste => match paste {
            Some(text) => field.insert(text).map(drop),
            None => Ok(()),
        },
        _ => Ok(()),
    };
    if let Err(e) = result {
        log::error!("field edit refused: {e}");
    }
    None
}

impl App {
    /// The pointer moved: carry on whatever drag is under way, then
    /// update what hovering shows.
    fn cursor_moved(&mut self, position: PhysicalPosition<f64>) {
        self.mouse = position;
        if self.sidebar.resizing {
            if let Some(layout) = self.window_layout() {
                self.sidebar.drag_to(&layout, to_f32(position.x));
                self.changed(false);
            }
        } else if let Some(d) = self.groups.dragging {
            self.drag_group_divider(d, to_f32(position.x));
        } else if let Some(grab) = self.clicks.scrollbar_grab {
            self.drag_scrollbar_to(to_f32(position.y) - grab);
        } else if let Some(grab) = self.clicks.minimap_grab {
            self.drag_minimap_to(to_f32(position.y) - grab);
        } else if self.sidebar.drag.is_some() {
            let at = (to_f32(position.x), to_f32(position.y));
            let copy = self.modifiers.alt_key();
            if let Some(layout) = self.window_layout()
                && self.sidebar.drag_over(&layout, at, copy)
            {
                self.changed(false);
            }
        } else if self.clicks.dragging {
            self.mouse_drag();
        }
        self.update_scrollbar_hover();
        self.update_fold_hover();
        self.update_cursor_icon();
        self.update_search_hover();
    }

    /// The left button came up: every drag ends, and a row dragged in the
    /// tree is dropped (Alt copies).
    fn left_released(&mut self) {
        self.clicks.dragging = false;
        self.clicks.scrollbar_grab = None;
        self.clicks.minimap_grab = None;
        self.groups.dragging = None;
        let dragged = self.sidebar.drag.is_some_and(|d| d.moving);
        if let Some((from, dir)) = self.sidebar.end_drag() {
            self.drop_in_tree(&from, &dir, self.modifiers.alt_key());
        }
        if dragged {
            self.changed(false);
        }
        if self.sidebar.resizing {
            self.sidebar.resizing = false;
            self.remember();
        }
    }
}

/// What Replace All puts in place of the panel's hits: its last search's
/// pattern and the replace field. `None` when there was no search.
fn replacement_of(panel: &crate::search_panel::Panel) -> Option<Replacement> {
    let query = &panel.params.as_ref()?.query;
    Some(Replacement {
        re: query.compile().ok().flatten()?,
        expand: panel.regex,
        template: panel.replacement(),
        multi_line: nimbus_core::search::spans_lines(query),
    })
}

impl ApplicationHandler<AppEvent> for App {
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::PickerChanged => self.tick_picker(),
            AppEvent::Menu(id) => {
                // A held menu shortcut delivers a menu event for every key
                // repeat, with no repeat flag. Drop events for the same item
                // arriving in quick succession, unless the command is one
                // that should repeat (undo, redo).
                let now = Instant::now();
                let repeat = self.last_menu.as_ref().is_some_and(|(last, at)| {
                    *last == id && now.duration_since(*at) < MENU_REPEAT_GAP
                });
                let Some(action) = crate::action::by_id(&id) else {
                    return;
                };
                self.last_menu = Some((id, now));
                if repeat && !matches!(action.command, Command::Undo | Command::Redo) {
                    return;
                }
                self.run(action.command, event_loop);
            }
            AppEvent::Diff(result) => self.diff_arrived(result),
            AppEvent::Merged(merged) => self.merged_arrived(merged),
            AppEvent::Syntax(result) => {
                let Some(result) = self.other_side_parsed(result) else {
                    return;
                };
                let Some(i) = self
                    .tabs
                    .iter()
                    .position(|t| self.docs[t.doc].syntax.waiting_for(result.id()))
                else {
                    return; // the tab closed, or its file was reloaded
                };
                let doc = &mut self.docs[self.tabs[i].doc];
                if let Some(job) = doc.syntax.finish(result, &doc.buffer, SYNTAX_BUDGET) {
                    spawn_syntax(&self.proxy, job);
                }
                if let Some(Off::Failed) = doc.syntax.off_reason() {
                    log::warn!("highlighting failed for {}", doc.display_name());
                }
                // A user query that did not compile is found on this load.
                let problems = nimbus_syntax::registry::take_warnings();
                if !problems.is_empty() {
                    self.say(problems.join("; "), true);
                }
                if i == self.active {
                    self.changed(false);
                }
            }
            AppEvent::Listing(req, result) => {
                let requests = match result {
                    Ok(entries) => self.sidebar.tree.loaded(&req, entries),
                    Err(e) => {
                        log::debug!("cannot list {}: {e}", req.path.display());
                        self.sidebar.tree.failed(&req);
                        Vec::new()
                    }
                };
                let layout = self.window_layout();
                self.sidebar.after_change(layout.as_ref());
                self.start_listings(requests);
                self.sync_watch();
                self.changed(false);
            }
            AppEvent::Disk(paths) => self.disk_changed(&paths),
            AppEvent::Index(token, chunk, error) => self.index_arrived(token, chunk, error),
            AppEvent::Folded(result) => self.folded(result),
            AppEvent::Revision(r) => self.revision_read(r),
            AppEvent::FolderCompare(batch) => self.folder_compare_arrived(batch),
            AppEvent::Save(event) => {
                self.save_event(event, event_loop);
                self.changed(false);
            }
            AppEvent::FontLoaded => self.changed(false),
            AppEvent::EditorFont(name, found) => self.editor_font_arrived(&name, found),
            AppEvent::UiFont(name, found) => self.ui_font_arrived(&name, found),
            AppEvent::FindFiles(batch) => {
                if self.sidebar.search.receive(batch) {
                    self.changed(false);
                }
            }
            AppEvent::Replaced(report) => self.replaced(report),
            AppEvent::Restored(path, doc) => self.restored_arrived(&path, doc),
            AppEvent::RecentMissing(gone) => self.recent_missing(&gone),
            AppEvent::FileOp(op, result) => self.file_op_done(&op, result),
            AppEvent::WatchWarning(warning) => {
                self.say(warning, true);
                self.changed(false);
            }
            AppEvent::Update(result, at_startup) => self.update_checked(result, at_startup),
            AppEvent::Search(batch) => {
                if !self.find.receive(batch) {
                    return;
                }
                // Jump once a match at or after the origin has arrived, or
                // when the search is complete (then it wraps to the first).
                let origin = self.find_origin;
                let reached = self.find.matches.last().is_some_and(|m| m.start >= origin);
                let done = self.find.complete && !self.find.matches.is_empty();
                if self.find_pending_jump && (reached || done) {
                    self.find_pending_jump = false;
                    self.jump_to_match(origin, false);
                }
                self.changed(false);
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        log::debug!("startup: resumed at {:?}", self.started.elapsed());

        let attrs = Window::default_attributes()
            .with_title(format!("{} — Nimbus", self.doc().display_name()))
            .with_inner_size(LogicalSize::new(960.0, 640.0));
        let attrs = self.restore_geometry(event_loop, with_app_icon(attrs));

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("could not create window: {e}");
                event_loop.exit();
                return;
            }
        };

        log::debug!("startup: window created at {:?}", self.started.elapsed());

        // Join the GPU thread started in `main`. By now it has usually
        // finished, so this rarely waits.
        let context = match self.gpu_init.take().map(JoinHandle::join) {
            Some(Ok(Ok(context))) => context,
            Some(Ok(Err(e))) => {
                log::error!("{e}");
                event_loop.exit();
                return;
            }
            Some(Err(_)) | None => {
                log::error!("GPU setup thread did not complete");
                event_loop.exit();
                return;
            }
        };
        log::debug!("startup: gpu joined at {:?}", self.started.elapsed());

        let font = match Font::bundled() {
            Ok(f) => f,
            Err(e) => {
                log::error!("{e}");
                event_loop.exit();
                return;
            }
        };
        let scale = effective_scale(window.scale_factor());
        match Gpu::new(
            &window,
            context,
            font,
            self.font_px(scale),
            self.options.linear_blend,
        ) {
            Ok(gpu) => self.gpu_ready(gpu),
            Err(e) => {
                log::error!("{e}");
                event_loop.exit();
                return;
            }
        }

        // Input methods (accents, CJK) are off by default in winit. On
        // macOS they also need to be on for dead keys to combine.
        window.set_ime_allowed(true);

        log::info!("window + gpu ready in {:?}", self.started.elapsed());
        let proxy = self.proxy.clone();
        self.menu_bar = menu::install(&window, &self.keymap, move |id| {
            let _ = proxy.send_event(AppEvent::Menu(id));
        });
        log::debug!("startup: menus at {:?}", self.started.elapsed());
        window.request_redraw();
        self.window = Some(window);
        self.update_title();
        if let Some(pattern) = self.options.find.clone() {
            self.open_find(false);
            let _ = self.find.find.ed().insert(&pattern);
            self.search(true);
        }
        if self.options.find.is_none() && self.options.diff.is_none() && !self.note_encoding() {
            self.note_conflict_markers();
        }
        if let Some(args) = self.options.merge.clone() {
            match open_merge_inputs(&args, &self.languages) {
                Ok(inputs) => {
                    self.start_merge(Shape::ThreeWay, inputs, Some(args.out.clone()), true);
                }
                Err(e) => self.say(e, true),
            }
        }
        if let Some((a, b)) = folder_diff(&self.options).map(|(a, b)| (a.to_owned(), b.to_owned()))
        {
            self.start_folder_compare(&a, &b);
        } else if let Some((a, b)) = self.options.diff.clone() {
            let b = io::canonical(&b);
            let found = self
                .tabs
                .iter()
                .position(|t| self.docs[t.doc].canonical.as_deref() == Some(b.as_path()));
            if let Some(i) = found {
                self.switch_to(i);
                match Document::open(&a) {
                    Ok(other) => self.start_compare(other, Other::File(a)),
                    Err(e) => self.say(format!("Cannot read {}: {e}", a.display()), true),
                }
            }
        }
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, _cause: StartCause) {
        let now = Instant::now();
        if self.saves.poll_at.is_some_and(|t| now >= t) {
            // Redraws only when a save ends (it reports with a message).
            self.saves.poll_at = None;
            self.poll_saves(event_loop);
        }
        let blink_changed = self.blink.tick(now);
        if self.tabs[self.active]
            .compare
            .as_ref()
            .and_then(|c| c.wake_at())
            .is_some_and(|t| now >= t)
        {
            self.changed(false);
        }
        let retry_due = self.hidden_retry.is_some_and(|(at, _)| now >= at);
        let scrollbar_due = self.scrollbar_hide_at.is_some_and(|at| now >= at);
        if scrollbar_due {
            self.scrollbar_hide_at = None;
        }
        if self.research_at.is_some_and(|t| now >= t) {
            self.research_at = None;
            if self.find.open {
                self.search(false);
                self.changed(false);
            }
        }
        if self.search_files_at.is_some_and(|t| now >= t) {
            self.search_files_at = None;
            self.search_files();
            self.changed(false);
        }
        if self.sidebar.search.tick(now) {
            self.changed(false);
        }
        if self.ws.save_at.is_some_and(|t| now >= t) {
            self.save_state();
        }
        if self.recheck_at.is_some_and(|t| now >= t) {
            self.recheck_at = None;
            let changes: Vec<Change> = std::mem::take(&mut self.recheck)
                .into_iter()
                .map(|path| Change {
                    path,
                    structural: false,
                })
                .collect();
            self.disk_changed(&changes);
        }
        let message_expired = self.message.as_ref().is_some_and(|m| now >= m.until);
        if message_expired {
            self.message = None;
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
        if self.pending_chord.is_some_and(|(_, until)| now >= until) {
            self.pending_chord = None;
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
        if (blink_changed || retry_due || scrollbar_due)
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.frames > 0 {
            // Huge files count their lines in the background, started
            // after the first frame so the disk is not shared with it.
            self.start_indexing();
        }
        if self.frames > 0 {
            self.start_restoring();
        }
        if self.frames > 0 && !self.watching {
            // Starting watches can take milliseconds (macOS restarts its
            // event stream for each folder), so not before the first frame.
            self.watching = true;
            self.sync_watch();
            // A configured font, looked up once the first frame is up.
            if !self.config.font.is_empty() {
                self.load_editor_font();
            }
            if !self.config.ui_font.is_empty() {
                self.load_ui_font();
            }
            // The startup update check, when the config asks for it: once,
            // after the first frame like the watches, and not in unattended
            // runs (--smoke, screenshots, benchmarks), which should not
            // depend on the network.
            let unattended = self.options.smoke
                || self.options.screenshot.is_some()
                || self.options.bench_typing > 0;
            if self.config.check_updates && !unattended {
                spawn_update_check(&self.proxy, true);
            }
        }
        // --smoke and --screenshot must not wait forever: a locked screen or
        // a hidden window never lets the first frame be drawn.
        // winit keeps calling this while it shuts down; once an exit is
        // under way there is nothing left to schedule or report.
        if event_loop.exiting() {
            return;
        }
        let unattended = self.options.smoke || self.options.screenshot.is_some();
        let deadline = self.started + UNATTENDED_TIMEOUT;
        if unattended && self.frames == 0 && Instant::now() >= deadline {
            print_err(
                "nimbus: no frame could be drawn in 10 s (window hidden or screen locked?); try --headless",
            );
            self.exit_code = 1;
            event_loop.exit();
            return;
        }
        // Sleep until the next blink or hidden-window retry, or indefinitely
        // when neither is pending.
        let next = [
            self.blink.next,
            self.hidden_retry.map(|(at, _)| at),
            self.scrollbar_hide_at,
            self.message.as_ref().map(|m| m.until),
            self.pending_chord.map(|(_, until)| until),
            self.research_at,
            self.search_files_at,
            self.sidebar.search.wake_at(),
            self.recheck_at,
            self.ws.save_at,
            self.saves.poll_at,
            self.tabs[self.active]
                .compare
                .as_ref()
                .and_then(|c| c.wake_at()),
            (unattended && self.frames == 0).then_some(deadline),
        ]
        .into_iter()
        .flatten()
        .min();
        event_loop.set_control_flow(next.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if self.window.is_none() || self.gpu.is_none() {
            return;
        }
        let now = Instant::now();
        if !matches!(event, WindowEvent::RedrawRequested) {
            log::trace!("window event: {event:?}");
        }
        match event {
            WindowEvent::CloseRequested => self.quit(event_loop),

            WindowEvent::ModifiersChanged(mods) => self.modifiers = mods.state(),

            WindowEvent::KeyboardInput { event, .. } => self.key_input(&event, now, event_loop),

            WindowEvent::Ime(ime) => self.ime(ime, event_loop),

            WindowEvent::CursorMoved { position, .. } => self.cursor_moved(position),

            WindowEvent::CursorLeft { .. } => {
                if self.sidebar.search.hover_gone() {
                    self.changed(false);
                }
            }

            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                if state == ElementState::Pressed {
                    self.mouse_down(now);
                    if let Some(cmd) = self.pending_action.take() {
                        self.run(cmd, event_loop);
                    }
                } else {
                    self.left_released();
                }
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => self.tree_context_menu(),

            WindowEvent::MouseWheel { delta, .. } => self.mouse_wheel(delta),

            // A file dragged in from the desktop opens; a folder joins the
            // workspace (M19).
            WindowEvent::DroppedFile(path) => {
                if path.is_dir() {
                    self.add_root(&path);
                } else {
                    self.open_path(&path);
                }
                self.changed(false);
            }

            WindowEvent::Focused(false) => {
                // Show the cursor solid in the background; it may have been
                // in the off phase of a blink.
                self.blink.stop();
                self.changed(false);
            }
            WindowEvent::Occluded(true) => self.blink.stop(),
            WindowEvent::Focused(true) => {
                self.blink.reset(now);
                self.changed(false);
            }
            WindowEvent::Occluded(false) => self.changed(false),

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // A Resized with the new physical size follows; this only has
                // to re-rasterize glyphs at the new density.
                let px = self.font_px(effective_scale(scale_factor));
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.set_font_size(px);
                }
                self.changed(false);
            }

            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.resize(size.width, size.height);
                }
                self.changed(false);
            }

            WindowEvent::RedrawRequested => self.redraw(event_loop),

            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_zoom_starts_where_the_last_session_left_it_unless_told_not_to() {
        let state = State {
            font_size: Some(20.0),
            ..State::default()
        };
        let mut config = Config::default();
        assert_eq!(starting_zoom(&config, &state), 20.0);
        config.remember_zoom = false;
        assert_eq!(starting_zoom(&config, &state), 14.0);
        config.remember_zoom = true;
        let wild = State {
            font_size: Some(500.0),
            ..State::default()
        };
        assert_eq!(starting_zoom(&config, &wild), MAX_ZOOM);
        assert_eq!(starting_zoom(&config, &State::default()), 14.0);
    }

    #[test]
    fn blink_toggles_then_stops_when_idle() {
        let t0 = Instant::now();
        let mut b = Blink::new(t0);
        assert!(!b.tick(t0), "not due yet");
        assert!(b.tick(t0 + BLINK_PERIOD));
        assert!(!b.visible);
        assert!(b.tick(t0 + BLINK_PERIOD * 2));
        assert!(b.visible);
        // Long after the last input: shows solid and stops scheduling.
        let late = t0 + BLINK_IDLE + BLINK_PERIOD * 3;
        b.visible = false;
        b.next = Some(late);
        assert!(b.tick(late));
        assert!(b.visible);
        assert_eq!(b.next, None);
        b.reset(late);
        assert!(b.next.is_some());
    }
}
