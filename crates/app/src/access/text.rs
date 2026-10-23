//! Text as a screen reader reads it (M22): a window of lines around the
//! cursor, described as AccessKit *text runs*.
//!
//! A screen reader never sees our pixels. To read text it asks for the
//! characters around the caret, the next word, the line above; AccessKit
//! answers those questions from a list of runs we hand it. Each run is a
//! node with role [`Role::TextRun`] holding a piece of one visual row:
//!
//! - its text (`value`), ending with the line break on the last run of a
//!   line, so the reader knows where paragraphs end;
//! - the byte length of each *character*, where a character is what one
//!   press of Right moves over: a grapheme cluster, like `é` written as
//!   `e` plus an accent, or a family emoji made of several code points.
//!   AccessKit counts in UTF-8 bytes, as our buffer does, so offsets map
//!   across without conversion;
//! - where words start, by the same rules as Alt+Left and Alt+Right in
//!   the editor (`nimbus_core::movement`), so "next word" in VoiceOver
//!   lands where ours does;
//! - where each character is drawn, for the reader's highlight box and a
//!   magnifier following the caret.
//!
//! Every run of a row but the last is linked to the next with
//! `next_on_line`: AccessKit stores word starts as `u8`, so a run holds at
//! most [`RUN_CHARS`] characters, and a long row is several runs that the
//! reader treats as one line. Separate rows are separate lines, so with
//! soft wrap on, "next line" moves by what is on screen, as Down does.
//!
//! Only a window of the text is described: [`WINDOW_LINES`] lines either
//! side of the cursor's line, and at most [`WINDOW_BYTES`] bytes either
//! side, so a 4 GB file costs what a small one does. The window stays put
//! while the cursor moves inside it and is rebuilt around the cursor when
//! the cursor comes within [`REBUILD_MARGIN`] lines of its edge. Runs are
//! named by their place in the window plus a *generation* that changes
//! when the window is rebuilt, so ids are stable while the window stands
//! and a rebuilt window never reuses an old id for other text.
//!
//! Updates are incremental: each run as last sent is remembered, and a run
//! whose text and place are unchanged is not sent again. Typing on a line
//! resends that line's runs (and, after Enter, the lines that moved down a
//! row); moving the cursor resends none.

use accesskit::{Node, NodeId, Rect, Role, TextPosition, TextSelection};
use nimbus_core::wrap::{self, RowPos, RowStart, WrapView};
use nimbus_core::{LineSpan, Selection, Text, column, long_line};
use unicode_segmentation::UnicodeSegmentation;

use super::id;

/// Lines described either side of the cursor's line.
pub const WINDOW_LINES: usize = 200;

/// Bytes described either side of the cursor's line, at most: long lines
/// make the window shorter than [`WINDOW_LINES`].
pub const WINDOW_BYTES: usize = 256 * 1024;

/// The window is rebuilt around the cursor once it comes this close to
/// either end (unless that end is the end of the text).
pub const REBUILD_MARGIN: usize = 50;

/// Most characters in one run. AccessKit keeps word starts as `u8`
/// indexes into a run's characters.
pub const RUN_CHARS: usize = 250;

/// Sizes the runs' positions are worked out from: the grid font's cell
/// and line height in physical pixels, and the tab width in columns.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    /// Width of one column.
    pub cell: f32,
    /// Height of one row.
    pub line_height: f32,
    /// Columns per tab stop.
    pub tab_width: usize,
}

/// Where the field is shown: what [`TextWindow::update`] needs besides the
/// text and selection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// The first row on screen, to place the window; `None` for a field
    /// that does not scroll.
    pub top: Option<RowPos>,
    /// The grid's sizes.
    pub geometry: Geometry,
    /// How far long lines are scrolled sideways beyond the field's own
    /// scroll, in pixels.
    pub long_scroll: f64,
}

/// One run of the window.
#[derive(Debug, Clone, Default, PartialEq)]
struct Run {
    /// Document byte where `text` starts. Not part of the node, so it may
    /// change (typing above shifts it) without the run being sent again.
    start: usize,
    /// Document byte where the run's line starts; with `line_row`, lets
    /// the next update reuse an unchanged line's runs instead of cutting
    /// them again.
    line: usize,
    /// The row within its line.
    line_row: usize,
    /// It is a piece of a long line, which is never reused.
    long: bool,
    /// The run's text, with the line break on a line's last run.
    text: String,
    /// Bytes of `text` before the line break.
    content: usize,
    /// The row within the window, which places it vertically.
    row: usize,
    /// Display column of the run's first character within its line: tab
    /// stops are counted from the line's start.
    col: usize,
    /// Horizontal offset of the row's start, in pixels, from where column
    /// 0 is drawn (nonzero only for a long line's piece).
    x: f32,
    /// Word starts, as character indexes into this run.
    words: Vec<u8>,
    /// The run continues a row begun by the run before it.
    continues: bool,
    /// The row continues in the run after it.
    continued: bool,
}

/// What [`TextWindow::update`] worked out besides the changed nodes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    /// The selection, as positions in the runs.
    pub selection: Option<TextSelection>,
    /// The window row showing `top`, when the window holds it.
    pub top_row: Option<usize>,
    /// Rows in the window.
    pub rows: usize,
}

/// The described window of one text field, and its runs as last sent.
#[derive(Debug, Default)]
pub struct TextWindow {
    /// Which field: part of every run's id ([`id::run`]).
    field: u8,
    /// Changes when the window is rebuilt; part of every run's id.
    generation: u16,
    /// Where the window's first line starts, once there is a window.
    first: Option<usize>,
    /// Runs as last sent.
    runs: Vec<Run>,
    /// A signature of each run's node as last sent ([`signature`]), to
    /// tell which changed.
    sent: Vec<u64>,
    /// During an update: the previous runs, which unchanged lines take
    /// theirs from, and how far through them the walk is.
    old: Vec<Run>,
    old_at: usize,
    /// Their ids, the text field's children.
    children: Vec<NodeId>,
    /// Scratch: the current line's rows and word starts.
    rows: Vec<RowStart>,
    words: Vec<usize>,
}

