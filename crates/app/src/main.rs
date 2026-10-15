//! Nimbus: a fast, lightweight text editor.
//!
//! Milestone 0: open a window, clear it every frame, resize and close cleanly.
//!
//! `winit` drives the program through an [`ApplicationHandler`]: we hand it a
//! struct, and it calls back into us when the OS has events. Windows may only
//! be created once the app is `resumed`, which is why creation lives there and
//! not in `main`.

use std::sync::Arc;
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use nimbus_render::Gpu;

/// Command-line options. A real parser arrives when there is more to parse.
struct Options {
    /// Exit right after the first frame is presented, printing time-to-first-frame.
    /// Used by CI and by `hyperfine` to track the startup budget (DESIGN.md §1).
    smoke: bool,
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

fn parse_options() -> Options {
    let mut smoke = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--smoke" => smoke = true,
            "--version" | "-V" => {
                print_out(&format!("nimbus {}", nimbus_core::VERSION));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                print_out("usage: nimbus [--smoke] [--version]");
                std::process::exit(0);
            }
            other => {
                print_err(&format!("nimbus: unknown argument {other:?}"));
                std::process::exit(2);
            }
        }
    }
    Options { smoke }
}

struct App {
    options: Options,
    started: Instant,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    frames: u64,
}

impl App {
    fn new(options: Options) -> Self {
        Self {
            options,
            started: Instant::now(),
            window: None,
            gpu: None,
            frames: 0,
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

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

        match Gpu::new(&window) {
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

            WindowEvent::Resized(size) => {
                gpu.resize(size.width, size.height);
                window.request_redraw();
            }

            WindowEvent::RedrawRequested => {
                match gpu.render() {
                    Ok(true) => self.frames += 1,
                    Ok(false) => {
                        log::debug!("frame skipped");
                        return;
                    }
                    Err(e) => {
                        log::error!("{e}");
                        event_loop.exit();
                        return;
                    }
                }

                if self.frames == 1 {
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

            // Includes ScaleFactorChanged: winit follows it with a Resized
            // carrying the new physical size, so nothing to do yet. Font
            // metrics will care about it in milestone 1.
            _ => {}
        }
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("nimbus=info"))
        .format_timestamp_millis()
        .init();

    let options = parse_options();

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

    let mut app = App::new(options);
    if let Err(e) = event_loop.run_app(&mut app) {
        print_err(&format!("nimbus: event loop error: {e}"));
        std::process::exit(1);
    }
}
