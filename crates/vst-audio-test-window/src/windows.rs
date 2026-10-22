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
        // SAFETY: registered class, bounded test dimensions, no borrowed creation data.
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
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
