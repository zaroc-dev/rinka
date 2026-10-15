//! Windows presentation: one child window per monitor behind the desktop
//! icons (see `desktop.rs` for where they go).
//!
//! Two threads: the window thread (`ui.rs`) owns the windows and only pumps
//! messages; this module's render thread loads projects, renders and
//! presents. Wallpaper windows take no input (`WS_EX_TRANSPARENT`,
//! `HTTRANSPARENT`) and never activate. Static content is drawn when a window
//! is created or moved or its project changes. Animated scenes are drawn from
//! a high-resolution waitable timer at their frame rate; presents use Fifo,
//! so drawing never outruns the display. Unlike Wayland there is no frame
//! callback; `coverage.rs` reads the window stack instead, and monitors that
//! other windows cover completely stop animating and keep their last frame.

use std::collections::HashMap;
use std::num::NonZeroIsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::Context;
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, Win32WindowHandle, WindowsDisplayHandle,
};
use rinka_engine::{
    FrameOutcome, GpuInfo, Project, Renderer, Source, SurfaceSetup, VideoTexture, gpu,
};
use serde_json::{Value, json};
use windows_sys::Win32::Foundation::{HINSTANCE, HWND};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

use crate::clock::SceneClock;
use crate::control::{self, Command};
use crate::project_io::{self, ImageCache};
use crate::protocol::{Error, ErrorCode, Reply, Request};
use crate::video::{OwnedFrame, Player};

mod coverage;
mod desktop;
mod monitor;
mod timer;
mod ui;

use desktop::Desktop;
use monitor::Monitor;
use timer::{WaitableTimer, Wake};
use ui::{Event, Ui};

/// Frame rate of animated scenes.
const SCENE_FPS: f64 = 30.0;

/// After the shell destroys a wallpaper window, wait this long before
/// rebuilding, unless explorer announces its restart sooner.
const REBUILD_DELAY: Duration = Duration::from_secs(2);

/// A window handle that can cross threads. Windows are only touched on the
/// window thread, through `Ui`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Hwnd(isize);

impl Hwnd {
    fn from_raw(hwnd: HWND) -> Option<Self> {
        (!hwnd.is_null()).then_some(Self(hwnd as isize))
    }

