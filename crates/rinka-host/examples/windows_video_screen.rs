//! Windows acceptance helper: actual desktop pixels and a generated cover
//! window that closes itself. Never changes or hides the user's windows.

#[cfg(windows)]
mod windows {
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    pub fn run() -> anyhow::Result<()> {
        // SAFETY: this helper owns its only thread and uses physical pixels.
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        let args: Vec<String> = std::env::args().collect();
        anyhow::ensure!(
            args.len() >= 6,
            "usage: windows_video_screen capture|cover X Y W H [OUT.PNG]"
        );
        let x: i32 = args[2].parse()?;
        let y: i32 = args[3].parse()?;
        let width: i32 = args[4].parse()?;
        let height: i32 = args[5].parse()?;
        anyhow::ensure!(
            (1..=8192).contains(&width) && (1..=8192).contains(&height),
            "invalid capture size"
        );
        match args[1].as_str() {
            "capture" => capture(
                x,
                y,
                width,
                height,
                args.get(6)
                    .ok_or_else(|| anyhow::anyhow!("missing output PNG"))?,
            ),
            "cover" => cover(x, y, width, height),
            _ => anyhow::bail!("unknown acceptance helper mode"),
        }
    }

    fn capture(x: i32, y: i32, width: i32, height: i32, out: &str) -> anyhow::Result<()> {
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        // SAFETY: compatible DC/bitmap handles are checked, the DIB buffer is
        // exactly width*height*4 bytes, and every GDI resource is released.
        unsafe {
            let screen = GetDC(null_mut());
            anyhow::ensure!(!screen.is_null(), "GetDC failed");
            let memory = CreateCompatibleDC(screen);
            let bitmap = CreateCompatibleBitmap(screen, width, height);
            if memory.is_null() || bitmap.is_null() {
                if !memory.is_null() {
                    DeleteDC(memory);
                }
                if !bitmap.is_null() {
                    DeleteObject(bitmap);
                }
                ReleaseDC(null_mut(), screen);
                anyhow::bail!("creating capture resources failed");
            }
            let previous = SelectObject(memory, bitmap);
            let copied = BitBlt(
                memory,
                0,
                0,
                width,
                height,
                screen,
                x,
                y,
                SRCCOPY | CAPTUREBLT,
            );
            SelectObject(memory, previous);
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..Default::default()
                },
                ..Default::default()
            };
            let rows = GetDIBits(
                memory,
                bitmap,
                0,
                height as u32,
                pixels.as_mut_ptr().cast(),
                &mut info,
                DIB_RGB_COLORS,
            );
            DeleteObject(bitmap);
            DeleteDC(memory);
            ReleaseDC(null_mut(), screen);
            anyhow::ensure!(
                copied != 0 && rows == height,
                "copying desktop pixels failed"
            );
        }
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        let center = ((height / 2 * width + width / 2) * 4) as usize;
        image::save_buffer(
            out,
            &pixels,
            width as u32,
            height as u32,
            image::ExtendedColorType::Rgba8,
        )?;
        println!(
            "{}",
            serde_json::json!({ "rgb": &pixels[center..center + 3] })
        );
        Ok(())
    }

    unsafe extern "system" fn procedure(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        // SAFETY: called by Windows with a live window; only the test timer
        // destroys it. All other messages go to the default procedure.
        unsafe {
            match message {
                WM_TIMER => {
                    DestroyWindow(hwnd);
                    0
                }
                WM_DESTROY => {
                    PostQuitMessage(0);
                    0
                }
                _ => DefWindowProcW(hwnd, message, w, l),
            }
        }
    }

    fn cover(x: i32, y: i32, width: i32, height: i32) -> anyhow::Result<()> {
        let class: Vec<u16> = "RinkaVideoAcceptanceCover\0".encode_utf16().collect();
        // SAFETY: class/title buffers outlive the window. The message loop
        // owns the window and its timer; the class brush is released last.
        unsafe {
            // Match the host's physical monitor coordinates on mixed-DPI desktops.
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let instance = GetModuleHandleW(std::ptr::null());
            let brush = CreateSolidBrush(30 | (111 << 8) | (115 << 16));
            let definition = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hbrBackground: brush,
                lpszClassName: class.as_ptr(),
                ..Default::default()
            };
            anyhow::ensure!(
                RegisterClassW(&definition) != 0,
                "registering cover window failed"
            );
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class.as_ptr(),
                class.as_ptr(),
                WS_POPUP | WS_VISIBLE,
                x,
                y,
                width,
                height,
                null_mut(),
                null_mut(),
                instance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                UnregisterClassW(class.as_ptr(), instance);
                DeleteObject(brush);
                anyhow::bail!("creating cover window failed");
            }
            if SetTimer(hwnd, 1, 2500, None) == 0 {
                DestroyWindow(hwnd);
                UnregisterClassW(class.as_ptr(), instance);
                DeleteObject(brush);
                anyhow::bail!("creating cover timer failed");
            }
            println!("cover-ready");
            let mut message = MSG::default();
            while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            UnregisterClassW(class.as_ptr(), instance);
            DeleteObject(brush);
        }
        Ok(())
    }
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    windows::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("this acceptance helper requires Windows");
}
