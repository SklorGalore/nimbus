//! Nimbus: a fast, lightweight text editor.
//!
//! This file is startup: command-line options, loading the config and the
//! files to open, and handing over to the event loop in [`app`].

mod action;
mod app;
mod bmp;
mod columns;
mod compare;
mod config;
mod debug;
mod docs;
mod document;
mod editing;
mod find;
mod find_files;
mod indexer;
mod input;
mod io;
mod keymap;
mod languages;
mod mapping;
mod menu;
mod merge_view;
mod picker;
mod replace_files;
mod saver;
mod search_panel;
mod sidebar;
mod state;
mod theme;
mod tree;
mod update;
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent command-line switches"
)]
#[derive(Debug, Default)]
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
    /// Open with the sidebar's Search panel searching the project for this
    /// pattern; with `--headless`, the search runs to the end first.
    pub search: Option<String>,
    /// Start with soft wrap on, whatever the config says.
    pub wrap: bool,
    /// Render without a window at this size in physical pixels (with
    /// `--screenshot`), for checks while the screen is locked.
    pub headless: Option<(u32, u32)>,
    /// With `--headless`: draw an open picker, `palette`, `files`, `line` or `symbols`,
    /// optionally with a query after a colon (`files:main`).
    pub picker: Option<String>,
    /// Use this theme instead of the config's.
    pub theme: Option<String>,
    /// Open every file in this backend, whatever its size (`--backend
    /// rope|mapped`), for measuring one against the other.
    pub backend: Option<io::Backend>,
    /// With `--headless`: count all of a huge file's lines before drawing,
    /// instead of drawing with only the first part counted.
    pub wait_index: bool,
    /// `--debug-overlay`: start with the debug overlay (F12) showing.
    pub debug_overlay: bool,
    /// `--cursors 3,10,20`: with `--headless`, put a cursor at each byte
    /// offset (the last is the primary), to render several cursors.
    pub cursors: Vec<usize>,
    /// `--diff A B`: compare `B` (opened as a tab) with `A`.
    pub diff: Option<(PathBuf, PathBuf)>,
    /// `--merge BASE LOCAL REMOTE -o RESULT`: a three-way merge tab whose
    /// result saves to `RESULT`; the exit code says whether it was saved
    /// with no conflicts left.
    pub merge: Option<MergeArgs>,
}

/// The files of `--merge`.
#[derive(Debug, Clone)]
pub struct MergeArgs {
    /// The common ancestor.
    pub base: PathBuf,
    /// Our version.
    pub local: PathBuf,
    /// Their version.
    pub remote: PathBuf,
    /// Where the result saves (`-o`); the local file when not given, as
    /// git's mergetool expects.
    pub out: PathBuf,
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

/// What `--help` prints.
const USAGE: &str = "usage: nimbus [FILE|FOLDER]... [--find PATTERN] [--search PATTERN] [--wrap] [--theme NAME] [--backend rope|mapped] [--headless WxH [--wait-index]] [--debug-overlay] [--cursors N,N] [--smoke] [--screenshot FILE.bmp] [--bench-typing N] [--diff A B] [--merge BASE LOCAL REMOTE -o RESULT] [--version]";

/// A comma-separated list of numbers, such as `3,10,20`; anything that is
/// not a number is skipped.
fn numbers(arg: Option<std::ffi::OsString>) -> Vec<usize> {
    let list = arg.and_then(|a| a.into_string().ok());
    list.iter()
        .flat_map(|l| l.split(','))
        .filter_map(|n| n.trim().parse().ok())
        .collect()
}

/// A `WxH` size after `--headless`, or exit with a usage error.
fn parse_size(arg: Option<std::ffi::OsString>) -> (u32, u32) {
    let size = arg.and_then(|a| {
        let a = a.into_string().ok()?;
        let (w, h) = a.split_once('x')?;
        Some((w.parse().ok()?, h.parse().ok()?))
    });
    size.unwrap_or_else(|| {
        print_err("nimbus: --headless needs a size like 1600x1000");
        std::process::exit(2);
    })
}

/// The two paths after an option, or exit with a usage error.
fn two_paths(args: &mut Args, option: &str) -> (PathBuf, PathBuf) {
    let (Some(a), Some(b)) = (args.next(), args.next()) else {
        print_err(&format!("nimbus: {option} needs two files"));
        std::process::exit(2);
    };
    (PathBuf::from(a), PathBuf::from(b))
}

fn parse_options() -> Options {
    let mut o = Options::default();
    let mut merge = None;
    let mut merge_out = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        let flag = arg.to_str().unwrap_or("");
        if run_flag(&mut o, flag, &mut args)
            || file_flag(&mut o, flag, &mut args, (&mut merge, &mut merge_out))
        {
            continue;
        }
        if flag.starts_with("--") {
            print_err(&format!("nimbus: unknown option {flag:?}"));
            std::process::exit(2);
        }
        o.files.push(PathBuf::from(arg));
    }
    o.merge = merge.map(|(base, local, remote)| MergeArgs {
        out: merge_out.unwrap_or_else(|| local.clone()),
        base,
        local,
        remote,
    });
    o
}

