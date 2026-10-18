//! The Markdown preview as the app runs it (M20): opening it beside its
//! source, laying it out on workers, keeping it in step with the source's
//! scrolling, and its scrolling, keys and links. The page itself is
//! [`crate::preview`]'s.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use nimbus_render::Gpu;
use nimbus_syntax::LanguageId;
use winit::event_loop::EventLoopProxy;

use super::{App, AppEvent, FindBar, Tab};
use crate::groups;
use crate::input::{Command, Motion};
use crate::preview::{Job, Laid, Params};
use crate::update;
use crate::view::Layout;

/// Lay a page out on its own thread and send it back. Layouts are rare
/// (a pause in typing, a resize), so a thread each is simplest.
fn spawn_layout(proxy: &EventLoopProxy<AppEvent>, job: Job) {
    let slot = Arc::new(Mutex::new(Some(job)));
    let theirs = Arc::clone(&slot);
    let sender = proxy.clone();
    let spawned = std::thread::Builder::new()
        .name("nimbus-preview".to_owned())
        .spawn(move || {
            if let Some(job) = theirs.lock().ok().and_then(|mut j| j.take()) {
                let _ = sender.send_event(AppEvent::Preview(job.run()));
            }
        });
    if let Err(e) = spawned {
        log::error!("could not start a preview thread ({e}); laying out on the main thread");
        if let Some(job) = slot.lock().ok().and_then(|mut j| j.take()) {
            let _ = proxy.send_event(AppEvent::Preview(job.run()));
        }
    }
}

