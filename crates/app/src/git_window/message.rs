//! The commit message box (M28): a Nimbus text area under the Changes
//! list, where the message for the next commit is written.
//!
//! **It is a `Document`.** The box holds the same document, cursors and
//! view the editor's tabs use, so the editing commands the editor knows
//! (undo, several cursors, selecting lines, soft wrap, the clipboard)
//! work here as they do there; this module only routes them, as
//! `App::run_edit` does for a tab. A ruler at column 72 marks where git's
//! own tools wrap a commit message.
//!
//! **Drafts survive closing the window.** A message being written is kept
//! per repository in `commit-drafts.toml` beside `state.toml`, until a
//! commit uses it. It is a file of its own because the editor writes
//! `state.toml` whole from what it read at startup, and would drop a draft
//! a git window wrote meanwhile.

use std::path::{Path, PathBuf};

use nimbus_core::Text as _;
use nimbus_render::Gpu;
use serde::{Deserialize, Serialize};

use crate::config::Whitespace;
use crate::document::{Carets, Document, Editor};
use crate::input::{Command, Motion};
use crate::theme::{Theme, Ui};
use crate::view::{Decorations, Layout, View};

/// Where git's tools wrap a commit message, and where the ruler goes.
pub const RULER: usize = 72;

/// Drafts kept, one per repository, newest first.
const MAX_DRAFTS: usize = 50;

/// One repository's unfinished message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Draft {
    repo: PathBuf,
    message: String,
}

/// The drafts file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Drafts {
    drafts: Vec<Draft>,
}

fn drafts_path() -> Option<PathBuf> {
    // Tests never touch the real config folder.
    if cfg!(test) {
        return None;
    }
    crate::config::path().and_then(|p| p.parent().map(|d| d.join("commit-drafts.toml")))
}

fn load_drafts() -> Drafts {
    drafts_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

/// The draft kept for the repository at `top`, if any.
#[must_use]
pub fn draft(top: &Path) -> Option<String> {
    load_drafts()
        .drafts
        .into_iter()
        .find(|d| d.repo == top)
        .map(|d| d.message)
}

/// Keep `message` as the draft for `top`; an empty one removes it.
pub fn keep_draft(top: &Path, message: &str) {
    let Some(path) = drafts_path() else { return };
    if top.to_str().is_none() {
        return;
    }
    let mut all = load_drafts();
    all.drafts.retain(|d| d.repo != top);
    if !message.trim().is_empty() {
        all.drafts.insert(
            0,
            Draft {
                repo: top.to_path_buf(),
                message: message.to_owned(),
            },
        );
    }
    all.drafts.truncate(MAX_DRAFTS);
    match toml::to_string(&all) {
        Ok(text) => {
            if let Err(e) = crate::state::write_atomic(&path, text.as_bytes()) {
                log::warn!("cannot keep the commit draft: {e}");
            }
        }
        Err(e) => log::warn!("cannot keep the commit draft: {e}"),
    }
}

/// The message box.
pub struct MessageBox {
    /// The message.
    pub doc: Document,
    /// Its cursors.
    pub carets: Carets,
    /// Its view (scroll, wrap).
    pub view: View,
    /// Amend the last commit instead of making a new one.
    pub amend: bool,
    /// The message written before Amend loaded the last commit's, put
    /// back when Amend is turned off.
    before_amend: Option<String>,
    /// The layout it was drawn with, for clicks and scrolling.
    pub layout: Option<Layout>,
}

impl std::fmt::Debug for MessageBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageBox")
            .field("amend", &self.amend)
            .finish_non_exhaustive()
    }
}

impl Default for MessageBox {
    fn default() -> Self {
        Self::new("")
    }
}

impl MessageBox {
    /// A box holding `text`, soft wrap on.
    #[must_use]
    pub fn new(text: &str) -> Self {
        let mut view = View::default();
        view.set_wrap(true);
        Self {
            doc: Document::with_text(None, text),
            carets: Carets::default(),
            view,
            amend: false,
            before_amend: None,
            layout: None,
        }
    }

