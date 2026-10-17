//! The fuzzy picker behind the command palette (Primary+Shift+P), the
//! file opener (Primary+P), Go to Line and Go to Symbol (Primary+Shift+R).
//!
//! Matching runs in `nucleo`, which scores items on its own thread pool and
//! accepts new items while it works. The file opener exploits that: a
//! walker thread streams paths from the project folder into it as it finds
//! them (respecting `.gitignore`), so the first results show before the walk
//! is done. Each frame takes the current best matches, cheaply.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nimbus_core::Text;
use nimbus_syntax::symbols::Symbol;
use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Nucleo};

use crate::action::{self, ActionInfo};
use crate::document::Document;
use crate::keymap::Keymap;

/// Walking stops after this many files, so a huge folder (a home directory
/// opened by mistake) cannot use unbounded memory.
pub const MAX_FILES: usize = 300_000;

/// Rows shown at once.
pub const VISIBLE_ROWS: usize = 12;

/// What a picker row stands for.
#[derive(Debug, Clone)]
pub enum Item {
    /// A palette entry.
    Action(&'static ActionInfo),
    /// A file, as a path relative to the project folder.
    File(PathBuf),
    /// A line to go to, counting from 1.
    Line(usize),
    /// A definition in the file: its name, kind and line (from 1), and
    /// the byte to put the cursor at.
    Symbol(Symbol, Option<usize>),
    /// An open tab to compare with (M9): its index and name.
    Tab(usize, String),
}

/// Which picker is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The command palette.
    Palette,
    /// The file opener.
    Files,
    /// Go to line: the query is a line number.
    Line,
    /// Go to symbol: the file's definitions.
    Symbols,
    /// Choosing another tab to compare with.
    Tabs,
}

/// One row to draw.
#[derive(Debug, Clone)]
pub struct Row {
    /// Main text: the action title or file name.
    pub text: String,
    /// Secondary text: the key binding or containing folder.
    pub detail: String,
}

/// An open picker.
pub struct Picker {
    /// Which picker.
    pub kind: Kind,
    /// The query field.
    pub input: Document,
    matcher: Nucleo<Item>,
    /// The query last given to the matcher, to tell appending from editing.
    last_query: String,
    /// Index of the highlighted row among the matches.
    pub selected: usize,
    /// First match shown, for scrolling through long lists.
    pub scroll: usize,
    /// Set by the walker thread when it has finished.
    walk_done: Arc<AtomicBool>,
    /// Asks the walker to stop early when the picker closes.
    stop: Arc<AtomicBool>,
    /// The project folder files are relative to.
    pub root: PathBuf,
    /// For go-to-line: how many lines the document has, if all are counted.
    lines: Option<usize>,
}

