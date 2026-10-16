//! Nimbus: a fast, lightweight text editor.
//!
//! Milestone 2: open a file, move around it, type, scroll, and save.
//!
//! `winit` drives the program through an [`ApplicationHandler`]: we hand it a
//! struct, and it calls back into us when the OS has events. Windows may only
//! be created once the app is `resumed`, which is why creation lives there and
//! not in `main`. Nothing redraws on a timer except the cursor blink, so an
//! idle editor uses no CPU.

mod bmp;
mod document;
mod find;
mod input;
mod io;
mod view;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use nimbus_render::{Font, FrameStatus, Gpu, GpuContext, GpuError};

use nimbus_core::Text;

use crate::document::Document;
use crate::find::{Field, FindBar, SearchBatch};
use crate::input::{Command, Motion};
use crate::view::{Decorations, Layout, Preedit, View};

/// Events sent to the event loop from other threads.
#[derive(Debug)]
enum AppEvent {
    /// Results from a background search.
    Search(SearchBatch),
}

/// A short note at the bottom of the window that disappears by itself.
struct Message {
    text: String,
    is_error: bool,
    until: Instant,
}

/// How long a message stays on screen.
const MESSAGE_TIME: Duration = Duration::from_secs(4);

/// Editor font size in logical points. Multiplied by the window's scale
/// factor to get the pixel size glyphs are rasterized at. Comes from config
/// in M4.
const FONT_SIZE_PT: f64 = 14.0;

/// Columns per tab stop. Shared by cursor movement and the renderer, which
/// must agree or the cursor drifts from the text. Comes from config in M4.
const TAB_WIDTH: usize = 4;

/// Command-line options.
struct Options {
    /// Exit right after the first frame is presented, printing time-to-first-frame.
    /// Used by CI and by `hyperfine` to track the startup budget (DESIGN.md §1).
    smoke: bool,
    /// Save the first frame as a BMP image at this path, then exit. A
    /// debugging aid for rendering work, since the OS may not allow external
    /// screenshots of the window.
    screenshot: Option<PathBuf>,
    /// The file to open.
    file: Option<PathBuf>,
    /// Type this many characters, one per frame, through the normal editing
    /// path, then print keystroke-to-present latency and exit. Measures
    /// typing cost without needing a person at the keyboard.
    bench_typing: usize,
    /// Open with the find bar searching for this pattern.
    find: Option<String>,
}

/// Write a line to stdout. Command-line output such as `--version` and the
/// `--smoke` timing is the program's job, so it is not logging.
#[expect(clippy::print_stdout, reason = "CLI output, not diagnostics")]
fn print_out(line: &str) {
    println!("{line}");
}

/// Write a line to stderr, for errors that happen before logging matters or
/// that the user must see even with logging filtered out.
#[expect(clippy::print_stderr, reason = "CLI errors shown to the user")]
fn print_err(line: &str) {
    eprintln!("{line}");
}

/// The scale factor to render at: the window's, unless a debug build sets
/// `NIMBUS_SCALE` to test another density on one display (winit's own
/// override only works on X11).
fn effective_scale(window_scale: f64) -> f64 {
    let overridden = std::env::var("NIMBUS_SCALE")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|s| *s > 0.0 && *s <= 8.0);
    match overridden {
        Some(s) if cfg!(debug_assertions) => s,
        _ => window_scale,
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "font sizes and scale factors are small; the renderer takes f32"
)]
fn to_f32(v: f64) -> f32 {
    v as f32
}

fn font_px(scale: f64) -> f32 {
    to_f32(FONT_SIZE_PT * scale)
}

