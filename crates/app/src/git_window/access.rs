//! The git window's screen reader adapter, as the editor's
//! (`app/access.rs`) is for its window: created before the window is first
//! shown, inactive until a screen reader asks, and handed a closure each
//! frame that builds only what changed.

use accesskit::{Action, ActionData, ActionRequest, NodeId, TreeUpdate};
use accesskit_winit::{Adapter, WindowEvent as AccessEvent};
use nimbus_core::wrap::{WrapCache, WrapView};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::Window;

use super::GitApp;
use super::load::GitEvent;
use super::pane::access::CompareText;
use super::pane::draw::Outcome;
use crate::access::text::{Frame, Geometry, TextWindow};
use crate::access::{self, Field, Picker, Sent, git};
use crate::input::Command;
use crate::picker::Item;

/// The adapter and what was last sent through it.
#[derive(Default)]
pub struct GitAccess {
    adapter: Option<Adapter>,
    /// The next update must describe the whole tree.
    need_full: bool,
    sent: Sent,
    text: CompareText,
    /// The palette's query field, and its row starts.
    picker: Option<TextWindow>,
    picker_rows: WrapCache,
}

impl std::fmt::Debug for GitAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitAccess")
            .field("active", &self.adapter.is_some())
            .finish_non_exhaustive()
    }
}

impl GitAccess {
    /// Attach the adapter to `window`, which must not have been shown yet
    /// (`accesskit_winit` panics otherwise, and a release build aborts).
    pub fn attach(
        &mut self,
        event_loop: &ActiveEventLoop,
        window: &Window,
        proxy: EventLoopProxy<GitEvent>,
    ) {
        if window.is_visible() == Some(true) {
            log::warn!("git window already visible; screen reader support is off");
            return;
        }
        self.adapter = Some(Adapter::with_event_loop_proxy(event_loop, window, proxy));
        self.need_full = true;
    }

    /// Let the adapter see a window event first.
    pub fn window_event(&mut self, window: &Window, event: &winit::event::WindowEvent) {
        if let Some(a) = self.adapter.as_mut() {
            a.process_event(window, event);
        }
    }

    fn start_over(&mut self) {
        self.need_full = true;
        self.sent.clear();
        self.text.reset();
        if let Some(p) = self.picker.as_mut() {
            p.reset();
        }
    }
}

impl GitApp {
    /// Send the screen reader what changed since the last update; nothing
    /// is built while no reader is running.
    pub(super) fn update_access(&mut self) {
        let Some(mut adapter) = self.access.adapter.take() else {
            return;
        };
        adapter.update_if_active(|| self.access_tree());
        self.access.adapter = Some(adapter);
    }

    fn access_tree(&mut self) -> TreeUpdate {
        let full = std::mem::replace(&mut self.access.need_full, false);
        if full {
            self.access.start_over();
            self.access.need_full = false;
        }
        let mut nodes = Vec::new();
        let scale = self.scale();
        let mut scene = self.pane.access_scene(
            self.gpu.as_ref(),
            scale,
            self.config.tab_width,
            &mut self.access.text,
            &mut nodes,
        );
        scene.message = self.message.as_ref().map(|m| m.text.clone());
        scene.picker = self.access_picker(&mut nodes);
        if scene.picker.is_some() {
            scene.focus = git::id::PICKER_FIELD;
        }
        let built = {
            let text = &self.access.text;
            let picker = self.access.picker.as_ref();
            git::build(&scene, &|n| {
                if n == git::id::PICKER_FIELD {
                    picker.map_or(&[], TextWindow::children)
                } else {
                    text.runs(n)
                }
            })
        };
        self.access.sent.diff(built, &mut nodes);
        access::update(nodes, scene.focus, full)
    }

