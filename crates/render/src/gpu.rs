//! The window's GPU connection: instance, adapter, device, queue and surface,
//! or, in compatibility mode (M23), the CPU painter in `soft.rs` instead.

use std::sync::Arc;

use winit::window::Window;

use crate::atlas::Atlas;
use crate::font::{Font, Metrics};
use crate::frame::FrameBuilder;
use crate::pipeline::{Globals, GlyphPipeline, Pipelines, RectPipeline};
use crate::soft::{CompatError, Cpu};
use crate::text::{ColorRun, LineStyle, TextRenderer};
use crate::ui::UiFont;

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
/// Despite the name, frames are painted by one of two painters: the GPU
/// through wgpu, or the CPU in compatibility mode (M23) when no GPU can be
/// opened. Both are fed the same [`FrameBuilder`], so callers never need
/// to know which one runs ([`Gpu::compatibility`] says, for the overlay).
pub struct Gpu {
    painter: Painter,
    atlas: Atlas,
    text: TextRenderer,
    /// The interface font (M20), when `ui_font` names one and it has
    /// loaded; `None` draws the interface on the editor's grid.
    ui_font: Option<crate::EditorFont>,
    frame: FrameBuilder,
    /// Set by [`Gpu::request_capture`]; the next frame is copied back.
    capture_requested: bool,
    captured: Option<Capture>,
    /// What each frame is cleared to: straight RGBA, as theme colours are.
    background: [f32; 4],
    /// Pixels kept at the top for a title bar the app draws
    /// ([`Gpu::set_inset_top`]).
    inset_top: f32,
}

/// Which painter draws the frames.
enum Painter {
    Wgpu(Box<Wgpu>),
    Cpu(Cpu),
}

/// The wgpu painter.
///
/// `wgpu` concepts in one breath: an `Instance` is the API entry point, an
/// `Adapter` is a physical GPU, a `Device` is our logical connection to it
/// (used to create resources), a `Queue` is where we submit work, and a
/// `Surface` is the window's drawable area, configured with a size and pixel
/// format.
struct Wgpu {
    /// Where frames go: a window's surface, or an off-screen texture.
    target: Target,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipelines: Pipelines,
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
    /// experiment took effect). Never in compatibility mode.
    #[must_use]
    pub fn linear_blend(&self) -> bool {
        match &self.painter {
            Painter::Wgpu(w) => w.config.format.is_srgb(),
            Painter::Cpu(_) => false,
        }
    }

    /// Whether frames are painted on the CPU (compatibility mode) rather
    /// than by the GPU.
    #[must_use]
    pub fn compatibility(&self) -> bool {
        matches!(self.painter, Painter::Cpu(_))
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

/// Failures while setting up the GPU for a window. Any of them starts
/// compatibility mode instead, except [`GpuError::Compat`], which is
/// compatibility mode failing too and is fatal at startup.
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
    /// Compatibility mode could not show its frames in the window either.
    Compat(CompatError),
}

impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GpuError::NoAdapter => write!(f, "no compatible GPU adapter found"),
            GpuError::Surface(e) => write!(f, "could not create surface: {e}"),
            GpuError::Device(e) => write!(f, "could not open GPU device: {e}"),
            GpuError::Unsupported => write!(f, "surface is not supported by the adapter"),
            GpuError::Compat(e) => write!(f, "compatibility mode: {e}"),
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
    /// Memory the colour (emoji) atlas page takes: 0 until the first
    /// colour glyph is drawn, then 16 MB.
    pub color_page_bytes: u64,
    /// Frames are painted on the CPU (compatibility mode).
    pub compatibility: bool,
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

