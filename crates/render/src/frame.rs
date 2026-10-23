//! Collecting one frame's rectangles, curve segments and glyphs, then
//! recording the draw.
//!
//! A frame is a stack of *layers*. Each layer draws its rectangles, then its
//! curve segments (the git graph's lines and dots), then its glyphs, clipped to its own region with a scissor rectangle. The editor
//! text is one layer clipped to the text area; overlays such as the find bar
//! are later layers, so their backgrounds cover the text beneath them.
//! Within a layer, all rectangles still draw before all segments, and all
//! segments before all glyphs: a row's background sits under the graph, and
//! a branch label's text sits over it. Segments draw in the order pushed.

use std::ops::Range;

use crate::atlas::{AtlasPage, AtlasSlot, GpuPages};
use crate::pipeline::{GLYPH_COLOR, GlyphInstance, Pipelines, RectInstance, SegmentInstance};

/// One layer: a clip region and the instances drawn in it.
#[derive(Debug, Clone)]
struct Layer {
    /// `[x, y, width, height]` in physical pixels; `None` is the whole frame.
    clip: Option<[u32; 4]>,
    rects: Range<u32>,
    segments: Range<u32>,
    glyphs: Range<u32>,
}

/// One finished layer as the CPU painter walks it: its clip and its
/// instances of each kind, in drawing order.
pub(crate) struct LayerView<'a> {
    /// `[x, y, width, height]` in physical pixels; `None` is the whole frame.
    pub(crate) clip: Option<[u32; 4]>,
    pub(crate) rects: &'a [RectInstance],
    pub(crate) segments: &'a [SegmentInstance],
    pub(crate) glyphs: &'a [GlyphInstance],
}

/// One frame's worth of instances.
///
/// The vectors are cleared, not dropped, between frames, so after the first
/// few frames building a frame allocates nothing.
#[derive(Debug, Default)]
pub struct FrameBuilder {
    rects: Vec<RectInstance>,
    segments: Vec<SegmentInstance>,
    glyphs: Vec<GlyphInstance>,
    /// Finished layers; the current one starts where the last one ended.
    layers: Vec<Layer>,
    clip: Option<[u32; 4]>,
}

