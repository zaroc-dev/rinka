//! What the render thread waits on: an animation timer and a wake event.

use std::time::Duration;

use anyhow::bail;
use windows_sys::Win32::Foundation::{CloseHandle, FALSE, HANDLE};
use windows_sys::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CancelWaitableTimer, CreateEventW,
    CreateWaitableTimerExW, INFINITE, SetEvent, SetWaitableTimer, TIMER_ALL_ACCESS,
    WaitForMultipleObjects,
};

/// A high-resolution waitable timer that wakes the loop for animation.
pub struct WaitableTimer(HANDLE);

impl WaitableTimer {
    pub fn new() -> anyhow::Result<Self> {
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
    pub fn arm(&self, wait: Option<Duration>) {
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

    /// Blocks until the timer fires or `wake` is set.
    pub fn wait(&self, wake: &Wake) {
        let handles = [self.0, wake.0];
        // SAFETY: two valid handles.
        unsafe { WaitForMultipleObjects(2, handles.as_ptr(), FALSE, INFINITE) };
    }
}

impl Drop for WaitableTimer {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe { CloseHandle(self.0) };
    }
}

/// An auto-reset event that other threads set when the render thread has
/// work: a control request or a window event.
pub struct Wake(HANDLE);

// SAFETY: event handles may be used from any thread.
unsafe impl Send for Wake {}
unsafe impl Sync for Wake {}

impl Wake {
    pub fn new() -> anyhow::Result<Self> {
        // SAFETY: an unnamed auto-reset event; the handle is checked.
        let handle = unsafe { CreateEventW(std::ptr::null(), FALSE, FALSE, std::ptr::null()) };
        if handle.is_null() {
            bail!("creating an event: {}", std::io::Error::last_os_error());
        }
        Ok(Self(handle))
    }

    pub fn set(&self) {
        // SAFETY: a valid event handle.
        unsafe { SetEvent(self.0) };
    }
}

impl Drop for Wake {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe { CloseHandle(self.0) };
    }
}
