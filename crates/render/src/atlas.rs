//! The glyph atlas: GPU textures holding every rasterized glyph bitmap.
//!
//! Drawing thousands of glyphs from one texture means one bind group and one
//! draw call per frame instead of one per glyph. The [`ShelfPacker`] decides
//! where each bitmap goes; this module owns the textures and copies bitmaps in.
//!
//! **Two pages (M16).** Ordinary glyphs are *coverage masks*: one byte per
//! pixel saying how much of it the outline covers, multiplied by the text
//! colour in the shader. A colour emoji is a picture with its own colours,
//! four bytes per pixel, which a one-byte texture cannot hold. So there is a
//! second texture, the colour page, in `Rgba8Unorm`. It costs 16 MB of GPU
//! memory (2048 × 2048 × 4 bytes), so it is created only when the first
//! colour glyph arrives; until then a 1 × 1 stand-in fills its slot in the
//! bind group, because a bind group must always name a texture.

use crate::packer::{Rect, ShelfPacker};

/// Side length of each atlas texture, in texels. Equal to the 2D texture size
/// limit under the downlevel limits the device is opened with.
pub const ATLAS_SIZE: u32 = 2048;

/// Bytes of GPU memory the colour page takes once created.
pub const COLOR_PAGE_BYTES: u64 = ATLAS_SIZE as u64 * ATLAS_SIZE as u64 * 4;

/// Which texture a glyph bitmap lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AtlasPage {
    /// The `R8Unorm` page of coverage masks, tinted by the text colour.
    Coverage,
    /// The `Rgba8Unorm` page of colour images (emoji), drawn as they are.
    /// Texels are premultiplied: red, green and blue already multiplied by
    /// alpha, which is what the blend state expects.
    Color,
}

impl AtlasPage {
    /// Bytes per texel on this page.
    #[must_use]
    pub fn bytes_per_texel(self) -> u32 {
        match self {
            AtlasPage::Coverage => 1,
            AtlasPage::Color => 4,
        }
    }
}

/// Where a glyph bitmap lives in the atlas, ready to copy into an instance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtlasSlot {
    /// Bitmap width and height in pixels.
    pub size: [f32; 2],
    /// Texture coordinates of the top-left corner.
    pub uv_min: [f32; 2],
    /// Texture coordinates of the bottom-right corner.
    pub uv_max: [f32; 2],
    /// The texture it is in.
    pub page: AtlasPage,
}

/// Why a glyph could not be added to the atlas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasError {
    /// No room left. The caller logs and skips the glyph.
    Full,
    /// The bitmap is not `width × height × bytes per texel` bytes.
    BitmapSize {
        /// Bytes the dimensions call for.
        expected: u64,
        /// Bytes supplied.
        got: u64,
    },
}

impl std::fmt::Display for AtlasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AtlasError::Full => write!(f, "glyph atlas is full"),
            AtlasError::BitmapSize { expected, got } => {
                write!(f, "glyph bitmap has {got} bytes, expected {expected}")
            }
        }
    }
}

impl std::error::Error for AtlasError {}

/// Where the packers have placed glyphs, one packer per page.
///
/// Kept apart from the textures so the bookkeeping can be tested without a
/// GPU: each page has its own packer, so filling one never moves anything
/// in the other.
#[derive(Debug)]
struct Book {
    coverage: ShelfPacker,
    color: ShelfPacker,
}

impl Book {
    fn new() -> Self {
        Self {
            coverage: ShelfPacker::new(ATLAS_SIZE, ATLAS_SIZE, 1),
            color: ShelfPacker::new(ATLAS_SIZE, ATLAS_SIZE, 1),
        }
    }

    fn packer(&mut self, page: AtlasPage) -> &mut ShelfPacker {
        match page {
            AtlasPage::Coverage => &mut self.coverage,
            AtlasPage::Color => &mut self.color,
        }
    }
}

/// The atlas textures, their packers, and the bind group the glyph shader
/// uses.
pub struct Atlas {
    device: wgpu::Device,
    coverage: wgpu::Texture,
    /// The colour page, once the first colour glyph has asked for it.
    color: Option<wgpu::Texture>,
    /// The 1 × 1 texture bound where the colour page goes until it exists.
    stand_in: wgpu::Texture,
    sampler: wgpu::Sampler,
    layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    book: Book,
}

impl std::fmt::Debug for Atlas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Atlas")
            .field("occupancy", &self.occupancy())
            .field("color_page", &self.color.is_some())
            .finish_non_exhaustive()
    }
}

