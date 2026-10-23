//! Windows desktop discovery and physical input for independent host comparisons.
use std::time::{Duration, Instant};
use windows::{
    Win32::{
        Foundation::*,
        System::{Com::*, Variant::VARIANT},
        UI::{Accessibility::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
    core::BOOL,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}
impl Rect {
    pub fn center(&self) -> (i32, i32) {
        (self.left + self.width() / 2, self.top + self.height() / 2)
    }
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}
impl From<RECT> for Rect {
    fn from(r: RECT) -> Self {
        Self {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Window {
    pub handle: u64,
    pub class: String,
    pub title: String,
    pub rect: Rect,
    pub pid: u32,
}
fn hwnd(handle: u64) -> HWND {
    HWND(handle as usize as *mut _)
}
fn describe(window: HWND) -> Window {
    let mut pid = 0;
    // SAFETY: queried HWND and writable process-id output; stale handles return zero.
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    let mut class = [0u16; 256];
    // SAFETY: writable buffer passed with its exact length.
    let c = unsafe { GetClassNameW(window, &mut class) };
    let mut title = [0u16; 1024];
    // SAFETY: writable buffer passed with its exact length.
    let t = unsafe { GetWindowTextW(window, &mut title) };
    let mut rect = RECT::default();
    // SAFETY: writable rectangle; a disappeared window leaves an empty rectangle.
    let _ = unsafe { GetWindowRect(window, &mut rect) };
    Window {
        handle: window.0 as usize as u64,
        class: String::from_utf16_lossy(&class[..c.max(0) as usize]),
        title: String::from_utf16_lossy(&title[..t.max(0) as usize]),
        rect: rect.into(),
        pid,
    }
}
unsafe extern "system" fn collect(window: HWND, data: LPARAM) -> BOOL {
    // SAFETY: synchronous enumeration exclusively borrows this vector until return.
    let windows = unsafe { &mut *(data.0 as *mut Vec<Window>) };
    windows.push(describe(window));
    true.into()
}
/// Visible top-level windows owned by this process.
pub fn top_windows(pid: u32) -> Vec<Window> {
    let mut result = Vec::<Window>::new();
    // SAFETY: synchronous callback receives a live, exclusively borrowed vector.
    let _ = unsafe {
        EnumWindows(
            Some(collect),
            LPARAM((&mut result as *mut Vec<Window>) as isize),
        )
    };
    result.retain(|w| {
        // SAFETY: visibility query accepts stale handles without dereferencing caller memory.
        w.pid == pid && unsafe { IsWindowVisible(hwnd(w.handle)) }.as_bool()
    });
    result
}
/// First descendant whose native class matches exactly.
pub fn find_child_class(top: u64, class: &str) -> Option<Window> {
    let mut result = Vec::<Window>::new();
    // SAFETY: synchronous callback receives a live, exclusively borrowed vector.
    let _ = unsafe {
        EnumChildWindows(
            Some(hwnd(top)),
            Some(collect),
            LPARAM((&mut result as *mut Vec<Window>) as isize),
        )
    };
    result.into_iter().find(|w| w.class == class)
}
struct Apartment(bool);
impl Apartment {
    fn new() -> Self {
        // SAFETY: reserved argument is null; initialization belongs to this thread.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        assert!(
            hr.is_ok() || hr == RPC_E_CHANGED_MODE,
            "COM initialization: {hr:?}"
        );
        Self(hr.is_ok())
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances this thread's successful initialization, after local UIA values drop.
            unsafe { CoUninitialize() };
        }
    }
}
thread_local! { static COM: Apartment = Apartment::new(); }
/// Bounds of the first named UIA descendant, optionally restricted by class.
pub fn find_named(top: u64, name: &str, class: Option<&str>) -> Option<Rect> {
    COM.with(|_| {});
    // SAFETY: COM is initialized on this thread; result is retained only within this call.
    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }.ok()?;
    // SAFETY: UIA validates the HWND; no raw caller pointers are retained.
    let root = unsafe { automation.ElementFromHandle(hwnd(top)) }.ok()?;
    let value = VARIANT::from(name);
    // SAFETY: live automation object and initialized string variant.
    let mut condition =
        unsafe { automation.CreatePropertyCondition(UIA_NamePropertyId, &value) }.ok()?;
    if let Some(class) = class {
        let value = VARIANT::from(class);
        // SAFETY: live automation object and initialized string variant.
        let other =
            unsafe { automation.CreatePropertyCondition(UIA_ClassNamePropertyId, &value) }.ok()?;
        // SAFETY: both conditions are owned COM references valid for this call.
        condition = unsafe { automation.CreateAndCondition(&condition, &other) }.ok()?;
    }
    // SAFETY: live root and condition, descendant-only search.
    let element = unsafe { root.FindFirst(TreeScope_Descendants, &condition) }.ok()?;
    // SAFETY: owned element reference, read-only property query.
    if unsafe { element.CurrentIsOffscreen() }.ok()?.as_bool() {
        return None;
    }
    // SAFETY: owned element reference, read-only property query.
    let rect: Rect = unsafe { element.CurrentBoundingRectangle() }.ok()?.into();
    let screen = desktop_rect();
    (rect.width() > 0
        && rect.height() > 0
        && screen.contains(rect.left, rect.top)
        && screen.contains(rect.right - 1, rect.bottom - 1))
    .then_some(rect)
}
/// RGBA pixels of a screen rectangle as composed on the desktop.
pub fn capture(rect: Rect) -> Vec<u8> {
    let (width, height) = (
        u32::try_from(rect.width()).unwrap(),
        u32::try_from(rect.height()).unwrap(),
    );
    screen_pixels(rect.left, rect.top, width, height).expect("screen capture")
}
pub(crate) fn screen_pixels(
    left: i32,
    top: i32,
    width: u32,
    height: u32,
) -> windows::core::Result<Vec<u8>> {
    use windows::Win32::Graphics::Gdi::*;
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err(windows::core::Error::new(E_FAIL, "capture size"));
    }
    // SAFETY: screen DC, released below on every path.
    let dc = unsafe { GetDC(None) };
    // SAFETY: compatible memory DC inherits the live screen DC's format.
    let memory = unsafe { CreateCompatibleDC(Some(dc)) };
    // SAFETY: bounded bitmap sized to the validated rectangle.
    let bitmap = unsafe { CreateCompatibleBitmap(dc, width as i32, height as i32) };
    // SAFETY: compatible bitmap selected into the memory DC for the copy.
    let previous = unsafe { SelectObject(memory, bitmap.into()) };
    // SAFETY: both DCs are live and the copy is bounded to the bitmap.
    let copied = unsafe {
        BitBlt(
            memory,
            0,
            0,
            width as i32,
            height as i32,
            Some(dc),
            left,
            top,
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
    // SAFETY: release the screen DC borrowed above.
    let _ = unsafe { ReleaseDC(None, dc) };
    copied?;
    if rows != height as i32 {
        return Err(windows::core::Error::new(E_FAIL, "capture rows"));
    }
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    Ok(rgba)
}
fn desktop_rect() -> Rect {
    // SAFETY: virtual desktop origin query has no pointer arguments.
    let left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    // SAFETY: virtual desktop origin query has no pointer arguments.
    let top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    // SAFETY: virtual desktop size query has no pointer arguments.
    let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    // SAFETY: virtual desktop size query has no pointer arguments.
    let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
    Rect {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}
pub(crate) fn send(dx: i32, dy: i32, flags: MOUSE_EVENT_FLAGS) {
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
}
pub(crate) fn move_pointer(point: POINT, button: Option<bool>, settle: impl Fn()) {
    let screen = desktop_rect();
    assert!(screen.width() > 1 && screen.height() > 1);
    let dx = (i64::from(point.x - screen.left) * 65535 / i64::from(screen.width() - 1)) as i32;
    let dy = (i64::from(point.y - screen.top) * 65535 / i64::from(screen.height() - 1)) as i32;
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
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        settle();
        let mut actual = POINT::default();
        // SAFETY: writable point receives current desktop cursor coordinates.
        unsafe { GetCursorPos(&mut actual) }.unwrap();
        if (actual.x - point.x).abs() <= 1 && (actual.y - point.y).abs() <= 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "OS did not deliver mouse movement: {actual:?}"
        );
    }
}
fn settle() {
    std::thread::sleep(Duration::from_millis(40));
}
fn assert_hit(pid: u32, x: i32, y: i32) {
    // SAFETY: screen point is a value, with no caller pointers.
    let hit = unsafe { WindowFromPoint(POINT { x, y }) };
    // SAFETY: query accepts the HWND returned by WindowFromPoint, including null.
    let root = unsafe { GetAncestor(hit, GA_ROOT) };
    let window = describe(root);
    assert_eq!(window.pid, pid, "input blocked at ({x},{y}): {window:?}");
}
/// Raises the smallest visible process window containing this screen point.
pub fn bring_to_front(pid: u32, x: i32, y: i32) {
    assert_ne!(pid, 0);
    let window = top_windows(pid)
        .into_iter()
        .filter(|w| w.rect.contains(x, y))
        .min_by_key(|w| i64::from(w.rect.width()) * i64::from(w.rect.height()))
        .unwrap_or_else(|| panic!("no window for pid {pid} at ({x},{y})"));
    send(0, 0, MOUSEEVENTF_MOVE);
    // SAFETY: enumerated top-level HWND; a concurrent close merely rejects activation.
    let _ = unsafe { SetForegroundWindow(hwnd(window.handle)) };
    settle();
    assert_hit(pid, x, y);
}
fn pointer(pid: u32, x: i32, y: i32, button: Option<bool>) {
    assert_hit(pid, x, y);
    move_pointer(POINT { x, y }, button, settle);
}
struct Release;
impl Drop for Release {
    fn drop(&mut self) {
        send(0, 0, MOUSEEVENTF_LEFTUP);
    }
}
/// Presses and releases the physical left mouse button at screen coordinates.
pub fn click(pid: u32, x: i32, y: i32) {
    bring_to_front(pid, x, y);
    let release = Release;
    pointer(pid, x, y, Some(true));
    pointer(pid, x, y, Some(false));
    std::mem::forget(release);
}
/// Drags with physical mouse input in screen coordinates.
pub fn drag(pid: u32, from: (i32, i32), to: (i32, i32), steps: u32) {
    assert!(steps > 0);
    bring_to_front(pid, from.0, from.1);
    let release = Release;
    pointer(pid, from.0, from.1, Some(true));
    for step in 1..=steps {
        let coordinate = |a: i32, b: i32| {
            (i64::from(a) + i64::from(b - a) * i64::from(step) / i64::from(steps)) as i32
        };
        pointer(
            pid,
            coordinate(from.0, to.0),
            coordinate(from.1, to.1),
            None,
        );
    }
    pointer(pid, to.0, to.1, Some(false));
    std::mem::forget(release);
}
