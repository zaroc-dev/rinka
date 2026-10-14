//! The window thread: owns every window the host creates.
//!
//! Wallpaper windows are children of Explorer's desktop window. Windows links
//! the input queues of threads whose windows are parent and child, so if the
//! thread owning the wallpaper windows stops pumping messages (decoding a
//! large image, building a scene, waiting for vsync), Explorer's desktop
//! stops responding with it. This thread therefore only pumps messages and
//! runs short window calls for the render thread (`Ui::call`); everything
//! slow happens on the render thread.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;

use anyhow::{Context, bail};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowThreadProcessId, HTTRANSPARENT, MA_NOACTIVATE, MSG, PostMessageW, PostQuitMessage,
    RegisterClassExW, RegisterWindowMessageW, SW_SHOWNOACTIVATE, SendMessageW, ShowWindow,
    TranslateMessage, WM_APP, WM_CLOSE, WM_DESTROY, WM_DISPLAYCHANGE, WM_ERASEBKGND,
    WM_MOUSEACTIVATE, WM_NCHITTEST, WNDCLASSEXW, WNDPROC, WS_EX_TOOLWINDOW, WS_POPUP,
};

use super::desktop::Desktop;
use super::timer::Wake;
use super::{Hwnd, hinstance, wide};

/// Runs a job on the window thread (`Ui::call`).
const WM_RINKA_CALL: u32 = WM_APP + 1;

pub const WALLPAPER_CLASS: &str = "RinkaWallpaper";
const MESSAGE_CLASS: &str = "RinkaHost";

/// `RegisterWindowMessageW("TaskbarCreated")`: broadcast when Explorer
/// (re)starts, after which wallpaper windows must be recreated.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// What the window thread saw, for the render thread.
pub enum Event {
    DisplayChanged,
    ShellRestarted,
    /// Something other than the host destroyed a wallpaper window.
    Destroyed(Hwnd),
}

thread_local! {
    static EVENTS: RefCell<Option<(mpsc::Sender<Event>, Arc<Wake>)>> = const { RefCell::new(None) };
    /// The window the host is destroying itself, which is not an event.
    static DESTROYING: Cell<isize> = const { Cell::new(0) };
}

fn push_event(event: Event) {
    EVENTS.with(|e| {
        if let Some((tx, wake)) = &*e.borrow() {
            let _ = tx.send(event);
            wake.set();
        }
    });
}

/// Handle to the window thread. Dropping it ends the thread.
pub struct Ui {
    /// Hidden top-level window: runs calls and receives the broadcasts
    /// (display changes, Explorer restarts) that message-only windows miss.
    message: Hwnd,
    thread: Option<JoinHandle<()>>,
}

