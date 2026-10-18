//! The window's GPU connection: instance, adapter, device, queue and surface.

use std::sync::Arc;

use winit::window::Window;

use crate::atlas::Atlas;
use crate::font::{Font, Metrics};
use crate::frame::FrameBuilder;
use crate::pipeline::{Globals, GlyphPipeline, Pipelines, RectPipeline};
use crate::text::{ColorRun, LineStyle, TextRenderer};

/// Columns per tab stop until the app sets one with [`Gpu::set_tab_width`].
const DEFAULT_TAB_WIDTH: usize = 4;

/// The default background colour, until the app sets the theme's with
/// [`Gpu::set_background`].
pub const BACKGROUND: wgpu::Color = wgpu::Color {
    r: 0.102,
    g: 0.110,
    b: 0.133,
    a: 1.0,
};

/// Everything needed to draw into one window.
///
/// `wgpu` concepts in one breath: an `Instance` is the API entry point, an
/// `Adapter` is a physical GPU, a `Device` is our logical connection to it
/// (used to create resources), a `Queue` is where we submit work, and a
/// `Surface` is the window's drawable area, configured with a size and pixel
/// format.
pub struct Gpu {
    /// Where frames go: a window's surface, or an off-screen texture.
    target: Target,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipelines: Pipelines,
    atlas: Atlas,
    text: TextRenderer,
    frame: FrameBuilder,
    /// Set by [`Gpu::request_capture`]; the next frame is copied back.
    capture_requested: bool,
    captured: Option<Capture>,
    /// What each frame is cleared to.
    background: wgpu::Color,
    /// The swapchain reported Suboptimal; reconfigure after presenting.
    /// Reconfiguring while a frame is acquired is an error in wgpu.
    reconfigure: bool,
}

/// The render target. Headless rendering (no window) draws into a texture,
/// which works with the screen locked and on machines with no display.
enum Target {
    Surface(wgpu::Surface<'static>),
    Offscreen(wgpu::Texture),
}

/// The sRGB transfer curve undone: a colour channel as stored in an image
/// (0–1, gamma-encoded) to light intensity (0–1, linear).
fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

impl Gpu {
    /// Whether frames blend in linear light (the `--linear-blend`
    /// experiment took effect).
    #[must_use]
    pub fn linear_blend(&self) -> bool {
        self.config.format.is_srgb()
    }
}

/// Format of off-screen frames: the same 8-bit format windows use on macOS,
/// so headless captures match on-screen ones.
const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

fn offscreen_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("offscreen frame"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// What happened when [`Gpu::render`] was asked to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameStatus {
    /// The frame is on screen.
    Presented,
    /// The frame is on screen but incomplete, for example glyphs were
    /// skipped because the atlas was full and is being reset. Request
    /// another redraw.
    Incomplete,
    /// Nothing was drawn because of a transient problem (stale swapchain,
    /// GPU timeout). Request another redraw.
    Retry,
    /// Nothing was drawn because the window is hidden. Wait for it to be
    /// shown again (`WindowEvent::Occluded(false)` or a resize) before
    /// redrawing; retrying now would spin.
    Hidden,
}

/// A frame read back from the GPU, for debugging and screenshot tests.
#[derive(Debug, Clone)]
pub struct Capture {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Tightly packed RGBA bytes, row by row from the top.
    pub rgba: Vec<u8>,
}

/// Failures while setting up the GPU for a window. All are fatal at startup.
#[derive(Debug)]
pub enum GpuError {
    /// No GPU (or software fallback) can present to this window.
    NoAdapter,
    /// The OS refused to give us a drawable surface for the window.
    Surface(wgpu::CreateSurfaceError),
    /// The adapter exists but would not open a device with our required limits.
    Device(wgpu::RequestDeviceError),
    /// The adapter cannot present to this surface in any format.
    Unsupported,
}

impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GpuError::NoAdapter => write!(f, "no compatible GPU adapter found"),
            GpuError::Surface(e) => write!(f, "could not create surface: {e}"),
            GpuError::Device(e) => write!(f, "could not open GPU device: {e}"),
            GpuError::Unsupported => write!(f, "surface is not supported by the adapter"),
        }
    }
}

impl std::error::Error for GpuError {}

