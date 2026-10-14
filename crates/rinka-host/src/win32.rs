//! Windows presentation: one child window per monitor behind the desktop
//! icons.
//!
//! Explorer's desktop is the `Progman` window with the icon view
//! (`SHELLDLL_DefView`). The undocumented message 0x052C makes `Progman` put
//! the wallpaper in a separate `WorkerW` window behind the icons. Wallpapers
//! are child windows between the two:
//!
//! - Windows 11 24H2 and later ("raised desktop": `Progman` has
//!   `WS_EX_NOREDIRECTIONBITMAP`): the icon view and the `WorkerW` are both
//!   children of `Progman`. Each wallpaper is a layered child of `Progman`
//!   with full opacity, z-ordered directly below the icon view, as Microsoft
//!   documents for this layout.
//! - Earlier versions: the `WorkerW` is a top-level window behind the one
//!   holding the icon view, and wallpapers are its children.
//!
//! Wallpaper windows take no input (`WS_EX_TRANSPARENT`, `HTTRANSPARENT`) and
//! never activate. Static content is drawn when a window is created or moved
//! or its project changes. Animated scenes are drawn from a high-resolution
//! waitable timer at their frame rate; presents use Fifo, so drawing never
//! outruns the display. Unlike Wayland there is no frame callback, so hidden
//! wallpapers are not detected yet and keep animating.

use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroIsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, Win32WindowHandle, WindowsDisplayHandle,
};
use rinka_engine::{FrameOutcome, GpuInfo, Project, Renderer, Source, SurfaceSetup, gpu};
use serde_json::{Value, json};
use windows_sys::Win32::Foundation::{
    FALSE, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    MapWindowPoints,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CancelWaitableTimer, CreateWaitableTimerExW, INFINITE,
    SetWaitableTimer, TIMER_ALL_ACCESS,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows, FindWindowExW,
    FindWindowW, GWL_EXSTYLE, GetWindowLongPtrW, HTTRANSPARENT, HWND_TOP, IsWindow, LWA_ALPHA,
    MA_NOACTIVATE, MONITORINFOF_PRIMARY, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx,
    PM_REMOVE, PeekMessageW, PostMessageW, QS_ALLINPUT, RegisterClassExW, RegisterWindowMessageW,
    SMTO_NORMAL, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SendMessageTimeoutW,
    SetLayeredWindowAttributes, SetWindowPos, ShowWindow, TranslateMessage, WM_APP, WM_DESTROY,
    WM_DISPLAYCHANGE, WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_NCHITTEST, WNDCLASSEXW, WS_CHILD,
    WS_CLIPSIBLINGS, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW,
    WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::control::{self, Command};
use crate::project_io::{self, ImageCache};
use crate::protocol::{Error, ErrorCode, Reply, Request};

/// Frame rate of animated scenes.
const SCENE_FPS: f64 = 30.0;

/// Undocumented: asks `Progman` to render the wallpaper in a `WorkerW`
/// behind the icon view. Harmless when that split already exists.
const SPAWN_WORKERW: u32 = 0x052C;

/// Posted to the message window when a control request is queued.
const WM_RINKA_COMMAND: u32 = WM_APP + 1;

/// After the shell destroys a wallpaper window, wait this long before
/// rebuilding, unless explorer announces its restart sooner.
const REBUILD_DELAY: Duration = Duration::from_secs(2);

/// `RegisterWindowMessageW("TaskbarCreated")`: broadcast when explorer
/// (re)starts, after which wallpaper windows must be recreated.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// Things the window procedure saw, handled by the loop after dispatch.
enum Event {
    DisplayChanged,
    ShellRestarted,
    Destroyed(HWND),
}

thread_local! {
    static EVENTS: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
}

fn push_event(event: Event) {
    EVENTS.with(|e| e.borrow_mut().push(event));
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
pub fn run(socket: PathBuf, assignments: HashMap<String, PathBuf>) -> anyhow::Result<()> {
    let classes = Classes::register()?;
    let name = wide("TaskbarCreated");
    // SAFETY: NUL-terminated string.
    TASKBAR_CREATED.store(
        unsafe { RegisterWindowMessageW(name.as_ptr()) },
        Ordering::Relaxed,
    );

    // A hidden top-level window: it wakes the loop for control requests and
    // receives the broadcasts (display changes, explorer restarts) that
    // message-only windows miss.
    let title = wide("Rinka host");
    // SAFETY: registered class, NUL-terminated strings; checked below.
    let message_window = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            classes.message.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance(),
            std::ptr::null(),
        )
    };
    if message_window.is_null() {
        bail!(
            "creating the message window: {}",
            std::io::Error::last_os_error()
        );
    }
    let timer = WaitableTimer::new()?;

    let mut host = Host {
        classes,
        instance: wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
        ),
        gpu: None,
        images: ImageCache::default(),
        desktop: None,
        rebuild_at: None,
        outputs: Vec::new(),
        assignments,
        paused: false,
        stop: false,
    };
    host.desktop = Some(Desktop::find()?);
    // Learn the monitors before accepting requests.
    host.sync_outputs();

    let (tx, rx) = mpsc::channel::<Command>();
    let listener = control::bind(&socket)?;
    log::info!("control pipe: {}", socket.display());
    let wake = message_window as usize;
    control::serve(listener, move |cmd| {
        // SAFETY: posting to a window owned by the loop thread; a failure
        // only means the loop has gone away.
        tx.send(cmd).is_ok() && unsafe { PostMessageW(wake as HWND, WM_RINKA_COMMAND, 0, 0) } != 0
    });

    loop {
        pump_messages();
        // Invalidate destroyed surfaces before a queued command can draw.
        host.process_events();
        while let Ok(cmd) = rx.try_recv() {
            let reply = host.handle(cmd.request);
            let _ = cmd.reply.send(reply);
            if host.stop {
                // Wait for the shutdown reply instead of racing the pipe
                // thread with process exit. A stalled client cannot hold
                // shutdown indefinitely.
                let _ = cmd.delivered.recv_timeout(Duration::from_secs(1));
                break;
            }
        }
        if host.stop {
            break;
        }
        timer.arm(host.tick(Instant::now()));
        timer.wait_or_message();
    }
    drop(host);
    // SAFETY: our own window, on its thread.
    unsafe { DestroyWindow(message_window) };
    Ok(())
}