    /// The palette, when open, as the editor describes its own.
    fn access_picker(&mut self, out: &mut Vec<(NodeId, accesskit::Node)>) -> Option<Picker> {
        let (Some(p), Some(layout)) = (self.picker.as_ref(), self.pane.window_layout()) else {
            if let Some(w) = self.access.picker.as_mut() {
                w.reset();
            }
            return None;
        };
        let (rows, selected) = p.rows(&self.keymap);
        let options = rows
            .into_iter()
            .enumerate()
            .map(|(r, row)| {
                let (_, bounds) = crate::view::picker_rects(&layout, r);
                (p.scroll + r, row.text, row.detail, bounds)
            })
            .collect();
        let label = crate::app::picker_title(p.kind);
        let (field_rect, _) = crate::view::picker_rects(&layout, 0);
        let tab_width = self.config.tab_width;
        let geometry = Geometry {
            cell: layout.metrics.cell_width,
            line_height: layout.metrics.line_height,
            tab_width,
        };
        let a = &mut self.access;
        let win = a
            .picker
            .get_or_insert_with(|| TextWindow::new(git::id::PICKER_TEXT));
        let mut w = WrapView::new(&mut a.picker_rows, None, tab_width);
        let t = &p.input.doc.buffer;
        let frame = Frame {
            top: None,
            geometry,
            long_scroll: 0.0,
        };
        let placed = win.update(t, &mut w, p.input.carets.selection(t), frame, out);
        Some(Picker {
            label: label.to_owned(),
            field: Field {
                label: label.to_owned(),
                placeholder: Some(crate::app::picker_placeholder(p.kind).to_owned()),
                bounds: field_rect,
                origin: [f64::from(field_rect[0]), f64::from(field_rect[1])],
                selection: placed.selection,
                read_only: false,
            },
            options,
            selected: selected.map(|s| p.scroll + s),
            total: p.match_count(),
            status: crate::app::picker_status(p),
        })
    }

    /// A reader's request while the palette is open: choose a match, or
    /// type and move the caret in its query.
    fn access_picker_action(&mut self, req: ActionRequest, event_loop: &ActiveEventLoop) {
        match git::id::target(req.target_node) {
            git::id::Target::Option(n) if req.action == Action::Click => {
                let Some(p) = self.picker.as_mut() else {
                    return;
                };
                p.selected = n;
                let chosen = p.chosen();
                self.picker = None;
                if let Some(Item::Action(a)) = chosen {
                    self.run(a.command, event_loop);
                }
                self.redraw();
            }
            git::id::Target::Fixed(git::id::PICKER_FIELD)
            | git::id::Target::Run(git::id::PICKER_TEXT) => match (req.action, req.data) {
                (Action::ReplaceSelectedText, Some(ActionData::Value(text))) => {
                    self.run(Command::Insert(&text), event_loop);
                }
                (Action::SetTextSelection, Some(ActionData::SetTextSelection(sel))) => {
                    let Some(win) = self.access.picker.as_ref() else {
                        return;
                    };
                    let (Some(anchor), Some(head)) =
                        (win.byte_of(sel.anchor), win.byte_of(sel.focus))
                    else {
                        return;
                    };
                    if let Some(p) = self.picker.as_mut() {
                        p.input.ed().select_span(anchor, head);
                    }
                    self.redraw();
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// A request from the adapter: build the tree, act for a reader, or
    /// stop.
    pub(super) fn access_event(&mut self, event: AccessEvent, event_loop: &ActiveEventLoop) {
        match event {
            AccessEvent::InitialTreeRequested => {
                self.access.start_over();
                self.update_access();
            }
            AccessEvent::AccessibilityDeactivated => self.access.start_over(),
            AccessEvent::ActionRequested(req) => {
                log::debug!(
                    "screen reader asks: {:?} on {:?}",
                    req.action,
                    req.target_node
                );
                if self.picker.is_some() {
                    self.access_picker_action(req, event_loop);
                    return;
                }
                let outcome = self.pane.access_action(&req, &self.access.text);
                self.after_pane();
                match outcome {
                    Outcome::OpenInEditor(p) => self.open_in_editor(p),
                    Outcome::Copy(t) => self.copy(t),
                    Outcome::Done(true) => self.redraw(),
                    Outcome::Done(false) | Outcome::Window => {}
                }
            }
        }
    }
}