/// Counts for the debug overlay ([`Gpu::stats`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    /// Glyphs queued in the frame.
    pub glyphs: usize,
    /// How full the glyph atlas is, from 0 to 1.
    pub atlas: f32,
    /// GPU memory the colour (emoji) atlas page takes: 0 until the first
    /// colour glyph is drawn, then 16 MB.
    pub color_page_bytes: u64,
}

/// Unrecoverable (from inside `render`) per-frame failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The surface must be recreated; the window should be torn down and rebuilt.
    SurfaceLost,
    /// A validation error was raised acquiring the frame; a bug on our side.
    Validation,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::SurfaceLost => write!(f, "GPU surface lost"),
            FrameError::Validation => write!(f, "GPU validation error acquiring frame"),
        }
    }
}

impl std::error::Error for FrameError {}

/// The GPU connection without a window: instance, adapter, device and queue.
///
/// Opening these takes tens of milliseconds, mostly in the driver. None of it
/// needs the window, so [`GpuContext::spawn`] does it on a background thread
/// started first thing in `main`, overlapping the time the OS spends creating
/// and showing the window. That overlap is what keeps cold start inside the
/// 100 ms budget (DESIGN.md §4.3, "Startup path").
pub struct GpuContext {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl std::fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContext")
            .field("adapter", &self.adapter.get_info().name)
            .finish_non_exhaustive()
    }
}

impl GpuContext {
    /// Open the GPU on the calling thread, choosing an adapter without
    /// knowing which window it will draw to.
    ///
    /// # Errors
    ///
    /// Returns [`GpuError::NoAdapter`] or [`GpuError::Device`] if no GPU is
    /// usable.
    pub fn open() -> Result<Self, GpuError> {
        let started = std::time::Instant::now();
        // Backends and debug flags can be overridden with WGPU_BACKEND /
        // WGPU_DEBUG environment variables, handy when something misrenders.
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = request_adapter(&instance, None)?;
        let (device, queue) = request_device(&adapter)?;
        log::info!("gpu context opened in {:?}", started.elapsed());
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
        })
    }

    /// Start [`GpuContext::open`] on a background thread. Join the handle
    /// once the window exists.
    ///
    /// # Errors
    ///
    /// Returns the OS error if the thread cannot be spawned. Errors opening
    /// the GPU come back through the join handle.
    pub fn spawn() -> std::io::Result<std::thread::JoinHandle<Result<Self, GpuError>>> {
        std::thread::Builder::new()
            .name("gpu-init".into())
            .spawn(Self::open)
    }
}

fn request_adapter(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
) -> Result<wgpu::Adapter, GpuError> {
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: surface,
        ..Default::default()
    }))
    .map_err(|_| GpuError::NoAdapter)?;
    let info = adapter.get_info();
    log::info!(
        "adapter: {} ({:?}, {:?})",
        info.name,
        info.backend,
        info.device_type
    );
    Ok(adapter)
}

/// Cap a surface size at the largest texture the device allows. A window
/// larger than that (rare: beyond 8K on most GPUs) is drawn at the cap and
/// stretched by the OS, instead of failing to configure.
fn clamp_to_device(device: &wgpu::Device, width: u32, height: u32) -> (u32, u32) {
    let max = device.limits().max_texture_dimension_2d;
    if width > max || height > max {
        log::warn!("window {width}x{height} exceeds GPU limit {max}; clamping");
    }
    (width.min(max), height.min(max))
}

fn request_device(adapter: &wgpu::Adapter) -> Result<(wgpu::Device, wgpu::Queue), GpuError> {
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("nimbus device"),
        required_features: wgpu::Features::empty(),
        // Conservative limits so the editor runs on old GPUs, except texture
        // size: the downlevel cap is 2048 px, smaller than a Retina window,
        // so the surface size limit comes from the real adapter.
        required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .map_err(GpuError::Device)
}

