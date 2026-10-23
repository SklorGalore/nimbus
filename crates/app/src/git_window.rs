//! The git window (M27): `nimbus --git [DIR]` opens a window laid out for
//! git instead of the editor, in a process of its own.
//!
//! It is the same `nimbus` binary and shares the editor's parts: the
//! renderer and compatibility painter, the config, theme, fonts and key
//! bindings, the compare view with its diff options and highlighting.
//! What it adds is a model of the repository (`pane.rs`), the history
//! with its graph (`history.rs`), and the workers that run git
//! (`load.rs`). M27 only reads: nothing here changes the repository.
//!
//! **One process per window.** The editor is one window per process
//! (DESIGN.md §2); the git window keeps that rule by being a separate
//! process, started from the command line or by the editor's "Open Git
//! Window". It never answers later `nimbus` commands' hand-offs; it hands
//! files *to* the editor through them (Open in Editor).
//!
//! **No git before the first frame.** The window and its first frame come
//! first ("Opening the repository…"); finding the repository, the status
//! and the history all start afterwards on workers, so the startup budget
//! holds however large the repository is.

mod history;
mod load;
mod pane;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{CursorIcon, Window, WindowId};

use nimbus_render::{Font, FrameStatus, Gpu, GpuContext, GpuError};

use crate::config::Config;
use crate::input::{self, Command, Resolved};
use crate::keymap::Keymap;
use crate::languages::Languages;
use crate::state::State;
use crate::theme::Theme;
use crate::watch::Watch;
use crate::{Options, effective_scale, print_err, print_out, save_screenshot, to_f32};

use load::Send;
pub use load::{GitEvent, start_nimbus};
use pane::GitWindow;
use pane::draw::Outcome;

/// How long after a change on disk the window reads the status again:
/// a commit or checkout touches many files, and one read should follow
/// the last of them.
const SETTLE: Duration = Duration::from_millis(300);

/// How long a message stays in the status bar.
const MESSAGE_TIME: Duration = Duration::from_secs(4);

