//! Native preview window.
//!
//! Renders a project in an ordinary desktop window through the same engine
//! path as wallpaper playback. Used to exercise the engine on platforms whose
//! wallpaper presentation is not implemented yet (Windows in M0).

use std::sync::Arc;

use rinka_engine::{FrameOutcome, GpuInfo, Project, Renderer, Source, SurfaceSetup, Target, gpu};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

/// A project with its image asset decoded, if it has one.
pub struct Loaded {
    pub project: Project,
    pub image: Option<rinka_engine::ImageData>,
}

pub fn run(loaded: Loaded) -> anyhow::Result<()> {
    let event_loop = EventLoop::new()?;
    let mut app = App {
        loaded,
        state: None,
        error: None,
    };
    event_loop.run_app(&mut app)?;
    match app.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

struct App {
    loaded: Loaded,
    state: Option<State>,
    error: Option<anyhow::Error>,
}

struct State {
    surface: wgpu::Surface<'static>,
    window: Arc<Window>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    setup: SurfaceSetup,
    renderer: Renderer,
    target: Target,
}

impl State {
    fn new(event_loop: &ActiveEventLoop, loaded: &Loaded) -> anyhow::Result<Self> {
        let project = &loaded.project;
        let attrs = Window::default_attributes()
            .with_title(format!("Rinka preview: {}", project.title))
            .with_inner_size(winit::dpi::LogicalSize::new(960, 540));
        let window = Arc::new(event_loop.create_window(attrs)?);
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(
                Box::new(event_loop.owned_display_handle()),
            ));
        let surface = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))?;
        log::info!(
            "{}",
            serde_json::to_string(&GpuInfo::from_adapter(&adapter))?
        );
        let (device, queue) = pollster::block_on(gpu::request_device(&adapter))?;
        let size = window.inner_size();
        let setup = SurfaceSetup::new(&surface.get_capabilities(&adapter), size.width, size.height)
            .ok_or_else(|| anyhow::anyhow!("surface offers no supported 8-bit format"))?;
        setup.configure(&surface, &device);
        let renderer = Renderer::new(&device, &queue, setup.render_format);
        let image = loaded
            .image
            .as_ref()
            .map(|data| renderer.upload_image(&device, &queue, data));
        let target = renderer.create_target(&device, project, image.as_ref().map(Source::Image));
        Ok(Self {
            surface,
            window,
            device,
            queue,
            setup,
            renderer,
            target,
        })
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        match State::new(event_loop, &self.loaded) {
            Ok(state) => self.state = Some(state),
            Err(e) => {
                self.error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(s) = &mut self.state else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                s.setup.resize(size.width, size.height);
                s.setup.configure(&s.surface, &s.device);
                s.window.request_redraw();
            }
            WindowEvent::RedrawRequested
                if s.setup
                    .draw(&s.surface, &s.device, &s.queue, |encoder, views, size| {
                        let frame = rinka_engine::Frame {
                            views,
                            size,
                            output_size: size,
                            time: 0.0,
                        };
                        s.target.render(&s.renderer, &s.queue, encoder, &frame);
                    })
                    == FrameOutcome::Lost =>
            {
                self.error = Some(anyhow::anyhow!("preview surface lost"));
                event_loop.exit();
            }
            _ => {}
        }
    }
}
