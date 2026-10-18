//! The Search panel: the sidebar's second mode, beside the file tree.
//!
//! The query, replacement, include and exclude fields are small
//! [`Document`]s, as the find bar's are, so they get movement, selection
//! and undo for free. Below them sits the count line and then the results,
//! grouped by file. The panel keeps the [`Results`] a search streams in and
//! turns them into rows; it draws only the rows on screen. What a click or
//! key asks for comes back as a [`Reply`] for the app to act on, since
//! opening files and starting searches is the app's job.

use std::fmt::Write as _;
use std::sync::Arc;

use nimbus_core::Text;
use nimbus_core::column;
use nimbus_core::search::{Generation, Query};
use nimbus_render::Gpu;

use crate::document::Document;
use crate::find_files::{self, Batch, Params, Results};
use crate::input::Motion;
use crate::theme::Ui;
use crate::view::{self, Layout};

/// Which part of the panel has keyboard focus while the sidebar does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The pattern.
    Query,
    /// The replacement.
    Replace,
    /// Globs of files to search.
    Include,
    /// Globs of files to leave out.
    Exclude,
    /// The results list.
    List,
}

/// One toggle on the options row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    /// Match case.
    Case,
    /// Regular expression.
    Regex,
    /// Whole word.
    Word,
    /// Show the replacement field.
    Replace,
}

/// What is at a point of the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A field.
    Field(Focus),
    /// A toggle.
    Toggle(Toggle),
    /// The Replace All button.
    ReplaceAll,
    /// A row of the list.
    Row(usize),
    /// The checkbox of a row (replace mode).
    Checkbox(usize),
    /// Nothing clickable.
    Nothing,
}

/// One row of the results list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// A file header: index into [`Results::files`].
    File(usize),
    /// A hit: file index and hit index within it.
    Hit(usize, usize),
}

/// What the app should do after a click or key in the panel.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reply {
    /// Open this hit in the editor.
    pub open: Option<(usize, usize)>,
    /// The query or an option changed: search again.
    pub search: bool,
    /// Replace All was asked for.
    pub replace_all: bool,
}

/// The panel's state.
#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent option toggles and modes; an enum would not be clearer"
)]
pub struct Panel {
    /// The pattern.
    pub query: Document,
    /// The replacement.
    pub replace: Document,
    /// Files to search, as globs.
    pub include: Document,
    /// Files to leave out, as globs.
    pub exclude: Document,
    /// Regular expression matching.
    pub regex: bool,
    /// Match case.
    pub case_sensitive: bool,
    /// Whole words only.
    pub whole_word: bool,
    /// The replacement field is shown.
    pub replace_mode: bool,
    /// Focused part, while the sidebar has focus.
    pub focus: Focus,
    /// Results of the current search, if one was started.
    pub results: Option<Results>,
    /// The search the results came from.
    pub params: Option<Params>,
    /// Why the last search could not start.
    pub error: Option<String>,
    /// The selected row.
    pub selected: Option<usize>,
    /// The first visible row.
    pub scroll: usize,
    /// A Replace All is running on a worker.
    pub replacing: bool,
    /// Rows of the list, rebuilt as results arrive.
    rows: Vec<Row>,
    /// Cancels the running search when bumped.
    generation: Arc<Generation>,
    /// Scroll left over from trackpad deltas smaller than a row.
    wheel: f32,
    /// The toggle under the pointer, for its tooltip.
    pub hover: Option<Toggle>,
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            query: Document::untitled(),
            replace: Document::untitled(),
            include: Document::untitled(),
            exclude: Document::untitled(),
            regex: false,
            case_sensitive: false,
            whole_word: false,
            replace_mode: false,
            focus: Focus::Query,
            results: None,
            params: None,
            error: None,
            selected: None,
            scroll: 0,
            replacing: false,
            rows: Vec::new(),
            generation: Arc::new(Generation::default()),
            wheel: 0.0,
            hover: None,
        }
    }
}

/// Text of a single-line field document.
fn field_text(doc: &Document) -> String {
    doc.buffer.slice(0..doc.buffer.len_bytes()).into_owned()
}

/// Where the panel's parts are, in physical pixels from the window's top
/// left; the panel spans the sidebar's width.
#[derive(Debug, Clone, Copy)]
struct Geometry {
    /// Top of each field row, in `Focus` order (`None` for a hidden one).
    query: f32,
    replace: Option<f32>,
    include: f32,
    exclude: f32,
    /// Top of the toggles row.
    toggles: f32,
    /// Top of the count line.
    count: f32,
    /// Top of the list.
    list: f32,
    /// Left and right edges of the fields.
    left: f32,
    right: f32,
}