    /// The message as written.
    #[must_use]
    pub fn text(&self) -> String {
        let t = &self.doc.buffer;
        t.slice(0..t.len_bytes()).into_owned()
    }

    /// Whether there is nothing but whitespace.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.text().trim().is_empty()
    }

    /// Replace the whole text (as one undoable edit), cursor at the end.
    pub fn set_text(&mut self, text: &str) {
        let mut ed = Editor::new(&mut self.doc, &mut self.carets);
        ed.select_all();
        if let Err(e) = ed.insert(text) {
            log::warn!("message box: {e}");
        }
        let end = self.doc.buffer.len_bytes();
        Editor::new(&mut self.doc, &mut self.carets).click(end, false);
    }

    /// Turn Amend on, loading `last` (the last commit's message) and
    /// keeping what was written; or off, putting that back.
    pub fn set_amend(&mut self, on: bool, last: Option<&str>) {
        if on == self.amend {
            return;
        }
        self.amend = on;
        if on {
            self.before_amend = Some(self.text());
            if let Some(last) = last {
                self.set_text(last);
            }
        } else {
            let back = self.before_amend.take().unwrap_or_default();
            self.set_text(&back);
        }
    }

    /// After a commit: an empty box, Amend off.
    pub fn clear(&mut self) {
        self.amend = false;
        self.before_amend = None;
        *self = Self {
            view: std::mem::take(&mut self.view),
            ..Self::new("")
        };
    }

    /// An editing command. Returns text for the clipboard (Copy, Cut), or
    /// `None`; `paste` is the clipboard's text for Paste.
    pub fn command(
        &mut self,
        cmd: Command<'_>,
        paste: Option<&str>,
        tab_width: usize,
    ) -> Option<String> {
        let page = self.layout.map_or(4, |l| l.full_lines());
        let layout = self.layout;
        let ending = self.doc.line_ending.as_str();
        let mut ed = Editor::new(&mut self.doc, &mut self.carets);
        let result = match cmd {
            Command::Move(motion, extend) => {
                match &layout {
                    Some(l) if self.view.wrap => {
                        let mut w = self.view.wrap_view(l, tab_width);
                        ed.move_cursor_wrapped(motion, extend, &mut w, page);
                    }
                    _ => ed.move_cursor(motion, extend, tab_width, page),
                }
                Ok(())
            }
            Command::Insert(text) => ed.type_text(text, false),
            Command::Newline => ed.insert(ending),
            Command::Tab => ed.insert_indent(),
            Command::Outdent => ed.outdent_lines(),
            Command::DeleteBack => ed.backspace().map(drop),
            Command::DeleteForward => ed.delete_forward().map(drop),
            Command::DeleteWordBack => ed.delete_to(Motion::WordLeft).map(drop),
            Command::DeleteWordForward => ed.delete_to(Motion::WordRight).map(drop),
            Command::DeleteToLineStart => ed.delete_to(Motion::LineStart).map(drop),
            Command::Undo => ed.undo().map(drop),
            Command::Redo => ed.redo().map(drop),
            Command::SelectAll => {
                ed.select_all();
                Ok(())
            }
            Command::SelectLine => {
                ed.select_line();
                Ok(())
            }
            Command::AddNextOccurrence => {
                ed.add_next_occurrence();
                Ok(())
            }
            Command::AddCursorAbove => {
                ed.add_cursor_vertical(-1, tab_width);
                Ok(())
            }
            Command::AddCursorBelow => {
                ed.add_cursor_vertical(1, tab_width);
                Ok(())
            }
            Command::Escape => {
                if !ed.collapse_cursors() {
                    let head = ed.selection().head;
                    ed.click(head, false);
                }
                Ok(())
            }
            Command::Copy => return ed.selected_text(),
            Command::Cut => {
                let text = ed.selected_text();
                if text.is_some()
                    && let Err(e) = ed.backspace()
                {
                    log::warn!("message box: {e}");
                }
                return text;
            }
            Command::Paste => match paste {
                Some(text) => ed.paste(text),
                None => Ok(()),
            },
            Command::Duplicate => ed.duplicate().map(drop),
            Command::MoveLineUp => ed.move_lines(false).map(drop),
            Command::MoveLineDown => ed.move_lines(true).map(drop),
            Command::DeleteLine => ed.delete_lines().map(drop),
            Command::JoinLines => ed.join_lines().map(drop),
            Command::ChangeCase(case) => ed.change_case(case).map(drop),
            Command::TrimWhitespace => ed.trim_whitespace().map(drop),
            Command::ToggleWrap => {
                let on = !self.view.wrap;
                self.view.set_wrap(on);
                self.carets.preferred_column = None;
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(e) = result {
            log::warn!("message box: {e}");
        }
        if let Some(l) = &self.layout {
            self.view
                .ensure_cursor_visible(&self.doc, &self.carets, l, tab_width);
        }
        None
    }

    /// A click at `(x, y)`, which the caller found inside the box.
    pub fn click(&mut self, x: f32, y: f32, extend: bool, tab_width: usize) {
        let Some(layout) = self.layout else { return };
        let at = self.view.hit(x, y, &self.doc, &layout, tab_width);
        Editor::new(&mut self.doc, &mut self.carets).click(at, extend);
    }

    /// Scroll by `dy` pixels.
    pub fn scroll_by(&mut self, dy: f32, tab_width: usize) {
        if let Some(l) = &self.layout {
            self.view.scroll_by(dy, &self.doc, l, tab_width);
        }
    }

    /// Draw the box in `rect` (`[x, y, w, h]`): its background, the ruler,
    /// the text, and "Commit message" while it is empty.
    #[expect(
        clippy::too_many_arguments,
        reason = "a frame's inputs, as the git window passes them"
    )]
    pub fn draw(
        &mut self,
        gpu: &mut Gpu,
        theme: &Theme,
        base: Layout,
        rect: [f32; 4],
        focused: bool,
        tab_width: usize,
        placeholder: &str,
    ) {
        let ui: Ui = theme.ui;
        let [x, y, w, h] = rect;
        let mut layout = base.column(x, x + w);
        // No line numbers: a message is prose, and short.
        layout.gutter_cols = 0;
        layout.text_left = x + (layout.margin / 2.0).round();
        layout.top = y + (layout.margin / 2.0).round();
        layout.bottom = y + h;
        self.layout = Some(layout);
        gpu.begin_layer(Some(rect));
        gpu.push_rect([x, y], [w, h], ui.background);
        let cell = layout.metrics.cell_width;
        #[expect(clippy::cast_precision_loss, reason = "a small column count")]
        let ruler = layout.text_left + RULER as f32 * cell;
        if ruler < x + w {
            gpu.push_rect(
                [ruler.round(), y],
                [layout.scale.round().max(1.0), h],
                ui.picker_border,
            );
        }
        let deco = Decorations {
            cursor_visible: focused,
            preedit: None,
            matches: &[],
            current: None,
            ui: &ui,
            looks: None,
            whitespace: Whitespace::None,
            guides: None,
            sticky: false,
        };
        self.view
            .draw(gpu, &self.doc, &self.carets, &layout, tab_width, &deco);
        if self.doc.buffer.len_bytes() == 0 {
            gpu.push_ui_text([layout.text_left, layout.top], placeholder, ui.gutter);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amend_loads_the_last_message_and_gives_the_draft_back() {
        let mut m = MessageBox::new("half written");
        m.set_amend(true, Some("Last commit\n\nIts body"));
        assert_eq!(m.text(), "Last commit\n\nIts body");
        m.set_amend(false, None);
        assert_eq!(m.text(), "half written");
    }

    #[test]
    fn editing_commands_reach_the_document() {
        let mut m = MessageBox::new("");
        m.command(Command::Insert("Fix it"), None, 4);
        m.command(Command::Newline, None, 4);
        m.command(Command::Insert("x"), None, 4);
        assert_eq!(m.text(), "Fix it\nx");
        m.command(Command::Undo, None, 4);
        assert!(!m.text().ends_with('x'));
        m.command(Command::SelectAll, None, 4);
        assert_eq!(
            m.command(Command::Copy, None, 4).as_deref(),
            Some(m.text().as_str())
        );
        m.clear();
        assert!(m.is_blank());
    }
}