        Ok(Self::assemble_wgpu(
            Target::Surface(surface),
            device,
            queue,
            config,
            (font, font_size_px),
        ))
    }

    /// Paint `window` on the CPU (compatibility mode, M23), for when the GPU
    /// could not be opened or `--compat` asks for it. Everything else
    /// behaves as with [`Gpu::new`].
    ///
    /// # Errors
    ///
    /// [`GpuError::Compat`] if the window cannot show a pixel buffer.
    pub fn compat(window: &Arc<Window>, font: Font, font_size_px: f32) -> Result<Self, GpuError> {
        let cpu = Cpu::window(window).map_err(GpuError::Compat)?;
        let (width, height) = cpu.size();
        log::info!("compatibility mode: painting {width}x{height} on the CPU");
        Ok(Self::assemble(
            Painter::Cpu(cpu),
            Atlas::in_memory(),
            (font, font_size_px),
        ))
    }

    /// Render without a window or a GPU, painting `width × height` frames
    /// on the CPU, as [`Gpu::compat`] does for a window.
    #[must_use]
    pub fn compat_headless(font: Font, font_size_px: f32, (width, height): (u32, u32)) -> Self {
        Self::assemble(
            Painter::Cpu(Cpu::offscreen(width, height)),
            Atlas::in_memory(),
            (font, font_size_px),
        )
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
        Self::assemble_wgpu(
            Target::Offscreen(texture),
            device,
            queue,
            config,
            (font, font_size_px),
        )
    }

    fn assemble_wgpu(
        target: Target,
        device: wgpu::Device,
        queue: wgpu::Queue,
        config: wgpu::SurfaceConfiguration,
        font: (Font, f32),
    ) -> Self {
        let atlas = Atlas::new(&device, &queue);
        let globals = Globals::new(&device, config.format.is_srgb());
        globals.set_viewport(&queue, config.width, config.height, 0.0);
        // A GPU atlas always has its pages; `new` above made them.
        let pipelines = atlas.gpu().map(|pages| Pipelines {
            rect: RectPipeline::new(&device, config.format, &globals),
            glyph: GlyphPipeline::new(&device, config.format, &globals, pages),
            globals,
        });
        let Some(pipelines) = pipelines else {
            log::error!("GPU atlas has no pages; painting on the CPU");
            let cpu = Cpu::offscreen(config.width, config.height);
            return Self::assemble(Painter::Cpu(cpu), Atlas::in_memory(), font);
        };
        let wgpu = Wgpu {
            target,
            device,
            queue,
            config,
            pipelines,
            reconfigure: false,
        };
        Self::assemble(Painter::Wgpu(Box::new(wgpu)), atlas, font)
    }

    fn assemble(painter: Painter, atlas: Atlas, (font, font_size_px): (Font, f32)) -> Self {
        let text = TextRenderer::new(font, font_size_px, DEFAULT_TAB_WIDTH);
        Self {
            painter,
            atlas,
            text,
            ui_font: None,
            frame: FrameBuilder::default(),
            capture_requested: false,
            captured: None,
            background: [BACKGROUND.r, BACKGROUND.g, BACKGROUND.b, BACKGROUND.a].map(to_f32),
            inset_top: 0.0,
        }
    }

    /// Reconfigure the surface after the window changed size or scale factor.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            // Minimised. Keep the old config; rendering is skipped until restored.
            return;
        }
        match &mut self.painter {
            Painter::Wgpu(w) => w.resize(width, height, self.inset_top),
            Painter::Cpu(cpu) => cpu.resize(width, height),
        }
    }

    /// Keep `px` pixels at the top of the window for a title bar the app
    /// draws itself (macOS, where the window's content runs up under the
    /// system's buttons). Everything drawn after this moves down by `px`:
    /// y = 0 is just below the bar, and the bar is drawn at negative y.
    /// Clips given to [`Gpu::begin_layer`] move with it, so the app lays
    /// out the window as if it were `px` shorter
    /// ([`Gpu::content_size`]) and nothing else changes. Both painters
    /// apply it: the GPU in its shaders, the CPU as it fills pixels.
    pub fn set_inset_top(&mut self, px: f32) {
        let px = px.max(0.0).round();
        if (px - self.inset_top).abs() < f32::EPSILON {
            return;
        }
        self.inset_top = px;
        if let Painter::Wgpu(w) = &self.painter {
            w.pipelines
                .globals
                .set_viewport(&w.queue, w.config.width, w.config.height, px);
        }
    }

    /// The top inset set by [`Gpu::set_inset_top`], in pixels.
    #[must_use]
    pub fn inset_top(&self) -> f32 {
        self.inset_top
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "colour channels from 0 to 1, where f32 is plenty"
)]
fn to_f32(v: f64) -> f32 {
    v as f32
}