fn parse_options() -> Options {
    let mut smoke = false;
    let mut screenshot = None;
    let mut file = None;
    let mut bench_typing = 0;
    let mut find = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--smoke") => smoke = true,
            Some("--screenshot") => {
                let Some(path) = args.next() else {
                    print_err("nimbus: --screenshot needs a file path");
                    std::process::exit(2);
                };
                screenshot = Some(PathBuf::from(path));
            }
            Some("--bench-typing") => {
                bench_typing = args
                    .next()
                    .and_then(|n| n.to_str().and_then(|n| n.parse().ok()))
                    .unwrap_or_else(|| {
                        print_err("nimbus: --bench-typing needs a count");
                        std::process::exit(2);
                    });
            }
            Some("--find") => {
                let Some(pattern) = args.next().and_then(|p| p.into_string().ok()) else {
                    print_err("nimbus: --find needs a pattern");
                    std::process::exit(2);
                };
                find = Some(pattern);
            }
            Some("--version" | "-V") => {
                print_out(&format!("nimbus {}", nimbus_core::VERSION));
                std::process::exit(0);
            }
            Some("--help" | "-h") => {
                print_out(
                    "usage: nimbus [FILE] [--find PATTERN] [--smoke] [--screenshot FILE.bmp] [--bench-typing N] [--version]",
                );
                std::process::exit(0);
            }
            Some(other) if other.starts_with("--") => {
                print_err(&format!("nimbus: unknown option {other:?}"));
                std::process::exit(2);
            }
            _ if file.is_none() => file = Some(PathBuf::from(arg)),
            _ => {
                print_err("nimbus: only one file can be opened until tabs arrive in M4");
                std::process::exit(2);
            }
        }
    }
    Options {
        smoke,
        screenshot,
        file,
        bench_typing,
        find,
    }
}

/// Write a captured frame to `path` as BMP. Returns whether it was saved;
/// failures are reported on stderr.
fn save_screenshot(path: &Path, capture: Option<&nimbus_render::Capture>) -> bool {
    let Some(capture) = capture else {
        print_err("nimbus: no frame was captured");
        return false;
    };
    let Some(bytes) = bmp::encode(capture.width, capture.height, &capture.rgba) else {
        print_err("nimbus: captured frame has inconsistent size");
        return false;
    };
    match std::fs::write(path, bytes) {
        Ok(()) => {
            print_out(&format!(
                "screenshot {}x{} saved to {}",
                capture.width,
                capture.height,
                path.display()
            ));
            true
        }
        Err(e) => {
            print_err(&format!("nimbus: could not write {}: {e}", path.display()));
            false
        }
    }
}

/// Cursor blink timing.
///
/// The cursor shows solid on any input and toggles every 530 ms after that.
/// After 10 s without input it stays solid and stops scheduling wakeups, so
/// an idle editor costs no CPU at all.
struct Blink {
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

struct App {
    options: Options,
    started: Instant,
    /// GPU setup running on a background thread since the start of `main`.
    /// Taken (left `None`) when the window joins it in `resumed`.
    gpu_init: Option<JoinHandle<Result<GpuContext, GpuError>>>,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    /// Modifier keys currently held, tracked from `ModifiersChanged`.
    modifiers: ModifiersState,
    /// Process exit status once the event loop ends; non-zero when a
    /// requested `--screenshot` could not be saved.
    exit_code: i32,
    frames: u64,
    doc: Document,
    view: View,
    blink: Blink,
    preedit: Option<Preedit>,
    latency: Latency,
    /// Last mouse position in physical pixels; clicks carry no position.
    mouse: PhysicalPosition<f64>,
    /// Last rectangle sent to the IME, to avoid resending it every frame.
    ime_area: Option<[i32; 4]>,
    /// Last window title set, to avoid resetting it every frame.
    title_dirty: Option<bool>,
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
    message: Option<Message>,
    proxy: EventLoopProxy<AppEvent>,
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
    fn new(
        options: Options,
        started: Instant,
        gpu_init: JoinHandle<Result<GpuContext, GpuError>>,
        doc: Document,
        proxy: EventLoopProxy<AppEvent>,
    ) -> Self {
        Self {
            options,
            started,
            gpu_init: Some(gpu_init),
            window: None,
            gpu: None,
            modifiers: ModifiersState::empty(),
            exit_code: 0,
            frames: 0,
            doc,
            view: View::default(),
            blink: Blink::new(started),
            preedit: None,
            latency: Latency::default(),
            mouse: PhysicalPosition::new(0.0, 0.0),
            ime_area: None,
            title_dirty: None,
            hidden_retry: None,
            clipboard: None,
            clicks: Clicks::default(),
            find: FindBar::default(),
            find_origin: 0,
            find_pending_jump: false,
            message: None,
            proxy,
        }
    }

