//! The two instanced pipelines: solid rectangles and textured glyph quads.
//!
//! **Instancing**: each pipeline draws with one call of "4 vertices × N
//! instances". The GPU runs the vertex shader 4·N times; each run reads the
//! per-instance struct (a [`RectInstance`] or [`GlyphInstance`]) and builds
//! one corner of a quad from the vertex index. So a rectangle or a glyph costs
//! one small struct, not four vertices, and a whole screen of text is a
//! single draw call.
//!
//! **Bind groups**: shaders read resources (buffers, textures, samplers)
//! through numbered groups. Group 0 holds [`Globals`], shared by both
//! pipelines; group 1 holds the atlas texture and sampler, used only by the
//! glyph pipeline. A group's *layout* is the type a pipeline is compiled
//! against; the *bind group* is a value of that type.

use std::marker::PhantomData;
use std::ops::Range;

use crate::atlas::Atlas;

/// One solid rectangle in physical pixels, origin top left.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RectInstance {
    /// Top-left corner.
    pub pos: [f32; 2],
    /// Width and height.
    pub size: [f32; 2],
    /// Straight (not premultiplied) RGBA, each 0 to 1.
    pub color: [f32; 4],
}

/// One glyph quad. `size` equals the bitmap size and `pos` is whole pixels,
/// so each screen pixel maps to exactly one atlas texel.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GlyphInstance {
    /// Top-left corner of the glyph bitmap.
    pub pos: [f32; 2],
    /// Bitmap width and height in pixels.
    pub size: [f32; 2],
    /// Atlas texture coordinates of the bitmap's top-left corner.
    pub uv_min: [f32; 2],
    /// Atlas texture coordinates of the bitmap's bottom-right corner.
    pub uv_max: [f32; 2],
    /// Straight RGBA text colour. A colour glyph keeps its own colours and
    /// takes only the alpha from here.
    pub color: [f32; 4],
    /// Bit 0 ([`GLYPH_COLOR`]): the bitmap is on the colour page.
    pub flags: u32,
}

/// [`GlyphInstance::flags`] bit: sample the colour page and draw the texel
/// as it is, rather than tinting a coverage mask with the text colour.
pub const GLYPH_COLOR: u32 = 1;

const _: () = assert!(size_of::<RectInstance>() == 32 && size_of::<GlyphInstance>() == 52);

const RECT_ATTRS: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4];
const GLYPH_ATTRS: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
    0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Float32x2, 4 => Float32x4, 5 => Uint32
];

fn instance_layout<T>(
    attributes: &'static [wgpu::VertexAttribute],
) -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: size_of::<T>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes,
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GlobalsUniform {
    viewport: [f32; 2],
    /// 1.0 when the target is sRGB (`--linear-blend`): the shaders turn
    /// colours into linear light before blending. 0.0 otherwise.
    linear: f32,
    /// Pixels at the top of the window kept for a title bar the app draws
    /// (see [`crate::Gpu::set_inset_top`]): positions shift down by this.
    top: f32,
}

/// Group 0: data every shader reads: the viewport size, which the shaders
/// need to turn pixel positions into clip-space positions, the top inset
/// they shift positions down by, and whether colours are blended in linear
/// light.
pub(crate) struct Globals {
    buffer: wgpu::Buffer,
    linear: bool,
    pub(crate) layout: wgpu::BindGroupLayout,
    pub(crate) bind_group: wgpu::BindGroup,
}

impl Globals {
    pub(crate) fn new(device: &wgpu::Device, linear: bool) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: size_of::<GlobalsUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });
        Self {
            buffer,
            linear,
            layout,
            bind_group,
        }
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "surface sizes are far below 2^24, exact in f32"
    )]
    pub(crate) fn set_viewport(&self, queue: &wgpu::Queue, width: u32, height: u32, top: f32) {
        let u = GlobalsUniform {
            viewport: [width as f32, height as f32],
            linear: if self.linear { 1.0 } else { 0.0 },
            top,
        };
        queue.write_buffer(&self.buffer, 0, bytemuck::bytes_of(&u));
    }
}

/// A GPU buffer of instances that grows by doubling. Recreating a buffer is
/// cheap but not free, so after the first few frames it never happens.
pub(crate) struct InstanceBuffer<T> {
    buffer: wgpu::Buffer,
    capacity: usize,
    len: u32,
    label: &'static str,
    _marker: PhantomData<T>,
}

