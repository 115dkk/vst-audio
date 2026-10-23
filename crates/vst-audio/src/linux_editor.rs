/// Linux floating editor backend. Other operating systems ignore this setting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum LinuxEditor {
    /// Native Wayland for supporting VST 3 views, then the session X display.
    #[default]
    Auto,
    /// Native Wayland only (VST 3 plugins that support it).
    Wayland,
    /// The session's X display (`DISPLAY`): Xorg or desktop XWayland.
    X11,
}
impl LinuxEditor {
    pub(crate) fn argument(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Wayland => "wayland",
            Self::X11 => "x11",
        }
    }
}
