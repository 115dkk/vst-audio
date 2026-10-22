#![forbid(unsafe_code)]
//! Main-thread lifecycle and audio-thread runtime for the isolated executable.
pub mod host;
mod native;
mod vst3;
pub use vst_audio_protocol::{
    Bus, BusInfo, BusKind, ClassInfo, PluginError, PluginFormat, layout, state,
};
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginInfo {
    pub name: String,
    pub vendor: String,
    pub latency_frames: u32,
    pub inputs: Vec<BusInfo>,
    pub outputs: Vec<BusInfo>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PluginEvents {
    pub params_changed: bool,
    pub latency_changed: Option<u32>,
    pub editor_closed: bool,
    pub fault: Option<&'static str>,
}

/// Main-thread lifecycle and state interface. The Windows adapter creates an
/// independent processor handle for the audio thread. A non-waiting admission
/// gate excludes foreign processing during control calls; only the main thread
/// waits for an in-flight block. Closed admission leaves blocks uncompleted so
/// the engine uses fallback. All editor and state calls stay on the main thread.
pub trait Plugin {
    fn info(&self) -> PluginInfo;
    fn configure(
        &mut self,
        rate: u32,
        max_frames: u32,
        in_channels: u16,
        out_channels: u16,
    ) -> Result<(), PluginError>;
    fn configure_audio(
        &mut self,
        setup: &vst_audio_protocol::ProcessingSetup,
    ) -> Result<(), PluginError> {
        self.configure(
            setup.sample_rate,
            setup.quantum,
            vst_audio_protocol::ProcessingSetup::channels(&setup.inputs),
            vst_audio_protocol::ProcessingSetup::channels(&setup.outputs),
        )
    }
    fn capabilities(&self) -> vst_audio_protocol::Capabilities {
        vst_audio_protocol::Capabilities::default()
    }
    fn activate(&mut self) -> Result<(), PluginError>;
    fn deactivate(&mut self);
    /// Transfer the unique audio handle after configuration.
    fn take_processor(&mut self) -> Result<Box<dyn Processor>, PluginError>;
    fn parameters(
        &mut self,
        _start: u32,
        _count: u16,
    ) -> Result<(u32, Vec<vst_audio_protocol::Parameter>), PluginError> {
        Err(PluginError("parameters unsupported".into()))
    }
    fn set_parameter(&mut self, _id: u32, _value: f64) -> Result<(), PluginError> {
        Err(PluginError("parameters unsupported".into()))
    }
    fn get_state(&mut self) -> Result<Vec<u8>, PluginError>;
    fn set_state(&mut self, blob: &[u8]) -> Result<(), PluginError>;
    fn open_editor(&mut self) -> Result<(), PluginError>;
    fn open_editor_in(
        &mut self,
        _parent: vst_audio_protocol::ParentWindow,
    ) -> Result<(), PluginError> {
        Err(PluginError("editor embedding unsupported".into()))
    }
    fn editor_action(
        &mut self,
        _action: vst_audio_protocol::EditorAction,
    ) -> Result<vst_audio_protocol::EditorReply, PluginError> {
        Err(PluginError("editor operation unsupported".into()))
    }
    fn close_editor(&mut self) -> Result<(), PluginError> {
        Ok(())
    }
    fn idle(&mut self);
    fn take_events(&mut self) -> PluginEvents;
}

pub trait Processor: Send {
    /// In-place processing. Return false without changing samples when admission
    /// is closed; the ring must leave this sequence pending and retry on a wake.
    fn process(&mut self, planes: &mut [&mut [f32]], frames: usize) -> bool;

    fn process_block(
        &mut self,
        planes: &mut [&mut [f32]],
        frames: usize,
        _context: &vst_audio_ring::Transport,
        _input: &[vst_audio_ring::Event],
        _output: &mut vst_audio_ring::EventBuffer,
    ) -> bool {
        self.process(planes, frames)
    }
    fn process_block_f64(
        &mut self,
        _planes: &mut [&mut [f64]],
        _frames: usize,
        _context: &vst_audio_ring::Transport,
        _input: &[vst_audio_ring::Event],
        _output: &mut vst_audio_ring::EventBuffer,
    ) -> bool {
        false
    }
    /// Runs on the audio thread after its last block, before main-thread teardown.
    fn stop(&mut self) {}
}