    fn raw(self) -> HWND {
        self.0 as HWND
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn hinstance() -> HINSTANCE {
    // SAFETY: a null name returns the executable's own module handle.
    unsafe { GetModuleHandleW(std::ptr::null()) }
}

/// Runs the host. `assignments` (output name → project directory) are shown
/// as their outputs appear.
pub fn run(endpoint: PathBuf, assignments: HashMap<String, PathBuf>) -> anyhow::Result<()> {
    let wake = Arc::new(Wake::new()?);
    let (ui, events) = Ui::start(wake.clone())?;
    let ui = Arc::new(ui);
    let timer = WaitableTimer::new()?;

    let mut host = Host {
        ui: ui.clone(),
        instance: wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
        ),
        gpu: None,
        images: ImageCache::default(),
        desktop: Some(Desktop::find()?),
        rebuild_at: None,
        outputs: Vec::new(),
        assignments,
        paused: false,
        stop: false,
    };
    // Learn the monitors before accepting requests.
    host.sync_outputs();

    let (tx, rx) = mpsc::channel::<Command>();
    let listener = control::bind(&endpoint).context("opening the control pipe")?;
    log::info!("control pipe: {}", endpoint.display());
    let waker = wake.clone();
    control::serve(listener, move |cmd| {
        let sent = tx.send(cmd).is_ok();
        waker.set();
        sent
    });

    loop {
        // Forget destroyed windows before a queued command can draw.
        while let Ok(event) = events.try_recv() {
            host.process_event(event);
        }
        while let Ok(cmd) = rx.try_recv() {
            let reply = host.handle(cmd.request);
            let _ = cmd.reply.send(reply);
            if host.stop {
                cmd.delivered.wait();
                break;
            }
        }
        if host.stop {
            break;
        }
        timer.arm(host.tick(Instant::now()));
        timer.wait(&wake);
    }
    // Destroys the wallpaper windows, then ends the window thread.
    drop(host);
    drop(ui);
    Ok(())
}

/// A wallpaper window, destroyed on drop.
struct Window {
    hwnd: Hwnd,
    ui: Arc<Ui>,
}

impl Drop for Window {
    fn drop(&mut self) {
        self.ui.destroy(self.hwnd);
    }
}

struct Gpu {
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// One per surface format in use; normally a single entry.
    renderers: Vec<(wgpu::TextureFormat, Renderer)>,
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
}

struct Host {
    ui: Arc<Ui>,
    instance: wgpu::Instance,
    gpu: Option<Gpu>,
    images: ImageCache,
    desktop: Option<Desktop>,
    /// The shell destroyed wallpaper windows; recreate them at this instant.
    rebuild_at: Option<Instant>,
    outputs: Vec<OutputSlot>,
    /// Desired output name -> project directory. Survives disconnection, so a
    /// reconnected monitor gets its wallpaper back.
    assignments: HashMap<String, PathBuf>,
    paused: bool,
    stop: bool,
}

struct OutputSlot {
    monitor: Monitor,
    wallpaper: Option<Wallpaper>,
    /// Other windows cover the monitor completely: animation stops and the
    /// last frame stays.
    covered: bool,
}

struct Wallpaper {
    project_dir: PathBuf,
    project: Project,
    /// Absolute path of the image asset, when the project shows one.
    image: Option<PathBuf>,
    /// Loaded assets of a scene project.
    scene: Option<std::sync::Arc<rinka_engine::SceneAssets>>,
    player: Option<Player>,
    video_frame: Option<OwnedFrame>,
    video_texture: Option<VideoTexture>,
    /// The scene's clock; it stops while animation is paused.
    scene_clock: SceneClock,
    /// The scene animates and the next frame is due at this instant.
    next_frame: Option<Instant>,
    /// The project changed since the target was last updated.
    dirty: bool,
    /// Shown only after the first successful present.
    shown: bool,
    // Field order matters: the wgpu surface must drop before its window.
    target: Option<Target>,
    /// `None` once the shell destroyed the window.
    window: Option<Window>,
}

struct Target {
    surface: wgpu::Surface<'static>,
    setup: SurfaceSetup,
    target: rinka_engine::Target,
}

/// A loaded project with its assets decoded, ready to show.
struct Prepared {
    dir: PathBuf,
    project: Project,
    image: Option<PathBuf>,
    scene: Option<std::sync::Arc<rinka_engine::SceneAssets>>,
    video: Option<(Player, OwnedFrame)>,
}

impl Host {
    fn slot_index(&self, name: &str) -> Option<usize> {
        self.outputs.iter().position(|s| s.monitor.name == name)
    }

    /// Loads a project and decodes its assets, so errors reach the client
    /// before anything on screen changes.
    fn prepare(&mut self, dir: &Path) -> Result<Prepared, Error> {
        let project = project_io::load(dir)?;
        let video = project_io::video_asset(dir, &project)?
            .map(|path| {
                let (_, frame) = crate::video::first_frame(&path)?;
                Ok((Player::open(&path)?, frame))
            })
            .transpose()
            .map_err(|e: anyhow::Error| {
                Error::new(ErrorCode::Project, format!("opening video: {e:#}"))
            })?;
        let image = project_io::image_asset(dir, &project)?;
        if let Some(path) = &image {
            self.images.decode(path)?;
        }
        let scene = project_io::scene_assets(dir, &project)?.map(std::sync::Arc::new);
        Ok(Prepared {
            dir: dir.to_owned(),
            project,
            image,
            scene,
            video,
        })
    }

