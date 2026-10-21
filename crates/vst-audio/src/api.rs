//! GUI- and executor-independent application API.
use crate::audio::{AudioEndpoint, PreparedAudio};
use crate::{ControlEvent, PluginControl, PluginFormat, PluginProcess, StartError, Started};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use vst_audio_protocol::{Action, Message};

/// Application-selected processing settings. No sound device is opened.
#[derive(Clone, Debug)]
pub struct Configuration {
    pub sample_rate: u32,
    /// Exact number of frames per realtime call.
    pub quantum: u32,
    pub fallback: Fallback,
    pub sample_format: vst_audio_protocol::SampleFormat,
    pub offline: bool,
    pub inputs: Option<Vec<vst_audio_protocol::BusConfiguration>>,
    pub outputs: Option<Vec<vst_audio_protocol::BusConfiguration>>,
}
impl Default for Configuration {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            quantum: 128,
            fallback: Fallback::Silence,
            sample_format: vst_audio_protocol::SampleFormat::F32,
            offline: false,
            inputs: None,
            outputs: None,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fallback {
    Silence,
    /// Delay input by pipeline + plugin latency; absent input channels are zero.
    DelayedInput,
}

#[derive(Clone, Debug)]
pub struct PluginDescriptor {
    pub path: PathBuf,
    pub format: PluginFormat,
    pub class_id: Option<[u8; 16]>,
}
impl PluginDescriptor {
    pub fn new(path: impl Into<PathBuf>, format: PluginFormat) -> Self {
        Self {
            path: path.into(),
            format,
            class_id: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Host {
    worker: PathBuf,
}
impl Host {
    pub fn new(worker: impl Into<PathBuf>) -> Self {
        Self {
            worker: worker.into(),
        }
    }
    pub fn worker_path(&self) -> &Path {
        &self.worker
    }
    /// Blocking control-thread operation. The scan itself runs out of process.
    pub fn scan(&self, plugin: &PluginDescriptor) -> Result<Vec<crate::ClassInfo>, StartError> {
        PluginProcess::scan(&self.worker, &plugin.path, plugin.format)
    }
    /// Blocking startup. GUI applications should use `spawn_async`.
    pub fn spawn(
        &self,
        plugin: &PluginDescriptor,
        config: &Configuration,
        state: Option<&[u8]>,
    ) -> Result<Instance, StartError> {
        let options = crate::ProcessingOptions {
            sample_rate: config.sample_rate,
            quantum: config.quantum,
            sample_format: config.sample_format,
            offline: config.offline,
            inputs: config.inputs.clone(),
            outputs: config.outputs.clone(),
        };
        let started = PluginProcess::start_configured(
            &self.worker,
            &plugin.path,
            plugin.format,
            plugin.class_id,
            &options,
            state,
        )?;
        Instance::new(started, config.fallback)
    }
    /// Starts loading on a dedicated control thread; polling never blocks the GUI.
    pub fn spawn_async(
        &self,
        plugin: PluginDescriptor,
        config: Configuration,
        state: Option<Vec<u8>>,
    ) -> LoadTask {
        let host = self.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let _ = tx.send(host.spawn(&plugin, &config, state.as_deref()));
        });
        LoadTask(rx)
    }
}

#[derive(Debug)]
pub struct LoadTask(mpsc::Receiver<Result<Instance, StartError>>);
impl LoadTask {
    pub fn try_complete(&self) -> Result<Option<Instance>, StartError> {
        match self.0.try_recv() {
            Ok(result) => result.map(Some),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(StartError(
                "load task disconnected or already consumed".into(),
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct RequestId(pub u64);
#[derive(Clone, Debug)]
pub enum Command {
    OpenEditor,
    CloseEditor,
    ListParameters { start: u32, count: u16 },
    SetParameter { id: u32, value: f64 },
    SaveState,
    RestoreState(Vec<u8>),
    Shutdown,
}
#[derive(Clone, Debug)]
pub enum HostEvent {
    Completed {
        request: RequestId,
    },
    State {
        request: Option<RequestId>,
        bytes: Vec<u8>,
    },
    Parameters {
        request: RequestId,
        total: u32,
        entries: Vec<vst_audio_protocol::Parameter>,
    },
    EditorClosed,
    ParametersChanged,
    LatencyChanged(u32),
    RequestFailed {
        request: RequestId,
        reason: String,
    },
    Failed {
        reason: String,
    },
    RecoveryRequired,
    Stopped,
    /// The GUI was slower than the bounded notification queue. `status` remains
    /// available independently; request a fresh state after draining notifications.
    NotificationsLost,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Running,
    Failed,
    Stopped,
}

#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) stop: AtomicBool,
    pub(crate) status: AtomicU8,
    overflow: AtomicBool,
    next_request: AtomicU64,
    pub(crate) generation: AtomicU64,
}
#[derive(Clone, Debug)]
pub struct Controller {
    requests: mpsc::SyncSender<(RequestId, Command)>,
    shared: Arc<Shared>,
}
impl Controller {
    /// Bounded, nonblocking GUI/control request. Never call from an audio callback:
    /// some commands own state allocations.
    pub fn request(&self, command: Command) -> Result<RequestId, StartError> {
        if let Command::RestoreState(state) = &command
            && state.len() > vst_audio_protocol::MAX_STATE_BYTES - 8
        {
            return Err(StartError("state too large".into()));
        }
        if self.status() != Status::Running {
            return Err(StartError("plugin is not running".into()));
        }
        let id = self
            .shared
            .next_request
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| StartError("request counter exhausted".into()))?;
        let id = RequestId(id);
        self.requests
            .try_send((id, command))
            .map_err(|_| StartError("control queue full or closed".into()))?;
        Ok(id)
    }
    pub fn status(&self) -> Status {
        status(&self.shared)
    }
}
fn status(shared: &Shared) -> Status {
    match shared.status.load(Ordering::Acquire) {
        0 => Status::Running,
        1 => Status::Failed,
        _ => Status::Stopped,
    }
}

/// Owns one worker's lifetime. Drop on a control thread. Dropping this owner
/// requests teardown even if cloned controllers or its audio endpoint remain.
#[derive(Debug)]
pub struct Instance {
    controller: Controller,
    events: mpsc::Receiver<HostEvent>,
    audio: Option<AudioEndpoint>,
}
impl Instance {
    fn new(started: Started, fallback: Fallback) -> Result<Self, StartError> {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            status: AtomicU8::new(0),
            overflow: AtomicBool::new(false),
            next_request: AtomicU64::new(1),
            generation: AtomicU64::new(1),
        });
        let Started {
            control,
            region,
            work,
            publisher,
            geometry,
            latency_frames,
            setup,
            capabilities,
            ..
        } = started;
        let prepared = PreparedAudio {
            region,
            work,
            publisher,
            geometry,
            latency: latency_frames,
            setup,
            capabilities,
            generation: 1,
        };
        let (updates, next) = rtrb::RingBuffer::new(2);
        let (retired, retirements) = rtrb::RingBuffer::new(2);
        let audio = AudioEndpoint::new(prepared, fallback, shared.clone(), next, retired)?;
        let (requests, rx) = mpsc::sync_channel(8);
        let (tx, events) = mpsc::sync_channel(16);
        let control_shared = shared.clone();
        thread::Builder::new()
            .name("vst-audio-supervisor".into())
            .spawn(move || supervise(control, rx, tx, control_shared, updates, retirements))
            .map_err(|e| StartError(e.to_string()))?;
        Ok(Self {
            controller: Controller { requests, shared },
            events,
            audio: Some(audio),
        })
    }
    pub fn controller(&self) -> Controller {
        self.controller.clone()
    }
    pub fn status(&self) -> Status {
        self.controller.status()
    }
    /// Transfer the only realtime handle to the application's audio thread.
    pub fn take_audio(&mut self) -> Option<AudioEndpoint> {
        self.audio.take()
    }
    pub fn try_next_event(&self) -> Option<HostEvent> {
        if self
            .controller
            .shared
            .overflow
            .swap(false, Ordering::AcqRel)
        {
            Some(HostEvent::NotificationsLost)
        } else {
            self.events.try_recv().ok()
        }
    }
}
impl Drop for Instance {
    fn drop(&mut self) {
        self.controller.shared.stop.store(true, Ordering::Release);
    }
}

fn emit(tx: &mpsc::SyncSender<HostEvent>, shared: &Shared, event: HostEvent) {
    if tx.try_send(event).is_err() {
        shared.overflow.store(true, Ordering::Release);
    }
}
fn supervise(
    mut control: PluginControl,
    requests: mpsc::Receiver<(RequestId, Command)>,
    events: mpsc::SyncSender<HostEvent>,
    shared: Arc<Shared>,
    _updates: rtrb::Producer<PreparedAudio>,
    mut retirements: rtrb::Consumer<PreparedAudio>,
) {
    let mut pending = BTreeMap::new();
    let mut stopping = false;
    let mut failure_reported = false;
    loop {
        while let Ok(retired) = retirements.pop() {
            drop(retired);
        }
        if shared.stop.load(Ordering::Acquire) && !stopping {
            let _ = control.shutdown();
            stopping = true;
        }
        for _ in 0..8 {
            let Ok((id, command)) = requests.try_recv() else {
                break;
            };
            if pending.len() >= 64 {
                emit(
                    &events,
                    &shared,
                    HostEvent::RequestFailed {
                        request: id,
                        reason: "too many pending requests".into(),
                    },
                );
                continue;
            }
            let result = match command {
                Command::OpenEditor => control.open_editor(),
                Command::CloseEditor => control.request_close_editor(),
                Command::ListParameters { start, count } => control.parameters(start, count),
                Command::SetParameter { id, value } => control.set_parameter(id, value),
                Command::SaveState => control.get_state(),
                Command::RestoreState(bytes) => control.set_state(&bytes),
                Command::Shutdown => {
                    stopping = true;
                    match control.shutdown() {
                        Ok(()) => emit(&events, &shared, HostEvent::Completed { request: id }),
                        Err(e) => emit(
                            &events,
                            &shared,
                            HostEvent::RequestFailed {
                                request: id,
                                reason: e.to_string(),
                            },
                        ),
                    }
                    continue;
                }
            };
            match result {
                Ok(wire) => {
                    pending.insert(wire, id);
                }
                Err(e) => emit(
                    &events,
                    &shared,
                    HostEvent::RequestFailed {
                        request: id,
                        reason: e.to_string(),
                    },
                ),
            }
        }
        for event in control.poll(control.elapsed_ms()) {
            match event {
                ControlEvent::Message(frame) => {
                    let request = pending.remove(&frame.request);
                    match frame.message {
                        Message::State { blob } => emit(
                            &events,
                            &shared,
                            HostEvent::State {
                                request,
                                bytes: blob,
                            },
                        ),
                        Message::Ack => {
                            if let Some(request) = request {
                                emit(&events, &shared, HostEvent::Completed { request });
                            }
                        }
                        Message::Parameters { total, entries } => {
                            if let Some(request) = request {
                                emit(
                                    &events,
                                    &shared,
                                    HostEvent::Parameters {
                                        request,
                                        total,
                                        entries,
                                    },
                                );
                            }
                        }
                        Message::RequestError { text, .. } => {
                            if let Some(request) = request {
                                emit(
                                    &events,
                                    &shared,
                                    HostEvent::RequestFailed {
                                        request,
                                        reason: text,
                                    },
                                );
                            }
                        }
                        Message::EditorClosed => emit(&events, &shared, HostEvent::EditorClosed),
                        Message::ParamsChanged => {
                            emit(&events, &shared, HostEvent::ParametersChanged)
                        }
                        Message::LatencyChanged { frames } => {
                            emit(&events, &shared, HostEvent::LatencyChanged(frames))
                        }
                        Message::Fault { text, .. } => {
                            shared.status.store(1, Ordering::Release);
                            failure_reported = true;
                            emit(&events, &shared, HostEvent::Failed { reason: text });
                        }
                        _ => {}
                    }
                }
                ControlEvent::Exited(code) => {
                    let failed = !stopping || code != 0;
                    shared
                        .status
                        .store(if failed { 1 } else { 2 }, Ordering::Release);
                    if failed && !failure_reported {
                        emit(
                            &events,
                            &shared,
                            HostEvent::Failed {
                                reason: format!("worker exited with status {code}"),
                            },
                        );
                    }
                    if failed {
                        emit(&events, &shared, HostEvent::RecoveryRequired);
                    } else {
                        emit(&events, &shared, HostEvent::Stopped);
                    }
                    for (_, request) in pending {
                        emit(
                            &events,
                            &shared,
                            HostEvent::RequestFailed {
                                request,
                                reason: "worker exited".into(),
                            },
                        );
                    }
                    return;
                }
                ControlEvent::Action(Action::Kill { .. } | Action::Disable { .. }) => {
                    shared.status.store(1, Ordering::Release);
                    if !stopping && !failure_reported {
                        failure_reported = true;
                        emit(
                            &events,
                            &shared,
                            HostEvent::Failed {
                                reason: "worker deadline exceeded".into(),
                            },
                        );
                    }
                }
                _ => {}
            }
        }
        // This supervisor never waits on a plugin call or pipe write.
        thread::sleep(Duration::from_millis(5));
    }
}
