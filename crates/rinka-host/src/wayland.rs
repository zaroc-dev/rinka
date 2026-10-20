//! Wayland presentation through wlr-layer-shell background surfaces.
//!
//! One layer surface per assigned output, anchored to all edges, with no
//! exclusive zone, keyboard focus or input region. Static content is drawn
//! only when a surface is configured, rescaled or its project changes.
//!
//! Video wallpapers are driven by a loop timer that wakes when the next frame
//! is due. Each video present requests a frame callback and the next draw
//! waits for it, so drawing follows the compositor's pace. A surface whose
//! callback does not arrive (hidden or powered-off output) pauses its video
//! clock until the compositor asks for frames again.
//!
//! Under niri each wallpaper has a second surface, a blurred and dimmed still
//! for the overview backdrop (see [`Backdrop`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use anyhow::Context;
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use rinka_engine::{
    BackdropRenderer, FrameOutcome, GpuInfo, Project, Renderer, Source, SurfaceSetup, VideoTexture,
    backdrop, gpu,
};
use serde_json::{Value, json};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData, Region},
    delegate_registry,
    dispatch2::Dispatch2,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop, LoopHandle, LoopSignal, channel,
            timer::{TimeoutAction, Timer},
        },
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, Proxy, QueueHandle,
            globals::registry_queue_init,
            protocol::{wl_output, wl_surface},
        },
        protocols::wp::{
            fractional_scale::v1::client::{
                wp_fractional_scale_manager_v1, wp_fractional_scale_v1,
            },
            viewporter::client::{wp_viewport, wp_viewporter},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
};

use crate::clock::SceneClock;
use crate::control::{self, Command};
use crate::project_io::{self, ImageCache};
use crate::protocol::{Error, ErrorCode, Reply, Request};
use crate::video::{OwnedFrame, Player};

/// Frame rate of animated scenes.
const SCENE_FPS: f64 = 30.0;

/// A video surface whose frame callback has not arrived for this long is
/// treated as hidden and its clock pauses.
const HIDDEN_AFTER: Duration = Duration::from_millis(500);

/// Fractional scale denominator defined by wp-fractional-scale-v1.
const SCALE_DENOMINATOR: u32 = 120;

/// Runs the host. `assignments` (output name → project directory) are shown
/// as their outputs appear.
pub fn run(endpoint: PathBuf, assignments: HashMap<String, PathBuf>) -> anyhow::Result<()> {
    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let (globals, mut queue) = registry_queue_init(&conn).context("reading Wayland globals")?;
    let qh = queue.handle();

    let mut event_loop: EventLoop<Host> = EventLoop::try_new()?;
    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor unavailable")?;
    let layer_shell =
        LayerShell::bind(&globals, &qh).context("compositor does not support wlr-layer-shell")?;
    // Fractional scaling needs both protocols: the preferred scale and a
    // viewport to map the larger buffer back to logical size.
    let fractional = globals
        .bind::<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, _, _>(
            &qh,
            1..=1,
            NoEvents,
        )
        .ok();
    let viewporter = globals
        .bind::<wp_viewporter::WpViewporter, _, _>(&qh, 1..=1, NoEvents)
        .ok();
    let scaling = match (fractional, viewporter) {
        (Some(f), Some(v)) => Scaling::Fractional(f, v),
        _ => Scaling::Integer,
    };

    let (tx, rx) = channel::channel::<Command>();
    event_loop
        .handle()
        .insert_source(rx, |event, _, host: &mut Host| {
            if let channel::Event::Msg(cmd) = event {
                let shutdown = matches!(cmd.request, Request::Shutdown);
                let reply = host.handle(cmd.request);
                let _ = cmd.reply.send(reply);
                if shutdown {
                    cmd.delivered.wait();
                }
            }
        })
        .map_err(|e| anyhow::anyhow!("inserting control channel: {e}"))?;

    let niri = crate::niri::socket();
    if let Some(socket) = niri.clone() {
        let (tx, rx) = channel::channel::<()>();
        event_loop
            .handle()
            .insert_source(rx, |event, _, host: &mut Host| {
                if let channel::Event::Msg(()) = event {
                    host.overview_opened();
                }
            })
            .map_err(|e| anyhow::anyhow!("inserting niri channel: {e}"))?;
        crate::niri::watch_overview(socket, move || tx.send(()).is_ok());
    }

    let mut host = Host {
        registry: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        scaling,
        conn,
        qh,
        instance: wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
        ),
        gpu: None,
        images: ImageCache::default(),
        loop_handle: event_loop.handle(),
        video_timer: false,
        outputs: Vec::new(),
        assignments,
        paused: false,
        backdrops: niri.is_some(),
        stop: event_loop.get_signal(),
    };
    // Learn the outputs and their names before accepting requests.
    queue.roundtrip(&mut host).context("Wayland roundtrip")?;
    queue.roundtrip(&mut host).context("Wayland roundtrip")?;
    WaylandSource::new(host.conn.clone(), queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("inserting Wayland source: {e}"))?;

    let listener = control::bind(&endpoint)?;
    log::info!("control socket: {}", endpoint.display());
    control::serve(listener, move |cmd| tx.send(cmd).is_ok());

    let result = event_loop.run(None, &mut host, |_| {});
    let _ = std::fs::remove_file(&endpoint);
    drop(host);
    result.context("event loop failed")
}

