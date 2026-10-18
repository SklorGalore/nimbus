//! The Markdown preview (M20): a Markdown file drawn as a page, in a
//! column beside its source, kept in step with it as it is edited and
//! scrolled.
//!
//! **Layout off the main thread.** Laying out a page means parsing the
//! Markdown, colouring its code blocks and measuring every word to wrap
//! it, which for a long document takes milliseconds. So [`lay_out`] runs
//! on a worker with a snapshot of the text, and the result, a [`Page`],
//! is just positions: runs of text at an x and y, and rectangles. Drawing
//! a frame then only walks the runs that are on screen.
//!
//! **Waiting for a pause.** While the source is being typed in, the page
//! is laid out again only once typing has paused for [`PAUSE`], so a fast
//! typist does not start a layout per keystroke. A new width or font lays
//! out at once.
//!
//! **Two layers of Markdown.** The page's shape (headings, paragraphs,
//! lists, code blocks, quotes, tables) comes from the syntax tree, through
//! [`nimbus_syntax::markdown::blocks`]. What is inside a paragraph
//! (emphasis, code, links) is read here by [`inline`], a small scanner:
//! the preview needs to know only where styles start and end, which the
//! editor's own inline grammar gives as colours, not structure.
//!
//! **Scrolling in step.** Each block remembers where it starts in the
//! source and where it was placed on the page ([`Page::anchors`]). When
//! the source view scrolls, the preview scrolls to the same place,
//! interpolating within a block.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nimbus_core::{Buffer, RopeBuffer, Snapshot, Text};
use nimbus_render::{ColorRun, Gpu, UiFont, UiStyle};
use nimbus_syntax::LanguageId;
use nimbus_syntax::highlight::Highlighter;
use nimbus_syntax::markdown::{self, Block, Kind, Outline};
use nimbus_syntax::state::SyntaxState;

use crate::theme::Theme;
use crate::view::Layout;

/// How long typing must pause before the page is laid out again.
pub const PAUSE: Duration = Duration::from_millis(150);

/// The largest file the preview lays out: above it there is no syntax
/// tree to read the blocks from (`DESIGN.md` §4.2).
pub const MAX_BYTES: usize = 16 * 1024 * 1024;

/// What a run or rectangle is painted with, resolved against the theme
/// when drawn, so a theme change needs no new layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paint {
    /// Body text.
    Text,
    /// Secondary text: placeholders, a list's numbers, a table's lines.
    Muted,
    /// A heading.
    Heading,
    /// A link.
    Link,
    /// Code, in a code span or a code block.
    Code,
    /// Behind code.
    CodeBack,
    /// Rules and a quote's bar.
    Line,
}

/// A run of text on the page, all in one style, on one line.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// Left edge, from the page's left.
    pub x: f32,
    /// Top of its line, from the page's top.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height of its line.
    pub h: f32,
    /// Its text, in [`Page::text`].
    pub text: Range<usize>,
    /// Size, weight and slant.
    pub style: UiStyle,
    /// Drawn by the editor font on its grid: a code block's line.
    pub mono: bool,
    /// Colour.
    pub paint: Paint,
    /// The link it is part of, in [`Page::urls`].
    pub link: Option<u32>,
    /// Struck through.
    pub strike: bool,
}

/// A rectangle on the page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// `[x, y, width, height]` from the page's top-left corner.
    pub at: [f32; 4],
    /// Colour.
    pub paint: Paint,
}

/// A syntax colour within a code block's line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeSpan {
    /// The run, in [`Page::runs`].
    pub run: u32,
    /// Bytes within the run's text.
    pub start: u32,
    /// End byte within the run's text, exclusive.
    pub end: u32,
    /// The capture table and capture that colour it.
    pub table: u32,
    /// Capture index in the table.
    pub capture: u32,
}

/// A laid-out page: everything drawing needs, in page coordinates.
#[derive(Debug, Default)]
pub struct Page {
    /// The text the runs draw.
    pub text: String,
    /// Runs, in reading order, by line top.
    pub runs: Vec<Run>,
    /// Rectangles: code backgrounds, rules, quote bars, table lines.
    pub rects: Vec<Rect>,
    /// Syntax colours of code-block runs, by run.
    pub spans: Vec<CodeSpan>,
    /// The links' targets.
    pub urls: Vec<String>,
    /// Where each block starts in the source, and the y it was placed at.
    pub anchors: Vec<(usize, f32)>,
    /// The page's height.
    pub height: f32,
}

impl Page {
    /// The page's y for source byte `pos`: within the block holding it,
    /// as far down as `pos` is through the block.
    #[must_use]
    pub fn y_for(&self, pos: usize) -> f32 {
        let i = self.anchors.partition_point(|&(s, _)| s <= pos);
        let Some(&(s0, y0)) = i.checked_sub(1).and_then(|i| self.anchors.get(i)) else {
            return 0.0;
        };
        let Some(&(s1, y1)) = self.anchors.get(i) else {
            return y0;
        };
        let share = f(pos - s0) / f((s1 - s0).max(1));
        y0 + share * (y1 - y0)
    }

    /// The link under page point `(x, y)`, if any.
    #[must_use]
    pub fn link_at(&self, x: f32, y: f32) -> Option<&str> {
        let first = self.runs.partition_point(|r| r.y + r.h <= y);
        self.runs
            .get(first..)?
            .iter()
            .take_while(|r| r.y <= y)
            .find(|r| r.link.is_some() && x >= r.x && x < r.x + r.w)
            .and_then(|r| self.urls.get(r.link? as usize))
            .map(String::as_str)
    }
}

/// What a page is laid out for.
#[derive(Debug, Clone, Copy)]
pub struct Params {
    /// The page's width.
    pub width: f32,
    /// The font and size.
    pub font: UiFont,
}

/// What a page was laid out for: the text's version, the width and the
/// font's size and cell (which change with the font).
pub type Key = (u64, u32, u32, u32, bool);

fn key_of(version: u64, p: &Params) -> Key {
    let m = p.font.editor();
    (
        version,
        p.width.to_bits(),
        m.size_px.to_bits(),
        m.cell_width.to_bits(),
        p.font.is_proportional(),
    )
}

/// A layout to run on a worker.
pub struct Job {
    /// What it is for.
    pub key: Key,
    text: Box<dyn Snapshot>,
    /// The document's own tree, which the editor keeps up to date as it
    /// is edited: copying it saves parsing the whole file again.
    outline: Outline,
    params: Params,
    cache: CodeCache,
}

impl Job {
    /// Lay the page out.
    #[must_use]
    pub fn run(self) -> Laid {
        let started = Instant::now();
        let mut cache = self.cache;
        let page = lay_out(&*self.text, &self.outline, &self.params, &mut cache);
        log::debug!(
            "preview laid out {} bytes in {:.1} ms",
            self.text.len_bytes(),
            started.elapsed().as_secs_f64() * 1000.0
        );
        Laid {
            key: self.key,
            page,
            cache,
        }
    }
}

