//! Which monitors other windows cover completely. Runs on the window thread.
//!
//! Windows has no frame callback that stops for hidden surfaces, so the
//! window stack is read instead: top-level windows from the top down to
//! Explorer's desktop, skipping invisible, minimized, cloaked (other virtual
//! desktops, suspended apps), click-through and translucent layered windows.
//! WinEvent hooks schedule a recheck; checks are coalesced, and while any
//! monitor counts as covered a slow poll catches events that were missed, so
//! a visible wallpaper never stays frozen.

use std::cell::RefCell;
use std::collections::HashMap;

use windows_sys::Win32::Foundation::{FALSE, HWND, LPARAM, RECT, TRUE};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CHILDID_SELF, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE,
    EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND,
    EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MOVESIZEEND, EnumWindows,
    GA_ROOT, GWL_EXSTYLE, GetAncestor, GetClassNameW, GetLayeredWindowAttributes,
    GetWindowLongPtrW, GetWindowRect, IsIconic, IsWindowVisible, KillTimer, LWA_ALPHA,
    LWA_COLORKEY, OBJID_WINDOW, SetTimer, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
    WS_EX_LAYERED, WS_EX_TRANSPARENT,
};

use super::Hwnd;
use super::monitor::Monitor;
use crate::rects::{Rect, fully_covered};

/// Timer ids on the message window.
pub const TIMER_RECHECK: usize = 1;
pub const TIMER_POLL: usize = 2;

/// Delay that coalesces bursts of window events (dragging, animations).
const RECHECK_MS: u32 = 100;
/// Poll interval while a monitor counts as covered.
const POLL_MS: u32 = 1000;

/// Output name → completely covered.
pub type Coverage = HashMap<String, bool>;

struct State {
    message: Hwnd,
    hooks: Vec<HWINEVENTHOOK>,
    pending: bool,
    last: Option<Coverage>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Installs the hooks and schedules the first check. `message` receives
/// the timers; pass them to `on_timer`.
pub fn install(message: Hwnd) {
    let ranges = [
        (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
        (EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZEEND),
        (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
        // Destroy, show and hide.
        (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE),
        (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
        (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
    ];
    let mut hooks = Vec::new();
    for (min, max) in ranges {
        // SAFETY: an out-of-context hook; its callback runs on this thread
        // from the message loop.
        let hook = unsafe {
            SetWinEventHook(
                min,
                max,
                std::ptr::null_mut(),
                Some(on_event),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        if hook.is_null() {
            log::warn!("window event hook {min:#x}..{max:#x} failed; coverage may lag");
        } else {
            hooks.push(hook);
        }
    }
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            message,
            hooks,
            pending: false,
            last: None,
        })
    });
    schedule();
}

/// Removes the hooks.
pub fn uninstall() {
    if let Some(state) = STATE.with(|s| s.borrow_mut().take()) {
        for hook in state.hooks {
            // SAFETY: a hook installed by `install` on this thread.
            unsafe { UnhookWinEvent(hook) };
        }
    }
}

/// Checks again shortly, unless a check is already scheduled.
pub fn schedule() {
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut()
            && !state.pending
        {
            state.pending = true;
            // SAFETY: a timer on our own window.
            unsafe { SetTimer(state.message.raw(), TIMER_RECHECK, RECHECK_MS, None) };
        }
    });
}

/// Handles a coverage timer. Returns the new coverage when it changed.
pub fn on_timer(id: usize) -> Option<Coverage> {
    if id != TIMER_RECHECK && id != TIMER_POLL {
        return None;
    }
    let coverage = compute();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let state = s.as_mut()?;
        // SAFETY: timers on our own window.
        unsafe {
            if id == TIMER_RECHECK {
                KillTimer(state.message.raw(), TIMER_RECHECK);
                state.pending = false;
            }
            if coverage.values().any(|&covered| covered) {
                SetTimer(state.message.raw(), TIMER_POLL, POLL_MS, None);
            } else {
                KillTimer(state.message.raw(), TIMER_POLL);
            }
        }
        if state.last.as_ref() == Some(&coverage) {
            return None;
        }
        state.last = Some(coverage.clone());
        Some(coverage)
    })
}

unsafe extern "system" fn on_event(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    _time: u32,
) {
    // Only whole top-level windows; caret and cursor moves also arrive as
    // location changes.
    if object != OBJID_WINDOW || child != CHILDID_SELF as i32 || hwnd.is_null() {
        return;
    }
    // SAFETY: a plain window query.
    if unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd {
        return;
    }
    schedule();
}

fn compute() -> Coverage {
    let covers = covering_rects();
    Monitor::all()
        .into_iter()
        .map(|m| {
            let r = m.rect;
            let target = Rect {
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: r.bottom,
            };
            (m.name, fully_covered(target, &covers))
        })
        .collect()
}

/// Rectangles of opaque top-level windows above the desktop.
fn covering_rects() -> Vec<Rect> {
    unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> windows_sys::core::BOOL {
        // SAFETY: `data` is the Vec passed below; window queries only read.
        unsafe {
            let list = &mut *(data as *mut Vec<Rect>);
            if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
                return TRUE;
            }
            // Explorer's desktop is at the bottom of the stack; nothing
            // below it covers the wallpaper.
            let mut class = [0u16; 32];
            let len = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
            let class = String::from_utf16_lossy(&class[..len.max(0) as usize]);
            if class == "Progman" || class == "WorkerW" {
                return FALSE;
            }
            let mut cloaked = 0u32;
            if DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED as u32,
                (&mut cloaked as *mut u32).cast(),
                4,
            ) == 0
                && cloaked != 0
            {
                return TRUE;
            }
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            if ex & WS_EX_TRANSPARENT != 0 {
                // Click-through overlays (game and recording overlays).
                return TRUE;
            }
            if ex & WS_EX_LAYERED != 0 && !opaque_layered(hwnd) {
                return TRUE;
            }
            if let Some(rect) = frame_bounds(hwnd) {
                list.push(rect);
            }
            TRUE
        }
    }
    let mut list: Vec<Rect> = Vec::new();
    // SAFETY: the callback only runs during this call.
    unsafe { EnumWindows(Some(visit), &mut list as *mut Vec<Rect> as LPARAM) };
    list
}

/// A layered window counts as opaque only with constant full alpha and no
/// color key. Per-pixel alpha windows (`UpdateLayeredWindow`) have no
/// attributes to read and count as translucent.
fn opaque_layered(hwnd: HWND) -> bool {
    let (mut key, mut alpha, mut flags) = (0u32, 0u8, 0u32);
    // SAFETY: a plain window query.
    let ok = unsafe { GetLayeredWindowAttributes(hwnd, &mut key, &mut alpha, &mut flags) };
    ok != 0 && flags & LWA_COLORKEY == 0 && (flags & LWA_ALPHA == 0 || alpha == 255)
}

/// The visible frame, without the invisible resize borders that
/// `GetWindowRect` includes.
fn frame_bounds(hwnd: HWND) -> Option<Rect> {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: `r` is a RECT, as the attribute requires.
    let ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&mut r as *mut RECT).cast(),
            std::mem::size_of::<RECT>() as u32,
        ) == 0
            || GetWindowRect(hwnd, &mut r) != 0
    };
    ok.then_some(Rect {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    })
}