    /// Geometry for the current window, if it exists.
    fn layout(&self) -> Option<Layout> {
        let (window, gpu) = (self.window.as_ref()?, self.gpu.as_ref()?);
        let (w, h) = gpu.size();
        let scale = to_f32(effective_scale(window.scale_factor()));
        let bar_rows = match (self.find.open, self.find.replace_mode) {
            (false, _) => 0,
            (true, false) => 1,
            (true, true) => 2,
        };
        let message_rows = usize::from(self.message.is_some());
        Some(
            Layout::new(gpu.metrics(), w, h, scale, self.doc.buffer.len_lines())
                .with_bars(bar_rows, message_rows),
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
        // Only the dirty marker changes while editing, so rebuild the title
        // only when it flips, not on every keystroke.
        let is_dirty = self.doc.is_dirty();
        if self.title_dirty == Some(is_dirty) {
            return;
        }
        let dirty = if is_dirty { "● " } else { "" };
        window.set_title(&format!("{dirty}{} — Nimbus", self.doc.display_name()));
        self.title_dirty = Some(is_dirty);
    }

    /// Something changed what is on screen.
    fn changed(&mut self, keep_cursor_visible: bool) {
        if keep_cursor_visible && let Some(layout) = self.layout() {
            self.view
                .ensure_cursor_visible(&self.doc, &layout, TAB_WIDTH);
        }
        self.update_title();
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn run(&mut self, cmd: Command<'_>, event_loop: &ActiveEventLoop) {
        if self.find.focused && self.run_find(cmd) {
            self.changed(false);
            return;
        }
        let page = self.layout().map_or(1, |l| l.full_lines());
        let result = match cmd {
            Command::Move(motion, extend) => {
                self.doc.move_cursor(motion, extend, TAB_WIDTH, page);
                if matches!(motion, Motion::PageUp | Motion::PageDown) {
                    // Scroll by the same amount so the cursor keeps its row.
                    if let Some(layout) = self.layout() {
                        let lh = layout.metrics.line_height;
                        let dir = if motion == Motion::PageUp { -1.0 } else { 1.0 };
                        #[expect(
                            clippy::cast_precision_loss,
                            reason = "page is a few hundred lines"
                        )]
                        let dy = dir * page as f32 * lh;
                        self.view
                            .scroll_by(dy, &layout, self.doc.buffer.len_lines());
                    }
                }
                Ok(())
            }
            Command::Insert(text) => self.doc.insert(text).map(drop),
            Command::Newline => self.doc.insert_newline().map(drop),
            Command::Tab => match self.doc.indent_lines() {
                Ok(true) => Ok(()),
                Ok(false) => self.doc.insert("\t").map(drop),
                Err(e) => Err(e),
            },
            Command::Outdent => self.doc.outdent_lines(TAB_WIDTH),
            Command::Undo => self.doc.undo().map(drop),
            Command::Redo => self.doc.redo().map(drop),
            Command::SelectAll => {
                self.doc.select_all();
                Ok(())
            }
            Command::Copy | Command::Cut => {
                if let Some(text) = self.doc.selected_text() {
                    self.clipboard_set(text);
                    if cmd == Command::Cut {
                        self.doc.history.seal();
                        let r = self.doc.delete_back().map(drop);
                        self.doc.history.seal();
                        r
                    } else {
                        Ok(())
                    }
                } else {
                    Ok(())
                }
            }
            Command::Paste => match self.clipboard_get() {
                Some(text) => self.doc.paste(&text).map(drop),
                None => Ok(()),
            },
            Command::Escape => {
                if self.find.open {
                    self.find.close();
                } else {
                    let head = self.doc.selection().head;
                    self.doc.click(head, false);
                }
                Ok(())
            }
            Command::Find | Command::Replace => {
                self.open_find(cmd == Command::Replace);
                Ok(())
            }
            Command::ToggleCase | Command::ToggleRegex | Command::ReplaceAll => {
                if self.find.open {
                    self.run_find(cmd);
                }
                Ok(())
            }
            // Arrives later in M3.
            Command::ToggleWrap => Ok(()),
            Command::DeleteBack => self.doc.delete_back().map(drop),
            Command::DeleteForward => self.doc.delete_forward().map(drop),
            Command::Save => {
                self.save();
                Ok(())
            }
            Command::Close => {
                self.close(event_loop);
                return;
            }
        };
        if let Err(e) = result {
            // A bug, not a user error; the document is unchanged.
            log::error!("edit refused: {e}");
        }
        // Edits invalidate search results; search again, without jumping.
        if self.find.open && self.doc.version != self.find.version {
            self.search(false);
        }
        // Saving changes only the title, so it must not pull the view back
        // to the cursor.
        self.changed(!matches!(
            cmd,
            Command::Save | Command::Find | Command::Replace
        ));
    }