/// A page laid out on a worker, for the preview whose job had `key`.
#[derive(Debug)]
pub struct Laid {
    /// What it was laid out for.
    pub key: Key,
    /// The page.
    pub page: Page,
    /// Code blocks' colours, for the next layout.
    cache: CodeCache,
}

/// One preview tab's page and scroll position.
#[derive(Debug, Default)]
pub struct Preview {
    page: Page,
    /// What the page shows; `None` before the first layout arrives.
    laid: Option<Key>,
    /// A layout under way.
    asked: Option<Key>,
    /// The text's version last seen, and when it was first seen: the
    /// page waits for [`PAUSE`] after the last change.
    seen: Option<(u64, Instant)>,
    /// Scroll position: the page's y at the top of the column.
    pub top: f32,
    /// The source view's top byte last followed.
    followed: Option<usize>,
    /// Reused for code lines' colours.
    colours: Vec<ColorRun>,
    /// Code blocks' colours from the last layout, by language and text:
    /// a block that did not change is not parsed again.
    cache: CodeCache,
    /// Why there is no page, when there is none.
    note: &'static str,
}

impl Preview {
    /// A job laying out the page again, if it is out of date: at once for
    /// a new width or font, after [`PAUSE`] for an edit. The second value
    /// is when to look again, while waiting for the pause.
    pub fn wants<B: Buffer>(
        &mut self,
        (text, version, syntax): (&B, u64, &SyntaxState),
        params: Params,
        now: Instant,
    ) -> (Option<Job>, Option<Instant>) {
        let key = key_of(version, &params);
        if self.laid == Some(key) || self.asked == Some(key) {
            return (None, None);
        }
        let since = match self.seen {
            Some((v, t)) if v == version => t,
            _ => {
                self.seen = Some((version, now));
                now
            }
        };
        let edited = self.laid.is_some_and(|k| k.0 != version);
        let due = since + PAUSE;
        if edited && now < due {
            return (None, Some(due));
        }
        if text.len_bytes() > MAX_BYTES {
            self.note = "Too large to preview";
            return (None, None);
        }
        // Wait for the tree to catch up with the text: the document's
        // parse is under way, and its end asks for a frame.
        let Some(outline) = markdown::outline(syntax) else {
            self.note = "Reading the file…";
            return (None, None);
        };
        self.asked = Some(key);
        let job = Job {
            key,
            text: text.snapshot(),
            outline,
            params,
            cache: std::mem::take(&mut self.cache),
        };
        (Some(job), None)
    }

    /// Take a page laid out on a worker, if it is for this preview's
    /// latest request. Returns whether it was taken.
    pub fn arrived(&mut self, laid: &mut Option<Laid>) -> bool {
        let Some(l) = laid.as_ref() else {
            return false;
        };
        if self.asked != Some(l.key) {
            return false;
        }
        if let Some(l) = laid.take() {
            self.asked = None;
            self.cache = l.cache;
            self.laid = Some(l.key);
            self.page = l.page;
        }
        true
    }

    /// Follow the source view, whose top line starts at `source_top`, if
    /// it has scrolled since last followed.
    pub fn follow(&mut self, source_top: usize) {
        if self.laid.is_none() || self.followed == Some(source_top) {
            return;
        }
        self.followed = Some(source_top);
        self.top = self.page.y_for(source_top);
    }

    /// Scroll by `dy` pixels within a column `height` tall.
    pub fn scroll_by(&mut self, dy: f32, height: f32) {
        let max = (self.page.height - height * 0.5).max(0.0);
        self.top = (self.top + dy).clamp(0.0, max);
    }

    /// The link at window point `(x, y)` in a column laid out by `layout`.
    #[must_use]
    pub fn link_at(&self, layout: &Layout, x: f32, y: f32) -> Option<&str> {
        let (px, py) = (x - layout.left, y - layout.top + self.top);
        self.page.link_at(px, py)
    }

    /// Queue the visible part of the page in the column `layout` gives.
    pub fn draw(&mut self, gpu: &mut Gpu, layout: &Layout, theme: &Theme) {
        let (left, top) = (layout.left, layout.top);
        let height = layout.bottom - top;
        gpu.begin_layer(Some([left, top, layout.width - left, height]));
        let ui = &theme.ui;
        if self.laid.is_none() {
            let note = if self.asked.is_some() || self.note.is_empty() {
                "Laying out…"
            } else {
                self.note
            };
            gpu.push_ui_text([left + layout.margin * 2.0, top], note, ui.gutter);
            return;
        }
        let paint = |p: Paint| -> [f32; 4] {
            let named =
                |name: &str, or: [f32; 4]| theme.named(name).and_then(|l| l.color).unwrap_or(or);
            match p {
                Paint::Text => ui.text,
                Paint::Muted => ui.gutter,
                Paint::Heading => named("text.title", ui.text),
                Paint::Link => named("text.uri", ui.cursor),
                Paint::Code => named("text.literal", ui.text),
                Paint::CodeBack => ui.fold_chip,
                Paint::Line => ui.guide,
            }
        };
        let (from, to) = (self.top, self.top + height);
        let page = &self.page;
        for r in &page.rects {
            let [x, y, w, h] = r.at;
            if y + h < from || y > to {
                continue;
            }
            gpu.push_rect([left + x, top + y - from], [w, h], paint(r.paint));
        }
        let font = gpu.preview_font();
        let first = page.runs.partition_point(|r| r.y + r.h <= from);
        let rule = (layout.scale).round().max(1.0);
        for (i, run) in page.runs.iter().enumerate().skip(first) {
            if run.y > to {
                break;
            }
            let Some(text) = page.text.get(run.text.clone()) else {
                continue;
            };
            let at = [left + run.x, top + run.y - from];
            let colour = paint(run.paint);
            if run.mono {
                self.colours.clear();
                let spans = code_spans(&page.spans, i);
                let mut pos = 0;
                for s in spans {
                    let (start, end) = (s.start as usize, s.end as usize);
                    if start > pos {
                        self.colours.push(ColorRun {
                            end: start,
                            color: colour,
                            style: nimbus_render::Style::default(),
                        });
                    }
                    let look = theme.look(s.table as usize, s.capture);
                    self.colours.push(ColorRun {
                        end,
                        color: look.and_then(|l| l.color).unwrap_or(colour),
                        style: look.map(|l| l.style).unwrap_or_default(),
                    });
                    pos = end;
                }
                gpu.push_text_runs(at, text, 0, (&self.colours, &[]), colour, f32::NEG_INFINITY);
            } else {
                gpu.push_styled_text(at, text, colour, (&font, run.style));
            }
            if run.strike {
                let mid = (at[1] + run.h * 0.55).round();
                gpu.push_rect([at[0], mid], [run.w, rule], colour);
            }
            if run.link.is_some() {
                let under = (at[1] + font.underline(run.style)).round();
                gpu.push_rect([at[0], under], [run.w, rule], colour);
            }
        }
    }
}