impl Geometry {
    fn of(layout: &Layout, replace_mode: bool) -> Self {
        let lh = layout.metrics.line_height;
        let gap = layout.margin;
        let header = layout.tabs_h.max(lh);
        let mut y = header + gap;
        let mut next = || {
            let top = y;
            y += lh + gap;
            top
        };
        let query = next();
        let toggles = next();
        let replace = replace_mode.then(&mut next);
        let include = next();
        let exclude = next();
        let count = next();
        Self {
            query,
            replace,
            include,
            exclude,
            toggles,
            count,
            list: y - gap / 2.0,
            left: layout.margin,
            right: layout.left - layout.margin,
        }
    }

    fn visible_rows(&self, layout: &Layout) -> usize {
        let h = layout.bottom - self.list;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a small non-negative row count"
        )]
        let rows = (h / layout.metrics.line_height).floor().max(1.0) as usize;
        rows
    }

    /// Width of the replacement field: room for the "All" button after it.
    fn replace_field_right(&self, layout: &Layout) -> f32 {
        self.right - 5.0 * layout.metrics.cell_width
    }
}

/// Columns a toggle's label takes, and the gap after it.
const TOGGLE_COLS: f32 = 3.0;
const TOGGLE_GAP: f32 = 1.0;

/// The toggles in drawing order, with their labels.
const TOGGLES: [(Toggle, &str); 4] = [
    (Toggle::Case, "Aa"),
    (Toggle::Regex, ".*"),
    (Toggle::Word, "ab"),
    (Toggle::Replace, "Replace"),
];

/// What each toggle does, for the tooltip shown while the pointer rests
/// on it; the labels alone ("Aa", ".*", "ab") are too terse to guess.
fn describe(toggle: Toggle) -> &'static str {
    match toggle {
        Toggle::Case => "Match case",
        Toggle::Regex => "Regular expression",
        Toggle::Word => "Whole words only",
        Toggle::Replace => "Show the replace field",
    }
}

/// Label width in columns: three for the short ones, the text's length for
/// "Replace".
fn toggle_cols(label: &str) -> f32 {
    f(label.chars().count().max(2)).max(TOGGLE_COLS)
}

#[expect(clippy::cast_precision_loss, reason = "small column counts")]
fn f(n: usize) -> f32 {
    n as f32
}

impl Panel {
    /// The query as typed, with its options.
    #[must_use]
    pub fn query(&self) -> Query {
        Query {
            pattern: field_text(&self.query),
            regex: self.regex,
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
        }
    }

    /// The replacement as typed.
    #[must_use]
    pub fn replacement(&self) -> String {
        field_text(&self.replace)
    }

    /// The include and exclude globs as typed.
    #[must_use]
    pub fn globs(&self) -> (String, String) {
        (field_text(&self.include), field_text(&self.exclude))
    }

    /// The focused field, if a field has focus.
    pub fn field_mut(&mut self) -> Option<&mut Document> {
        Some(match self.focus {
            Focus::Query => &mut self.query,
            Focus::Replace => &mut self.replace,
            Focus::Include => &mut self.include,
            Focus::Exclude => &mut self.exclude,
            Focus::List => return None,
        })
    }

    /// Put the keyboard in the query field, selecting its text so typing
    /// replaces it; with `text`, the query is that first.
    pub fn focus_query(&mut self, text: Option<String>) {
        if let Some(text) = text.filter(|t| !t.contains('\n') && t.len() <= 200) {
            self.query = Document::untitled();
            let _ = self.query.insert(&text);
        }
        self.query.select_all();
        self.focus = Focus::Query;
    }

    /// Tab: the next part in order; Shift+Tab the previous.
    pub fn cycle_focus(&mut self, backwards: bool) {
        let order: &[Focus] = if self.replace_mode {
            &[
                Focus::Query,
                Focus::Replace,
                Focus::Include,
                Focus::Exclude,
                Focus::List,
            ]
        } else {
            &[Focus::Query, Focus::Include, Focus::Exclude, Focus::List]
        };
        let i = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        let n = order.len();
        self.focus = order[if backwards {
            (i + n - 1) % n
        } else {
            (i + 1) % n
        }];
        if let Some(field) = self.field_mut() {
            field.select_all();
        }
    }