enum Scaling {
    Fractional(
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        wp_viewporter::WpViewporter,
    ),
    Integer,
}

struct Gpu {
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// One per surface format in use; normally a single entry.
    renderers: Vec<(wgpu::TextureFormat, Renderer)>,
    /// Likewise for backdrop surfaces.
    backdrops: Vec<(wgpu::TextureFormat, BackdropRenderer)>,
}

impl Gpu {
    /// Index into `renderers` for `format`, creating the renderer on first
    /// use. Returns an index so callers can borrow the device alongside it.
    fn renderer_index(&mut self, format: wgpu::TextureFormat) -> usize {
        match self.renderers.iter().position(|(f, _)| *f == format) {
            Some(i) => i,
            None => {
                let renderer = Renderer::new(&self.device, &self.queue, format);
                self.renderers.push((format, renderer));
                self.renderers.len() - 1
            }
        }
    }

    fn backdrop_index(&mut self, format: wgpu::TextureFormat) -> usize {
        match self.backdrops.iter().position(|(f, _)| *f == format) {
            Some(i) => i,
            None => {
                let renderer = BackdropRenderer::new(&self.device, format);
                self.backdrops.push((format, renderer));
                self.backdrops.len() - 1
            }
        }
    }
}

struct Host {
    registry: RegistryState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    scaling: Scaling,
    conn: Connection,
    qh: QueueHandle<Host>,
    instance: wgpu::Instance,
    gpu: Option<Gpu>,
    images: ImageCache,
    loop_handle: LoopHandle<'static, Host>,
    /// Whether the video loop timer is registered.
    video_timer: bool,
    outputs: Vec<OutputSlot>,
    /// Desired output name -> project directory. Survives disconnection, so a
    /// reconnected output gets its wallpaper back.
    assignments: HashMap<String, PathBuf>,
    paused: bool,
    /// Wallpapers get overview backdrop surfaces (under niri).
    backdrops: bool,
    stop: LoopSignal,
}

struct OutputSlot {
    wl: wl_output::WlOutput,
    wallpaper: Option<Wallpaper>,
}

struct Wallpaper {
    project_dir: PathBuf,
    project: Project,
    /// Absolute path of the image asset, when the project shows one.
    image: Option<PathBuf>,
    /// Absolute path of the video asset, when the project shows one.
    video: Option<PathBuf>,
    /// Loaded assets of a scene project.
    scene: Option<std::sync::Arc<rinka_engine::SceneAssets>>,
    /// The scene's clock; it stops while animation is paused.
    scene_clock: SceneClock,
    /// The scene animates and the next frame is due at this instant.
    next_frame: Option<Instant>,
    player: Option<Player>,
    video_texture: Option<VideoTexture>,
    /// The newest due frame, waiting for the next draw.
    video_frame: Option<OwnedFrame>,
    /// When the pending frame callback was requested.
    frame_pending: Option<Instant>,
    /// The video clock is paused because the surface looks hidden.
    hidden: bool,
    /// The project changed since the target was last updated.
    dirty: bool,
    backdrop: Option<Backdrop>,
    // Field order matters: the wgpu surface must drop before the wl_surface
    // owned by `layer`.
    target: Option<Target>,
    viewport: Option<wp_viewport::WpViewport>,
    fractional: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
    layer: LayerSurface,
    /// Size in surface-local (logical) coordinates; zero until configured.
    logical: (u32, u32),
    /// Preferred fractional scale in 120ths.
    scale_120: u32,
    /// Integer buffer scale, used without fractional-scale support.
    scale_int: u32,
}

struct Target {
    surface: wgpu::Surface<'static>,
    setup: SurfaceSetup,
    target: rinka_engine::Target,
}

/// A blurred, dimmed still of a wallpaper on its own background surface,
/// for niri's overview backdrop. niri moves it there only when a layer rule
/// with `place-within-backdrop` matches its namespace; otherwise it stacks
/// below the wallpaper, unseen.
///
/// The still is drawn when the wallpaper's content or size changes and, for
/// animated wallpapers, each time the overview opens.
struct Backdrop {
    // Field order matters: the wgpu surface must drop before `layer`.
    target: Option<BackdropTarget>,
    viewport: Option<wp_viewport::WpViewport>,
    layer: LayerSurface,
    /// Size in surface-local (logical) coordinates; zero until configured.
    logical: (u32, u32),
    /// The wallpaper buffer size the current still was drawn for; `None`
    /// when it needs drawing.
    drawn_for: Option<(u32, u32)>,
}

struct BackdropTarget {
    surface: wgpu::Surface<'static>,
    setup: SurfaceSetup,
}

impl Drop for Backdrop {
    fn drop(&mut self) {
        self.target = None;
        if let Some(v) = self.viewport.take() {
            v.destroy();
        }
    }
}

/// A loaded project with its assets decoded or probed, ready to show.
struct Prepared {
    dir: PathBuf,
    project: Project,
    image: Option<PathBuf>,
    video: Option<PathBuf>,
    scene: Option<std::sync::Arc<rinka_engine::SceneAssets>>,
}