/// The colours of run `run`, from spans sorted by run.
fn code_spans(spans: &[CodeSpan], run: usize) -> &[CodeSpan] {
    let run = u32::try_from(run).unwrap_or(u32::MAX);
    let a = spans.partition_point(|s| s.run < run);
    let b = spans.partition_point(|s| s.run <= run);
    spans.get(a..b).unwrap_or(&[])
}

/// Code blocks' syntax colours, by a hash of their language and text:
/// `(start, end, table, capture)` within the block's text.
pub type CodeCache = HashMap<u64, Arc<[(u32, u32, u32, u32)]>>;

/// Lay out the Markdown document `text`, whose tree `outline` is, as a
/// page `params.width` wide. `cache` holds code blocks' colours from the
/// last layout and is left holding this one's.
#[must_use]
pub fn lay_out(text: &dyn Text, outline: &Outline, params: &Params, cache: &mut CodeCache) -> Page {
    let source = text.slice(0..text.len_bytes());
    let blocks = markdown::blocks(outline, text);
    let mut flow = Flow::new(&source, params, std::mem::take(cache));
    for (n, block) in blocks.iter().enumerate() {
        flow.block(block, n);
    }
    *cache = std::mem::take(&mut flow.fresh);
    flow.finish()
}

/// The syntax colours of a code block in `lang`, parsed on its own.
fn colour_code(lang: LanguageId, code: &str) -> Arc<[(u32, u32, u32, u32)]> {
    let buffer = RopeBuffer::from_text(code);
    let mut state = SyntaxState::new(Some(lang), code.len());
    state.parse_now(&buffer);
    let mut hl = Highlighter::default();
    let to = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    hl.spans(&state, &buffer, std::slice::from_ref(&(0..code.len())))
        .iter()
        .map(|s| (to(s.start), to(s.end), to(s.table), s.capture))
        .collect()
}

/// Inline styles: what a piece of a paragraph looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent styles that Markdown lets nest in any combination"
)]
pub struct Inline {
    /// Strong emphasis.
    pub bold: bool,
    /// Emphasis.
    pub italic: bool,
    /// A code span.
    pub code: bool,
    /// Struck through.
    pub strike: bool,
    /// An image's placeholder.
    pub image: bool,
    /// The link it is in, by index into the urls.
    pub link: Option<u32>,
}

/// A piece of a paragraph in one style: its text in the output string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    /// Its text.
    pub text: Range<usize>,
    /// Its style.
    pub style: Inline,
}

/// Read the inline Markdown in `src` (a paragraph, a heading or a table
/// cell, with line breaks as spaces): append its shown text to `out` in
/// pieces of one style each, and link targets to `urls`.
///
/// Emphasis opens only where a matching closer follows, so an unpaired
/// `*` stays a `*`. Images are shown as a placeholder with their alt text.
pub fn inline(src: &str, out: &mut String, pieces: &mut Vec<Piece>, urls: &mut Vec<String>) {
    let mut s = Scan {
        src,
        out,
        pieces,
        urls,
        style: Inline::default(),
        link_end: None,
    };
    s.run();
}

struct Scan<'a> {
    src: &'a str,
    out: &'a mut String,
    pieces: &'a mut Vec<Piece>,
    urls: &'a mut Vec<String>,
    style: Inline,
    /// Where the link being read ends: its `]` and the byte after its
    /// `(…)`.
    link_end: Option<(usize, usize)>,
}

impl Scan<'_> {
    fn run(&mut self) {
        let src = self.src;
        let b = src.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if let Some((close, after)) = self.link_end
                && i == close
            {
                self.style.link = None;
                self.link_end = None;
                i = after;
                continue;
            }
            let c = b[i];
            match c {
                b'\\' if b.get(i + 1).is_some_and(u8::is_ascii_punctuation) => {
                    self.emit(&src[i + 1..i + 2]);
                    i += 2;
                }
                b'`' => i = self.code(i),
                b'*' | b'_' => i = self.emphasis(i, c),
                b'~' if b.get(i + 1) == Some(&b'~') => i = self.strike(i),
                b'!' if b.get(i + 1) == Some(&b'[') => i = self.image(i),
                b'[' if self.style.link.is_none() => i = self.link(i),
                b'<' => i = self.autolink(i),
                b'h' if self.style.link.is_none() && at_word_start(b, i) => i = self.bare_url(i),
                _ => {
                    let len = src[i..].chars().next().map_or(1, char::len_utf8);
                    self.emit(&src[i..i + len]);
                    i += len;
                }
            }
        }
    }

    fn emit(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let start = self.out.len();
        self.out.push_str(text);
        let end = self.out.len();
        match self.pieces.last_mut() {
            Some(p) if p.style == self.style && p.text.end == start => p.text.end = end,
            _ => self.pieces.push(Piece {
                text: start..end,
                style: self.style,
            }),
        }
    }

    /// A code span from the backticks at `i`: its text up to the next
    /// run of as many backticks, verbatim.
    fn code(&mut self, i: usize) -> usize {
        let src = self.src;
        let n = src[i..].bytes().take_while(|&c| c == b'`').count();
        let ticks = &src[i..i + n];
        let body = i + n;
        let mut at = body;
        while let Some(off) = src[at..].find(ticks) {
            let pos = at + off;
            let run = src[pos..].bytes().take_while(|&c| c == b'`').count();
            if run == n {
                let mut inner = &src[body..pos];
                if inner.len() > 1 && inner.starts_with(' ') && inner.ends_with(' ') {
                    inner = &inner[1..inner.len() - 1];
                }
                let was = self.style;
                self.style.code = true;
                self.emit(inner);
                self.style = was;
                return pos + n;
            }
            at = pos + run;
        }
        self.emit(ticks);
        body
    }

    /// `*`, `**`, `_` or `__` at `i`: toggles emphasis when it can open or
    /// close, else is text.
    fn emphasis(&mut self, i: usize, c: u8) -> usize {
        let src = self.src;
        let b = src.as_bytes();
        let run = src[i..].bytes().take_while(|&x| x == c).count();
        let before = i.checked_sub(1).and_then(|p| b.get(p)).copied();
        let after = b.get(i + run).copied();
        let space = |x: Option<u8>| x.is_none_or(|x| x.is_ascii_whitespace());
        let word = |x: Option<u8>| x.is_some_and(|x| x.is_ascii_alphanumeric());
        // Underscores inside a word (`snake_case`) are text.
        if c == b'_' && word(before) && word(after) {
            self.emit(&src[i..i + run]);
            return i + run;
        }
        let strong = run >= 2;
        let n = if strong { 2 } else { 1 };
        let delim = &src[i..i + n];
        let open = if strong {
            self.style.bold
        } else {
            self.style.italic
        };
        let can_close = open && !space(before);
        let can_open = !open && !space(after) && closes_later(src, i + n, delim);
        if can_close || can_open {
            if strong {
                self.style.bold = !open;
            } else {
                self.style.italic = !open;
            }
            return i + n;
        }
        self.emit(delim);
        i + n
    }

    fn strike(&mut self, i: usize) -> usize {
        let src = self.src;
        let open = self.style.strike;
        if open || closes_later(src, i + 2, "~~") {
            self.style.strike = !open;
        } else {
            self.emit("~~");
        }
        i + 2
    }

    /// `![alt](url)`: shown as `[image: alt]`, muted.
    fn image(&mut self, i: usize) -> usize {
        let src = self.src;
        let Some((close, after, _)) = link_parts(src, i + 1) else {
            self.emit("!");
            return i + 1;
        };
        let was = self.style;
        self.style.image = true;
        let alt = &src[i + 2..close];
        self.emit("[image");
        if !alt.is_empty() {
            self.emit(": ");
            self.emit(alt);
        }
        self.emit("]");
        self.style = was;
        after
    }

    /// `[text](url)`: its text is read on with the link set, up to the
    /// `]`, after which the `(url)` is skipped.
    fn link(&mut self, i: usize) -> usize {
        let src = self.src;
        let Some((close, after, url)) = link_parts(src, i) else {
            self.emit("[");
            return i + 1;
        };
        self.style.link = Some(self.push_url(url));
        self.link_end = Some((close, after));
        i + 1
    }

    /// `<https://…>` or `<name@host>`: a link showing its target.
    fn autolink(&mut self, i: usize) -> usize {
        let src = self.src;
        let end = src[i + 1..].find(['>', ' ', '\n', '<']).map(|e| i + 1 + e);
        if let Some(end) = end
            && src.as_bytes().get(end) == Some(&b'>')
        {
            let inner = &src[i + 1..end];
            let url = if inner.contains("://") {
                Some(inner.to_owned())
            } else if inner.contains('@') {
                Some(format!("mailto:{inner}"))
            } else {
                None
            };
            if let Some(url) = url {
                let was = self.style;
                self.style.link = Some(self.push_url(&url));
                self.emit(inner);
                self.style = was;
                return end + 1;
            }
        }
        self.emit("<");
        i + 1
    }

    /// A bare `https://…` or `http://…`, up to the next space, without
    /// trailing punctuation.
    fn bare_url(&mut self, i: usize) -> usize {
        let src = self.src;
        let rest = &src[i..];
        if !(rest.starts_with("https://") || rest.starts_with("http://")) {
            self.emit("h");
            return i + 1;
        }
        let len = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let url = rest[..len].trim_end_matches(['.', ',', ';', ':', ')', '!', '?']);
        let was = self.style;
        self.style.link = Some(self.push_url(url));
        self.emit(url);
        self.style = was;
        i + url.len()
    }

    fn push_url(&mut self, url: &str) -> u32 {
        self.urls.push(url.to_owned());
        u32::try_from(self.urls.len() - 1).unwrap_or(u32::MAX)
    }
}