    /// Flip a toggle. Returns whether a new search is needed.
    pub fn toggle(&mut self, which: Toggle) -> bool {
        match which {
            Toggle::Case => self.case_sensitive = !self.case_sensitive,
            Toggle::Regex => self.regex = !self.regex,
            Toggle::Word => self.whole_word = !self.whole_word,
            Toggle::Replace => {
                self.replace_mode = !self.replace_mode;
                if !self.replace_mode && self.focus == Focus::Replace {
                    self.focus = Focus::Query;
                }
                return false;
            }
        }
        true
    }

    /// The parameters a search now would use, under `root`.
    #[must_use]
    pub fn params(&self, root: &std::path::Path, show_ignored: bool) -> Params {
        let (include, exclude) = self.globs();
        Params {
            query: self.query(),
            root: root.to_path_buf(),
            include,
            exclude,
            show_ignored,
        }
    }

    /// Start a search with `params`, cancelling any running one. The
    /// results are empty until batches arrive through [`Panel::receive`].
    /// An empty pattern clears the results instead.
    pub fn start(
        &mut self,
        params: Params,
        snapshots: find_files::Snapshots,
        send: impl Fn(Batch) -> bool + Send + Sync + 'static,
    ) {
        self.generation.bump();
        self.selected = None;
        self.scroll = 0;
        self.rows.clear();
        if params.query.pattern.is_empty() {
            self.results = None;
            self.params = None;
            self.error = None;
            return;
        }
        match find_files::start(&params, snapshots, &self.generation, send) {
            Ok(generation) => {
                self.results = Some(Results::new(generation, params.root.clone()));
                self.error = None;
            }
            Err(e) => {
                self.results = None;
                self.error = Some(e);
            }
        }
        self.params = Some(params);
    }

    /// Forget the results and the query.
    pub fn clear(&mut self) {
        self.generation.bump();
        self.results = None;
        self.params = None;
        self.error = None;
        self.rows.clear();
        self.selected = None;
        self.scroll = 0;
        self.query = Document::untitled();
        self.focus = Focus::Query;
    }

    /// Take a batch from a worker. Returns `false` if it was stale.
    pub fn receive(&mut self, batch: Batch) -> bool {
        let Some(results) = &mut self.results else {
            return false;
        };
        if !results.receive(batch) {
            return false;
        }
        self.rebuild_rows();
        true
    }

    /// The list's rows as the results stand.
    fn rebuild_rows(&mut self) {
        self.rows.clear();
        if let Some(results) = &self.results {
            for (i, file) in results.files.iter().enumerate() {
                self.rows.push(Row::File(i));
                for j in 0..file.hits.len() {
                    self.rows.push(Row::Hit(i, j));
                }
            }
        }
    }

    /// The rows of the list.
    #[cfg(test)]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The selected hit, if the selected row is one.
    #[must_use]
    pub fn selected_hit(&self) -> Option<(usize, usize)> {
        match self.selected.and_then(|r| self.rows.get(r))? {
            Row::Hit(i, j) => Some((*i, *j)),
            Row::File(_) => None,
        }
    }

    /// The count line: how many hits in how many files, and what else the
    /// user should know.
    #[must_use]
    pub fn status(&self) -> String {
        if let Some(e) = &self.error {
            return e.lines().last().unwrap_or("invalid pattern").to_owned();
        }
        if self.replacing {
            return "Replacing…".to_owned();
        }
        let Some(r) = &self.results else {
            return String::new();
        };
        if r.capped {
            return format!(
                "more than {} matches, narrow the search",
                find_files::MAX_HITS
            );
        }
        let plural =
            |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let mut s = if r.hits == 0 && r.done {
            "no matches".to_owned()
        } else {
            format!(
                "{} in {}",
                plural(r.hits, "match", "matches"),
                plural(r.files.len(), "file", "files")
            )
        };
        if !r.done {
            s.push_str(", still searching…");
        }
        if r.skipped > 0 {
            let _ = write!(s, ", {} skipped", plural(r.skipped, "file", "files"));
        }
        if r.errors > 0 {
            let _ = write!(s, ", {} unreadable", r.errors);
        }
        s
    }

    /// Scroll so row `row` is on screen.
    fn show_row(&mut self, row: usize, layout: &Layout) {
        let rows = Geometry::of(layout, self.replace_mode).visible_rows(layout);
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll + rows {
            self.scroll = row + 1 - rows;
        }
    }