impl Wgpu {
    fn resize(&mut self, width: u32, height: u32, top: f32) {
        let (width, height) = clamp_to_device(&self.device, width, height);
        if width == self.config.width && height == self.config.height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.configure();
        self.pipelines
            .globals
            .set_viewport(&self.queue, width, height, top);
    }

    /// The clear colour for `background` (straight RGBA): an sRGB target
    /// takes it in linear light.
    fn clear_color(&self, background: [f32; 4]) -> wgpu::Color {
        let [r, g, b, a] = background.map(f64::from);
        let lin = |c: f64| {
            if self.config.format.is_srgb() {
                srgb_to_linear(c)
            } else {
                c
            }
        };
        wgpu::Color {
            r: lin(r),
            g: lin(g),
            b: lin(b),
            a,
        }
    }

    /// Draw `frame` into `texture`, and read it back when `capture` is set.
    fn render_to(
        &mut self,
        texture: &wgpu::Texture,
        frame: &mut FrameBuilder,
        (atlas, background): (&Atlas, [f32; 4]),
        capture: bool,
    ) -> Option<Capture> {
        let Some(pages) = atlas.gpu() else {
            log::error!("GPU painter given an atlas in memory; frame skipped");
            return None;
        };
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        let clear = self.clear_color(background);
        frame.record(
            &self.device,
            &self.queue,
            &mut encoder,
            (&view, [self.config.width, self.config.height], clear),
            &mut self.pipelines,
            pages,
        );
        let readback = capture.then(|| self.record_readback(&mut encoder, texture));
        self.queue.submit(std::iter::once(encoder.finish()));
        readback.and_then(|(buffer, padded_row)| self.finish_readback(&buffer, padded_row))
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
}

impl Gpu {
    /// Change the font's pixel size, for a scale-factor or font-size change.
    /// Clears the glyph cache and atlas when the size actually changes.
    pub fn set_font_size(&mut self, size_px: f32) {
        self.text.set_size(size_px, &mut self.atlas);
    }

    /// Draw the editor with `font` instead of the bundled one, bold text from
    /// `font.bold` (or the regular weight). The atlas is refilled and the
    /// grid measured again, so the caller should lay out and redraw.
    pub fn set_font(&mut self, font: crate::EditorFont) {
        self.text.set_font(font.regular, font.bold, &mut self.atlas);
    }

