#![forbid(unsafe_code)]
//! Safe application-side orchestration for isolated audio plugins.
pub mod watch;
pub use layout::wave_bits;
pub use vst_audio_protocol::{Bus, ClassInfo, PluginFormat, layout, state};
mod process;
pub use process::{ControlEvent, PluginControl, PluginProcess, StartError, Started};
pub use vst_audio_transport_sys::{Event, SharedEvent, SharedRegion};
