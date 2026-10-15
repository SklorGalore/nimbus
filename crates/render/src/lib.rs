//! GPU rendering for Nimbus: surface management, and later the glyph atlas,
//! text layout and the two instanced pipelines described in DESIGN.md §4.3.
//!
//! Milestone 0 only owns the surface and clears it to the background colour.

use std::sync::Arc;

use winit::window::Window;

/// Background colour of the editor area. Will come from the theme later.
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
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
}

#[derive(Debug)]
pub enum GpuError {
    NoAdapter,
    Surface(wgpu::CreateSurfaceError),
    Device(wgpu::RequestDeviceError),
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

impl Gpu {
    /// Create a surface for `window` and configure it at the window's current size.
    ///
    /// Blocks on adapter and device creation; this happens once at startup and
    /// is part of the time-to-first-frame budget, so it is logged.
    pub fn new(window: Arc<Window>) -> Result<Self, GpuError> {
        // Backends and debug flags can be overridden with WGPU_BACKEND /
        // WGPU_DEBUG environment variables, handy when something misrenders.
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());

        let surface = instance
            .create_surface(window.clone())
            .map_err(GpuError::Surface)?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
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

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("nimbus device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            ..Default::default()
        }))
        .map_err(GpuError::Device)?;

        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or(GpuError::Unsupported)?;
        // Prefer a non-sRGB view format; we'll handle gamma ourselves when
        // text rendering arrives, which keeps glyph edges predictable.
        let caps = surface.get_capabilities(&adapter);
        if let Some(fmt) = caps.formats.iter().copied().find(|f| !f.is_srgb()) {
            config.format = fmt;
        }
        // Fifo is present everywhere and never tears. We draw on demand, so
        // vsync latency is not a concern.
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&device, &config);

        log::info!(
            "surface: {}x{} {:?}",
            config.width,
            config.height,
            config.format
        );

        Ok(Self {
            surface,
            device,
            queue,
            config,
        })
    }

    /// Reconfigure the surface after the window changed size or scale factor.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            // Minimised. Keep the old config; rendering is skipped until restored.
            return;
        }
        if width == self.config.width && height == self.config.height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Draw one frame: currently just a clear to [`BACKGROUND`].
    ///
    /// Returns `Ok(false)` when the frame was skipped (window occluded, GPU
    /// timeout, stale swapchain) and the caller should simply try again on
    /// the next redraw request.
    pub fn render(&mut self) -> Result<bool, FrameError> {
        use wgpu::CurrentSurfaceTexture as T;

        let frame = match self.surface.get_current_texture() {
            T::Success(frame) => frame,
            T::Suboptimal(frame) => {
                // Still usable this frame; reconfigure for the next one.
                self.surface.configure(&self.device, &self.config);
                frame
            }
            T::Outdated => {
                // Swapchain no longer matches the surface (e.g. a resize race).
                self.surface.configure(&self.device, &self.config);
                return Ok(false);
            }
            T::Timeout | T::Occluded => return Ok(false),
            T::Lost => return Err(FrameError::SurfaceLost),
            T::Validation => return Err(FrameError::Validation),
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });

        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
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
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        self.queue.present(frame);
        Ok(true)
    }
}
