#![deny(unsafe_code)]

//! Test-only stereo to 5.1 matrix plugin. Never ship this library.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod helper;
mod plugin;
mod probe;
mod vst3;

mod visual;
#[cfg(target_os = "linux")]
mod wayland;