fn at_word_start(b: &[u8], i: usize) -> bool {
    i.checked_sub(1)
        .and_then(|p| b.get(p))
        .is_none_or(|c| c.is_ascii_whitespace() || matches!(c, b'(' | b'['))
}

/// Whether `delim` appears later in `src` from `from`, after a character
/// that is not a space: something that could close emphasis opened here.
fn closes_later(src: &str, from: usize, delim: &str) -> bool {
    let Some(rest) = src.get(from..) else {
        return false;
    };
    let mut at = 0;
    while let Some(off) = rest[at..].find(delim) {
        let pos = at + off;
        if pos > 0 && !rest.as_bytes()[pos - 1].is_ascii_whitespace() {
            return true;
        }
        at = pos + delim.len();
    }
    false
}

/// For `[` at `i`: its matching `]`, the byte after a following `(…)`,
/// and the url in it (without a title or angle brackets).
fn link_parts(src: &str, i: usize) -> Option<(usize, usize, &str)> {
    let b = src.as_bytes();
    let mut depth = 0usize;
    let mut j = i;
    let close = loop {
        match b.get(j)? {
            b'\\' => j += 1,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    break j;
                }
            }
            _ => {}
        }
        j += 1;
    };
    if b.get(close + 1) != Some(&b'(') {
        return None;
    }
    let open = close + 2;
    let mut depth = 1usize;
    let mut k = open;
    let end = loop {
        match b.get(k)? {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    break k;
                }
            }
            _ => {}
        }
        k += 1;
    };
    let inner = src[open..end].trim();
    let url = inner.split_whitespace().next().unwrap_or("");
    let url = url.trim_start_matches('<').trim_end_matches('>');
    Some((close, end + 1, url))
}

/// Widths of ASCII characters by style, worked out once each: most text
/// is ASCII, and adding up a few cached numbers is much faster than
/// asking the font for every character of a long document.
struct Measure {
    font: UiFont,
    tables: Vec<(UiStyle, [f32; 128])>,
}

impl Measure {
    fn new(font: UiFont) -> Self {
        Self {
            font,
            tables: Vec::new(),
        }
    }

    fn width(&mut self, text: &str, style: UiStyle) -> f32 {
        if !text.is_ascii() || text.contains('\t') {
            return self.font.styled_width(text, style);
        }
        let font = self.font;
        let at = if let Some(at) = self.tables.iter().position(|(s, _)| same_size(*s, style)) {
            at
        } else {
            self.tables.push((style, [f32::NAN; 128]));
            self.tables.len() - 1
        };
        let Some((_, table)) = self.tables.get_mut(at) else {
            return font.styled_width(text, style);
        };
        let mut w = 0.0;
        for c in text.bytes() {
            let slot = &mut table[usize::from(c & 0x7f)];
            if slot.is_nan() {
                let mut buf = [0u8; 4];
                *slot = font.styled_width(char::from(c).encode_utf8(&mut buf), style);
            }
            w += *slot;
        }
        w
    }
}

/// Whether two styles have the same advances: italic slants without
/// changing them.
fn same_size(a: UiStyle, b: UiStyle) -> bool {
    a.scale.to_bits() == b.scale.to_bits() && a.bold == b.bold
}

/// The page being laid out, top to bottom.
struct Flow<'a> {
    source: &'a str,
    font: UiFont,
    measure: Measure,
    page: Page,
    width: f32,
    /// Space around the page and between blocks.
    pad: f32,
    gap: f32,
    cell: f32,
    y: f32,
    /// Code colours from the last layout, and the ones this one used.
    cache: CodeCache,
    fresh: CodeCache,
    /// Scratch for a code block's text.
    code_text: String,
    /// Scratch for a paragraph's text and pieces.
    inline_text: String,
    pieces: Vec<Piece>,
    joined: String,
}