fn create_instance_buffer(device: &wgpu::Device, label: &str, bytes: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

impl<T: bytemuck::Pod> InstanceBuffer<T> {
    fn new(device: &wgpu::Device, label: &'static str, capacity: usize) -> Self {
        Self {
            buffer: create_instance_buffer(device, label, capacity * size_of::<T>()),
            capacity,
            len: 0,
            label,
            _marker: PhantomData,
        }
    }

    pub(crate) fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, data: &[T]) {
        self.len = 0;
        if data.is_empty() {
            return;
        }
        let Ok(len) = u32::try_from(data.len()) else {
            log::error!("{}: too many instances ({})", self.label, data.len());
            return;
        };
        if data.len() > self.capacity {
            let mut cap = self.capacity.max(1);
            while cap < data.len() {
                cap = cap.saturating_mul(2);
            }
            self.buffer = create_instance_buffer(device, self.label, cap * size_of::<T>());
            self.capacity = cap;
            log::debug!("{} instance buffer grown to {cap}", self.label);
        }
        // Instance structs are 32 or 52 bytes, so the 4-byte copy alignment holds.
        queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(data));
        self.len = len;
    }

    fn slice(&self) -> wgpu::BufferSlice<'_> {
        // len never exceeds capacity, so this range is always in bounds.
        self.buffer
            .slice(..u64::from(self.len) * size_of::<T>() as u64)
    }
}

fn create_pipeline(
    device: &wgpu::Device,
    label: &str,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    instances: wgpu::VertexBufferLayout<'static>,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(instances)],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..wgpu::PrimitiveState::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                // Premultiplied alpha: the shader already multiplied rgb by
                // alpha, so blending is out = src + dst × (1 − src.a). This
                // avoids dark fringes around text and makes "transparent" a
                // true zero.
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Draws [`RectInstance`]s.
pub(crate) struct RectPipeline {
    pipeline: wgpu::RenderPipeline,
    pub(crate) instances: InstanceBuffer<RectInstance>,
}

impl RectPipeline {
    pub(crate) fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        globals: &Globals,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/rect.wgsl"));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rect"),
            bind_group_layouts: &[Some(&globals.layout)],
            immediate_size: 0,
        });
        Self {
            pipeline: create_pipeline(
                device,
                "rect",
                &shader,
                &layout,
                instance_layout::<RectInstance>(&RECT_ATTRS),
                format,
            ),
            instances: InstanceBuffer::new(device, "rects", 256),
        }
    }

    /// Draw the instances in `range` (indices into this frame's upload).
    pub(crate) fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        globals: &Globals,
        range: Range<u32>,
    ) {
        let range = range.start.min(self.instances.len)..range.end.min(self.instances.len);
        if range.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &globals.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice());
        pass.draw(0..4, range);
    }
}

/// Draws [`GlyphInstance`]s, sampling coverage or colour from the atlas.
pub(crate) struct GlyphPipeline {
    pipeline: wgpu::RenderPipeline,
    pub(crate) instances: InstanceBuffer<GlyphInstance>,
}

impl GlyphPipeline {
    pub(crate) fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        globals: &Globals,
        atlas: &Atlas,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/glyph.wgsl"));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glyph"),
            bind_group_layouts: &[Some(&globals.layout), Some(atlas.layout())],
            immediate_size: 0,
        });
        Self {
            pipeline: create_pipeline(
                device,
                "glyph",
                &shader,
                &layout,
                instance_layout::<GlyphInstance>(&GLYPH_ATTRS),
                format,
            ),
            instances: InstanceBuffer::new(device, "glyphs", 4096),
        }
    }

    /// Draw the instances in `range` (indices into this frame's upload).
    pub(crate) fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        globals: &Globals,
        atlas: &Atlas,
        range: Range<u32>,
    ) {
        let range = range.start.min(self.instances.len)..range.end.min(self.instances.len);
        if range.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &globals.bind_group, &[]);
        pass.set_bind_group(1, atlas.bind_group(), &[]);
        pass.set_vertex_buffer(0, self.instances.slice());
        pass.draw(0..4, range);
    }
}

/// Everything the draw needs, owned by `Gpu`.
pub(crate) struct Pipelines {
    pub(crate) globals: Globals,
    pub(crate) rect: RectPipeline,
    pub(crate) glyph: GlyphPipeline,
}

#[cfg(test)]
mod tests {
    use wgpu::naga;

    fn validate(src: &str) {
        let module = naga::front::wgsl::parse_str(src).unwrap();
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap();
    }

    #[test]
    fn rect_shader_is_valid_wgsl() {
        validate(include_str!("shaders/rect.wgsl"));
    }

    #[test]
    fn glyph_shader_is_valid_wgsl() {
        validate(include_str!("shaders/glyph.wgsl"));
    }
}