/// Two clicks this close together are a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// The folder `--git` opens: the one named after it, or the current one.
#[must_use]
pub fn folder(options: &Options) -> PathBuf {
    options
        .files
        .first()
        .cloned()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Run the git window until it closes. Returns the exit code.
pub fn run(
    options: Options,
    started: Instant,
    gpu_init: Option<JoinHandle<Result<GpuContext, GpuError>>>,
    (config, languages): (Config, Languages),
    warnings: &[String],
) -> i32 {
    let event_loop = match EventLoop::<GitEvent>::with_user_event().build() {
        Ok(el) => el,
        Err(e) => {
            print_err(&format!("nimbus: could not start event loop: {e}"));
            return 1;
        }
    };
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let mut app = GitApp::new(options, started, gpu_init, (config, languages), proxy);
    if !warnings.is_empty() {
        app.say(warnings.join("; "), true);
    }
    if let Err(e) = event_loop.run_app(&mut app) {
        print_err(&format!("nimbus: event loop error: {e}"));
        return 1;
    }
    app.exit_code
}

/// A sender that delivers to the event loop.
fn proxy_send(proxy: &EventLoopProxy<GitEvent>) -> Send {
    let proxy = proxy.clone();
    Arc::new(move |e| proxy.send_event(e).is_ok())
}

struct Message {
    text: String,
    is_error: bool,
    until: Instant,
}

/// The window, its painter and the pane model.
struct GitApp {
    options: Options,
    started: Instant,
    gpu_init: Option<JoinHandle<Result<GpuContext, GpuError>>>,
    config: Config,
    keymap: Keymap,
    theme: Theme,
    pane: GitWindow,
    proxy: EventLoopProxy<GitEvent>,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    font_size: f64,
    modifiers: ModifiersState,
    cursor: (f32, f32),
    last_click: Option<(Instant, (f32, f32))>,
    message: Option<Message>,
    frames: u64,
    watch: Option<Watch>,
    refresh_at: Option<Instant>,
    clipboard: Option<arboard::Clipboard>,
    exit_code: i32,
}

impl GitApp {
    fn new(
        options: Options,
        started: Instant,
        gpu_init: Option<JoinHandle<Result<GpuContext, GpuError>>>,
        (config, languages): (Config, Languages),
        proxy: EventLoopProxy<GitEvent>,
    ) -> Self {
        let (keymap, key_warnings) = Keymap::new(&config.keys);
        let (theme, theme_warnings) = Theme::load(&config.theme);
        let send = proxy_send(&proxy);
        let normalize = State::load().diff_options();
        let pane = GitWindow::new(folder(&options), send, Arc::new(languages), normalize);
        let mut app = Self {
            font_size: config.font_size,
            options,
            started,
            gpu_init,
            config,
            keymap,
            theme,
            pane,
            proxy,
            window: None,
            gpu: None,
            modifiers: ModifiersState::empty(),
            cursor: (0.0, 0.0),
            last_click: None,
            message: None,
            frames: 0,
            watch: None,
            refresh_at: None,
            clipboard: None,
            exit_code: 0,
        };
        let warnings: Vec<String> = key_warnings.into_iter().chain(theme_warnings).collect();
        if !warnings.is_empty() {
            app.say(warnings.join("; "), true);
        }
        app
    }

    fn scale(&self) -> f32 {
        to_f32(effective_scale(
            self.window.as_ref().map_or(1.0, |w| w.scale_factor()),
        ))
    }

    fn font_px(&self) -> f32 {
        to_f32(self.font_size * f64::from(self.scale()))
    }

    fn say(&mut self, text: String, is_error: bool) {
        self.message = Some(Message {
            text,
            is_error,
            until: Instant::now() + MESSAGE_TIME,
        });
        self.redraw();
    }

    fn redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn set_title(&self) {
        if let Some(w) = &self.window {
            w.set_title(&self.pane.title());
        }
    }

    /// Start the painter: the GPU when it opened, the CPU otherwise.
    fn start_painter(&mut self, window: &Arc<Window>) -> Result<(), String> {
        let font = Font::bundled().map_err(|e| e.to_string())?;
        let px = self.font_px();
        let gpu = match self.gpu_init.take().map(JoinHandle::join) {
            _ if self.options.compat || self.config.compat => Err(None),
            Some(Ok(Ok(context))) => Gpu::new(window, context, font, px, self.options.linear_blend)
                .map_err(|e| Some(e.to_string())),
            Some(Ok(Err(e))) => Err(Some(e.to_string())),
            Some(Err(_)) | None => Err(Some("GPU setup did not complete".to_owned())),
        };
        let mut gpu = match gpu {
            Ok(g) => g,
            Err(why) => {
                if let Some(why) = why {
                    log::warn!("compatibility mode: {why}");
                }
                Gpu::compat(window, font, px).map_err(|e| e.to_string())?
            }
        };
        gpu.set_tab_width(self.config.tab_width);
        gpu.set_ligatures(self.config.ligatures);
        let proxy = self.proxy.clone();
        gpu.set_font_notify(Arc::new(move || {
            let _ = proxy.send_event(GitEvent::FontLoaded);
        }));
        self.gpu = Some(gpu);
        Ok(())
    }

    fn paint(&mut self) {
        let scale = self.scale();
        let now = Instant::now();
        if self.message.as_ref().is_some_and(|m| now >= m.until) {
            self.message = None;
        }
        let Some(gpu) = self.gpu.as_mut() else { return };
        let message = self.message.as_ref().map(|m| (m.text.as_str(), m.is_error));
        self.pane.draw(gpu, &mut self.theme, scale, message);
    }

    fn present(&mut self, event_loop: &ActiveEventLoop) {
        self.paint();
        let Some(gpu) = self.gpu.as_mut() else { return };
        if self.frames == 0 && self.options.screenshot.is_some() && !gpu.request_capture() {
            print_err("nimbus: this surface cannot be captured");
            self.exit_code = 1;
            event_loop.exit();
            return;
        }
        match gpu.render() {
            Ok(FrameStatus::Presented) => self.frames += 1,
            Ok(FrameStatus::Incomplete) => {
                self.frames += 1;
                self.redraw();
            }
            Ok(FrameStatus::Retry) => {
                self.redraw();
                return;
            }
            Ok(FrameStatus::Hidden) => return,
            Err(e) => {
                log::error!("{e}");
                let lost_gpu = !gpu.compatibility();
                let window = self.window.clone();
                let restarted = lost_gpu
                    && window.is_some_and(|w| {
                        Font::bundled().is_ok_and(|font| {
                            Gpu::compat(&w, font, self.font_px()).is_ok_and(|g| {
                                self.gpu = Some(g);
                                true
                            })
                        })
                    });
                if restarted {
                    self.redraw();
                } else {
                    event_loop.exit();
                }
                return;
            }
        }
        if self.frames == 1 {
            self.first_frame(event_loop);
        }
    }

    /// The first frame is up: report it for `--smoke` and `--screenshot`,
    /// then start everything that touches git or the disk.
    fn first_frame(&mut self, event_loop: &ActiveEventLoop) {
        let elapsed = self.started.elapsed();
        log::info!("git window: first frame in {elapsed:?}");
        if let Some(path) = self.options.screenshot.clone() {
            let capture = self.gpu.as_mut().and_then(Gpu::take_capture);
            if !save_screenshot(&path, capture.as_ref()) {
                self.exit_code = 1;
            }
            event_loop.exit();
            return;
        }
        if self.options.smoke {
            print_out(&format!(
                "first-frame-ms={:.1}",
                elapsed.as_secs_f64() * 1000.0
            ));
            event_loop.exit();
            return;
        }
        load::open(self.pane.dir.clone(), proxy_send(&self.proxy));
        let proxy = self.proxy.clone();
        let warn = self.proxy.clone();
        self.watch = Some(Watch::new(
            move |_| {
                let _ = proxy.send_event(GitEvent::Disk);
            },
            move |w| {
                let _ = warn.send_event(GitEvent::Say(w, true));
            },
        ));
        self.load_fonts();
    }

    /// Look up the configured fonts on a worker, as the editor does.
    fn load_fonts(&self) {
        let (font, ui_font) = (self.config.font.clone(), self.config.ui_font.clone());
        if font.is_empty() && ui_font.is_empty() {
            return;
        }
        let proxy = self.proxy.clone();
        let spawned = std::thread::Builder::new()
            .name("nimbus-font".to_owned())
            .spawn(move || {
                let find = |name: &str| {
                    (!name.is_empty())
                        .then(|| nimbus_render::find_editor_font(name))
                        .flatten()
                };
                let _ = proxy.send_event(GitEvent::Fonts(Box::new((find(&font), find(&ui_font)))));
            });
        if let Err(e) = spawned {
            log::warn!("could not look up fonts: {e}");
        }
    }

    fn sync_watch(&mut self) {
        let wanted = self.pane.watched();
        if let Some(w) = self.watch.as_mut() {
            w.sync(&wanted);
        }
    }

    fn run(&mut self, cmd: Command<'_>, event_loop: &ActiveEventLoop) {
        match self.pane.command(cmd) {
            Outcome::Done(changed) => {
                if changed {
                    self.redraw();
                }
            }
            Outcome::OpenInEditor(path) => self.open_in_editor(path),
            Outcome::Copy(text) => self.copy(text),
            Outcome::Window => self.window_command(cmd, event_loop),
        }
    }

    fn window_command(&mut self, cmd: Command<'_>, event_loop: &ActiveEventLoop) {
        match cmd {
            Command::ZoomIn => self.zoom(self.font_size + 1.0),
            Command::ZoomOut => self.zoom(self.font_size - 1.0),
            Command::ResetZoom => self.zoom(self.config.font_size),
            Command::ToggleTheme => {
                let next = if self.theme.base == "light" {
                    "dark"
                } else {
                    "light"
                };
                if let Some(t) = Theme::built_in(next) {
                    self.theme = t;
                }
                self.redraw();
            }
            Command::Close | Command::CloseTab => event_loop.exit(),
            _ => {}
        }
    }

    fn zoom(&mut self, size: f64) {
        self.font_size = size.clamp(6.0, 72.0);
        let px = self.font_px();
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.set_font_size(px);
        }
        self.pane.relayout();
        self.redraw();
    }

    fn open_in_editor(&mut self, path: PathBuf) {
        load::open_in_editor(path, proxy_send(&self.proxy));
    }

    fn copy(&mut self, text: String) {
        if self.clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(c) => self.clipboard = Some(c),
                Err(e) => log::warn!("clipboard unavailable: {e}"),
            }
        }
        let shown = text.chars().take(40).collect::<String>();
        if let Some(c) = self.clipboard.as_mut()
            && c.set_text(text).is_ok()
        {
            self.say(format!("Copied {shown}"), false);
        }
    }

    fn mouse_down(&mut self) {
        let now = Instant::now();
        let (x, y) = self.cursor;
        let double = self.last_click.is_some_and(|(at, (px, py))| {
            now.duration_since(at) < DOUBLE_CLICK && (px - x).abs() < 4.0 && (py - y).abs() < 4.0
        });
        self.last_click = if double { None } else { Some((now, (x, y))) };
        match self.pane.click(x, y, double) {
            Outcome::OpenInEditor(p) => self.open_in_editor(p),
            Outcome::Copy(t) => self.copy(t),
            Outcome::Done(true) => self.redraw(),
            Outcome::Done(false) | Outcome::Window => {}
        }
    }

    fn cursor_moved(&mut self, position: PhysicalPosition<f64>) {
        self.cursor = (to_f32(position.x), to_f32(position.y));
        let (x, y) = self.cursor;
        let scale = self.scale();
        if let Some(gpu) = &self.gpu
            && self.pane.drag(gpu, scale, x, y)
        {
            self.redraw();
        }
        if let Some(w) = &self.window {
            let icon = match self.pane.dragging.or_else(|| self.pane.on_divider(x, y)) {
                Some(0 | 1) => CursorIcon::ColResize,
                Some(_) => CursorIcon::RowResize,
                None => CursorIcon::Default,
            };
            w.set_cursor(icon);
        }
    }

    fn wheel(&mut self, delta: MouseScrollDelta) {
        let line = self.gpu.as_ref().map_or(16.0, |g| g.metrics().line_height);
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => -y * 3.0 * line,
            MouseScrollDelta::PixelDelta(p) => -to_f32(p.y),
        };
        let scale = self.scale();
        if let Some(gpu) = &self.gpu
            && self.pane.wheel(gpu, scale, self.cursor, dy)
        {
            self.redraw();
        }
    }

    fn key(&mut self, event: &winit::event::KeyEvent, event_loop: &ActiveEventLoop) {
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
            false,
            None,
        );
        if let Resolved::Command(cmd) = resolved {
            self.run(cmd, event_loop);
        }
    }
}