impl<'a> Flow<'a> {
    fn new(source: &'a str, params: &Params, cache: CodeCache) -> Self {
        let m = params.font.editor();
        let pad = (m.cell_width * 2.0).round();
        Self {
            source,
            font: params.font,
            measure: Measure::new(params.font),
            page: Page::default(),
            width: params.width,
            pad,
            gap: (m.line_height * 0.6).round(),
            cell: m.cell_width,
            y: pad,
            cache,
            fresh: CodeCache::new(),
            code_text: String::new(),
            inline_text: String::new(),
            pieces: Vec::new(),
            joined: String::new(),
        }
    }

    fn finish(mut self) -> Page {
        self.page.height = self.y + self.pad;
        self.page
    }

    fn block(&mut self, block: &Block, n: usize) {
        let in_list = block.list_depth > 0;
        // Items of a list sit closer together than paragraphs.
        if n > 0 {
            self.y += if in_list && block.bullet.is_some() {
                (self.gap * 0.4).round()
            } else {
                self.gap
            };
        }
        self.page.anchors.push((block.range.start, self.y));
        let quote_step = (self.cell * 1.5).round();
        let list_step = (self.cell * 2.5).round();
        let left = self.pad
            + f(usize::from(block.quote_depth)) * quote_step
            + f(usize::from(block.list_depth)) * list_step;
        let right = (self.width - self.pad).max(left + self.cell * 8.0);
        let top = self.y;
        let body = UiStyle::PLAIN;
        if let Some(b) = &block.bullet {
            let paint = if b.ends_with(['.', ')']) {
                Paint::Muted
            } else {
                Paint::Text
            };
            let w = self.measure.width(b, body);
            let x = (left - self.cell * 0.8 - w).round();
            self.run_at(b, x, w, body, paint);
        }
        match &block.kind {
            Kind::Heading(level) => {
                let style = heading_style(*level);
                self.paragraph(block, (left, right), style, Paint::Heading);
            }
            Kind::Paragraph => self.paragraph(block, (left, right), body, Paint::Text),
            Kind::Code(lang) => self.code(block, lang.as_deref(), (left, right)),
            Kind::Rule => {
                let h = self.font.line_height(body);
                let mid = (self.y + h * 0.5).round();
                self.rect(
                    [left, mid, right - left, 1.0_f32.max(h / 16.0).round()],
                    Paint::Line,
                );
                self.y += h;
            }
            Kind::Html => {
                let h = self.font.line_height(body);
                let w = self.measure.width("[HTML]", body);
                self.run_at("[HTML]", left, w, body, Paint::Muted);
                self.y += h;
            }
            Kind::Table(rows) => self.table(rows, (left, right)),
        }
        // A quote's bars run down beside everything in it.
        for d in 0..block.quote_depth {
            let x = self.pad + f(usize::from(d)) * quote_step;
            let w = (self.cell * 0.3).round().max(1.0);
            let extra = if n > 0 { self.gap } else { 0.0 };
            self.rect([x, top - extra, w, self.y - top + extra], Paint::Line);
        }
    }

    /// A paragraph or heading: its inline text wrapped between `left` and
    /// `right`.
    fn paragraph(
        &mut self,
        block: &Block,
        (left, right): (f32, f32),
        style: UiStyle,
        paint: Paint,
    ) {
        self.joined.clear();
        for r in &block.text {
            if let Some(s) = self.source.get(r.clone()) {
                self.joined.push_str(s);
            }
        }
        let mut src = std::mem::take(&mut self.joined);
        tidy(&mut src, matches!(block.kind, Kind::Heading(_)));
        self.inline_text.clear();
        self.pieces.clear();
        inline(
            &src,
            &mut self.inline_text,
            &mut self.pieces,
            &mut self.page.urls,
        );
        self.joined = src;
        if self.pieces.is_empty() {
            if block.bullet.is_some() {
                self.y += self.font.line_height(style);
            }
            return;
        }
        let pieces = std::mem::take(&mut self.pieces);
        let text = std::mem::take(&mut self.inline_text);
        self.wrap(&text, &pieces, (left, right), style, paint);
        self.pieces = pieces;
        self.inline_text = text;
    }

    /// Wrap `pieces` of `text` into lines between `left` and `right`,
    /// starting at the current y, which ends below the last line.
    fn wrap(
        &mut self,
        text: &str,
        pieces: &[Piece],
        (left, right): (f32, f32),
        base: UiStyle,
        paint: Paint,
    ) {
        let h = self.font.line_height(base);
        let mut x = left;
        // A space waiting to be placed before the next word, in the look
        // of the text it was in: the space before a link is not
        // underlined, nor the one before code on a code background.
        let mut space: Option<(UiStyle, Paint, Inline)> = None;
        for p in pieces {
            let Some(s) = text.get(p.text.clone()) else {
                continue;
            };
            let style = UiStyle {
                bold: base.bold || p.style.bold,
                italic: base.italic || p.style.italic,
                ..base
            };
            let piece_paint = if p.style.link.is_some() {
                Paint::Link
            } else if p.style.code {
                Paint::Code
            } else if p.style.image {
                Paint::Muted
            } else {
                paint
            };
            for word in split_words(s) {
                if word.starts_with(char::is_whitespace) {
                    space = Some((style, piece_paint, p.style));
                    continue;
                }
                let w = self.measure.width(word, style);
                let lead = match space.take() {
                    Some(look) if x > left => Some((self.measure.width(" ", look.0), look)),
                    _ => None,
                };
                let lead_w = lead.map_or(0.0, |(w, _)| w);
                if x > left && x + lead_w + w > right {
                    x = left;
                    self.y += h;
                } else if let Some((lw, look)) = lead {
                    self.append(" ", x, lw, h, look);
                    x += lw;
                }
                if w <= right - left {
                    self.append(word, x, w, h, (style, piece_paint, p.style));
                    x += w;
                } else {
                    x = self.long_word(word, (left, right), x, h, (style, piece_paint, p.style));
                }
            }
        }
        self.y += h;
    }

    /// A word wider than the column, broken wherever the line is full.
    fn long_word(
        &mut self,
        word: &str,
        (left, right): (f32, f32),
        mut x: f32,
        h: f32,
        look: (UiStyle, Paint, Inline),
    ) -> f32 {
        let mut rest = word;
        while !rest.is_empty() {
            let mut fit = 0;
            let mut w = 0.0;
            for (i, ch) in rest.char_indices() {
                let mut buf = [0u8; 4];
                let cw = self.measure.width(ch.encode_utf8(&mut buf), look.0);
                if x + w + cw > right && (i > 0 || x > left) {
                    break;
                }
                w += cw;
                fit = i + ch.len_utf8();
            }
            if fit == 0 {
                x = left;
                self.y += h;
                continue;
            }
            self.append(&rest[..fit], x, w, h, look);
            x += w;
            rest = &rest[fit..];
            if !rest.is_empty() {
                x = left;
                self.y += h;
            }
        }
        x
    }

