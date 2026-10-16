//! Nimbus: a fast, lightweight text editor.
//!
//! This file is startup: command-line options, loading the config and the
//! files to open, and handing over to the event loop in [`app`].

mod action;
mod app;
mod bmp;
mod config;
mod document;
mod find;
mod input;
mod io;
mod keymap;
mod languages;
mod menu;
mod picker;
mod sidebar;
mod state;
mod theme;
mod tree;
mod view;
mod watch;

use std::path::{Path, PathBuf};
use std::time::Instant;

use rfd::{MessageButtons, MessageDialog, MessageLevel};
use winit::event_loop::{ControlFlow, EventLoop};

use nimbus_render::GpuContext;

use crate::app::{App, AppEvent};
use crate::config::Config;
use crate::document::Document;
use crate::languages::Languages;

/// Command-line options.
pub struct Options {
    /// Exit right after the first frame is presented, printing time-to-first-frame.
    /// Used by CI and by `hyperfine` to track the startup budget (DESIGN.md §1).
    pub smoke: bool,
    /// Save the first frame as a BMP image at this path, then exit. A
    /// debugging aid for rendering work, since the OS may not allow external
    /// screenshots of the window.
    pub screenshot: Option<PathBuf>,
    /// Files to open, one tab each; a folder sets the project root.
    pub files: Vec<PathBuf>,
    /// Type this many characters, one per frame, through the normal editing
    /// path, then print keystroke-to-present latency and exit. Measures
    /// typing cost without needing a person at the keyboard.
    pub bench_typing: usize,
    /// Open with the find bar searching for this pattern.
    pub find: Option<String>,
    /// Start with soft wrap on, whatever the config says.
    pub wrap: bool,
    /// Render without a window at this size in physical pixels (with
    /// `--screenshot`), for checks while the screen is locked.
    pub headless: Option<(u32, u32)>,
    /// With `--headless`: draw an open picker, `palette` or `files`,
    /// optionally with a query after a colon (`files:main`).
    pub picker: Option<String>,
    /// Use this theme instead of the config's.
    pub theme: Option<String>,
}

/// Write a line to stdout. Command-line output such as `--version` and the
/// `--smoke` timing is the program's job, so it is not logging.
#[expect(clippy::print_stdout, reason = "CLI output, not diagnostics")]
pub fn print_out(line: &str) {
    println!("{line}");
}

/// Write a line to stderr, for errors that happen before logging matters or
/// that the user must see even with logging filtered out.
#[expect(clippy::print_stderr, reason = "CLI errors shown to the user")]
pub fn print_err(line: &str) {
    eprintln!("{line}");
}

/// The scale factor to render at: the window's, unless a debug build sets
/// `NIMBUS_SCALE` to test another density on one display (winit's own
/// override only works on X11).
#[must_use]
pub fn effective_scale(window_scale: f64) -> f64 {
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
/// Convert to `f32` for the renderer, which works in single precision.
#[must_use]
pub fn to_f32(v: f64) -> f32 {
    v as f32
}

fn parse_options() -> Options {
    let mut smoke = false;
    let mut screenshot = None;
    let mut files = Vec::new();
    let mut bench_typing = 0;
    let mut find = None;
    let mut wrap = false;
    let mut headless = None;
    let mut picker = None;
    let mut theme = None;
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
            Some("--wrap") => wrap = true,
            Some("--picker") => picker = args.next().and_then(|p| p.into_string().ok()),
            Some("--theme") => theme = args.next().and_then(|t| t.into_string().ok()),
            Some("--headless") => {
                let size = args.next().and_then(|a| {
                    let a = a.into_string().ok()?;
                    let (w, h) = a.split_once('x')?;
                    Some((w.parse().ok()?, h.parse().ok()?))
                });
                let Some(size) = size else {
                    print_err("nimbus: --headless needs a size like 1600x1000");
                    std::process::exit(2);
                };
                headless = Some(size);
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
                    "usage: nimbus [FILE|FOLDER]... [--find PATTERN] [--wrap] [--theme NAME] [--headless WxH] [--smoke] [--screenshot FILE.bmp] [--bench-typing N] [--version]",
                );
                std::process::exit(0);
            }
            Some(other) if other.starts_with("--") => {
                print_err(&format!("nimbus: unknown option {other:?}"));
                std::process::exit(2);
            }
            _ => files.push(PathBuf::from(arg)),
        }
    }
    Options {
        smoke,
        screenshot,
        files,
        bench_typing,
        find,
        wrap,
        headless,
        picker,
        theme,
    }
}