    fn clamp_scroll(&mut self, layout: &Layout) {
        let rows = Geometry::of(layout, self.replace_mode).visible_rows(layout);
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(rows));
    }

    /// Scroll by `rows` (negative is up).
    pub fn scroll_by(&mut self, rows: isize, layout: &Layout) {
        self.scroll = self.scroll.saturating_add_signed(rows);
        self.clamp_scroll(layout);
    }

    /// Scroll by a wheel movement of `dy` physical pixels.
    pub fn wheel(&mut self, dy: f32, layout: &Layout) {
        self.wheel += dy / layout.metrics.line_height;
        let whole = self.wheel.trunc();
        self.wheel -= whole;
        #[expect(clippy::cast_possible_truncation, reason = "a small row count")]
        self.scroll_by(whole as isize, layout);
    }

    /// Select row `row` and show it.
    pub fn select(&mut self, row: usize, layout: &Layout) {
        if self.rows.is_empty() {
            self.selected = None;
            return;
        }
        let row = row.min(self.rows.len() - 1);
        self.selected = Some(row);
        self.show_row(row, layout);
    }

    /// Move the selection in the list.
    pub fn move_selection(&mut self, motion: Motion, layout: &Layout) {
        if self.rows.is_empty() {
            return;
        }
        let page = Geometry::of(layout, self.replace_mode)
            .visible_rows(layout)
            .saturating_sub(1)
            .max(1);
        let last = self.rows.len() - 1;
        let current = self.selected.unwrap_or(0);
        let target = match motion {
            Motion::Up => current.saturating_sub(1),
            Motion::Down => current + 1,
            Motion::PageUp => current.saturating_sub(page),
            Motion::PageDown => current + page,
            Motion::DocStart | Motion::LineStart => 0,
            Motion::DocEnd | Motion::LineEnd => last,
            Motion::Left | Motion::Right | Motion::WordLeft | Motion::WordRight => current,
        };
        self.select(if self.selected.is_none() { 0 } else { target }, layout);
    }

    /// Select the next (or previous) hit row from the selection, wrapping
    /// around, and return it. Returns `None` when there are no hits.
    pub fn step(&mut self, backwards: bool, layout: &Layout) -> Option<(usize, usize)> {
        if self.rows.is_empty() {
            return None;
        }
        let n = self.rows.len();
        let mut row = self
            .selected
            .map_or(if backwards { n - 1 } else { 0 }, |s| {
                if backwards {
                    (s + n - 1) % n
                } else {
                    (s + 1) % n
                }
            });
        for _ in 0..n {
            if let Row::Hit(i, j) = self.rows[row] {
                self.select(row, layout);
                return Some((i, j));
            }
            row = if backwards {
                (row + n - 1) % n
            } else {
                (row + 1) % n
            };
        }
        None
    }

    /// Tick or untick a row: a file row changes all of its hits.
    pub fn toggle_tick(&mut self, row: usize) {
        let Some(results) = &mut self.results else {
            return;
        };
        match self.rows.get(row) {
            Some(Row::File(i)) => {
                if let Some(file) = results.files.get_mut(*i) {
                    let all = file.hits.iter().all(|h| h.ticked);
                    for h in &mut file.hits {
                        h.ticked = !all;
                    }
                }
            }
            Some(Row::Hit(i, j)) => {
                if let Some(h) = results.files.get_mut(*i).and_then(|f| f.hits.get_mut(*j)) {
                    h.ticked = !h.ticked;
                }
            }
            None => {}
        }
    }

    /// What is at `(x, y)` of the panel.
    #[must_use]
    /// Note the pointer's position for the toggle tooltips. Returns `true`
    /// when the tooltip changed and the panel needs drawing again.
    pub fn hover_at(&mut self, layout: &Layout, x: f32, y: f32) -> bool {
        let hover = match self.hit(layout, x, y) {
            Target::Toggle(t) if x < layout.left => Some(t),
            _ => None,
        };
        let changed = hover != self.hover;
        self.hover = hover;
        changed
    }

    /// The pointer left the window: no tooltip. Returns `true` when one
    /// was showing.
    pub fn hover_gone(&mut self) -> bool {
        self.hover.take().is_some()
    }

    pub fn hit(&self, layout: &Layout, x: f32, y: f32) -> Target {
        let g = Geometry::of(layout, self.replace_mode);
        let lh = layout.metrics.line_height;
        let cell = layout.metrics.cell_width;
        let in_row = |top: f32| y >= top && y < top + lh;
        if in_row(g.query) {
            return Target::Field(Focus::Query);
        }
        if in_row(g.include) {
            return Target::Field(Focus::Include);
        }
        if in_row(g.exclude) {
            return Target::Field(Focus::Exclude);
        }
        if let Some(top) = g.replace
            && in_row(top)
        {
            return if x >= g.replace_field_right(layout) {
                Target::ReplaceAll
            } else {
                Target::Field(Focus::Replace)
            };
        }
        if in_row(g.toggles) {
            let mut tx = g.left;
            for (toggle, label) in TOGGLES {
                let w = toggle_cols(label) * cell;
                if x >= tx && x < tx + w {
                    return Target::Toggle(toggle);
                }
                tx += (toggle_cols(label) + TOGGLE_GAP) * cell;
            }
            return Target::Nothing;
        }
        if y < g.list || y >= layout.bottom {
            return Target::Nothing;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a small non-negative row index"
        )]
        let row = ((y - g.list) / lh).floor() as usize + self.scroll;
        if row >= self.rows.len() {
            return Target::Nothing;
        }
        if self.replace_mode && x < g.left + 4.0 * cell {
            return Target::Checkbox(row);
        }
        Target::Row(row)
    }

    /// A click at `(x, y)`: focus a field, flip a toggle, or select and
    /// open a row.
    pub fn click(&mut self, layout: &Layout, x: f32, y: f32) -> Reply {
        let mut reply = Reply::default();
        match self.hit(layout, x, y) {
            Target::Field(focus) => self.focus = focus,
            Target::Toggle(t) => reply.search = self.toggle(t),
            Target::ReplaceAll => reply.replace_all = true,
            Target::Row(row) => {
                self.focus = Focus::List;
                self.select(row, layout);
                reply.open = self.selected_hit();
            }
            Target::Checkbox(row) => {
                self.focus = Focus::List;
                self.select(row, layout);
                self.toggle_tick(row);
            }
            Target::Nothing => {}
        }
        reply
    }
}

