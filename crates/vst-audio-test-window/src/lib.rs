#![deny(unsafe_code)]
//! Test-only application windows. Native resources remain on the creating thread.
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::Parent;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;
#[cfg(windows)]
pub use windows::Parent;