/// Write a captured frame to `path` as BMP. Returns whether it was saved;
/// failures are reported on stderr.
#[must_use]
pub fn save_screenshot(path: &Path, capture: Option<&nimbus_render::Capture>) -> bool {
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

/// Open the files named on the command line. A folder becomes the project
/// root for the file opener; otherwise the root is the first file's folder,
/// or the current directory. A file that cannot be opened is reported and
/// exits, as there is no window yet to show the error in.
fn open_files(options: &Options, languages: &Languages) -> (Vec<Document>, PathBuf) {
    let mut docs = Vec::new();
    let mut root = None;
    for path in &options.files {
        if path.is_dir() {
            root.get_or_insert_with(|| path.clone());
            continue;
        }
        match Document::open(path) {
            Ok(mut doc) => {
                doc.detect_language(languages);
                if root.is_none() {
                    root = std::fs::canonicalize(path)
                        .ok()
                        .and_then(|p| p.parent().map(Path::to_path_buf));
                }
                docs.push(doc);
            }
            Err(e) => {
                let msg = format!("Cannot open {}: {e}", path.display());
                print_err(&format!("nimbus: {msg}"));
                // Launched from a file manager there is no terminal, so also
                // show the reason in a dialog.
                if !options.smoke && options.screenshot.is_none() && options.headless.is_none() {
                    MessageDialog::new()
                        .set_level(MessageLevel::Error)
                        .set_title("Nimbus")
                        .set_description(msg)
                        .set_buttons(MessageButtons::Ok)
                        .show();
                }
                std::process::exit(1);
            }
        }
    }
    let root = root
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    (docs, root)
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

    let (mut config, mut warnings) = Config::load();
    if let Some(theme) = &options.theme {
        config.theme.clone_from(theme);
    }
    if let Some(dir) = languages::queries_dir() {
        nimbus_syntax::registry::set_user_queries(dir);
    }
    let (languages, language_warnings) = Languages::load(&config.languages);
    warnings.extend(language_warnings);
    let (docs, root) = open_files(&options, &languages);
    log::debug!(
        "startup: config and {} file(s) loaded at {:?}",
        docs.len(),
        started.elapsed()
    );

    if let Some(size) = options.headless {
        // No message line to show them in.
        for w in &warnings {
            print_err(&format!("nimbus: {w}"));
        }
        let code = match gpu_init.join() {
            Ok(Ok(context)) => app::run_headless(&options, &config, (docs, root), size, context),
            Ok(Err(e)) => {
                print_err(&format!("nimbus: {e}"));
                1
            }
            Err(_) => {
                print_err("nimbus: GPU setup thread did not complete");
                1
            }
        };
        std::process::exit(code);
    }

    let mut builder = EventLoop::<AppEvent>::with_user_event();
    #[cfg(target_os = "macos")]
    {
        // winit's default macOS menu binds Cmd+Q to quit the app directly,
        // which would skip the unsaved-changes prompt. Our own menus
        // (crate::menu) replace it.
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
    let mut app = App::new(
        options,
        started,
        gpu_init,
        (config, languages, docs, root),
        proxy,
        warnings,
    );
    if let Err(e) = event_loop.run_app(&mut app) {
        print_err(&format!("nimbus: event loop error: {e}"));
        std::process::exit(1);
    }
    if app.exit_code != 0 {
        std::process::exit(app.exit_code);
    }
}