impl TextWindow {
    /// A window for the text field `field` (see [`id`]).
    #[must_use]
    pub fn new(field: u8) -> Self {
        Self {
            field,
            ..Self::default()
        }
    }

    /// Forget what was sent: the next update describes every run again
    /// under a new generation. For a full tree, or when the field left the
    /// tree.
    pub fn reset(&mut self) {
        self.first = None;
        self.runs.clear();
        self.sent.clear();
        self.children.clear();
    }

    /// Move the window's start through an edit, given as a map from old
    /// offsets to new ones, so the runs above the edit keep their ids.
    pub fn follow(&mut self, map: impl Fn(usize) -> usize) {
        if let Some(first) = self.first.as_mut() {
            *first = map(*first);
        }
        // The runs follow too, so an unchanged line below an edit is
        // recognised where it now is.
        for run in &mut self.runs {
            run.start = map(run.start);
            run.line = map(run.line);
        }
    }

    /// The text field's children: its runs, in order.
    #[must_use]
    pub fn children(&self) -> &[NodeId] {
        &self.children
    }

    /// The document offset of a position a screen reader names, or `None`
    /// for a run that is no longer described.
    #[must_use]
    pub fn byte_of(&self, at: TextPosition) -> Option<usize> {
        let i = self.children.iter().position(|&c| c == at.node)?;
        let run = &self.runs[i];
        let lens = char_lengths(&run.text, run.content);
        let k = at.character_index.min(lens.len());
        let bytes: usize = lens[..k].iter().map(|&l| usize::from(l)).sum();
        // The line break counts as one character: a position on it is the
        // end of the line's content, one after it the next line's start.
        Some(
            run.start
                + if bytes > run.content {
                    run.text.len()
                } else {
                    bytes
                },
        )
    }

    /// Describe the window around the selection's head in `t`, adding the
    /// runs that changed since the last update to `out`. `w` gives rows
    /// (soft wrap, folds); `frame` says where the field is shown.
    pub fn update(
        &mut self,
        t: &dyn Text,
        w: &mut WrapView<'_>,
        selection: Selection,
        frame: Frame,
        out: &mut Vec<(NodeId, Node)>,
    ) -> Placed {
        let Frame {
            top,
            geometry,
            long_scroll,
        } = frame;
        let len = t.len_bytes();
        let head = selection.head.min(len);
        let cursor_line = wrap::row_pos_of(t, w, head).start;
        if !self.window_holds(t, w, cursor_line) {
            self.rebuild(t, w, cursor_line);
        }
        self.old = std::mem::take(&mut self.runs);
        self.old_at = 0;
        let spare = Vec::with_capacity(self.old.len());
        let mut walk = Walk::new(geometry, long_scroll, top, spare);
        let first = self.first.unwrap_or(0);
        self.walk(t, w, first, head, cursor_line, &mut walk);
        if !walk.reached_cursor {
            // The window ran out of room before the cursor's line (long
            // lines above it): start again around the cursor.
            self.rebuild(t, w, cursor_line);
            walk.runs.clear();
            walk = Walk::new(geometry, long_scroll, top, std::mem::take(&mut walk.runs));
            let first = self.first.unwrap_or(0);
            self.walk(t, w, first, head, cursor_line, &mut walk);
        }
        self.old.clear();
        self.send(&walk, geometry, out);
        self.runs = walk.runs;
        let anchor = self.position(selection.anchor.min(len));
        let focus = self.position(head);
        Placed {
            selection: anchor
                .zip(focus)
                .map(|(anchor, focus)| TextSelection { anchor, focus }),
            top_row: walk.top_row,
            rows: walk.row,
        }
    }

    /// Whether the current window holds `cursor_line` comfortably away
    /// from its edges.
    fn window_holds(&self, t: &dyn Text, w: &WrapView<'_>, cursor_line: usize) -> bool {
        let Some(first) = self.first else {
            return false;
        };
        let first = t.line_span(first.min(t.len_bytes())).start;
        if cursor_line < first {
            return false;
        }
        let total = 2 * WINDOW_LINES + 1;
        let mut at = first;
        let mut n = 0;
        while at < cursor_line && n <= total {
            match w.next_line(t, t.line_span(at)) {
                Some(next) => at = next,
                None => break,
            }
            n += 1;
        }
        if at != cursor_line {
            return false;
        }
        let near_top = n < REBUILD_MARGIN && first > 0;
        let near_end = n + REBUILD_MARGIN > total || cursor_line - first > WINDOW_BYTES * 3 / 2;
        !near_top && !near_end
    }

    /// Start a new window [`WINDOW_LINES`] lines (or [`WINDOW_BYTES`]
    /// bytes) above the cursor's line.
    fn rebuild(&mut self, t: &dyn Text, w: &WrapView<'_>, cursor_line: usize) {
        let mut first = cursor_line;
        for _ in 0..WINDOW_LINES {
            match w.prev_line(t, first) {
                Some(prev) if cursor_line - prev <= WINDOW_BYTES => first = prev,
                _ => break,
            }
        }
        self.first = Some(first);
        self.generation = self.generation.wrapping_add(1);
        self.runs.clear();
        self.old.clear();
        self.sent.clear();
        self.children.clear();
    }

    /// Walk the window's lines from `first`, collecting runs into `walk`.
    fn walk(
        &mut self,
        t: &dyn Text,
        w: &mut WrapView<'_>,
        first: usize,
        head: usize,
        cursor_line: usize,
        walk: &mut Walk,
    ) {
        let first = t.line_span(first.min(t.len_bytes())).start;
        let mut start = Some(first);
        let mut lines = 0;
        while let Some(at) = start {
            let span = t.line_span(at);
            // The window's end stays put while the cursor moves inside it:
            // it is counted from the first line, never from the cursor.
            let full = lines > 2 * WINDOW_LINES || span.start - first > 2 * WINDOW_BYTES;
            if full && span.start > cursor_line {
                break;
            }
            lines += 1;
            if span.start == cursor_line {
                walk.reached_cursor = true;
            }
            if long_line::is_long(span) {
                self.long_line(t, span, head, walk);
            } else {
                self.line(t, w, span, walk);
            }
            start = w.next_line(t, span);
        }
    }