impl std::fmt::Debug for Picker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Picker")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl Drop for Picker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Picker {
    /// Open the command palette.
    pub fn palette(notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        let picker = Self::new(Kind::Palette, Config::DEFAULT, notify, PathBuf::new());
        let injector = picker.matcher.injector();
        for a in action::palette_actions() {
            injector.push(Item::Action(a), |item, cols| {
                if let Item::Action(a) = item {
                    cols[0] = a.title.into();
                }
            });
        }
        picker.walk_done.store(true, Ordering::Relaxed);
        picker
    }

    /// Open the file opener on `root`, starting a background walk.
    pub fn files(root: &Path, notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        let picker = Self::new(
            Kind::Files,
            Config::DEFAULT.match_paths(),
            Arc::clone(&notify),
            root.to_path_buf(),
        );
        let injector = picker.matcher.injector();
        let (done, stop) = (Arc::clone(&picker.walk_done), Arc::clone(&picker.stop));
        let notify_done = notify;
        let root = root.to_path_buf();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let spawned = std::thread::Builder::new()
            .name("file-walk".into())
            .spawn(move || {
                ignore::WalkBuilder::new(&root)
                    .hidden(true)
                    .build_parallel()
                    .run(|| {
                        let injector = injector.clone();
                        let (root, stop, count) =
                            (root.clone(), Arc::clone(&stop), Arc::clone(&count));
                        Box::new(move |entry| {
                            if stop.load(Ordering::Relaxed)
                                || count.load(Ordering::Relaxed) >= MAX_FILES
                            {
                                return ignore::WalkState::Quit;
                            }
                            let Ok(entry) = entry else {
                                return ignore::WalkState::Continue;
                            };
                            if entry.file_type().is_some_and(|t| t.is_file()) {
                                let rel = entry
                                    .path()
                                    .strip_prefix(&root)
                                    .unwrap_or(entry.path())
                                    .to_path_buf();
                                count.fetch_add(1, Ordering::Relaxed);
                                injector.push(Item::File(rel), |item, cols| {
                                    if let Item::File(p) = item {
                                        cols[0] = p.to_string_lossy().as_ref().into();
                                    }
                                });
                            }
                            ignore::WalkState::Continue
                        })
                    });
                done.store(true, Ordering::Relaxed);
                // Wake the app once more so "loading" can end on screen.
                notify_done();
            });
        if let Err(e) = spawned {
            log::error!("could not start file walk: {e}");
            picker.walk_done.store(true, Ordering::Relaxed);
        }
        picker
    }

    fn new(kind: Kind, config: Config, notify: Arc<dyn Fn() + Send + Sync>, root: PathBuf) -> Self {
        Self {
            kind,
            input: Document::untitled(),
            matcher: Nucleo::new(config, notify, None, 1),
            last_query: String::new(),
            selected: 0,
            scroll: 0,
            walk_done: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
            root,
            lines: None,
        }
    }

    /// Open go-to-line for a document of `lines` lines (`None` while a
    /// huge file is still being counted). No list to match: the query is
    /// the line number.
    pub fn go_to_line(notify: Arc<dyn Fn() + Send + Sync>, lines: Option<usize>) -> Self {
        let mut picker = Self::new(Kind::Line, Config::DEFAULT, notify, PathBuf::new());
        picker.walk_done.store(true, Ordering::Relaxed);
        picker.lines = lines;
        picker
    }

    /// Open Go to Symbol over `symbols`, each with its line number when
    /// known. Matching is by name only; the kind is shown, not matched.
    pub fn symbols(
        notify: Arc<dyn Fn() + Send + Sync>,
        symbols: impl IntoIterator<Item = (Symbol, Option<usize>)>,
    ) -> Self {
        let picker = Self::new(Kind::Symbols, Config::DEFAULT, notify, PathBuf::new());
        let injector = picker.matcher.injector();
        for (symbol, line) in symbols {
            injector.push(Item::Symbol(symbol, line), |item, cols| {
                if let Item::Symbol(s, _) = item {
                    cols[0] = s.name.as_str().into();
                }
            });
        }
        picker.walk_done.store(true, Ordering::Relaxed);
        picker
    }

    /// Open a list of tabs, `(index, name)`, to pick one to compare with.
    pub fn tabs(names: Vec<(usize, String)>, notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        let picker = Self::new(Kind::Tabs, Config::DEFAULT, notify, PathBuf::new());
        let injector = picker.matcher.injector();
        for (i, name) in names {
            injector.push(Item::Tab(i, name), |item, cols| {
                if let Item::Tab(_, name) = item {
                    cols[0] = name.as_str().into();
                }
            });
        }
        picker.walk_done.store(true, Ordering::Relaxed);
        picker
    }

    /// The line number typed, counting from 1; a leading `:` is allowed, as
    /// some editors write it.
    fn line_target(&self) -> Option<usize> {
        let query = self.input.buffer.line(0);
        let digits = query.trim().trim_start_matches(':').trim();
        digits.parse::<usize>().ok().filter(|n| *n > 0)
    }

    /// Feed the current query to the matcher and let it work for up to a
    /// millisecond. Returns whether results changed or are still coming.
    pub fn tick(&mut self) -> bool {
        if self.kind == Kind::Line {
            return false;
        }
        let query = self.input.buffer.line(0).into_owned();
        if query != self.last_query {
            // Appending lets nucleo narrow the previous results instead of
            // starting over.
            let append = query.starts_with(&self.last_query);
            self.matcher.pattern.reparse(
                0,
                &query,
                CaseMatching::Smart,
                Normalization::Smart,
                append,
            );
            self.last_query = query;
            self.selected = 0;
            self.scroll = 0;
        }
        let status = self.matcher.tick(1);
        status.changed || status.running || !self.walk_done.load(Ordering::Relaxed)
    }

    /// Number of items matching the query.
    #[must_use]
    pub fn match_count(&self) -> usize {
        if self.kind == Kind::Line {
            return usize::from(self.line_target().is_some());
        }
        self.matcher.snapshot().matched_item_count() as usize
    }

    /// Whether more items may still arrive (the walk is running).
    #[must_use]
    pub fn loading(&self) -> bool {
        !self.walk_done.load(Ordering::Relaxed)
    }

    /// Move the highlight by `delta` rows, wrapping, and keep it in view.
    pub fn move_selection(&mut self, delta: isize) {
        let n = self.match_count();
        if n == 0 {
            return;
        }
        let n_i = isize::try_from(n).unwrap_or(isize::MAX);
        let cur = isize::try_from(self.selected).unwrap_or(0);
        self.selected = usize::try_from((cur + delta).rem_euclid(n_i)).unwrap_or(0);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + VISIBLE_ROWS {
            self.scroll = self.selected + 1 - VISIBLE_ROWS;
        }
    }

    /// The highlighted item.
    #[must_use]
    pub fn chosen(&self) -> Option<Item> {
        if self.kind == Kind::Line {
            return self.line_target().map(Item::Line);
        }
        let i = u32::try_from(self.selected).ok()?;
        self.matcher
            .snapshot()
            .get_matched_item(i)
            .map(|it| it.data.clone())
    }

    /// The rows currently visible, with the index of the highlighted one.
    #[must_use]
    pub fn rows(&self, keymap: &Keymap) -> (Vec<Row>, Option<usize>) {
        if self.kind == Kind::Line {
            let Some(n) = self.line_target() else {
                return (Vec::new(), None);
            };
            let detail = match self.lines {
                Some(total) if n > total => format!("past the last line, {total}"),
                Some(total) => format!("of {total}"),
                None => "lines still being counted".to_owned(),
            };
            let row = Row {
                text: format!("Go to line {n}"),
                detail,
            };
            return (vec![row], Some(0));
        }
        let snap = self.matcher.snapshot();
        let n = snap.matched_item_count();
        let start = u32::try_from(self.scroll).unwrap_or(0).min(n);
        let end = start
            .saturating_add(u32::try_from(VISIBLE_ROWS).unwrap_or(0))
            .min(n);
        let rows = snap
            .matched_items(start..end)
            .map(|it| match it.data {
                Item::Action(a) => Row {
                    text: a.title.to_owned(),
                    detail: keymap.label(a.id),
                },
                Item::Line(n) => Row {
                    text: format!("Go to line {n}"),
                    detail: String::new(),
                },
                Item::Symbol(s, line) => Row {
                    text: s.name.clone(),
                    detail: match line {
                        Some(n) => format!("{}, line {n}", s.kind.label()),
                        None => s.kind.label().to_owned(),
                    },
                },
                Item::Tab(_, name) => Row {
                    text: name.clone(),
                    detail: String::new(),
                },
                Item::File(p) => Row {
                    text: p
                        .file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                    detail: p
                        .parent()
                        .map_or_else(String::new, |d| d.to_string_lossy().into_owned()),
                },
            })
            .collect();
        (rows, self.selected.checked_sub(self.scroll))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_notify() -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(|| {})
    }

    fn settle(p: &mut Picker) {
        for _ in 0..2000 {
            if !p.tick() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn palette_matches_titles_fuzzily() {
        let mut p = Picker::palette(no_notify());
        settle(&mut p);
        assert!(p.match_count() > 10);
        let _ = p.input.insert("sav as");
        settle(&mut p);
        match p.chosen() {
            Some(Item::Action(a)) => assert_eq!(a.id, "file.saveAs"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn file_opener_walks_and_respects_gitignore() {
        let dir = std::env::temp_dir().join(format!("nimbus-picker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "").unwrap();
        std::fs::write(dir.join("README.md"), "").unwrap();
        std::fs::write(dir.join("target/junk.rs"), "").unwrap();
        std::fs::write(dir.join(".ignore"), "target/\n").unwrap();
        let mut p = Picker::files(&dir, no_notify());
        settle(&mut p);
        assert_eq!(p.match_count(), 2, "target/ is ignored");
        let _ = p.input.insert("mainrs");
        settle(&mut p);
        match p.chosen() {
            Some(Item::File(path)) => assert_eq!(path, PathBuf::from("src/main.rs")),
            other => panic!("{other:?}"),
        }
        let (rows, sel) = p.rows(&Keymap::default());
        assert_eq!(
            (rows[0].text.as_str(), rows[0].detail.as_str(), sel),
            ("main.rs", "src", Some(0))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Time to first results and to the end of the walk on a big folder.
    /// Ignored by default; run with `NIMBUS_BENCH_DIR=<folder> cargo test
    /// --release -p nimbus -- --ignored --nocapture walk_benchmark`.
    #[test]
    #[ignore = "benchmark; needs NIMBUS_BENCH_DIR"]
    fn walk_benchmark() {
        let Ok(dir) = std::env::var("NIMBUS_BENCH_DIR") else {
            println!("set NIMBUS_BENCH_DIR");
            return;
        };
        let start = std::time::Instant::now();
        let mut p = Picker::files(Path::new(&dir), no_notify());
        let _ = p.input.insert("lib.rs");
        let mut first = None;
        while p.tick() {
            if first.is_none() && p.match_count() > 0 {
                first = Some(start.elapsed());
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        println!(
            "first match after {first:?}; walk + match done after {:?}; {} matches",
            start.elapsed(),
            p.match_count()
        );
    }

    #[test]
    fn selection_wraps_and_scrolls() {
        let mut p = Picker::palette(no_notify());
        settle(&mut p);
        let n = p.match_count();
        p.move_selection(-1);
        assert_eq!(p.selected, n - 1);
        assert_eq!(p.scroll, n - VISIBLE_ROWS);
        p.move_selection(1);
        assert_eq!((p.selected, p.scroll), (0, 0));
    }

    #[test]
    fn symbols_match_by_name_and_show_their_kind() {
        use nimbus_syntax::symbols::SymbolKind;
        let sym = |name: &str, kind, start| Symbol {
            name: name.to_owned(),
            kind,
            start,
        };
        let mut p = Picker::symbols(
            no_notify(),
            [
                (sym("main", SymbolKind::Function, 0), Some(1)),
                (sym("Config", SymbolKind::Class, 40), Some(5)),
                (sym("parse", SymbolKind::Method, 90), None),
            ],
        );
        settle(&mut p);
        assert_eq!(p.match_count(), 3);
        let _ = p.input.insert("cfg");
        settle(&mut p);
        match p.chosen() {
            Some(Item::Symbol(s, Some(5))) => assert_eq!(s.start, 40),
            other => panic!("{other:?}"),
        }
        let (rows, _) = p.rows(&Keymap::default());
        assert_eq!(
            (rows[0].text.as_str(), rows[0].detail.as_str()),
            ("Config", "type, line 5")
        );
    }

    #[test]
    fn go_to_line_reads_a_number() {
        let mut p = Picker::go_to_line(no_notify(), Some(40));
        assert_eq!(p.match_count(), 0);
        let _ = p.input.insert(":12");
        assert!(!p.tick());
        assert_eq!(p.match_count(), 1);
        assert!(matches!(p.chosen(), Some(Item::Line(12))));
        let (rows, sel) = p.rows(&Keymap::default());
        assert_eq!(
            (rows[0].text.as_str(), rows[0].detail.as_str(), sel),
            ("Go to line 12", "of 40", Some(0))
        );
        let _ = p.input.insert("x");
        assert!(p.chosen().is_none());
    }
}
