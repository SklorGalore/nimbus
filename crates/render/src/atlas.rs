//! The glyph atlas: one GPU texture holding every rasterized glyph bitmap.
//!
//! Drawing thousands of glyphs from one texture means one bind group and one
//! draw call per frame instead of one per glyph. The [`ShelfPacker`] decides
//! where each bitmap goes; this module owns the texture and copies bitmaps in.

use crate::packer::{Rect, ShelfPacker};

/// Side length of the atlas texture, in texels. Equal to the 2D texture size
/// limit under the downlevel limits the device is opened with, so a larger
/// atlas would need higher device limits; a second page is the plan instead.
pub const ATLAS_SIZE: u32 = 2048;

/// Where a glyph bitmap lives in the atlas, ready to copy into an instance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtlasSlot {
    /// Bitmap width and height in pixels.
    pub size: [f32; 2],
    /// Texture coordinates of the top-left corner.
    pub uv_min: [f32; 2],
    /// Texture coordinates of the bottom-right corner.
    pub uv_max: [f32; 2],
}

/// Why a glyph could not be added to the atlas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasError {
    /// No room left. The caller logs and skips the glyph.
    Full,
    /// The bitmap is not `width × height` bytes, for example a colour bitmap.
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

/// The atlas texture, its packer, and the bind group the glyph shader uses.
pub struct Atlas {
    texture: wgpu::Texture,
    layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    packer: ShelfPacker,
}

impl std::fmt::Debug for Atlas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Atlas")
            .field("occupancy", &self.occupancy())
            .finish_non_exhaustive()
    }
}

impl Atlas {
    /// Create the texture. Its format is `R8Unorm`: one byte per texel, which
    /// is exactly the grayscale coverage mask the rasterizer produces.
    #[must_use]
    pub fn new(device: &wgpu::Device) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // Nearest: glyphs are drawn at exactly their rasterized size, so there
        // is nothing to interpolate.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..wgpu::SamplerDescriptor::default()
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atlas"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atlas"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        Self {
            texture,
            layout,
            bind_group,
            packer: ShelfPacker::new(ATLAS_SIZE, ATLAS_SIZE, 1),
        }
    }

    /// Reserve space for a `width × height` coverage bitmap and queue a copy
    /// of it to the GPU. The copy lands before the next `queue.submit`.
    ///
    /// # Errors
    ///
    /// [`AtlasError::Full`] when there is no room; [`AtlasError::BitmapSize`]
    /// when `coverage` is not exactly `width * height` bytes.
    pub fn insert(
        &mut self,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        coverage: &[u8],
    ) -> Result<AtlasSlot, AtlasError> {
        let expected = u64::from(width) * u64::from(height);
        let got = coverage.len() as u64;
        if got != expected {
            return Err(AtlasError::BitmapSize { expected, got });
        }
        let rect = self
            .packer
            .allocate(width, height)
            .ok_or(AtlasError::Full)?;
        if width > 0 && height > 0 {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: rect.x,
                        y: rect.y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                coverage,
                // Rows are tightly packed, one byte per texel. The 256-byte
                // row alignment rule applies to buffer-to-texture copies, not
                // to write_texture.
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        }
        Ok(slot_for(rect))
    }

    /// Forget every glyph, for example after a scale change, and zero the
    /// texture. Zeroing matters because new glyphs rely on the empty padding
    /// around them; stale texels there could bleed into edge samples.
    ///
    /// Uploads 4 MB of zeros, which is fine for a rare event.
    pub fn clear(&mut self, queue: &wgpu::Queue) {
        self.packer.clear();
        let zeros = vec![0u8; (ATLAS_SIZE * ATLAS_SIZE) as usize];
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
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

    /// Fraction of the atlas in use, for logging.
    #[must_use]
    pub fn occupancy(&self) -> f32 {
        self.packer.occupancy()
    }

    pub(crate) fn layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    pub(crate) fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "atlas coordinates are at most 2048, exact in f32"
)]
fn slot_for(r: Rect) -> AtlasSlot {
    let s = ATLAS_SIZE as f32;
    AtlasSlot {
        size: [r.w as f32, r.h as f32],
        uv_min: [r.x as f32 / s, r.y as f32 / s],
        uv_max: [(r.x + r.w) as f32 / s, (r.y + r.h) as f32 / s],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_uvs_cover_exactly_the_rect() {
        let slot = slot_for(Rect {
            x: 1024,
            y: 512,
            w: 8,
            h: 16,
        });
        assert_eq!(slot.size, [8.0, 16.0]);
        assert_eq!(slot.uv_min, [0.5, 0.25]);
        assert_eq!(slot.uv_max, [1032.0 / 2048.0, 528.0 / 2048.0]);
    }
}