    /// The runs of an ordinary line: one row each with soft wrap off, more
    /// when it wraps.
    fn line(&mut self, t: &dyn Text, w: &mut WrapView<'_>, span: LineSpan, walk: &mut Walk) {
        let whole = read(t, span.start..span.next.unwrap_or(span.end));
        let (text, brk) = whole.split_at((span.end - span.start).min(whole.len()));
        self.rows.clear();
        self.rows.extend_from_slice(w.rows(text));
        let rows = std::mem::take(&mut self.rows);
        if let Some(top) = walk
            .top
            .filter(|top| top.start == span.start && top.row < rows.len())
        {
            walk.top_row = Some(walk.row + top.row);
        }
        if self.reuse(span.start, text, brk, &rows, walk) {
            self.rows = rows;
            return;
        }
        self.words.clear();
        self.words.extend(word_starts(text));
        for (r, row) in rows.iter().enumerate() {
            let end = rows.get(r + 1).map_or(text.len(), |next| next.byte);
            let last = r + 1 == rows.len();
            let piece = RowPiece {
                line: span.start,
                line_row: r,
                long: false,
                start: span.start + row.byte,
                text: &text[row.byte..end],
                brk: if last { brk } else { "" },
                col: row.col,
                x: 0.0,
                line_start: row.byte == 0,
            };
            walk.row_runs(&piece, &self.words, row.byte);
            walk.row += 1;
        }
        self.rows = rows;
    }

    /// Take the line starting at `line`'s runs from the previous update,
    /// when its text and rows are unchanged: cutting runs and finding
    /// words is most of an update's cost, and an edit changes one line.
    fn reuse(
        &mut self,
        line: usize,
        text: &str,
        brk: &str,
        rows: &[RowStart],
        walk: &mut Walk,
    ) -> bool {
        let old = &mut self.old;
        while self.old_at < old.len() && old[self.old_at].line < line {
            self.old_at += 1;
        }
        let from = self.old_at;
        let mut to = from;
        while to < old.len() && old[to].line == line {
            to += 1;
        }
        if from == to || !same_line(&old[from..to], line, text, brk, rows) {
            return false;
        }
        for run in &mut old[from..to] {
            let mut run = std::mem::take(run);
            run.row = walk.row + run.line_row;
            walk.runs.push(run);
        }
        walk.row += rows.len();
        self.old_at = to;
        true
    }

    /// A long line (over 64 KB) is never read whole: only the part around
    /// the cursor (or its start, when the cursor is elsewhere) is
    /// described, as one row.
    fn long_line(&mut self, t: &dyn Text, span: LineSpan, head: usize, walk: &mut Walk) {
        let at = if (span.start..=span.end).contains(&head) {
            head
        } else {
            span.start
        };
        // Centre on the cursor's block, so moving within it keeps the same
        // text and nothing is sent again.
        let block = span.start + (at - span.start) / long_line::BLOCK * long_line::BLOCK;
        let centre = (block + long_line::BLOCK / 2).min(span.end);
        let (text, base) = long_line::around(t, span, centre);
        let reaches_end = base + text.len() == span.end;
        let brk = if reaches_end {
            t.slice(span.end..span.next.unwrap_or(span.end))
        } else {
            "".into()
        };
        if walk.top.is_some_and(|top| top.start == span.start) {
            walk.top_row = Some(walk.row);
        }
        let col = long_line::col_at(t, span, base, walk.geometry.tab_width);
        #[expect(clippy::cast_possible_truncation, reason = "an on-screen offset")]
        let x = (f64::from(walk.geometry.cell) * f64_of(col) - walk.long_scroll) as f32;
        self.words.clear();
        self.words.extend(word_starts(&text));
        let piece = RowPiece {
            line: span.start,
            line_row: 0,
            long: true,
            start: base,
            text: &text,
            brk: &brk,
            col,
            x,
            line_start: base == span.start,
        };
        walk.row_runs(&piece, &self.words, 0);
        walk.row += 1;
    }

    /// Send the walked runs that differ from what was sent, and remember
    /// them.
    fn send(&mut self, walk: &Walk, geometry: Geometry, out: &mut Vec<(NodeId, Node)>) {
        self.children.clear();
        self.sent.truncate(walk.runs.len());
        for (i, run) in walk.runs.iter().enumerate() {
            let node_id = id::run(self.field, self.generation, i);
            self.children.push(node_id);
            let sig = signature(run, geometry);
            let same = self.sent.get(i) == Some(&sig);
            if let Some(old) = self.sent.get_mut(i) {
                *old = sig;
            } else {
                self.sent.push(sig);
            }
            if !same {
                let prev = run
                    .continues
                    .then(|| id::run(self.field, self.generation, i - 1));
                let next = run
                    .continued
                    .then(|| id::run(self.field, self.generation, i + 1));
                out.push((node_id, run_node(run, geometry, prev, next)));
            }
        }
    }

    /// The run and character showing document offset `pos`. A row's end
    /// is the next row's start, so it belongs to the next run; the end of
    /// a line's content is its line break. Offsets outside the window are
    /// clamped to its first or last position.
    fn position(&self, pos: usize) -> Option<TextPosition> {
        let i = self
            .runs
            .partition_point(|r| r.start <= pos)
            .saturating_sub(1);
        let run = self.runs.get(i)?;
        let lens = char_lengths(&run.text, run.content);
        let off = pos.saturating_sub(run.start);
        let k = if off >= run.content && run.text.len() > run.content {
            // The end of the line's content: on its line break.
            lens.len() - 1
        } else {
            let mut acc = 0;
            let mut k = 0;
            while k < lens.len() && acc < off {
                acc += usize::from(lens[k]);
                k += 1;
            }
            k
        };
        Some(TextPosition {
            node: self.children[i],
            character_index: k,
        })
    }
}

