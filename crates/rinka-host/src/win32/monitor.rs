//! Connected monitors, from GDI.

use windows_sys::Win32::Foundation::{LPARAM, RECT, TRUE};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;

/// A connected monitor.
#[derive(Clone)]
pub struct Monitor {
    /// `DISPLAY1`, from the GDI device name `\\.\DISPLAY1`.
    pub name: String,
    /// Screen pixels in the virtual-screen coordinate space.
    pub rect: RECT,
    pub dpi: u32,
    pub primary: bool,
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
    pub fn size(&self) -> (u32, u32) {
        (
            (self.rect.right - self.rect.left).max(0) as u32,
            (self.rect.bottom - self.rect.top).max(0) as u32,
        )
    }

    pub fn all() -> Vec<Monitor> {
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

/// `RECT` comparison (windows-sys types don't implement `PartialEq`).
fn same_rect(a: &RECT, b: &RECT) -> bool {
    (a.left, a.top, a.right, a.bottom) == (b.left, b.top, b.right, b.bottom)
}
