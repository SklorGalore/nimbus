//! The application: windows events in, editor commands out.
//!
//! `winit` drives the program through an [`ApplicationHandler`]: we hand it a
//! struct, and it calls back into us when the OS has events. Windows may only
//! be created once the app is `resumed`, which is why creation lives there and
//! not in `main`. Nothing redraws on a timer except the cursor blink (and
//! short retries), so an idle editor uses no CPU.

use std::collections::HashSet;
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
use nimbus_core::{Buffer, Text};
use nimbus_render::{Font, FrameStatus, Gpu, GpuContext, GpuError};
use nimbus_syntax::state::{Job, JobResult, MAX_HIGHLIGHT_BYTES, Off};

use crate::config::{self, Config};
use crate::document::{DiskChange, Document, MAX_COPY_BYTES};
use crate::find::{self, Field, FindBar, SearchBatch};
use crate::input::{self, Command, Motion};
use crate::io;
use crate::keymap::Keymap;
use crate::languages::Languages;
use crate::menu::{self, MenuBar};
use crate::picker::{self, Item, Kind, Picker};
use crate::sidebar::{self, Sidebar};
use crate::state::State;
use crate::theme::{Theme, Ui};
use crate::tree::{self, Entry, ListRequest};
use crate::view::{self, Decorations, Layout, PickerView, Preedit, TabLabel, View};
use crate::watch::{Change, Watch};
use crate::{Options, effective_scale, print_err, print_out, save_screenshot, to_f32};

/// Events sent to the event loop from other threads.
#[derive(Debug)]
pub enum AppEvent {
    /// Results from a background search.
    Search(SearchBatch),
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
    /// indexing token.
    Index(u64, IndexChunk),
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

/// How long `--smoke` and `--screenshot` wait for a first frame.
const UNATTENDED_TIMEOUT: Duration = Duration::from_secs(10);

/// Menu events for the same item closer together than this are key
/// repeats from a held shortcut. Auto-repeat runs at about 30 ms; a
/// deliberate second press is rarely this fast.
const MENU_REPEAT_GAP: Duration = Duration::from_millis(250);

/// How long a message stays on screen.
const MESSAGE_TIME: Duration = Duration::from_secs(4);

/// One open file: its text and history, and where it is scrolled to.
pub struct Tab {
    /// The document.
    pub doc: Document,
    /// Its scroll position and wrap setting.
    pub view: View,
    /// The "too large to highlight" note has been shown.
    noted_size: bool,
}

impl Tab {
    /// A tab showing `doc`, wrapped if `wrap`.
    #[must_use]
    pub fn new(doc: Document, wrap: bool) -> Self {
        let mut view = View::default();
        view.set_wrap(wrap);
        Self {
            doc,
            view,
            noted_size: false,
        }
    }
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

/// Keystroke-to-present latency tracking (DESIGN.md §5). The M8 overlay
/// shows these values; until then they are logged at debug level.
#[derive(Default)]
struct Latency {
    /// When the oldest input not yet on screen arrived.
    pending: Option<Instant>,
    worst: Duration,
    /// Every sample, kept only during `--bench-typing`.
    samples: Option<Vec<Duration>>,
}

impl Latency {
    fn input(&mut self, now: Instant) {
        self.pending.get_or_insert(now);
    }

    fn presented(&mut self) {
        if let Some(started) = self.pending.take() {
            let took = started.elapsed();
            self.worst = self.worst.max(took);
            log::debug!("keystroke to present: {took:?} (worst {:?})", self.worst);
            if let Some(samples) = &mut self.samples {
                samples.push(took);
            }
        }
    }
}

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
    frames: u64,
    tabs: Vec<Tab>,
    active: usize,
    blink: Blink,
    preedit: Option<Preedit>,
    latency: Latency,
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
    /// The last menu event and when it arrived, to drop auto-repeats.
    last_menu: Option<(String, Instant)>,
    /// The project tree.
    sidebar: Sidebar,
    /// Changes on disk to the sidebar's folders and open files.
    watch: Watch,
    /// Remembered between runs (sidebar visibility and width).
    ui_state: State,
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
        proxy: EventLoopProxy<AppEvent>,
        warnings: Vec<String>,
    ) -> Self {
        let (keymap, key_warnings) = Keymap::new(&config.keys);
        let (theme, theme_warnings) = Theme::load(&config.theme);
        let ui_state = State::load();
        let disk_proxy = proxy.clone();
        let warn_proxy = proxy.clone();
        let sidebar = Sidebar::new(
            io::canonical(&root),
            ui_state.sidebar_visible,
            ui_state.sidebar_width,
        );
        let wrap = options.wrap || config.soft_wrap;
        let mut tabs: Vec<Tab> = docs.into_iter().map(|d| Tab::new(d, wrap)).collect();
        if tabs.is_empty() {
            tabs.push(Tab::new(Document::untitled(), wrap));
        }
        let all_warnings: Vec<String> = warnings
            .into_iter()
            .chain(key_warnings)
            .chain(theme_warnings)
            .collect();
        let message = (!all_warnings.is_empty()).then(|| {
            for w in &all_warnings {
                log::warn!("{w}");
            }
            Message {
                text: all_warnings.join("; "),
                is_error: true,
                until: Instant::now() + MESSAGE_TIME * 2,
            }
        });
        Self {
            options,
            started,
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
            frames: 0,
            tabs,
            active: 0,
            blink: Blink::new(started),
            preedit: None,
            latency: Latency::default(),
            mouse: PhysicalPosition::new(0.0, 0.0),
            ime_area: None,
            title_key: None,
            hidden_retry: None,
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
            last_menu: None,
            sidebar,
            watch: Watch::new(
                move |changes| {
                    let _ = disk_proxy.send_event(AppEvent::Disk(changes));
                },
                move |warning| {
                    let _ = warn_proxy.send_event(AppEvent::WatchWarning(warning));
                },
            ),
            ui_state,
            resize_cursor: false,
            watching: false,
            pending_line: None,
            recheck: Vec::new(),
            recheck_at: None,
        }
    }

