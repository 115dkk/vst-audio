#![deny(unsafe_code)]
//! OS resource ownership, with no plugin loading dependency.
#[cfg(windows)]
#[path = "windows/events.rs"]
pub mod events;
#[cfg(windows)]
#[path = "windows/mapping.rs"]
pub mod mapping;
#[cfg(windows)]
#[path = "windows/runtime.rs"]
pub mod runtime;
pub use events::{Event, SharedEvent};
pub use mapping::SharedRegion;
pub mod sandbox;

#[cfg(unix)]
#[path = "unix/events.rs"]
pub mod events;

#[cfg(unix)]
#[path = "unix/mapping.rs"]
pub mod mapping;

#[cfg(unix)]
#[path = "unix/runtime.rs"]
pub mod runtime;