    /// Add `s` at `x` on the current line, joining the previous run when
    /// it is in the same style and ends there.
    fn append(
        &mut self,
        s: &str,
        x: f32,
        w: f32,
        h: f32,
        (style, paint, inline): (UiStyle, Paint, Inline),
    ) {
        let start = self.page.text.len();
        self.page.text.push_str(s);
        let end = self.page.text.len();
        if let Some(last) = self.page.runs.last_mut()
            && last.text.end == start
            && (last.y - self.y).abs() < 0.5
            && (last.x + last.w - x).abs() < 0.5
            && last.style == style
            && last.paint == paint
            && last.link == inline.link
            && last.strike == inline.strike
            && !last.mono
            && !inline.code
        {
            last.text.end = end;
            last.w += w;
            return;
        }
        if inline.code {
            let padding = (self.cell * 0.15).round();
            self.rect([x - padding, self.y, w + 2.0 * padding, h], Paint::CodeBack);
        }
        self.page.runs.push(Run {
            x,
            y: self.y,
            w,
            h,
            text: start..end,
            style,
            mono: false,
            paint,
            link: inline.link,
            strike: inline.strike,
        });
    }

    /// A one-off run on the current line, such as a list marker.
    fn run_at(&mut self, s: &str, x: f32, w: f32, style: UiStyle, paint: Paint) {
        let start = self.page.text.len();
        self.page.text.push_str(s);
        self.page.runs.push(Run {
            x,
            y: self.y,
            w,
            h: self.font.line_height(style),
            text: start..self.page.text.len(),
            style,
            mono: false,
            paint,
            link: None,
            strike: false,
        });
    }

    fn rect(&mut self, at: [f32; 4], paint: Paint) {
        self.page.rects.push(Rect { at, paint });
    }

    /// A code block: its lines on the editor's grid, on a background,
    /// coloured by the grammar its fence names.
    fn code(&mut self, block: &Block, lang: Option<&str>, (left, right): (f32, f32)) {
        let h = self.font.line_height(UiStyle::PLAIN);
        let inset = (self.cell * 0.8).round();
        let top = self.y;
        self.rect([left, top, right - left, 0.0], Paint::CodeBack);
        let back = self.page.rects.len() - 1;
        self.y += inset;
        self.code_text.clear();
        for seg in &block.text {
            if let Some(s) = self.source.get(seg.clone()) {
                self.code_text.push_str(s);
            }
        }
        let code = std::mem::take(&mut self.code_text);
        let colours = self.colours_of(lang, &code);
        let mut at = 0;
        let mut lines = 0;
        for line in code.split_inclusive('\n') {
            let shown = line.trim_end_matches(['\n', '\r']);
            self.code_line(shown, at, left + inset, h, &colours);
            at += line.len();
            lines += 1;
        }
        self.code_text = code;
        if lines == 0 {
            self.y += h;
        }
        self.y += inset;
        if let Some(r) = self.page.rects.get_mut(back) {
            r.at[3] = self.y - top;
        }
    }

    /// The colours of a code block, from the last layout's when the block
    /// is unchanged.
    fn colours_of(&mut self, lang: Option<&str>, code: &str) -> Arc<[(u32, u32, u32, u32)]> {
        let Some(lang) = lang.and_then(LanguageId::from_alias) else {
            return Arc::from([]);
        };
        let mut h = DefaultHasher::new();
        lang.index().hash(&mut h);
        code.hash(&mut h);
        let key = h.finish();
        let colours = self
            .cache
            .get(&key)
            .or_else(|| self.fresh.get(&key))
            .cloned()
            .unwrap_or_else(|| colour_code(lang, code));
        self.fresh.insert(key, Arc::clone(&colours));
        colours
    }

    /// One line of a code block, starting at byte `from` of the block's
    /// text, with the block's `colours`.
    fn code_line(
        &mut self,
        line: &str,
        from: usize,
        x: f32,
        h: f32,
        colours: &[(u32, u32, u32, u32)],
    ) {
        let start = self.page.text.len();
        self.page.text.push_str(line);
        let run = u32::try_from(self.page.runs.len()).unwrap_or(u32::MAX);
        let to = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        let (from, end) = (to(from), to(from + line.len()));
        let first = colours.partition_point(|s| s.1 <= from);
        for &(a, b, table, capture) in colours.get(first..).unwrap_or(&[]) {
            if a >= end {
                break;
            }
            self.page.spans.push(CodeSpan {
                run,
                start: a.max(from) - from,
                end: b.min(end) - from,
                table,
                capture,
            });
        }
        let w = f(nimbus_core::column::line_width(line, 4)) * self.cell;
        self.page.runs.push(Run {
            x,
            y: self.y,
            w,
            h,
            text: start..self.page.text.len(),
            style: UiStyle::PLAIN,
            mono: true,
            paint: Paint::Code,
            link: None,
            strike: false,
        });
        self.y += h;
    }

    /// A table: columns as wide as their widest cell, shrunk to fit, with
    /// cells wrapped within them and lines between the rows.
    fn table(&mut self, rows: &[Vec<Range<usize>>], (left, right): (f32, f32)) {
        let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
        if cols == 0 {
            return;
        }
        let cell_pad = (self.cell * 0.8).round();
        // Each cell's pieces, read once.
        let mut cells: Vec<Vec<(String, Vec<Piece>)>> = Vec::with_capacity(rows.len());
        let mut natural = vec![0.0f32; cols];
        // The widest word in each column: narrower than this, a cell
        // breaks inside a word.
        let mut least = vec![0.0f32; cols];
        for (r, row) in rows.iter().enumerate() {
            let mut out = Vec::with_capacity(row.len());
            for (c, range) in row.iter().enumerate() {
                let src = self
                    .source
                    .get(range.clone())
                    .unwrap_or("")
                    .trim()
                    .replace("\\|", "|");
                let (mut text, mut pieces) = (String::new(), Vec::new());
                inline(&src, &mut text, &mut pieces, &mut self.page.urls);
                let style = UiStyle {
                    bold: r == 0,
                    ..UiStyle::PLAIN
                };
                let w = self.measure.width(&text, style);
                if let Some(n) = natural.get_mut(c) {
                    *n = n.max(w + 2.0 * cell_pad);
                }
                let word = text
                    .split_whitespace()
                    .map(|word| self.measure.width(word, style))
                    .fold(0.0f32, f32::max);
                if let Some(l) = least.get_mut(c) {
                    *l = l.max(word + 2.0 * cell_pad);
                }
                out.push((text, pieces));
            }
            cells.push(out);
        }
        let widths = share_columns(&natural, &least, right - left);
        let table_w: f32 = widths.iter().sum();
        let line = 1.0;
        let starts_at = self.page.runs.len();
        self.rect([left, self.y, table_w, line], Paint::Muted);
        for (r, row) in cells.iter().enumerate() {
            let top = self.y;
            let mut bottom = top;
            let mut x = left;
            for (c, (text, pieces)) in row.iter().enumerate() {
                let w = widths.get(c).copied().unwrap_or(0.0);
                self.y = top + cell_pad * 0.5;
                let style = UiStyle {
                    bold: r == 0,
                    ..UiStyle::PLAIN
                };
                self.wrap(
                    text,
                    pieces,
                    (x + cell_pad, x + w - cell_pad),
                    style,
                    Paint::Text,
                );
                bottom = bottom.max(self.y + cell_pad * 0.5);
                x += w;
            }
            self.y = bottom;
            self.rect([left, self.y, table_w, line], Paint::Muted);
        }
        // Cells were laid out column by column; drawing finds runs by
        // their line, so put them back in order of their tops.
        if let Some(runs) = self.page.runs.get_mut(starts_at..) {
            runs.sort_by(|a, b| a.y.total_cmp(&b.y));
        }
    }
}

