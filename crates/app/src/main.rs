//! Nimbus: a fast, lightweight text editor.
//!
//! Milestone 1: draw a sample of text through our own shaping, glyph atlas
//! and GPU pipelines, at the display's scale factor.
//!
//! `winit` drives the program through an [`ApplicationHandler`]: we hand it a
//! struct, and it calls back into us when the OS has events. Windows may only
//! be created once the app is `resumed`, which is why creation lives there and
//! not in `main`.

mod bmp;

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState};
use winit::window::{Window, WindowId};

use nimbus_render::{Font, FrameStatus, Gpu, GpuContext, GpuError};

/// Command-line options. A real parser arrives when there is more to parse.
struct Options {
    /// Exit right after the first frame is presented, printing time-to-first-frame.
    /// Used by CI and by `hyperfine` to track the startup budget (DESIGN.md §1).
    smoke: bool,
    /// Save the first frame as a BMP image at this path, then exit. A
    /// debugging aid for rendering work, since the OS may not allow external
    /// screenshots of the window.
    screenshot: Option<PathBuf>,
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

/// Editor font size in logical points. Multiplied by the window's scale
/// factor to get the pixel size glyphs are rasterized at. Comes from config
/// in M4.
const FONT_SIZE_PT: f64 = 14.0;

/// Text colour until themes arrive in M5.
const TEXT_COLOR: [f32; 4] = [0.85, 0.87, 0.91, 1.0];

/// Stand-in cursor colour.
const CURSOR_COLOR: [f32; 4] = [0.98, 0.75, 0.30, 1.0];

/// The M1 sample: code, accents, a tab-indented line, box drawing, Nerd Font
/// icons, and one character the font lacks, which draws as its missing-glyph
/// box. Replaced by a real file in M2.
const SAMPLE: &str = "\
fn main() {
\tlet greeting = \"naïve café, déjà vu\";
\tprintln!(\"{greeting}\");
}

┌──────────┬──────────┐
│ box      │ drawing  │
└──────────┴──────────┘
Nerd Font icons:  \u{e0a0} branch  \u{f07b} folder  \u{f121} code  \u{e7a8} rust
Missing glyph: \u{10000}
0123456789 !@#$%^&*()_+-=[]{};':\",./<>? ~`|\\";

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
    reason = "font sizes in pixels are small; f32 is what the renderer takes"
)]
fn font_px(scale: f64) -> f32 {
    (FONT_SIZE_PT * scale) as f32
}

/// Queue the sample text and stand-in cursor for one frame.
#[expect(
    clippy::cast_possible_truncation,
    reason = "scale factors are small; the renderer works in f32 pixels"
)]
fn draw_sample(gpu: &mut Gpu, scale: f64) {
    let m = gpu.metrics();
    let scale = scale as f32;
    let margin = (12.0 * scale).round();
    gpu.begin_frame();
    // The cursor sits after "fn main() {" on line 1: 11 cells in.
    gpu.push_rect(
        [margin + 11.0 * m.cell_width, margin],
        [(2.0 * scale).max(1.0).round(), m.line_height],
        CURSOR_COLOR,
    );
    let mut y = margin;
    for line in SAMPLE.lines() {
        gpu.push_text_line([margin, y], line, TEXT_COLOR);
        y += m.line_height;
    }
}

/// The platform's "primary" modifier: Command on macOS, Control elsewhere.
/// Keybindings are written in terms of it (DESIGN.md §3).
fn primary_held(mods: ModifiersState) -> bool {
    if cfg!(target_os = "macos") {
        mods.super_key()
    } else {
        mods.control_key()
    }
}

/// Primary+W or Primary+Q: close the window, which quits while there is only
/// one window and no tabs. M4 replaces this with the action registry, where
/// Primary+W closes a tab instead.
fn is_close_chord(event: &KeyEvent, mods: ModifiersState) -> bool {
    event.state == ElementState::Pressed
        && !event.repeat
        && primary_held(mods)
        && matches!(&event.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("w") || c.eq_ignore_ascii_case("q"))
}