/// Draw the panel in the sidebar column. `focused` is whether the sidebar
/// has the keyboard, `blink` whether the text cursor is in its visible
/// phase.
pub fn draw(gpu: &mut Gpu, ui: &Ui, layout: &Layout, panel: &Panel, focused: bool, blink: bool) {
    let geo = Geometry::of(layout, panel.replace_mode);
    draw_fields(gpu, ui, layout, panel, &geo, focused, blink);
    draw_toggles(gpu, ui, layout, panel, &geo);
    let status = panel.status();
    let color = if panel.error.is_some() {
        ui.error
    } else {
        ui.gutter
    };
    gpu.push_text_line([geo.left, geo.count], &status, color);
    if let Some(results) = &panel.results {
        draw_list(gpu, ui, layout, (panel, results), &geo, focused);
    }
    if let Some(toggle) = panel.hover {
        draw_tooltip(gpu, ui, layout, panel, &geo, toggle);
    }
}

/// A small box under the toggle the pointer rests on, saying what it does
/// and whether it is on. It covers the row beneath (the replace field and
/// its All button, or the include globs), and within one layer every
/// rectangle draws under every glyph, so the box gets a layer of its own;
/// it is kept inside the sidebar's width.
fn draw_tooltip(
    gpu: &mut Gpu,
    ui: &Ui,
    layout: &Layout,
    panel: &Panel,
    geo: &Geometry,
    toggle: Toggle,
) {
    let (cell, lh) = (layout.metrics.cell_width, layout.metrics.line_height);
    let on = match toggle {
        Toggle::Case => panel.case_sensitive,
        Toggle::Regex => panel.regex,
        Toggle::Word => panel.whole_word,
        Toggle::Replace => panel.replace_mode,
    };
    let text = format!("{} ({})", describe(toggle), if on { "on" } else { "off" });
    let mut left = geo.left;
    for (t, label) in TOGGLES {
        if t == toggle {
            break;
        }
        left += (toggle_cols(label) + TOGGLE_GAP) * cell;
    }
    let pad = cell / 2.0;
    let width = f(text.chars().count()) * cell + 2.0 * pad;
    let left = left.min(layout.left - width - layout.margin).max(0.0);
    let top = geo.toggles + lh + layout.margin / 2.0;
    let border = layout.scale.round().max(1.0);
    gpu.begin_layer(Some([0.0, 0.0, layout.left, layout.bottom]));
    gpu.push_rect([left, top], [width, lh + 2.0 * border], ui.picker_border);
    gpu.push_rect(
        [left + border, top + border],
        [width - 2.0 * border, lh],
        ui.picker,
    );
    gpu.push_text_line([left + pad, top + border], &text, ui.text);
}