    fn tab_width(&self) -> usize {
        self.config.tab_width
    }

    fn doc(&self) -> &Document {
        &self.tabs[self.active].doc
    }

    fn doc_mut(&mut self) -> &mut Document {
        &mut self.tabs[self.active].doc
    }

    fn font_px(&self, scale: f64) -> f32 {
        to_f32(self.config.font_size * scale)
    }

    /// Geometry for the current window, if it exists.
    fn layout(&self) -> Option<Layout> {
        let (window, gpu) = (self.window.as_ref()?, self.gpu.as_ref()?);
        let scale = to_f32(effective_scale(window.scale_factor()));
        Some(
            layout_for(gpu, scale, self.doc(), &self.find, self.message.is_some())
                .with_sidebar(self.sidebar.px(scale)),
        )
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
        if keep_cursor_visible && let Some(layout) = self.layout() {
            let tab_width = self.tab_width();
            let tab = &mut self.tabs[self.active];
            tab.view.ensure_cursor_visible(&tab.doc, &layout, tab_width);
        }
        self.update_title();
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// Run a command from a key, menu, the palette or the IME.
    fn run(&mut self, cmd: Command<'_>, event_loop: &ActiveEventLoop) {
        if self.picker.is_some() && self.run_picker(cmd, event_loop) {
            self.changed(false);
            return;
        }
        if self.find.focused && self.run_find(cmd) {
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
                let wrap = self.config.soft_wrap;
                self.tabs.push(Tab::new(Document::untitled(), wrap));
                self.switch_to(self.tabs.len() - 1);
            }
            Command::CloseTab => self.close_tab(self.active, event_loop),
            Command::NextTab => self.switch_to((self.active + 1) % self.tabs.len()),
            Command::PrevTab => {
                self.switch_to((self.active + self.tabs.len() - 1) % self.tabs.len());
            }
            Command::GoToTab(i) => self.switch_to(i.min(self.tabs.len() - 1)),
            Command::QuickOpen => self.open_picker(Kind::Files),
            Command::GoToLine => self.open_picker(Kind::Line),
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
            Command::ToggleSidebar => {
                self.sidebar.visible = !self.sidebar.visible;
                if self.sidebar.visible {
                    self.show_sidebar();
                } else {
                    self.sidebar.focused = false;
                }
                self.remember();
            }
            Command::FocusSidebar => {
                self.sidebar.visible = true;
                self.sidebar.focused = true;
                self.find.focused = false;
                self.show_sidebar();
                self.remember();
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
            Command::ReloadFromDisk => {
                let languages = &self.languages;
                match self.tabs[self.active].doc.reload(languages) {
                    Ok(()) => self.say("Reloaded from disk", false),
                    Err(e) => self.say(format!("Cannot reload: {e}"), true),
                }
            }
            Command::KeepMine => {
                self.doc_mut().keep_mine();
                self.say(
                    "Keeping your version; saving overwrites the file on disk",
                    false,
                );
            }
            Command::OpenConfig => self.open_config(),
            Command::Close => {
                self.quit(event_loop);
                return true;
            }
            _ => return false,
        }
        self.changed(false);
        true
    }

    /// Editing, movement and find commands on the active tab.
    fn run_edit(&mut self, cmd: Command<'_>) -> Result<(), nimbus_core::EditError> {
        let page = self.layout().map_or(1, |l| l.full_lines());
        let tab_width = self.tab_width();
        match cmd {
            Command::Move(motion, extend) => {
                self.move_cursor(motion, extend, page);
                Ok(())
            }
            Command::Insert(text) => self.doc_mut().insert(text).map(drop),
            Command::Newline => self.doc_mut().insert_newline().map(drop),
            Command::Tab => match self.doc_mut().indent_lines() {
                Ok(true) => Ok(()),
                Ok(false) => self.doc_mut().insert("\t").map(drop),
                Err(e) => Err(e),
            },
            Command::Outdent => self.doc_mut().outdent_lines(tab_width),
            Command::Undo => self.doc_mut().undo().map(drop),
            Command::Redo => self.doc_mut().redo().map(drop),
            Command::SelectAll => {
                self.doc_mut().select_all();
                Ok(())
            }
            Command::Copy | Command::Cut | Command::Paste => self.clipboard_command(cmd),
            Command::Escape => {
                if self.find.open {
                    self.find.close();
                } else {
                    let head = self.doc().selection().head;
                    self.doc_mut().click(head, false);
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
                tab.doc.preferred_column = None;
                self.say(if on { "Soft wrap on" } else { "Soft wrap off" }, false);
                Ok(())
            }
            Command::DeleteBack => self.doc_mut().delete_back().map(drop),
            Command::DeleteForward => self.doc_mut().delete_forward().map(drop),
            Command::DeleteWordBack => self.doc_mut().delete_to(Motion::WordLeft).map(drop),
            Command::DeleteWordForward => self.doc_mut().delete_to(Motion::WordRight).map(drop),
            Command::DeleteToLineStart => self.doc_mut().delete_to(Motion::LineStart).map(drop),
            _ => Ok(()),
        }
    }

    fn move_cursor(&mut self, motion: Motion, extend: bool, page: usize) {
        let tab_width = self.tab_width();
        let layout = self.layout();
        let tab = &mut self.tabs[self.active];
        match &layout {
            Some(layout) if tab.view.wrap => {
                let mut w = tab.view.wrap_view(layout, tab_width);
                tab.doc.move_cursor_wrapped(motion, extend, &mut w, page);
            }
            _ => tab.doc.move_cursor(motion, extend, tab_width, page),
        }
        if matches!(motion, Motion::PageUp | Motion::PageDown)
            && let Some(layout) = layout
        {
            // Scroll by the same amount so the cursor keeps its row.
            let dir = if motion == Motion::PageUp { -1.0 } else { 1.0 };
            #[expect(clippy::cast_precision_loss, reason = "page is a few hundred lines")]
            let dy = dir * page as f32 * layout.metrics.line_height;
            tab.view.scroll_by(dy, &tab.doc, &layout, tab_width);
        }
    }

    /// Make tab `i` the active one.
    fn switch_to(&mut self, i: usize) {
        if i == self.active || i >= self.tabs.len() {
            return;
        }
        self.active = i;
        self.preedit = None;
        self.sidebar.focused = false;
        if self.find.open {
            self.find_origin = self.doc().cursor();
            self.search(false);
        }
        self.reveal_active();
        self.changed(false);
    }

    /// Move the cursor to the start of line `n` (counting from 1). Past the
    /// last line it goes to the last line; in a huge file whose lines are
    /// still being counted, the jump waits until counting reaches it.
    fn go_to_line(&mut self, n: usize) {
        let t = &self.tabs[self.active].doc.buffer;
        let line = n.saturating_sub(1);
        if line >= t.len_lines() && !t.is_fully_indexed() {
            self.pending_line = Some(n);
            self.say(
                format!("Going to line {n} once lines are counted that far"),
                false,
            );
            return;
        }
        self.pending_line = None;
        let pos = t.line_to_byte(line.min(t.len_lines().saturating_sub(1)));
        let doc = self.doc_mut();
        doc.click(pos, false);
        doc.preferred_column = None;
        self.changed(true);
    }

    /// Start line counting for any mapped document that has not started.
    fn start_indexing(&mut self) {
        for tab in &mut self.tabs {
            if let Some(job) = tab.doc.indexing.take() {
                let proxy = self.proxy.clone();
                job.spawn(move |token, chunk| {
                    proxy.send_event(AppEvent::Index(token, chunk)).is_ok()
                });
            }
        }
    }

    /// A chunk of line index arrived: merge it, report progress, and finish
    /// a go-to-line that was waiting for it.
    fn index_arrived(&mut self, token: u64, chunk: IndexChunk) {
        let Some(i) = self
            .tabs
            .iter_mut()
            .position(|t| t.doc.index_token == Some(token))
        else {
            return; // closed or reloaded meanwhile
        };
        if !self.tabs[i].doc.merge_index(token, chunk) || i != self.active {
            return;
        }
        let t = &self.tabs[i].doc.buffer;
        if t.is_fully_indexed() {
            let lines = t.len_lines();
            self.say(format!("{lines} lines"), false);
        } else {
            #[expect(clippy::cast_precision_loss, reason = "a percentage for display")]
            let pct = t.indexed_bytes() as f64 * 100.0 / t.len_bytes().max(1) as f64;
            self.say(format!("Counting lines… {pct:.0}%"), false);
        }
        if let Some(n) = self.pending_line {
            let t = &self.tabs[i].doc.buffer;
            if n <= t.len_lines() || t.is_fully_indexed() {
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
        let layout = self.layout();
        let requests = self.sidebar.reveal(&path, layout.as_ref());
        self.start_listings(requests);
    }

    /// Remember the sidebar's visibility and width for the next run.
    fn remember(&mut self) {
        let state = State {
            sidebar_visible: self.sidebar.visible,
            sidebar_width: self.sidebar.width,
        };
        if state != self.ui_state {
            state.save();
            self.ui_state = state;
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
            if let Some(parent) = tab.doc.canonical.as_deref().and_then(Path::parent) {
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
        for tab in &mut self.tabs {
            let Some(path) = tab.doc.canonical.as_deref() else {
                continue;
            };
            if !changes.iter().any(|c| c.path == path) {
                continue;
            }
            // A file written to over and over (a growing log) reloads at
            // most once per RELOAD_GAP; the rest wait for the timer. Not a
            // mapped one: it never reloads by itself unless cut short, and
            // that must be handled at once.
            if let Some(at) = tab.doc.reloaded_at
                && !tab.doc.buffer.is_mapped()
                && now.duration_since(at) < RELOAD_GAP
            {
                let due = at + RELOAD_GAP;
                self.recheck_at = Some(self.recheck_at.map_or(due, |t| t.min(due)));
                if !self.recheck.iter().any(|p| p == path) {
                    self.recheck.push(path.to_path_buf());
                }
                continue;
            }
            let name = tab.doc.display_name();
            let note = match tab.doc.disk_changed(&self.languages) {
                DiskChange::None => continue,
                DiskChange::Reloaded => format!("{name} changed on disk; reloaded"),
                DiskChange::Conflict => {
                    is_error = true;
                    format!(
                        "{name} changed on disk. Run Reload from Disk, or Keep My Version, \
                         from the command palette"
                    )
                }
                DiskChange::NotReloaded => format!(
                    "{name} changed on disk; huge files do not reload by themselves. \
                     Run Reload from Disk from the command palette to see the changes"
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
        self.changed(false);
    }

    /// A command while the sidebar has focus. Movement and Enter work in
    /// the tree, Escape gives focus back to the text, and typing is
    /// ignored. Returns `false` for commands that should run as usual.
    fn run_sidebar(&mut self, cmd: Command<'_>) -> bool {
        let Some(layout) = self.layout() else {
            return false;
        };
        let (motion, enter) = match cmd {
            Command::Move(m, _) => (Some(m), false),
            Command::Newline => (None, true),
            Command::Escape => {
                self.sidebar.focused = false;
                return true;
            }
            Command::Insert(_)
            | Command::Tab
            | Command::Outdent
            | Command::DeleteBack
            | Command::DeleteForward
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
        true
    }

    /// Open `path` in a tab, or switch to it if it is already open.
    fn open_path(&mut self, path: &Path) {
        // Whatever asked (the sidebar, the picker, a dialog), the file
        // gets the keyboard.
        self.sidebar.focused = false;
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(i) = self.tabs.iter().position(|t| {
            t.doc.path.as_ref().is_some_and(|p| {
                std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()) == canonical
            })
        }) {
            self.switch_to(i);
            return;
        }
        match Document::open(path) {
            Ok(mut doc) => {
                doc.detect_language(&self.languages);
                // Replace a lone untouched untitled tab rather than keeping it.
                let replace = self.tabs.len() == 1
                    && self.tabs[0].doc.path.is_none()
                    && !self.tabs[0].doc.is_dirty();
                let tab = Tab::new(doc, self.config.soft_wrap);
                if replace {
                    self.tabs[0] = tab;
                    self.active = 0;
                    self.title_key = None;
                    self.reveal_active();
                } else {
                    self.tabs.push(tab);
                    self.switch_to(self.tabs.len() - 1);
                }
                self.sync_watch();
                self.changed(false);
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

    /// Save tab `i`; an untitled tab asks for a name. Returns whether it
    /// was saved.
    fn save(&mut self, i: usize) -> bool {
        if self.tabs[i].doc.path.is_none() {
            return self.save_as(i);
        }
        match self.tabs[i].doc.save() {
            Ok(()) => {
                let name = self.tabs[i].doc.display_name();
                log::info!("saved {name}");
                self.title_key = None;
                self.say(format!("Saved {name}"), false);
                true
            }
            Err(e) => {
                log::error!("{e}");
                self.dialog(MessageLevel::Error, "Could not save", &e.to_string());
                false
            }
        }
    }

    /// Ask for a file name and save tab `i` there.
    fn save_as(&mut self, i: usize) -> bool {
        let doc = &self.tabs[i].doc;
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
            return false;
        };
        let old = self.tabs[i].doc.path.replace(path);
        self.title_key = None;
        let saved = self.save(i);
        if saved {
            // A new name can mean a new language: untitled to `notes.md`.
            self.tabs[i].doc.detect_language(&self.languages);
            self.sync_watch();
            self.reveal_active();
        } else {
            // Keep pointing at the old file, so the next save does not
            // silently retry the path that failed.
            self.tabs[i].doc.path = old;
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

    /// Offer to save tab `i` if it has unsaved changes. Returns `false` if
    /// the user cancelled, or the save failed, and the tab must stay open.
    fn confirm_discard(&mut self, i: usize) -> bool {
        let unattended = self.options.smoke || self.options.screenshot.is_some();
        let doc = &self.tabs[i].doc;
        // A deleted file's tab holds the only copy left, edited or not.
        if !(doc.is_dirty() || doc.deleted) || unattended {
            return true;
        }
        self.switch_to(i);
        let doc = &self.tabs[i].doc;
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
        match dialog.show() {
            MessageDialogResult::Yes => self.save(i),
            MessageDialogResult::Custom(label) if label == "Save" => self.save(i),
            MessageDialogResult::No => true,
            MessageDialogResult::Custom(label) if label == "Don't Save" => true,
            _ => false, // Cancel or dismissed: keep editing.
        }
    }

    /// Close tab `i`, offering to save it. Closing the last tab quits.
    fn close_tab(&mut self, i: usize, event_loop: &ActiveEventLoop) {
        if !self.confirm_discard(i) {
            return;
        }
        if self.tabs.len() == 1 {
            event_loop.exit();
            return;
        }
        self.tabs.remove(i);
        if self.active >= i && self.active > 0 {
            self.active -= 1;
        }
        self.title_key = None;
        self.preedit = None;
        if self.find.open {
            self.search(false);
        }
        self.sync_watch();
        self.reveal_active();
    }

    /// Quit, offering to save every tab with unsaved changes.
    fn quit(&mut self, event_loop: &ActiveEventLoop) {
        for i in 0..self.tabs.len() {
            if !self.confirm_discard(i) {
                return;
            }
        }
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
        self.languages = languages;
        for tab in &mut self.tabs {
            tab.doc.detect_language(&self.languages);
        }
        self.keymap = keymap;
        self.config = config;
        if let Some(menu) = &self.menu_bar {
            menu.update(&self.keymap);
        }
        if let (Some(gpu), Some(window)) = (self.gpu.as_mut(), self.window.as_ref()) {
            gpu.set_font_size(to_f32(
                self.config.font_size * effective_scale(window.scale_factor()),
            ));
            gpu.set_tab_width(self.config.tab_width);
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
            Kind::Files => Picker::files(&self.root, notify),
            Kind::Line => {
                let t = &self.doc().buffer;
                Picker::go_to_line(notify, t.is_fully_indexed().then(|| t.len_lines()))
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
                let root = p.root.clone();
                self.picker = None;
                match chosen {
                    Some(Item::Action(a)) => self.run(a.command, event_loop),
                    Some(Item::File(rel)) => self.open_path(&root.join(rel)),
                    Some(Item::Line(n)) => self.go_to_line(n),
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
        let selection = self.doc().selected_text();
        self.find.open(replace_mode, selection);
        self.find_origin = self.doc().selection().range().start;
        self.search(true);
    }

    /// Search the document for the current query. With `jump`, select the
    /// first match from where the search started, as incremental search does.
    fn search(&mut self, jump: bool) {
        let proxy = self.proxy.clone();
        let doc = &self.tabs[self.active].doc;
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
            self.doc_mut().select(range);
            if let Some(layout) = self.layout() {
                let tab_width = self.tab_width();
                let tab = &mut self.tabs[self.active];
                tab.view.ensure_cursor_visible(&tab.doc, &layout, tab_width);
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
                self.find.field_mut().select_all();
            }
            Command::Tab | Command::Outdent => {}
            Command::Newline if self.find.focus == Field::Replace => self.replace_one(),
            Command::Newline => {
                let sel = self.doc().selection().range();
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
            | Command::FocusSidebar
            | Command::ToggleIgnored
            | Command::ReloadFromDisk
            | Command::KeepMine
            | Command::OpenFile
            | Command::NewFile
            | Command::NextTab
            | Command::PrevTab
            | Command::GoToTab(_)
            | Command::QuickOpen
            | Command::Palette
            | Command::ReloadConfig
            | Command::OpenConfig => return false,
            _ => {
                let paste = (cmd == Command::Paste)
                    .then(|| self.clipboard_get())
                    .flatten();
                let tab_width = self.tab_width();
                let field = self.find.field_mut();
                let before = field.version;
                let copied = edit_field(field, cmd, paste.as_deref(), tab_width);
                let changed = field.version != before;
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
        let sel = self.doc().selection().range();
        let is_match = self.find.current.and_then(|i| self.find.matches.get(i)) == Some(&sel);
        if is_match && !sel.is_empty() {
            if let Some(replacement) = self.find.replacement_for(self.doc(), sel.clone())
                && let Err(e) = self.doc_mut().replace(sel, &replacement)
            {
                log::error!("replace refused: {e}");
            }
            self.doc_mut().history.seal();
            // Search the changed text, then go to the next match after the
            // replacement; on large files that waits for the results.
            self.find_origin = self.doc().cursor();
            self.search(true);
            return;
        }
        let from = self.doc().cursor();
        self.jump_to_match(from, false);
    }

    /// Replace every match as one undo step.
    fn replace_all(&mut self) {
        if !self.find.complete {
            // A background search is still running: finish it here so no
            // match is missed.
            let doc = &self.tabs[self.active].doc;
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
                let rep = self.find.replacement_for(self.doc(), r.clone())?;
                Some((r.clone(), rep))
            })
            .collect();
        match self.doc_mut().replace_many(&items) {
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

    /// Copy, cut or paste in the editor. Cut and paste are their own undo
    /// steps.
    fn clipboard_command(&mut self, cmd: Command<'_>) -> Result<(), nimbus_core::EditError> {
        if cmd == Command::Paste {
            return match self.clipboard_get() {
                Some(text) => self.doc_mut().paste(&text).map(drop),
                None => Ok(()),
            };
        }
        let len = self.doc().selection().range().len();
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
        let Some(text) = self.doc().selected_text() else {
            return Ok(());
        };
        self.clipboard_set(text);
        if cmd != Command::Cut {
            return Ok(());
        }
        let doc = self.doc_mut();
        doc.history.seal();
        let result = doc.delete_back().map(drop);
        doc.history.seal();
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
        let Some(layout) = self.layout() else { return };
        let (x, y) = (to_f32(self.mouse.x), to_f32(self.mouse.y));
        if self.picker.is_some() {
            self.picker_click(&layout, x, y);
            return;
        }
        if self.sidebar.on_divider(&layout, x) {
            self.sidebar.resizing = true;
            return;
        }
        if self.sidebar.contains(&layout, x) {
            self.find.focused = false;
            let outcome = self.sidebar.click(&layout, y);
            self.start_listings(outcome.requests);
            if let Some(path) = outcome.open {
                self.sidebar.focused = false;
                self.open_path(&path);
            }
            self.changed(false);
            return;
        }
        self.sidebar.focused = false;
        let labels = self.tab_labels();
        if let Some(i) = view::tab_at(&layout, &labels, self.active, x, y) {
            self.switch_to(i);
            return;
        }
        let rows = match (self.find.open, self.find.replace_mode) {
            (false, _) => 0,
            (true, false) => 1,
            (true, true) => 2,
        };
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
        self.find.focused = false;
        let tab_width = self.tab_width();
        let tab = &mut self.tabs[self.active];
        let pos = tab.view.hit(x, y, &tab.doc, &layout, tab_width);
        let count = self.clicks.press(now, (x, y), layout.metrics.cell_width);
        let doc = &mut tab.doc;
        match count {
            1 => {
                doc.click(pos, self.modifiers.shift_key());
                // Dragging extends from the selection's anchor: the old one
                // after a Shift+click, the clicked point otherwise.
                let anchor = doc.selection().anchor;
                self.clicks.anchor = anchor..anchor;
            }
            2 => {
                let word = nimbus_core::movement::word_at(&doc.buffer, pos);
                self.clicks.anchor = word.clone();
                doc.select(word);
            }
            _ => {
                let line = nimbus_core::movement::line_range(&doc.buffer, pos);
                self.clicks.anchor = line.clone();
                doc.select(line);
            }
        }
        self.clicks.dragging = true;
        self.blink.reset(now);
        self.changed(false);
    }

    /// Scroll the sidebar or the text under the pointer.
    fn mouse_wheel(&mut self, delta: MouseScrollDelta) {
        let Some(layout) = self.layout() else { return };
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
        if self.sidebar.contains(&layout, to_f32(self.mouse.x)) {
            self.sidebar.wheel(-dy, &layout);
        } else {
            let tab_width = self.tab_width();
            let tab = &mut self.tabs[self.active];
            tab.view.scroll_by(-dy, &tab.doc, &layout, tab_width);
            tab.view.scroll_x_by(-dx);
        }
        self.changed(false);
    }

    /// Show the resize cursor over the sidebar's divider, the usual one
    /// elsewhere. Only tells the window when it changes.
    fn update_cursor_icon(&mut self) {
        let Some(layout) = self.layout() else { return };
        let resize =
            self.sidebar.resizing || self.sidebar.on_divider(&layout, to_f32(self.mouse.x));
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
        let pos = tab.view.hit(x, y, &tab.doc, &layout, tab_width);
        let t = &tab.doc.buffer;
        let unit = match self.clicks.count {
            1 => pos..pos,
            2 => nimbus_core::movement::word_at(t, pos),
            _ => nimbus_core::movement::line_range(t, pos),
        };
        let anchor = self.clicks.anchor.clone();
        if unit.start < anchor.start {
            tab.doc.select_span(anchor.end, unit.start);
        } else {
            tab.doc.select_span(anchor.start, unit.end.max(anchor.end));
        }
        self.changed(true);
    }

    /// Labels for the tab bar.
    fn tab_labels(&self) -> Vec<TabLabel> {
        self.tabs
            .iter()
            .map(|t| {
                let name = t.doc.display_name();
                let name = if t.doc.deleted {
                    format!("{name} (deleted)")
                } else if t.doc.conflict {
                    format!("{name} (changed on disk)")
                } else {
                    name
                };
                TabLabel {
                    name,
                    dirty: t.doc.is_dirty(),
                }
            })
            .collect()
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
                let root = picker.root.clone();
                self.picker = None;
                match chosen {
                    Some(Item::File(rel)) => self.open_path(&root.join(rel)),
                    Some(Item::Action(a)) => self.pending_action = Some(a.command),
                    Some(Item::Line(n)) => self.go_to_line(n),
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
        if let Some(job) = tab.doc.syntax.take_job(&tab.doc.buffer) {
            spawn_syntax(&self.proxy, job);
        }
        if tab.doc.syntax.lexical().is_some() && !tab.noted_size {
            tab.noted_size = true;
            let mb = MAX_HIGHLIGHT_BYTES / (1024 * 1024);
            self.say(format!("Over {mb} MB: simpler syntax colours"), false);
        }
    }

    /// Queue the whole frame: editor text, then the overlays.
    fn paint(&mut self, gpu: &mut Gpu, layout: &Layout) {
        let labels = self.tab_labels();
        let tab_width = self.tab_width();
        let tab = &mut self.tabs[self.active];
        // Bring the tree up to date with this frame's edits, or hand the
        // rest to a worker if that would take too long.
        if let Some(job) = tab.doc.syntax.refresh(&tab.doc.buffer, SYNTAX_BUDGET) {
            spawn_syntax(&self.proxy, job);
        }
        paint_scene(
            gpu,
            layout,
            &mut Scene {
                doc: &tab.doc,
                view: &mut tab.view,
                theme: &mut self.theme,
                find: &self.find,
                message: self.message.as_ref(),
                cursor_visible: self.blink.visible
                    && self.picker.is_none()
                    && !self.sidebar.focused,
                preedit: self.preedit.as_ref(),
                tab_width,
            },
        );
        view::draw_tabs(gpu, &self.theme.ui, layout, &labels, self.active);
        let active = self.tabs[self.active].doc.canonical.as_deref();
        sidebar::draw(gpu, &self.theme.ui, layout, &self.sidebar, active);
        if let Some(p) = &self.picker {
            let ui = &self.theme.ui;
            draw_picker_overlay(gpu, ui, layout, p, &self.keymap, self.blink.visible);
        }
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        // The first parse runs on a worker while this frame shows the
        // text plain; colours follow when it finishes. Before the layout,
        // which leaves room for any message this adds.
        self.start_syntax();
        if self.frames == 0 && self.sidebar.visible {
            // Listing runs on a worker; this only starts it.
            self.show_sidebar();
        }
        let Some(layout) = self.layout() else { return };
        // Painting reads the whole app; take the GPU out while it does.
        if let Some(mut gpu) = self.gpu.take() {
            self.paint(&mut gpu, &layout);
            self.gpu = Some(gpu);
        }
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
        self.latency.presented();

        // Tell the IME where the cursor is, so its candidate window appears
        // next to it. Physical pixels, relative to the window.
        let tab_width = self.config.tab_width;
        let r = self.tabs[self.active].view.cursor_rect(
            &layout,
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

        if self.options.bench_typing > 0 && self.frames >= 2 {
            self.bench_step(event_loop);
            return;
        }

        if self.frames == 1 {
            if let Some(path) = &self.options.screenshot {
                if !save_screenshot(path, gpu.take_capture().as_ref()) {
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
        let samples = self.latency.samples.get_or_insert_with(Vec::new);
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
        self.latency.input(Instant::now());
        self.run(Command::Insert(ch), event_loop);
    }

    fn ime(&mut self, ime: Ime, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        self.blink.reset(now);
        match ime {
            Ime::Enabled => {}
            Ime::Preedit(text, cursor) => {
                self.preedit = (!text.is_empty()).then_some(Preedit { text, cursor });
                self.latency.input(now);
                self.changed(false);
            }
            Ime::Commit(text) => {
                self.preedit = None;
                self.latency.input(now);
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

/// Geometry for the window `gpu` draws to, with room for the find bar and
/// message line when they are shown.
pub fn layout_for(gpu: &Gpu, scale: f32, doc: &Document, find: &FindBar, message: bool) -> Layout {
    let (w, h) = gpu.size();
    let bar_rows = match (find.open, find.replace_mode) {
        (false, _) => 0,
        (true, false) => 1,
        (true, true) => 2,
    };
    Layout::new(gpu.metrics(), w, h, scale, line_estimate(&doc.buffer))
        .with_chrome(true, bar_rows, message)
}

/// Everything one frame shows.
pub struct Scene<'a> {
    doc: &'a Document,
    view: &'a mut View,
    theme: &'a mut Theme,
    find: &'a FindBar,
    message: Option<&'a Message>,
    /// Blink phase: the cursor is in its visible half.
    cursor_visible: bool,
    preedit: Option<&'a Preedit>,
    tab_width: usize,
}

/// Queue a whole frame: editor text, then the find bar and message line.
/// Shared by the window and headless rendering.
pub fn paint_scene(gpu: &mut Gpu, layout: &Layout, scene: &mut Scene<'_>) {
    let find = scene.find;
    let current = find.current.and_then(|i| find.matches.get(i)).cloned();
    let ui = scene.theme.ui;
    let syntax = &scene.doc.syntax;
    let captures = match syntax.capture_names() {
        Some((key, names)) => scene.theme.capture_colors(key, names),
        None => &[],
    };
    scene.view.draw(
        gpu,
        scene.doc,
        layout,
        scene.tab_width,
        &Decorations {
            cursor_visible: scene.cursor_visible && !find.focused,
            preedit: scene.preedit,
            // Hide highlights from before an edit until the search reruns.
            matches: if find.open && find.version == scene.doc.version {
                &find.matches
            } else {
                &[]
            },
            current,
            ui: &ui,
            captures,
        },
    );
    if find.open {
        view::draw_find_bar(gpu, &ui, find, layout, scene.cursor_visible);
    }
    if let Some(m) = scene.message {
        view::draw_message(gpu, &ui, &m.text, layout, m.is_error);
    }
}

/// `--headless WxH --screenshot FILE`: render the first frame into an
/// off-screen texture, with no window, and save it. Works with the screen
/// locked or no display attached. Returns the process exit code.
pub fn run_headless(
    options: &Options,
    config: &Config,
    (docs, root): (Vec<Document>, PathBuf),
    size: (u32, u32),
    context: GpuContext,
) -> i32 {
    let labels: Vec<TabLabel> = docs
        .iter()
        .map(|d| TabLabel {
            name: d.display_name(),
            dirty: false,
        })
        .collect();
    let mut doc = docs.into_iter().next().unwrap_or_else(Document::untitled);
    let Some(path) = &options.screenshot else {
        print_err("nimbus: --headless needs --screenshot FILE.bmp");
        return 2;
    };
    let font = match Font::bundled() {
        Ok(f) => f,
        Err(e) => {
            print_err(&format!("nimbus: {e}"));
            return 1;
        }
    };
    // No window to ask: default to a Retina scale, or NIMBUS_SCALE.
    let scale = effective_scale(2.0);
    let mut gpu = Gpu::headless(
        context,
        font,
        to_f32(config.font_size * scale),
        size.0,
        size.1,
    );
    gpu.set_tab_width(config.tab_width);
    let mut view = View::default();
    view.set_wrap(options.wrap || config.soft_wrap);
    if options.wait_index {
        index_now(&mut doc);
    }
    let find = headless_find(options.find.as_deref(), &mut doc);
    let (sidebar, layout) = headless_sidebar(
        &root,
        layout_for(&gpu, to_f32(scale), &doc, &find, false),
        doc.canonical.as_deref(),
    );
    view.ensure_cursor_visible(&doc, &layout, config.tab_width);
    // One frame only: parse here rather than on a worker.
    doc.syntax.parse_now(&doc.buffer);
    for w in nimbus_syntax::registry::take_warnings() {
        print_err(&format!("nimbus: {w}"));
    }
    let (mut theme, warnings) = Theme::load(&config.theme);
    for w in warnings {
        print_err(&format!("nimbus: {w}"));
    }
    let label = TabLabel {
        name: doc.display_name(),
        dirty: false,
    };
    let mut scene = Scene {
        doc: &doc,
        view: &mut view,
        theme: &mut theme,
        find: &find,
        message: None,
        cursor_visible: true,
        preedit: None,
        tab_width: config.tab_width,
    };
    paint_scene(&mut gpu, &layout, &mut scene);
    let labels = if labels.is_empty() {
        vec![label]
    } else {
        labels
    };
    view::draw_tabs(&mut gpu, &theme.ui, &layout, &labels, 0);
    sidebar::draw(
        &mut gpu,
        &theme.ui,
        &layout,
        &sidebar,
        doc.canonical.as_deref(),
    );
    if let Some(spec) = &options.picker {
        // "palette" or "files", optionally followed by ":query".
        let (kind, query) = spec.split_once(':').unwrap_or((spec.as_str(), ""));
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        let mut p = match kind {
            "files" => Picker::files(&root, notify),
            "line" => Picker::go_to_line(notify, Some(doc.buffer.len_lines())),
            _ => Picker::palette(notify),
        };
        let _ = p.input.insert(query);
        // Let the walk and matcher finish, up to two seconds.
        let deadline = Instant::now() + Duration::from_secs(2);
        while p.tick() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let (keymap, _) = Keymap::new(&config.keys);
        draw_picker_overlay(&mut gpu, &theme.ui, &layout, &p, &keymap, true);
    }
    gpu.request_capture();
    if let Err(e) = gpu.render() {
        print_err(&format!("nimbus: {e}"));
        return 1;
    }
    i32::from(!save_screenshot(path, gpu.take_capture().as_ref()))
}

/// `--wait-index`: count all of a huge file's lines on this thread.
fn index_now(doc: &mut Document) {
    if let Some(job) = doc.indexing.take() {
        let started = Instant::now();
        job.run(|token, chunk| doc.merge_index(token, chunk));
        log::info!(
            "indexed {} lines in {:?}",
            doc.buffer.len_lines(),
            started.elapsed()
        );
    }
}

/// `--find PATTERN`: the find bar searching, with the first match selected.
fn headless_find(pattern: Option<&str>, doc: &mut Document) -> FindBar {
    let mut find = FindBar::default();
    if let Some(pattern) = pattern {
        find.open(false, None);
        let _ = find.find.insert(pattern);
        find.search_now(doc);
        if let Some(range) = find.select_from(0, false) {
            doc.select(range);
        }
    }
    find
}

/// The sidebar as the window would show it, listed here in place, and the
/// layout with room for it.
fn headless_sidebar(root: &Path, layout: Layout, file: Option<&Path>) -> (Sidebar, Layout) {
    let state = State::load();
    let mut sidebar = Sidebar::new(
        io::canonical(root),
        state.sidebar_visible,
        state.sidebar_width,
    );
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
    (sidebar, layout)
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
        (Kind::Line, _) => "",
    };
    let more = if p.loading() { "…" } else { "" };
    let status = if p.kind == Kind::Line {
        String::new()
    } else {
        format!("{n} {noun}{more}")
    };
    let placeholder = match p.kind {
        Kind::Palette => "Type a command",
        Kind::Files => "Type a file name",
        Kind::Line => "Type a line number",
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
    field: &mut Document,
    cmd: Command<'_>,
    paste: Option<&str>,
    tab_width: usize,
) -> Option<String> {
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
            AppEvent::Syntax(result) => {
                let Some(i) = self
                    .tabs
                    .iter()
                    .position(|t| t.doc.syntax.waiting_for(result.id()))
                else {
                    return; // the tab closed, or its file was reloaded
                };
                let doc = &mut self.tabs[i].doc;
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
                let layout = self.layout();
                self.sidebar.after_change(layout.as_ref());
                self.start_listings(requests);
                self.sync_watch();
                self.changed(false);
            }
            AppEvent::Disk(paths) => self.disk_changed(&paths),
            AppEvent::Index(token, chunk) => self.index_arrived(token, chunk),
            AppEvent::WatchWarning(warning) => {
                self.say(warning, true);
                self.changed(false);
            }
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
        match Gpu::new(&window, context, font, self.font_px(scale)) {
            Ok(mut gpu) => {
                gpu.set_tab_width(self.tab_width());
                self.gpu = Some(gpu);
            }
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
            let _ = self.find.find.insert(&pattern);
            self.search(true);
        }
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, _cause: StartCause) {
        let now = Instant::now();
        let blink_changed = self.blink.tick(now);
        let retry_due = self.hidden_retry.is_some_and(|(at, _)| now >= at);
        if self.research_at.is_some_and(|t| now >= t) {
            self.research_at = None;
            if self.find.open {
                self.search(false);
                self.changed(false);
            }
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
        if (blink_changed || retry_due)
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
        if self.frames > 0 && !self.watching {
            // Starting watches can take milliseconds (macOS restarts its
            // event stream for each folder), so not before the first frame.
            self.watching = true;
            self.sync_watch();
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
            self.message.as_ref().map(|m| m.until),
            self.research_at,
            self.recheck_at,
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

            WindowEvent::KeyboardInput { event, .. } => {
                let cmd = input::resolve(
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
                );
                if let Some(cmd) = cmd {
                    self.blink.reset(now);
                    self.latency.input(now);
                    self.run(cmd, event_loop);
                }
            }

            WindowEvent::Ime(ime) => self.ime(ime, event_loop),

            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = position;
                if self.sidebar.resizing {
                    if let Some(layout) = self.layout() {
                        self.sidebar.drag_to(&layout, to_f32(position.x));
                        self.changed(false);
                    }
                } else if self.clicks.dragging {
                    self.mouse_drag();
                }
                self.update_cursor_icon();
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
                    self.clicks.dragging = false;
                    if self.sidebar.resizing {
                        self.sidebar.resizing = false;
                        self.remember();
                    }
                }
            }

            WindowEvent::MouseWheel { delta, .. } => self.mouse_wheel(delta),

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