impl App {
    /// Open Preview: show the current Markdown file as a page in a group
    /// on the right, or go to its preview if one is open. The keyboard
    /// stays in the source, so typing goes on and the page follows.
    pub(super) fn open_preview(&mut self) {
        let doc = self.tabs[self.active].doc;
        if self.tabs[self.active].preview.is_some() {
            return;
        }
        if self.docs[doc].syntax.language() != Some(LanguageId::Markdown) {
            self.say("Open Preview is for Markdown files", false);
            return;
        }
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.doc == doc && t.preview.is_some())
        {
            // Bring it to the front of its group: the keyboard stays here
            // unless it is in this group.
            let g = self.tabs[i].group;
            if g == self.groups.focused {
                self.switch_to(i);
            } else {
                self.groups.list[g].active = i;
            }
            self.changed(false);
            return;
        }
        let from = self.groups.focused;
        let at = if from + 1 < self.groups.len() {
            from + 1
        } else if self.groups.len() < groups::MAX_GROUPS {
            let at = self.groups.insert_after(from);
            for tab in &mut self.tabs {
                if tab.group >= at {
                    tab.group += 1;
                }
            }
            self.groups.list[at].find = FindBar::sharing(&self.find);
            at
        } else {
            self.say(
                format!("At most {} groups side by side", groups::MAX_GROUPS),
                false,
            );
            return;
        };
        let mut tab = Tab::new(doc, false);
        tab.group = at;
        tab.preview = Some(Box::default());
        self.tabs.push(tab);
        self.groups.list[at].active = self.tabs.len() - 1;
        self.changed(false);
    }

    /// Before a frame: start a layout for each preview on screen that is
    /// out of date, note when to look again while typing pauses, and
    /// follow each source's scrolling.
    pub(super) fn prepare_previews(&mut self, gpu: &Gpu, panes: &[Layout]) {
        let now = Instant::now();
        let mut wake: Option<Instant> = None;
        for (g, pane) in panes.iter().enumerate() {
            let i = self.group_active(g);
            if self.tabs[i].preview.is_none() {
                continue;
            }
            let doc_id = self.tabs[i].doc;
            let source_top = self.source_top(doc_id);
            let params = Params {
                width: pane.width - pane.left,
                font: gpu.preview_font(),
            };
            let doc = &self.docs[doc_id];
            let Some(p) = self.tabs[i].preview.as_deref_mut() else {
                continue;
            };
            let (job, again) = p.wants((&doc.buffer, doc.version, &doc.syntax), params, now);
            if let Some(job) = job {
                spawn_layout(&self.proxy, job);
            }
            wake = wake.into_iter().chain(again).min();
            if let Some(top) = source_top {
                p.follow(top);
            }
        }
        self.preview_at = wake;
    }

    /// The top line of the view showing `doc` as text (not a preview):
    /// the focused group's if it shows it, else the first group's that
    /// does.
    fn source_top(&self, doc: crate::docs::DocId) -> Option<usize> {
        let focused = self.groups.focused;
        let order =
            std::iter::once(focused).chain((0..self.groups.len()).filter(|&g| g != focused));
        order
            .map(|g| &self.tabs[self.group_active(g)])
            .find(|t| t.doc == doc && t.preview.is_none())
            .map(|t| t.view.scroll_top)
    }

    /// A page arrived from a worker: the preview that asked for it takes
    /// it.
    pub(super) fn preview_arrived(&mut self, laid: Laid) {
        let mut laid = Some(laid);
        for tab in &mut self.tabs {
            if let Some(p) = tab.preview.as_deref_mut()
                && p.arrived(&mut laid)
            {
                self.changed(false);
                return;
            }
        }
    }

    /// Keys in a focused preview, which shows no text to edit: the arrows
    /// and page keys scroll it, and anything else is refused.
    pub(super) fn run_preview(&mut self, cmd: Command<'_>) {
        let Some(layout) = self.layout() else {
            return;
        };
        let lh = layout.metrics.line_height;
        let height = layout.bottom - layout.top;
        let Some(p) = self.tabs[self.active].preview.as_deref_mut() else {
            return;
        };
        let dy = match cmd {
            Command::Move(Motion::Up, _) => -lh,
            Command::Move(Motion::Down, _) => lh,
            Command::Move(Motion::PageUp, _) => -(height - lh).max(lh),
            Command::Move(Motion::PageDown, _) => (height - lh).max(lh),
            Command::Move(Motion::DocStart, _) => f32::NEG_INFINITY,
            Command::Move(Motion::DocEnd, _) => f32::INFINITY,
            Command::Move(..) => 0.0,
            _ => {
                self.say("The preview is read-only; edit the file beside it", false);
                return;
            }
        };
        p.scroll_by(dy.clamp(-1e9, 1e9), height);
        self.changed(false);
    }

    /// Scroll the preview in group `i`'s active tab by `dy` pixels.
    pub(super) fn preview_wheel(&mut self, i: usize, dy: f32, layout: &Layout) -> bool {
        let Some(p) = self.tabs[i].preview.as_deref_mut() else {
            return false;
        };
        p.scroll_by(dy, layout.bottom - layout.top);
        true
    }

    /// A click in a preview: follow the link under it, if any. A web link
    /// opens in the browser; a relative one opens the file it names.
    pub(super) fn preview_click(&mut self, i: usize, layout: &Layout, x: f32, y: f32) -> bool {
        let Some(p) = self.tabs[i].preview.as_deref() else {
            return false;
        };
        let Some(url) = p.link_at(layout, x, y).map(str::to_owned) else {
            return true;
        };
        if url.starts_with("https://") || url.starts_with("http://") {
            if let Err(e) = update::open_url(&url) {
                self.say(format!("Could not open {url}: {e}"), true);
            }
            return true;
        }
        let file = url.split('#').next().unwrap_or("");
        let dir = self.docs[self.tabs[i].doc]
            .path
            .as_deref()
            .and_then(std::path::Path::parent)
            .map(std::path::Path::to_path_buf);
        match dir.map(|d| d.join(file)) {
            Some(path) if !file.is_empty() && !file.contains(':') && path.is_file() => {
                self.open_path(&path);
            }
            _ => self.say(format!("Not a link Nimbus can open: {url}"), false),
        }
        true
    }

    /// Whether the pointer is over a link in the preview under it.
    pub(super) fn over_preview_link(&self) -> bool {
        let Some(window) = self.window_layout() else {
            return false;
        };
        let (x, y) = (crate::to_f32(self.mouse.x), crate::to_f32(self.mouse.y));
        let Some(g) = crate::groups::Groups::at(&self.group_columns(&window), x) else {
            return false;
        };
        let Some(layout) = self.group_layout(g) else {
            return false;
        };
        let i = self.group_active(g);
        self.tabs[i]
            .preview
            .as_deref()
            .is_some_and(|p| y >= layout.top && p.link_at(&layout, x, y).is_some())
    }

    /// Whether tab `i` is a preview.
    pub(super) fn is_preview(&self, i: usize) -> bool {
        self.tabs.get(i).is_some_and(|t| t.preview.is_some())
    }
}
