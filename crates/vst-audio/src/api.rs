//! GUI- and executor-independent application API.
use crate::audio::{AudioEndpoint, PreparedAudio};
use crate::{Architecture, PluginFormat, PluginProcess, StartError, Started, plugin_architectures};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
        mpsc,
    },
    thread,
};

/// Application-selected processing settings. No sound device is opened.
#[derive(Clone, Debug)]
pub struct Configuration {
    pub sample_rate: u32,
    /// Worker processing quantum; caller callbacks may have variable lengths.
    pub quantum: u32,
    pub fallback: Fallback,
    pub recovery: RecoveryPolicy,
    pub sample_format: vst_audio_protocol::SampleFormat,
    pub offline: bool,
    pub sandbox: Option<crate::SandboxPolicy>,
    pub linux_editor: crate::LinuxEditor,
    pub buses: Option<crate::BusSelector>,
    pub inputs: Option<Vec<vst_audio_protocol::BusConfiguration>>,
    pub outputs: Option<Vec<vst_audio_protocol::BusConfiguration>>,
}
impl Default for Configuration {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            quantum: 128,
            fallback: Fallback::Silence,
            recovery: RecoveryPolicy::Manual,
            sample_format: vst_audio_protocol::SampleFormat::F32,
            offline: false,
            sandbox: None,
            linux_editor: crate::LinuxEditor::Auto,
            buses: None,
            inputs: None,
            outputs: None,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecoveryPolicy {
    #[default]
    Manual,
    Automatic {
        attempts: u8,
        backoff_ms: u32,
    },
}
#[derive(Clone, Debug)]
pub(crate) struct Launch {
    pub worker: PathBuf,
    pub plugin: PluginDescriptor,
    pub configuration: Configuration,
}
impl Launch {
    pub fn start(
        &self,
        state: Option<&[u8]>,
        cancel: Arc<AtomicBool>,
    ) -> Result<Started, StartError> {
        PluginProcess::start_configured(
            &self.worker,
            &self.plugin.path,
            self.plugin.format,
            self.plugin.class_id,
            &self.configuration.options(Some(cancel)),
            state,
        )
    }
}
impl Configuration {
    fn options(&self, cancel: Option<Arc<AtomicBool>>) -> crate::ProcessingOptions {
        crate::ProcessingOptions {
            cancel,
            sample_rate: self.sample_rate,
            quantum: self.quantum,
            sample_format: self.sample_format,
            offline: self.offline,
            sandbox: self.sandbox.clone(),
            linux_editor: self.linux_editor,
            buses: self.buses.clone(),
            inputs: self.inputs.clone(),
            outputs: self.outputs.clone(),
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
    workers: Vec<(Architecture, PathBuf)>,
}
impl Host {
    pub fn new(worker: impl Into<PathBuf>) -> Self {
        Self {
            worker: worker.into(),
            workers: Vec::new(),
        }
    }
    /// A worker that runs plugins built for `architecture`; `new`'s worker is
    /// the host's own. Registering an architecture again replaces its worker.
    pub fn with_worker(mut self, architecture: Architecture, worker: impl Into<PathBuf>) -> Self {
        if architecture == Architecture::host() {
            self.worker = worker.into();
        } else if let Some(entry) = self
            .workers
            .iter_mut()
            .find(|entry| entry.0 == architecture)
        {
            entry.1 = worker.into();
        } else {
            self.workers.push((architecture, worker.into()));
        }
        self
    }
    /// Select the native worker when possible (also when the binary header
    /// cannot be read), otherwise the first registered compatible worker.
    /// This inspects files on the control thread, never on an audio callback.
    pub fn worker_for(&self, plugin: &PluginDescriptor) -> Result<PathBuf, StartError> {
        let Ok(architectures) = plugin_architectures(&plugin.path, plugin.format) else {
            return Ok(self.worker.clone());
        };
        if architectures.contains(&Architecture::host()) {
            return Ok(self.worker.clone());
        }
        if let Some((_, worker)) = self.workers.iter().find(|(architecture, _)| {
            architectures.contains(architecture) && architecture.runs_on_host()
        }) {
            return Ok(worker.clone());
        }
        Err(StartError(format!(
            "plugin is built for {}; no worker for those architectures is configured",
            architectures
                .iter()
                .map(|architecture| architecture.name())
                .collect::<Vec<_>>()
                .join(", ")
        )))
    }
    pub fn worker_path(&self) -> &Path {
        &self.worker
    }
    /// Blocking control-thread operation. The scan itself runs out of process.
    pub fn scan(&self, plugin: &PluginDescriptor) -> Result<Vec<crate::ClassInfo>, StartError> {
        PluginProcess::scan(&self.worker_for(plugin)?, &plugin.path, plugin.format)
    }
    /// Scan under the same opt-in restrictions as a processing worker.
    pub fn scan_sandboxed(
        &self,
        plugin: &PluginDescriptor,
        policy: &crate::SandboxPolicy,
    ) -> Result<Vec<crate::ClassInfo>, StartError> {
        PluginProcess::scan_sandboxed(
            &self.worker_for(plugin)?,
            &plugin.path,
            plugin.format,
            Some(policy),
        )
    }
    /// Blocking startup. GUI applications should use `spawn_async`.
    pub fn spawn(
        &self,
        plugin: &PluginDescriptor,
        config: &Configuration,
        state: Option<&[u8]>,
    ) -> Result<Instance, StartError> {
        self.spawn_cancel(plugin, config, state, Arc::new(AtomicBool::new(false)))
    }
    fn spawn_cancel(
        &self,
        plugin: &PluginDescriptor,
        config: &Configuration,
        state: Option<&[u8]>,
        cancel: Arc<AtomicBool>,
    ) -> Result<Instance, StartError> {
        if matches!(config.recovery,RecoveryPolicy::Automatic{attempts,backoff_ms} if attempts>8||backoff_ms>60000)
        {
            return Err(StartError("invalid recovery policy".into()));
        }
        let launch = Launch {
            worker: self.worker_for(plugin)?,
            plugin: plugin.clone(),
            configuration: config.clone(),
        };
        let started = launch.start(state, cancel.clone())?;
        Instance::new(started, config.fallback, launch, cancel)
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
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        thread::spawn(move || {
            let _ = tx.send(host.spawn_cancel(&plugin, &config, state.as_deref(), worker_cancel));
        });
        LoadTask {
            receive: rx,
            cancel,
            delivered: std::cell::Cell::new(false),
        }
    }
}

#[derive(Debug)]
pub struct LoadTask {
    receive: mpsc::Receiver<Result<Instance, StartError>>,
    cancel: Arc<AtomicBool>,
    delivered: std::cell::Cell<bool>,
}
impl Drop for LoadTask {
    fn drop(&mut self) {
        if !self.delivered.get() {
            self.cancel.store(true, Ordering::Release);
        }
    }
}
impl LoadTask {
    pub fn cancel(&self) {
        if !self.delivered.get() {
            self.cancel.store(true, Ordering::Release);
        }
    }
    pub fn try_complete(&self) -> Result<Option<Instance>, StartError> {
        match self.receive.try_recv() {
            Ok(result) => {
                self.delivered.set(true);
                result.map(Some)
            }
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
    Editor(vst_audio_protocol::EditorAction),
    OpenEditor,
    OpenEditorIn(vst_audio_protocol::ParentWindow),
    CloseEditor,
    ListParameters { start: u32, count: u16 },
    SetParameter { id: u32, value: f64 },
    SaveState,
    RestoreState(Vec<u8>),
    Recover { use_previous: bool },
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
    EditorStatus {
        request: RequestId,
        status: vst_audio_protocol::EditorStatus,
    },
    EditorImage {
        request: RequestId,
        image: vst_audio_protocol::EditorImage,
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
    Recovered {
        generation: u64,
    },
    Stopped,
    /// The GUI was slower than the bounded notification queue. `status` remains
    /// available independently; request a fresh state after draining notifications.
    NotificationsLost,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Running,
    Failed,
    Recovering,
    Stopped,
}

#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) stop: Arc<AtomicBool>,
    pub(crate) status: AtomicU8,
    pub(crate) worker_pid: AtomicU32,
    pub(crate) overflow: AtomicBool,
    next_request: AtomicU64,
    pub(crate) generation: AtomicU64,
    pub(crate) applied_generation: AtomicU64,
    pub(crate) processed_generation: AtomicU64,
    pub(crate) history: Mutex<vst_audio_protocol::StateHistory>,
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
        if self.status() != Status::Running
            && !matches!(
                (&command, self.status()),
                (Command::Recover { .. }, Status::Failed) | (Command::Shutdown, _)
            )
        {
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
    /// Current worker identity; may change after recovery.
    pub fn worker_pid(&self) -> u32 {
        self.shared.worker_pid.load(Ordering::Acquire)
    }
    pub fn status(&self) -> Status {
        status(&self.shared)
    }
}
fn status(shared: &Shared) -> Status {
    match shared.status.load(Ordering::Acquire) {
        0 => Status::Running,
        1 => Status::Failed,
        3 | 4 => Status::Recovering,
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
    fn new(
        started: Started,
        fallback: Fallback,
        launch: Launch,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, StartError> {
        let shared = Arc::new(Shared {
            stop: cancel,
            status: AtomicU8::new(0),
            worker_pid: AtomicU32::new(started.control.pid()),
            overflow: AtomicBool::new(false),
            next_request: AtomicU64::new(1),
            generation: AtomicU64::new(1),
            applied_generation: AtomicU64::new(1),
            processed_generation: AtomicU64::new(0),
            history: Mutex::new(started.control.state_history()),
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
        let recovery = crate::supervision::RecoveryConfig {
            launch,
            geometry,
            setup: setup.clone(),
        };
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
            .spawn(move || {
                crate::supervision::supervise(
                    control,
                    rx,
                    tx,
                    control_shared,
                    updates,
                    retirements,
                    recovery,
                )
            })
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