type Args = std::iter::Skip<std::env::ArgsOs>;

/// The value after an option, or exit with `needs`.
fn value(args: &mut Args, option: &str, needs: &str) -> std::ffi::OsString {
    args.next().unwrap_or_else(|| {
        print_err(&format!("nimbus: {option} needs {needs}"));
        std::process::exit(2);
    })
}

/// Options about how the window runs. `true` when `flag` was one.
fn run_flag(o: &mut Options, flag: &str, args: &mut Args) -> bool {
    match flag {
        "--smoke" => o.smoke = true,
        "--screenshot" => {
            o.screenshot = Some(PathBuf::from(value(args, flag, "a file path")));
        }
        "--bench-typing" => {
            o.bench_typing = value(args, flag, "a count")
                .to_str()
                .and_then(|n| n.parse().ok())
                .unwrap_or_else(|| {
                    print_err("nimbus: --bench-typing needs a count");
                    std::process::exit(2);
                });
        }
        "--wrap" => o.wrap = true,
        "--wait-index" => o.wait_index = true,
        "--debug-overlay" => o.debug_overlay = true,
        "--cursors" => o.cursors = numbers(args.next()),
        "--picker" => o.picker = args.next().and_then(|p| p.into_string().ok()),
        "--theme" => o.theme = args.next().and_then(|t| t.into_string().ok()),
        "--backend" => {
            let Some(b) = args
                .next()
                .and_then(|b| b.to_str().and_then(io::Backend::parse))
            else {
                print_err("nimbus: --backend needs rope or mapped");
                std::process::exit(2);
            };
            o.backend = Some(b);
        }
        "--headless" => o.headless = Some(parse_size(args.next())),
        _ => return false,
    }
    true
}

/// Options about files and what to do with them. `true` when `flag` was
/// one. `--merge` and `-o` collect into `merge` until the end.
fn file_flag(
    o: &mut Options,
    flag: &str,
    args: &mut Args,
    (merge, merge_out): (
        &mut Option<(PathBuf, PathBuf, PathBuf)>,
        &mut Option<PathBuf>,
    ),
) -> bool {
    match flag {
        "--merge" => {
            let base = value(args, flag, "BASE LOCAL REMOTE");
            let local = value(args, flag, "BASE LOCAL REMOTE");
            let remote = value(args, flag, "BASE LOCAL REMOTE");
            *merge = Some((
                PathBuf::from(base),
                PathBuf::from(local),
                PathBuf::from(remote),
            ));
        }
        "-o" | "--output" => *merge_out = Some(PathBuf::from(value(args, flag, "a file"))),
        "--diff" => {
            let (a, b) = two_paths(args, "--diff");
            o.files.push(b.clone());
            o.diff = Some((a, b));
        }
        "--find" | "--search" => {
            let Ok(pattern) = value(args, flag, "a pattern").into_string() else {
                print_err(&format!("nimbus: {flag} needs a UTF-8 pattern"));
                std::process::exit(2);
            };
            if flag == "--find" {
                o.find = Some(pattern);
            } else {
                o.search = Some(pattern);
            }
        }
        "--version" | "-V" => {
            print_out(&format!("nimbus {}", env!("CARGO_PKG_VERSION")));
            std::process::exit(0);
        }
        "--help" | "-h" => {
            print_out(USAGE);
            std::process::exit(0);
        }
        _ => return false,
    }
    true
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
    if let Some(backend) = options.backend {
        io::force_backend(backend);
    }

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