fn pump_messages() {
    // SAFETY: standard message loop on the thread that owns the windows.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // Clicks and hover fall through to whatever is beneath.
        WM_NCHITTEST => return HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => return MA_NOACTIVATE as LRESULT,
        // The swap chain covers the window; GDI must not paint over it.
        WM_ERASEBKGND => return 1,
        WM_DISPLAYCHANGE => push_event(Event::DisplayChanged),
        WM_DESTROY => push_event(Event::Destroyed(hwnd)),
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            push_event(Event::ShellRestarted)
        }
        _ => {}
    }
    // SAFETY: forwarding the message we were given.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Registered window class names.
struct Classes {
    wallpaper: Vec<u16>,
    message: Vec<u16>,
}

impl Classes {
    fn register() -> anyhow::Result<Self> {
        let classes = Self {
            wallpaper: wide("RinkaWallpaper"),
            message: wide("RinkaHost"),
        };
        for name in [&classes.wallpaper, &classes.message] {
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(window_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance(),
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                // No background brush: nothing is painted before the first
                // present.
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: name.as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };
            // SAFETY: a fully initialized class description.
            if unsafe { RegisterClassExW(&class) } == 0 {
                bail!(
                    "registering a window class: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
        Ok(classes)
    }
}

/// A high-resolution waitable timer that wakes the loop for animation.
struct WaitableTimer(HANDLE);

impl WaitableTimer {
    fn new() -> anyhow::Result<Self> {
        // SAFETY: plain creation calls; the handle is checked.
        let mut handle = unsafe {
            CreateWaitableTimerExW(
                std::ptr::null(),
                std::ptr::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS,
            )
        };
        if handle.is_null() {
            // Before Windows 10 1803: a normal timer (~15.6 ms granularity).
            // SAFETY: as above.
            handle = unsafe {
                CreateWaitableTimerExW(std::ptr::null(), std::ptr::null(), 0, TIMER_ALL_ACCESS)
            };
        }
        if handle.is_null() {
            bail!(
                "creating a waitable timer: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(Self(handle))
    }

    /// Fires after `wait`, or never with `None`.
    fn arm(&self, wait: Option<Duration>) {
        // SAFETY: a valid timer handle.
        unsafe {
            match wait {
                Some(wait) => {
                    // Negative: relative, in 100 ns units.
                    let due = -((wait.as_nanos() / 100).max(1) as i64);
                    SetWaitableTimer(self.0, &due, 0, None, std::ptr::null(), FALSE);
                }
                None => {
                    CancelWaitableTimer(self.0);
                }
            }
        }
    }

    /// Blocks until the timer fires or a message arrives.
    fn wait_or_message(&self) {
        // SAFETY: one valid handle.
        unsafe {
            MsgWaitForMultipleObjectsEx(1, &self.0, INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        }
    }
}

impl Drop for WaitableTimer {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

/// Where wallpaper windows go.
struct Desktop {
    /// `Progman` on raised desktops, the wallpaper `WorkerW` otherwise.
    parent: HWND,
    /// The icon view; wallpapers sit directly below it on raised desktops.
    icons: HWND,
    /// Raised desktop (Windows 11 24H2+): wallpapers must be layered.
    raised: bool,
}

impl Desktop {
    fn find() -> anyhow::Result<Self> {
        let progman_class = wide("Progman");
        let icons_class = wide("SHELLDLL_DefView");
        let worker_class = wide("WorkerW");
        // SAFETY: Win32 window queries with NUL-terminated class names; the
        // enumeration callback only writes through the pointer it is given.
        unsafe {
            let progman = FindWindowW(progman_class.as_ptr(), std::ptr::null());
            if progman.is_null() {
                bail!("the Windows desktop (explorer's Progman window) is not running");
            }
            let mut result = 0;
            SendMessageTimeoutW(
                progman,
                SPAWN_WORKERW,
                0xD,
                0x1,
                SMTO_NORMAL,
                1000,
                &mut result,
            );
            // Splitting the desktop may itself change this style.
            let raised =
                GetWindowLongPtrW(progman, GWL_EXSTYLE) as u32 & WS_EX_NOREDIRECTIONBITMAP != 0;
            if raised {
                let icons = FindWindowExW(
                    progman,
                    std::ptr::null_mut(),
                    icons_class.as_ptr(),
                    std::ptr::null(),
                );
                if icons.is_null() {
                    bail!("the raised desktop has no icon view; Explorer may still be starting");
                }
                log::info!("desktop: raised (Progman {progman:?}, icons {icons:?})");
                return Ok(Self {
                    parent: progman,
                    icons,
                    raised: true,
                });
            }
            // The top-level window holding the icon view; the wallpaper
            // WorkerW is the next top-level WorkerW after it.
            struct Search<'a> {
                icons_class: &'a [u16],
                worker_class: &'a [u16],
                icons: HWND,
                worker: HWND,
            }
            unsafe extern "system" fn visit(top: HWND, data: LPARAM) -> windows_sys::core::BOOL {
                // SAFETY: `data` is the `Search` passed below.
                unsafe {
                    let search = &mut *(data as *mut Search);
                    let icons = FindWindowExW(
                        top,
                        std::ptr::null_mut(),
                        search.icons_class.as_ptr(),
                        std::ptr::null(),
                    );
                    if icons.is_null() {
                        return TRUE;
                    }
                    search.icons = icons;
                    search.worker = FindWindowExW(
                        std::ptr::null_mut(),
                        top,
                        search.worker_class.as_ptr(),
                        std::ptr::null(),
                    );
                    FALSE
                }
            }
            let mut search = Search {
                icons_class: &icons_class,
                worker_class: &worker_class,
                icons: std::ptr::null_mut(),
                worker: std::ptr::null_mut(),
            };
            EnumWindows(Some(visit), &mut search as *mut Search as LPARAM);
            if search.worker.is_null() {
                bail!("explorer did not create a wallpaper WorkerW behind the desktop icons");
            }
            log::info!("desktop: WorkerW {:?}", search.worker);
            Ok(Self {
                parent: search.worker,
                icons: search.icons,
                raised: false,
            })
        }
    }

    /// Creates a hidden wallpaper window covering `rect` (screen pixels).
    fn create_window(&self, class: &[u16], rect: RECT) -> anyhow::Result<HWND> {
        let ex_style =
            WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | if self.raised { WS_EX_LAYERED } else { 0 };
        let title = wide("Rinka wallpaper");
        // SAFETY: registered class, NUL-terminated strings, a live parent;
        // the handle is checked.
        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                class.as_ptr(),
                title.as_ptr(),
                WS_CHILD | WS_CLIPSIBLINGS,
                0,
                0,
                0,
                0,
                self.parent,
                std::ptr::null_mut(),
                hinstance(),
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            bail!(
                "creating a wallpaper window: {}",
                std::io::Error::last_os_error()
            );
        }
        if self.raised {
            // SAFETY: our window.
            unsafe { SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA) };
        }
        self.place(hwnd, rect);
        Ok(hwnd)
    }

    /// Moves a wallpaper window over `rect` (screen pixels).
    fn place(&self, hwnd: HWND, rect: RECT) {
        let mut points = [
            POINT {
                x: rect.left,
                y: rect.top,
            },
            POINT {
                x: rect.right,
                y: rect.bottom,
            },
        ];
        // Below the icon view on raised desktops; on top of the WorkerW's
        // (empty) child list otherwise.
        let after = if self.raised && !self.icons.is_null() {
            self.icons
        } else {
            HWND_TOP
        };
        // SAFETY: our window and the live parent.
        unsafe {
            // The parent's client area spans the whole virtual screen, whose
            // origin can be negative (monitors left of or above the primary).
            MapWindowPoints(std::ptr::null_mut(), self.parent, points.as_mut_ptr(), 2);
            SetWindowPos(
                hwnd,
                after,
                points[0].x,
                points[0].y,
                points[1].x - points[0].x,
                points[1].y - points[0].y,
                SWP_NOACTIVATE,
            );
        }
    }
}

/// A connected monitor.
#[derive(Clone)]
struct Monitor {
    /// `DISPLAY1`, from the GDI device name `\\.\DISPLAY1`.
    name: String,
    /// Screen pixels in the virtual-screen coordinate space.
    rect: RECT,
    dpi: u32,
    primary: bool,
}

impl PartialEq for Monitor {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && same_rect(&self.rect, &other.rect)
            && self.dpi == other.dpi
            && self.primary == other.primary
    }
}

impl Monitor {
    fn size(&self) -> (u32, u32) {
        (
            (self.rect.right - self.rect.left).max(0) as u32,
            (self.rect.bottom - self.rect.top).max(0) as u32,
        )
    }

    fn all() -> Vec<Monitor> {
        unsafe extern "system" fn visit(
            monitor: HMONITOR,
            _: HDC,
            _: *mut RECT,
            data: LPARAM,
        ) -> windows_sys::core::BOOL {
            // SAFETY: `data` is the Vec passed below; `info` is sized for the
            // extended structure as GetMonitorInfoW requires.
            unsafe {
                let list = &mut *(data as *mut Vec<Monitor>);
                let mut info: MONITORINFOEXW = std::mem::zeroed();
                info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
                if GetMonitorInfoW(
                    monitor,
                    &mut info as *mut MONITORINFOEXW as *mut MONITORINFO,
                ) == 0
                {
                    return TRUE;
                }
                let len = info
                    .szDevice
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(info.szDevice.len());
                let device = String::from_utf16_lossy(&info.szDevice[..len]);
                let (mut dpi_x, mut dpi_y) = (96, 96);
                GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
                list.push(Monitor {
                    name: device.trim_start_matches(r"\\.\").to_owned(),
                    rect: info.monitorInfo.rcMonitor,
                    dpi: dpi_x,
                    primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                });
                TRUE
            }
        }
        let mut list: Vec<Monitor> = Vec::new();
        // SAFETY: the callback only runs during this call.
        unsafe {
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(visit),
                &mut list as *mut Vec<Monitor> as LPARAM,
            );
        }
        list
    }
}

impl PartialEq for RectEq {
    fn eq(&self, other: &Self) -> bool {
        let (a, b) = (&self.0, &other.0);
        (a.left, a.top, a.right, a.bottom) == (b.left, b.top, b.right, b.bottom)
    }
}

/// `RECT` comparison (windows-sys types don't implement `PartialEq`).
struct RectEq(RECT);

fn same_rect(a: &RECT, b: &RECT) -> bool {
    RectEq(*a) == RectEq(*b)
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
    classes: Classes,
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
}

struct Wallpaper {
    project_dir: PathBuf,
    project: Project,
    /// Absolute path of the image asset, when the project shows one.
    image: Option<PathBuf>,
    /// Loaded assets of a scene project.
    scene: Option<std::sync::Arc<rinka_engine::SceneAssets>>,
    /// When the scene started, for its clock.
    scene_start: Instant,
    /// The scene animates and the next frame is due at this instant.
    next_frame: Option<Instant>,
    /// The project changed since the target was last updated.
    dirty: bool,
    /// Shown only after the first successful present.
    shown: bool,
    // The wgpu surface must drop before its window is destroyed; `Drop`
    // clears it first.
    target: Option<Target>,
    /// Null once the shell destroyed the window.
    hwnd: HWND,
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
}

impl Drop for Wallpaper {
    fn drop(&mut self) {
        self.target = None;
        // SAFETY: our window, destroyed on the thread that created it.
        if !self.hwnd.is_null() && unsafe { IsWindow(self.hwnd) } != 0 {
            unsafe { DestroyWindow(self.hwnd) };
        }
    }
}

impl Host {
    fn slot_index(&self, name: &str) -> Option<usize> {
        self.outputs.iter().position(|s| s.monitor.name == name)
    }

    /// Loads a project and decodes its assets, so errors reach the client
    /// before anything on screen changes.
    fn prepare(&mut self, dir: &Path) -> Result<Prepared, Error> {
        let project = project_io::load(dir)?;
        if project_io::video_asset(dir, &project)?.is_some() {
            return Err(Error::new(
                ErrorCode::Project,
                "video wallpapers are not supported on Windows yet",
            ));
        }
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
                let hwnd = self.create_window(index)?;
                let previous = self.outputs[index].wallpaper.replace(Wallpaper {
                    project_dir: prepared.dir,
                    project: prepared.project,
                    image: prepared.image,
                    scene: prepared.scene,
                    scene_start: Instant::now(),
                    next_frame: None,
                    dirty: false,
                    shown: false,
                    target: None,
                    hwnd,
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

    fn create_window(&self, index: usize) -> Result<HWND, Error> {
        let desktop = self
            .desktop
            .as_ref()
            .ok_or_else(|| Error::new(ErrorCode::Gpu, "the Windows desktop is not available"))?;
        desktop
            .create_window(&self.classes.wallpaper, self.outputs[index].monitor.rect)
            .map_err(|e| Error::new(ErrorCode::Gpu, format!("{e:#}")))
    }

    fn draw(&mut self, index: usize) -> Result<(), Error> {
        let slot = &mut self.outputs[index];
        let (width, height) = slot.monitor.size();
        let Some(w) = slot.wallpaper.as_mut() else {
            return Ok(());
        };
        if w.hwnd.is_null() || width == 0 || height == 0 {
            w.next_frame = None;
            return Ok(());
        }

        if w.target.is_none() {
            let mut window =
                Win32WindowHandle::new(NonZeroIsize::new(w.hwnd as isize).expect("window handle"));
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
            // Bind the project's image below.
            w.dirty = true;
        }
        let gpu = self.gpu.as_mut().expect("gpu initialized");
        let t = w.target.as_mut().expect("target created");
        let index = gpu.renderer_index(t.setup.render_format);
        let renderer = &gpu.renderers[index].1;
        if w.dirty {
            let image = w
                .image
                .as_deref()
                .and_then(|p| self.images.gpu_image(p, renderer, &gpu.device, &gpu.queue));
            let source = image.as_ref().map(Source::Image);
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
        let time = w.scene_start.elapsed().as_secs_f64();
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
                    // SAFETY: our child window; never activate it. Failed
                    // replacements remain hidden until they are dropped.
                    unsafe { ShowWindow(w.hwnd, SW_SHOWNOACTIVATE) };
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
        for index in 0..self.outputs.len() {
            let due = self.outputs[index]
                .wallpaper
                .as_ref()
                .and_then(|w| w.next_frame);
            let Some(mut due) = due else { continue };
            if due <= now {
                if let Err(e) = self.draw(index) {
                    log::error!("{}: {}", self.outputs[index].monitor.name, e.message);
                    self.outputs[index]
                        .wallpaper
                        .as_mut()
                        .expect("wallpaper")
                        .next_frame = None;
                }
                match self.outputs[index]
                    .wallpaper
                    .as_ref()
                    .and_then(|w| w.next_frame)
                {
                    Some(d) => due = d,
                    None => continue,
                }
            }
            let now = Instant::now();
            soonest(due.saturating_duration_since(now));
        }
        next
    }

    /// Handles what the window procedure recorded.
    fn process_events(&mut self) {
        let events = EVENTS.with(|e| std::mem::take(&mut *e.borrow_mut()));
        for event in events {
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
                Event::Destroyed(hwnd) => {
                    for slot in &mut self.outputs {
                        if let Some(w) = &mut slot.wallpaper
                            && w.hwnd == hwnd
                        {
                            log::warn!(
                                "the shell destroyed the wallpaper window on {}",
                                slot.monitor.name
                            );
                            w.target = None;
                            w.hwnd = std::ptr::null_mut();
                            w.next_frame = None;
                            self.rebuild_at
                                .get_or_insert(Instant::now() + REBUILD_DELAY);
                        }
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
                // SAFETY: our window, if it still exists.
                if !w.hwnd.is_null() && unsafe { IsWindow(w.hwnd) } != 0 {
                    unsafe { DestroyWindow(w.hwnd) };
                }
                w.hwnd = std::ptr::null_mut();
                w.next_frame = None;
                w.shown = false;
            }
        }
        // The destroy events just queued refer to windows already handled.
        EVENTS.with(|e| e.borrow_mut().retain(|e| !matches!(e, Event::Destroyed(_))));
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
                Ok(hwnd) => {
                    let w = self.outputs[index].wallpaper.as_mut().expect("wallpaper");
                    w.hwnd = hwnd;
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
        for monitor in monitors {
            match self.slot_index(&monitor.name) {
                Some(index) => {
                    let slot = &mut self.outputs[index];
                    if slot.monitor == monitor {
                        continue;
                    }
                    let moved = !same_rect(&slot.monitor.rect, &monitor.rect);
                    slot.monitor = monitor;
                    if moved
                        && let (Some(w), Some(desktop)) = (&slot.wallpaper, &self.desktop)
                        && !w.hwnd.is_null()
                    {
                        desktop.place(w.hwnd, slot.monitor.rect);
                        if let Err(e) = self.draw(index) {
                            log::error!("{}: {}", self.outputs[index].monitor.name, e.message);
                        }
                    }
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
            // Only videos pause, as on Wayland; Windows has no video yet.
            Request::Pause => {
                self.paused = true;
                Ok(Value::Null)
            }
            Request::Resume => {
                self.paused = false;
                Ok(Value::Null)
            }
            Request::Seek { output, position } => {
                if !position.is_finite() || position < 0.0 {
                    return Err(Error::new(
                        ErrorCode::BadRequest,
                        "seek position must be finite and non-negative",
                    ));
                }
                self.target_outputs(output.as_deref())?;
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
                "visibility_pause": false,
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
                    "video": null,
                    "surface": w.map(|w| json!({
                        "logical_size": m.size(),
                        "buffer_size": m.size(),
                        "scale": scale,
                        "presenting": w.target.is_some(),
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
