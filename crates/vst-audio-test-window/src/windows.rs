use std::cell::Cell;
use vst_audio_protocol::{EditorImage, ParentWindow};
use windows::{
    Win32::{Foundation::*, UI::WindowsAndMessaging::*},
    core::w,
};

unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: unchanged parameters from Windows' callback.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}
unsafe extern "system" fn child(window: HWND, data: LPARAM) -> windows::core::BOOL {
    let mut pid = 0;
    // SAFETY: enumeration supplies a live HWND and writable pid output.
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    // SAFETY: synchronous enumeration borrows this vector exclusively for the call.
    unsafe { &mut *(data.0 as *mut Vec<(u64, Option<u32>)>) }
        .push((window.0 as usize as u64, Some(pid)));
    true.into()
}
#[derive(Debug)]
pub struct Parent {
    window: HWND,
    destroyed: Cell<bool>,
    input_pid: Cell<u32>,
}
impl Parent {
    pub fn create(width: u32, height: u32) -> Self {
        // SAFETY: null name requests current executable module.
        let module =
            unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None) }.unwrap();
        let instance = HINSTANCE(module.0);
        let class = WNDCLASSW {
            hInstance: instance,
            lpfnWndProc: Some(procedure),
            lpszClassName: w!("VstAudioTestParent"),
            ..Default::default()
        };
        // SAFETY: static class name and callback belong to this executable.
        let _ = unsafe { RegisterClassW(&class) };
        // Runner desktops keep console windows open over the origin. A topmost
        // fixture stays the window that screen capture and input actually reach.
        // SAFETY: registered class, bounded test dimensions, no borrowed creation data.
        let window = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST,
                w!("VstAudioTestParent"),
                w!("Embedding fixture"),
                WS_POPUP | WS_VISIBLE | WS_CLIPCHILDREN,
                0,
                0,
                width.try_into().unwrap(),
                height.try_into().unwrap(),
                None,
                None,
                Some(instance),
                None,
            )
        }
        .unwrap();
        Self {
            window,
            destroyed: Cell::new(false),
            input_pid: Cell::new(0),
        }
    }
    pub fn handle(&self) -> ParentWindow {
        ParentWindow::Win32 {
            hwnd: self.window.0 as usize as u64,
        }
    }
    pub fn pump(&self) {
        let mut message = MSG::default();
        // SAFETY: writable message, this thread's queue, removing consumed messages.
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            // SAFETY: valid message from PeekMessageW.
            let _ = unsafe { TranslateMessage(&message) };
            // SAFETY: valid message and static window callbacks.
            unsafe { DispatchMessageW(&message) };
        }
    }
    pub fn expect_input_process(&self, pid: u32) {
        assert_ne!(pid, 0);
        self.input_pid.set(pid);
    }
    pub(crate) fn pointer(&self, x: i32, y: i32, button: Option<bool>) {
        use windows::Win32::{Graphics::Gdi::ClientToScreen, UI::Input::KeyboardAndMouse::*};
        let send = |dx, dy, flags| {
            let input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx,
                        dy,
                        dwFlags: flags,
                        ..Default::default()
                    },
                },
            };
            // SAFETY: one fully initialized mouse INPUT, with its exact ABI size.
            let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
            assert_eq!(
                sent,
                1,
                "SendInput blocked (desktop/UIPI): {}",
                windows::core::Error::from_thread()
            );
        };
        // Harmless relative zero motion permits a foreground request without bypassing OS policy.
        send(0, 0, MOUSEEVENTF_MOVE);
        // SAFETY: live parent top-level window on this thread.
        let _ = unsafe { SetForegroundWindow(self.window) };
        self.settle();
        // SAFETY: queries current desktop foreground window, retaining no borrowed handle.
        let foreground = unsafe { GetForegroundWindow() };
        assert_eq!(
            foreground, self.window,
            "foreground blocked: another desktop/window owns input"
        );
        let mut point = POINT { x, y };
        // SAFETY: live parent and writable client point.
        assert!(unsafe { ClientToScreen(self.window, &mut point) }.as_bool());
        // SAFETY: screen point is a value; query does not dereference caller data.
        let hit = unsafe { WindowFromPoint(point) };
        let mut pid = 0;
        // SAFETY: returned HWND and writable process-id output.
        unsafe { GetWindowThreadProcessId(hit, Some(&mut pid)) };
        assert_ne!(
            self.input_pid.get(),
            0,
            "expected worker pid must be configured"
        );
        assert_eq!(
            pid,
            self.input_pid.get(),
            "input blocked at {point:?}: hit {hit:?}, process {pid}"
        );
        // SAFETY: these constants query dimensions/origin of the active virtual desktop.
        let left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        // SAFETY: virtual desktop origin query has no pointer arguments.
        let top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        // SAFETY: virtual desktop size query has no pointer arguments.
        let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
        // SAFETY: virtual desktop size query has no pointer arguments.
        let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
        assert!(width > 1 && height > 1);
        let dx = (i64::from(point.x - left) * 65535 / i64::from(width - 1)) as i32;
        let dy = (i64::from(point.y - top) * 65535 / i64::from(height - 1)) as i32;
        send(
            dx,
            dy,
            MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MOVE | MOUSEEVENTF_VIRTUALDESK,
        );
        if let Some(press) = button {
            send(
                0,
                0,
                if press {
                    MOUSEEVENTF_LEFTDOWN
                } else {
                    MOUSEEVENTF_LEFTUP
                },
            );
        }
        // SendInput queues into the system input stream; observe the delivered cursor position.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            self.settle();
            let mut actual = POINT::default();
            // SAFETY: writable point receives current desktop cursor coordinates.
            unsafe { GetCursorPos(&mut actual) }.unwrap();
            if (actual.x - point.x).abs() <= 1 && (actual.y - point.y).abs() <= 1 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "OS did not deliver mouse movement: {actual:?}"
            );
        }
    }
    pub fn children(&self) -> Vec<(u64, Option<u32>)> {
        let mut result: Vec<(u64, Option<u32>)> = Vec::new();
        // SAFETY: synchronous callback receives exclusive vector pointer valid until return.
        let _ = unsafe {
            EnumChildWindows(
                Some(self.window),
                Some(child),
                LPARAM((&mut result as *mut Vec<(u64, Option<u32>)>) as isize),
            )
        };
        result
    }
    /// Describes the top-level window that owns a parent client point on screen.
    pub fn window_at(&self, x: i32, y: i32) -> String {
        use windows::Win32::Graphics::Gdi::ClientToScreen;
        let mut point = POINT { x, y };
        // SAFETY: live parent and writable client point.
        let _ = unsafe { ClientToScreen(self.window, &mut point) };
        // SAFETY: screen point is a value; query does not dereference caller data.
        let hit = unsafe { WindowFromPoint(point) };
        // SAFETY: any HWND, including null, is accepted and only queried.
        let root = unsafe { GetAncestor(hit, GA_ROOT) };
        let mut pid = 0;
        // SAFETY: queried HWND and writable process-id output.
        unsafe { GetWindowThreadProcessId(root, Some(&mut pid)) };
        let mut class = [0u16; 128];
        // SAFETY: writable buffer passed with its exact length.
        let class_length = unsafe { GetClassNameW(root, &mut class) };
        let mut title = [0u16; 128];
        // SAFETY: writable buffer passed with its exact length.
        let title_length = unsafe { GetWindowTextW(root, &mut title) };
        let text = |buffer: &[u16], length: i32| {
            String::from_utf16_lossy(&buffer[..length.clamp(0, 128) as usize])
        };
        format!(
            "{root:?} (parent {}) pid {pid} class {:?} title {:?}",
            root == self.window,
            text(&class, class_length),
            text(&title, title_length)
        )
    }
    fn status(&self) -> windows::core::Result<(bool, u32, u32)> {
        let mut rect = RECT::default();
        // SAFETY: live test window and writable client rectangle.
        unsafe { GetClientRect(self.window, &mut rect) }?;
        Ok((
            true,
            (rect.right - rect.left) as u32,
            (rect.bottom - rect.top) as u32,
        ))
    }
    pub fn capture(&self) -> EditorImage {
        self.capture_result().unwrap()
    }
    fn capture_result(&self) -> windows::core::Result<vst_audio_protocol::EditorImage> {
        use windows::Win32::Graphics::Gdi::*;
        let (visible, width, height) = self.status()?;
        if !visible || width == 0 || height == 0 || width > 2048 || height > 2048 {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                "editor capture unavailable",
            ));
        }
        // SAFETY: client DC of this owned visible window, released below on every path.
        let dc = unsafe { GetDC(None) };
        let mut origin = POINT::default();
        // SAFETY: valid client window and writable point converted to screen coordinates.
        assert!(unsafe { ClientToScreen(self.window, &mut origin) }.as_bool());
        // SAFETY: compatible memory DC inherits the live client DC's format.
        let memory = unsafe { CreateCompatibleDC(Some(dc)) };
        // SAFETY: bounded bitmap sized to validated client area.
        let bitmap = unsafe { CreateCompatibleBitmap(dc, width as i32, height as i32) };
        // SAFETY: compatible bitmap selected into the memory DC for the copy.
        let previous = unsafe { SelectObject(memory, bitmap.into()) };
        // SAFETY: both DCs are live and the copy is bounded to their client bitmap.
        let copied = unsafe {
            BitBlt(
                memory,
                0,
                0,
                width as i32,
                height as i32,
                Some(dc),
                origin.x,
                origin.y,
                SRCCOPY,
            )
        };
        // SAFETY: restore original object before reading or deleting the bitmap.
        let _ = unsafe { SelectObject(memory, previous) };
        let mut rgba = vec![0u8; width as usize * height as usize * 4];
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: output allocation holds exactly height rows of width 32-bit pixels.
        let rows = unsafe {
            GetDIBits(
                dc,
                bitmap,
                0,
                height,
                Some(rgba.as_mut_ptr().cast()),
                &mut info,
                DIB_RGB_COLORS,
            )
        };
        // SAFETY: bitmap deselected, solely owned by this function.
        let _ = unsafe { DeleteObject(bitmap.into()) };
        // SAFETY: memory DC has no owned objects selected.
        let _ = unsafe { DeleteDC(memory) };
        // SAFETY: release the DC borrowed from this window.
        let _ = unsafe { ReleaseDC(None, dc) };
        copied?;
        if rows != height as i32 {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                "editor capture unavailable",
            ));
        }
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        Ok(vst_audio_protocol::EditorImage {
            width,
            height,
            rgba,
        })
    }
    pub fn set_visible(&self, visible: bool) {
        // SAFETY: owned live window on the creating thread.
        let _ = unsafe { ShowWindow(self.window, if visible { SW_SHOW } else { SW_HIDE }) };
    }
    pub fn destroy(&self) {
        if !self.destroyed.replace(true) {
            // SAFETY: owned window, destroyed once on the creating thread.
            unsafe { DestroyWindow(self.window) }.unwrap();
        }
    }
}
impl Drop for Parent {
    fn drop(&mut self) {
        self.destroy();
    }
}