    /// Prefer these fallback fonts per script and draw Han ideographs in
    /// `prefs.cjk`'s regional forms (M24: the config's `fallback_fonts`
    /// and `cjk_language`). Characters already found are looked up again.
    pub fn set_fallback_prefs(&mut self, prefs: crate::FallbackPrefs) {
        self.text.set_fallback_prefs(prefs);
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

    /// Draw the interface (sidebar, tabs, status bar, pickers) in `font`,
    /// a proportional font, or on the editor's grid with `None` (M20).
    pub fn set_ui_font(&mut self, font: Option<crate::EditorFont>) {
        self.ui_font = font;
        self.text.clear_ui_glyphs();
    }

    /// How interface text is measured at the current size: carry it in a
    /// layout to measure labels without the GPU.
    #[must_use]
    pub fn ui_font(&self) -> UiFont {
        let editor = self.text.metrics();
        match self.ui_font {
            Some(font) => UiFont::new(font.regular, editor).with_bold(font.bold),
            None => UiFont::grid(editor),
        }
    }

    /// The font the Markdown preview (M20) is drawn in: the interface
    /// font when one is set, else the editor font. Unlike [`Gpu::ui_font`]
    /// it is never the grid, because the preview needs sizes and weights
    /// the grid cannot draw.
    #[must_use]
    pub fn preview_font(&self) -> UiFont {
        if self.ui_font.is_some() {
            return self.ui_font();
        }
        let (regular, bold) = self.text.editor_faces();
        UiFont::new(regular, self.text.metrics()).with_bold(Some(bold))
    }

    /// Queue a line of text in `ui` and `style` (a heading's size, bold,
    /// italic) with its top-left corner at `origin`. Returns the x where it
    /// ends. `ui` should come from [`Gpu::preview_font`] or
    /// [`Gpu::ui_font`] at the current size.
    pub fn push_styled_text(
        &mut self,
        origin: [f32; 2],
        text: &str,
        color: [f32; 4],
        (ui, style): (&UiFont, crate::UiStyle),
    ) -> f32 {
        if !ui.is_proportional() {
            return self.push_ui_text(origin, text, color);
        }
        self.text.push_ui_styled(
            &mut self.atlas,
            &mut self.frame,
            (origin, color),
            text,
            ui,
            style,
        )
    }

    /// Queue a line of interface text with its top-left corner at
    /// `origin`, in the interface font when one is set (M20). Returns the
    /// x where it ends.
    pub fn push_ui_text(&mut self, origin: [f32; 2], text: &str, color: [f32; 4]) -> f32 {
        let ui = self.ui_font();
        self.text
            .push_ui_line(&mut self.atlas, &mut self.frame, origin, text, color, &ui)
    }

    /// Copy the next rendered frame back from the GPU; collect it with
    /// [`Gpu::take_capture`]. Returns `false` if this surface cannot be
    /// copied from or uses a pixel format the capture does not handle.
    /// Frames painted on the CPU can always be captured.
    pub fn request_capture(&mut self) -> bool {
        let readable = match &self.painter {
            Painter::Wgpu(w) => {
                w.config.usage.contains(wgpu::TextureUsages::COPY_SRC)
                    && matches!(
                        w.config.format.remove_srgb_suffix(),
                        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
                    )
            }
            Painter::Cpu(_) => true,
        };
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
        self.background = color;
    }

    /// Start a new frame, discarding anything queued for the previous one.
    pub fn begin_frame(&mut self) {
        self.frame.clear();
        self.text.begin_frame(&mut self.atlas);
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
        let top = self.inset_top;
        self.frame
            .begin_layer(clip.map(|[x, y, w, h]| [to_px(x), to_px(y + top), to_px(w), to_px(h)]));
    }

    /// The current layer's clip, in the form [`Gpu::begin_layer`] takes
    /// (rounded to whole pixels). A helper that needs a layer of its own
    /// reads this first and passes it back to `begin_layer` when done, so
    /// whatever it was drawn inside, such as the sidebar, stays clipped.
    #[must_use]
    pub fn clip(&self) -> Option<[f32; 4]> {
        #[expect(
            clippy::cast_precision_loss,
            reason = "window pixel coordinates, far below 2^24"
        )]
        let to_f = |v: u32| v as f32;
        let top = self.inset_top;
        self.frame
            .clip()
            .map(|[x, y, w, h]| [to_f(x), to_f(y) - top, to_f(w), to_f(h)])
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
        self.text
            .push_char(&mut self.atlas, &mut self.frame, origin, ch, color);
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
            compatibility: self.compatibility(),
        }
    }

    /// The size the app lays out: the surface less the top inset
    /// ([`Gpu::set_inset_top`]), in physical pixels.
    #[must_use]
    pub fn content_size(&self) -> (u32, u32) {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a whole, non-negative number of pixels"
        )]
        let top = self.inset_top as u32;
        let (width, height) = self.size();
        (width, height.saturating_sub(top))
    }

    /// Current surface size in physical pixels, as `(width, height)`.
    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        match &self.painter {
            Painter::Wgpu(w) => (w.config.width, w.config.height),
            Painter::Cpu(cpu) => cpu.size(),
        }
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
        let capture = std::mem::take(&mut self.capture_requested);
        let status = match &mut self.painter {
            Painter::Wgpu(w) => {
                let (status, captured) =
                    w.render(&mut self.frame, (&self.atlas, self.background), capture)?;
                self.captured = captured.or(self.captured.take());
                status
            }
            Painter::Cpu(cpu) => {
                let Some(pages) = self.atlas.cpu() else {
                    log::error!("CPU painter given a GPU atlas; frame skipped");
                    return Ok(FrameStatus::Retry);
                };
                let (status, captured) = cpu.render(
                    &mut self.frame,
                    pages,
                    (self.background, self.inset_top),
                    capture,
                );
                self.captured = captured.or(self.captured.take());
                status
            }
        };
        log::debug!(
            "frame: {} glyphs, atlas {:.1}% full",
            self.frame.glyph_count(),
            self.atlas.occupancy() * 100.0
        );
        Ok(match status {
            FrameStatus::Presented if self.text.needs_redraw() => FrameStatus::Incomplete,
            other => other,
        })
    }
}