impl Drop for Wallpaper {
    fn drop(&mut self) {
        self.target = None;
        if let Some(v) = self.viewport.take() {
            v.destroy();
        }
        if let Some(f) = self.fractional.take() {
            f.destroy();
        }
    }
}

impl Wallpaper {
    fn buffer_size(&self) -> (u32, u32) {
        let (w, h) = self.logical;
        if self.viewport.is_some() {
            // Rounding half away from zero, as the protocol specifies.
            let scale = |v: u32| (v * self.scale_120 + SCALE_DENOMINATOR / 2) / SCALE_DENOMINATOR;
            (scale(w), scale(h))
        } else {
            (w * self.scale_int, h * self.scale_int)
        }
    }
}

impl Host {
    fn output_name(&self, wl: &wl_output::WlOutput) -> String {
        self.output_state
            .info(wl)
            .and_then(|i| i.name)
            .unwrap_or_else(|| format!("wl_output@{}", wl.id().protocol_id()))
    }

    fn slot_index(&self, name: &str) -> Option<usize> {
        self.outputs
            .iter()
            .position(|s| self.output_name(&s.wl) == name)
    }

    /// Loads a project and decodes its assets, so errors reach the client
    /// before anything on screen changes.
    fn prepare(&mut self, dir: &Path) -> Result<Prepared, Error> {
        let project = project_io::load(dir)?;
        let image = project_io::image_asset(dir, &project)?;
        if let Some(path) = &image {
            self.images.decode(path)?;
        }
        let video = project_io::video_asset(dir, &project)?;
        if let Some(path) = &video {
            crate::video::probe(path).map_err(|e| {
                Error::new(ErrorCode::Project, format!("{}: {e:#}", path.display()))
            })?;
        }
        let scene = project_io::scene_assets(dir, &project)?.map(std::sync::Arc::new);
        Ok(Prepared {
            dir: dir.to_owned(),
            project,
            image,
            video,
            scene,
        })
    }