impl Gpu {
    /// Create a surface for `window` from an already-opened [`GpuContext`]
    /// and configure it at the window's current size.
    ///
    /// The context is normally built on a background thread by
    /// [`GpuContext::spawn`] while the OS creates the window. If the adapter
    /// it picked cannot present to this window, which can happen because it
    /// was chosen before the window existed, a compatible adapter and device
    /// are requested here instead, on the slower path.
    ///
    /// The surface keeps its own clone of `window` alive, because a surface
    /// must never outlive the window it draws into.
    ///
    /// `linear_blend` is the M16 experiment (`--linear-blend`): draw to an
    /// sRGB surface so that blending happens in linear light.
    ///
    /// # Errors
    ///
    /// Returns a [`GpuError`] if no adapter can present to the window, the
    /// device cannot be opened, or the surface cannot be created or configured.
    pub fn new(
        window: &Arc<Window>,
        context: GpuContext,
        font: Font,
        font_size_px: f32,
        linear_blend: bool,
    ) -> Result<Self, GpuError> {
        let GpuContext {
            instance,
            mut adapter,
            mut device,
            mut queue,
        } = context;

        let surface = instance
            .create_surface(window.clone())
            .map_err(GpuError::Surface)?;

        if !adapter.is_surface_supported(&surface) {
            log::warn!("pre-selected adapter cannot present to the window; re-requesting");
            adapter = request_adapter(&instance, Some(&surface))?;
            (device, queue) = request_device(&adapter)?;
        }

        // By default wgpu panics on a validation error, and release builds
        // abort on panic. Log instead, so a GPU bug degrades the frame rather
        // than killing the editor with unsaved work.
        device.on_uncaptured_error(Arc::new(|e| log::error!("wgpu: {e}")));

        let size = window.inner_size();
        let (width, height) = clamp_to_device(&device, size.width.max(1), size.height.max(1));
        let mut config = surface
            .get_default_config(&adapter, width, height)
            .ok_or(GpuError::Unsupported)?;
        // Prefer a non-sRGB view format; we'll handle gamma ourselves when
        // text rendering arrives, which keeps glyph edges predictable.
        // Use the 8-bit non-sRGB twin of the default format. Picking "any
        // non-sRGB format" could land on a 16-bit float HDR format, which is
        // linear and would wash out gamma-space colours.
        let caps = surface.get_capabilities(&adapter);
        let plain = config.format.remove_srgb_suffix();
        if caps.formats.contains(&plain) {
            config.format = plain;
        }
        if linear_blend {
            let srgb = plain.add_srgb_suffix();
            if caps.formats.contains(&srgb) {
                config.format = srgb;
            } else {
                log::warn!("--linear-blend: no sRGB surface format; blending in gamma space");
            }
        }
        // Fifo is present everywhere and never tears. We draw on demand, so
        // vsync latency is not a concern.
        config.present_mode = wgpu::PresentMode::AutoVsync;
        // Allow copying frames back for `--screenshot` where the platform
        // supports it. Costs nothing unless a copy is recorded.
        if caps.usages.contains(wgpu::TextureUsages::COPY_SRC) {
            config.usage |= wgpu::TextureUsages::COPY_SRC;
        }
        surface.configure(&device, &config);

        log::info!(
            "surface: {}x{} {:?}",
            config.width,
            config.height,
            config.format
        );

        Ok(Self::assemble(
            Target::Surface(surface),
            device,
            queue,
            config,
            (font, font_size_px),
        ))
    }

