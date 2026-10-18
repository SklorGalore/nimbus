//! The window's size and place, remembered between runs (M12).
//!
//! The size is saved in logical pixels (points on macOS), so a window
//! comes back the same size on a display with another scale factor. The
//! position is saved in logical pixels too, with the name of the display
//! it was on. It is only used again when a display of that name is still
//! there and the position is on it; otherwise the system places the
//! window, as it would a new one, so it never opens somewhere invisible.

use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowAttributes};

use super::App;
use crate::state::WindowGeometry;

/// Smallest remembered size used again, so a window cannot come back too
/// small to find.
const MIN_SIZE: (f64, f64) = (320.0, 200.0);

/// A display as winit describes it: name, top-left corner and size in
/// physical pixels, and scale factor.
struct Display {
    name: Option<String>,
    origin: (f64, f64),
    size: (f64, f64),
    scale: f64,
}

/// Where a window saved as `g` goes on these displays, in physical
/// pixels: on the display of the same name, if the saved corner is on it.
fn placement(g: &WindowGeometry, displays: &[Display]) -> Option<PhysicalPosition<f64>> {
    let name = g.monitor.as_deref()?;
    let d = displays.iter().find(|d| d.name.as_deref() == Some(name))?;
    let (x, y) = (g.x * d.scale, g.y * d.scale);
    let inside = x >= d.origin.0
        && y >= d.origin.1
        && x < d.origin.0 + d.size.0
        && y < d.origin.1 + d.size.1;
    inside.then_some(PhysicalPosition::new(x, y))
}

impl App {
    /// The new window's attributes with the remembered size, place and
    /// maximized state, when there are any (not in unattended runs).
    pub(super) fn restore_geometry(
        &self,
        event_loop: &ActiveEventLoop,
        attrs: WindowAttributes,
    ) -> WindowAttributes {
        let Some(g) = self
            .ws
            .state
            .window
            .as_ref()
            .filter(|_| !self.options.unattended())
        else {
            return attrs;
        };
        let mut attrs = attrs
            .with_inner_size(LogicalSize::new(
                g.width.max(MIN_SIZE.0),
                g.height.max(MIN_SIZE.1),
            ))
            .with_maximized(g.maximized);
        let displays: Vec<Display> = event_loop
            .available_monitors()
            .map(|m| Display {
                name: m.name(),
                origin: (f64::from(m.position().x), f64::from(m.position().y)),
                size: (f64::from(m.size().width), f64::from(m.size().height)),
                scale: m.scale_factor(),
            })
            .collect();
        if let Some(at) = placement(g, &displays) {
            attrs = attrs.with_position(at);
        }
        attrs
    }

    /// The window's size and place now, to save. A maximized window keeps
    /// the size it had before, so un-maximizing after a restart gives it
    /// back.
    pub(super) fn window_geometry(&self, window: &Window) -> WindowGeometry {
        let maximized = window.is_maximized();
        let previous = self.ws.state.window.as_ref();
        let (width, height) = match previous {
            Some(p) if maximized => (p.width, p.height),
            _ => {
                let size = window.inner_size().to_logical::<f64>(window.scale_factor());
                (size.width, size.height)
            }
        };
        let monitor = window.current_monitor();
        let scale = monitor.as_ref().map_or(
            window.scale_factor(),
            winit::monitor::MonitorHandle::scale_factor,
        );
        // Wayland does not tell a window where it is; then only the size
        // is kept.
        let (x, y, monitor) = match window.outer_position() {
            Ok(p) if !maximized => (
                f64::from(p.x) / scale,
                f64::from(p.y) / scale,
                monitor.and_then(|m| m.name()),
            ),
            _ => previous.map_or((0.0, 0.0, None), |p| (p.x, p.y, p.monitor.clone())),
        };
        WindowGeometry {
            width,
            height,
            x,
            y,
            monitor,
            maximized,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(name: &str, origin: (f64, f64), scale: f64) -> Display {
        Display {
            name: Some(name.to_owned()),
            origin,
            size: (2000.0, 1000.0),
            scale,
        }
    }

    fn saved(x: f64, y: f64, monitor: &str) -> WindowGeometry {
        WindowGeometry {
            width: 800.0,
            height: 600.0,
            x,
            y,
            monitor: Some(monitor.to_owned()),
            maximized: false,
        }
    }

    #[test]
    fn a_position_is_used_only_on_its_display() {
        let displays = [
            display("Built-in", (0.0, 0.0), 2.0),
            display("Studio", (2000.0, 0.0), 1.0),
        ];
        let at = placement(&saved(100.0, 50.0, "Built-in"), &displays).unwrap();
        assert_eq!((at.x, at.y), (200.0, 100.0), "logical times the scale");
        let at = placement(&saved(2100.0, 10.0, "Studio"), &displays).unwrap();
        assert_eq!((at.x, at.y), (2100.0, 10.0));
        assert!(
            placement(&saved(100.0, 50.0, "Projector"), &displays).is_none(),
            "that display is gone"
        );
        assert!(
            placement(&saved(100.0, 50.0, "Studio"), &displays).is_none(),
            "off the display it names"
        );
        let mut no_name = saved(100.0, 50.0, "Built-in");
        no_name.monitor = None;
        assert!(placement(&no_name, &displays).is_none());
    }
}