    /// A full-output background surface with no exclusive zone, keyboard
    /// focus or input region.
    fn background_layer(&self, wl: &wl_output::WlOutput, namespace: &str) -> LayerSurface {
        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            surface,
            Layer::Background,
            Some(namespace),
            Some(wl),
        );
        layer.set_anchor(Anchor::all());
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(0, 0);
        // An empty input region lets pointer events fall through to the
        // desktop beneath.
        if let Ok(region) = Region::new(&self.compositor) {
            layer
                .wl_surface()
                .set_input_region(Some(region.wl_region()));
        }
        layer
    }

    fn create_wallpaper(&self, wl: &wl_output::WlOutput, prepared: Prepared) -> Wallpaper {
        // Created first: surfaces on one layer stack in creation order, so
        // without niri's backdrop rule the backdrop stays under the wallpaper.
        let backdrop = self.backdrops.then(|| {
            let layer = self.background_layer(wl, "rinka-backdrop");
            let viewport = match &self.scaling {
                Scaling::Fractional(_, v) => {
                    Some(v.get_viewport(layer.wl_surface(), &self.qh, NoEvents))
                }
                Scaling::Integer => None,
            };
            layer.commit();
            Backdrop {
                target: None,
                viewport,
                layer,
                logical: (0, 0),
                drawn_for: None,
            }
        });
        let layer = self.background_layer(wl, "rinka-wallpaper");
        let (fractional, viewport) = match &self.scaling {
            Scaling::Fractional(f, v) => (
                Some(f.get_fractional_scale(
                    layer.wl_surface(),
                    &self.qh,
                    FractionalScale(layer.wl_surface().clone()),
                )),
                Some(v.get_viewport(layer.wl_surface(), &self.qh, NoEvents)),
            ),
            Scaling::Integer => (None, None),
        };
        let scale_int = self
            .output_state
            .info(wl)
            .map_or(1, |i| i.scale_factor.max(1) as u32);
        layer.commit();
        Wallpaper {
            project_dir: prepared.dir,
            project: prepared.project,
            image: prepared.image,
            video: None,
            scene: prepared.scene,
            scene_clock: SceneClock::new(Instant::now(), self.paused),
            next_frame: None,
            player: None,
            video_texture: None,
            video_frame: None,
            frame_pending: None,
            hidden: false,
            dirty: false,
            backdrop,
            target: None,
            viewport,
            fractional,
            layer,
            logical: (0, 0),
            scale_120: SCALE_DENOMINATOR * scale_int,
            scale_int,
        }
    }

    /// Ensures the output shows its assigned project, creating, updating or
    /// removing its wallpaper surface.
    fn apply_assignment(&mut self, index: usize, prepared: Option<Prepared>) {
        match prepared {
            None => self.outputs[index].wallpaper = None,
            Some(prepared) => {
                let video = prepared.video.clone();
                let paused = self.paused;
                if let Some(w) = &mut self.outputs[index].wallpaper {
                    w.project_dir = prepared.dir;
                    w.project = prepared.project;
                    w.image = prepared.image;
                    if prepared.scene.is_some() || w.scene.is_some() {
                        w.scene_clock = SceneClock::new(Instant::now(), paused);
                    }
                    w.scene = prepared.scene;
                    w.dirty = true;
                } else {
                    let wl = self.outputs[index].wl.clone();
                    self.outputs[index].wallpaper = Some(self.create_wallpaper(&wl, prepared));
                }
                let w = self.outputs[index]
                    .wallpaper
                    .as_mut()
                    .expect("wallpaper exists");
                // A player whose decoder stopped is retried on reassignment.
                if w.video != video || (video.is_some() && w.player.is_none()) {
                    w.player = None;
                    w.video_texture = None;
                    w.video_frame = None;
                    w.player = video.as_deref().and_then(|path| match Player::open(path) {
                        Ok(mut player) => {
                            if self.paused {
                                player.pause(Instant::now());
                            }
                            Some(player)
                        }
                        Err(e) => {
                            log::error!("{}: {e:#}", path.display());
                            None
                        }
                    });
                    w.video = video;
                }
                self.draw(index);
                self.ensure_video_timer();
            }
        }
        let used: Vec<PathBuf> = self
            .outputs
            .iter()
            .filter_map(|s| s.wallpaper.as_ref()?.image.clone())
            .collect();
        self.images.retain(&used);
    }

    fn draw(&mut self, index: usize) {
        let Some(w) = self.outputs[index].wallpaper.as_mut() else {
            return;
        };
        if w.logical.0 == 0 || w.logical.1 == 0 {
            return; // Not configured yet.
        }
        let (width, height) = w.buffer_size();
        // Scale state is double-buffered and committed by `present`.
        match &w.viewport {
            Some(v) => v.set_destination(w.logical.0 as i32, w.logical.1 as i32),
            None => w.layer.wl_surface().set_buffer_scale(w.scale_int as i32),
        }

        if w.target.is_none() {
            // SAFETY: `Wallpaper` drops the target before its wl_surface.
            let surface =
                match unsafe { gpu_surface(&self.instance, &self.conn, w.layer.wl_surface()) } {
                    Ok(s) => s,
                    Err(e) => return log::error!("creating GPU surface: {e}"),
                };
            if self.gpu.is_none() {
                match init_gpu(&self.instance, Some(&surface)) {
                    Ok(gpu) => self.gpu = Some(gpu),
                    Err(e) => return log::error!("{e:#}"),
                }
            }
            let gpu = self.gpu.as_mut().expect("gpu initialized");
            let Some(setup) =
                SurfaceSetup::new(&surface.get_capabilities(&gpu.adapter), width, height)
            else {
                return log::error!("surface offers no supported 8-bit format");
            };
            setup.configure(&surface, &gpu.device);
            let renderer = gpu.renderer_index(setup.render_format);
            let renderer = &gpu.renderers[renderer].1;
            let target = renderer.create_target(&gpu.device, &w.project, None);
            w.target = Some(Target {
                surface,
                setup,
                target,
            });
            // Bind the project's image or video below.
            w.dirty = true;
        }
        let gpu = self.gpu.as_mut().expect("gpu initialized");
        let t = w.target.as_mut().expect("target created");
        let renderer = gpu.renderer_index(t.setup.render_format);
        let renderer = &gpu.renderers[renderer].1;
        if let Some(frame) = w.video_frame.take() {
            let yuv = frame.as_yuv();
            if w.video_texture.as_ref().is_none_or(|v| !v.fits(&yuv)) {
                w.video_texture = Some(renderer.create_video_texture(&gpu.device, &yuv));
                w.dirty = true;
            }
            let video = w.video_texture.as_mut().expect("video texture");
            video.upload(&gpu.queue, &yuv);
            t.target.set_video_color(video.matrix, video.range);
        }
        if w.dirty {
            let image = w
                .image
                .as_deref()
                .and_then(|p| self.images.gpu_image(p, renderer, &gpu.device, &gpu.queue));
            let source = match (&image, &w.video_texture) {
                (Some(image), _) => Some(Source::Image(image)),
                (None, Some(video)) if w.video.is_some() => Some(Source::Yuv(video)),
                _ => None,
            };
            t.target
                .set_project(&gpu.device, renderer, &w.project, source);
            if let (rinka_engine::Content::Scene(content), Some(assets)) =
                (&w.project.content, &w.scene)
            {
                match rinka_engine::GpuScene::new(
                    &gpu.device,
                    &gpu.queue,
                    renderer,
                    content,
                    assets,
                    t.setup.config.format,
                ) {
                    Ok(scene) => t.target.set_scene(Some(scene)),
                    Err(e) => log::error!("building scene: {e}"),
                }
            }
            w.next_frame = t.target.animated().then(Instant::now);
            w.dirty = false;
            if let Some(b) = &mut w.backdrop {
                b.drawn_for = None;
            }
        }
        let draw_start = Instant::now();
        let time = w.scene_clock.time(Instant::now());
        if let Some(next) = &mut w.next_frame {
            *next = Instant::now() + Duration::from_secs_f64(1.0 / SCENE_FPS);
        }
        if w.player.is_some() || w.next_frame.is_some() {
            // Pace animation by the compositor; committed by `present`.
            let surface = w.layer.wl_surface();
            surface.frame(&self.qh, FrameCallbackData(surface.clone()));
            w.frame_pending = Some(Instant::now());
        }
        if (t.setup.config.width, t.setup.config.height) != (width, height) {
            t.setup.resize(width, height);
            t.setup.configure(&t.surface, &gpu.device);
        }
        let outcome = t.setup.draw(
            &t.surface,
            &gpu.device,
            &gpu.queue,
            |encoder, views, size| {
                let frame = rinka_engine::Frame {
                    views,
                    size,
                    output_size: size,
                    time,
                };
                t.target.render(renderer, &gpu.queue, encoder, &frame);
            },
        );
        match outcome {
            FrameOutcome::Presented => {
                log::debug!("presented {width}x{height} in {:?}", draw_start.elapsed());
            }
            FrameOutcome::Skipped => log::debug!("frame skipped"),
            FrameOutcome::Lost => {
                log::warn!("surface lost; recreating on next draw");
                w.target = None;
            }
        }
        let backdrop_stale = w
            .backdrop
            .as_ref()
            .is_some_and(|b| b.drawn_for != Some((width, height)));
        // A scene learns it animates on its first draw, which happens after
        // the assignment that would otherwise start the timer.
        if w.next_frame.is_some() {
            self.ensure_video_timer();
        }
        if backdrop_stale {
            // The wallpaper's time: particle systems replay from the start
            // when drawn at an earlier time than before.
            self.draw_backdrop(index, time);
        }
    }

    /// Draws the backdrop still of output `index` from its wallpaper at
    /// scene time `time`, in a submission of its own.
    fn draw_backdrop(&mut self, index: usize, time: f64) {
        let Some(w) = self.outputs[index].wallpaper.as_mut() else {
            return;
        };
        let output_size = w.buffer_size();
        let (Some(b), Some(t), Some(gpu)) = (&mut w.backdrop, &mut w.target, &mut self.gpu) else {
            return; // The wallpaper draws it once it has a target.
        };
        if b.logical.0 == 0 || b.logical.1 == 0 {
            return; // Not configured yet.
        }
        // With a viewport the compositor scales the small still up; without
        // one the still is sampled up to the logical size.
        let (width, height) = match &b.viewport {
            Some(v) => {
                v.set_destination(b.logical.0 as i32, b.logical.1 as i32);
                backdrop::still_size(output_size)
            }
            None => b.logical,
        };
        if b.target.is_none() {
            // SAFETY: `Backdrop` drops the target before its wl_surface.
            let surface =
                match unsafe { gpu_surface(&self.instance, &self.conn, b.layer.wl_surface()) } {
                    Ok(s) => s,
                    Err(e) => return log::error!("creating backdrop GPU surface: {e}"),
                };
            let caps = surface.get_capabilities(&gpu.adapter);
            let Some(mut setup) = SurfaceSetup::new(&caps, width, height) else {
                return log::error!("backdrop surface offers no supported 8-bit format");
            };
            // The backdrop is hidden outside the overview and gets no frame
            // callbacks there; FIFO would block a second present until it
            // is shown again.
            match [wgpu::PresentMode::Mailbox, wgpu::PresentMode::Immediate]
                .into_iter()
                .find(|m| caps.present_modes.contains(m))
            {
                Some(mode) => setup.config.present_mode = mode,
                None => log::warn!(
                    "backdrop surface offers only {:?}; redraws may stall while hidden",
                    caps.present_modes
                ),
            }
            setup.configure(&surface, &gpu.device);
            b.target = Some(BackdropTarget { surface, setup });
        }
        let bt = b.target.as_mut().expect("backdrop target created");
        if (bt.setup.config.width, bt.setup.config.height) != (width, height) {
            bt.setup.resize(width, height);
            bt.setup.configure(&bt.surface, &gpu.device);
        }
        let renderer = gpu.renderer_index(t.setup.render_format);
        let backdrop = gpu.backdrop_index(bt.setup.render_format);
        let (renderer, backdrop) = (&gpu.renderers[renderer].1, &gpu.backdrops[backdrop].1);
        let draw_start = Instant::now();
        let outcome = bt
            .setup
            .draw(&bt.surface, &gpu.device, &gpu.queue, |encoder, views, _| {
                backdrop.render(
                    &gpu.device,
                    &gpu.queue,
                    encoder,
                    renderer,
                    &mut t.target,
                    t.setup.render_format,
                    output_size,
                    time,
                    views,
                );
            });
        match outcome {
            FrameOutcome::Presented => {
                log::debug!(
                    "backdrop {width}x{height} presented in {:?}",
                    draw_start.elapsed()
                );
                b.drawn_for = Some(output_size);
            }
            FrameOutcome::Skipped => log::debug!("backdrop frame skipped"),
            FrameOutcome::Lost => {
                log::warn!("backdrop surface lost; recreating on next draw");
                b.target = None;
            }
        }
    }

    /// niri's overview opened: refresh the stills of animated wallpapers.
    fn overview_opened(&mut self) {
        let now = Instant::now();
        for index in 0..self.outputs.len() {
            let Some(w) = &self.outputs[index].wallpaper else {
                continue;
            };
            if w.backdrop.is_some() && (w.player.is_some() || w.next_frame.is_some()) {
                let time = w.scene_clock.time(now);
                self.draw_backdrop(index, time);
            }
        }
    }

    fn players_mut(&mut self) -> impl Iterator<Item = &mut Player> {
        self.outputs
            .iter_mut()
            .filter_map(|s| s.wallpaper.as_mut()?.player.as_mut())
    }

    /// Registers the video loop timer when a video needs it.
    fn ensure_video_timer(&mut self) {
        let animating = |w: &Wallpaper| w.player.is_some() || w.next_frame.is_some();
        if self.video_timer
            || !self
                .outputs
                .iter()
                .any(|s| s.wallpaper.as_ref().is_some_and(animating))
        {
            return;
        }
        let registered =
            self.loop_handle
                .insert_source(Timer::immediate(), |_, _, host: &mut Host| {
                    match host.video_tick(Instant::now()) {
                        Some(wait) => TimeoutAction::ToDuration(wait),
                        None => {
                            host.video_timer = false;
                            TimeoutAction::Drop
                        }
                    }
                });
        match registered {
            Ok(_) => self.video_timer = true,
            Err(e) => log::error!("registering the video timer: {e}"),
        }
    }

    /// Advances every video. Returns when to run again, or `None` when no
    /// video needs the timer.
    fn video_tick(&mut self, now: Instant) -> Option<Duration> {
        let mut next: Option<Duration> = None;
        let mut any = false;
        let paused = self.paused;
        let mut soonest =
            |wait: Duration| next = Some(next.map_or(wait, |n: Duration| n.min(wait)));
        for index in 0..self.outputs.len() {
            let Some(w) = self.outputs[index].wallpaper.as_mut() else {
                continue;
            };
            let looks_hidden = w
                .frame_pending
                .is_some_and(|t| now.duration_since(t) > HIDDEN_AFTER);
            if let Some(due) = w.next_frame {
                // A paused scene keeps its last frame; `resume` restarts the
                // timer.
                if paused {
                    continue;
                }
                // Animated scene: draw at its frame rate when the compositor
                // is ready.
                any = true;
                if due <= now && w.frame_pending.is_none() {
                    self.draw(index);
                    soonest(Duration::from_secs_f64(1.0 / SCENE_FPS));
                    continue;
                }
                soonest(if due > now {
                    due - now
                } else {
                    Duration::from_millis(5)
                });
                continue;
            }
            let Some(player) = w.player.as_mut() else {
                continue;
            };
            any = true;
            if !w.hidden && !player.is_paused() && looks_hidden {
                log::debug!("video surface looks hidden; pausing its clock");
                player.pause(now);
                w.hidden = true;
            }
            let (frame, wait) = player.poll(now);
            if player.stopped() {
                // Its last frame stays on screen as a still.
                w.player = None;
            }
            if let Some(frame) = frame {
                w.video_frame = Some(frame);
            }
            if w.video_frame.is_some() && w.frame_pending.is_none() {
                self.draw(index);
            }
            if let Some(wait) = wait {
                soonest(wait);
            }
        }
        // Paused or hidden content still needs an occasional check.
        any.then(|| next.unwrap_or(Duration::from_millis(250)))
    }

    fn find_surface(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.outputs.iter().position(|s| {
            s.wallpaper
                .as_ref()
                .is_some_and(|w| w.layer.wl_surface() == surface)
        })
    }

    fn find_backdrop(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.outputs.iter().position(|s| {
            s.wallpaper
                .as_ref()
                .and_then(|w| w.backdrop.as_ref())
                .is_some_and(|b| b.layer.wl_surface() == surface)
        })
    }

    fn handle(&mut self, request: Request) -> Reply {
        match request {
            Request::Hello { .. } => unreachable!("handled by the control thread"),
            Request::Capabilities => self.capabilities(),
            Request::Outputs => Ok(self.outputs_json()),
            Request::Assign { output, project } => {
                let targets = self.target_outputs(output.as_deref())?;
                for (index, name) in targets {
                    let prepared = self.prepare(&project)?;
                    self.assignments.insert(name, project.clone());
                    self.apply_assignment(index, Some(prepared));
                }
                Ok(Value::Null)
            }
            Request::Unassign { output } => {
                match output.as_deref() {
                    Some(name) => {
                        if self.assignments.remove(name).is_none() {
                            return Err(Error::new(
                                ErrorCode::UnknownOutput,
                                format!("{name} has no wallpaper"),
                            ));
                        }
                    }
                    None => self.assignments.clear(),
                }
                for index in 0..self.outputs.len() {
                    let name = self.output_name(&self.outputs[index].wl);
                    if !self.assignments.contains_key(&name) {
                        self.apply_assignment(index, None);
                    }
                }
                Ok(Value::Null)
            }
            Request::Reload { project } => {
                for index in 0..self.outputs.len() {
                    let shows = self.outputs[index]
                        .wallpaper
                        .as_ref()
                        .is_some_and(|w| same_path(&w.project_dir, &project));
                    if shows {
                        let prepared = self.prepare(&project)?;
                        self.apply_assignment(index, Some(prepared));
                    }
                }
                Ok(Value::Null)
            }
            // Static content renders only on demand, so pausing changes no
            // GPU work yet; the state is kept for animated content.
            Request::Pause => {
                self.paused = true;
                let now = Instant::now();
                for p in self.players_mut() {
                    p.pause(now);
                }
                for w in self.outputs.iter_mut().filter_map(|s| s.wallpaper.as_mut()) {
                    w.scene_clock.pause(now);
                }
                Ok(Value::Null)
            }
            Request::Resume => {
                self.paused = false;
                let now = Instant::now();
                for slot in &mut self.outputs {
                    if let Some(w) = &mut slot.wallpaper
                        && !w.hidden
                        && let Some(p) = &mut w.player
                    {
                        p.resume(now);
                    }
                }
                for w in self.outputs.iter_mut().filter_map(|s| s.wallpaper.as_mut()) {
                    w.scene_clock.resume(now);
                }
                self.ensure_video_timer();
                Ok(Value::Null)
            }
            Request::Seek { output, position } => {
                let targets = self.target_outputs(output.as_deref())?;
                let now = Instant::now();
                for (index, _) in targets {
                    if let Some(p) = self.outputs[index]
                        .wallpaper
                        .as_mut()
                        .and_then(|w| w.player.as_mut())
                    {
                        p.seek(position, now);
                    }
                }
                Ok(Value::Null)
            }
            Request::Shutdown => {
                log::info!("shutdown requested");
                for slot in &mut self.outputs {
                    slot.wallpaper = None;
                }
                self.stop.stop();
                Ok(Value::Null)
            }
        }
    }

    fn target_outputs(&self, output: Option<&str>) -> Result<Vec<(usize, String)>, Error> {
        match output {
            Some(name) => self
                .slot_index(name)
                .map(|i| vec![(i, name.to_owned())])
                .ok_or_else(|| {
                    Error::new(
                        ErrorCode::UnknownOutput,
                        format!("no connected output named {name}"),
                    )
                }),
            None => Ok(self
                .outputs
                .iter()
                .enumerate()
                .map(|(i, s)| (i, self.output_name(&s.wl)))
                .collect()),
        }
    }

    fn capabilities(&mut self) -> Reply {
        if self.gpu.is_none() {
            self.gpu = Some(
                init_gpu(&self.instance, None)
                    .map_err(|e| Error::new(ErrorCode::Gpu, format!("{e:#}")))?,
            );
        }
        let gpu = self.gpu.as_ref().expect("gpu initialized");
        Ok(json!({
            "gpu": GpuInfo::from_adapter(&gpu.adapter),
            "presentation": {
                "kind": "wayland-layer-shell",
                "compositor": std::env::var("XDG_CURRENT_DESKTOP").ok(),
                "fractional_scale": matches!(self.scaling, Scaling::Fractional(..)),
                // Not available through layer-shell alone; needs
                // compositor-specific adapters.
                "global_pointer": false,
                "visibility_pause": false,
                // Shown only where a niri layer rule places it.
                "overview_backdrop": self.backdrops,
            },
            "paused": self.paused,
        }))
    }

    fn outputs_json(&self) -> Value {
        let outputs: Vec<Value> = self
            .outputs
            .iter()
            .map(|s| {
                let info = self.output_state.info(&s.wl);
                let name = self.output_name(&s.wl);
                let w = s.wallpaper.as_ref();
                json!({
                    "name": name,
                    "description": info.as_ref().and_then(|i| i.description.clone()),
                    "make": info.as_ref().map(|i| i.make.clone()),
                    "model": info.as_ref().map(|i| i.model.clone()),
                    "logical_position": info.as_ref().and_then(|i| i.logical_position),
                    "logical_size": info.as_ref().and_then(|i| i.logical_size),
                    "scale_factor": info.as_ref().map(|i| i.scale_factor),
                    "transform": info.as_ref().map(|i| format!("{:?}", i.transform)),
                    "assigned": self.assignments.get(&name),
                    "video": w.and_then(|w| w.player.as_ref()).map(|p| json!({
                        "codec": p.info.codec,
                        "size": (p.info.width, p.info.height),
                        "frame_rate": p.info.frame_rate,
                        "duration": p.info.duration,
                        "position": p.position_in_video(Instant::now()),
                        "paused": p.is_paused(),
                        "hidden": w.is_some_and(|w| w.hidden),
                    })),
                    "surface": w.map(|w| json!({
                        "logical_size": w.logical,
                        "buffer_size": w.buffer_size(),
                        "scale": f64::from(w.scale_120) / f64::from(SCALE_DENOMINATOR),
                        "presenting": w.target.is_some(),
                        "backdrop": w.backdrop.as_ref().map(|b| json!({
                            "logical_size": b.logical,
                            "buffer_size": b.target.as_ref()
                                .map(|t| (t.setup.config.width, t.setup.config.height)),
                            "drawn_for": b.drawn_for,
                        })),
                    })),
                })
            })
            .collect();
        Value::Array(outputs)
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

fn init_gpu(instance: &wgpu::Instance, surface: Option<&wgpu::Surface<'_>>) -> anyhow::Result<Gpu> {
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        // Wallpapers should not wake a discrete GPU when an integrated one
        // can do the job.
        power_preference:
            wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::LowPower),
        compatible_surface: surface,
        ..Default::default()
    }))?;
    let (device, queue) = pollster::block_on(gpu::request_device(&adapter))?;
    let info = adapter.get_info();
    log::info!(
        "GPU: {} ({:?}, {})",
        info.name,
        info.backend,
        info.driver_info
    );
    Ok(Gpu {
        adapter,
        device,
        queue,
        renderers: Vec::new(),
        backdrops: Vec::new(),
    })
}

