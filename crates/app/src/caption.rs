//! The Windows title bar Nimbus draws: the window's title and its
//! minimise, maximise and close buttons, in the same strip as the drawn
//! menu bar (`menu::drawn`), all in the theme's colours.
//!
//! On Windows the window is created without the system's frame
//! (`with_decorations(false)`), so the whole window is Nimbus's to paint.
//! It keeps the system's shadow, rounded corners and snapping, because the
//! window still has the styles that ask for them; it just has no visible
//! caption. Everything the caption did by itself then has to be done here:
//!
//! - **Moving.** A press on an empty part of the bar hands the drag to the
//!   system (`Window::drag_window`), which moves the window and snaps it
//!   to the screen's edges and corners as usual.
//! - **Resizing.** The system no longer has a border to grab, so a strip
//!   along each edge, inside the window, does the same
//!   (`Window::drag_resize_window`), with the matching arrow cursor.
//! - **The buttons.** Drawn here, they act when the button comes up over
//!   the one it went down on, as the system's do.
//! - **Double-click** on the bar maximises or restores, and a right-click
//!   opens the system's window menu (Restore, Move, Size, Minimise...).
//!
//! The hit-testing is plain geometry over the layout, so it is tested on
//! every system even though only Windows uses it.

use std::time::{Duration, Instant};

use nimbus_render::Gpu;
use winit::window::ResizeDirection;

use crate::theme::Ui;
use crate::view::Layout;

/// Whether Nimbus draws the window's title bar and buttons itself: on
/// Windows. macOS keeps its own buttons over the bar Nimbus paints
/// (`view::draw_title_bar`), and Linux leaves the frame to the window
/// manager.
pub const DRAWS_CAPTION: bool = cfg!(target_os = "windows");

/// The bar's height in points: Windows 11's own caption height.
const CAPTION_PT: f32 = 32.0;
/// Each button's width in points, as Windows draws them.
const BUTTON_PT: f32 = 46.0;
/// The strip along each edge that resizes the window, in points.
const BORDER_PT: f32 = 6.0;
/// How far along an edge from a corner still resizes from the corner.
const CORNER_PT: f32 = 16.0;
/// The size of the drawn button symbols, in points.
const GLYPH_PT: f32 = 10.0;
/// Two presses on the bar this close together maximise or restore.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);
/// The close button's hover colour: Windows' own red.
const CLOSE_RED: [f32; 4] = [0.769, 0.169, 0.110, 1.0];

/// One of the window buttons at the right of the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    /// Minimise to the taskbar.
    Minimize,
    /// Maximise, or restore when maximised.
    Maximize,
    /// Close the window (asking about unsaved changes first).
    Close,
}

/// What a point in the window is, as far as the frame is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// An edge or corner that resizes the window.
    Resize(ResizeDirection),
    /// A window button.
    Button(Button),
    /// An empty part of the bar, which moves the window.
    Caption,
    /// Anything else: the app's own.
    None,
}

/// The window's state that changes the frame: a maximised window has no
/// edges to resize, and a full-screen one has no buttons and does not
/// move.
#[derive(Debug, Clone, Copy, Default)]
pub struct Frame {
    /// The window fills the screen's work area.
    pub maximized: bool,
    /// The window is full screen.
    pub fullscreen: bool,
}

/// The buttons' state between frames.
#[derive(Debug, Default)]
pub struct Caption {
    /// The button under the pointer.
    pub hover: Option<Button>,
    /// The button the left button went down on, until it comes up.
    pub pressed: Option<Button>,
    /// When the bar was last pressed, to tell a double-click.
    last_press: Option<Instant>,
}

impl Caption {
    /// A press on the bar at `now`: whether it is the second of a
    /// double-click. A double-click is not also the start of the next one.
    pub fn double_click(&mut self, now: Instant) -> bool {
        let double = self
            .last_press
            .is_some_and(|t| now.saturating_duration_since(t) < DOUBLE_CLICK);
        self.last_press = if double { None } else { Some(now) };
        double
    }
}

/// The height of the bar for `line_height` at `scale`: the menu bar's
/// height, and never less than Windows' own caption.
#[must_use]
pub fn bar_height(line_height: f32, scale: f32) -> f32 {
    crate::menu::drawn::bar_height(line_height).max((CAPTION_PT * scale).round())
}

/// Each button's left edge and width, from the left: minimise, maximise,
/// close, against the window's right edge.
fn buttons(layout: &Layout) -> [(Button, f32, f32); 3] {
    let w = (BUTTON_PT * layout.scale).round();
    let right = layout.width;
    [
        (Button::Minimize, right - 3.0 * w, w),
        (Button::Maximize, right - 2.0 * w, w),
        (Button::Close, right - w, w),
    ]
}