    fn open_find(&mut self, replace_mode: bool) {
        let selection = self.doc.selected_text();
        self.find.open(replace_mode, selection);
        self.find_origin = self.doc.selection().range().start;
        self.search(true);
    }

    /// Search the document for the current query. With `jump`, select the
    /// first match from where the search started, as incremental search does.
    fn search(&mut self, jump: bool) {
        let proxy = self.proxy.clone();
        let complete = self.find.search(&self.doc, move |b| {
            proxy.send_event(AppEvent::Search(b)).is_ok()
        });
        self.find_pending_jump = jump && !complete;
        if jump && complete {
            self.jump_to_match(self.find_origin, false);
        }
    }

    /// Select the match at or after `from` (or before, `backwards`).
    fn jump_to_match(&mut self, from: usize, backwards: bool) {
        if let Some(range) = self.find.select_from(from, backwards) {
            self.doc.select(range);
            if let Some(layout) = self.layout() {
                self.view
                    .ensure_cursor_visible(&self.doc, &layout, TAB_WIDTH);
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
                let sel = self.doc.selection().range();
                let from = if backwards {
                    sel.start
                } else {
                    sel.end.max(sel.start + 1)
                };
                self.jump_to_match(from, backwards);
            }
            Command::ReplaceAll => self.replace_all(),
            Command::ToggleCase => {
                self.find.case_sensitive = !self.find.case_sensitive;
                self.search(true);
            }
            Command::ToggleRegex => {
                self.find.regex = !self.find.regex;
                self.search(true);
            }
            Command::Save | Command::Close | Command::ToggleWrap => return false,
            _ => {
                let paste = (cmd == Command::Paste)
                    .then(|| self.clipboard_get())
                    .flatten();
                let field = self.find.field_mut();
                let before = field.version;
                let copied = edit_field(field, cmd, paste.as_deref());
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
        let sel = self.doc.selection().range();
        let is_match = self.find.current.and_then(|i| self.find.matches.get(i)) == Some(&sel);
        if is_match && !sel.is_empty() {
            let matched = self.doc.buffer.slice(sel.clone()).into_owned();
            let replacement = self.find.replacement_for(&matched);
            if let Err(e) = self.doc.replace(sel, &replacement) {
                log::error!("replace refused: {e}");
            }
            self.doc.history.seal();
            self.search(false);
        }
        let from = self.doc.cursor();
        self.jump_to_match(from, false);
    }

    /// Replace every match as one undo step.
    fn replace_all(&mut self) {
        if !self.find.complete {
            // A background search is still running: finish it here so no
            // match is missed.
            self.find.search_now(&self.doc);
        }
        let items: Vec<(std::ops::Range<usize>, String)> = self
            .find
            .matches
            .iter()
            .map(|r| {
                let matched = self.doc.buffer.slice(r.clone());
                (r.clone(), self.find.replacement_for(&matched))
            })
            .collect();
        match self.doc.replace_many(&items) {
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
        if self.find.open && y < layout.top {
            // A click in the bar focuses the field on that row.
            let row_h = layout.metrics.line_height + layout.margin;
            let second = self.find.replace_mode && y >= layout.margin + row_h;
            self.find.focused = true;
            self.find.focus = if second { Field::Replace } else { Field::Find };
            self.changed(false);
            return;
        }
        self.find.focused = false;
        let pos = self.view.hit(x, y, &self.doc, &layout, TAB_WIDTH);
        let count = self.clicks.press(now, (x, y), layout.metrics.cell_width);
        let t = &self.doc.buffer;
        match count {
            1 => {
                self.doc.click(pos, self.modifiers.shift_key());
                let head = self.doc.cursor();
                self.clicks.anchor = self.doc.selection().anchor..head;
                if !self.modifiers.shift_key() {
                    self.clicks.anchor = head..head;
                }
            }
            2 => {
                let word = nimbus_core::movement::word_at(t, pos);
                self.clicks.anchor = word.clone();
                self.doc.select(word);
            }
            _ => {
                let line = nimbus_core::movement::line_range(t, pos);
                self.clicks.anchor = line.clone();
                self.doc.select(line);
            }
        }
        self.clicks.dragging = true;
        self.blink.reset(now);
        self.changed(false);
    }

    /// The mouse moved with the button held: extend the selection by the
    /// unit of the click that started it (character, word or line).
    fn mouse_drag(&mut self) {
        let Some(layout) = self.layout() else { return };
        let (x, y) = (to_f32(self.mouse.x), to_f32(self.mouse.y));
        let pos = self.view.hit(x, y, &self.doc, &layout, TAB_WIDTH);
        let t = &self.doc.buffer;
        let unit = match self.clicks.count {
            1 => pos..pos,
            2 => nimbus_core::movement::word_at(t, pos),
            _ => nimbus_core::movement::line_range(t, pos),
        };
        let anchor = self.clicks.anchor.clone();
        if unit.start < anchor.start {
            self.doc.select_span(anchor.end, unit.start);
        } else {
            self.doc.select_span(anchor.start, unit.end.max(anchor.end));
        }
        self.changed(true);
    }

    fn save(&mut self) -> bool {
        match self.doc.save() {
            Ok(()) => {
                let name = self.doc.display_name();
                log::info!("saved {name}");
                self.update_title();
                self.say(format!("Saved {name}"), false);
                true
            }
            Err(e) => {
                log::error!("{e}");
                let mut dialog = MessageDialog::new()
                    .set_level(MessageLevel::Error)
                    .set_title("Could not save")
                    .set_description(e.to_string())
                    .set_buttons(MessageButtons::Ok);
                if let Some(window) = &self.window {
                    dialog = dialog.set_parent(window.as_ref());
                }
                dialog.show();
                false
            }
        }
    }

    /// Close the window, first offering to save unsaved changes.
    fn close(&mut self, event_loop: &ActiveEventLoop) {
        if self.doc.is_dirty() && !self.options.smoke && self.options.screenshot.is_none() {
            let name = self.doc.display_name();
            let mut dialog = MessageDialog::new()
                .set_level(MessageLevel::Warning)
                .set_title("Unsaved changes")
                .set_description(format!("Save changes to {name} before closing?"))
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
                _ => return, // Cancel or dialog dismissed: keep editing.
            };
            if save && !self.save() {
                return; // Save failed; the error was shown. Keep editing.
            }
        }
        event_loop.exit();
    }

    /// Queue the whole frame: editor text, then the overlays.
    fn paint(&mut self, gpu: &mut Gpu, layout: &Layout) {
        let current = self
            .find
            .current
            .and_then(|i| self.find.matches.get(i))
            .cloned();
        self.view.draw(
            gpu,
            &self.doc,
            layout,
            TAB_WIDTH,
            Decorations {
                cursor_visible: self.blink.visible && !self.find.focused,
                preedit: self.preedit.as_ref(),
                matches: if self.find.open {
                    &self.find.matches
                } else {
                    &[]
                },
                current,
            },
        );
        if self.find.open {
            view::draw_find_bar(gpu, &self.find, layout, self.blink.visible);
        }
        if let Some(m) = &self.message {
            view::draw_message(gpu, &m.text, layout, m.is_error);
        }
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(layout) = self.layout() else { return };
        // Painting reads the whole app; take the GPU out while it does.
        if let Some(mut gpu) = self.gpu.take() {
            self.paint(&mut gpu, &layout);
            self.gpu = Some(gpu);
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
        let r = self
            .view
            .cursor_rect(&self.doc, &layout, TAB_WIDTH, self.preedit.as_ref());
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

/// Apply a text-editing command to a one-line field (the find bar). Returns
/// text to put on the clipboard for Copy and Cut. Line breaks are not typed
/// into fields; pasted ones are kept, so a multi-line literal can be found.
fn edit_field(field: &mut Document, cmd: Command<'_>, paste: Option<&str>) -> Option<String> {
    let result = match cmd {
        Command::Move(motion, extend) => {
            field.move_cursor(motion, extend, TAB_WIDTH, 1);
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
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::Search(batch) => {
                if !self.find.receive(batch) {
                    return;
                }
                if self.find_pending_jump && !self.find.matches.is_empty() {
                    self.find_pending_jump = false;
                    self.jump_to_match(self.find_origin, false);
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
            .with_title(format!("{} — Nimbus", self.doc.display_name()))
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
        match Gpu::new(&window, context, font, font_px(scale)) {
            Ok(mut gpu) => {
                gpu.set_tab_width(TAB_WIDTH);
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
        // Sleep until the next blink or hidden-window retry, or indefinitely
        // when neither is pending.
        let next = [
            self.blink.next,
            self.hidden_retry.map(|(at, _)| at),
            self.message.as_ref().map(|m| m.until),
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
            WindowEvent::CloseRequested => self.close(event_loop),

            WindowEvent::ModifiersChanged(mods) => self.modifiers = mods.state(),

            WindowEvent::KeyboardInput { event, .. } => {
                let cmd = input::map_key(input::KeyPress {
                    key: &event.logical_key,
                    physical: event.physical_key,
                    text: event.text.as_deref(),
                    pressed: event.state == ElementState::Pressed,
                    repeat: event.repeat,
                    mods: self.modifiers,
                });
                if let Some(cmd) = cmd {
                    self.blink.reset(now);
                    self.latency.input(now);
                    self.run(cmd, event_loop);
                }
            }

            WindowEvent::Ime(ime) => self.ime(ime, event_loop),

            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = position;
                if self.clicks.dragging {
                    self.mouse_drag();
                }
            }

            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                if state == ElementState::Pressed {
                    self.mouse_down(now);
                } else {
                    self.clicks.dragging = false;
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(layout) = self.layout() {
                    let lh = layout.metrics.line_height;
                    let cell = layout.metrics.cell_width;
                    // Trackpads send pixels, with momentum from the OS; wheels
                    // send lines, already accelerated on macOS.
                    let lines_per_notch = if cfg!(target_os = "macos") { 1.0 } else { 3.0 };
                    let (dx, dy) = match delta {
                        MouseScrollDelta::PixelDelta(p) => (to_f32(p.x), to_f32(p.y)),
                        MouseScrollDelta::LineDelta(x, y) => {
                            (x * cell * lines_per_notch, y * lh * lines_per_notch)
                        }
                    };
                    self.view
                        .scroll_by(-dy, &layout, self.doc.buffer.len_lines());
                    self.view.scroll_x_by(-dx);
                    self.changed(false);
                }
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
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.set_font_size(font_px(effective_scale(scale_factor)));
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

fn main() {
    let started = Instant::now();
    // Logging and argument parsing take microseconds. They come first so the
    // GPU thread's log lines are not lost and `--version` never wakes the GPU.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("nimbus=info"))
        .format_timestamp_millis()
        .init();
    let options = parse_options();

    // Start opening the GPU next, so the driver work overlaps reading the
    // file, event-loop setup and window creation (DESIGN.md §4.3).
    let gpu_init = match GpuContext::spawn() {
        Ok(handle) => handle,
        Err(e) => {
            print_err(&format!("nimbus: could not start GPU thread: {e}"));
            std::process::exit(1);
        }
    };

    let doc = match &options.file {
        None => Document::untitled(),
        Some(path) => match Document::open(path) {
            Ok(doc) => doc,
            Err(e) => {
                let msg = format!("Cannot open {}: {e}", path.display());
                print_err(&format!("nimbus: {msg}"));
                // Launched from a file manager there is no terminal, so also
                // show the reason in a dialog.
                if !options.smoke && options.screenshot.is_none() {
                    MessageDialog::new()
                        .set_level(MessageLevel::Error)
                        .set_title("Nimbus")
                        .set_description(msg)
                        .set_buttons(MessageButtons::Ok)
                        .show();
                }
                std::process::exit(1);
            }
        },
    };
    log::debug!("startup: document loaded at {:?}", started.elapsed());

    let mut builder = EventLoop::<AppEvent>::with_user_event();
    #[cfg(target_os = "macos")]
    {
        // winit's default macOS menu binds Cmd+Q to quit the app directly,
        // which would skip the unsaved-changes prompt. Our key handling does
        // Cmd+Q instead; a real menu arrives in M4.
        use winit::platform::macos::EventLoopBuilderExtMacOS;
        builder.with_default_menu(false);
    }
    let event_loop = match builder.build() {
        Ok(el) => el,
        Err(e) => {
            print_err(&format!("nimbus: could not start event loop: {e}"));
            std::process::exit(1);
        }
    };
    event_loop.set_control_flow(ControlFlow::Wait);
    log::debug!("startup: event loop ready at {:?}", started.elapsed());

    let proxy = event_loop.create_proxy();
    let mut app = App::new(options, started, gpu_init, doc, proxy);
    if let Err(e) = event_loop.run_app(&mut app) {
        print_err(&format!("nimbus: event loop error: {e}"));
        std::process::exit(1);
    }
    if app.exit_code != 0 {
        std::process::exit(app.exit_code);
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
