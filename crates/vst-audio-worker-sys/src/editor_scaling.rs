//! Shared policy, kept separate from the foreign view's acceptance of a scale.
use crate::{editor::EditorWindow, plugin::PluginError};
use vst_audio_protocol::{EditorScaling, ParentWindow};

pub(crate) fn validate(
    mode: EditorScaling,
    parent: Option<ParentWindow>,
) -> Result<(), PluginError> {
    if mode == EditorScaling::Stretch
        && !cfg!(target_os = "macos")
        && (!cfg!(windows) || parent.is_some())
    {
        return Err(PluginError(
            "stretched editors need a Windows floating window".into(),
        ));
    }
    Ok(())
}
pub(crate) fn requests_scale(mode: EditorScaling) -> bool {
    matches!(mode, EditorScaling::Auto | EditorScaling::Plugin)
}
pub(crate) fn fallback(mode: EditorScaling, parent: Option<ParentWindow>) -> EditorScaling {
    if cfg!(windows)
        && parent.is_none()
        && matches!(mode, EditorScaling::Auto | EditorScaling::Stretch)
    {
        EditorScaling::Stretch
    } else {
        EditorScaling::Off
    }
}
pub(crate) fn open(
    title: &str,
    parent: Option<ParentWindow>,
    width: i32,
    height: i32,
    mode: EditorScaling,
) -> Result<EditorWindow, PluginError> {
    validate(mode, parent)?;
    match parent {
        Some(parent) => EditorWindow::open_embedded(parent, width, height)
            .map_err(|e| PluginError(e.to_string())),
        None => {
            #[cfg(windows)]
            let result =
                EditorWindow::open_scaled(title, width, height, mode == EditorScaling::Stretch);
            #[cfg(not(windows))]
            let result = EditorWindow::open(title, width, height);
            result.map_err(|e| PluginError(e.to_string()))
        }
    }
}
pub(crate) fn platform_scale(window: &EditorWindow) -> f32 {
    #[cfg(not(target_os = "macos"))]
    {
        window.scale()
    }
    #[cfg(target_os = "macos")]
    {
        let _ = window;
        1.0
    }
}
#[cfg(windows)]
pub(crate) fn scope(window: &EditorWindow) -> crate::editor::DpiScope {
    window.dpi_scope()
}
#[cfg(not(windows))]
pub(crate) struct DpiScope;
#[cfg(not(windows))]
pub(crate) fn scope(_window: &EditorWindow) -> DpiScope {
    DpiScope
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_auto_and_plugin_request_scale() {
        assert!(requests_scale(EditorScaling::Auto));
        assert!(requests_scale(EditorScaling::Plugin));
        assert!(!requests_scale(EditorScaling::Off));
        assert!(!requests_scale(EditorScaling::Stretch));
    }
    #[test]
    fn plugin_and_off_never_fall_back_to_stretch() {
        assert_eq!(fallback(EditorScaling::Plugin, None), EditorScaling::Off);
        assert_eq!(fallback(EditorScaling::Off, None), EditorScaling::Off);
        assert_eq!(
            fallback(EditorScaling::Auto, Some(ParentWindow::X11 { window: 1 })),
            EditorScaling::Off
        );
    }
}
