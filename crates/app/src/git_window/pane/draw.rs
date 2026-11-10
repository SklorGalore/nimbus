//! Drawing the git window, and the input that depends on where things
//! were drawn (clicks, the wheel, dragging dividers, keys that move
//! through a list).
//!
//! **The graph's lines.** Each row's band is one row high. A lane is a
//! column `lane_w` wide; its line runs down the column's middle. Lines
//! that pass through are straight; a line that comes into the commit's
//! dot from another lane is a cubic Bézier curve from the top of its
//! column to the dot, with both control points at mid-height, so it
//! leaves and arrives vertically and joins the straight lines above and
//! below without a kink. Lines leaving the dot for its parents are the
//! same curve upside down. The renderer turns each curve into smooth,
//! anti-aliased pixels (`render`'s segment pipeline, and its twin in
//! compatibility mode).

use nimbus_render::Gpu;

use super::{
    DiffPane, Focus, Geo, GitWindow, Hit, InfoLine, Selected, SideItem, f, fade, kind_color,
    lane_color, lock, whole,
};
use crate::compare::Toggle;
use crate::prompt;
use nimbus_core::Text as _;

use crate::git::RefKind;
use crate::input::{Command, GitCmd, Motion};
use crate::theme::{Theme, Ui};
use crate::view::{self, Layout};

/// What the window must do after input the pane handled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing more: redraw if it changed something.
    Done(bool),
    /// Open this file in the editor.
    OpenInEditor(std::path::PathBuf),
    /// Put this text on the clipboard.
    Copy(String),
    /// A command for the window itself (zoom, theme, quit).
    Window,
}

impl GitWindow {
    pub(super) fn geo(&self, gpu: &Gpu, scale: f32) -> Geo {
        let (w, h) = gpu.size();
        let (w, h) = (f(w as usize), f(h as usize));
        let m = gpu.metrics();
        let (lh, cell) = (m.line_height, m.cell_width);
        let margin = (8.0 * scale).round();
        let header = (lh + 1.5 * margin).round();
        let bottom = (h - lh).max(header + lh);
        let side_w = (self.side_cells * cell)
            .clamp(10.0 * cell, (w * 0.4).max(10.0 * cell))
            .round();
        let rest = (w - side_w).max(0.0);
        let details_w = (rest * self.details_share)
            .clamp(20.0 * cell, (rest - 20.0 * cell).max(20.0 * cell))
            .round();
        let split = (w - details_w).max(side_w);
        let changes = self.selected == Selected::Changes;
        // Changes also holds the commit area: keep room for it, and give
        // the details at least 60% of the height.
        let least = if changes { 11.0 } else { 3.0 } * lh;
        let share = if changes {
            self.info_share.max(0.6)
        } else {
            self.info_share
        };
        let info_bottom = (header + (bottom - header) * share)
            .clamp(header + least, (bottom - 4.0 * lh).max(header + least))
            .round();
        let commit_top = if changes {
            // Four lines of message and the button row; at most half the
            // details, so the file lists keep room.
            let commit_h = (5.0 * lh + 3.0 * margin).min((info_bottom - header) * 0.5);
            (info_bottom - commit_h).round()
        } else {
            info_bottom
        };
        let bar_rows = if changes && self.file_sel.is_some() {
            2
        } else {
            1
        };
        let bar_bottom = (info_bottom + f(bar_rows) * (lh + margin)).round();
        Geo {
            w,
            lh,
            cell,
            margin,
            header,
            bottom,
            side: (0.0, side_w),
            history: (side_w, split),
            details: (split, w),
            info_bottom,
            commit_top,
            bar_bottom,
            bar_rows,
            row_h: (lh * 1.3).round(),
            line: scale.round().max(1.0),
        }
    }

    /// A layout covering the window, for the status bar and for
    /// measuring interface text.
    fn base_layout(gpu: &Gpu, scale: f32, lines: usize) -> Layout {
        let (w, h) = gpu.size();
        Layout::new(gpu.metrics(), w, h, scale, lines)
            .with_chrome(false, 0, true)
            .with_ui(gpu.ui_font())
    }

    /// Queue the whole frame. `message` is the status bar's note, if any.
    pub fn draw(
        &mut self,
        gpu: &mut Gpu,
        theme: &mut Theme,
        scale: f32,
        message: Option<(&str, bool)>,
    ) {
        self.now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
        self.hits.clear();
        self.drawn_buttons.clear();
        let ui = theme.ui;
        view::begin_frame(gpu, &ui);
        let g = self.geo(gpu, scale);
        let base = Self::base_layout(gpu, scale, 1);
        self.draw_header(gpu, &ui, &g, &base);
        if self.repo.is_none() {
            let text = self
                .error
                .clone()
                .unwrap_or_else(|| "Opening the repository…".to_owned());
            let room = g.w - 2.0 * g.margin;
            let end = base.ui.fit(&text, room);
            let x = ((g.w - base.ui.width(&text[..end])) / 2.0).max(g.margin);
            let color = if self.error.is_some() {
                ui.error
            } else {
                ui.gutter
            };
            gpu.push_ui_text(
                [x.round(), f32::midpoint(g.header, g.bottom).round()],
                &text[..end],
                color,
            );
            view::draw_status(gpu, &ui, &base, message, "");
            return;
        }
        self.draw_side(gpu, &ui, &g, &base);
        self.draw_history(gpu, &ui, &g, &base);
        self.draw_info(gpu, &ui, &g, &base);
        if self.selected == Selected::Changes {
            self.draw_commit(gpu, theme, &g, &base, scale);
        }
        self.draw_bar(gpu, &ui, &g, &base);
        let info = self.draw_compare(gpu, theme, &g, scale);
        self.draw_dividers(gpu, &ui, &g);
        let busy = self.busy.clone().or_else(|| {
            self.committing
                .is_some()
                .then(|| "Committing… (Escape stops it)".to_owned())
        });
        let message = message.or_else(|| busy.as_deref().map(|b| (b, false)));
        view::draw_status(gpu, &ui, &base, message, &info);
        if let Some(p) = &self.prompt {
            p.draw(gpu, &ui, &base);
        }
        self.window_layout = Some(base);
    }