/// Creates a wgpu surface presenting to `surface`.
///
/// # Safety
///
/// The returned surface must be dropped before `surface` is destroyed.
unsafe fn gpu_surface(
    instance: &wgpu::Instance,
    conn: &Connection,
    surface: &wl_surface::WlSurface,
) -> Result<wgpu::Surface<'static>, wgpu::CreateSurfaceError> {
    let display = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
        NonNull::new(conn.backend().display_ptr().cast()).expect("display pointer"),
    ));
    let window = RawWindowHandle::Wayland(WaylandWindowHandle::new(
        NonNull::new(surface.id().as_ptr().cast()).expect("surface pointer"),
    ));
    // SAFETY: the handles stay valid as long as the caller promises.
    unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(display),
            raw_window_handle: window,
        })
    }
}

impl CompositorHandler for Host {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        factor: i32,
    ) {
        let Some(index) = self.find_surface(surface) else {
            return;
        };
        let w = self.outputs[index]
            .wallpaper
            .as_mut()
            .expect("found wallpaper");
        w.scale_int = factor.max(1) as u32;
        if w.viewport.is_none() {
            self.draw(index);
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
        // Layer-shell configures already carry the rotated logical size.
    }

    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        let Some(index) = self.find_surface(surface) else {
            return;
        };
        let paused = self.paused;
        let w = self.outputs[index]
            .wallpaper
            .as_mut()
            .expect("found wallpaper");
        w.frame_pending = None;
        let was_hidden = std::mem::take(&mut w.hidden);
        if was_hidden
            && !paused
            && let Some(p) = &mut w.player
        {
            p.resume(Instant::now());
        }
        let has_frame =
            w.video_frame.is_some() || w.next_frame.is_some_and(|due| due <= Instant::now());
        if was_hidden {
            self.ensure_video_timer();
        }
        if has_frame {
            self.draw(index);
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Host {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, wl: wl_output::WlOutput) {
        let name = self.output_name(&wl);
        log::info!("output added: {name}");
        self.outputs.push(OutputSlot {
            wl,
            wallpaper: None,
        });
        if let Some(dir) = self.assignments.get(&name).cloned() {
            match self.prepare(&dir) {
                Ok(prepared) => self.apply_assignment(self.outputs.len() - 1, Some(prepared)),
                Err(e) => log::error!("restoring wallpaper on {name}: {}", e.message),
            }
        }
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, wl: wl_output::WlOutput) {
        log::info!("output removed: {}", self.output_name(&wl));
        self.outputs.retain(|s| s.wl != wl);
    }
}

impl LayerShellHandler for Host {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if let Some(index) = self.find_backdrop(layer.wl_surface()) {
            log::info!(
                "compositor closed backdrop surface on {}",
                self.output_name(&self.outputs[index].wl)
            );
            if let Some(w) = &mut self.outputs[index].wallpaper {
                w.backdrop = None;
            }
            return;
        }
        if let Some(index) = self.find_surface(layer.wl_surface()) {
            log::info!(
                "compositor closed wallpaper surface on {}",
                self.output_name(&self.outputs[index].wl)
            );
            self.outputs[index].wallpaper = None;
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        if let Some(index) = self.find_backdrop(layer.wl_surface()) {
            let w = self.outputs[index]
                .wallpaper
                .as_mut()
                .expect("found wallpaper");
            let b = w.backdrop.as_mut().expect("found backdrop");
            b.logical = configure.new_size;
            b.drawn_for = None;
            let time = w.scene_clock.time(Instant::now());
            self.draw_backdrop(index, time);
            return;
        }
        let Some(index) = self.find_surface(layer.wl_surface()) else {
            return;
        };
        let w = self.outputs[index]
            .wallpaper
            .as_mut()
            .expect("found wallpaper");
        w.logical = configure.new_size;
        self.draw(index);
    }
}

impl ProvidesRegistryState for Host {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState];
}

delegate_registry!(Host);
smithay_client_toolkit::delegate_dispatch2!(Host);

/// User data for protocol objects that emit no events we use.
struct NoEvents;

impl<I: Proxy> Dispatch2<I, Host> for NoEvents {
    fn event(&self, _: &mut Host, _: &I, _: I::Event, _: &Connection, _: &QueueHandle<Host>) {}
}

/// User data for a surface's wp_fractional_scale_v1 object.
struct FractionalScale(wl_surface::WlSurface);

impl Dispatch2<wp_fractional_scale_v1::WpFractionalScaleV1, Host> for FractionalScale {
    fn event(
        &self,
        host: &mut Host,
        _: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &Connection,
        _: &QueueHandle<Host>,
    ) {
        let wp_fractional_scale_v1::Event::PreferredScale { scale } = event else {
            return;
        };
        let Some(index) = host.find_surface(&self.0) else {
            return;
        };
        let w = host.outputs[index]
            .wallpaper
            .as_mut()
            .expect("found wallpaper");
        if w.scale_120 != scale {
            log::info!(
                "preferred scale {}",
                f64::from(scale) / f64::from(SCALE_DENOMINATOR)
            );
            w.scale_120 = scale;
            host.draw(index);
        }
    }
}
