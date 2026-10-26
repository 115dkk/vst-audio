//! GUI- and executor-independent application API.
use crate::audio::{AudioEndpoint, PreparedAudio};
use crate::{
    Architecture, BlockEndpoint, PluginFormat, PluginProcess, StartError, StartErrorKind, Started,
    plugin_architectures,
};
use std::{
    cell::Cell,
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
    /// Worker processing quantum. A callback may be any length up to
    /// `max_callback_frames`, which defaults to this value.
    pub quantum: u32,
    /// Largest accepted callback. Longer callbacks are refused because later
    /// quanta could not be processed in time. Latency is
    /// `(ceil(max_callback_frames / quantum) + 1) * quantum + plugin latency`.
    pub max_callback_frames: Option<u32>,
    /// Ring slots per lane. Allowed values are 2, 4, 8, and 16, and the value
    /// must be at least callback depth + 1. `None` uses the next allowed power
    /// of two at or above callback depth + 1.
    pub slots: Option<u32>,
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
            max_callback_frames: None,
            slots: None,
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
    /// Restart a failed worker the way the low-level watch does: after 0.5 s,
    /// 2 s, then 8 s, and give up (status `Failed`, `RecoveryRequired`) after
    /// three failures in ten minutes. A respawn restores the newest state, or the
    /// one before when the worker failed right after restoring the newest. The
    /// failure window and both states belong to the instance, not one process.
    Watch,
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
    fn callback_geometry(&self) -> Result<(u32, u32, u32), StartError> {
        let maximum = self.max_callback_frames.unwrap_or(self.quantum);
        if maximum == 0 || self.quantum == 0 {
            return Err(StartError::new(
                StartErrorKind::InvalidInput,
                format!(
                    "max_callback_frames {maximum} and quantum {} must both be nonzero",
                    self.quantum
                ),
            ));
        }
        let depth = maximum / self.quantum + u32::from(!maximum.is_multiple_of(self.quantum));
        let required = depth.saturating_add(1);
        let slots = self
            .slots
            .unwrap_or_else(|| required.checked_next_power_of_two().unwrap_or(u32::MAX));
        if slots < required || slots > crate::MAX_SLOTS || !slots.is_power_of_two() {
            return Err(StartError::new(
                StartErrorKind::InvalidInput,
                format!(
                    "max_callback_frames {maximum} with quantum {} requires at least {required} slots; slots {slots} must be a power of two and not exceed MAX_SLOTS ({})",
                    self.quantum,
                    crate::MAX_SLOTS
                ),
            ));
        }
        Ok((maximum, depth, slots))
    }

    fn options(&self, cancel: Option<Arc<AtomicBool>>) -> crate::ProcessingOptions {
        let (_, _, slots) = self
            .callback_geometry()
            .expect("configuration is validated before starting or recovering");
        crate::ProcessingOptions {
            cancel,
            sample_rate: self.sample_rate,
            quantum: self.quantum,
            slots,
            sequence_origin: 0,
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
        Err(StartError::new(
            StartErrorKind::Start,
            format!(
                "plugin is built for {}; no worker for those architectures is configured",
                architectures
                    .iter()
                    .map(|architecture| architecture.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ))
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
            return Err(StartError::new(
                StartErrorKind::InvalidInput,
                "invalid recovery policy",
            ));
        }
        let (max_callback_frames, depth, _) = config.callback_geometry()?;
        let launch = Launch {
            worker: self.worker_for(plugin)?,
            plugin: plugin.clone(),
            configuration: config.clone(),
        };
        let started = launch.start(state, cancel.clone())?;
        Instance::new(
            started,
            config.fallback,
            max_callback_frames,
            depth,
            launch,
            cancel,
        )
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
            Err(mpsc::TryRecvError::Disconnected) => Err(StartError::new(
                StartErrorKind::Disconnected,
                "load task disconnected or already consumed",
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
    /// Open a floating or embedded editor with an explicit scaling policy.
    OpenEditorWith {
        parent: Option<vst_audio_protocol::ParentWindow>,
        scaling: vst_audio_protocol::EditorScaling,
    },
    CloseEditor,
    ListParameters {
        start: u32,
        count: u16,
    },
    SetParameter {
        id: u32,
        value: f64,
    },
    SaveState,
    RestoreState(Vec<u8>),
    /// Start a new worker from the newest saved state, or the one before it with
    /// `use_previous`. Accepted while `Failed` and also while `Running`, where it
    /// deliberately replaces the healthy worker: a forced restart.
    Recover {
        use_previous: bool,
    },
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
    EditorResized {
        width: u32,
        height: u32,
    },
    /// A plugin-originated edit, normally from its editor. App `SetParameter` and
    /// state restores do not emit this. `ParametersChanged` reports bulk changes
    /// or lost edit notifications.
    ParameterEdited {
        id: u32,
        phase: vst_audio_protocol::EditPhase,
        value: f64,
    },
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

/// Where the plugin session stands, as the supervisor and the audio thread
/// agree on it. Stored in `Shared.status`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum Lifecycle {
    Running = 0,
    Failed = 1,
    Stopped = 2,
    /// A replacement worker's resources were handed to the audio thread; audio
    /// keeps processing (the old generation until it adopts the new one).
    Replacing = 3,
    /// The worker is gone and recovery is pending or scheduled; audio must not process.
    Recovering = 4,
}
impl Lifecycle {
    fn from_wire(value: u8) -> Self {
        match value {
            0 => Self::Running,
            1 => Self::Failed,
            2 => Self::Stopped,
            3 => Self::Replacing,
            4 => Self::Recovering,
            _ => Self::Stopped,
        }
    }
    /// Audio may process and deliver blocks in this state.
    pub(crate) fn processing_allowed(self) -> bool {
        matches!(self, Self::Running | Self::Replacing)
    }
    pub(crate) fn status(self) -> Status {
        match self {
            Self::Running => Status::Running,
            Self::Failed => Status::Failed,
            Self::Replacing | Self::Recovering => Status::Recovering,
            Self::Stopped => Status::Stopped,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) stop: Arc<AtomicBool>,
    lifecycle: AtomicU8,
    pub(crate) worker_pid: AtomicU32,
    pub(crate) overflow: AtomicBool,
    next_request: AtomicU64,
    pub(crate) generation: AtomicU64,
    // The runtime latency and the low 32 bits of its generation in one word, so
    // the audio thread never reads a torn pair (audit #11 TD-05).
    runtime_latency: AtomicU64,
    pub(crate) applied_generation: AtomicU64,
    pub(crate) processed_generation: AtomicU64,
    pub(crate) history: Mutex<vst_audio_protocol::StateHistory>,
}
impl Shared {
    pub(crate) fn set_lifecycle(&self, lifecycle: Lifecycle) {
        self.lifecycle.store(lifecycle as u8, Ordering::Release);
    }
    pub(crate) fn lifecycle(&self) -> Lifecycle {
        Lifecycle::from_wire(self.lifecycle.load(Ordering::Acquire))
    }

    pub(crate) fn publish_runtime_latency(&self, generation: u64, frames: u32) {
        self.runtime_latency
            .store(runtime_latency_word(generation, frames), Ordering::Release);
    }

    /// The latest runtime latency, if it belongs to `generation`.
    pub(crate) fn runtime_latency(&self, generation: u64) -> Option<u32> {
        let word = self.runtime_latency.load(Ordering::Acquire);
        ((word >> 32) as u32 == generation as u32).then_some(word as u32)
    }
}
fn runtime_latency_word(generation: u64, frames: u32) -> u64 {
    u64::from(generation as u32) << 32 | u64::from(frames)
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
            && state.len() > vst_audio_protocol::MAX_STATE_BODY_BYTES
        {
            return Err(StartError::new(
                StartErrorKind::InvalidInput,
                "state too large",
            ));
        }
        if self.status() != Status::Running
            && !matches!(
                (&command, self.status()),
                (Command::Recover { .. }, Status::Failed) | (Command::Shutdown, _)
            )
        {
            return Err(StartError::new(
                StartErrorKind::NotRunning,
                "plugin is not running",
            ));
        }
        let id = self
            .shared
            .next_request
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| StartError::new(StartErrorKind::Start, "request counter exhausted"))?;
        let id = RequestId(id);
        self.requests.try_send((id, command)).map_err(|error| {
            let kind = match error {
                mpsc::TrySendError::Full(_) => StartErrorKind::QueueFull,
                mpsc::TrySendError::Disconnected(_) => StartErrorKind::Disconnected,
            };
            StartError::new(kind, "control queue full or closed")
        })?;
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
    shared.lifecycle().status()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_queue_dequeues_after_reporting_loss_under_pressure() {
        let shared = Shared::for_audio_test(0);
        let (send, events) = mpsc::sync_channel(2);
        send.send(HostEvent::Stopped).unwrap();
        let queue = NotificationQueue::new(events, shared.clone());

        shared.overflow.store(true, Ordering::Release);
        assert!(matches!(
            queue.try_next(),
            Some(HostEvent::NotificationsLost)
        ));
        shared.overflow.store(true, Ordering::Release);
        assert!(matches!(queue.try_next(), Some(HostEvent::Stopped)));
        assert!(matches!(
            queue.try_next(),
            Some(HostEvent::NotificationsLost)
        ));
    }

    #[test]
    fn runtime_latency_is_read_whole_for_its_generation() {
        let shared = Shared::for_audio_test(64);
        assert_eq!(shared.runtime_latency(1), Some(64));
        shared.publish_runtime_latency(2, 128);
        assert_eq!(shared.runtime_latency(1), None);
        assert_eq!(shared.runtime_latency(2), Some(128));
        // Only the generation's low 32 bits are kept; equality still holds past them.
        let late = (1 << 32) + 5;
        shared.publish_runtime_latency(late, 256);
        assert_eq!(shared.runtime_latency(late), Some(256));
        assert_eq!(shared.runtime_latency(2), None);
    }

    #[test]
    fn controller_request_distinguishes_full_and_disconnected_queues() {
        let shared = Shared::for_audio_test(0);
        let (requests, receive) = mpsc::sync_channel(1);
        let controller = Controller { requests, shared };

        controller.request(Command::SaveState).unwrap();
        let full = controller.request(Command::SaveState).unwrap_err();
        assert_eq!(full.kind(), crate::StartErrorKind::QueueFull);
        assert_eq!(full.to_string(), "control queue full or closed");

        drop(receive);
        let disconnected = controller.request(Command::SaveState).unwrap_err();
        assert_eq!(disconnected.kind(), crate::StartErrorKind::Disconnected);
        assert_eq!(disconnected.to_string(), "control queue full or closed");
    }

    #[test]
    fn controller_request_reports_invalid_input_and_not_running() {
        let shared = Shared::for_audio_test(0);
        let (requests, _receive) = mpsc::sync_channel(1);
        let controller = Controller {
            requests,
            shared: shared.clone(),
        };

        let invalid = controller
            .request(Command::RestoreState(vec![
                0;
                vst_audio_protocol::MAX_STATE_BODY_BYTES
                    + 1
            ]))
            .unwrap_err();
        assert_eq!(invalid.kind(), StartErrorKind::InvalidInput);
        assert_eq!(invalid.to_string(), "state too large");

        shared.set_lifecycle(Lifecycle::Stopped);
        let not_running = controller.request(Command::SaveState).unwrap_err();
        assert_eq!(not_running.kind(), StartErrorKind::NotRunning);
        assert_eq!(not_running.to_string(), "plugin is not running");
    }

    #[test]
    fn callback_slots_are_power_of_two_and_cover_depth_plus_one() {
        let config = Configuration {
            quantum: 128,
            max_callback_frames: Some(256),
            ..Default::default()
        };
        assert_eq!(config.callback_geometry().unwrap(), (256, 2, 4));

        let invalid = Configuration {
            slots: Some(5),
            ..Default::default()
        };
        let error = invalid.callback_geometry().unwrap_err();
        assert!(error.to_string().contains("must be a power of two"));
    }

    #[test]
    fn lifecycle_preserves_status_and_processing_rules() {
        let cases = [
            (Lifecycle::Running, Status::Running, true),
            (Lifecycle::Failed, Status::Failed, false),
            (Lifecycle::Stopped, Status::Stopped, false),
            (Lifecycle::Replacing, Status::Recovering, true),
            (Lifecycle::Recovering, Status::Recovering, false),
        ];
        for (lifecycle, status, processing_allowed) in cases {
            assert_eq!(lifecycle.status(), status);
            assert_eq!(lifecycle.processing_allowed(), processing_allowed);
            assert_eq!(Lifecycle::from_wire(lifecycle as u8), lifecycle);
        }
    }

    #[test]
    fn lifecycle_treats_unknown_values_as_stopped() {
        assert_eq!(Lifecycle::from_wire(5), Lifecycle::Stopped);
        assert_eq!(Lifecycle::from_wire(u8::MAX), Lifecycle::Stopped);
    }
}

#[cfg(test)]
impl Shared {
    pub(crate) fn for_audio_test(latency_frames: u32) -> Arc<Self> {
        Arc::new(Self {
            stop: Arc::new(AtomicBool::new(false)),
            lifecycle: AtomicU8::new(Lifecycle::Running as u8),
            worker_pid: AtomicU32::new(1),
            overflow: AtomicBool::new(false),
            next_request: AtomicU64::new(1),
            generation: AtomicU64::new(1),
            runtime_latency: AtomicU64::new(runtime_latency_word(1, latency_frames)),
            applied_generation: AtomicU64::new(1),
            processed_generation: AtomicU64::new(0),
            history: Mutex::new(vst_audio_protocol::StateHistory::new()),
        })
    }
}

/// Owns one worker's lifetime. Drop on a control thread. Dropping this owner
/// requests teardown even if cloned controllers or its audio endpoint remain.
#[derive(Debug)]
struct NotificationQueue {
    events: mpsc::Receiver<HostEvent>,
    shared: Arc<Shared>,
    reported_loss: Cell<bool>,
}
impl NotificationQueue {
    fn new(events: mpsc::Receiver<HostEvent>, shared: Arc<Shared>) -> Self {
        Self {
            events,
            shared,
            reported_loss: Cell::new(false),
        }
    }

    fn try_next(&self) -> Option<HostEvent> {
        if !self.reported_loss.get() && self.shared.overflow.swap(false, Ordering::AcqRel) {
            self.reported_loss.set(true);
            return Some(HostEvent::NotificationsLost);
        }

        if let Ok(event) = self.events.try_recv() {
            self.reported_loss.set(false);
            return Some(event);
        }

        if self.reported_loss.replace(false) && self.shared.overflow.swap(false, Ordering::AcqRel) {
            self.reported_loss.set(true);
            return Some(HostEvent::NotificationsLost);
        }

        None
    }
}

#[derive(Debug)]
pub struct Instance {
    controller: Controller,
    notifications: NotificationQueue,
    audio: Option<AudioEndpoint>,
}
impl Instance {
    #[cfg(all(test, unix))]
    pub(crate) fn for_supervision_test(
        requests: mpsc::SyncSender<(RequestId, Command)>,
        events: mpsc::Receiver<HostEvent>,
        shared: Arc<Shared>,
    ) -> Self {
        Self {
            controller: Controller {
                requests,
                shared: shared.clone(),
            },
            notifications: NotificationQueue::new(events, shared),
            audio: None,
        }
    }

    fn new(
        started: Started,
        fallback: Fallback,
        max_callback_frames: u32,
        depth: u32,
        launch: Launch,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, StartError> {
        let initial_latency = started.latency_frames;
        let shared = Arc::new(Shared {
            stop: cancel,
            lifecycle: AtomicU8::new(Lifecycle::Running as u8),
            worker_pid: AtomicU32::new(started.control.pid()),
            overflow: AtomicBool::new(false),
            next_request: AtomicU64::new(1),
            generation: AtomicU64::new(1),
            runtime_latency: AtomicU64::new(runtime_latency_word(1, initial_latency)),
            applied_generation: AtomicU64::new(1),
            processed_generation: AtomicU64::new(0),
            history: Mutex::new(started.control.state_history()),
        });
        let recovery = crate::supervision::RecoveryConfig {
            launch,
            geometry: started.geometry,
            setup: started.setup.clone(),
        };
        let (prepared, control) = PreparedAudio::from_started(started, 1).map_err(|error| {
            StartError::new(
                StartErrorKind::InvalidInput,
                format!("invalid block lane: {error:?}"),
            )
        })?;
        let (updates, next) = rtrb::RingBuffer::new(2);
        let (retired, retirements) = rtrb::RingBuffer::new(2);
        let audio = AudioEndpoint::new(
            prepared,
            fallback,
            max_callback_frames,
            depth,
            shared.clone(),
            next,
            retired,
        )?;
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
            .map_err(|e| StartError::new(StartErrorKind::Io, e.to_string()))?;
        Ok(Self {
            controller: Controller {
                requests,
                shared: shared.clone(),
            },
            notifications: NotificationQueue::new(events, shared),
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
    /// Transfer the only realtime handle as a block-synchronous endpoint.
    pub fn take_blocks(&mut self) -> Option<BlockEndpoint> {
        self.audio.take().map(AudioEndpoint::into_blocks)
    }
    pub fn try_next_event(&self) -> Option<HostEvent> {
        self.notifications.try_next()
    }
}
impl Drop for Instance {
    fn drop(&mut self) {
        self.controller.shared.stop.store(true, Ordering::Release);
    }
}
