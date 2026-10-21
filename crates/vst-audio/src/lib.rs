#![forbid(unsafe_code)]
//! Safe application-side orchestration for isolated audio plugins.
pub mod watch;
pub use layout::wave_bits;
pub use vst_audio_protocol::{Bus, ClassInfo, PluginFormat, layout, state};
mod process;
pub use process::{
    ControlEvent, PluginControl, PluginProcess, ProcessingOptions, StartError, Started,
};
pub use vst_audio_transport_sys::{Event, SharedEvent, SharedRegion};

mod api;
pub use api::{
    Command, Configuration, Controller, Fallback, Host, HostEvent, Instance, LoadTask,
    PluginDescriptor, RecoveryPolicy, RequestId, Status,
};

pub use vst_audio_protocol::Parameter;

pub use vst_audio_protocol::{BusConfiguration, Capabilities, ProcessingSetup, SampleFormat};
pub use vst_audio_ring::{
    Event as AudioEvent, EventBuffer, EventData, MAX_EVENTS, MAX_SYSEX_BYTES, Transport,
};

mod audio;
pub use audio::{AudioEndpoint, AudioStatus, ProcessReport};

mod supervision;
