//! WebGPU editor preview.
//!
//! Wraps the shared engine for a canvas. The host page owns the canvas size
//! and calls [`Preview::render`] when something changed; there is no frame
//! loop for static content.
//!
//! Only meaningful on `wasm32`; native workspace builds compile it empty.

#![cfg(target_arch = "wasm32")]

use std::collections::HashMap;
use wasm_bindgen::prelude::*;
use web_sys::{HtmlCanvasElement, HtmlVideoElement};

use rinka_engine::{
    ColorMatrix, ColorRange, Content, FrameOutcome, GpuImage, GpuInfo, ImageData, Project,
    Renderer, Source, SurfaceSetup, Target, VideoTexture, YuvFrame, gpu, image::PixelFormat,
};

#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
}

#[wasm_bindgen(js_name = engineVersion)]
pub fn engine_version() -> String {
    rinka_engine::ENGINE_VERSION.to_owned()
}

/// Validates a manifest and returns the asset paths it depends on.
#[wasm_bindgen(js_name = projectDependencies)]
pub fn project_dependencies(json: &str) -> Result<Vec<String>, JsError> {
    let project = Project::from_json(json).map_err(to_js)?;
    Ok(project
        .dependencies()
        .into_iter()
        .map(str::to_owned)
        .collect())
}

/// Decodes an image for the preview. Meant to run in a worker so large
/// images never block the page; the result is moved to the preview with
/// [`Preview::add_decoded_asset`].
#[wasm_bindgen(js_name = decodeImage)]
pub fn decode_image(bytes: &[u8], max_dimension: u32) -> Result<DecodedImage, JsError> {
    rinka_engine::image::decode_with_limit(bytes, max_dimension)
        .map(DecodedImage)
        .map_err(to_js)
}

/// Decodes without color conversion, for gamma-space scene textures.
#[wasm_bindgen(js_name = decodeImageRaw)]
pub fn decode_image_raw(bytes: &[u8], max_dimension: u32) -> Result<DecodedImage, JsError> {
    rinka_engine::image::decode_raw(bytes, max_dimension)
        .map(DecodedImage)
        .map_err(to_js)
}

#[wasm_bindgen]
pub struct DecodedImage(ImageData);

#[wasm_bindgen]
impl DecodedImage {
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.0.width
    }

    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.0.height
    }

    #[wasm_bindgen(getter, js_name = sourceWidth)]
    pub fn source_width(&self) -> u32 {
        self.0.source_size.0
    }

    #[wasm_bindgen(getter, js_name = sourceHeight)]
    pub fn source_height(&self) -> u32 {
        self.0.source_size.1
    }

    /// 0: sRGB RGBA8; 1: linear premultiplied RGBA16F.
    #[wasm_bindgen(getter)]
    pub fn format(&self) -> u32 {
        match self.0.format {
            PixelFormat::Srgba8 => 0,
            PixelFormat::LinearPremulF16 => 1,
            PixelFormat::Rgba8Raw => 2,
        }
    }

    /// Moves the pixels out as a `Uint8Array`.
    #[wasm_bindgen(js_name = takePixels)]
    pub fn take_pixels(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.0.pixels)
    }
}

/// A decoded or uploaded image asset, keyed by the page.
enum Asset {
    Decoded(ImageData),
    Uploaded(GpuImage),
}

#[wasm_bindgen]
pub struct Preview {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    setup: SurfaceSetup,
    renderer: Renderer,
    target: Target,
    info: GpuInfo,
    assets: HashMap<String, Asset>,
    /// The project shown, kept to rebind when the video frame size changes.
    project: Option<Project>,
    /// Texture the current `<video>` frame is copied into.
    video_frame: Option<(wgpu::Texture, GpuImage)>,
    /// Planes of the current WebCodecs frame.
    video_yuv: Option<VideoTexture>,
    /// Scene shader sources by key.
    shader_sources: HashMap<String, String>,
}