/// The four fields, with placeholders while empty and unfocused.
fn draw_fields(
    gpu: &mut Gpu,
    ui: &Ui,
    layout: &Layout,
    panel: &Panel,
    geo: &Geometry,
    focused: bool,
    blink: bool,
) {
    let metrics = layout.metrics;
    let (cell, lh) = (metrics.cell_width, metrics.line_height);
    let border = layout.scale.round().max(1.0);
    let fields = [
        (Focus::Query, geo.query, geo.right, "Search"),
        (Focus::Include, geo.include, geo.right, "files to include"),
        (Focus::Exclude, geo.exclude, geo.right, "files to exclude"),
    ];
    let replace = geo.replace.map(|top| {
        (
            Focus::Replace,
            top,
            geo.replace_field_right(layout),
            "Replace",
        )
    });
    for (focus, top, right, placeholder) in fields.into_iter().chain(replace) {
        let width = (right - geo.left).max(2.0 * cell);
        let is_focused = focused && panel.focus == focus;
        if is_focused {
            gpu.push_rect(
                [geo.left - border, top - border],
                [width + 2.0 * border, lh + 2.0 * border],
                ui.field_focus,
            );
        }
        gpu.push_rect([geo.left, top], [width, lh], ui.field);
        let doc = match focus {
            Focus::Query => &panel.query,
            Focus::Replace => &panel.replace,
            Focus::Include => &panel.include,
            Focus::Exclude => &panel.exclude,
            Focus::List => continue,
        };
        if doc.buffer.len_bytes() == 0 && !is_focused {
            gpu.push_text_line([geo.left + cell / 2.0, top], placeholder, ui.gutter);
        } else {
            view::draw_field(
                gpu,
                ui,
                doc,
                [geo.left, top],
                width,
                layout,
                is_focused && blink,
            );
        }
    }
    if let Some(top) = geo.replace {
        // The "All" button right of the replacement field.
        let x = geo.replace_field_right(layout) + cell;
        gpu.push_rect([x - cell / 2.0, top], [4.0 * cell, lh], ui.toggle_on);
        gpu.push_text_line([x, top], "All", ui.text);
    }
}

/// The option toggles, lit when on.
fn draw_toggles(gpu: &mut Gpu, ui: &Ui, layout: &Layout, panel: &Panel, geo: &Geometry) {
    let (cell, lh) = (layout.metrics.cell_width, layout.metrics.line_height);
    let mut x = geo.left;
    for (toggle, label) in TOGGLES {
        let on = match toggle {
            Toggle::Case => panel.case_sensitive,
            Toggle::Regex => panel.regex,
            Toggle::Word => panel.whole_word,
            Toggle::Replace => panel.replace_mode,
        };
        let cols = toggle_cols(label) * cell;
        if on {
            gpu.push_rect([x - cell / 4.0, geo.toggles], [cols, lh], ui.toggle_on);
        }
        gpu.push_text_line([x, geo.toggles], label, ui.text);
        x += (toggle_cols(label) + TOGGLE_GAP) * cell;
    }
}