/// What (`x`, `y`) is, in layout pixels (the bar, `inset` high, is at
/// negative y). `titles_end` is where the menu bar's titles end: those are
/// the menus', not the caption's.
#[must_use]
pub fn hit(layout: &Layout, (x, y): (f32, f32), inset: f32, frame: Frame, titles_end: f32) -> Hit {
    let top = -inset;
    if !frame.maximized && !frame.fullscreen {
        let border = BORDER_PT * layout.scale;
        let corner = CORNER_PT * layout.scale;
        let (w, bottom) = (layout.width, layout.height);
        let (left, right) = (x < border, x >= w - border);
        let (upper, lower) = (y < top + border, y >= bottom - border);
        let (near_left, near_right) = (x < corner, x >= w - corner);
        let (near_top, near_bottom) = (y < top + corner, y >= bottom - corner);
        let direction = if upper || lower {
            let (west, east, side) = if upper {
                (
                    ResizeDirection::NorthWest,
                    ResizeDirection::NorthEast,
                    ResizeDirection::North,
                )
            } else {
                (
                    ResizeDirection::SouthWest,
                    ResizeDirection::SouthEast,
                    ResizeDirection::South,
                )
            };
            Some(if near_left {
                west
            } else if near_right {
                east
            } else {
                side
            })
        } else if left || right {
            let (north, south, side) = if left {
                (
                    ResizeDirection::NorthWest,
                    ResizeDirection::SouthWest,
                    ResizeDirection::West,
                )
            } else {
                (
                    ResizeDirection::NorthEast,
                    ResizeDirection::SouthEast,
                    ResizeDirection::East,
                )
            };
            Some(if near_top {
                north
            } else if near_bottom {
                south
            } else {
                side
            })
        } else {
            None
        };
        if let Some(d) = direction {
            return Hit::Resize(d);
        }
    }
    if y >= 0.0 || y < top || frame.fullscreen || x < titles_end {
        return Hit::None;
    }
    for (button, left, w) in buttons(layout) {
        if x >= left && x < left + w {
            return Hit::Button(button);
        }
    }
    Hit::Caption
}

/// Draw the window's `title` and buttons over the menu bar (which paints
/// the bar's background first). The title is centred in the window where
/// it fits between the menus and the buttons, and dimmed with the window
/// in the background, as are the buttons' symbols.
pub fn draw(
    gpu: &mut Gpu,
    ui: &Ui,
    layout: &Layout,
    (title, active): (&str, bool),
    state: &Caption,
    frame: Frame,
) {
    let bar = gpu.inset_top();
    if bar <= 0.0 {
        return;
    }
    gpu.begin_layer(None);
    let color = if active { ui.text } else { ui.gutter };
    let gap = 2.0 * layout.metrics.cell_width;
    let start = crate::menu::drawn::titles_end(layout) + gap;
    let end = if frame.fullscreen {
        layout.width - layout.margin
    } else {
        buttons(layout)[0].1 - gap
    };
    let room = (end - start).max(0.0);
    // A title too long for the room is cut short with an ellipsis.
    let whole = layout.ui.fit(title, room) == title.len();
    let tail = if whole { "" } else { "…" };
    let tail_w = layout.ui.width(tail);
    let text = &title[..layout.ui.fit(title, (room - tail_w).max(0.0))];
    let text_w = layout.ui.width(text) + tail_w;
    let text_x = ((layout.width - text_w) / 2.0)
        .clamp(start, (end - text_w).max(start))
        .round();
    let text_y = (-bar + (bar - layout.metrics.line_height) / 2.0).round();
    let after = gpu.push_ui_text([text_x, text_y], text, color);
    if !whole && !text.is_empty() {
        gpu.push_ui_text([after, text_y], tail, color);
    }
    if frame.fullscreen {
        return;
    }
    let size = (GLYPH_PT * layout.scale).round();
    for (button, left, width) in buttons(layout) {
        let hovered = state.hover == Some(button);
        let pressed = hovered && state.pressed == Some(button);
        let mut glyph = color;
        if hovered {
            let fill = if button == Button::Close {
                glyph = [1.0, 1.0, 1.0, 1.0];
                let mut red = CLOSE_RED;
                if pressed {
                    red[3] = 0.85;
                }
                red
            } else {
                let mut shade = ui.text;
                shade[3] = if pressed { 0.18 } else { 0.10 };
                shade
            };
            gpu.push_rect([left, -bar], [width, bar], fill);
        }
        let corner = [
            (left + (width - size) / 2.0).round(),
            (-bar + (bar - size) / 2.0).round(),
        ];
        let symbol = match button {
            Button::Minimize => Symbol::Minimize,
            Button::Maximize if frame.maximized => Symbol::Restore,
            Button::Maximize => Symbol::Maximize,
            Button::Close => Symbol::Close,
        };
        draw_symbol(gpu, symbol, corner, (size, layout.scale), glyph);
    }
}