fn page_texture(
    device: &wgpu::Device,
    label: &str,
    size: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

impl Atlas {
    /// Create the coverage page. Its format is `R8Unorm`: one byte per
    /// texel, which is exactly the grayscale coverage mask the rasterizer
    /// produces. The colour page waits for the first colour glyph.
    #[must_use]
    pub fn new(device: &wgpu::Device) -> Self {
        let coverage = page_texture(
            device,
            "glyph atlas",
            ATLAS_SIZE,
            wgpu::TextureFormat::R8Unorm,
        );
        let stand_in = page_texture(
            device,
            "colour atlas stand-in",
            1,
            wgpu::TextureFormat::Rgba8Unorm,
        );
        // Nearest: glyphs are drawn at exactly their rasterized size (colour
        // bitmaps too: swash resizes a bitmap strike to the size asked for),
        // so there is nothing to interpolate.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..wgpu::SamplerDescriptor::default()
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atlas"),
            entries: &[
                texture_entry(0),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture_entry(2),
            ],
        });
        let bind_group = bind(device, &layout, &coverage, &stand_in, &sampler);
        Self {
            device: device.clone(),
            coverage,
            color: None,
            stand_in,
            sampler,
            layout,
            bind_group,
            book: Book::new(),
        }
    }

    fn rebind(&mut self) {
        let color = self.color.as_ref().unwrap_or(&self.stand_in);
        self.bind_group = bind(
            &self.device,
            &self.layout,
            &self.coverage,
            color,
            &self.sampler,
        );
    }

    /// Reserve space on `page` for a `width × height` bitmap and queue a
    /// copy of it to the GPU. The copy lands before the next `queue.submit`.
    /// The first colour glyph creates the colour page.
    ///
    /// # Errors
    ///
    /// [`AtlasError::Full`] when there is no room; [`AtlasError::BitmapSize`]
    /// when `data` is not exactly `width * height` texels of the page's
    /// size.
    pub fn insert(
        &mut self,
        queue: &wgpu::Queue,
        page: AtlasPage,
        (width, height): (u32, u32),
        data: &[u8],
    ) -> Result<AtlasSlot, AtlasError> {
        let bpt = page.bytes_per_texel();
        let expected = u64::from(width) * u64::from(height) * u64::from(bpt);
        let got = data.len() as u64;
        if got != expected {
            return Err(AtlasError::BitmapSize { expected, got });
        }
        let rect = self
            .book
            .packer(page)
            .allocate(width, height)
            .ok_or(AtlasError::Full)?;
        if width == 0 || height == 0 {
            return Ok(slot_for(rect, page));
        }
        if page == AtlasPage::Color && self.color.is_none() {
            log::info!(
                "colour atlas page created: {} MB of GPU memory",
                COLOR_PAGE_BYTES >> 20
            );
            self.color = Some(page_texture(
                &self.device,
                "colour glyph atlas",
                ATLAS_SIZE,
                wgpu::TextureFormat::Rgba8Unorm,
            ));
            self.rebind();
        }
        let texture = match page {
            AtlasPage::Coverage => &self.coverage,
            AtlasPage::Color => self.color.as_ref().unwrap_or(&self.stand_in),
        };
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: rect.x,
                    y: rect.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            // Rows are tightly packed. The 256-byte row alignment rule
            // applies to buffer-to-texture copies, not to write_texture.
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * bpt),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        Ok(slot_for(rect, page))
    }

    /// Forget every glyph, for example after a scale or zoom change. The
    /// coverage page is zeroed, because new glyphs rely on the empty padding
    /// around them; stale texels there could bleed into edge samples. The
    /// colour page is dropped instead (a new texture starts zeroed), which
    /// also gives its 16 MB back until an emoji is drawn again.
    ///
    /// Uploads 4 MB of zeros, which is fine for a rare event.
    pub fn clear(&mut self, queue: &wgpu::Queue) {
        self.book.coverage.clear();
        self.book.color.clear();
        if self.color.take().is_some() {
            self.rebind();
        }
        let zeros = vec![0u8; (ATLAS_SIZE * ATLAS_SIZE) as usize];
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.coverage,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &zeros,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(ATLAS_SIZE),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Fraction of the coverage page in use, for logging.
    #[must_use]
    pub fn occupancy(&self) -> f32 {
        self.book.coverage.occupancy()
    }

    /// GPU memory the colour page takes: 0 until the first colour glyph.
    #[must_use]
    pub fn color_bytes(&self) -> u64 {
        if self.color.is_some() {
            COLOR_PAGE_BYTES
        } else {
            0
        }
    }

    pub(crate) fn layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    pub(crate) fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }
}

/// Bind group 1: the coverage page, the sampler, and the colour page (or
/// its stand-in).
fn bind(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    coverage: &wgpu::Texture,
    color: &wgpu::Texture,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    let coverage = coverage.create_view(&wgpu::TextureViewDescriptor::default());
    let color = color.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("atlas"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&coverage),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&color),
            },
        ],
    })
}

#[expect(
    clippy::cast_precision_loss,
    reason = "atlas coordinates are at most 2048, exact in f32"
)]
fn slot_for(r: Rect, page: AtlasPage) -> AtlasSlot {
    let s = ATLAS_SIZE as f32;
    AtlasSlot {
        size: [r.w as f32, r.h as f32],
        uv_min: [r.x as f32 / s, r.y as f32 / s],
        uv_max: [(r.x + r.w) as f32 / s, (r.y + r.h) as f32 / s],
        page,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_uvs_cover_exactly_the_rect() {
        let slot = slot_for(
            Rect {
                x: 1024,
                y: 512,
                w: 8,
                h: 16,
            },
            AtlasPage::Coverage,
        );
        assert_eq!(slot.size, [8.0, 16.0]);
        assert_eq!(slot.uv_min, [0.5, 0.25]);
        assert_eq!(slot.uv_max, [1032.0 / 2048.0, 528.0 / 2048.0]);
    }

    #[test]
    fn packing_into_the_colour_page_never_touches_the_coverage_page() {
        let mut book = Book::new();
        let a = book.packer(AtlasPage::Coverage).allocate(10, 20).unwrap();
        let before = book.coverage.occupancy();
        // Fill the colour page completely.
        while book.packer(AtlasPage::Color).allocate(64, 64).is_some() {}
        assert!(book.color.occupancy() > 0.9);
        assert!((book.coverage.occupancy() - before).abs() < f32::EPSILON);
        // The coverage page carries on where it left off.
        let b = book.packer(AtlasPage::Coverage).allocate(10, 20).unwrap();
        assert_eq!((b.x, b.y), (a.x + 11, a.y));
    }

    #[test]
    fn the_colour_page_is_sixteen_megabytes() {
        assert_eq!(COLOR_PAGE_BYTES, 16 * 1024 * 1024);
    }
}
