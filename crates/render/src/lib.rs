//! GPU rendering for Nimbus: surface management, the bundled font, shaping,
//! the glyph atlas, and the instanced pipelines described in DESIGN.md
//! §4.3 (rects and glyph quads, plus M27's curve segments), plus the CPU painter for compatibility mode (M23).

pub mod atlas;
pub mod fallback;
pub mod font;
pub mod frame;
mod gpu;
pub mod packer;
mod pipeline;
pub mod raster;
pub mod script;
mod segment;
pub mod shape;
mod soft;
mod text;
pub mod ui;

pub use fallback::{EditorFont, FallbackPrefs, bundled_editor_font, find_editor_font};
pub use font::{Font, FontError, Metrics};
pub use gpu::{
    BACKGROUND, Capture, FrameError, FrameStats, FrameStatus, Gpu, GpuContext, GpuError,
};
pub use pipeline::{GLYPH_COLOR, GlyphInstance, RectInstance, SegmentInstance};
pub use script::CjkLanguage;
pub use soft::CompatError;
pub use text::{ColorRun, Style};
pub use ui::{UiFont, UiStyle};