/// A piece of a visual row to cut into runs.
struct RowPiece<'a> {
    /// Where its line starts, its row in the line, and whether it is a
    /// piece of a long line (see [`Run`]).
    line: usize,
    line_row: usize,
    long: bool,
    /// Document byte where `text` starts.
    start: usize,
    /// The row's text, without its line break.
    text: &'a str,
    /// The line break after it, on a line's last row.
    brk: &'a str,
    /// Display column where it starts within its line.
    col: usize,
    /// Pixels right of column 0 that `col` is drawn at, for long lines.
    x: f32,
    /// It starts its line, so its first character starts a word.
    line_start: bool,
}

/// Runs collected over one walk of the window.
struct Walk {
    geometry: Geometry,
    long_scroll: f64,
    top: Option<RowPos>,
    top_row: Option<usize>,
    reached_cursor: bool,
    row: usize,
    runs: Vec<Run>,
}

impl Walk {
    /// A walk collecting into `runs` (empty, kept for its capacity).
    fn new(geometry: Geometry, long_scroll: f64, top: Option<RowPos>, runs: Vec<Run>) -> Self {
        Self {
            geometry,
            long_scroll,
            top,
            top_row: None,
            reached_cursor: false,
            row: 0,
            runs,
        }
    }

    /// Cut one row into runs of at most [`RUN_CHARS`] characters. `words`
    /// are word starts as byte offsets into the line (or long-line piece)
    /// the row is part of; the row starts `row_byte` bytes into it.
    fn row_runs(&mut self, piece: &RowPiece<'_>, words: &[usize], row_byte: usize) {
        let tab = self.geometry.tab_width;
        let mut cut = 0;
        let mut col = piece.col;
        let first = self.runs.len();
        let mut boundaries = piece.text.grapheme_indices(true).map(|(i, _)| i).peekable();
        loop {
            // Up to RUN_CHARS graphemes from `cut`.
            let mut end = piece.text.len();
            let mut count = 0;
            while let Some(&i) = boundaries.peek() {
                if i < cut {
                    boundaries.next();
                    continue;
                }
                if count == RUN_CHARS {
                    end = i;
                    break;
                }
                boundaries.next();
                count += 1;
            }
            let last = end == piece.text.len();
            let content = &piece.text[cut..end];
            let mut text = String::with_capacity(content.len() + piece.brk.len());
            text.push_str(content);
            if last {
                text.push_str(piece.brk);
            }
            let word_idx = run_words(content, words, row_byte + cut, piece.line_start && cut == 0);
            self.runs.push(Run {
                line: piece.line,
                line_row: piece.line_row,
                long: piece.long,
                start: piece.start + cut,
                text,
                content: content.len(),
                row: self.row,
                col,
                x: piece.x,
                words: word_idx,
                continues: self.runs.len() > first,
                continued: !last,
            });
            if last {
                break;
            }
            col = column::col_at_from(content, content.len(), col, tab);
            cut = end;
        }
    }
}

/// The text in `range`, borrowed straight from the buffer when it lies in
/// one chunk (most lines do): cheaper than [`Text::slice`], which looks up
/// both ends to widen them to character boundaries.
fn read(t: &dyn Text, range: std::ops::Range<usize>) -> std::borrow::Cow<'_, str> {
    let (chunk, at) = t.chunk_at(range.start);
    if range.start >= at
        && let Some(bytes) = chunk.get(range.start - at..range.end - at)
        && let Ok(s) = std::str::from_utf8(bytes)
    {
        return std::borrow::Cow::Borrowed(s);
    }
    t.slice(range)
}

/// Whether `runs`, the previous update's runs of the line starting at
/// `line`, still describe it: the same text, cut at the same rows.
fn same_line(runs: &[Run], line: usize, text: &str, brk: &str, rows: &[RowStart]) -> bool {
    let mut at = 0;
    let mut row = 0;
    for run in runs {
        if run.long || run.start != line + at {
            return false;
        }
        if !run.continues {
            match rows.get(row) {
                Some(r) if r.byte == at && r.col == run.col && run.line_row == row => row += 1,
                _ => return false,
            }
        }
        let Some(content) = text.get(at..at + run.content) else {
            return false;
        };
        if run.text[..run.content] != *content {
            return false;
        }
        at += run.content;
        let tail = &run.text[run.content..];
        if !tail.is_empty() && (at != text.len() || tail != brk) {
            return false;
        }
    }
    let ends = runs
        .last()
        .is_some_and(|last| last.text.len() - last.content == brk.len());
    at == text.len() && row == rows.len() && ends
}

/// A hash of everything a run's node is built from, so an update can tell
/// which nodes changed without keeping the previous runs' text.
fn signature(run: &Run, g: Geometry) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    run.text.hash(&mut h);
    run.content.hash(&mut h);
    run.row.hash(&mut h);
    run.col.hash(&mut h);
    run.x.to_bits().hash(&mut h);
    run.words.hash(&mut h);
    run.continues.hash(&mut h);
    run.continued.hash(&mut h);
    g.cell.to_bits().hash(&mut h);
    g.line_height.to_bits().hash(&mut h);
    g.tab_width.hash(&mut h);
    h.finish()
}

/// Word starts within a run, as character indexes. `words` are byte
/// offsets into the line (or long-line piece) the run is cut from, and the
/// run's content starts `offset` bytes into it.
fn run_words(content: &str, words: &[usize], offset: usize, starts_line: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let from = words.partition_point(|&b| b < offset);
    let mut k: u8 = 0;
    let mut chars = char_lengths(content, content.len()).into_iter();
    let mut at = offset;
    for &b in &words[from..] {
        if b >= offset + content.len() {
            break;
        }
        while at < b {
            let Some(l) = chars.next() else { break };
            at += usize::from(l);
            k = k.saturating_add(1);
        }
        if at == b && !(k == 0 && starts_line) {
            out.push(k);
        }
    }
    out
}