#[wasm_bindgen]
impl Preview {
    /// Requires hardware-backed WebGPU; there is no WebGL fallback.
    pub async fn create(canvas: HtmlCanvasElement) -> Result<Preview, JsError> {
        let (width, height) = (canvas.width(), canvas.height());
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::BROWSER_WEBGPU,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
            .map_err(to_js)?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .map_err(|e| JsError::new(&format!("WebGPU unavailable: {e}")))?;
        let info = GpuInfo::from_adapter(&adapter);
        if adapter.get_info().device_type == wgpu::DeviceType::Cpu {
            return Err(JsError::new(
                "hardware-backed WebGPU is required, but only a software fallback adapter is available",
            ));
        }
        let (device, queue) = gpu::request_device(&adapter).await.map_err(to_js)?;
        let setup = SurfaceSetup::new(&surface.get_capabilities(&adapter), width, height)
            .ok_or_else(|| JsError::new("canvas offers no supported 8-bit format"))?;
        setup.configure(&surface, &device);
        let renderer = Renderer::new(&device, &queue, setup.render_format);
        let target = renderer.create_target(
            &device,
            &Project::solid_color("", rinka_engine::Srgb8::BLACK),
            None,
        );
        Ok(Preview {
            surface,
            device,
            queue,
            setup,
            renderer,
            target,
            info,
            assets: HashMap::new(),
            project: None,
            video_frame: None,
            video_yuv: None,
            shader_sources: HashMap::new(),
        })
    }

    /// Whether an asset with this key was already added.
    #[wasm_bindgen(js_name = hasAsset)]
    pub fn has_asset(&self, key: &str) -> bool {
        self.assets.contains_key(key)
    }

    /// Decodes an image asset and keeps it under `key`. Keys are chosen by
    /// the page and should change when the file changes.
    #[wasm_bindgen(js_name = addAsset)]
    pub fn add_asset(&mut self, key: &str, bytes: &[u8]) -> Result<(), JsError> {
        let data = rinka_engine::image::decode(bytes).map_err(to_js)?;
        self.assets.insert(key.to_owned(), Asset::Decoded(data));
        Ok(())
    }

    /// Adds an asset decoded by [`decode_image`], typically in a worker.
    #[wasm_bindgen(js_name = addDecodedAsset)]
    #[allow(clippy::too_many_arguments)]
    pub fn add_decoded_asset(
        &mut self,
        key: &str,
        width: u32,
        height: u32,
        source_width: u32,
        source_height: u32,
        format: u32,
        pixels: Vec<u8>,
    ) -> Result<(), JsError> {
        let format = match format {
            0 => PixelFormat::Srgba8,
            1 => PixelFormat::LinearPremulF16,
            2 => PixelFormat::Rgba8Raw,
            _ => return Err(JsError::new("unknown pixel format")),
        };
        let expected = width as usize * height as usize * format.bytes_per_pixel() as usize;
        if pixels.len() != expected {
            return Err(JsError::new("pixel data does not match the image size"));
        }
        let data = ImageData {
            width,
            height,
            format,
            pixels,
            source_size: (source_width, source_height),
        };
        self.assets.insert(key.to_owned(), Asset::Decoded(data));
        Ok(())
    }

    /// Adds a scene shader source under `key`.
    #[wasm_bindgen(js_name = addShaderSource)]
    pub fn add_shader_source(&mut self, key: &str, source: String) {
        self.shader_sources.insert(key.to_owned(), source);
    }

    /// Whether the shown project changes over time (an animated scene).
    #[wasm_bindgen(js_name = isAnimated)]
    pub fn is_animated(&self) -> bool {
        self.target.animated()
    }

    /// Drops every asset whose key is not listed.
    #[wasm_bindgen(js_name = retainAssets)]
    pub fn retain_assets(&mut self, keys: Vec<String>) {
        self.assets.retain(|k, _| keys.contains(k));
    }