/// Column widths for a table `room` wide, given each column's natural
/// width (its widest cell on one line) and least width (its widest
/// word). The browser's way: if everything fits, each column gets its
/// natural width. Otherwise each gets its least width, so no word is
/// broken, and the room left over goes to the columns that want more, in
/// proportion to how much more they want. Only when even the words do
/// not fit are the least widths shrunk to fit.
fn share_columns(natural: &[f32], least: &[f32], room: f32) -> Vec<f32> {
    let total: f32 = natural.iter().sum();
    if total <= room {
        return natural.to_vec();
    }
    let floor: f32 = least.iter().sum();
    if floor >= room {
        return least
            .iter()
            .map(|w| (w / floor.max(1.0) * room).floor())
            .collect();
    }
    let want = (total - floor).max(1.0);
    let spare = room - floor;
    natural
        .iter()
        .zip(least)
        .map(|(&n, &l)| (l + (n - l).max(0.0) / want * spare).floor())
        .collect()
}

/// Heading levels' sizes: from 1.8 times the body down to the body's.
fn heading_style(level: u8) -> UiStyle {
    let scale = match level {
        1 => 1.8,
        2 => 1.5,
        3 => 1.25,
        4 => 1.1,
        _ => 1.0,
    };
    UiStyle {
        scale,
        bold: true,
        italic: false,
    }
}

/// Make a block's joined text one line: each line's surrounding spaces
/// become one space. A heading also loses its closing `#`s.
fn tidy(src: &mut String, heading: bool) {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(line);
    }
    if heading {
        let t = out.trim_end_matches('#');
        if t.len() < out.len() && (t.is_empty() || t.ends_with(' ')) {
            out.truncate(t.trim_end().len());
        }
    }
    *src = out;
}

/// `s` split into runs of spaces and runs of anything else.
fn split_words(s: &str) -> impl Iterator<Item = &str> {
    let mut rest = s;
    std::iter::from_fn(move || {
        let first = rest.chars().next()?;
        let space = first.is_whitespace();
        let end = rest
            .char_indices()
            .find(|&(_, c)| c.is_whitespace() != space)
            .map_or(rest.len(), |(i, _)| i);
        let (word, tail) = rest.split_at(end);
        rest = tail;
        Some(word)
    })
}