impl Wgpu {
    /// Acquire the window's next texture (or use the off-screen one), draw
    /// into it and present it. Returns the frame read back when `capture`.
    fn render(
        &mut self,
        frame: &mut FrameBuilder,
        scene: (&Atlas, [f32; 4]),
        capture: bool,
    ) -> Result<(FrameStatus, Option<Capture>), FrameError> {
        use wgpu::CurrentSurfaceTexture as T;

        let acquire_started = std::time::Instant::now();
        let surface = match &self.target {
            Target::Surface(surface) => surface,
            Target::Offscreen(texture) => {
                let texture = texture.clone();
                let captured = self.render_to(&texture, frame, scene, capture);
                return Ok((FrameStatus::Presented, captured));
            }
        };
        let next = match surface.get_current_texture() {
            T::Success(next) => next,
            T::Suboptimal(next) => {
                // Still usable this frame; reconfigure once it is presented.
                self.reconfigure = true;
                next
            }
            T::Outdated => {
                // Swapchain no longer matches the surface (e.g. a resize
                // race). No frame is held, so reconfiguring now is safe.
                self.configure();
                return Ok((FrameStatus::Retry, None));
            }
            T::Timeout => return Ok((FrameStatus::Retry, None)),
            T::Occluded => return Ok((FrameStatus::Hidden, None)),
            T::Lost => return Err(FrameError::SurfaceLost),
            T::Validation => return Err(FrameError::Validation),
        };

        log::trace!("frame acquired in {:?}", acquire_started.elapsed());
        let captured = self.render_to(&next.texture, frame, scene, capture);
        self.queue.present(next);
        if std::mem::take(&mut self.reconfigure) {
            self.configure();
        }
        log::trace!(
            "frame presented {:?} after acquire began",
            acquire_started.elapsed()
        );
        Ok((FrameStatus::Presented, captured))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Queue a frame shaped like a full editor window: a background, a
    /// current-line highlight, a translucent selection, a gutter, and
    /// `rows` lines of code-like text.
    fn editor_frame(gpu: &mut Gpu, (width, height): (u32, u32)) {
        #[expect(clippy::cast_precision_loss, reason = "a window size")]
        let (w, h) = (width as f32, height as f32);
        let lh = gpu.metrics().line_height.ceil();
        gpu.begin_frame();
        gpu.set_background([0.1, 0.11, 0.13, 1.0]);
        gpu.push_rect([0.0, 0.0], [80.0, h], [0.12, 0.13, 0.15, 1.0]);
        gpu.push_rect([80.0, lh * 5.0], [w - 80.0, lh], [0.2, 0.2, 0.25, 1.0]);
        gpu.begin_layer(Some([80.0, 0.0, w - 80.0, h]));
        gpu.push_rect([200.0, lh * 8.0], [600.0, lh * 4.0], [0.3, 0.5, 0.9, 0.35]);
        let line = "    let mut counts = HashMap::new(); // a fairly long line of code to draw ok";
        let mut y = 0.0;
        while y < h {
            gpu.push_text_line([90.0, y], line, [0.85, 0.85, 0.8, 1.0]);
            y += lh;
        }
    }

    #[test]
    fn compat_mode_paints_text_without_a_gpu() {
        let font = Font::bundled().unwrap();
        let mut gpu = Gpu::compat_headless(font, 28.0, (400, 200));
        assert!(gpu.compatibility() && !gpu.linear_blend());
        editor_frame(&mut gpu, (400, 200));
        assert!(gpu.request_capture());
        assert_eq!(gpu.render().unwrap(), FrameStatus::Presented);
        let cap = gpu.take_capture().unwrap();
        assert_eq!((cap.width, cap.height), (400, 200));
        // Text pixels are lighter than anything else in the frame.
        let light = cap
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0] > 150)
            .count();
        assert!(light > 500, "{light} text pixels");
        assert!(gpu.stats().compatibility);
    }

    #[test]
    fn compat_mode_moves_everything_down_by_the_inset() {
        let font = Font::bundled().unwrap();
        let mut gpu = Gpu::compat_headless(font, 28.0, (8, 8));
        gpu.set_background([0.0, 0.0, 0.0, 1.0]);
        gpu.set_inset_top(3.0);
        assert_eq!(gpu.content_size(), (8, 5));
        gpu.begin_frame();
        // A title bar at negative y, then a rect clipped to the content.
        gpu.push_rect([0.0, -3.0], [8.0, 3.0], [1.0, 0.0, 0.0, 1.0]);
        gpu.begin_layer(Some([0.0, 0.0, 8.0, 1.0]));
        assert_eq!(gpu.clip(), Some([0.0, 0.0, 8.0, 1.0]));
        gpu.push_rect([0.0, 0.0], [8.0, 8.0], [0.0, 0.0, 1.0, 1.0]);
        assert!(gpu.request_capture());
        gpu.render().unwrap();
        let cap = gpu.take_capture().unwrap();
        let row = |y: usize| cap.rgba.get(y * 8 * 4..y * 8 * 4 + 4).unwrap().to_vec();
        assert_eq!(row(0), [255, 0, 0, 255]);
        assert_eq!(row(2), [255, 0, 0, 255]);
        assert_eq!(row(3), [0, 0, 255, 255]);
        assert_eq!(row(4)[..3], [0, 0, 0]);
    }

    /// `cargo test --release -p nimbus-render -- --ignored --nocapture
    /// compat_frame_bench`: the M23 budget is under 16 ms for a full
    /// 1600x1000 frame. Also times a Retina-sized 3200x2000 frame.
    #[test]
    #[ignore = "benchmark; run in release"]
    fn compat_frame_bench() {
        for size in [(1600, 1000), (3200, 2000)] {
            let font = Font::bundled().unwrap();
            let mut gpu = Gpu::compat_headless(font, 28.0, size);
            let mut times = Vec::new();
            for _ in 0..50 {
                editor_frame(&mut gpu, size);
                let t = std::time::Instant::now();
                gpu.render().unwrap();
                times.push(t.elapsed());
            }
            times.sort();
            println!(
                "compat frame {}x{}: p50 {:?}, p90 {:?}, worst {:?} ({} glyphs)",
                size.0,
                size.1,
                times[25],
                times[45],
                times[49],
                gpu.stats().glyphs
            );
        }
    }
}