    /// Render without a window, into an off-screen `width × height` texture.
    /// Every frame can be captured with [`Gpu::request_capture`]. Used for
    /// headless screenshots, which work with the screen locked.
    /// `linear_blend` as for [`Gpu::new`].
    #[must_use]
    pub fn headless(
        context: GpuContext,
        font: Font,
        font_size_px: f32,
        (width, height): (u32, u32),
        linear_blend: bool,
    ) -> Self {
        let GpuContext { device, queue, .. } = context;
        device.on_uncaptured_error(Arc::new(|e| log::error!("wgpu: {e}")));
        let (width, height) = clamp_to_device(&device, width.max(1), height.max(1));
        let format = if linear_blend {
            OFFSCREEN_FORMAT.add_srgb_suffix()
        } else {
            OFFSCREEN_FORMAT
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            format,
            width,
            height,
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: Vec::new(),
            // Unused off-screen; only a window surface has a colour space.
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        let texture = offscreen_texture(&device, width, height, format);
        Self::assemble(
            Target::Offscreen(texture),
            device,
            queue,
            config,
            (font, font_size_px),
        )
    }

    fn assemble(
        target: Target,
        device: wgpu::Device,
        queue: wgpu::Queue,
        config: wgpu::SurfaceConfiguration,
        (font, font_size_px): (Font, f32),
    ) -> Self {
        let atlas = Atlas::new(&device);
        let globals = Globals::new(&device, config.format.is_srgb());
        globals.set_viewport(&queue, config.width, config.height);
        let pipelines = Pipelines {
            rect: RectPipeline::new(&device, config.format, &globals),
            glyph: GlyphPipeline::new(&device, config.format, &globals, &atlas),
            globals,
        };
        let text = TextRenderer::new(font, font_size_px, DEFAULT_TAB_WIDTH);

        Self {
            target,
            device,
            queue,
            config,
            pipelines,
            atlas,
            text,
            frame: FrameBuilder::default(),
            capture_requested: false,
            captured: None,
            background: BACKGROUND,
            reconfigure: false,
        }
    }

    /// Reconfigure the surface after the window changed size or scale factor.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            // Minimised. Keep the old config; rendering is skipped until restored.
            return;
        }
        let (width, height) = clamp_to_device(&self.device, width, height);
        if width == self.config.width && height == self.config.height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.configure();
        self.pipelines
            .globals
            .set_viewport(&self.queue, width, height);
    }

    /// Draw the queued frame into `texture`, capturing it if requested.
    fn render_to(&mut self, texture: &wgpu::Texture) {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        self.frame.record(
            &self.device,
            &self.queue,
            &mut encoder,
            (
                &view,
                [self.config.width, self.config.height],
                self.background,
            ),
            &mut self.pipelines,
            &self.atlas,
        );
        let readback = if self.capture_requested {
            self.capture_requested = false;
            Some(self.record_readback(&mut encoder, texture))
        } else {
            None
        };
        self.queue.submit(std::iter::once(encoder.finish()));
        if let Some((buffer, padded_row)) = readback {
            self.captured = self.finish_readback(&buffer, padded_row);
        }
    }

    /// Apply `self.config` to the target: reconfigure the window surface,
    /// or recreate the off-screen texture at the new size.
    fn configure(&mut self) {
        match &mut self.target {
            Target::Surface(surface) => surface.configure(&self.device, &self.config),
            Target::Offscreen(texture) => {
                *texture = offscreen_texture(
                    &self.device,
                    self.config.width,
                    self.config.height,
                    self.config.format,
                );
            }
        }
    }

    /// Record a copy of `texture` into a new mappable buffer. Rows in the
    /// buffer are padded to 256 bytes, which buffer copies require.
    fn record_readback(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
    ) -> (wgpu::Buffer, u32) {
        let (width, height) = (self.config.width, self.config.height);
        let padded_row = (width * 4).div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture"),
            size: u64::from(padded_row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        (buffer, padded_row)
    }

    /// Wait for the copy, then unpad rows and convert to RGBA.
    fn finish_readback(&self, buffer: &wgpu::Buffer, padded_row: u32) -> Option<Capture> {
        buffer.map_async(wgpu::MapMode::Read, .., |r| {
            if let Err(e) = r {
                log::error!("capture map failed: {e}");
            }
        });
        if let Err(e) = self.device.poll(wgpu::PollType::wait_indefinitely()) {
            log::error!("capture wait failed: {e}");
            return None;
        }
        let view = match buffer.get_mapped_range(..) {
            Ok(v) => v,
            Err(e) => {
                log::error!("capture read failed: {e}");
                return None;
            }
        };
        let (width, height) = (self.config.width, self.config.height);
        let row = (width * 4) as usize;
        let mut rgba = Vec::with_capacity(row * height as usize);
        for chunk in view.chunks(padded_row as usize).take(height as usize) {
            rgba.extend_from_slice(chunk.get(..row)?);
        }
        drop(view);
        buffer.unmap();
        // An sRGB target's bytes are already encoded as sRGB: the same
        // bytes a plain target holds, so they are saved as they are.
        if self.config.format.remove_srgb_suffix() == wgpu::TextureFormat::Bgra8Unorm {
            for px in rgba.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
            }
        }
        Some(Capture {
            width,
            height,
            rgba,
        })
    }

    /// Change the font's pixel size, for a scale-factor or font-size change.
    /// Clears the glyph cache and atlas when the size actually changes.
    pub fn set_font_size(&mut self, size_px: f32) {
        self.text.set_size(size_px, &mut self.atlas, &self.queue);
    }

    /// Draw the editor with `font` instead of the bundled one, bold text from
    /// `font.bold` (or the regular weight). The atlas is refilled and the
    /// grid measured again, so the caller should lay out and redraw.
    pub fn set_font(&mut self, font: crate::EditorFont) {
        self.text
            .set_font(font.regular, font.bold, &mut self.atlas, &self.queue);
    }

    /// Let the font draw ligatures (`ligatures = true` in the config). Off
    /// by default. Changing it needs no atlas reset: a ligature is just
    /// another glyph id.
    pub fn set_ligatures(&mut self, on: bool) {
        self.text.set_ligatures(on);
    }

    /// Whether ligatures are on, so the caller knows to pass cursor
    /// positions to [`Gpu::push_text_runs`].
    #[must_use]
    pub fn ligatures(&self) -> bool {
        self.text.ligatures()
    }

    /// Columns per tab stop. Must match the value cursor movement uses, or
    /// the cursor drifts from the text after a tab.
    pub fn set_tab_width(&mut self, tab_width: usize) {
        self.text.set_tab_width(tab_width);
    }

    /// Grid measurements for the current font size.
    #[must_use]
    pub fn metrics(&self) -> Metrics {
        self.text.metrics()
    }

    /// Copy the next rendered frame back from the GPU; collect it with
    /// [`Gpu::take_capture`]. Returns `false` if this surface cannot be
    /// copied from or uses a pixel format the capture does not handle.
    pub fn request_capture(&mut self) -> bool {
        let readable = self.config.usage.contains(wgpu::TextureUsages::COPY_SRC)
            && matches!(
                self.config.format.remove_srgb_suffix(),
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
            );
        self.capture_requested = readable;
        readable
    }

    /// The frame captured after [`Gpu::request_capture`], if it is ready.
    pub fn take_capture(&mut self) -> Option<Capture> {
        self.captured.take()
    }

    /// The colour each frame is cleared to, as straight RGBA in the same
    /// space as rectangle colours.
    pub fn set_background(&mut self, color: [f32; 4]) {
        let [r, g, b, a] = color.map(f64::from);
        // An sRGB target takes the clear colour in linear light.
        let lin = |c: f64| {
            if self.config.format.is_srgb() {
                srgb_to_linear(c)
            } else {
                c
            }
        };
        self.background = wgpu::Color {
            r: lin(r),
            g: lin(g),
            b: lin(b),
            a,
        };
    }

    /// Start a new frame, discarding anything queued for the previous one.
    pub fn begin_frame(&mut self) {
        self.frame.clear();
        self.text.begin_frame(&mut self.atlas, &self.queue);
    }

    /// Start a new layer drawn above everything queued so far, clipped to
    /// `clip` as `[x, y, width, height]` in physical pixels (`None` for the
    /// whole window). Overlays use this to cover the text beneath them.
    pub fn begin_layer(&mut self, clip: Option<[f32; 4]>) {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "window pixel coordinates, clamped to be non-negative"
        )]
        let to_px = |v: f32| v.max(0.0).round() as u32;
        self.frame
            .begin_layer(clip.map(|[x, y, w, h]| [to_px(x), to_px(y), to_px(w), to_px(h)]));
    }

    /// Queue a filled rectangle for this frame. Within a layer, rectangles
    /// draw under text.
    pub fn push_rect(&mut self, pos: [f32; 2], size: [f32; 2], color: [f32; 4]) {
        self.frame.push_rect(pos, size, color);
    }

    /// Queue one line of text with its top-left corner at `origin`, in
    /// physical pixels. `line` must not contain a newline.
    pub fn push_text_line(&mut self, origin: [f32; 2], line: &str, color: [f32; 4]) {
        self.push_text_span(origin, line, 0, color, f32::NEG_INFINITY);
    }

    /// Queue part of a line: `text` starts at column `start_col` of its
    /// line, and `origin` is where that column is drawn. Tab stops are still
    /// measured from the start of the line. Glyphs that start left of
    /// `clip_left` are skipped, so horizontally scrolled text does not draw
    /// over the gutter. Drawing only the visible span keeps long lines cheap.
    pub fn push_text_span(
        &mut self,
        origin: [f32; 2],
        text: &str,
        start_col: usize,
        color: [f32; 4],
        clip_left: f32,
    ) {
        self.push_text_runs(origin, text, start_col, (&[], &[]), color, clip_left);
    }

    /// As [`Gpu::push_text_span`], with colours by byte range: each glyph
    /// takes the colour of the run covering its first byte, or `color` past
    /// the last run. `splits` are byte offsets in `text` where a cursor or
    /// selection edge is, so a ligature across one is drawn split.
    pub fn push_text_runs(
        &mut self,
        origin: [f32; 2],
        text: &str,
        start_col: usize,
        (runs, splits): (&[ColorRun], &[usize]),
        color: [f32; 4],
        clip_left: f32,
    ) {
        self.text.push_line(
            &self.queue,
            &mut self.atlas,
            &mut self.frame,
            origin,
            text,
            LineStyle {
                color,
                runs,
                clip_left,
                start_col,
                splits,
            },
        );
    }

    /// Queue one character with its cell's top-left corner at `origin`,
    /// skipping shaping: for markers drawn many times a frame, such as the
    /// dots of visible whitespace.
    pub fn push_char(&mut self, origin: [f32; 2], ch: char, color: [f32; 4]) {
        self.text.push_char(
            &self.queue,
            &mut self.atlas,
            &mut self.frame,
            origin,
            ch,
            color,
        );
    }

    /// Call `notify` (from a worker thread) when a fallback font arrives
    /// for a character the bundled font lacks; the window should then draw
    /// again, and the next frame uses the font.
    pub fn set_font_notify(&mut self, notify: crate::fallback::Notify) {
        self.text.set_font_notify(notify);
    }

    /// Wait up to `timeout` for fallback fonts still being looked for, for
    /// a headless render, which has no later frame to show them in.
    /// Returns whether any arrived: draw the frame again if so.
    pub fn wait_for_fonts(&mut self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut any = self.text.receive_fonts();
        while self.text.fonts_pending() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
            any |= self.text.receive_fonts();
        }
        any
    }

    /// Counts for the debug overlay: glyphs queued so far in the frame being
    /// built, and how full the glyph atlas is.
    #[must_use]
    pub fn stats(&self) -> FrameStats {
        FrameStats {
            glyphs: self.frame.glyph_count(),
            atlas: self.atlas.occupancy(),
            color_page_bytes: self.atlas.color_bytes(),
        }
    }

    /// Current surface size in physical pixels, as `(width, height)`.
    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Draw the queued frame: clear to the background, then rectangles, then text.
    ///
    /// The returned [`FrameStatus`] tells the caller whether to schedule
    /// another redraw.
    ///
    /// # Errors
    ///
    /// Returns a [`FrameError`] when the surface is lost or acquiring the frame
    /// raised a validation error. Neither can be fixed by retrying.
    pub fn render(&mut self) -> Result<FrameStatus, FrameError> {
        use wgpu::CurrentSurfaceTexture as T;

        let acquire_started = std::time::Instant::now();
        let surface = match &self.target {
            Target::Surface(surface) => surface,
            Target::Offscreen(texture) => {
                let texture = texture.clone();
                self.render_to(&texture);
                return Ok(FrameStatus::Presented);
            }
        };
        let frame = match surface.get_current_texture() {
            T::Success(frame) => frame,
            T::Suboptimal(frame) => {
                // Still usable this frame; reconfigure once it is presented.
                self.reconfigure = true;
                frame
            }
            T::Outdated => {
                // Swapchain no longer matches the surface (e.g. a resize
                // race). No frame is held, so reconfiguring now is safe.
                self.configure();
                return Ok(FrameStatus::Retry);
            }
            T::Timeout => return Ok(FrameStatus::Retry),
            T::Occluded => return Ok(FrameStatus::Hidden),
            T::Lost => return Err(FrameError::SurfaceLost),
            T::Validation => return Err(FrameError::Validation),
        };

        log::trace!("frame acquired in {:?}", acquire_started.elapsed());
        self.render_to(&frame.texture);
        self.queue.present(frame);
        if std::mem::take(&mut self.reconfigure) {
            self.configure();
        }
        log::trace!(
            "frame presented {:?} after acquire began",
            acquire_started.elapsed()
        );
        log::debug!(
            "frame: {} glyphs, atlas {:.1}% full",
            self.frame.glyph_count(),
            self.atlas.occupancy() * 100.0
        );
        Ok(if self.text.needs_redraw() {
            FrameStatus::Incomplete
        } else {
            FrameStatus::Presented
        })
    }
}