/// Byte offsets where words start in `line`: the start of each run of
/// word characters or of punctuation, the classes Alt+Left and Alt+Right
/// stop at (`nimbus_core::movement`). Spaces belong to the word before
/// them; spaces at the start of a line are a word of their own, which
/// starts the line.
fn word_starts(line: &str) -> impl Iterator<Item = usize> + '_ {
    #[derive(PartialEq, Clone, Copy)]
    enum Class {
        Word,
        Space,
        Punct,
    }
    fn class(g: &str) -> Class {
        match g.chars().next() {
            Some(c) if c.is_alphanumeric() || c == '_' => Class::Word,
            Some(c) if c.is_whitespace() => Class::Space,
            _ => Class::Punct,
        }
    }
    let mut prev = None;
    line.grapheme_indices(true).filter_map(move |(i, g)| {
        let c = class(g);
        let starts = c != Class::Space && prev != Some(c);
        prev = Some(c);
        starts.then_some(i)
    })
}

/// The byte length of each character of `text`: its graphemes before
/// `content`, then the line break after it as one character. A grapheme
/// longer than 255 bytes (a pile of combining marks) is split at code
/// point boundaries, since lengths are `u8`.
fn char_lengths(text: &str, content: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(content);
    for g in text[..content].graphemes(true) {
        push_lengths(g, &mut out);
    }
    if text.len() > content {
        push_lengths(&text[content..], &mut out);
    }
    out
}

/// Push `g`'s length, in pieces of at most 255 bytes on char boundaries.
fn push_lengths(g: &str, out: &mut Vec<u8>) {
    let mut rest = g;
    while !rest.is_empty() {
        let mut cut = rest.len().min(255);
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        out.push(u8::try_from(cut).unwrap_or(u8::MAX));
        rest = &rest[cut..];
    }
}

/// The node for one run: its text, characters, words and where each
/// character is drawn, in the text field's coordinates (x from column 0 of
/// the line, y from the window's first row).
fn run_node(run: &Run, g: Geometry, prev: Option<NodeId>, next: Option<NodeId>) -> Node {
    let mut node = Node::new(Role::TextRun);
    let lens = char_lengths(&run.text, run.content);
    let mut positions = Vec::with_capacity(lens.len());
    let mut widths = Vec::with_capacity(lens.len());
    let mut col = run.col;
    for gph in run.text[..run.content].graphemes(true) {
        let cols = column::grapheme_cols(gph, col, g.tab_width);
        let pieces = gph.len().div_ceil(255).max(1);
        for p in 0..pieces {
            positions.push(f(col - run.col) * g.cell);
            widths.push(if p == 0 { f(cols) * g.cell } else { 0.0 });
        }
        col += cols;
    }
    if run.text.len() > run.content {
        positions.push(f(col - run.col) * g.cell);
        widths.push(0.0);
    }
    let x0 = f64::from(run.x) + f64::from(f(run.col) * g.cell);
    let y0 = f64::from(f(run.row) * g.line_height);
    node.set_bounds(Rect {
        x0,
        y0,
        x1: x0 + f64::from(f(col - run.col) * g.cell),
        y1: y0 + f64::from(g.line_height),
    });
    node.set_value(run.text.as_str());
    node.set_character_lengths(lens);
    node.set_character_positions(positions);
    node.set_character_widths(widths);
    node.set_word_starts(run.words.clone());
    if let Some(prev) = prev {
        node.set_previous_on_line(prev);
    }
    if let Some(next) = next {
        node.set_next_on_line(next);
    }
    node
}

#[expect(
    clippy::cast_precision_loss,
    reason = "column and row counts on screen"
)]
fn f(n: usize) -> f32 {
    n as f32
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a column, for an offset in pixels"
)]
fn f64_of(n: usize) -> f64 {
    n as f64
}

/// The byte range of the window's text, for tests.
#[cfg(test)]
impl TextWindow {
    fn span(&self) -> std::ops::Range<usize> {
        let first = self.runs.first().map_or(0, |r| r.start);
        let last = self.runs.last().map_or(0, |r| r.start + r.text.len());
        first..last
    }

    fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nimbus_core::wrap::WrapCache;
    use nimbus_core::{Folds, RopeBuffer};

    const G: Geometry = Geometry {
        cell: 10.0,
        line_height: 20.0,
        tab_width: 4,
    };

    fn frame() -> Frame {
        Frame {
            top: None,
            geometry: G,
            long_scroll: 0.0,
        }
    }