/// The results, grouped by file; only the rows on screen are drawn.
fn draw_list(
    gpu: &mut Gpu,
    ui: &Ui,
    layout: &Layout,
    (panel, results): (&Panel, &Results),
    geo: &Geometry,
    focused: bool,
) {
    let (cell, lh) = (layout.metrics.cell_width, layout.metrics.line_height);
    let width = layout.left;
    let mut y = geo.list;
    let indent = if panel.replace_mode { 4.0 * cell } else { 0.0 };
    for (row_index, row) in panel.rows.iter().enumerate().skip(panel.scroll) {
        if y >= layout.bottom {
            break;
        }
        if panel.selected == Some(row_index) {
            let color = if focused && panel.focus == Focus::List {
                ui.picker_selected
            } else {
                ui.selection
            };
            gpu.push_rect([0.0, y], [width, lh], color);
        }
        match *row {
            Row::File(i) => {
                let Some(file) = results.files.get(i) else {
                    continue;
                };
                if panel.replace_mode {
                    let all = file.hits.iter().all(|h| h.ticked);
                    let any = file.hits.iter().any(|h| h.ticked);
                    let mark = if all {
                        "[x]"
                    } else if any {
                        "[-]"
                    } else {
                        "[ ]"
                    };
                    gpu.push_text_line([geo.left, y], mark, ui.gutter);
                }
                let dot = if file.dirty { " ●" } else { "" };
                let text = format!("{}{dot}", file.rel);
                gpu.push_text_line([geo.left + indent, y], &text, ui.search_file);
                let count = file.hits.len().to_string();
                let cx = width - layout.margin - f(count.len()) * cell;
                gpu.push_rect(
                    [cx - cell / 2.0, y],
                    [f(count.len() + 1) * cell, lh],
                    ui.tab_bar,
                );
                gpu.push_text_line([cx, y], &count, ui.gutter);
            }
            Row::Hit(i, j) => {
                let Some(hit) = results.files.get(i).and_then(|f| f.hits.get(j)) else {
                    continue;
                };
                if panel.replace_mode {
                    let mark = if hit.ticked { "[x]" } else { "[ ]" };
                    gpu.push_text_line([geo.left, y], mark, ui.gutter);
                }
                let preview = results.preview(hit);
                let number = format!("{}:", hit.line + 1);
                let x0 = geo.left + indent;
                gpu.push_text_line([x0, y], &number, ui.gutter);
                let tx = x0 + f(number.len() + 1) * cell;
                // The match's background, then the text over it.
                let from = column::col_at(preview, hit.mark.start, 4);
                let to = column::col_at(preview, hit.mark.end, 4);
                gpu.push_rect(
                    [tx + f(from) * cell, y],
                    [f(to.saturating_sub(from).max(1)) * cell, lh],
                    ui.search_match,
                );
                let mut shown = preview.to_owned();
                if panel.replace_mode {
                    // The replacement after the match, so the change reads
                    // "old→new".
                    let rep = panel.replacement();
                    let after = f(to) * cell;
                    let rep_cols = column::line_width(&rep, 4);
                    gpu.push_rect(
                        [tx + after, y],
                        [f(rep_cols.max(1) + 1) * cell, lh],
                        ui.toggle_on,
                    );
                    shown = format!(
                        "{}→{rep}{}",
                        &preview[..hit.mark.end],
                        &preview[hit.mark.end..]
                    );
                }
                gpu.push_text_span([tx, y], &shown, 0, ui.text, 0.0);
            }
        }
        y += lh;
    }
}