impl ApplicationHandler<GitEvent> for GitApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(self.pane.title())
            .with_inner_size(LogicalSize::new(1200.0, 760.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                crate::fatal(&self.options, &format!("Could not create a window: {e}"));
                self.exit_code = 1;
                event_loop.exit();
                return;
            }
        };
        self.window = Some(Arc::clone(&window));
        if let Err(e) = self.start_painter(&window) {
            crate::fatal(&self.options, &format!("Could not set up drawing: {e}"));
            self.exit_code = 1;
            event_loop.exit();
            return;
        }
        log::info!(
            "git window: window and painter ready in {:?}",
            self.started.elapsed()
        );
        window.request_redraw();
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: GitEvent) {
        match event {
            GitEvent::Disk => {
                self.refresh_at = Some(Instant::now() + SETTLE);
            }
            GitEvent::Say(text, is_error) => self.say(text, is_error),
            GitEvent::FontLoaded => self.redraw(),
            GitEvent::Fonts(fonts) => {
                let (font, ui_font) = *fonts;
                if let Some(gpu) = self.gpu.as_mut() {
                    if let Some(f) = font {
                        gpu.set_font(f);
                    }
                    if ui_font.is_some() {
                        gpu.set_ui_font(ui_font);
                    }
                }
                self.pane.relayout();
                self.redraw();
            }
            event => {
                let refreshed = matches!(event, GitEvent::Refreshed(_) | GitEvent::Opened(_));
                if self.pane.event(event) {
                    self.redraw();
                }
                if refreshed {
                    self.set_title();
                    self.sync_watch();
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if event_loop.exiting() {
            return;
        }
        let now = Instant::now();
        if self.refresh_at.is_some_and(|t| now >= t) {
            self.refresh_at = None;
            self.pane.refresh();
        }
        let unattended = self.options.smoke || self.options.screenshot.is_some();
        let deadline = self.started + Duration::from_secs(10);
        if unattended && self.frames == 0 && now >= deadline {
            print_err("nimbus: no frame could be drawn in 10 s; try --headless");
            self.exit_code = 1;
            event_loop.exit();
            return;
        }
        let next = [
            self.refresh_at,
            self.message.as_ref().map(|m| m.until),
            (unattended && self.frames == 0).then_some(deadline),
        ]
        .into_iter()
        .flatten()
        .min();
        event_loop.set_control_flow(next.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, _cause: winit::event::StartCause) {
        if self
            .message
            .as_ref()
            .is_some_and(|m| Instant::now() >= m.until)
        {
            self.message = None;
            self.redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if self.gpu.is_none() {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::KeyboardInput { event, .. } => self.key(&event, event_loop),
            WindowEvent::CursorMoved { position, .. } => self.cursor_moved(position),
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                if state == ElementState::Pressed {
                    self.mouse_down();
                } else {
                    self.pane.dragging = None;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => self.wheel(delta),
            // Coming back to the window: what changed meanwhile shows at
            // once, even where no watch reaches.
            WindowEvent::Focused(true) => {
                if self.frames > 0 {
                    self.pane.refresh();
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                let px = self.font_px();
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.set_font_size(px);
                }
                self.pane.relayout();
                self.redraw();
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.resize(size.width, size.height);
                }
                self.pane.relayout();
                self.redraw();
            }
            WindowEvent::Occluded(false) => self.redraw(),
            WindowEvent::RedrawRequested => self.present(event_loop),
            _ => {}
        }
    }
}

/// `--git DIR --headless WxH --screenshot FILE`: open the repository,
/// read its status, refs and history, select the newest commit and its
/// first file, all on this thread, then draw one frame and save it.
/// Returns the exit code.
pub fn run_headless(
    options: &Options,
    (config, languages): (&Config, Languages),
    size: (u32, u32),
    context: Option<GpuContext>,
) -> i32 {
    let Some(path) = &options.screenshot else {
        print_err("nimbus: --headless needs --screenshot FILE.bmp");
        return 2;
    };
    let scale = effective_scale(2.0);
    let Ok(font) = Font::bundled() else {
        print_err("nimbus: cannot read the bundled font");
        return 1;
    };
    let px = to_f32(config.font_size * scale);
    let mut gpu = match context {
        Some(c) => Gpu::headless(c, font, px, size, options.linear_blend),
        None => Gpu::compat_headless(font, px, size),
    };
    gpu.set_tab_width(config.tab_width);
    let (tx, rx) = mpsc::channel::<GitEvent>();
    let tx = std::sync::Mutex::new(tx);
    let send: Send = Arc::new(move |e| tx.lock().is_ok_and(|t| t.send(e).is_ok()));
    let normalize = State::load().diff_options();
    let mut pane = GitWindow::new(folder(options), send, Arc::new(languages), normalize);
    let (mut theme, warnings) = Theme::load(&config.theme);
    for w in warnings {
        print_err(&format!("nimbus: {w}"));
    }
    pane.headless_load(&rx, options.git_select.as_deref());
    pane.draw(&mut gpu, &mut theme, to_f32(scale), None);
    let _ = gpu.wait_for_fonts(Duration::from_secs(5));
    pane.draw(&mut gpu, &mut theme, to_f32(scale), None);
    gpu.request_capture();
    if let Err(e) = gpu.render() {
        print_err(&format!("nimbus: {e}"));
        return 1;
    }
    i32::from(!save_screenshot(path, gpu.take_capture().as_ref()))
}