fn index(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl FrameBuilder {
    /// Empty the builder, keeping its capacity. The first layer covers the
    /// whole frame.
    pub fn clear(&mut self) {
        self.rects.clear();
        self.segments.clear();
        self.glyphs.clear();
        self.layers.clear();
        self.clip = None;
    }

    /// Finish the current layer and start a new one drawn above it, clipped
    /// to `clip` (`[x, y, width, height]` in physical pixels), or unclipped.
    pub fn begin_layer(&mut self, clip: Option<[u32; 4]>) {
        self.close_layer();
        self.clip = clip;
    }

    /// The current layer's clip, as given to [`FrameBuilder::begin_layer`].
    #[must_use]
    pub fn clip(&self) -> Option<[u32; 4]> {
        self.clip
    }

    fn close_layer(&mut self) {
        let start = self
            .layers
            .last()
            .map_or((0, 0, 0), |l| (l.rects.end, l.segments.end, l.glyphs.end));
        let end = (
            index(self.rects.len()),
            index(self.segments.len()),
            index(self.glyphs.len()),
        );
        if end != start {
            self.layers.push(Layer {
                clip: self.clip,
                rects: start.0..end.0,
                segments: start.1..end.1,
                glyphs: start.2..end.2,
            });
        }
    }

    /// Queue a filled rectangle in the current layer, under its glyphs.
    pub fn push_rect(&mut self, pos: [f32; 2], size: [f32; 2], color: [f32; 4]) {
        self.rects.push(RectInstance { pos, size, color });
    }

    /// Queue a curve segment in the current layer, over its rectangles and
    /// under its glyphs, above the segments queued before it.
    pub fn push_segment(&mut self, segment: SegmentInstance) {
        self.segments.push(segment);
    }

    /// Queue a glyph whose bitmap's top-left corner is at whole-pixel `pos`.
    pub fn push_glyph(&mut self, pos: [f32; 2], slot: &AtlasSlot, color: [f32; 4]) {
        self.glyphs.push(GlyphInstance {
            pos,
            size: slot.size,
            uv_min: slot.uv_min,
            uv_max: slot.uv_max,
            color,
            flags: if slot.page == AtlasPage::Color {
                GLYPH_COLOR
            } else {
                0
            },
        });
    }

    /// Close the layer being built, so [`FrameBuilder::layers`] sees it.
    /// Both painters call this before drawing; calling it twice is harmless.
    pub(crate) fn finish(&mut self) {
        self.close_layer();
    }

    /// The finished layers in drawing order: each one's clip, rectangles,
    /// segments and glyphs. The CPU painter (compatibility mode) walks these.
    pub(crate) fn layers(&self) -> impl Iterator<Item = LayerView<'_>> {
        let slice = |r: &Range<u32>| r.start as usize..r.end as usize;
        self.layers.iter().map(move |l| LayerView {
            clip: l.clip,
            rects: self.rects.get(slice(&l.rects)).unwrap_or_default(),
            segments: self.segments.get(slice(&l.segments)).unwrap_or_default(),
            glyphs: self.glyphs.get(slice(&l.glyphs)).unwrap_or_default(),
        })
    }

    /// Number of glyphs queued, for logging.
    #[must_use]
    pub fn glyph_count(&self) -> usize {
        self.glyphs.len()
    }

    /// Upload the instances, then record one render pass: clear, then each
    /// layer's rects, segments and glyphs within its clip. Uploads come first because
    /// the pass borrows the encoder. `target` is the view, its size in pixels,
    /// and the colour to clear it to.
    pub(crate) fn record(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: (&wgpu::TextureView, [u32; 2], wgpu::Color),
        pipelines: &mut Pipelines,
        atlas: &GpuPages,
    ) {
        self.finish();
        let (view, [width, height], background) = target;
        pipelines.rect.instances.upload(device, queue, &self.rects);
        pipelines
            .segment
            .instances
            .upload(device, queue, &self.segments);
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
                    load: wgpu::LoadOp::Clear(background),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        for layer in &self.layers {
            // The scissor must lie inside the frame, and wgpu rejects an
            // empty one; skip layers clipped away entirely.
            let [x, y, w, h] = layer.clip.unwrap_or([0, 0, width, height]);
            let (x, y) = (x.min(width), y.min(height));
            let (w, h) = (w.min(width - x), h.min(height - y));
            if w == 0 || h == 0 {
                continue;
            }
            pass.set_scissor_rect(x, y, w, h);
            pipelines
                .rect
                .draw(&mut pass, &pipelines.globals, layer.rects.clone());
            pipelines
                .segment
                .draw(&mut pass, &pipelines.globals, layer.segments.clone());
            pipelines
                .glyph
                .draw(&mut pass, &pipelines.globals, atlas, layer.glyphs.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_partition_the_instances_in_order() {
        let mut f = FrameBuilder::default();
        f.push_rect([0.0; 2], [1.0; 2], [1.0; 4]);
        f.begin_layer(Some([0, 10, 100, 50]));
        f.push_rect([0.0; 2], [1.0; 2], [1.0; 4]);
        f.push_rect([0.0; 2], [1.0; 2], [1.0; 4]);
        f.begin_layer(None);
        f.begin_layer(None); // empty layers are dropped
        f.push_rect([0.0; 2], [1.0; 2], [1.0; 4]);
        f.close_layer();
        let spans: Vec<_> = f.layers.iter().map(|l| (l.clip, l.rects.clone())).collect();
        assert_eq!(
            spans,
            vec![(None, 0..1), (Some([0, 10, 100, 50]), 1..3), (None, 3..4)]
        );
    }

    fn segment(x: f32) -> SegmentInstance {
        SegmentInstance {
            p0: [x, 0.0],
            p1: [x, 1.0],
            p2: [x, 2.0],
            p3: [x, 3.0],
            color: [1.0; 4],
            width: 1.0,
            pad: [0.0; 3],
        }
    }

    #[test]
    fn layers_partition_segments_too() {
        let mut f = FrameBuilder::default();
        f.push_segment(segment(0.0));
        f.begin_layer(Some([0, 0, 5, 5]));
        f.push_rect([0.0; 2], [1.0; 2], [1.0; 4]);
        f.begin_layer(None);
        // A layer holding only segments is kept.
        f.push_segment(segment(1.0));
        f.push_segment(segment(2.0));
        f.finish();
        let spans: Vec<_> = f
            .layers
            .iter()
            .map(|l| (l.rects.clone(), l.segments.clone()))
            .collect();
        assert_eq!(spans, vec![(0..0, 0..1), (0..1, 1..1), (1..1, 1..3)]);
        let xs: Vec<Vec<f32>> = f
            .layers()
            .map(|l| l.segments.iter().map(|s| s.p0[0]).collect())
            .collect();
        assert_eq!(xs, vec![vec![0.0], vec![], vec![1.0, 2.0]]);
        f.clear();
        assert!(f.segments.is_empty() && f.segments.capacity() >= 3);
    }

    #[test]
    fn clip_reports_the_current_layer() {
        let mut f = FrameBuilder::default();
        assert_eq!(f.clip(), None);
        f.begin_layer(Some([1, 2, 3, 4]));
        assert_eq!(f.clip(), Some([1, 2, 3, 4]));
        f.clear();
        assert_eq!(f.clip(), None);
    }
}
