//! Framework-independent native editor operations. Unsupported native features
//! return RequestError; no operation falls back to loading code in the parent.
/// A native window of the application that an editor may live inside.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParentWindow {
    /// An HWND of another process.
    Win32 { hwnd: u64 },
    /// An X11 window ID on the worker's display.
    X11 { window: u64 },
}

#[derive(Clone, Debug, PartialEq)]
pub enum EditorAction {
    Show,
    Hide,
    Focus,
    Resize { width: u32, height: u32 },
    Scale(f32),
    Inspect,
    Capture,
}
/// How an editor is fitted to the display's scale when it opens.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
#[non_exhaustive]
pub enum EditorScaling {
    /// The plugin scales itself when it can; otherwise the system stretches
    /// it where the platform can (Windows), else it stays 1:1.
    #[default]
    Auto = 0,
    /// Only the plugin scales; a plugin that cannot stays 1:1.
    Plugin = 1,
    /// The system stretches a 1:1 drawing (sharpness is lost). Supported by
    /// Windows floating editors and, in a Wayland session with `DISPLAY`, by a
    /// floating X11 editor that the desktop's XWayland scales. Embedded views,
    /// plain X11 sessions and Wayland sessions without `DISPLAY` reject it.
    Stretch = 2,
    /// No scaling: the plugin is told nothing and draws 1:1 physical pixels.
    Off = 3,
}
/// The backend actually hosting the editor, independent of the requested policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
#[non_exhaustive]
pub enum EditorPlatform {
    Win32 = 0,
    Cocoa = 1,
    X11 = 2,
    Wayland = 3,
}
#[derive(Clone, Debug, PartialEq)]
pub struct EditorStatus {
    pub platform: EditorPlatform,
    pub scaling: EditorScaling,
    pub visible: bool,
    pub embedded: bool,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub resize_supported: bool,
    pub scale_supported: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditorImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}
impl EditorImage {
    pub fn valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.width <= 2048
            && self.height <= 2048
            && self.rgba.len() == self.width as usize * self.height as usize * 4
    }
}
#[derive(Debug)]
pub enum EditorReply {
    Status(EditorStatus),
    Image(EditorImage),
}