/// The clickable area of the Replace All button, for tests.
#[cfg(test)]
fn replace_all_at(layout: &Layout, panel: &Panel) -> Option<(f32, f32)> {
    let g = Geometry::of(layout, panel.replace_mode);
    g.replace
        .map(|top| (g.replace_field_right(layout) + 1.0, top + 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::find_files::{FileBatch, RawHit};
    use nimbus_render::Metrics;
    use std::path::PathBuf;

    fn layout() -> Layout {
        let metrics = Metrics {
            size_px: 14.0,
            line_height: 20.0,
            baseline: 15.0,
            cell_width: 10.0,
        };
        Layout::new(metrics, 800, 400, 1.0, 10)
            .with_chrome(true, 0, false)
            .with_sidebar(240.0)
    }

    fn hit(line: usize, start: usize) -> RawHit {
        RawHit {
            line,
            range: start..start + 3,
            preview: "a foo b".into(),
            mark: 2..5,
        }
    }

    /// A panel with results: two files, three hits.
    fn panel() -> Panel {
        let mut p = Panel::default();
        let _ = p.query.insert("foo");
        p.results = Some(Results::new(1, PathBuf::from("/r")));
        p.receive(Batch {
            generation: 1,
            items: vec![
                FileBatch {
                    path: "/r/a.rs".into(),
                    dirty: false,
                    hits: vec![hit(0, 2), hit(4, 40)],
                },
                FileBatch {
                    path: "/r/b.rs".into(),
                    dirty: true,
                    hits: vec![hit(9, 2)],
                },
            ],
            searched: 5,
            skipped: 1,
            errors: 0,
            capped: false,
            done: true,
        });
        p
    }

    #[test]
    fn rows_group_hits_under_their_files() {
        let p = panel();
        assert_eq!(
            p.rows(),
            &[
                Row::File(0),
                Row::Hit(0, 0),
                Row::Hit(0, 1),
                Row::File(1),
                Row::Hit(1, 0)
            ]
        );
        assert_eq!(p.status(), "3 matches in 2 files, 1 file skipped");
        let stale = Batch {
            generation: 0,
            ..Batch::default()
        };
        let mut p = p;
        assert!(!p.receive(stale));
    }

    #[test]
    fn keys_move_through_the_list_and_f4_steps_over_file_rows() {
        let l = layout();
        let mut p = panel();
        p.focus = Focus::List;
        p.move_selection(Motion::Down, &l);
        assert_eq!(p.selected, Some(0), "nothing selected: the first row");
        p.move_selection(Motion::Down, &l);
        assert_eq!(p.selected_hit(), Some((0, 0)));
        assert_eq!(p.step(false, &l), Some((0, 1)));
        assert_eq!(p.step(false, &l), Some((1, 0)), "skips the file row");
        assert_eq!(p.step(false, &l), Some((0, 0)), "wraps");
        assert_eq!(p.step(true, &l), Some((1, 0)), "wraps backwards");
        p.move_selection(Motion::DocEnd, &l);
        assert_eq!(p.selected, Some(4));
        assert!(Panel::default().step(false, &l).is_none());
    }

    #[test]
    fn the_pointer_over_a_toggle_shows_its_tooltip() {
        let l = layout();
        let mut p = panel();
        let geo = Geometry::of(&l, false);
        assert!(p.hover_at(&l, geo.left + 1.0, geo.toggles + 1.0));
        assert_eq!(p.hover, Some(Toggle::Case));
        assert_eq!(describe(Toggle::Case), "Match case");
        // Still there: nothing to redraw.
        assert!(!p.hover_at(&l, geo.left + 2.0, geo.toggles + 1.0));
        // The second toggle.
        assert!(p.hover_at(&l, geo.left + (3.0 + 1.0) * 10.0 + 1.0, geo.toggles + 1.0));
        assert_eq!(p.hover, Some(Toggle::Regex));
        // Off the row, and past the sidebar's edge.
        assert!(p.hover_at(&l, geo.left + 1.0, geo.query + 1.0));
        assert_eq!(p.hover, None);
        assert!(!p.hover_at(&l, l.left + 5.0, geo.toggles + 1.0));
        assert!(!p.hover_gone());
    }

    #[test]
    fn clicks_focus_fields_flip_toggles_and_open_rows() {
        let l = layout();
        let mut p = panel();
        let geo = Geometry::of(&l, false);
        assert_eq!(p.click(&l, 20.0, geo.include + 1.0), Reply::default());
        assert_eq!(p.focus, Focus::Include);
        let reply = p.click(&l, geo.left + 1.0, geo.toggles + 1.0);
        assert!(reply.search && p.case_sensitive);
        // The fourth toggle shows the replace field, which moves the list.
        let reply = p.click(
            &l,
            geo.left + 3.0 * (3.0 + 1.0) * 10.0 + 1.0,
            geo.toggles + 1.0,
        );
        assert!(!reply.search && p.replace_mode);
        let geo = Geometry::of(&l, true);
        assert!(geo.replace.is_some());
        let reply = p.click(&l, 100.0, geo.list + 1.0 * 20.0 + 1.0);
        assert_eq!(reply.open, Some((0, 0)));
        assert_eq!(p.focus, Focus::List);
        // In replace mode the first columns are the checkbox.
        p.click(&l, 10.0, geo.list + 1.0 * 20.0 + 1.0);
        assert!(!p.results.as_ref().unwrap().files[0].hits[0].ticked);
        p.click(&l, 10.0, geo.list + 1.0);
        let results = p.results.as_ref().unwrap();
        assert!(
            results.files[0].hits.iter().all(|h| h.ticked),
            "file row ticks all"
        );
        let (at_x, at_y) = replace_all_at(&l, &p).unwrap();
        assert!(p.click(&l, at_x, at_y).replace_all);
        assert_eq!(p.hit(&l, 10.0, 399.0), Target::Nothing);
    }

    #[test]
    fn tab_cycles_the_fields_then_the_list() {
        let mut p = Panel::default();
        p.cycle_focus(false);
        assert_eq!(p.focus, Focus::Include);
        p.cycle_focus(false);
        p.cycle_focus(false);
        assert_eq!(p.focus, Focus::List);
        p.cycle_focus(false);
        assert_eq!(p.focus, Focus::Query);
        p.replace_mode = true;
        p.cycle_focus(false);
        assert_eq!(p.focus, Focus::Replace);
        p.cycle_focus(true);
        assert_eq!(p.focus, Focus::Query);
    }

    #[test]
    fn ticked_hits_are_grouped_by_file_and_status_reports_the_cap() {
        let mut p = panel();
        p.toggle_tick(2);
        let r = p.results.as_ref().unwrap();
        assert_eq!(r.ticked(), vec![(0, vec![0]), (1, vec![0])]);
        p.results.as_mut().unwrap().capped = true;
        assert!(p.status().starts_with("more than"));
        p.error = Some("invalid pattern: x".into());
        assert_eq!(p.status(), "invalid pattern: x");
        p.clear();
        assert!(p.results.is_none() && p.rows().is_empty());
    }
}