    /// Ensures the output shows its assigned project, creating, updating or
    /// removing its wallpaper window.
    fn apply_assignment(&mut self, index: usize, prepared: Option<Prepared>) -> Result<(), Error> {
        match prepared {
            None => self.outputs[index].wallpaper = None,
            Some(prepared) => {
                // Keep the old window and target alive until the replacement
                // has built successfully. An invalid scene must not erase a
                // working wallpaper or leave an old scene under a new name.
                let window = self.create_window(index)?;
                let paused = self.paused || self.outputs[index].covered;
                let (mut player, video_frame) = match prepared.video {
                    Some((player, frame)) => (Some(player), Some(frame)),
                    None => (None, None),
                };
                if paused && let Some(player) = &mut player {
                    player.pause(Instant::now());
                }
                let previous = self.outputs[index].wallpaper.replace(Wallpaper {
                    project_dir: prepared.dir,
                    project: prepared.project,
                    image: prepared.image,
                    scene: prepared.scene,
                    player,
                    video_frame,
                    video_texture: None,
                    scene_clock: SceneClock::new(Instant::now(), paused),
                    next_frame: None,
                    dirty: false,
                    shown: false,
                    target: None,
                    window: Some(window),
                });
                if let Err(error) = self.draw(index) {
                    self.outputs[index].wallpaper = previous;
                    return Err(error);
                }
            }
        }
        let used: Vec<PathBuf> = self
            .outputs
            .iter()
            .filter_map(|s| s.wallpaper.as_ref()?.image.clone())
            .collect();
        self.images.retain(&used);
        Ok(())
    }

    fn create_window(&self, index: usize) -> Result<Window, Error> {
        let desktop = self
            .desktop
            .as_ref()
            .ok_or_else(|| Error::new(ErrorCode::Gpu, "the Windows desktop is not available"))?;
        let hwnd = self
            .ui
            .create_wallpaper(desktop, self.outputs[index].monitor.rect)
            .map_err(|e| Error::new(ErrorCode::Gpu, format!("{e:#}")))?;
        Ok(Window {
            hwnd,
            ui: self.ui.clone(),
        })
    }

    fn draw(&mut self, index: usize) -> Result<(), Error> {
        let slot = &mut self.outputs[index];
        let (width, height) = slot.monitor.size();
        let Some(w) = slot.wallpaper.as_mut() else {
            return Ok(());
        };
        let Some(hwnd) = w.window.as_ref().map(|window| window.hwnd) else {
            w.next_frame = None;
            return Ok(());
        };
        if width == 0 || height == 0 {
            w.next_frame = None;
            return Ok(());
        }

        if w.target.is_none() {
            let mut window =
                Win32WindowHandle::new(NonZeroIsize::new(hwnd.0).expect("window handle"));
            window.hinstance = NonZeroIsize::new(hinstance() as isize);
            // SAFETY: the window outlives the surface, which `Wallpaper`
            // drops before destroying the window.
            let surface = match unsafe {
                self.instance
                    .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                        raw_display_handle: Some(RawDisplayHandle::Windows(
                            WindowsDisplayHandle::new(),
                        )),
                        raw_window_handle: RawWindowHandle::Win32(window),
                    })
            } {
                Ok(s) => s,
                Err(e) => {
                    return Err(Error::new(
                        ErrorCode::Gpu,
                        format!("creating GPU surface: {e}"),
                    ));
                }
            };
            if self.gpu.is_none() {
                match init_gpu(&self.instance, Some(&surface)) {
                    Ok(gpu) => self.gpu = Some(gpu),
                    Err(e) => return Err(Error::new(ErrorCode::Gpu, format!("{e:#}"))),
                }
            }
            let gpu = self.gpu.as_mut().expect("gpu initialized");
            let Some(setup) =
                SurfaceSetup::new(&surface.get_capabilities(&gpu.adapter), width, height)
            else {
                return Err(Error::new(
                    ErrorCode::Gpu,
                    "surface offers no supported 8-bit format",
                ));
            };
            setup.configure(&surface, &gpu.device);
            let index = gpu.renderer_index(setup.render_format);
            let renderer = &gpu.renderers[index].1;
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
        let index = gpu.renderer_index(t.setup.render_format);
        let renderer = &gpu.renderers[index].1;
        if let Some(frame) = w.video_frame.take() {
            let yuv = frame.as_yuv();
            if w.video_texture
                .as_ref()
                .is_none_or(|video| !video.fits(&yuv))
            {
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
                (None, Some(video)) => Some(Source::Yuv(video)),
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
                    Err(e) => {
                        return Err(Error::new(
                            ErrorCode::Project,
                            format!("building scene: {e}"),
                        ));
                    }
                }
            }
            w.next_frame = t.target.animated().then(Instant::now);
            w.dirty = false;
        }
        let draw_start = Instant::now();
        let time = w.scene_clock.time(Instant::now());
        if let Some(next) = &mut w.next_frame {
            *next = Instant::now() + Duration::from_secs_f64(1.0 / SCENE_FPS);
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
        let animated = t.target.animated();
        match outcome {
            FrameOutcome::Presented => {
                if !w.shown {
                    // Failed replacements remain hidden until they are
                    // dropped.
                    self.ui.show(hwnd);
                    w.shown = true;
                }
                log::debug!("presented {width}x{height} in {:?}", draw_start.elapsed());
            }
            FrameOutcome::Skipped => log::debug!("frame skipped"),
            FrameOutcome::Lost => {
                log::warn!("surface lost; recreating on next draw");
                w.target = None;
            }
        }
        // Static surfaces also need a retry after an acquisition failure.
        if outcome != FrameOutcome::Presented {
            w.next_frame = Some(Instant::now() + Duration::from_millis(250));
        } else if !animated {
            w.next_frame = None;
        }
        Ok(())
    }