    /// `n` numbered lines.
    fn numbered(n: usize, words: &str) -> String {
        use std::fmt::Write as _;
        (0..n).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "line {i}{words}");
            s
        })
    }

    fn update(
        win: &mut TextWindow,
        t: &RopeBuffer,
        wrap: Option<usize>,
        sel: Selection,
    ) -> (Placed, Vec<(NodeId, Node)>) {
        let mut cache = WrapCache::default();
        let mut w = WrapView::new(&mut cache, wrap, G.tab_width);
        let mut out = Vec::new();
        let placed = win.update(t, &mut w, sel, frame(), &mut out);
        (placed, out)
    }

    /// Every position in `text` maps to a run and back to itself.
    fn round_trips(text: &str, wrap: Option<usize>) {
        let t = RopeBuffer::from_text(text);
        let mut win = TextWindow::new(1);
        update(&mut win, &t, wrap, Selection::cursor(0));
        assert_eq!(win.text(), text, "the window holds a short text whole");
        for (pos, _) in text.char_indices().chain([(text.len(), ' ')]) {
            // Only positions a cursor can take: grapheme boundaries, not
            // inside a CRLF.
            let snapped = nimbus_core::movement::clamp(&t, pos);
            let is_boundary =
                pos == text.len() || text.grapheme_indices(true).any(|(i, _)| i == pos);
            if snapped != pos || !is_boundary {
                continue;
            }
            let at = win.position(pos).expect("a position");
            assert_eq!(win.byte_of(at), Some(pos), "{text:?} at {pos}");
        }
    }

    #[test]
    fn positions_map_both_ways() {
        round_trips("", None);
        round_trips("hello", None);
        round_trips("one\ntwo\n", None);
        round_trips("a\r\nb\r\n\r\nc", None);
        round_trips("e\u{301}t\u{e9} 日本 👨‍👩‍👧 x", None);
        round_trips("hello world, this wraps\nnext", Some(6));
    }

    #[test]
    fn runs_end_with_their_line_break_counted_as_one_character() {
        let t = RopeBuffer::from_text("ab\r\ncd");
        let mut win = TextWindow::new(1);
        let (_, nodes) = update(&mut win, &t, None, Selection::cursor(0));
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].1.value(), Some("ab\r\n"));
        assert_eq!(nodes[0].1.character_lengths(), &[1, 1, 2]);
        assert_eq!(nodes[1].1.value(), Some("cd"));
        // The end of a line's content is on its break.
        let at = win.position(2).expect("a position");
        assert_eq!((at.node, at.character_index), (win.children()[0], 2));
        let at = win.position(4).expect("a position");
        assert_eq!((at.node, at.character_index), (win.children()[1], 0));
    }

    #[test]
    fn wrapped_rows_are_runs_of_their_own() {
        let t = RopeBuffer::from_text("hello world\n");
        let mut win = TextWindow::new(1);
        let (_, nodes) = update(&mut win, &t, Some(8), Selection::cursor(0));
        let values: Vec<_> = nodes.iter().map(|(_, n)| n.value().unwrap_or("")).collect();
        assert_eq!(values, ["hello ", "world\n", ""]);
        // Rows sit one line height apart.
        let y: Vec<_> = nodes
            .iter()
            .map(|(_, n)| n.bounds().map(|b| b.y0))
            .collect();
        assert_eq!(y, [Some(0.0), Some(20.0), Some(40.0)]);
        // A row break belongs to the next row.
        let at = win.position(6).expect("a position");
        assert_eq!((at.node, at.character_index), (win.children()[1], 0));
    }

    #[test]
    fn words_follow_the_editors_word_movement() {
        assert_eq!(
            word_starts("  foo.bar(x) baz").collect::<Vec<_>>(),
            [2, 5, 6, 9, 10, 11, 13]
        );
        assert_eq!(run_words("  foo.bar", &[2, 5, 6], 0, true), [2, 5, 6]);
        assert_eq!(
            run_words("foo bar", &[0, 4], 0, true),
            [4],
            "0 is implied at a line start"
        );
        assert_eq!(run_words("bar", &[0, 4], 4, false), [0]);
    }

    #[test]
    fn positions_and_widths_follow_columns_and_tabs() {
        let t = RopeBuffer::from_text("\tx日y");
        let mut win = TextWindow::new(1);
        let (_, nodes) = update(&mut win, &t, None, Selection::cursor(0));
        let node = &nodes[0].1;
        assert_eq!(
            node.character_positions(),
            Some(&[0.0, 40.0, 50.0, 70.0][..])
        );
        assert_eq!(node.character_widths(), Some(&[40.0, 10.0, 20.0, 10.0][..]));
    }

    #[test]
    fn long_rows_are_several_runs_on_one_line() {
        let text = "ab ".repeat(200);
        let t = RopeBuffer::from_text(&text);
        let mut win = TextWindow::new(1);
        let (_, nodes) = update(&mut win, &t, None, Selection::cursor(0));
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].1.character_lengths().len(), RUN_CHARS);
        assert_eq!(nodes[0].1.next_on_line(), Some(nodes[1].0));
        assert_eq!(nodes[1].1.previous_on_line(), Some(nodes[0].0));
        assert_eq!(nodes[2].1.next_on_line(), None);
        assert_eq!(win.text(), text);
        // The second run starts 250 characters in: at column 250.
        assert_eq!(nodes[1].1.bounds().map(|b| b.x0), Some(2500.0));
        let at = win.position(251).expect("a position");
        assert_eq!((at.node, at.character_index), (nodes[1].0, 1));
        assert_eq!(win.byte_of(at), Some(251));
    }

    #[test]
    fn a_cursor_move_sends_nothing_and_an_edit_sends_its_line() {
        let mut t = RopeBuffer::from_text("one\ntwo\nthree\n");
        let mut win = TextWindow::new(1);
        let (_, first) = update(&mut win, &t, None, Selection::cursor(0));
        assert_eq!(first.len(), 4);
        let (placed, again) = update(&mut win, &t, None, Selection::cursor(5));
        assert!(again.is_empty(), "moving the cursor resends no runs");
        let sel = placed.selection.expect("a selection");
        assert_eq!(
            (sel.focus.node, sel.focus.character_index),
            (win.children()[1], 1)
        );
        nimbus_core::Buffer::replace(&mut t, 5..5, "X").expect("an edit");
        let (_, edited) = update(&mut win, &t, None, Selection::cursor(6));
        assert_eq!(edited.len(), 1);
        assert_eq!(edited[0].1.value(), Some("tXwo\n"));
        // A new line moves the lines below it down a row: they are sent.
        nimbus_core::Buffer::replace(&mut t, 0..0, "zero\n").expect("an edit");
        win.follow(|p| if p == 0 { 0 } else { p + 5 });
        let (_, moved) = update(&mut win, &t, None, Selection::cursor(0));
        assert_eq!(moved.len(), 5);
    }

    /// An edit that keeps every offset (one letter replaced by another)
    /// still resends its line: reuse checks the text, not the offsets.
    #[test]
    fn a_same_length_replace_resends_its_line() {
        let mut t = RopeBuffer::from_text("one\ntwo\nthree\n");
        let mut win = TextWindow::new(1);
        update(&mut win, &t, None, Selection::cursor(0));
        nimbus_core::Buffer::replace(&mut t, 4..5, "T").expect("an edit");
        win.follow(|p| p);
        let (_, edited) = update(&mut win, &t, None, Selection::cursor(0));
        assert_eq!(edited.len(), 1);
        assert_eq!(edited[0].1.value(), Some("Two\n"));
        assert_eq!(win.text(), "one\nTwo\nthree\n");
    }

    #[test]
    fn a_selection_reaching_outside_the_window_is_clamped_to_it() {
        let text = numbered(2000, "");
        let t = RopeBuffer::from_text(&text);
        let mut win = TextWindow::new(1);
        let head = text.find("line 1000\n").expect("the line");
        let (placed, _) = update(&mut win, &t, None, Selection { anchor: 0, head });
        let span = win.span();
        assert_eq!(&text[span.start..span.start + 9], "line 800\n");
        assert!(
            text[span.clone()].ends_with("line 1200\n"),
            "{:?}",
            &text[span.end - 20..span.end]
        );
        let sel = placed.selection.expect("a selection");
        assert_eq!(
            win.byte_of(sel.anchor),
            Some(span.start),
            "clamped to the window"
        );
        assert_eq!(win.byte_of(sel.focus), Some(head));
    }

    #[test]
    fn the_window_stays_until_the_cursor_nears_its_edge_then_moves() {
        let text = numbered(2000, "");
        let t = RopeBuffer::from_text(&text);
        let at = |n: usize| text.find(&format!("line {n}\n")).expect("the line");
        let mut win = TextWindow::new(1);
        update(&mut win, &t, None, Selection::cursor(at(1000)));
        let ids = win.children().to_vec();
        let (_, sent) = update(&mut win, &t, None, Selection::cursor(at(1100)));
        assert!(sent.is_empty(), "the cursor moved inside the window");
        assert_eq!(win.children(), ids.as_slice(), "same window, same ids");
        let (_, sent) = update(&mut win, &t, None, Selection::cursor(at(1160)));
        assert!(!sent.is_empty(), "rebuilt around the cursor");
        assert_ne!(win.children()[0], ids[0], "a new generation of ids");
        assert_eq!(win.span().start, at(960));
    }

    #[test]
    fn huge_texts_describe_a_bounded_window() {
        // Lines of 1 KB: the byte limit, not the line limit, bounds it.
        let line = format!("{}\n", "x".repeat(1023));
        let text = line.repeat(4096);
        let t = RopeBuffer::from_text(&text);
        let mut win = TextWindow::new(1);
        update(&mut win, &t, None, Selection::cursor(text.len() / 2));
        let span = win.span();
        assert!(
            span.len() <= 2 * WINDOW_BYTES + 2 * line.len(),
            "{}",
            span.len()
        );
        assert!(span.contains(&(text.len() / 2)));
    }

    #[test]
    fn a_long_line_is_read_around_the_cursor() {
        let text = "a".repeat(200_000);
        let t = RopeBuffer::from_text(&text);
        let mut win = TextWindow::new(1);
        let (placed, _) = update(&mut win, &t, None, Selection::cursor(100_000));
        let span = win.span();
        assert!(span.len() < 3 * long_line::NEAR, "{span:?}");
        assert!(span.contains(&100_000));
        let sel = placed.selection.expect("a selection");
        assert_eq!(win.byte_of(sel.focus), Some(100_000));
        let (_, sent) = update(&mut win, &t, None, Selection::cursor(100_001));
        assert!(sent.is_empty(), "moving within the block resends nothing");
    }

    #[test]
    fn folded_lines_are_left_out() {
        let t = RopeBuffer::from_text("a\nb\nc\nd\n");
        let mut folds = Folds::default();
        folds.fold(3..5);
        let mut cache = WrapCache::default();
        let mut w = WrapView::new(&mut cache, None, 4).with_folds(&folds);
        let mut win = TextWindow::new(1);
        let mut out = Vec::new();
        win.update(&t, &mut w, Selection::cursor(0), frame(), &mut out);
        let values: Vec<_> = out.iter().map(|(_, n)| n.value().unwrap_or("")).collect();
        assert_eq!(values, ["a\n", "b\n", "d\n", ""]);
    }

    /// AccessKit's own copy of a tree holding one text field, as a screen
    /// reader's platform keeps it, updated the way the app updates it.
    struct Reader {
        tree: Option<accesskit_consumer::Tree>,
    }

    struct Ignore;
    impl accesskit_consumer::TreeChangeHandler for Ignore {
        fn node_added(&mut self, _: &accesskit_consumer::NodeRef<'_>) {}
        fn node_updated(
            &mut self,
            _: &accesskit_consumer::NodeRef<'_>,
            _: &accesskit_consumer::NodeRef<'_>,
        ) {
        }
        fn focus_moved(
            &mut self,
            _: Option<&accesskit_consumer::NodeRef<'_>>,
            _: Option<&accesskit_consumer::NodeRef<'_>>,
        ) {
        }
        fn node_removed(&mut self, _: &accesskit_consumer::NodeRef<'_>) {}
    }

    impl Reader {
        const ROOT: NodeId = NodeId(1);
        const FIELD: NodeId = NodeId(2);

        /// Apply the changed `runs` and the field (with `placed`'s
        /// selection) as one update; the first one is a full tree.
        fn apply(&mut self, win: &TextWindow, mut runs: Vec<(NodeId, Node)>, placed: Placed) {
            use accesskit::{TreeId, TreeInfo, TreeUpdate};
            let mut field = Node::new(Role::MultilineTextInput);
            field.set_children(win.children().to_vec());
            field.set_text_direction(accesskit::TextDirection::LeftToRight);
            if let Some(sel) = placed.selection {
                field.set_text_selection(sel);
            }
            runs.push((Self::FIELD, field));
            let full = self.tree.is_none();
            if full {
                let mut root = Node::new(Role::Window);
                root.set_children(vec![Self::FIELD]);
                runs.push((Self::ROOT, root));
            }
            let update = TreeUpdate {
                nodes: runs,
                tree: full.then(|| TreeInfo::new(Self::ROOT)),
                tree_id: TreeId::ROOT,
                focus: Self::FIELD,
            };
            match self.tree.as_mut() {
                None => self.tree = Some(accesskit_consumer::Tree::new(update, true)),
                Some(tree) => tree.update_and_process_changes(update, &mut Ignore),
            }
        }

        /// The field's whole text and the caret, as the reader sees them.
        fn read(&self) -> (String, TextPosition) {
            let tree = self.tree.as_ref().expect("a tree");
            let field = tree.state().focus().expect("the field has focus");
            let caret = field.text_selection_focus().expect("a caret");
            let at = TextPosition {
                node: caret.inner_node().locate().0,
                character_index: caret.to_raw().character_index,
            };
            (field.document_range().text(), at)
        }
    }

    /// A small deterministic random number generator (xorshift), so the
    /// test needs no crate and fails the same way every run.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            usize::try_from(self.0 % (n.max(1) as u64)).unwrap_or(0)
        }
    }

    /// Random edits, cursor moves and wrap changes, each applied to
    /// AccessKit's own copy of the tree as an incremental update: the copy
    /// must accept every update and always read back the window's text,
    /// with the caret where the editor has it.
    #[test]
    fn incremental_updates_keep_the_readers_copy_right() {
        use nimbus_core::Buffer;

        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        let snippets = ["a", "word ", "\n", "\r\n", "é", "👨‍👩‍👧", "\t", "x\ny\n"];
        let long = "w".repeat(300);
        let mut oracle = numbered(600, " has words");
        let mut buffer = RopeBuffer::from_text(&oracle);
        let mut win = TextWindow::new(0);
        let mut reader = Reader { tree: None };
        let (mut head, mut wrap) = (0, None);
        for step in 0..400 {
            let at = nimbus_core::movement::clamp(&buffer, rng.below(oracle.len() + 1));
            match rng.below(10) {
                0..=3 => {
                    let piece = if rng.below(20) == 0 {
                        long.as_str()
                    } else {
                        snippets[rng.below(snippets.len())]
                    };
                    buffer.replace(at..at, piece).expect("an insert");
                    oracle.insert_str(at, piece);
                    win.follow(|p| if p <= at { p } else { p + piece.len() });
                    head = at + piece.len();
                }
                4 | 5 => {
                    let end = (at + rng.below(40)).min(oracle.len());
                    let end = nimbus_core::movement::clamp(&buffer, end);
                    buffer.replace(at..end, "").expect("a delete");
                    oracle.replace_range(at..end, "");
                    win.follow(|p| p.min(at).max(p.saturating_sub(end - at)));
                    head = at;
                }
                6 => wrap = if wrap.is_some() { None } else { Some(12) },
                _ => head = at,
            }
            // A caret sits between characters: snap it forward to one.
            head = oracle
                .grapheme_indices(true)
                .map(|(i, _)| i)
                .chain([oracle.len()])
                .find(|&i| i >= head)
                .unwrap_or(oracle.len());
            let mut cache = WrapCache::default();
            let mut rows = WrapView::new(&mut cache, wrap, 4);
            let mut runs = Vec::new();
            let sel = Selection::cursor(head);
            let placed = win.update(&buffer, &mut rows, sel, frame(), &mut runs);
            reader.apply(&win, runs, placed);
            let (text, caret) = reader.read();
            let span = win.span();
            assert_eq!(text, win.text(), "step {step}");
            assert_eq!(
                text,
                oracle[span.clone()],
                "step {step}: the window is the text"
            );
            assert!(span.contains(&head) || head == span.end, "step {step}");
            assert_eq!(
                win.byte_of(caret),
                Some(head),
                "step {step}: the caret maps back"
            );
        }
    }

    /// Keystroke cost of keeping the editor's description current on a
    /// 10 MB file: after each typed character, the window is followed
    /// through the edit and the changed runs rebuilt. The plan's budget is under
    /// 1 ms a keystroke with a screen reader running.
    /// `cargo test --release -p nimbus --bin nimbus -- --ignored --nocapture access_typing_bench`
    #[test]
    #[ignore = "benchmark"]
    fn access_typing_bench() {
        use nimbus_core::Buffer;
        use std::time::Instant;

        let text = numbered(210_000, " = some_function(argument, other) + 1;");
        let mut buffer = RopeBuffer::from_text(&text);
        let mut win = TextWindow::new(1);
        let mut cache = WrapCache::default();
        let mut out = Vec::new();
        let mut at = text.len() / 2;
        let mut update = |win: &mut TextWindow, buffer: &RopeBuffer, at: usize| {
            let mut w = WrapView::new(&mut cache, None, G.tab_width);
            out.clear();
            win.update(buffer, &mut w, Selection::cursor(at), frame(), &mut out);
        };
        update(&mut win, &buffer, at);
        let mut times = Vec::new();
        for i in 0..2000 {
            let piece = if i % 40 == 39 { "\n" } else { "x" };
            buffer.replace(at..at, piece).expect("an insert");
            let start = Instant::now();
            let edit = at;
            win.follow(|p| if p <= edit { p } else { p + piece.len() });
            at += piece.len();
            update(&mut win, &buffer, at);
            times.push(start.elapsed());
        }
        times.sort_unstable();
        let pick = |q: usize| times[(times.len() - 1) * q / 100];
        eprintln!(
            "access typing on {} MB: p50 {:?}  p99 {:?}  max {:?} (the cost on top of the edit)",
            text.len() >> 20,
            pick(50),
            pick(99),
            pick(100)
        );
    }
}