    /// A button: `label` in a box at `x`, row top `y`, `h` high; returns
    /// its right edge. Dimmed when not `usable`.
    #[expect(
        clippy::too_many_arguments,
        reason = "a button's place, look and meaning"
    )]
    fn button(
        &mut self,
        gpu: &mut Gpu,
        ui: &Ui,
        base: &Layout,
        (x, y, h): (f32, f32, f32),
        label: &'static str,
        (on, usable): (bool, bool),
        hit: Hit,
    ) -> f32 {
        let pad = (base.metrics.cell_width * 0.5).round();
        let w = base.ui.width(label) + 2.0 * pad;
        let bg = if on { ui.toggle_on } else { ui.field };
        gpu.push_rect([x, y], [w, h], bg);
        let ty = (y + (h - base.metrics.line_height) / 2.0).round();
        let color = if usable { ui.text } else { ui.gutter };
        gpu.push_ui_text([x + pad, ty], label, color);
        self.hits.push(([x, y, w, h], hit));
        self.drawn_buttons.push((hit, label, on, usable));
        x + w
    }

    /// Width of [`button`](Self::button) for `label`.
    fn button_w(base: &Layout, label: &str) -> f32 {
        base.ui.width(label) + 2.0 * (base.metrics.cell_width * 0.5).round()
    }

    /// The commit area under Changes' list: the message box, then Commit,
    /// Amend and who the commit will be by.
    #[expect(
        clippy::many_single_char_names,
        clippy::too_many_lines,
        reason = "g is the frame geometry and x, y, h a button's place, as everywhere in this file"
    )]
    fn draw_commit(&mut self, gpu: &mut Gpu, theme: &Theme, g: &Geo, base: &Layout, scale: f32) {
        let ui = theme.ui;
        let (x0, x1) = g.details;
        let (top, bottom) = (g.commit_top, g.info_bottom);
        gpu.begin_layer(Some([x0, top, x1 - x0, bottom - top]));
        gpu.push_rect([x0, top], [x1 - x0, bottom - top], ui.tab_bar);
        gpu.push_rect([x0, top], [x1 - x0, g.line], ui.picker_border);
        let row_h = (g.lh + g.margin).round();
        let box_top = top + g.margin;
        let box_bottom = (bottom - row_h - g.margin / 2.0).max(box_top + g.lh);
        let rect = [
            x0 + g.margin,
            box_top,
            x1 - x0 - 2.0 * g.margin,
            box_bottom - box_top,
        ];
        let window = Self::base_layout(gpu, scale, self.message.doc.buffer.len_lines());
        let placeholder = if self.message.amend {
            "The amended commit's message"
        } else {
            "Commit message"
        };
        let focused = self.focus == Focus::Message;
        self.message.draw(
            gpu,
            theme,
            window,
            rect,
            focused,
            self.tab_width,
            placeholder,
        );
        if focused {
            let b = g.line;
            gpu.begin_layer(Some([x0, top, x1 - x0, bottom - top]));
            let [rx, ry, rw, rh] = rect;
            for (p, sz) in [
                ([rx, ry], [rw, b]),
                ([rx, ry + rh - b], [rw, b]),
                ([rx, ry], [b, rh]),
                ([rx + rw - b, ry], [b, rh]),
            ] {
                gpu.push_rect(p, sz, ui.tab_accent);
            }
        }
        self.hits.push((rect, Hit::Message));
        gpu.begin_layer(Some([x0, top, x1 - x0, bottom - top]));
        let y = (bottom - row_h - g.margin / 4.0).round();
        let h = row_h - g.margin / 2.0;
        let mut x = x0 + g.margin;
        let staged = self.files.iter().any(|f| f.staged == Some(true));
        if self.committing.is_some() {
            x = self.button(
                gpu,
                &ui,
                base,
                (x, y, h),
                "Stop",
                (true, true),
                Hit::StopCommit,
            );
        } else {
            let label = if self.message.amend {
                "Amend"
            } else {
                "Commit"
            };
            let usable = (staged || self.message.amend) && !self.message.is_blank();
            x = self.button(
                gpu,
                &ui,
                base,
                (x, y, h),
                label,
                (usable, usable),
                Hit::Git(crate::input::GitCmd::Commit),
            );
        }
        x += g.cell;
        x = self.button(
            gpu,
            &ui,
            base,
            (x, y, h),
            "Amend last commit",
            (self.message.amend, true),
            Hit::Git(crate::input::GitCmd::ToggleAmend),
        );
        x += g.cell;
        self.scratch.clear();
        let color = match &self.prefill.author {
            Ok(a) => {
                let _ = write!(self.scratch, "by {a}");
                ui.gutter
            }
            Err(e) if e.is_empty() => ui.gutter,
            Err(e) => {
                let _ = write!(self.scratch, "no author: {e}");
                ui.error
            }
        };
        let ty = (y + (h - g.lh) / 2.0).round();
        let end = base.ui.fit(&self.scratch, (x1 - g.margin - x).max(0.0));
        gpu.push_ui_text([x, ty], &self.scratch[..end], color);
    }

    #[expect(
        clippy::many_single_char_names,
        reason = "g is the frame geometry and f the float cast, as everywhere in this file"
    )]
    fn draw_header(&mut self, gpu: &mut Gpu, ui: &Ui, g: &Geo, base: &Layout) {
        gpu.begin_layer(None);
        gpu.push_rect([0.0, 0.0], [g.w, g.header], ui.tab_bar);
        gpu.push_rect([0.0, g.header - g.line], [g.w, g.line], ui.picker_border);
        let y = ((g.header - g.lh) / 2.0).round();
        let mut x = g.margin;
        if let Some(repo) = &self.repo {
            x = gpu.push_ui_text([x, y], &repo.name(), ui.text) + 2.0 * g.cell;
        }
        if let Some(s) = &self.status {
            self.scratch.clear();
            match &s.branch {
                Some(b) => self.scratch.push_str(b),
                None => self.scratch.push_str("detached HEAD"),
            }
            if let Some((ahead, behind)) = s.ahead_behind
                && (ahead, behind) != (0, 0)
            {
                let _ = write!(self.scratch, "  ↑{ahead} ↓{behind}");
            }
            if let Some(u) = &s.upstream {
                let _ = write!(self.scratch, "  ({u})");
            }
            x = gpu.push_ui_text([x, y], &self.scratch, ui.tab_accent);
        }
        let _ = x;
        self.scratch.clear();
        if let Some(c) = &self.current {
            let h = lock(&c.history);
            let n = h.len();
            let more = c.preview || self.next.is_some();
            match &h.finished {
                Some(Ok(())) if more => {
                    let _ = write!(self.scratch, "Reading history…");
                }
                None => {
                    let _ = write!(self.scratch, "Reading history… {}", thousands(n));
                }
                Some(Ok(())) => {
                    let _ = write!(
                        self.scratch,
                        "{} commit{}",
                        thousands(n),
                        if n == 1 { "" } else { "s" }
                    );
                }
                Some(Err(e)) => {
                    let _ = write!(self.scratch, "git log: {e}");
                }
            }
        }
        let tw = base.ui.width(&self.scratch);
        gpu.push_ui_text([(g.w - g.margin - tw).round(), y], &self.scratch, ui.gutter);
    }

    fn draw_side(&mut self, gpu: &mut Gpu, ui: &Ui, g: &Geo, base: &Layout) {
        let (x0, x1) = g.side;
        gpu.begin_layer(Some([x0, g.header, x1 - x0, g.bottom - g.header]));
        gpu.push_rect([x0, g.header], [x1 - x0, g.bottom - g.header], ui.tab_bar);
        let rh = g.row_h;
        let total = f(self.side_items.len()) * rh;
        let room = g.bottom - g.header - g.margin;
        self.side_scroll = self.side_scroll.clamp(0.0, (total - room).max(0.0));
        let mut y = g.header + g.margin / 2.0 - self.side_scroll;
        let text_dy = ((rh - g.lh) / 2.0).round();
        let head = self.status.as_ref().and_then(|s| s.branch.clone());
        for (i, item) in self.side_items.iter().enumerate() {
            if y + rh < g.header {
                y += rh;
                continue;
            }
            if y > g.bottom {
                break;
            }
            let ty = (y + text_dy).round();
            let pad = g.margin + g.cell;
            match item {
                SideItem::Section(title, n) => {
                    let open = !self.collapsed.get(*n).copied().unwrap_or(false);
                    let mark = if open { "▾ " } else { "▸ " };
                    let x = gpu.push_ui_text([g.margin, ty], mark, ui.gutter);
                    gpu.push_ui_text([x, ty], title, ui.gutter);
                }
                SideItem::Changes => {
                    if self.selected == Selected::Changes {
                        let c = if self.focus == Focus::Sidebar {
                            ui.picker_selected
                        } else {
                            ui.selection
                        };
                        gpu.push_rect([x0, y], [x1 - x0, rh], c);
                    }
                    gpu.push_ui_text([g.margin, ty], "Changes", ui.text);
                    let n = self.status.as_ref().map_or(0, |s| s.entries.len());
                    self.scratch.clear();
                    let _ = write!(self.scratch, "{n}");
                    let w = base.ui.width(&self.scratch);
                    gpu.push_ui_text([(x1 - g.margin - w).round(), ty], &self.scratch, ui.gutter);
                }
                SideItem::Ref(r) => {
                    let Some(rf) = self.refs.get(*r) else {
                        continue;
                    };
                    let current = rf.kind == RefKind::Branch && head.as_deref() == Some(&rf.name);
                    let selected = matches!(&self.selected, Selected::Commit(o) if *o == rf.target);
                    if selected {
                        gpu.push_rect([x0, y], [x1 - x0, rh], fade(ui.selection, 0.6));
                    }
                    if current {
                        gpu.push_dot(
                            [g.margin + g.cell * 0.5, y + rh / 2.0],
                            g.lh * 0.35,
                            ui.tab_accent,
                        );
                    }
                    let room = x1 - pad - g.margin;
                    let end = base.ui.fit(&rf.name, room);
                    let color = if current {
                        ui.text
                    } else {
                        ui.current_line_number
                    };
                    gpu.push_ui_text([pad, ty], &rf.name[..end], color);
                }
                SideItem::Stash(s) => {
                    let Some(st) = self.stashes.get(*s) else {
                        continue;
                    };
                    self.scratch.clear();
                    let _ = write!(self.scratch, "{}  {}", st.name, st.subject);
                    let room = x1 - pad - g.margin;
                    let end = base.ui.fit(&self.scratch, room);
                    gpu.push_ui_text([pad, ty], &self.scratch[..end], ui.current_line_number);
                }
                SideItem::Note(n) => {
                    gpu.push_ui_text([pad, ty], n, fade(ui.gutter, 0.7));
                }
            }
            self.hits
                .push(([x0, y.max(g.header), x1 - x0, rh], Hit::Side(i)));
            y += rh;
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one pass over the visible rows: graph, labels and text together"
    )]
    #[expect(
        clippy::many_single_char_names,
        reason = "g is the frame geometry and f the float cast, as everywhere in this file"
    )]
    fn draw_history(&mut self, gpu: &mut Gpu, ui: &Ui, g: &Geo, base: &Layout) {
        let (x0, x1) = g.history;
        let (top, bottom) = (g.header, g.bottom);
        gpu.begin_layer(Some([x0, top, x1 - x0, bottom - top]));
        let Some(current) = &self.current else {
            return;
        };
        let history = std::sync::Arc::clone(&current.history);
        let h = lock(&history);
        let rh = g.row_h;
        let visible = whole((bottom - top) / rh).max(1);
        let len = h.len();
        if let Some(row) = self.pending_scroll.take() {
            let first = whole(self.scroll);
            if row < first {
                self.scroll = f(row);
            } else if row >= first + visible {
                self.scroll = f(row + 1 - visible);
            }
        }
        self.scroll = self.scroll.clamp(0.0, f(len.saturating_sub(visible)));
        if len == 0 {
            let note = match &h.finished {
                Some(Ok(())) => "No commits yet",
                Some(Err(_)) => "The history could not be read",
                None => "Reading history…",
            };
            gpu.push_ui_text([x0 + g.margin, top + g.margin], note, ui.gutter);
            return;
        }
        let first = whole(self.scroll);
        let offset = (self.scroll - f(first)) * rh;
        let lane_w = (g.cell * 1.6).round();
        let graph_x = x0 + g.margin;
        // As wide as the lanes on screen need, so a busy stretch far down
        // the history does not squeeze the subjects everywhere; at most
        // two fifths of the pane, the rest clipped.
        let shown = first..(first + visible + 2).min(len);
        let lanes = shown
            .clone()
            .map(|i| {
                let gr = h.graph(i);
                let widest = gr.pass.iter().chain(gr.ins).chain(gr.outs).max().copied();
                gr.lane
                    .min(MAX_LANES - 1)
                    .max(widest.map_or(0, usize::from))
                    + 1
            })
            .max()
            .unwrap_or(1);
        let graph_w = (f(lanes) * lane_w).min(((x1 - x0) * 0.4).round());
        let stroke = (1.6 * g.line).max(1.0);
        let dot = (g.lh * 0.42).round();
        let date_w = 10.0 * g.cell;
        let author_w = if x1 - x0 > 70.0 * g.cell {
            18.0 * g.cell
        } else {
            0.0
        };
        let head = self.status.as_ref().and_then(|s| s.head.clone());
        let text_dy = ((rh - g.lh) / 2.0).round();
        for i in shown {
            let y = top + f(i - first) * rh - offset;
            let selected = self.selected_row == Some(i);
            let row_bg = match (selected, self.focus == Focus::History) {
                (false, _) => ui.background,
                (true, true) => ui.picker_selected,
                (true, false) => ui.selection,
            };
            if selected {
                gpu.push_rect([x0, y], [x1 - x0, rh], row_bg);
            }
            self.hits.push(([x0, y, x1 - x0, rh], Hit::Row(i)));
            // The graph.
            let gr = h.graph(i);
            let lx = |lane: u8| graph_x + (f(usize::from(lane)) + 0.5) * lane_w;
            let cx = graph_x + (f(gr.lane.min(MAX_LANES - 1)) + 0.5) * lane_w;
            let (ty, my, by) = (y, y + rh / 2.0, y + rh);
            for &l in gr.pass {
                gpu.push_line(
                    [lx(l), ty],
                    [lx(l), by],
                    stroke,
                    lane_color(ui, usize::from(l)),
                );
            }
            for &l in gr.ins {
                let x = lx(l);
                let mid = f32::midpoint(ty, my);
                gpu.push_curve(
                    [[x, ty], [x, mid], [cx, mid], [cx, my]],
                    stroke,
                    lane_color(ui, usize::from(l)),
                );
            }
            for &l in gr.outs {
                let x = lx(l);
                let mid = f32::midpoint(my, by);
                gpu.push_curve(
                    [[cx, my], [cx, mid], [x, mid], [x, by]],
                    stroke,
                    lane_color(ui, usize::from(l)),
                );
            }
            let lane_c = if gr.lane >= MAX_LANES {
                ui.gutter
            } else {
                lane_color(ui, gr.lane)
            };
            gpu.push_dot([cx, my], dot, lane_c);
            let is_head = head.as_deref().is_some_and(|hd| h.oid(i) == hd);
            let merge = h.parents(i).len() > 1;
            if is_head || merge {
                // A ring: the dot's middle in the row's own colour.
                gpu.push_dot([cx, my], (dot - 2.0 * stroke).max(1.0), row_bg);
                if is_head {
                    gpu.push_dot([cx, my], (dot * 0.35).max(1.0), lane_c);
                }
            }
            // Ref labels, then the subject.
            let ty = (y + text_dy).round();
            let mut x = graph_x + graph_w + g.cell;
            let text_right = x1 - g.margin - date_w - author_w;
            let oid = h.oid(i);
            if let Some(labels) = self.labels.get(&oid) {
                for &r in labels {
                    let Some(rf) = self.refs.get(r) else { continue };
                    let w = base.ui.width(&rf.name);
                    if x + w + g.cell > text_right {
                        break;
                    }
                    let current = rf.kind == RefKind::Branch
                        && self.status.as_ref().and_then(|s| s.branch.as_deref()) == Some(&rf.name);
                    let bg = match rf.kind {
                        RefKind::Tag => fade(ui.graph_5, 0.28),
                        RefKind::Remote => fade(ui.graph_2, 0.25),
                        _ if current => fade(ui.tab_accent, 0.4),
                        _ => fade(ui.graph_3, 0.25),
                    };
                    let pad = (g.cell * 0.5).round();
                    gpu.push_rect([x, y + rh * 0.12], [w + 2.0 * pad, rh * 0.76], bg);
                    gpu.push_ui_text([x + pad, ty], &rf.name, ui.text);
                    x += w + 2.0 * pad + g.cell * 0.5;
                }
            }
            let subject = h.subject(i);
            let end = base.ui.fit(subject, (text_right - x).max(0.0));
            gpu.push_ui_text([x, ty], &subject[..end], ui.text);
            if author_w > 0.0 {
                let a = h.author(i);
                let end = base.ui.fit(a, author_w - g.cell);
                gpu.push_ui_text(
                    [x1 - g.margin - date_w - author_w + g.cell, ty],
                    &a[..end],
                    ui.gutter,
                );
            }
            crate::git::ago(&mut self.scratch, h.time(i), self.now);
            let w = base.ui.width(&self.scratch);
            gpu.push_ui_text([(x1 - g.margin - w).round(), ty], &self.scratch, ui.gutter);
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one line list per selection kind, read top to bottom"
    )]
    #[expect(
        clippy::many_single_char_names,
        reason = "g is the frame geometry and f the float cast, as everywhere in this file"
    )]
    /// Lay out the details' lines for a pane `width` wide.
    fn build_info(&mut self, ui: &Ui, base: &Layout, width: f32) {
        self.info.clear();
        self.info_width = width;
        let wrap = |text: &str, out: &mut Vec<InfoLine>, color: [f32; 4]| {
            for para in text.lines() {
                let mut rest = para;
                if rest.is_empty() {
                    out.push(InfoLine::Blank);
                }
                while !rest.is_empty() {
                    let mut end = base.ui.fit(rest, width).max(1);
                    while !rest.is_char_boundary(end) {
                        end += 1;
                    }
                    if end < rest.len()
                        && let Some(space) = rest.get(..end).and_then(|s| s.rfind(' '))
                        && space > 0
                    {
                        end = space + 1;
                    }
                    out.push(InfoLine::Text(rest[..end].trim_end().to_owned(), color));
                    rest = &rest[end..];
                }
            }
        };
        match (&self.selected, &self.commit) {
            (Selected::Changes, _) => {
                let s = self.status.as_ref();
                let title = match s.and_then(|s| s.branch.as_deref()) {
                    Some(b) => format!("Changes on {b}"),
                    None => "Changes (detached HEAD)".to_owned(),
                };
                self.info.push(InfoLine::Text(title, ui.text));
                if self.files.is_empty() {
                    self.info.push(InfoLine::Text(
                        "Nothing to commit: the working tree matches HEAD".to_owned(),
                        ui.gutter,
                    ));
                }
                let staged = self.files.iter().filter(|f| f.staged == Some(true)).count();
                let mut heading_done = [false, false];
                for (i, file) in self.files.iter().enumerate() {
                    let k = usize::from(file.staged != Some(true));
                    if !heading_done[k] {
                        heading_done[k] = true;
                        self.info.push(InfoLine::Blank);
                        let label = if k == 0 {
                            format!("Staged ({staged})")
                        } else {
                            format!("Not staged ({})", self.files.len() - staged)
                        };
                        self.info.push(InfoLine::Heading(k == 0, label));
                    }
                    self.info.push(InfoLine::File(i));
                }
            }
            (Selected::Commit(oid), None) => {
                self.info.push(InfoLine::Field("Commit", oid.clone()));
                self.info
                    .push(InfoLine::Text("Reading…".to_owned(), ui.gutter));
            }
            (Selected::Commit(oid), Some(Err(e))) => {
                self.info.push(InfoLine::Field("Commit", oid.clone()));
                self.info
                    .push(InfoLine::Text(format!("Cannot read it: {e}"), ui.error));
            }
            (Selected::Commit(oid), Some(Ok(c))) => {
                let mut lines = c.message.lines();
                let subject = lines.next().unwrap_or("").to_owned();
                let body: Vec<&str> = lines.skip_while(|l| l.trim().is_empty()).collect();
                wrap(&subject, &mut self.info, ui.text);
                self.info.push(InfoLine::Blank);
                self.info.push(InfoLine::Field("Commit", oid.clone()));
                let a = &c.author;
                self.info.push(InfoLine::Field(
                    "Author",
                    format!("{} <{}>", a.name, a.email),
                ));
                self.info
                    .push(InfoLine::Field("Date", crate::git::date(a.time, a.offset)));
                let k = &c.committer;
                if (k.name.as_str(), k.email.as_str()) != (a.name.as_str(), a.email.as_str()) {
                    self.info.push(InfoLine::Field(
                        "Committer",
                        format!("{} <{}>", k.name, k.email),
                    ));
                }
                if k.time != a.time {
                    self.info.push(InfoLine::Field(
                        "Committed",
                        crate::git::date(k.time, k.offset),
                    ));
                }
                match c.parents.len() {
                    0 => self.info.push(InfoLine::Field(
                        "Parent",
                        "none: the first commit".to_owned(),
                    )),
                    1 => self.info.push(InfoLine::Field(
                        "Parent",
                        c.parents[0].get(..10).unwrap_or("").to_owned(),
                    )),
                    _ => self.info.push(InfoLine::Parents),
                }
                if !body.is_empty() {
                    self.info.push(InfoLine::Blank);
                    wrap(&body.join("\n"), &mut self.info, ui.current_line_number);
                }
                self.info.push(InfoLine::Blank);
                let n = self.files.len();
                let text = match &self.files_error {
                    Some(e) => format!("Cannot list the files: {e}"),
                    None => format!("{n} file{} changed", if n == 1 { "" } else { "s" }),
                };
                self.info.push(InfoLine::Text(text, ui.gutter));
                for i in 0..n {
                    self.info.push(InfoLine::File(i));
                }
            }
        }
    }

    #[expect(
        clippy::many_single_char_names,
        reason = "g is the frame geometry and f the float cast, as everywhere in this file"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per kind of line, drawn top to bottom"
    )]
    fn draw_info(&mut self, gpu: &mut Gpu, ui: &Ui, g: &Geo, base: &Layout) {
        let (x0, x1) = g.details;
        let (top, bottom) = (g.header, g.commit_top);
        let width = x1 - x0 - 2.0 * g.margin;
        if (width - self.info_width).abs() > 0.5 {
            self.build_info(ui, base, width);
        }
        gpu.begin_layer(Some([x0, top, x1 - x0, bottom - top]));
        let lh = g.lh;
        let total = f(self.info.len()) * lh + g.margin;
        self.info_scroll = self
            .info_scroll
            .clamp(0.0, (total - (bottom - top - g.margin)).max(0.0));
        let label_w = 10.0 * g.cell;
        self.info_view = (lh, bottom - top - g.margin);
        let mut y = top + g.margin - self.info_scroll;
        // Taken for the loop, which draws buttons through `self`.
        let info = std::mem::take(&mut self.info);
        for line in &info {
            if y + lh < top {
                y += lh;
                continue;
            }
            if y > bottom {
                break;
            }
            let x = x0 + g.margin;
            match line {
                InfoLine::Text(t, c) => {
                    gpu.push_ui_text([x, y], t, *c);
                }
                InfoLine::Field(label, value) => {
                    gpu.push_ui_text([x, y], label, ui.gutter);
                    let end = base.ui.fit(value, (width - label_w).max(0.0));
                    gpu.push_ui_text([x + label_w, y], &value[..end], ui.text);
                }
                InfoLine::Parents => {
                    gpu.push_ui_text([x, y], "Compare with", ui.gutter);
                    let mut px = x + label_w.max(base.ui.width("Compare with") + g.cell);
                    if let Some(Ok(c)) = &self.commit {
                        for (k, p) in c.parents.iter().enumerate() {
                            self.scratch.clear();
                            let _ = write!(
                                self.scratch,
                                "parent {} {}",
                                k + 1,
                                p.get(..8).unwrap_or("")
                            );
                            let w = base.ui.width(&self.scratch);
                            let pad = (g.cell * 0.5).round();
                            let bg = if k == self.parent {
                                ui.toggle_on
                            } else {
                                ui.field
                            };
                            gpu.push_rect([px, y], [w + 2.0 * pad, lh], bg);
                            gpu.push_ui_text([px + pad, y], &self.scratch, ui.text);
                            self.hits.push(([px, y, w + 2.0 * pad, lh], Hit::Parent(k)));
                            px += w + 2.0 * pad + g.cell;
                        }
                    }
                }
                InfoLine::File(i) => {
                    let Some(file) = self.files.get(*i) else {
                        continue;
                    };
                    let (kind, staged, partly) = (file.kind, file.staged, file.partly);
                    if self.file_sel == Some(*i) {
                        let c = if self.focus == Focus::Files {
                            ui.picker_selected
                        } else {
                            ui.selection
                        };
                        gpu.push_rect([x0, y], [x1 - x0, lh], c);
                    }
                    let mut mark = [0u8; 4];
                    let mark = kind.letter().encode_utf8(&mut mark);
                    gpu.push_ui_text([x, y], mark, kind_color(ui, kind));
                    let fx = x + 2.0 * g.cell;
                    self.hits.push(([x0, y, x1 - x0, lh], Hit::File(*i)));
                    // Changes' own buttons, right to left.
                    let buttons: &[(&str, GitCmd)] = match staged {
                        Some(true) => &[("Unstage", GitCmd::UnstageFile)],
                        Some(false) => &[
                            ("Stage", GitCmd::StageFile),
                            ("Discard", GitCmd::DiscardFile),
                        ],
                        None => &[],
                    };
                    let mut right = x1 - g.margin;
                    for &(label, cmd) in buttons {
                        right -= Self::button_w(base, label);
                        let usable = self.busy.is_none();
                        self.button(
                            gpu,
                            ui,
                            base,
                            (right, y, lh),
                            label,
                            (false, usable),
                            Hit::FileGit(*i, cmd),
                        );
                        right -= g.cell * 0.5;
                    }
                    let note = match (partly, staged) {
                        (false, _) => "",
                        (true, Some(true)) => "  part staged",
                        (true, _) => "  rest not staged",
                    };
                    let Some(file) = self.files.get(*i) else {
                        continue;
                    };
                    let note_w = base.ui.width(note);
                    let room = (right - g.cell - fx - note_w).max(0.0);
                    let end = base.ui.fit(&file.label, room);
                    let after = gpu.push_ui_text([fx, y], &file.label[..end], ui.text);
                    if !note.is_empty() {
                        gpu.push_ui_text([after, y], note, ui.gutter);
                    }
                }
                InfoLine::Heading(staged, label) => {
                    gpu.push_ui_text([x, y], label, ui.gutter);
                    let (text, cmd) = if *staged {
                        ("Unstage All", GitCmd::UnstageAll)
                    } else {
                        ("Stage All", GitCmd::StageAll)
                    };
                    let w = Self::button_w(base, text);
                    let usable = self.busy.is_none();
                    self.button(
                        gpu,
                        ui,
                        base,
                        (x1 - g.margin - w, y, lh),
                        text,
                        (false, usable),
                        Hit::Git(cmd),
                    );
                }
                InfoLine::Blank => {}
            }
            y += lh;
        }
        self.info = info;
    }

    /// The bar above the compare: the file, the options, Open in Editor;
    /// for Changes, a second row with the hunk and line buttons.
    fn draw_bar(&mut self, gpu: &mut Gpu, ui: &Ui, g: &Geo, base: &Layout) {
        let (x0, x1) = g.details;
        let (top, end) = (g.info_bottom, g.bar_bottom);
        gpu.begin_layer(Some([x0, top, x1 - x0, end - top]));
        gpu.push_rect([x0, top], [x1 - x0, end - top], ui.bar);
        gpu.push_rect([x0, top], [x1 - x0, g.line], ui.picker_border);
        let bottom = (top + g.lh + g.margin).round().min(end);
        let y = (top + (bottom - top - g.lh) / 2.0).round();
        let pad = (g.cell * 0.5).round();
        // Right to left: Open in Editor, then the toggles.
        let ws = match self.normalize.whitespace {
            nimbus_core::Whitespace::Exact => "Spaces: count",
            nimbus_core::Whitespace::Ends => "Spaces: ends",
            nimbus_core::Whitespace::All => "Spaces: ignore",
        };
        let unified =
            matches!(&self.diff, Some(DiffPane::Compare { compare, .. }) if compare.unified);
        let buttons: [(&str, bool, Hit); 5] = [
            ("Open in Editor", false, Hit::OpenInEditor),
            ("Unified", unified, Hit::Toggle(Toggle::Unified)),
            (
                "Blank lines",
                self.normalize.blank_lines,
                Hit::Toggle(Toggle::BlankLines),
            ),
            ("Case", self.normalize.case, Hit::Toggle(Toggle::Case)),
            (
                ws,
                self.normalize.whitespace != nimbus_core::Whitespace::Exact,
                Hit::Toggle(Toggle::Whitespace),
            ),
        ];
        let mut right = x1 - g.margin;
        for (label, on, hit) in buttons {
            let w = base.ui.width(label) + 2.0 * pad;
            let left = right - w;
            if left < x0 + g.margin {
                break;
            }
            let usable = hit != Hit::OpenInEditor || self.file_sel.is_some();
            let bg = if on { ui.toggle_on } else { ui.field };
            gpu.push_rect(
                [left, top + g.margin / 2.0],
                [w, bottom - top - g.margin],
                bg,
            );
            let color = if usable { ui.text } else { ui.gutter };
            gpu.push_ui_text([left + pad, y], label, color);
            self.hits.push(([left, top, w, bottom - top], hit));
            right = left - g.cell * 0.5;
        }
        if let Some(file) = self.file_sel.and_then(|i| self.files.get(i)) {
            let end = base
                .ui
                .fit(&file.label, (right - x0 - 2.0 * g.margin).max(0.0));
            gpu.push_ui_text([x0 + g.margin, y], &file.label[..end], ui.text);
        }
        if g.bar_rows > 1 {
            self.draw_part_buttons(gpu, ui, g, base, (bottom, end));
        }
    }

    /// The bar's second row under Changes: which side the compare shows,
    /// then Stage or Unstage, and Discard, for the hunk and the picked
    /// lines.
    fn draw_part_buttons(
        &mut self,
        gpu: &mut Gpu,
        ui: &Ui,
        g: &Geo,
        base: &Layout,
        (top, bottom): (f32, f32),
    ) {
        let (x0, x1) = g.details;
        let Some(staged) = self
            .file_sel
            .and_then(|i| self.files.get(i))
            .and_then(|f| f.staged)
        else {
            return;
        };
        let y = (top + g.margin / 4.0).round();
        let h = (bottom - top - g.margin / 2.0).max(1.0);
        let label = if staged {
            "Staged: HEAD → index"
        } else {
            "Not staged: index → working tree"
        };
        let ty = (top + (bottom - top - g.lh) / 2.0).round();
        let compare = matches!(self.diff, Some(DiffPane::Compare { .. }));
        let picked = self.picked.is_some();
        let free = self.busy.is_none();
        let buttons: &[(&str, GitCmd, bool)] = if staged {
            &[
                ("Unstage Hunk", GitCmd::UnstageHunk, compare),
                ("Unstage Lines", GitCmd::UnstageLines, compare && picked),
            ]
        } else {
            &[
                ("Stage Hunk", GitCmd::StageHunk, compare),
                ("Stage Lines", GitCmd::StageLines, compare && picked),
                ("Discard Hunk", GitCmd::DiscardHunk, compare),
                ("Discard Lines", GitCmd::DiscardLines, compare && picked),
            ]
        };
        let total: f32 = buttons
            .iter()
            .map(|(l, ..)| Self::button_w(base, l) + g.cell * 0.5)
            .sum();
        let mut x = (x1 - g.margin - total).max(x0 + g.margin);
        let room = (x - g.cell - x0 - g.margin).max(0.0);
        let end = base.ui.fit(label, room);
        gpu.push_ui_text([x0 + g.margin, ty], &label[..end], ui.gutter);
        for &(text, cmd, usable) in buttons {
            x = self.button(
                gpu,
                ui,
                base,
                (x, y, h),
                text,
                (false, usable && free),
                Hit::Git(cmd),
            );
            x += g.cell * 0.5;
        }
    }

    /// The compare, or its message; returns the status bar's text.
    fn draw_compare(&mut self, gpu: &mut Gpu, theme: &mut Theme, g: &Geo, scale: f32) -> String {
        let (x0, x1) = g.details;
        let (top, bottom) = (g.bar_bottom, g.bottom);
        let ui = theme.ui;
        gpu.begin_layer(Some([x0, top, x1 - x0, bottom - top]));
        match &mut self.diff {
            None => {
                self.compare_layout = None;
                String::new()
            }
            Some(DiffPane::Message(m)) => {
                self.compare_layout = None;
                gpu.push_ui_text([x0 + g.margin, top + g.margin], m, ui.gutter);
                String::new()
            }
            Some(DiffPane::Compare {
                doc,
                carets,
                view,
                compare,
                ..
            }) => {
                let lines = doc.buffer.len_lines().max(compare.other.buffer.len_lines());
                let mut layout = Self::base_layout(gpu, scale, lines).column(x0, x1);
                layout.top = top + g.margin / 2.0;
                layout.bottom = bottom;
                self.compare_layout = Some(layout);
                theme.resolve_tables();
                compare.draw(
                    gpu,
                    &layout,
                    doc,
                    carets,
                    view,
                    theme,
                    4,
                    false,
                    None,
                    (&[], None),
                );
                // The picked rows (M28): a tint and a bar at the left
                // edge, over the text so the compare's own colors show.
                if let Some((a, b)) = self.picked
                    && !compare.unified
                {
                    let lh = layout.metrics.line_height;
                    let (lo, hi) = (a.min(b), a.max(b));
                    let y0 = compare.row_y(lo, &layout).max(top);
                    let y1 = (compare.row_y(hi, &layout) + lh).min(bottom);
                    if y1 > y0 {
                        gpu.begin_layer(Some([x0, top, x1 - x0, bottom - top]));
                        gpu.push_rect([x0, y0], [x1 - x0, y1 - y0], fade(ui.selection, 0.35));
                        gpu.push_rect([x0, y0], [3.0 * g.line, y1 - y0], ui.tab_accent);
                    }
                }
                if !compare.unified {
                    self.hits
                        .push(([x0, layout.top, x1 - x0, bottom - layout.top], Hit::Compare));
                }
                compare.status("").to_owned()
            }
        }
    }

    fn draw_dividers(&mut self, gpu: &mut Gpu, ui: &Ui, g: &Geo) {
        gpu.begin_layer(None);
        let grab = (3.0 * g.line).max(3.0);
        for (k, x) in [(0, g.side.1), (1, g.history.1)] {
            gpu.push_rect(
                [x - g.line, g.header],
                [g.line, g.bottom - g.header],
                ui.picker_border,
            );
            self.hits.push((
                [x - grab, g.header, 2.0 * grab, g.bottom - g.header],
                Hit::Divider(k),
            ));
        }
        let (x0, x1) = g.details;
        self.hits.push((
            [x0, g.info_bottom - grab, x1 - x0, 2.0 * grab],
            Hit::Divider(2),
        ));
    }

    // ---- Input --------------------------------------------------------

    /// What is under `(x, y)` in the last frame, the last-drawn first.
    #[must_use]
    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits
            .iter()
            .rev()
            .find(|([hx, hy, w, h], _)| x >= *hx && x < hx + w && y >= *hy && y < hy + h)
            .map(|(_, hit)| *hit)
    }

    /// A click at `(x, y)`; `double` for the second of a double click,
    /// `shift` to extend a pick of compare rows.
    pub fn click(&mut self, x: f32, y: f32, double: bool, shift: bool) -> Outcome {
        if let Some(p) = &self.prompt {
            // A prompt takes every click; one outside it does nothing.
            if let Some(layout) = &self.window_layout
                && let Some(answer) = p.click(layout, x, y)
            {
                self.answered(answer);
            }
            return Outcome::Done(true);
        }
        let Some(hit) = self.hit(x, y) else {
            return Outcome::Done(false);
        };
        if hit != Hit::Message && self.focus == Focus::Message {
            self.keep_draft_now();
        }
        match hit {
            Hit::Side(i) => {
                self.focus = Focus::Sidebar;
                self.side_click(i);
            }
            Hit::Row(i) => {
                self.focus = Focus::History;
                self.select_row(i, false);
            }
            Hit::File(i) => {
                self.focus = Focus::Files;
                self.select_file(i);
                if double && let Some(p) = self.selected_path() {
                    return Outcome::OpenInEditor(p);
                }
            }
            Hit::Parent(k) => self.choose_parent(k),
            Hit::Toggle(t) => self.toggle(t),
            Hit::OpenInEditor => {
                if let Some(p) = self.selected_path() {
                    return Outcome::OpenInEditor(p);
                }
            }
            Hit::Divider(k) => self.dragging = Some(k),
            Hit::Git(cmd) => {
                self.git_command(cmd);
            }
            Hit::FileGit(i, cmd) => {
                self.focus = Focus::Files;
                self.file_button(i, cmd);
            }
            Hit::Message => {
                self.focus = Focus::Message;
                let tab = self.tab_width;
                self.message.click(x, y, shift, tab);
                self.message_drag = true;
            }
            Hit::Compare => {
                self.focus = Focus::Diff;
                self.picking = true;
                self.pick_at(x, y, shift);
            }
            Hit::StopCommit => self.stop_commit(),
        }
        Outcome::Done(true)
    }

    /// Move a divider being dragged to `(x, y)`.
    pub fn drag(&mut self, gpu: &Gpu, scale: f32, x: f32, y: f32) -> bool {
        if self.picking {
            self.pick_at(x, y, true);
            return true;
        }
        if self.message_drag {
            let tab = self.tab_width;
            self.message.click(x, y, true, tab);
            return true;
        }
        let Some(k) = self.dragging else { return false };
        let g = self.geo(gpu, scale);
        match k {
            0 => self.side_cells = (x / g.cell).max(10.0),
            1 => {
                let rest = (g.w - g.side.1).max(1.0);
                self.details_share = ((g.w - x) / rest).clamp(0.15, 0.85);
            }
            _ => {
                let body = (g.bottom - g.header).max(1.0);
                self.info_share = ((y - g.header) / body).clamp(0.1, 0.85);
            }
        }
        self.info_width = 0.0;
        true
    }

    /// The mouse wheel turned by `dy` pixels (positive scrolls down) over
    /// `(x, y)`.
    pub fn wheel(&mut self, gpu: &Gpu, scale: f32, (x, y): (f32, f32), dy: f32) -> bool {
        let g = self.geo(gpu, scale);
        if x < g.side.1 {
            self.side_scroll += dy;
        } else if x < g.history.1 {
            self.scroll = (self.scroll + dy / g.row_h).max(0.0);
        } else if y >= g.commit_top && y < g.info_bottom {
            self.message.scroll_by(dy, self.tab_width);
        } else if y < g.info_bottom {
            self.info_scroll += dy;
        } else if let (Some(DiffPane::Compare { compare, .. }), Some(layout)) =
            (&mut self.diff, &self.compare_layout)
        {
            compare.scroll_by(dy, layout);
        }
        true
    }

    /// Whether `(x, y)` is on a divider, for the resize cursor.
    #[must_use]
    pub fn on_divider(&self, x: f32, y: f32) -> Option<usize> {
        match self.hit(x, y) {
            Some(Hit::Divider(k)) => Some(k),
            _ => None,
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per editor command the window understands"
    )]
    /// An editor command, as the user's key bindings produced it;
    /// `paste` is the clipboard's text for Paste.
    pub fn command(&mut self, cmd: Command<'_>, paste: Option<&str>) -> Outcome {
        if self.prompt.is_some() {
            return self.prompt_key(cmd);
        }
        if let Command::Git(g) = cmd {
            if g != GitCmd::FocusMessage && self.focus == Focus::Message {
                self.keep_draft_now();
            }
            return Outcome::Done(self.git_command(g));
        }
        if self.focus == Focus::Message
            && let Some(out) = self.message_command(cmd, paste)
        {
            return out;
        }
        if cmd == Command::Escape && self.committing() {
            self.stop_commit();
            return Outcome::Done(true);
        }
        let page = 20;
        let step = |motion: Motion| -> Option<isize> {
            match motion {
                Motion::Up => Some(-1),
                Motion::Down => Some(1),
                Motion::PageUp => Some(-page),
                Motion::PageDown => Some(page),
                Motion::DocStart => Some(isize::MIN / 2),
                Motion::DocEnd => Some(isize::MAX / 2),
                _ => None,
            }
        };
        match cmd {
            Command::Move(motion, extend) => {
                let Some(d) = step(motion) else {
                    return Outcome::Done(false);
                };
                match self.focus {
                    Focus::Diff => self.step_pick(d, extend),
                    // Handled above; nothing reaches here.
                    Focus::Message => {}
                    Focus::History => {
                        let len = self.current.as_ref().map_or(0, |c| lock(&c.history).len());
                        if len == 0 {
                            return Outcome::Done(false);
                        }
                        let from = self.selected_row.unwrap_or(whole(self.scroll));
                        let to = from.saturating_add_signed(d).min(len - 1);
                        self.select_row(to, true);
                    }
                    Focus::Files => {
                        if self.files.is_empty() {
                            return Outcome::Done(false);
                        }
                        let from = self.file_sel.unwrap_or(0);
                        let to = from.saturating_add_signed(d).min(self.files.len() - 1);
                        self.select_file(to);
                        self.reveal_file(to);
                    }
                    Focus::Sidebar => {
                        let n = self.side_items.len();
                        let at = self.side_selected().unwrap_or(0);
                        let mut to = at.saturating_add_signed(d).min(n.saturating_sub(1));
                        // Skip headings and notes in the direction moved.
                        while let Some(SideItem::Section(..) | SideItem::Note(_)) =
                            self.side_items.get(to)
                        {
                            let next = to.saturating_add_signed(d.signum());
                            if next == to || next >= n {
                                break;
                            }
                            to = next;
                        }
                        self.side_click(to);
                    }
                }
                Outcome::Done(true)
            }
            Command::Newline => match self.selected_path() {
                Some(p) => Outcome::OpenInEditor(p),
                None => Outcome::Done(false),
            },
            Command::Copy => match (&self.focus, &self.selected) {
                (Focus::Files, _) => self
                    .file_sel
                    .and_then(|i| self.files.get(i))
                    .map_or(Outcome::Done(false), |f| Outcome::Copy(f.path.clone())),
                (_, Selected::Commit(oid)) => Outcome::Copy(oid.clone()),
                _ => Outcome::Done(false),
            },
            Command::DiffWhitespace => {
                self.toggle(Toggle::Whitespace);
                Outcome::Done(true)
            }
            Command::DiffCase => {
                self.toggle(Toggle::Case);
                Outcome::Done(true)
            }
            Command::DiffBlankLines => {
                self.toggle(Toggle::BlankLines);
                Outcome::Done(true)
            }
            Command::DiffUnified => {
                self.toggle(Toggle::Unified);
                Outcome::Done(true)
            }
            Command::DiffNext | Command::DiffPrev => {
                let forward = matches!(cmd, Command::DiffNext);
                if let (
                    Some(DiffPane::Compare {
                        doc,
                        carets,
                        compare,
                        ..
                    }),
                    Some(layout),
                ) = (&mut self.diff, &self.compare_layout)
                {
                    let cursor = carets.cursor(&doc.buffer);
                    let to = if forward {
                        compare.next_change(doc, cursor, layout)
                    } else {
                        compare.prev_change(doc, cursor, layout)
                    };
                    if let Some(to) = to {
                        crate::document::Editor::new(doc, carets).click(to, false);
                    }
                }
                Outcome::Done(true)
            }
            Command::NextTab | Command::FocusSidebar => {
                self.focus = self.next_focus(true);
                Outcome::Done(true)
            }
            Command::PrevTab => {
                self.focus = self.next_focus(false);
                Outcome::Done(true)
            }
            // Outside the message box, Undo puts back what was discarded
            // last (M28).
            Command::Undo => Outcome::Done(self.git_command(GitCmd::UndoDiscard)),
            Command::Escape => {
                if self.focus == Focus::Diff && self.picked.is_some() {
                    self.picked = None;
                } else {
                    self.focus = Focus::History;
                }
                Outcome::Done(true)
            }
            Command::ZoomIn
            | Command::ZoomOut
            | Command::ResetZoom
            | Command::ToggleTheme
            | Command::Close
            | Command::CloseTab => Outcome::Window,
            _ => Outcome::Done(false),
        }
    }

    /// The pane after (or before) the focused one, in the order Tab
    /// visits them: the commit box and the compare only under Changes.
    fn next_focus(&self, forward: bool) -> Focus {
        let changes = self.selected == Selected::Changes;
        let compare = matches!(self.diff, Some(DiffPane::Compare { .. }));
        let mut order = vec![Focus::Sidebar, Focus::History, Focus::Files];
        if changes && compare {
            order.push(Focus::Diff);
        }
        if changes {
            order.push(Focus::Message);
        }
        let at = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        let n = order.len();
        let to = if forward {
            (at + 1) % n
        } else {
            (at + n - 1) % n
        };
        order[to]
    }

    /// A command while the message box has the focus. `None` when the
    /// box leaves it to the window (Tab to move on, zoom, the palette).
    fn message_command(&mut self, cmd: Command<'_>, paste: Option<&str>) -> Option<Outcome> {
        match cmd {
            Command::Escape => {
                self.keep_draft_now();
                self.focus = Focus::Files;
                Some(Outcome::Done(true))
            }
            // Tab types a tab in the editor; here it moves on, as in a
            // form, and the message has no use for indentation.
            Command::NextTab
            | Command::PrevTab
            | Command::FocusSidebar
            | Command::Tab
            | Command::Outdent
            | Command::ZoomIn
            | Command::ZoomOut
            | Command::ResetZoom
            | Command::ToggleTheme
            | Command::Close
            | Command::CloseTab
            | Command::Palette => {
                if matches!(cmd, Command::Tab) {
                    self.keep_draft_now();
                    self.focus = self.next_focus(true);
                    return Some(Outcome::Done(true));
                }
                if matches!(cmd, Command::Outdent) {
                    self.keep_draft_now();
                    self.focus = self.next_focus(false);
                    return Some(Outcome::Done(true));
                }
                None
            }
            _ => {
                let tab = self.tab_width;
                let copied = self.message.command(cmd, paste, tab);
                Some(copied.map_or(Outcome::Done(true), Outcome::Copy))
            }
        }
    }

    /// A key while a prompt is open: it answers or moves between the
    /// buttons; everything else waits.
    fn prompt_key(&mut self, cmd: Command<'_>) -> Outcome {
        let key = match cmd {
            Command::Newline | Command::Insert(" ") => prompt::Key::Press,
            Command::Escape => prompt::Key::Escape,
            Command::Tab | Command::Move(Motion::Right, _) => prompt::Key::Next,
            Command::Outdent | Command::Move(Motion::Left, _) => prompt::Key::Prev,
            Command::Insert(text) => match text.chars().next() {
                Some(c) if text.chars().count() == 1 && c.is_alphabetic() => prompt::Key::Letter(c),
                _ => return Outcome::Done(false),
            },
            _ => return Outcome::Done(false),
        };
        let answer = self.prompt.as_mut().and_then(|p| p.key(key));
        if let Some(answer) = answer {
            self.answered(answer);
        }
        Outcome::Done(true)
    }

    /// The sidebar line that is selected: Changes, or the first ref at
    /// the selected commit.
    pub(super) fn side_selected(&self) -> Option<usize> {
        self.side_items.iter().position(|item| match item {
            SideItem::Changes => self.selected == Selected::Changes,
            SideItem::Ref(r) => self
                .refs
                .get(*r)
                .is_some_and(|rf| self.selected == Selected::Commit(rf.target.clone())),
            SideItem::Stash(s) => self
                .stashes
                .get(*s)
                .is_some_and(|st| self.selected == Selected::Commit(st.oid.clone())),
            _ => false,
        })
    }

    /// Scroll the details so file `i` is on screen, by the last frame's
    /// line height and pane height.
    pub(super) fn reveal_file(&mut self, i: usize) {
        let Some(n) = self.info.iter().position(|l| *l == InfoLine::File(i)) else {
            return;
        };
        let (lh, room) = self.info_view;
        if lh <= 0.0 {
            return;
        }
        let y = f(n) * lh;
        if y < self.info_scroll {
            self.info_scroll = y;
        } else if y + 2.0 * lh > self.info_scroll + room {
            self.info_scroll = y + 2.0 * lh - room;
        }
    }

    /// The kind marks drawn, for tests.
    #[cfg(test)]
    pub fn kinds(&self) -> Vec<crate::git::Kind> {
        self.files.iter().map(|f| f.kind).collect()
    }
}

/// `n` with thin separators every three digits: 1,234,567.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

use std::fmt::Write as _;

use super::MAX_LANES;

#[cfg(test)]
mod tests {
    use super::thousands;

    #[test]
    fn thousands_are_separated() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