    /// Draws due animation frames and rebuilds windows the shell destroyed.
    /// Returns when to run again, or `None` when nothing is scheduled.
    fn tick(&mut self, now: Instant) -> Option<Duration> {
        let mut next: Option<Duration> = None;
        let mut soonest =
            |wait: Duration| next = Some(next.map_or(wait, |n: Duration| n.min(wait)));
        if let Some(at) = self.rebuild_at
            && at <= now
        {
            self.rebuild_at = None;
            self.rebuild();
        }
        if let Some(at) = self.rebuild_at {
            soonest(at.saturating_duration_since(Instant::now()));
        }
        // A successful rebuild must resume players before deciding whether
        // any timer is needed; otherwise an idle video could stay paused.
        let now = Instant::now();
        self.update_playback_state(now);
        for index in 0..self.outputs.len() {
            let slot = &mut self.outputs[index];
            let Some(w) = slot.wallpaper.as_mut() else {
                continue;
            };
            let (frame, video_wait) = w
                .player
                .as_mut()
                .map(|p| p.poll(now))
                .unwrap_or((None, None));
            if let Some(frame) = frame {
                w.video_frame = Some(frame);
            }
            let video_due = video_wait.map(|wait| now + wait);
            let paused_scene =
                self.paused && w.target.as_ref().is_some_and(|t| t.target.animated());
            // A seek may update the still image while globally paused.
            // Covered windows retain that frame until they become visible.
            let draw = !slot.covered
                && w.window.is_some()
                && (w.video_frame.is_some()
                    || (!paused_scene && w.next_frame.is_some_and(|due| due <= now)));
            if draw && let Err(e) = self.draw(index) {
                log::error!("{}: {}", self.outputs[index].monitor.name, e.message);
                self.outputs[index]
                    .wallpaper
                    .as_mut()
                    .expect("wallpaper")
                    .next_frame = None;
            }
            let now = Instant::now();
            if let Some(due) = video_due {
                soonest(due.saturating_duration_since(now));
            }
            if !self.outputs[index].covered
                && let Some(w) = self.outputs[index].wallpaper.as_ref()
                && !(self.paused && w.target.as_ref().is_some_and(|t| t.target.animated()))
                && let Some(due) = w.next_frame
            {
                soonest(due.saturating_duration_since(now));
            }
        }
        next
    }

    fn update_playback_state(&mut self, now: Instant) {
        for slot in &mut self.outputs {
            let Some(w) = slot.wallpaper.as_mut() else {
                continue;
            };
            let paused = self.paused || slot.covered || w.window.is_none();
            if paused {
                w.scene_clock.pause(now);
            } else {
                w.scene_clock.resume(now);
            }
            if let Some(player) = &mut w.player {
                if paused {
                    player.pause(now);
                } else {
                    player.resume(now);
                }
            }
        }
    }

