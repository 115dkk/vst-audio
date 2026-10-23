#![forbid(unsafe_code)]
//! Safe application-side orchestration for isolated audio plugins.
pub mod watch;
pub use layout::wave_bits;
pub use vst_audio_protocol::{Bus, BusInfo, BusKind, ClassInfo, PluginFormat, layout, state};
mod linux_editor;
mod process;
pub use linux_editor::LinuxEditor;
pub use process::{
    BusChoice, BusSelector, ControlEvent, PluginControl, PluginProcess, ProcessingOptions,
    StartError, Started,
};
pub use vst_audio_transport_sys::sandbox::{SandboxPolicy, SandboxReport};
pub use vst_audio_transport_sys::{Event, SharedEvent, SharedRegion};

mod architecture;
pub use architecture::{Architecture, plugin_architectures};

mod api;
pub use api::{
    Command, Configuration, Controller, Fallback, Host, HostEvent, Instance, LoadTask,
    PluginDescriptor, RecoveryPolicy, RequestId, Status,
};

pub use vst_audio_protocol::{EditPhase, Parameter};

pub use vst_audio_protocol::{BusConfiguration, Capabilities, ProcessingSetup, SampleFormat};
pub use vst_audio_ring::{
    Event as AudioEvent, EventBuffer, EventData, MAX_EVENTS, MAX_SYSEX_BYTES, Transport,
};

mod audio;
pub use audio::{AudioEndpoint, AudioStatus, ProcessReport};

mod supervision;

pub use vst_audio_protocol::{
    EditorAction, EditorImage, EditorPlatform, EditorStatus, ParentWindow,
};
