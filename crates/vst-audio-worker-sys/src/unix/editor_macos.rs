#![allow(unsafe_code)]
//! AppKit belongs to the worker's main thread, independent of the caller's GUI.
use objc2::{
    MainThreadMarker, MainThreadOnly,
    rc::{Retained, autoreleasepool},
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEventMask, NSView,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize, NSString};
use std::{cell::Cell, io};

pub fn initialize() -> io::Result<()> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| io::Error::other("worker must start on macOS main thread"))?;
    autoreleasepool(|_| {
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();
    });
    Ok(())
}

#[derive(Debug)]
pub struct EditorWindow {
    window: Retained<NSWindow>,
    view: Retained<NSView>,
    closed: Cell<bool>,
}
impl EditorWindow {
    pub fn open(title: &str, width: i32, height: i32) -> io::Result<Self> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| io::Error::other("editor requires main thread"))?;
        autoreleasepool(|_| {
            let rect = NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(width.clamp(1, 8192) as f64, height.clamp(1, 8192) as f64),
            );
            // SAFETY: main-thread allocation; retain ownership and disable AppKit's
            // automatic release on close before any window can be shown.
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    rect,
                    NSWindowStyleMask::Titled
                        | NSWindowStyleMask::Closable
                        | NSWindowStyleMask::Miniaturizable,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            // SAFETY: retained Rust owner is solely responsible for releasing the window.
            unsafe { window.setReleasedWhenClosed(false) };
            window.setTitle(&NSString::from_str(title));
            let view = window
                .contentView()
                .ok_or_else(|| io::Error::other("window has no content view"))?;
            window.center();
            window.makeKeyAndOrderFront(None);
            Ok(Self {
                window,
                view,
                closed: Cell::new(false),
            })
        })
    }
    pub fn handle_value(&self) -> usize {
        Retained::as_ptr(&self.view) as usize
    }
    pub fn pump(&self) -> bool {
        autoreleasepool(|_| {
            let mtm = MainThreadMarker::new().expect("editor remains on main thread");
            let app = NSApplication::sharedApplication(mtm);
            let until = NSDate::distantPast();
            // SAFETY: immutable framework global, initialized by AppKit.
            let mode = unsafe { NSDefaultRunLoopMode };
            // Bound each dispatch batch so IPC and parent-liveness checks progress.
            for _ in 0..128 {
                let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
                    NSEventMask::Any,
                    Some(&until),
                    mode,
                    true,
                ) else {
                    break;
                };
                app.sendEvent(&event);
            }
            app.updateWindows();
            self.closed.get() || !self.window.isVisible()
        })
    }
    pub fn request_close(&self) -> io::Result<()> {
        // Defer destruction until the plugin receives removed/effEditClose.
        self.closed.set(true);
        Ok(())
    }
    pub fn resize(&self, width: i32, height: i32) -> io::Result<()> {
        self.window.setContentSize(NSSize::new(
            width.clamp(1, 8192) as f64,
            height.clamp(1, 8192) as f64,
        ));
        Ok(())
    }
}
impl Drop for EditorWindow {
    fn drop(&mut self) {
        self.window.close();
    }
}