    /// Handles what the window thread saw.
    fn process_event(&mut self, event: Event) {
        match event {
            Event::DisplayChanged => {
                log::info!("display configuration changed");
                self.sync_outputs();
            }
            Event::ShellRestarted => {
                log::info!("explorer restarted; recreating wallpaper windows");
                self.rebuild_at = None;
                self.rebuild();
            }
            Event::Coverage(coverage) => {
                for slot in &mut self.outputs {
                    let covered = coverage.get(&slot.monitor.name).copied().unwrap_or(false);
                    if covered != slot.covered {
                        log::debug!(
                            "{} {}",
                            slot.monitor.name,
                            if covered {
                                "covered; pausing"
                            } else {
                                "visible; resuming"
                            }
                        );
                        slot.covered = covered;
                    }
                }
            }
            Event::Destroyed(hwnd) => {
                for slot in &mut self.outputs {
                    if let Some(w) = &mut slot.wallpaper
                        && w.window.as_ref().is_some_and(|window| window.hwnd == hwnd)
                    {
                        log::warn!(
                            "the shell destroyed the wallpaper window on {}",
                            slot.monitor.name
                        );
                        w.target = None;
                        w.window = None;
                        w.next_frame = None;
                        self.rebuild_at
                            .get_or_insert(Instant::now() + REBUILD_DELAY);
                    }
                }
            }
        }
    }

    /// Finds the desktop again and recreates every wallpaper window.
    fn rebuild(&mut self) {
        for slot in &mut self.outputs {
            if let Some(w) = &mut slot.wallpaper {
                w.target = None;
                w.window = None;
                w.next_frame = None;
                w.shown = false;
            }
        }
        match Desktop::find() {
            Ok(desktop) => self.desktop = Some(desktop),
            Err(e) => {
                log::error!("{e:#}; retrying");
                self.desktop = None;
                self.rebuild_at = Some(Instant::now() + REBUILD_DELAY);
                return;
            }
        }
        for index in 0..self.outputs.len() {
            if self.outputs[index].wallpaper.is_none() {
                continue;
            }
            match self.create_window(index) {
                Ok(window) => {
                    let w = self.outputs[index].wallpaper.as_mut().expect("wallpaper");
                    w.window = Some(window);
                    w.dirty = true;
                    if let Err(e) = self.draw(index) {
                        log::error!("{}: {}", self.outputs[index].monitor.name, e.message);
                    }
                }
                Err(e) => {
                    log::error!("{}: {}", self.outputs[index].monitor.name, e.message);
                    self.rebuild_at = Some(Instant::now() + REBUILD_DELAY);
                }
            }
        }
    }

