#![allow(unsafe_code)]
use std::{cell::Cell, io};
use vst_audio_protocol::ParentWindow;
use windows::{
    Win32::{Foundation::*, UI::WindowsAndMessaging::*},
    core::{PCWSTR, w},
};

// These user32 signatures and pseudo-handles match windows 0.62.2 UI::HiDpi.
// Explicit FFI avoids enabling a new workspace Cargo feature. Windows 10 1809+
// supplies mixed hosting and UNAWARE_GDISCALED.
#[link(name = "user32")]
unsafe extern "system" {
    fn SetThreadDpiAwarenessContext(context: isize) -> isize;
    fn GetWindowDpiAwarenessContext(window: HWND) -> isize;
    fn AreDpiAwarenessContextsEqual(a: isize, b: isize) -> windows::core::BOOL;
    fn SetThreadDpiHostingBehavior(behavior: i32) -> i32;
    fn GetDpiForWindow(window: HWND) -> u32;
    fn AdjustWindowRectExForDpi(
        rect: *mut RECT,
        style: u32,
        menu: windows::core::BOOL,
        extended: u32,
        dpi: u32,
    ) -> windows::core::BOOL;
}
const PER_MONITOR_V2: isize = -4;
const UNAWARE_GDISCALED: isize = -5;
#[derive(Debug)]
pub(crate) struct DpiScope(isize);
impl DpiScope {
    fn enter(context: isize) -> Self {
        // SAFETY: documented DPI pseudo-handle or a context obtained from a live window.
        Self(unsafe { SetThreadDpiAwarenessContext(context) })
    }
}
impl Drop for DpiScope {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: restore the context returned by the matching call on this thread.
            unsafe { SetThreadDpiAwarenessContext(self.0) };
        }
    }
}
struct HostingScope(i32);
impl Drop for HostingScope {
    fn drop(&mut self) {
        if self.0 >= 0 {
            // SAFETY: restore this thread's previous hosting behavior.
            unsafe { SetThreadDpiHostingBehavior(self.0) };
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct DpiChange {
    pub scale: f32,
    rectangle: RECT,
}
#[derive(Debug, Default)]
struct WindowState {
    destroyed: Cell<bool>,
    dpi: Cell<Option<DpiChange>>,
}

unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_DESTROY || message == WM_DPICHANGED {
        // SAFETY: stable main-thread allocation retained through DestroyWindow.
        let state = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const WindowState;
        if !state.is_null() {
            // SAFETY: installed after creation and kept alive through destruction on this thread.
            let state = unsafe { &*state };
            if message == WM_DESTROY {
                state.destroyed.set(true);
            } else if lparam.0 != 0 {
                // SAFETY: WM_DPICHANGED supplies a RECT valid only during this callback.
                // Copy it; never retain LPARAM or invoke foreign code from the window procedure.
                let rectangle = unsafe { (lparam.0 as *const RECT).read() };
                state.dpi.set(Some(DpiChange {
                    scale: ((wparam.0 & 0xffff) as f32 / 96.0).clamp(0.5, 4.0),
                    rectangle,
                }));
                return LRESULT(0);
            }
        }
    }
    if message == WM_CLOSE {
        // SAFETY: live callback window; hiding defers destruction until effEditClose.
        // SAFETY: the property is an integer marker and does not own borrowed memory.
        let _ = unsafe {
            SetPropW(
                window,
                w!("VstAudioClosed"),
                Some(HANDLE(std::ptr::dangling_mut())),
            )
        };
        // SAFETY: callback window remains live until deferred destruction.
        let _ = unsafe { ShowWindow(window, SW_HIDE) };
        return LRESULT(0);
    }
    // SAFETY: parameters are forwarded unchanged from Windows' window procedure.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

unsafe extern "system" fn close_matching(window: HWND, pid: LPARAM) -> windows::core::BOOL {
    let mut owner = 0;
    // SAFETY: enumeration supplies a live window and owner is a writable pid output.
    unsafe { GetWindowThreadProcessId(window, Some(&mut owner)) };
    if owner == pid.0 as u32 {
        // SAFETY: posts a pointer-free close message to a window in the specified test process.
        let _ = unsafe { PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
    true.into()
}

#[doc(hidden)]
pub fn close_process_editor(pid: u32) -> windows::core::Result<()> {
    // SAFETY: callback is static; lparam carries only an integer process id.
    unsafe { EnumWindows(Some(close_matching), LPARAM(pid as isize)) }
}

#[derive(Debug)]
pub struct EditorWindow {
    window: HWND,
    parent: Option<ParentWindow>,
    state: Box<WindowState>,
}
impl EditorWindow {
    pub fn open(title: &str, width: i32, height: i32) -> windows::core::Result<Self> {
        Self::open_scaled(title, width, height, false)
    }
    pub(crate) fn open_scaled(
        title: &str,
        width: i32,
        height: i32,
        stretch: bool,
    ) -> windows::core::Result<Self> {
        let _dpi = DpiScope::enter(if stretch {
            UNAWARE_GDISCALED
        } else {
            PER_MONITOR_V2
        });
        // SAFETY: null module name requests the current executable's live module.
        let module = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None) }?;
        let instance = HINSTANCE(module.0);
        let class = WNDCLASSW {
            hInstance: instance,
            lpfnWndProc: Some(procedure),
            lpszClassName: w!("PluginHostEditor"),
            ..Default::default()
        };
        // SAFETY: class callback is static and class-name storage is permanent.
        let _ = unsafe { RegisterClassW(&class) };
        let name: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut rectangle = RECT {
            left: 0,
            top: 0,
            right: width.clamp(1, 8192),
            bottom: height.clamp(1, 8192),
        };
        // SAFETY: writable rectangle and valid top-level window style.
        unsafe { AdjustWindowRectEx(&mut rectangle, style, false, WINDOW_EX_STYLE(0)) }?;
        // SAFETY: registered class, terminated title, valid bounded dimensions, no borrowed creation data.
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("PluginHostEditor"),
                PCWSTR(name.as_ptr()),
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                rectangle.right - rectangle.left,
                rectangle.bottom - rectangle.top,
                None,
                None,
                Some(instance),
                None,
            )
        }?;
        // SAFETY: freshly created window belongs to the current thread.
        let _ = unsafe { ShowWindow(window, SW_SHOW) };
        Ok(Self::owned(window, None))
    }
    fn owned(window: HWND, parent: Option<ParentWindow>) -> Self {
        let state = Box::new(WindowState::default());
        // SAFETY: stable allocation retained until after native destruction, callback uses it only on this thread.
        unsafe {
            SetWindowLongPtrW(
                window,
                GWLP_USERDATA,
                // LONG_PTR: i32 on 32-bit Windows, isize on 64-bit.
                (&*state as *const WindowState) as _,
            )
        };
        Self {
            window,
            parent,
            state,
        }
    }
    pub fn open_embedded(parent: ParentWindow, width: i32, height: i32) -> io::Result<Self> {
        let ParentWindow::Win32 { hwnd } = parent else {
            return Err(io::Error::other(
                "parent window platform does not match the worker",
            ));
        };
        let value =
            usize::try_from(hwnd).map_err(|_| io::Error::other("parent window is not a window"))?;
        let parent_hwnd = HWND(value as *mut std::ffi::c_void);
        // SAFETY: IsWindow validates opaque HWND values without dereferencing them in Rust.
        if !unsafe { IsWindow(Some(parent_hwnd)) }.as_bool() {
            return Err(io::Error::other("parent window is not a window"));
        }
        // SAFETY: null module name requests this executable's live module.
        let module = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None) }
            .map_err(io::Error::other)?;
        let instance = HINSTANCE(module.0);
        let class = WNDCLASSW {
            hInstance: instance,
            lpfnWndProc: Some(procedure),
            lpszClassName: w!("PluginHostEditor"),
            ..Default::default()
        };
        // SAFETY: static callback and class name; registration is idempotent for this module.
        let _ = unsafe { RegisterClassW(&class) };
        let _dpi = DpiScope::enter(PER_MONITOR_V2);
        // SAFETY: validated live parent HWND; query returns its opaque awareness context.
        let parent_context = unsafe { GetWindowDpiAwarenessContext(parent_hwnd) };
        // SAFETY: both values are documented/queried awareness contexts.
        let mixed =
            !unsafe { AreDpiAwarenessContextsEqual(parent_context, PER_MONITOR_V2) }.as_bool();
        // This scope affects only child creation. It cannot retrofit the foreign
        // parent's hosting policy; the application must create a compatible parent.
        let _hosting = if mixed {
            // SAFETY: DPI_HOSTING_BEHAVIOR_MIXED is 1; restored on all exit paths.
            Some(HostingScope(unsafe { SetThreadDpiHostingBehavior(1) }))
        } else {
            None
        };
        // SAFETY: validated parent HWND, registered class, bounded dimensions, no borrowed creation data.
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("PluginHostEditor"),
                None,
                WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS | WS_VISIBLE,
                0,
                0,
                width.clamp(1, 8192),
                height.clamp(1, 8192),
                Some(parent_hwnd),
                None,
                Some(instance),
                None,
            )
        }
        .map_err(|e| io::Error::other(format!("embedded window creation failed: {e}")))?;
        Ok(Self::owned(window, Some(parent)))
    }
    pub(crate) fn dpi_scope(&self) -> DpiScope {
        // SAFETY: owned window is live while used by the main-thread owner.
        DpiScope::enter(unsafe { GetWindowDpiAwarenessContext(self.window) })
    }
    pub fn scale(&self) -> f32 {
        let window = match self.parent {
            Some(ParentWindow::Win32 { hwnd }) => HWND(hwnd as usize as *mut std::ffi::c_void),
            _ => self.window,
        };
        // SAFETY: owned window or validated parent; zero indicates an unavailable DPI.
        let dpi = unsafe { GetDpiForWindow(window) };
        if dpi == 0 {
            1.0
        } else {
            (dpi as f32 / 96.0).clamp(0.5, 4.0)
        }
    }
    pub(crate) fn take_dpi_change(&self) -> Option<DpiChange> {
        self.state.dpi.take()
    }
    pub(crate) fn apply_dpi_rectangle(&self, change: DpiChange) -> windows::core::Result<()> {
        let _dpi = self.dpi_scope();
        let rect = change.rectangle;
        let width = rect.right.saturating_sub(rect.left).clamp(1, 16384);
        let height = rect.bottom.saturating_sub(rect.top).clamp(1, 16384);
        // SAFETY: owned window and copied bounded rectangle; no borrowed callback memory.
        unsafe {
            SetWindowPos(
                self.window,
                None,
                rect.left,
                rect.top,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        }
    }
    pub fn parent(&self) -> Option<ParentWindow> {
        self.parent
    }
    pub fn handle_value(&self) -> usize {
        self.window.0 as usize
    }
    pub fn pump(&self) -> bool {
        let mut message = MSG::default();
        // SAFETY: writable message, only this thread's messages, removing each consumed message.
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            // SAFETY: message was obtained from PeekMessageW.
            let _ = unsafe { TranslateMessage(&message) };
            // SAFETY: message was obtained from PeekMessageW and handlers remain live.
            unsafe { DispatchMessageW(&message) };
        }
        if self.state.destroyed.get() {
            return true;
        }
        // SAFETY: owned window has not received WM_DESTROY.
        unsafe { GetPropW(self.window, w!("VstAudioClosed")) }.0 as usize == 1
    }
    pub fn show(&self, visible: bool) -> windows::core::Result<()> {
        // SAFETY: live window owned by this main thread.
        let _ = unsafe { ShowWindow(self.window, if visible { SW_SHOW } else { SW_HIDE }) };
        Ok(())
    }
    pub fn focus(&self) -> windows::core::Result<()> {
        if self.parent.is_some() {
            // SAFETY: own child window on the UI thread; OS decides whether focus is granted.
            let _ =
                unsafe { windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(self.window)) };
            return Ok(());
        }
        // SAFETY: requesting foreground ownership of this live window; OS may deny it.
        let _ = unsafe { SetForegroundWindow(self.window) };
        Ok(())
    }
    pub fn status(&self) -> windows::core::Result<(bool, u32, u32)> {
        let _dpi = self.dpi_scope();
        let mut rect = RECT::default();
        // SAFETY: live window and writable output rectangle.
        unsafe { GetClientRect(self.window, &mut rect) }?;
        // SAFETY: owned live window.
        let visible = unsafe { IsWindowVisible(self.window) }.as_bool();
        Ok((
            visible,
            (rect.right - rect.left) as u32,
            (rect.bottom - rect.top) as u32,
        ))
    }
    pub fn capture(&self) -> windows::core::Result<vst_audio_protocol::EditorImage> {
        let _dpi = self.dpi_scope();
        use windows::Win32::Graphics::Gdi::*;
        let (visible, width, height) = self.status()?;
        if !visible || width == 0 || height == 0 || width > 2048 || height > 2048 {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                "editor capture unavailable",
            ));
        }
        // SAFETY: client DC of this owned visible window, released below on every path.
        let dc = unsafe { GetDC(Some(self.window)) };
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
                0,
                0,
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
        let _ = unsafe { ReleaseDC(Some(self.window), dc) };
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
    pub fn request_close(&self) -> windows::core::Result<()> {
        // SAFETY: asynchronous close carries no pointers to borrowed memory.
        unsafe { PostMessageW(Some(self.window), WM_CLOSE, WPARAM(0), LPARAM(0)) }
    }
    pub fn resize(&self, width: i32, height: i32) -> windows::core::Result<()> {
        let _dpi = self.dpi_scope();
        let mut rectangle = RECT {
            left: 0,
            top: 0,
            right: width.clamp(1, 8192),
            bottom: height.clamp(1, 8192),
        };
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        if self.parent.is_none() {
            // SAFETY: owned live HWND; unaware windows return 96, PMv2 returns actual DPI.
            let dpi = unsafe { GetDpiForWindow(self.window) }.max(96);
            // SAFETY: writable client rectangle, matching style and window-specific DPI.
            unsafe { AdjustWindowRectExForDpi(&mut rectangle, style.0, false.into(), 0, dpi) }
                .ok()?;
        }
        // SAFETY: owned window, bounded dimensions, no z-order or position change.
        unsafe {
            SetWindowPos(
                self.window,
                None,
                0,
                0,
                rectangle.right - rectangle.left,
                rectangle.bottom - rectangle.top,
                SWP_NOMOVE | SWP_NOZORDER,
            )
        }
    }
}
impl Drop for EditorWindow {
    fn drop(&mut self) {
        if !self.state.destroyed.get() {
            // SAFETY: owned window on creating thread, stable callback state remains alive until this returns.
            let _ = unsafe { DestroyWindow(self.window) };
        }
    }
}
