//! Collecting one frame's rectangles and glyphs, then recording the draw.

use crate::atlas::{Atlas, AtlasSlot};
use crate::gpu::BACKGROUND;
use crate::pipeline::{GlyphInstance, Pipelines, RectInstance};

/// One frame's worth of instances.
///
/// The vectors are cleared, not dropped, between frames, so after the first
/// few frames building a frame allocates nothing.
#[derive(Debug, Default)]
pub struct FrameBuilder {
    rects: Vec<RectInstance>,
    glyphs: Vec<GlyphInstance>,
}

impl FrameBuilder {
    /// Empty the builder, keeping its capacity.
    pub fn clear(&mut self) {
        self.rects.clear();
        self.glyphs.clear();
    }

    /// Queue a filled rectangle. All rectangles draw before all glyphs.
    pub fn push_rect(&mut self, pos: [f32; 2], size: [f32; 2], color: [f32; 4]) {
        self.rects.push(RectInstance { pos, size, color });
    }

    /// Queue a glyph whose bitmap's top-left corner is at whole-pixel `pos`.
    pub fn push_glyph(&mut self, pos: [f32; 2], slot: &AtlasSlot, color: [f32; 4]) {
        self.glyphs.push(GlyphInstance {
            pos,
            size: slot.size,
            uv_min: slot.uv_min,
            uv_max: slot.uv_max,
            color,
        });
    }

    /// Number of glyphs queued, for logging.
    #[must_use]
    pub fn glyph_count(&self) -> usize {
        self.glyphs.len()
    }

    /// Upload the instances, then record one render pass: clear, rects,
    /// glyphs. Uploads come first because the pass borrows the encoder.
    pub(crate) fn record(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        pipelines: &mut Pipelines,
        atlas: &Atlas,
    ) {
        pipelines.rect.instances.upload(device, queue, &self.rects);
        pipelines
            .glyph
            .instances
            .upload(device, queue, &self.glyphs);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(BACKGROUND),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pipelines.rect.draw(&mut pass, &pipelines.globals);
        pipelines.glyph.draw(&mut pass, &pipelines.globals, atlas);
    }
}