    /// Matches the output list to the connected monitors.
    fn sync_outputs(&mut self) {
        let monitors = Monitor::all();
        self.outputs.retain(|slot| {
            let keep = monitors.iter().any(|m| m.name == slot.monitor.name);
            if !keep {
                log::info!("output removed: {}", slot.monitor.name);
            }
            keep
        });
        let mut resized = Vec::new();
        for monitor in monitors {
            match self.slot_index(&monitor.name) {
                Some(index) => {
                    let slot = &mut self.outputs[index];
                    if slot.monitor == monitor {
                        continue;
                    }
                    if slot.monitor.size() != monitor.size() {
                        resized.push(index);
                    }
                    slot.monitor = monitor;
                }
                None => {
                    let name = monitor.name.clone();
                    log::info!(
                        "output added: {name} {}x{} at ({}, {})",
                        monitor.size().0,
                        monitor.size().1,
                        monitor.rect.left,
                        monitor.rect.top
                    );
                    self.outputs.push(OutputSlot {
                        monitor,
                        wallpaper: None,
                        covered: false,
                    });
                    if let Some(dir) = self.assignments.get(&name).cloned() {
                        let index = self.outputs.len() - 1;
                        let result = self
                            .prepare(&dir)
                            .and_then(|prepared| self.apply_assignment(index, Some(prepared)));
                        if let Err(e) = result {
                            log::error!("restoring wallpaper on {name}: {}", e.message);
                        }
                    }
                }
            }
        }
        // Window positions are relative to Explorer's desktop window, which
        // moves when the virtual screen's origin does (a monitor left of or
        // above the others came or went). Place every window again, not only
        // those whose monitor changed.
        if let Some(desktop) = &self.desktop {
            for slot in &self.outputs {
                if let Some(window) = slot.wallpaper.as_ref().and_then(|w| w.window.as_ref()) {
                    self.ui.place(desktop, window.hwnd, slot.monitor.rect);
                }
            }
        }
        for index in resized {
            if let Err(e) = self.draw(index) {
                log::error!("{}: {}", self.outputs[index].monitor.name, e.message);
            }
        }
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
                    self.apply_assignment(index, Some(prepared))?;
                    self.assignments.insert(name, project.clone());
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
                    if !self
                        .assignments
                        .contains_key(&self.outputs[index].monitor.name)
                    {
                        self.apply_assignment(index, None)?;
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
                        self.apply_assignment(index, Some(prepared))?;
                    }
                }
                Ok(Value::Null)
            }
            // Animation stops and the last frame stays on screen.
            Request::Pause => {
                self.paused = true;
                self.update_playback_state(Instant::now());
                Ok(Value::Null)
            }
            Request::Resume => {
                self.paused = false;
                self.update_playback_state(Instant::now());
                Ok(Value::Null)
            }
            Request::Seek { output, position } => {
                if !position.is_finite() || position < 0.0 {
                    return Err(Error::new(
                        ErrorCode::BadRequest,
                        "seek position must be finite and non-negative",
                    ));
                }
                let targets = self.target_outputs(output.as_deref())?;
                let now = Instant::now();
                for (index, _) in targets {
                    if let Some(player) = self.outputs[index]
                        .wallpaper
                        .as_mut()
                        .and_then(|w| w.player.as_mut())
                    {
                        player.seek(position, now);
                    }
                }
                Ok(Value::Null)
            }
            Request::Shutdown => {
                log::info!("shutdown requested");
                for slot in &mut self.outputs {
                    slot.wallpaper = None;
                }
                self.stop = true;
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
                .map(|(i, s)| (i, s.monitor.name.clone()))
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
        let raised = self.desktop.as_ref().map(|d| d.raised);
        Ok(json!({
            "gpu": GpuInfo::from_adapter(&gpu.adapter),
            "presentation": {
                "kind": "windows-desktop",
                "compositor": match raised {
                    Some(true) => Some("Windows (raised desktop)"),
                    Some(false) => Some("Windows (WorkerW)"),
                    None => None,
                },
                "fractional_scale": true,
                "global_pointer": false,
                "visibility_pause": true,
            },
            "paused": self.paused,
        }))
    }

    fn outputs_json(&self) -> Value {
        let outputs: Vec<Value> = self
            .outputs
            .iter()
            .map(|s| {
                let m = &s.monitor;
                let w = s.wallpaper.as_ref();
                let scale = f64::from(m.dpi) / 96.0;
                json!({
                    "name": m.name,
                    "description": if m.primary { "Primary display" } else { "Display" },
                    "make": null,
                    "model": null,
                    // Windows lays monitors out in physical pixels; the
                    // editor only needs a consistent space for its map.
                    "logical_position": (m.rect.left, m.rect.top),
                    "logical_size": m.size(),
                    "scale_factor": scale,
                    "transform": "Normal",
                    "assigned": self.assignments.get(&m.name),
                    "video": w.and_then(|w| w.player.as_ref()).map(|p| json!({
                        "codec": p.info.codec,
                        "size": (p.info.width, p.info.height),
                        "frame_rate": p.info.frame_rate,
                        "duration": p.info.duration,
                        "position": p.position_in_video(Instant::now()),
                        "paused": p.is_paused(),
                        "hidden": s.covered || w.is_some_and(|w| w.window.is_none()),
                    })),
                    "surface": w.map(|w| json!({
                        "logical_size": m.size(),
                        "buffer_size": m.size(),
                        "scale": scale,
                        "presenting": w.target.is_some(),
                        "visible": !s.covered,
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
    }))
    .context("no compatible GPU adapter")?;
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
    })
}
