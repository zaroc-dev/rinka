//! Shared graphics profile and surface conventions.
//!
//! The profile is the baseline both native wgpu and browser WebGPU must
//! provide. Projects are checked against it before playback.

use serde::Serialize;

/// Features every engine build requires. M0 needs none beyond core WebGPU.
pub fn required_features() -> wgpu::Features {
    wgpu::Features::empty()
}

/// Limits every engine build requires: the WebGPU defaults, which browsers
/// guarantee. `max_texture_dimension_2d` (8192) bounds per-output size.
pub fn required_limits() -> wgpu::Limits {
    wgpu::Limits::default()
}

#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("no suitable GPU adapter: {0}")]
    Adapter(#[from] wgpu::RequestAdapterError),
    #[error("adapter {adapter:?} lacks required features {missing:?}")]
    Features {
        adapter: String,
        missing: wgpu::Features,
    },
    #[error("could not create device: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
}

/// Requests a device restricted to the shared profile, so native builds cannot
/// silently depend on capabilities the browser preview lacks.
pub async fn request_device(
    adapter: &wgpu::Adapter,
) -> Result<(wgpu::Device, wgpu::Queue), GpuError> {
    let missing = required_features() - adapter.features();
    if !missing.is_empty() {
        return Err(GpuError::Features {
            adapter: adapter.get_info().name,
            missing,
        });
    }
    let desc = wgpu::DeviceDescriptor {
        label: Some("rinka"),
        required_features: required_features(),
        required_limits: required_limits(),
        ..Default::default()
    };
    Ok(adapter.request_device(&desc).await?)
}

/// Adapter description for diagnostics and baseline records.
#[derive(Clone, Debug, Serialize)]
pub struct GpuInfo {
    pub engine_version: &'static str,
    pub name: String,
    pub backend: String,
    pub device_type: String,
    pub driver: String,
    pub driver_info: String,
    pub vendor: u32,
    pub device: u32,
    pub features: Vec<String>,
    pub max_texture_dimension_2d: u32,
    pub required_features: Vec<String>,
}

impl GpuInfo {
    pub fn from_adapter(adapter: &wgpu::Adapter) -> Self {
        let info = adapter.get_info();
        let names = |f: wgpu::Features| {
            f.iter_names()
                .map(|(n, _)| n.to_owned())
                .collect::<Vec<_>>()
        };
        Self {
            engine_version: crate::ENGINE_VERSION,
            name: info.name,
            backend: format!("{:?}", info.backend),
            device_type: format!("{:?}", info.device_type),
            driver: info.driver,
            driver_info: info.driver_info,
            vendor: info.vendor,
            device: info.device,
            features: names(adapter.features()),
            max_texture_dimension_2d: adapter.limits().max_texture_dimension_2d,
            required_features: names(required_features()),
        }
    }
}

/// How a host configures a presentation surface and which view format the
/// renderer draws through.
#[derive(Clone, Debug)]
pub struct SurfaceSetup {
    pub config: wgpu::SurfaceConfiguration,
    /// Always an sRGB format: the engine writes linear values and relies on
    /// the view to encode them (see [`crate::color`]).
    pub render_format: wgpu::TextureFormat,
}

impl SurfaceSetup {
    /// Picks an 8-bit format, presenting through an sRGB view, an opaque alpha
    /// mode when available and vsync. Returns `None` when the surface offers
    /// no usable format.
    pub fn new(caps: &wgpu::SurfaceCapabilities, width: u32, height: u32) -> Option<Self> {
        use wgpu::TextureFormat as F;
        let eight_bit = |f: &&F| matches!(f.remove_srgb_suffix(), F::Bgra8Unorm | F::Rgba8Unorm);
        // Prefer a non-sRGB storage format with an sRGB view: that is what
        // browser canvases allow, so native and preview share one path.
        let format = caps
            .formats
            .iter()
            .filter(eight_bit)
            .min_by_key(|f| f.is_srgb())
            .copied()?;
        let render_format = format.add_srgb_suffix();
        // Frames are viewed both as sRGB and as stored (see `FrameViews`).
        let view_formats = if format.is_srgb() {
            vec![format.remove_srgb_suffix()]
        } else {
            vec![render_format]
        };
        let alpha_mode = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
            wgpu::CompositeAlphaMode::Opaque
        } else {
            caps.alpha_modes.first().copied().unwrap_or_default()
        };
        Some(Self {
            config: wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                color_space: wgpu::SurfaceColorSpace::Srgb,
                width: width.max(1),
                height: height.max(1),
                present_mode: wgpu::PresentMode::Fifo,
                desired_maximum_frame_latency: 2,
                alpha_mode,
                view_formats,
            },
            render_format,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
    }

    /// The view the renderer must draw into for a texture of this surface.
    pub fn view(&self, texture: &wgpu::Texture) -> wgpu::TextureView {
        texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.render_format),
            ..Default::default()
        })
    }
}

/// Views of one frame's render target. Both view the same texture.
pub struct FrameViews<'a> {
    /// sRGB view: linear-light composition (the engine default).
    pub srgb: &'a wgpu::TextureView,
    /// Storage-format view: values are written as-is (gamma-space scenes).
    pub raw: &'a wgpu::TextureView,
}

/// Result of [`SurfaceSetup::draw`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameOutcome {
    Presented,
    /// The presentation engine had no texture available (timeout or
    /// occlusion). The host should draw again on its next trigger.
    Skipped,
    /// The surface must be recreated before drawing again.
    Lost,
}

impl SurfaceSetup {
    pub fn configure(&self, surface: &wgpu::Surface<'_>, device: &wgpu::Device) {
        surface.configure(device, &self.config);
    }

    /// Acquires a texture, lets `render` record a frame into it and presents
    /// it, reconfiguring once if the surface reports itself outdated.
    /// `render` receives the encoder, the frame's views and its size.
    pub fn draw(
        &self,
        surface: &wgpu::Surface<'_>,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        render: impl FnOnce(&mut wgpu::CommandEncoder, &FrameViews<'_>, (u32, u32)),
    ) -> FrameOutcome {
        let mut reconfigured = false;
        let texture = loop {
            match surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(t) => break t,
                wgpu::CurrentSurfaceTexture::Suboptimal(t) if reconfigured => break t,
                wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                    drop(t);
                    self.configure(surface, device);
                    reconfigured = true;
                }
                wgpu::CurrentSurfaceTexture::Outdated if !reconfigured => {
                    self.configure(surface, device);
                    reconfigured = true;
                }
                wgpu::CurrentSurfaceTexture::Timeout
                | wgpu::CurrentSurfaceTexture::Occluded
                | wgpu::CurrentSurfaceTexture::Outdated => return FrameOutcome::Skipped,
                wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Validation => {
                    return FrameOutcome::Lost;
                }
            }
        };
        let srgb = self.view(&texture.texture);
        let raw = texture.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.render_format.remove_srgb_suffix()),
            ..Default::default()
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rinka frame"),
        });
        render(
            &mut encoder,
            &FrameViews {
                srgb: &srgb,
                raw: &raw,
            },
            (self.config.width, self.config.height),
        );
        queue.submit([encoder.finish()]);
        queue.present(texture);
        FrameOutcome::Presented
    }
}
