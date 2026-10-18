//! GPU rendering for Nimbus: surface management, the bundled font, shaping,
//! the glyph atlas, and the two instanced pipelines described in
//! DESIGN.md §4.3.

pub mod atlas;
pub mod fallback;
pub mod font;
pub mod frame;
mod gpu;
pub mod packer;
mod pipeline;
pub mod raster;
pub mod shape;
mod text;
pub mod ui;

pub use fallback::{EditorFont, find_editor_font};
pub use font::{Font, FontError, Metrics};
pub use gpu::{
    BACKGROUND, Capture, FrameError, FrameStats, FrameStatus, Gpu, GpuContext, GpuError,
};
pub use pipeline::{GLYPH_COLOR, GlyphInstance, RectInstance};
pub use text::{ColorRun, Style};
pub use ui::{UiFont, UiStyle};