    /// Shows a project. Each dependency `path` is looked up as
    /// `asset_prefix + path`; add assets first. Call [`Preview::render`]
    /// afterwards.
    #[wasm_bindgen(js_name = setProject)]
    pub fn set_project(&mut self, json: &str, asset_prefix: &str) -> Result<(), JsError> {
        let project = Project::from_json(json).map_err(to_js)?;
        if let Content::Scene(content) = &project.content {
            // Scene assets move into the GPU scene; the page adds them again
            // the next time the scene is opened.
            let mut assets = rinka_engine::SceneAssets::default();
            for path in content.textures.keys() {
                let key = format!("{asset_prefix}{path}");
                match self.assets.remove(&key) {
                    Some(Asset::Decoded(data)) => {
                        assets.textures.insert(path.clone(), data);
                    }
                    _ => return Err(JsError::new(&format!("scene texture {key} was not added"))),
                }
            }
            for shader in content.shaders.values() {
                for path in [&shader.vertex, &shader.fragment] {
                    let key = format!("{asset_prefix}{path}");
                    let source = self.shader_sources.remove(&key).ok_or_else(|| {
                        JsError::new(&format!("scene shader {key} was not added"))
                    })?;
                    assets.shaders.insert(path.clone(), source);
                }
            }
            self.target
                .set_project(&self.device, &self.renderer, &project, None);
            let scene = rinka_engine::GpuScene::new(
                &self.device,
                &self.queue,
                &self.renderer,
                content,
                &assets,
                self.setup.config.format,
            )
            .map_err(to_js)?;
            self.target.set_scene(Some(scene));
            self.project = Some(project);
            return Ok(());
        }
        let image = match &project.content {
            Content::Image { source, .. } => {
                let key = format!("{asset_prefix}{source}");
                let asset = self
                    .assets
                    .get_mut(&key)
                    .ok_or_else(|| JsError::new(&format!("asset {key} was not added")))?;
                if let Asset::Decoded(data) = asset {
                    *asset = Asset::Uploaded(self.renderer.upload_image(
                        &self.device,
                        &self.queue,
                        data,
                    ));
                }
                match asset {
                    Asset::Uploaded(image) => Some(image.clone()),
                    Asset::Decoded(_) => unreachable!(),
                }
            }
            Content::Video { .. } => self.video_frame.as_ref().map(|(_, image)| image.clone()),
            // Scenes are not previewed yet; the canvas shows black.
            Content::Color { .. } | Content::Scene(_) => None,
        };
        let source = match (&project.content, &self.video_yuv) {
            (Content::Video { .. }, Some(yuv)) => Some(Source::Yuv(yuv)),
            _ => image.as_ref().map(Source::Image),
        };
        self.target
            .set_project(&self.device, &self.renderer, &project, source);
        self.project = Some(project);
        Ok(())
    }