/// The symbols on the buttons.
#[derive(Clone, Copy)]
enum Symbol {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// Draw `symbol` in a `size`-pixel square with its top left at `corner`,
/// with lines one point thick. Only rectangles, so the compatibility
/// painter draws them the same: the close button's cross is a run of small
/// squares along each diagonal, one pixel apart, which is what a 45° line
/// is on a pixel grid.
fn draw_symbol(
    gpu: &mut Gpu,
    symbol: Symbol,
    corner: [f32; 2],
    (size, scale): (f32, f32),
    color: [f32; 4],
) {
    let [left, top] = corner;
    let thick = scale.round().max(1.0);
    let outline = |gpu: &mut Gpu, [left, top]: [f32; 2], side: f32| {
        let inner = side - 2.0 * thick;
        gpu.push_rect([left, top], [side, thick], color);
        gpu.push_rect([left, top + side - thick], [side, thick], color);
        gpu.push_rect([left, top + thick], [thick, inner], color);
        gpu.push_rect([left + side - thick, top + thick], [thick, inner], color);
    };
    match symbol {
        Symbol::Minimize => {
            let middle = (top + size / 2.0).round();
            gpu.push_rect([left, middle], [size, thick], color);
        }
        Symbol::Maximize => outline(gpu, corner, size),
        Symbol::Restore => {
            // A window behind a window: the back one's top and right
            // edges show above and beside the front one.
            let off = (2.0 * scale).round();
            let front = size - off;
            outline(gpu, [left, top + off], front);
            gpu.push_rect([left + off, top], [front, thick], color);
            gpu.push_rect([left + off + front - thick, top], [thick, front], color);
        }
        Symbol::Close => {
            let span = (size - thick).max(0.0);
            let mut step = 0.0;
            while step <= span {
                let dot = [thick, thick];
                gpu.push_rect([left + step, top + step], dot, color);
                gpu.push_rect([left + span - step, top + step], dot, color);
                step += 1.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        let metrics = nimbus_render::Metrics {
            size_px: 14.0,
            line_height: 20.0,
            baseline: 15.0,
            cell_width: 10.0,
        };
        Layout::new(metrics, 1000, 600, 1.0, 10)
    }

    const INSET: f32 = 32.0;

    fn at(x: f32, y: f32, frame: Frame) -> Hit {
        hit(&layout(), (x, y), INSET, frame, 200.0)
    }

    #[test]
    fn edges_and_corners_resize() {
        let f = Frame::default();
        assert_eq!(at(2.0, 300.0, f), Hit::Resize(ResizeDirection::West));
        assert_eq!(at(998.0, 300.0, f), Hit::Resize(ResizeDirection::East));
        assert_eq!(at(500.0, 598.0, f), Hit::Resize(ResizeDirection::South));
        assert_eq!(at(500.0, -31.0, f), Hit::Resize(ResizeDirection::North));
        assert_eq!(at(2.0, -31.0, f), Hit::Resize(ResizeDirection::NorthWest));
        // Near a corner along either edge still takes the corner.
        assert_eq!(at(10.0, 598.0, f), Hit::Resize(ResizeDirection::SouthWest));
        assert_eq!(at(998.0, -20.0, f), Hit::Resize(ResizeDirection::NorthEast));
        assert_eq!(at(500.0, 300.0, f), Hit::None);
    }

    #[test]
    fn a_maximized_window_has_no_edges() {
        let f = Frame {
            maximized: true,
            fullscreen: false,
        };
        assert_eq!(at(2.0, 300.0, f), Hit::None);
        assert_eq!(at(998.0, -20.0, f), Hit::Button(Button::Close));
        assert_eq!(at(500.0, -31.0, f), Hit::Caption);
    }

    #[test]
    fn the_bar_has_buttons_a_caption_and_menus() {
        let f = Frame::default();
        assert_eq!(at(980.0, -16.0, f), Hit::Button(Button::Close));
        assert_eq!(at(940.0, -16.0, f), Hit::Button(Button::Maximize));
        assert_eq!(at(880.0, -16.0, f), Hit::Button(Button::Minimize));
        assert_eq!(at(500.0, -16.0, f), Hit::Caption);
        // The menu titles are the menus'.
        assert_eq!(at(100.0, -16.0, f), Hit::None);
    }

    #[test]
    fn full_screen_has_no_buttons_and_does_not_move() {
        let f = Frame {
            maximized: false,
            fullscreen: true,
        };
        assert_eq!(at(980.0, -16.0, f), Hit::None);
        assert_eq!(at(500.0, -16.0, f), Hit::None);
    }

    #[test]
    fn a_double_click_is_two_presses_close_together() {
        let mut c = Caption::default();
        let t = Instant::now();
        assert!(!c.double_click(t));
        assert!(c.double_click(t + Duration::from_millis(200)));
        // The third press starts over.
        assert!(!c.double_click(t + Duration::from_millis(300)));
        assert!(!c.double_click(t + Duration::from_secs(2)));
    }

    #[test]
    fn the_bar_is_never_shorter_than_windows_caption() {
        assert!((bar_height(14.0, 1.0) - 32.0).abs() < f32::EPSILON);
        assert!((bar_height(14.0, 2.0) - 64.0).abs() < f32::EPSILON);
        assert!((bar_height(30.0, 1.0) - 45.0).abs() < f32::EPSILON);
    }
}
