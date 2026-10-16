//! GPU rendering for Nimbus: surface management, the bundled font, shaping,
//! the glyph atlas, and the two instanced pipelines described in
//! DESIGN.md §4.3.

pub mod atlas;
pub mod font;
pub mod frame;
mod gpu;
pub mod packer;
mod pipeline;
pub mod raster;
pub mod shape;
mod text;

pub use font::{Font, FontError, Metrics};
pub use gpu::{BACKGROUND, Capture, FrameError, FrameStatus, Gpu, GpuContext, GpuError};
pub use pipeline::{GlyphInstance, RectInstance};
pub use text::ColorRun;
