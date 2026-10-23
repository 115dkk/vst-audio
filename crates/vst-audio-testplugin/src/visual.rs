#![allow(unsafe_code)]
//! A real native child in the public fixture; captures must include its pixels.
#[cfg(target_os = "macos")]
pub(crate) struct Canvas(objc2::rc::Retained<objc2_app_kit::NSBox>);
#[cfg(target_os = "macos")]
impl Canvas {
    pub unsafe fn attach(parent: *mut std::ffi::c_void, width: i32, height: i32) -> Option<Self> {
        use objc2::{MainThreadMarker, MainThreadOnly};
        use objc2_app_kit::{NSBox, NSBoxType, NSColor, NSTitlePosition, NSView};
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        let mtm = MainThreadMarker::new()?;
        // SAFETY: attached/effEditOpen supplies a live NSView on its main thread.
        let parent = unsafe { (parent as *const NSView).as_ref() }?;
        let view = NSBox::initWithFrame(
            NSBox::alloc(mtm),
            NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(f64::from(width), f64::from(height)),
            ),
        );
        view.setBoxType(NSBoxType::Custom);
        view.setTitlePosition(NSTitlePosition::NoTitle);
        view.setBorderWidth(0.0);
        view.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.125, 0.25, 0.5, 1.0,
        ));
        parent.addSubview(&view);
        view.setNeedsDisplay(true);
        Some(Self(view))
    }
    pub fn resize(&self, width: i32, height: i32) {
        use objc2::MainThreadOnly;
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        self.0.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(f64::from(width), f64::from(height)),
        ));
    }
}
#[cfg(target_os = "macos")]
impl Drop for Canvas {
    fn drop(&mut self) {
        self.0.removeFromSuperview();
    }
}

#[cfg(windows)]
pub(crate) struct Canvas(windows::Win32::Foundation::HWND);
#[cfg(windows)]
impl Canvas {
    pub unsafe fn attach(parent: *mut std::ffi::c_void, width: i32, height: i32) -> Option<Self> {
        use windows::{
            Win32::{Foundation::HWND, UI::WindowsAndMessaging::*},
            core::w,
        };
        // SAFETY: plugin attach supplies a live parent HWND; title and class are static.
        let child = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("VST audio fixture\n\nStereo to surround\nGain controlled through IPC"),
                WS_CHILD | WS_VISIBLE,
                0,
                0,
                width,
                height,
                Some(HWND(parent)),
                None,
                None,
                None,
            )
        }
        .ok()?;
        Some(Self(child))
    }
    pub fn resize(&self, width: i32, height: i32) {
        // SAFETY: owned child remains live until Drop and is resized on the UI thread.
        let _ = unsafe {
            windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
                self.0,
                None,
                0,
                0,
                width,
                height,
                windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE
                    | windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER,
            )
        };
    }
}
#[cfg(windows)]
impl Drop for Canvas {
    fn drop(&mut self) {
        // SAFETY: child owned by the fixture, removed before its host parent closes.
        let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.0) };
    }
}

#[cfg(target_os = "linux")]
pub(crate) struct Canvas(x11rb::rust_connection::RustConnection, u32);
#[cfg(target_os = "linux")]
impl Canvas {
    pub unsafe fn attach(parent: *mut std::ffi::c_void, width: i32, height: i32) -> Option<Self> {
        use x11rb::{connection::Connection, protocol::xproto::*};
        let (connection, _) = x11rb::connect(None).ok()?;
        let window = connection.generate_id().ok()?;
        connection
            .create_window(
                0,
                window,
                parent as usize as u32,
                0,
                0,
                width.try_into().ok()?,
                height.try_into().ok()?,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new().background_pixel(0x204080),
            )
            .ok()?
            .check()
            .ok()?;
        connection.map_window(window).ok()?.check().ok()?;
        connection.flush().ok()?;
        Some(Self(connection, window))
    }
    pub fn resize(&self, width: i32, height: i32) {
        use x11rb::{connection::Connection, protocol::xproto::ConnectionExt as _};
        let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
            return;
        };
        if let Ok(cookie) = self.0.configure_window(
            self.1,
            &x11rb::protocol::xproto::ConfigureWindowAux::new()
                .width(width)
                .height(height),
        ) {
            let _ = cookie.check();
        }
        let _ = self.0.flush();
    }
}
#[cfg(target_os = "linux")]
impl Drop for Canvas {
    fn drop(&mut self) {
        use x11rb::{connection::Connection, protocol::xproto::ConnectionExt};
        let _ = self.0.destroy_window(self.1);
        let _ = self.0.flush();
    }
}