fn parse_options() -> Options {
    let mut smoke = false;
    let mut screenshot = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--smoke" => smoke = true,
            "--screenshot" => {
                let Some(path) = args.next() else {
                    print_err("nimbus: --screenshot needs a file path");
                    std::process::exit(2);
                };
                screenshot = Some(PathBuf::from(path));
            }
            "--version" | "-V" => {
                print_out(&format!("nimbus {}", nimbus_core::VERSION));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                print_out("usage: nimbus [--smoke] [--screenshot FILE.bmp] [--version]");
                std::process::exit(0);
            }
            other => {
                print_err(&format!("nimbus: unknown argument {other:?}"));
                std::process::exit(2);
            }
        }
    }
    Options { smoke, screenshot }
}

/// Write a captured frame to `path` as BMP. Returns whether it was saved;
/// failures are reported on stderr.
fn save_screenshot(path: &std::path::Path, capture: Option<&nimbus_render::Capture>) -> bool {
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
}

impl App {
    fn new(
        options: Options,
        started: Instant,
        gpu_init: JoinHandle<Result<GpuContext, GpuError>>,
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
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        log::debug!("startup: resumed at {:?}", self.started.elapsed());

        let attrs = Window::default_attributes()
            .with_title(format!("Nimbus {}", nimbus_core::VERSION))
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
            Ok(gpu) => self.gpu = Some(gpu),
            Err(e) => {
                log::error!("{e}");
                event_loop.exit();
                return;
            }
        }

        log::info!("window + gpu ready in {:?}", self.started.elapsed());
        window.request_redraw();
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let (Some(window), Some(gpu)) = (self.window.as_ref(), self.gpu.as_mut()) else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::ModifiersChanged(mods) => self.modifiers = mods.state(),

            WindowEvent::KeyboardInput { event, .. } => {
                if is_close_chord(&event, self.modifiers) {
                    event_loop.exit();
                }
            }

            WindowEvent::Occluded(false) => window.request_redraw(),

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // A Resized with the new physical size follows; this only has
                // to re-rasterize glyphs at the new density.
                gpu.set_font_size(font_px(effective_scale(scale_factor)));
                window.request_redraw();
            }

            WindowEvent::Resized(size) => {
                gpu.resize(size.width, size.height);
                window.request_redraw();
            }

            WindowEvent::RedrawRequested => {
                draw_sample(gpu, effective_scale(window.scale_factor()));
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
                        window.request_redraw();
                    }
                    Ok(FrameStatus::Retry) => {
                        log::debug!("frame skipped; retrying");
                        window.request_redraw();
                        return;
                    }
                    Ok(FrameStatus::Hidden) => {
                        // Drawn again on Occluded(false) or Resized.
                        log::debug!("frame skipped; window hidden");
                        return;
                    }
                    Err(e) => {
                        log::error!("{e}");
                        event_loop.exit();
                        return;
                    }
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

    // Start opening the GPU next, so the driver work overlaps event-loop
    // setup and window creation (DESIGN.md §4.3).
    let gpu_init = match GpuContext::spawn() {
        Ok(handle) => handle,
        Err(e) => {
            print_err(&format!("nimbus: could not start GPU thread: {e}"));
            std::process::exit(1);
        }
    };

    let event_loop = match EventLoop::new() {
        Ok(el) => el,
        Err(e) => {
            print_err(&format!("nimbus: could not start event loop: {e}"));
            std::process::exit(1);
        }
    };
    // Wait: sleep until the OS has an event for us. We redraw on demand, never
    // in a loop, so an idle editor costs no CPU (DESIGN.md §4.3).
    event_loop.set_control_flow(ControlFlow::Wait);
    log::debug!("startup: event loop ready at {:?}", started.elapsed());

    let mut app = App::new(options, started, gpu_init);
    if let Err(e) = event_loop.run_app(&mut app) {
        print_err(&format!("nimbus: event loop error: {e}"));
        std::process::exit(1);
    }
    if app.exit_code != 0 {
        std::process::exit(app.exit_code);
    }
}
