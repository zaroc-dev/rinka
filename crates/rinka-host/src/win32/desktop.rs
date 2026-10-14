//! Explorer's desktop and where wallpaper windows go in it.
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

use anyhow::{Context, bail};
use windows_sys::Win32::Foundation::{FALSE, HWND, LPARAM, POINT, RECT, TRUE};
use windows_sys::Win32::Graphics::Gdi::MapWindowPoints;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, EnumWindows, FindWindowExW, FindWindowW, GWL_EXSTYLE, GetWindowLongPtrW,
    HWND_TOP, LWA_ALPHA, SMTO_NORMAL, SWP_NOACTIVATE, SendMessageTimeoutW,
    SetLayeredWindowAttributes, SetWindowPos, WS_CHILD, WS_CLIPSIBLINGS, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_NOPARENTNOTIFY, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TRANSPARENT,
};

use super::ui::WALLPAPER_CLASS;
use super::{Hwnd, hinstance, wide};

/// Undocumented: asks `Progman` to render the wallpaper in a `WorkerW`
/// behind the icon view. Harmless when that split already exists.
const SPAWN_WORKERW: u32 = 0x052C;

/// Where wallpaper windows go.
#[derive(Clone)]
pub struct Desktop {
    /// `Progman` on raised desktops, the wallpaper `WorkerW` otherwise.
    parent: Hwnd,
    /// The icon view; wallpapers sit directly below it on raised desktops.
    icons: Hwnd,
    /// Raised desktop (Windows 11 24H2+): wallpapers must be layered.
    pub raised: bool,
}

impl Desktop {
    pub fn find() -> anyhow::Result<Self> {
        let progman_class = wide("Progman");
        let icons_class = wide("SHELLDLL_DefView");
        let worker_class = wide("WorkerW");
        // SAFETY: Win32 window queries with NUL-terminated class names; the
        // enumeration callback only writes through the pointer it is given.
        unsafe {
            let progman = FindWindowW(progman_class.as_ptr(), std::ptr::null());
            let Some(progman) = Hwnd::from_raw(progman) else {
                bail!("the Windows desktop (explorer's Progman window) is not running");
            };
            let mut result = 0;
            SendMessageTimeoutW(
                progman.raw(),
                SPAWN_WORKERW,
                0xD,
                0x1,
                SMTO_NORMAL,
                1000,
                &mut result,
            );
            // Splitting the desktop may itself change this style.
            let raised = GetWindowLongPtrW(progman.raw(), GWL_EXSTYLE) as u32
                & WS_EX_NOREDIRECTIONBITMAP
                != 0;
            if raised {
                let icons = FindWindowExW(
                    progman.raw(),
                    std::ptr::null_mut(),
                    icons_class.as_ptr(),
                    std::ptr::null(),
                );
                let icons = Hwnd::from_raw(icons).context(
                    "the raised desktop has no icon view; Explorer may still be starting",
                )?;
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
            let (Some(worker), Some(icons)) =
                (Hwnd::from_raw(search.worker), Hwnd::from_raw(search.icons))
            else {
                bail!("explorer did not create a wallpaper WorkerW behind the desktop icons");
            };
            log::info!("desktop: WorkerW {worker:?}");
            Ok(Self {
                parent: worker,
                icons,
                raised: false,
            })
        }
    }

    /// Creates a hidden wallpaper window covering `rect` (screen pixels).
    /// Window thread only (`Ui::create_wallpaper`).
    pub(super) fn create_window(&self, rect: RECT) -> anyhow::Result<Hwnd> {
        // No WM_PARENTNOTIFY: creating a child would otherwise wait for
        // Explorer to handle that message.
        let ex_style = WS_EX_NOACTIVATE
            | WS_EX_TRANSPARENT
            | WS_EX_NOPARENTNOTIFY
            | if self.raised { WS_EX_LAYERED } else { 0 };
        let class = wide(WALLPAPER_CLASS);
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
                self.parent.raw(),
                std::ptr::null_mut(),
                hinstance(),
                std::ptr::null(),
            )
        };
        let hwnd = Hwnd::from_raw(hwnd).with_context(|| {
            format!(
                "creating a wallpaper window: {}",
                std::io::Error::last_os_error()
            )
        })?;
        if self.raised {
            // SAFETY: our window.
            unsafe { SetLayeredWindowAttributes(hwnd.raw(), 0, 255, LWA_ALPHA) };
        }
        self.place(hwnd, rect);
        Ok(hwnd)
    }

    /// Moves a wallpaper window over `rect` (screen pixels). Window thread
    /// only (`Ui::place`).
    pub(super) fn place(&self, hwnd: Hwnd, rect: RECT) {
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
        let after = if self.raised {
            self.icons.raw()
        } else {
            HWND_TOP
        };
        // SAFETY: our window and the live parent.
        unsafe {
            // The parent's client area spans the whole virtual screen, whose
            // origin can be negative (monitors left of or above the primary).
            MapWindowPoints(
                std::ptr::null_mut(),
                self.parent.raw(),
                points.as_mut_ptr(),
                2,
            );
            SetWindowPos(
                hwnd.raw(),
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