#[expect(clippy::cast_precision_loss, reason = "page positions and byte shares")]
fn f(n: usize) -> f32 {
    n as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use nimbus_render::Font;

    fn styled(src: &str) -> Vec<(String, Inline)> {
        let (mut out, mut pieces, mut urls) = (String::new(), Vec::new(), Vec::new());
        inline(src, &mut out, &mut pieces, &mut urls);
        pieces
            .iter()
            .map(|p| (out[p.text.clone()].to_owned(), p.style))
            .collect()
    }

    fn shown(src: &str) -> String {
        styled(src).into_iter().map(|(s, _)| s).collect()
    }

    #[test]
    fn emphasis_code_and_escapes() {
        let s = styled("a **b** *c* `d*e` \\*f");
        assert_eq!(shown("a **b** *c* `d*e` \\*f"), "a b c d*e *f");
        assert!(s.iter().any(|(t, st)| t == "b" && st.bold && !st.italic));
        assert!(s.iter().any(|(t, st)| t == "c" && st.italic));
        assert!(s.iter().any(|(t, st)| t == "d*e" && st.code));
    }

    #[test]
    fn unpaired_and_intraword_markers_stay_text() {
        assert_eq!(shown("2 * 3 = 6"), "2 * 3 = 6");
        assert_eq!(shown("snake_case_name"), "snake_case_name");
        assert_eq!(shown("a `b"), "a `b");
        assert_eq!(shown("~~gone~~ x"), "gone x");
    }

    #[test]
    fn links_images_and_autolinks() {
        let (mut out, mut pieces, mut urls) = (String::new(), Vec::new(), Vec::new());
        inline(
            "see [the *docs*](https://a.example/x \"t\"), ![logo](l.png) <https://b.example> and https://c.example.",
            &mut out,
            &mut pieces,
            &mut urls,
        );
        assert_eq!(
            out,
            "see the docs, [image: logo] https://b.example and https://c.example."
        );
        assert_eq!(
            urls,
            vec![
                "https://a.example/x",
                "https://b.example",
                "https://c.example"
            ]
        );
        let docs = pieces.iter().find(|p| &out[p.text.clone()] == "docs");
        assert!(docs.is_some_and(|p| p.style.italic && p.style.link == Some(0)));
        assert!(
            pieces
                .iter()
                .any(|p| p.style.image && &out[p.text.clone()] == "[image: logo]")
        );
        assert_eq!(
            shown("[not a link] and [ref][x]"),
            "[not a link] and [ref][x]"
        );
    }

    fn params(width: f32) -> Params {
        let font = Font::bundled().unwrap_or_else(|_| unreachable!());
        let metrics = font.metrics(20.0);
        Params {
            width,
            font: UiFont::new(font, metrics),
        }
    }

    fn parsed(src: &str) -> (RopeBuffer, SyntaxState) {
        let text = RopeBuffer::from_text(src);
        let mut state = SyntaxState::new(Some(LanguageId::Markdown), src.len());
        state.parse_now(&text);
        (text, state)
    }

    fn page(src: &str, width: f32) -> Page {
        let (text, state) = parsed(src);
        let Some(o) = markdown::outline(&state) else {
            panic!("no tree");
        };
        lay_out(&text, &o, &params(width), &mut CodeCache::new())
    }

    fn line_texts(p: &Page) -> Vec<(i32, String)> {
        let mut out: Vec<(i32, String)> = Vec::new();
        for r in &p.runs {
            #[expect(clippy::cast_possible_truncation, reason = "test positions")]
            let y = r.y as i32;
            let t = &p.text[r.text.clone()];
            match out.last_mut() {
                Some((ly, s)) if *ly == y => s.push_str(t),
                _ => out.push((y, t.to_owned())),
            }
        }
        out
    }

    #[test]
    fn paragraphs_wrap_within_the_width() {
        let p = page("one two three four five six seven eight nine ten\n", 300.0);
        let lines = line_texts(&p);
        assert!(lines.len() > 1, "{lines:?}");
        for r in &p.runs {
            assert!(r.x + r.w <= 300.0, "{r:?}");
        }
        let joined: Vec<String> = lines.into_iter().map(|(_, s)| s).collect();
        assert_eq!(
            joined.join(" "),
            "one two three four five six seven eight nine ten"
        );
    }

    #[test]
    fn blocks_stack_down_the_page_with_anchors() {
        let src = "# Title\n\nBody text.\n\n- a\n- b\n\n```rust\nfn main() {}\n```\n\n---\n";
        let p = page(src, 800.0);
        let lines = line_texts(&p);
        let texts: Vec<&str> = lines.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(
            texts,
            vec!["Title", "Body text.", "•a", "•b", "fn main() {}"]
        );
        let ys: Vec<i32> = lines.iter().map(|(y, _)| *y).collect();
        assert!(ys.windows(2).all(|w| w[0] < w[1]), "{ys:?}");
        assert_eq!(p.anchors.len(), 6);
        assert!(
            p.anchors
                .windows(2)
                .all(|w| w[0].0 < w[1].0 && w[0].1 < w[1].1)
        );
        let heading = &p.runs[0];
        assert!(heading.style.scale > 1.0 && heading.style.bold);
        let code = p.runs.iter().find(|r| r.mono);
        assert!(code.is_some());
        assert!(p.rects.iter().any(|r| r.paint == Paint::CodeBack));
        assert!(p.rects.iter().any(|r| r.paint == Paint::Line), "the rule");
        // The source's middle maps between the first and last anchors.
        let y = p.y_for(src.find("Body").unwrap_or(0) + 3);
        assert!(y > p.anchors[1].1 && y < p.anchors[2].1, "{y}");
    }

    #[test]
    fn code_blocks_take_their_languages_colours() {
        let p = page("```rust\nfn main() { let x = 1; }\n```\n", 800.0);
        assert!(!p.spans.is_empty(), "rust spans");
        let rust = LanguageId::from_name("rust").map(LanguageId::index);
        assert!(p.spans.iter().all(|s| Some(s.table as usize) == rust));
    }

    #[test]
    fn tables_lay_out_in_rows() {
        let p = page("| a | b |\n|---|---|\n| one | two |\n", 800.0);
        let lines = line_texts(&p);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(p.runs.iter().any(|r| r.style.bold));
        assert!(p.runs.windows(2).all(|w| w[0].y <= w[1].y));
    }

    #[test]
    fn narrow_columns_keep_their_words_whole() {
        // A short date column beside a long prose one (DESIGN.md's
        // decision log): the date keeps its width and the prose wraps.
        let w = share_columns(&[100.0, 2000.0, 40.0], &[100.0, 120.0, 40.0], 800.0);
        assert!(w[0] >= 100.0 && w[2] >= 40.0, "{w:?}");
        assert!(w.iter().sum::<f32>() <= 800.0, "{w:?}");
        // Everything fits: natural widths.
        assert_eq!(
            share_columns(&[10.0, 20.0], &[5.0, 5.0], 100.0),
            vec![10.0, 20.0]
        );
        // Not even the words fit: shrunk in proportion.
        let w = share_columns(&[300.0, 300.0], &[200.0, 200.0], 100.0);
        assert!(w.iter().sum::<f32>() <= 100.0, "{w:?}");
    }

    #[test]
    fn links_are_found_by_position() {
        let p = page("go [here](https://x.example) now\n", 800.0);
        let link = p.runs.iter().find(|r| r.link.is_some());
        let Some(r) = link else {
            panic!("no link run: {:?}", p.runs);
        };
        assert_eq!(p.link_at(r.x + 1.0, r.y + 1.0), Some("https://x.example"));
        assert_eq!(p.link_at(1.0, r.y + 1.0), None);
    }

    #[test]
    fn layout_waits_for_a_pause_in_typing() {
        let mut pv = Preview::default();
        let (text, state) = parsed("# a\n");
        let t0 = Instant::now();
        let (job, _) = pv.wants((&text, 1, &state), params(500.0), t0);
        let Some(job) = job else {
            panic!("the first layout starts at once");
        };
        let mut laid = Some(job.run());
        assert!(pv.arrived(&mut laid));
        // An edit: nothing until the pause has passed.
        let (job, wake) = pv.wants((&text, 2, &state), params(500.0), t0);
        assert!(job.is_none());
        assert_eq!(wake, Some(t0 + PAUSE));
        let (job, _) = pv.wants((&text, 2, &state), params(500.0), t0 + PAUSE);
        assert!(job.is_some());
        // A new width lays out at once.
        let (job, _) = pv.wants((&text, 2, &state), params(400.0), t0 + PAUSE);
        assert!(job.is_some());
    }

    #[test]
    #[ignore = "benchmark; run with --ignored --nocapture"]
    fn m20_preview_bench() {
        use std::fmt::Write as _;
        let mut src = String::new();
        let mut n = 0;
        while src.len() < 1024 * 1024 {
            n += 1;
            let _ = write!(
                src,
                "## Section {n}\n\nSome *emphasis*, **strong** text, `code` and a [link](https://example.com/{n}) in a paragraph that runs long enough to wrap over a few lines of the page.\n\n- one item\n- two item with `code`\n\n```rust\nfn f{n}(x: u32) -> u32 {{ x + {n} }}\n```\n\n| a | b |\n|---|---|\n| {n} | x |\n\n"
            );
        }
        let (text, state) = parsed(&src);
        let Some(o) = markdown::outline(&state) else {
            panic!("no tree");
        };
        // The first layout parses every code block; later ones, after an
        // edit, find most of them in the cache.
        let mut cache = CodeCache::new();
        let t = Instant::now();
        let _ = lay_out(&text, &o, &params(900.0), &mut cache);
        let first = t.elapsed().as_secs_f64() * 1000.0;
        let mut times = Vec::new();
        for _ in 0..5 {
            let t = Instant::now();
            let p = lay_out(&text, &o, &params(900.0), &mut cache);
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            assert!(p.height > 0.0);
        }
        times.sort_by(f64::total_cmp);
        println!(
            "preview layout of {} KB: first {first:.1} ms; again min {:.1} ms, median {:.1} ms",
            src.len() / 1024,
            times[0],
            times[2]
        );
    }
}