    /// Copies the `<video>` element's current frame into the engine. The
    /// browser decodes and converts the frame to sRGB; placement, background
    /// and composition stay the engine's. Call [`Preview::render`] after.
    #[wasm_bindgen(js_name = updateVideoFrame)]
    pub fn update_video_frame(&mut self, video: HtmlVideoElement) {
        let (width, height) = (video.video_width(), video.video_height());
        if width == 0 || height == 0 {
            return;
        }
        let resized = self
            .video_frame
            .as_ref()
            .is_none_or(|(_, image)| (image.width, image.height) != (width, height));
        if resized {
            self.video_frame = Some(
                self.renderer
                    .create_frame_image(&self.device, width, height),
            );
        }
        let (texture, image) = self.video_frame.as_ref().expect("frame texture");
        self.queue.copy_external_image_to_texture(
            &wgpu::CopyExternalImageSourceInfo {
                source: wgpu::ExternalImageSource::HTMLVideoElement(video),
                origin: wgpu::Origin2d::ZERO,
                flip_y: false,
            },
            wgpu::CopyExternalImageDestInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
                color_space: wgpu::PredefinedColorSpace::Srgb,
                premultiplied_alpha: false,
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        if resized
            && let Some(project) = &self.project
            && matches!(project.content, Content::Video { .. })
        {
            let image = image.clone();
            self.target.set_project(
                &self.device,
                &self.renderer,
                project,
                Some(Source::Image(&image)),
            );
        }
    }

    /// Uploads a decoded Y'CbCr frame (as laid out by `VideoFrame.copyTo`)
    /// for the engine to convert, the same way native playback does.
    /// `matrix`: 0 BT.601, 1 BT.709, 2 BT.2020; anything else means
    /// unspecified and follows the engine's guess.
    #[wasm_bindgen(js_name = uploadYuvFrame)]
    #[allow(clippy::too_many_arguments)]
    pub fn upload_yuv_frame(
        &mut self,
        width: u32,
        height: u32,
        chroma_width: u32,
        chroma_height: u32,
        matrix: u32,
        full_range: bool,
        data: &[u8],
        y_offset: u32,
        y_stride: u32,
        u_offset: u32,
        u_stride: u32,
        v_offset: u32,
        v_stride: u32,
    ) -> Result<(), JsError> {
        let plane = |offset: u32, stride: u32, rows: u32| {
            let start = offset as usize;
            let end = start + stride as usize * rows as usize;
            data.get(start..end)
                .ok_or_else(|| JsError::new("frame planes exceed the buffer"))
        };
        let frame = YuvFrame {
            width,
            height,
            chroma_width,
            chroma_height,
            y: plane(y_offset, y_stride, height)?,
            y_stride,
            u: plane(u_offset, u_stride, chroma_height)?,
            u_stride,
            v: plane(v_offset, v_stride, chroma_height)?,
            v_stride,
            matrix: match matrix {
                0 => ColorMatrix::Bt601,
                1 => ColorMatrix::Bt709,
                2 => ColorMatrix::Bt2020Ncl,
                _ => ColorMatrix::guess(height),
            },
            range: if full_range {
                ColorRange::Full
            } else {
                ColorRange::Limited
            },
        };
        let recreate = self.video_yuv.as_ref().is_none_or(|v| !v.fits(&frame));
        if recreate {
            self.video_yuv = Some(self.renderer.create_video_texture(&self.device, &frame));
        }
        let video = self.video_yuv.as_mut().expect("video texture");
        video.upload(&self.queue, &frame);
        if recreate
            && let Some(project) = &self.project
            && matches!(project.content, Content::Video { .. })
        {
            self.target.set_project(
                &self.device,
                &self.renderer,
                project,
                Some(Source::Yuv(video)),
            );
        }
        self.target.set_video_color(video.matrix, video.range);
        Ok(())
    }

    /// Releases the video frame textures.
    #[wasm_bindgen(js_name = clearVideoFrame)]
    pub fn clear_video_frame(&mut self) {
        self.video_frame = None;
        self.video_yuv = None;
    }

    /// Resizes the drawing buffer, in device pixels.
    pub fn resize(&mut self, width: u32, height: u32) {
        if (width, height) != (self.setup.config.width, self.setup.config.height) {
            self.setup.resize(width, height);
            self.setup.configure(&self.surface, &self.device);
        }
    }

    /// Draws the project as it would appear on an output of
    /// `output_width`×`output_height` pixels, scaled to the canvas.
    /// `time` is the scene clock in seconds; other content ignores it.
    pub fn render(
        &mut self,
        output_width: u32,
        output_height: u32,
        time: f64,
    ) -> Result<(), JsError> {
        let output = (output_width.max(1), output_height.max(1));
        let (renderer, queue, target) = (&self.renderer, &self.queue, &mut self.target);
        let outcome = self.setup.draw(
            &self.surface,
            &self.device,
            queue,
            |encoder, views, size| {
                let frame = rinka_engine::Frame {
                    views,
                    size,
                    output_size: output,
                    time,
                };
                target.render(renderer, queue, encoder, &frame);
            },
        );
        match outcome {
            FrameOutcome::Presented | FrameOutcome::Skipped => Ok(()),
            FrameOutcome::Lost => Err(JsError::new("WebGPU canvas context lost")),
        }
    }

    /// Adapter and profile description as JSON.
    #[wasm_bindgen(js_name = gpuInfo)]
    pub fn gpu_info(&self) -> String {
        serde_json::to_string(&self.info).expect("info serializes")
    }

    /// The canvas format and the sRGB view format the engine renders through.
    #[wasm_bindgen(js_name = surfaceFormat)]
    pub fn surface_format(&self) -> String {
        format!(
            "{:?} via {:?}",
            self.setup.config.format, self.setup.render_format
        )
    }
}

fn to_js(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}