impl Ui {
    /// Starts the window thread. Its events arrive on the returned channel,
    /// each followed by `wake.set()`.
    pub fn start(wake: Arc<Wake>) -> anyhow::Result<(Self, mpsc::Receiver<Event>)> {
        let (events, received) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("rinka-windows".into())
            .spawn(move || {
                let message = match setup() {
                    Ok(message) => message,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                EVENTS.with(|e| *e.borrow_mut() = Some((events, wake)));
                let _ = ready_tx.send(Ok(message));
                pump();
            })
            .context("starting the window thread")?;
        let message = ready_rx.recv().context("the window thread exited")??;
        Ok((
            Self {
                message,
                thread: Some(thread),
            },
            received,
        ))
    }

    /// Runs `f` on the window thread and returns its result. Window
    /// handles must be created and destroyed on the thread that owns them.
    pub fn call<R: Send>(&self, f: impl FnOnce() -> R + Send) -> R {
        let mut result = None;
        {
            let mut f = Some(f);
            let mut job = || result = Some((f.take().expect("called once"))());
            let mut job: &mut dyn FnMut() = &mut job;
            // SAFETY: SendMessageW returns only after the window thread ran
            // the job, so the borrowed closure outlives its use there.
            unsafe {
                SendMessageW(
                    self.message.raw(),
                    WM_RINKA_CALL,
                    0,
                    &mut job as *mut &mut dyn FnMut() as LPARAM,
                )
            };
        }
        result.expect("the window thread is running")
    }

    /// Creates a hidden wallpaper window covering `rect` (screen pixels).
    pub fn create_wallpaper(&self, desktop: &Desktop, rect: RECT) -> anyhow::Result<Hwnd> {
        let desktop = desktop.clone();
        self.call(move || desktop.create_window(rect))
    }

    /// Moves a wallpaper window over `rect` (screen pixels).
    pub fn place(&self, desktop: &Desktop, hwnd: Hwnd, rect: RECT) {
        let desktop = desktop.clone();
        self.call(move || desktop.place(hwnd, rect));
    }

    /// Shows a wallpaper window without activating it.
    pub fn show(&self, hwnd: Hwnd) {
        // SAFETY: a window owned by the window thread.
        self.call(move || unsafe { ShowWindow(hwnd.raw(), SW_SHOWNOACTIVATE) });
    }

    /// Destroys one of our windows. A handle the shell already destroyed is
    /// ignored, even if Windows has reused it for another process's window.
    pub fn destroy(&self, hwnd: Hwnd) {
        self.call(move || {
            // SAFETY: plain queries, and DestroyWindow on a window this
            // thread owns.
            unsafe {
                if GetWindowThreadProcessId(hwnd.raw(), std::ptr::null_mut())
                    == GetCurrentThreadId()
                {
                    DESTROYING.set(hwnd.0);
                    DestroyWindow(hwnd.raw());
                    DESTROYING.set(0);
                }
            }
        });
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        // SAFETY: posting to our own window; the thread ends its loop.
        unsafe { PostMessageW(self.message.raw(), WM_CLOSE, 0, 0) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Registers the window classes and creates the message window.
fn setup() -> anyhow::Result<Hwnd> {
    let name = wide("TaskbarCreated");
    // SAFETY: NUL-terminated string.
    TASKBAR_CREATED.store(
        unsafe { RegisterWindowMessageW(name.as_ptr()) },
        Ordering::Relaxed,
    );
    register_class(WALLPAPER_CLASS, Some(wallpaper_proc))?;
    register_class(MESSAGE_CLASS, Some(message_proc))?;
    let class = wide(MESSAGE_CLASS);
    let title = wide("Rinka host");
    // SAFETY: registered class, NUL-terminated strings; checked below.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
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
    Hwnd::from_raw(hwnd).with_context(|| {
        format!(
            "creating the message window: {}",
            std::io::Error::last_os_error()
        )
    })
}

fn register_class(name: &str, proc: WNDPROC) -> anyhow::Result<()> {
    let name = wide(name);
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: proc,
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance(),
        hIcon: std::ptr::null_mut(),
        hCursor: std::ptr::null_mut(),
        // No background brush: nothing is painted before the first present.
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
    Ok(())
}

fn pump() {
    // SAFETY: standard message loop on the thread that owns the windows.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn message_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_RINKA_CALL => {
            // SAFETY: `Ui::call` passes a pointer to its job and waits.
            unsafe { (*(lparam as *mut &mut dyn FnMut()))() };
            return 0;
        }
        WM_DISPLAYCHANGE => push_event(Event::DisplayChanged),
        WM_CLOSE => {
            // SAFETY: our own window, on its thread.
            unsafe { DestroyWindow(hwnd) };
            return 0;
        }
        // SAFETY: ends this thread's message loop.
        WM_DESTROY => unsafe { PostQuitMessage(0) },
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            push_event(Event::ShellRestarted)
        }
        _ => {}
    }
    // SAFETY: forwarding the message we were given.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe extern "system" fn wallpaper_proc(
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
        WM_DESTROY if DESTROYING.get() != hwnd as isize => {
            if let Some(hwnd) = Hwnd::from_raw(hwnd) {
                push_event(Event::Destroyed(hwnd));
            }
        }
        _ => {}
    }
    // SAFETY: forwarding the message we were given.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
