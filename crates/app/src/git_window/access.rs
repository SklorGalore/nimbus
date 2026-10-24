//! The git window's screen reader adapter, as the editor's
//! (`app/access.rs`) is for its window: created before the window is first
//! shown, inactive until a screen reader asks, and handed a closure each
//! frame that builds only what changed.

use accesskit::TreeUpdate;
use accesskit_winit::{Adapter, WindowEvent as AccessEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::Window;

use super::GitApp;
use super::load::GitEvent;
use super::pane::access::CompareText;
use super::pane::draw::Outcome;
use crate::access::{self, Sent, git};

/// The adapter and what was last sent through it.
#[derive(Default)]
pub struct GitAccess {
    adapter: Option<Adapter>,
    /// The next update must describe the whole tree.
    need_full: bool,
    sent: Sent,
    text: CompareText,
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
        let built = {
            let text = &self.access.text;
            git::build(&scene, &|n| {
                if n == git::id::COMPARE {
                    text.runs()
                } else {
                    &[]
                }
            })
        };
        self.access.sent.diff(built, &mut nodes);
        access::update(nodes, scene.focus, full)
    }

    /// A request from the adapter: build the tree, act for a reader, or
    /// stop.
    pub(super) fn access_event(&mut self, event: AccessEvent) {
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
                match self.pane.access_action(&req, &self.access.text) {
                    Outcome::OpenInEditor(p) => self.open_in_editor(p),
                    Outcome::Copy(t) => self.copy(t),
                    Outcome::Done(true) => self.redraw(),
                    Outcome::Done(false) | Outcome::Window => {}
                }
            }
        }
    }
}
