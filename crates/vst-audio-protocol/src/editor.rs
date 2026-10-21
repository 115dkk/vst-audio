//! Framework-independent floating editor operations. Unsupported native features
//! return RequestError; no operation falls back to loading code in the parent.
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
#[derive(Clone, Debug, PartialEq)]
pub struct EditorStatus {
    pub visible: bool,
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
