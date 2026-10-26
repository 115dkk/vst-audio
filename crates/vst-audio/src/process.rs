//! Application-side process proxy. Never calls a plugin ABI.
use crate::watch::{ControlWatch, elapsed_ms};
use std::{
    ffi::OsString,
    io::{Read, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};
use vst_audio_protocol::{
    Action, ClassInfo, CodecError, DecodedFrame, Frame, Message, PluginFormat, RequestKind,
    read_frame, write_frame,
};
use vst_audio_protocol::{
    BusConfiguration, BusInfo, BusKind, Capabilities, ProcessingSetup, SampleFormat,
};
use vst_audio_ring::{Geometry, MAX_SLOTS, Producer, Publisher, Signal, Waited};
use vst_audio_transport_sys::{
    Event, SharedRegion,
    runtime::{self, ChildProcess, Pipe},
};

const FIRST_REQUEST: u32 = 6;

fn following_request(request: u32) -> u32 {
    if request == u32::MAX {
        FIRST_REQUEST
    } else {
        request + 1
    }
}
/// Programmatic classification of application startup and control failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartErrorKind {
    /// The operation was cancelled by the caller.
    Cancelled,
    /// Caller input or configuration is invalid.
    InvalidInput,
    /// The current lifecycle does not allow the request.
    NotRunning,
    /// A bounded control queue has no available capacity.
    QueueFull,
    /// A control channel or load task has closed.
    Disconnected,
    /// An operating-system or I/O operation failed.
    Io,
    /// A control frame or protocol exchange was invalid.
    Protocol,
    /// Startup, worker rejection, or an operational resource limit prevented completion.
    Start,
}

/// A classified application error whose display text retains the original diagnostic.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct StartError {
    kind: StartErrorKind,
    message: String,
}
impl StartError {
    pub fn new(kind: StartErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> StartErrorKind {
        self.kind
    }
}

fn codec_error(error: CodecError) -> StartError {
    let kind = if matches!(error, CodecError::Io(_)) {
        StartErrorKind::Io
    } else {
        StartErrorKind::Protocol
    };
    StartError::new(kind, error.to_string())
}

struct Transport {
    process: Arc<ChildProcess>,
    cancel: Option<Arc<AtomicBool>>,
    pending: std::cell::RefCell<std::collections::VecDeque<Frame>>,
    send: SyncSender<Frame>,
    receive: Receiver<Result<Option<DecodedFrame>, CodecError>>,
}
impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transport").finish_non_exhaustive()
    }
}
impl Transport {
    fn new(
        mut input: impl Read + Send + 'static,
        mut output: impl Write + Send + 'static,
        cancel: Option<Arc<AtomicBool>>,
        process: Arc<ChildProcess>,
    ) -> Self {
        let (tx, rx) = mpsc::sync_channel(8);
        let (frames, received) = mpsc::sync_channel(2);
        thread::spawn(move || {
            loop {
                let result = read_frame(&mut input);
                let end = !matches!(result, Ok(Some(_)));
                if frames.send(result).is_err() || end {
                    break;
                }
            }
        });
        thread::spawn(move || {
            while let Ok(frame) = rx.recv() {
                if write_frame(&mut output, &frame).is_err() {
                    break;
                }
            }
        });
        Self {
            process,
            cancel,
            pending: Default::default(),
            send: tx,
            receive: received,
        }
    }
    fn send(&self, request: u32, message: Message) -> Result<(), StartError> {
        self.send
            .try_send(Frame {
                flags: 0,
                request,
                message,
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    StartError::new(StartErrorKind::QueueFull, error.to_string())
                }
                mpsc::TrySendError::Disconnected(_) => {
                    StartError::new(StartErrorKind::Disconnected, error.to_string())
                }
            })
    }
    fn response(&self, request: u32, timeout: Duration) -> Result<Frame, StartError> {
        let deadline = Instant::now() + timeout;
        loop {
            if self
                .cancel
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Acquire))
            {
                return Err(StartError::new(
                    StartErrorKind::Cancelled,
                    "operation cancelled",
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(StartError::new(
                    StartErrorKind::Start,
                    format!("worker response to request {request} timed out"),
                ));
            }
            let frame = match self
                .receive
                .recv_timeout(remaining.min(Duration::from_millis(20)))
            {
                Ok(frame) => frame.map_err(codec_error)?,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(e) => {
                    return Err(StartError::new(StartErrorKind::Disconnected, e.to_string()));
                }
            };
            let Some(frame) = frame else {
                let _ = self.process.wait(Duration::from_millis(100));
                return Err(StartError::new(
                    StartErrorKind::Disconnected,
                    format!(
                        "host closed control pipe; exit={:?}",
                        self.process.exit_code().ok().flatten()
                    ),
                ));
            };
            if let DecodedFrame::Known(frame) = frame {
                if let Message::Fault { code, text } | Message::RequestError { code, text } =
                    &frame.message
                {
                    return Err(StartError::new(
                        StartErrorKind::Start,
                        format!("host fault {code}: {text}"),
                    ));
                }
                if frame.request == request {
                    return Ok(frame);
                }
                if self.pending.borrow().len() >= 64 {
                    return Err(StartError::new(
                        StartErrorKind::Protocol,
                        "too many unsolicited startup messages",
                    ));
                }
                self.pending.borrow_mut().push_back(frame);
            }
        }
    }
}

/// The buses to activate, chosen after the plugin has loaded: one
/// configuration per declared bus, in declared order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BusChoice {
    pub inputs: Vec<BusConfiguration>,
    pub outputs: Vec<BusConfiguration>,
}

type SelectBuses = dyn Fn(&[BusInfo], &[BusInfo]) -> Result<BusChoice, String> + Send + Sync;

/// Chooses, once the plugin has loaded and declared its buses, which of them
/// to activate and which arrangement to request for each. It runs on the
/// starting thread between loading and preparing, so it must decide at once.
/// Recovery starts a new worker with the same selector.
#[derive(Clone)]
pub struct BusSelector(Arc<SelectBuses>);

impl BusSelector {
    pub fn new(
        select: impl Fn(&[BusInfo], &[BusInfo]) -> Result<BusChoice, String> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(select))
    }

    /// Each direction's first `Main` bus active, every other bus inactive,
    /// arrangements as declared. A direction without a main bus activates none.
    pub fn main_only() -> Self {
        Self::new(|inputs, outputs| {
            let choose = |buses: &[BusInfo]| {
                let first_main = buses.iter().position(|bus| bus.kind == BusKind::Main);
                buses
                    .iter()
                    .enumerate()
                    .map(|(index, bus)| bus.configuration(Some(index) == first_main))
                    .collect()
            };
            Ok(BusChoice {
                inputs: choose(inputs),
                outputs: choose(outputs),
            })
        })
    }
}
impl std::fmt::Debug for BusSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BusSelector(..)")
    }
}

#[derive(Clone, Debug)]
pub struct ProcessingOptions {
    pub cancel: Option<Arc<AtomicBool>>,
    pub sample_rate: u32,
    pub quantum: u32,
    pub slots: u32,
    /// Sequence the ring starts from. Ordinary use is 0; tests set it near
    /// `u32::MAX` to cross the 2^32 boundary with a real worker. The first
    /// published sequence is `sequence_origin.wrapping_add(1)`.
    pub sequence_origin: u32,
    pub sample_format: SampleFormat,
    pub offline: bool,
    pub sandbox: Option<crate::SandboxPolicy>,
    pub linux_editor: crate::LinuxEditor,
    pub buses: Option<BusSelector>,
    pub inputs: Option<Vec<BusConfiguration>>,
    pub outputs: Option<Vec<BusConfiguration>>,
}
impl Default for ProcessingOptions {
    fn default() -> Self {
        Self {
            cancel: None,
            sample_rate: 48000,
            quantum: 128,
            slots: 2,
            sequence_origin: 0,
            sample_format: SampleFormat::F32,
            offline: false,
            sandbox: None,
            linux_editor: crate::LinuxEditor::Auto,
            buses: None,
            inputs: None,
            outputs: None,
        }
    }
}
fn resolve_buses(
    inputs: &[BusInfo],
    outputs: &[BusInfo],
    options: &ProcessingOptions,
) -> Result<(Vec<BusConfiguration>, Vec<BusConfiguration>), StartError> {
    let selected = options
        .buses
        .as_ref()
        .map(|selector| (selector.0)(inputs, outputs))
        .transpose()
        .map_err(|reason| {
            StartError::new(
                StartErrorKind::InvalidInput,
                format!("bus selection: {reason}"),
            )
        })?;
    if selected.as_ref().is_some_and(|choice| {
        choice.inputs.len() != inputs.len() || choice.outputs.len() != outputs.len()
    }) {
        return Err(StartError::new(
            StartErrorKind::InvalidInput,
            "bus selection must configure every declared bus",
        ));
    }
    let default = |buses: &[BusInfo]| {
        buses
            .iter()
            .map(|bus| bus.configuration(true))
            .collect::<Vec<_>>()
    };
    Ok((
        options.inputs.clone().unwrap_or_else(|| {
            selected
                .as_ref()
                .map_or_else(|| default(inputs), |choice| choice.inputs.clone())
        }),
        options.outputs.clone().unwrap_or_else(|| {
            selected
                .as_ref()
                .map_or_else(|| default(outputs), |choice| choice.outputs.clone())
        }),
    ))
}

fn speaker_bits(buses: &[BusConfiguration]) -> Vec<u32> {
    buses
        .iter()
        .filter(|b| b.active)
        .flat_map(|b| {
            crate::layout::wave_bits(b.layout.arrangement)
                .unwrap_or_else(|_| vec![0; usize::from(b.layout.channels)])
        })
        .collect()
}

/// A host that loaded, configured and activated its plugin, with the ring it
/// marked Ready.
///
/// `publisher` is the ring's real-time half; it works on
/// `region.region()` (an `Arc<SharedRegion>` is a [`vst_audio_ring::RegionMemory`])
/// and signals `work` after each publish, so the engine can own all three
/// (`RingUpmixer`) without borrowing from this value. The [`Producer`] that
/// announced the header is gone by the time this is returned.
#[derive(Debug)]
pub struct Started {
    pub sandbox: Option<crate::SandboxReport>,
    pub setup: ProcessingSetup,
    pub declared_inputs: Vec<BusInfo>,
    pub declared_outputs: Vec<BusInfo>,
    pub capabilities: Capabilities,
    pub region: Arc<SharedRegion>,
    pub work: Arc<Event>,
    pub done: Arc<Event>,
    pub ready: Arc<Event>,
    pub publisher: Publisher,
    pub geometry: Geometry,
    pub input_wave_bits: Vec<u32>,
    pub output_wave_bits: Vec<u32>,
    pub latency_frames: u32,
    pub control: PluginControl,
}
#[derive(Debug)]
pub enum ControlEvent {
    Message(Frame),
    Action(Action),
    Exited(u32),
    NeedsRespawn,
}
#[derive(Debug)]
pub struct PluginControl {
    process: Arc<ChildProcess>,
    transport: Transport,
    region: Arc<SharedRegion>,
    work: Arc<Event>,
    watch: ControlWatch,
    next_request: u32,
    origin: Instant,
    clock_base: u64,
    exit_seen: bool,
    pub needs_respawn: bool,
}
impl PluginControl {
    pub fn state_history(&self) -> vst_audio_protocol::StateHistory {
        self.watch.states.clone()
    }
    fn now(&self) -> u64 {
        elapsed_ms(self.clock_base, self.origin)
    }
    fn request(&mut self, message: Message, kind: RequestKind) -> Result<u32, StartError> {
        let id = self.next_request;
        self.next_request = following_request(id);
        self.transport.send(id, message)?;
        self.watch.request(self.now(), id, kind);
        Ok(id)
    }
    pub fn parameters(&mut self, start: u32, count: u16) -> Result<u32, StartError> {
        if count == 0 || count > 32 {
            return Err(StartError::new(
                StartErrorKind::InvalidInput,
                "parameter page size must be 1..=32",
            ));
        }
        self.request(
            Message::ListParameters { start, count },
            RequestKind::StateSave,
        )
    }
    pub fn set_parameter(&mut self, id: u32, value: f64) -> Result<u32, StartError> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(StartError::new(
                StartErrorKind::InvalidInput,
                "invalid normalized parameter",
            ));
        }
        self.request(Message::SetParameter { id, value }, RequestKind::StateSave)
    }
    pub fn get_state(&mut self) -> Result<u32, StartError> {
        self.request(Message::GetState, RequestKind::StateSave)
    }
    pub fn set_state(&mut self, blob: &[u8]) -> Result<u32, StartError> {
        if blob.len() > vst_audio_protocol::MAX_STATE_BODY_BYTES {
            return Err(StartError::new(
                StartErrorKind::InvalidInput,
                "state too large",
            ));
        }
        self.request(
            Message::SetState {
                blob: blob.to_vec(),
            },
            RequestKind::StateSave,
        )
    }
    pub fn editor_action(
        &mut self,
        action: vst_audio_protocol::EditorAction,
    ) -> Result<u32, StartError> {
        self.request(Message::Editor(action), RequestKind::Editor)
    }
    pub fn open_editor(&mut self) -> Result<u32, StartError> {
        self.open_editor_with(None, vst_audio_protocol::EditorScaling::Auto)
    }
    pub fn open_editor_in(
        &mut self,
        parent: vst_audio_protocol::ParentWindow,
    ) -> Result<u32, StartError> {
        self.open_editor_with(Some(parent), vst_audio_protocol::EditorScaling::Auto)
    }
    /// Open a floating or embedded editor with an explicit scaling policy.
    pub fn open_editor_with(
        &mut self,
        parent: Option<vst_audio_protocol::ParentWindow>,
        scaling: vst_audio_protocol::EditorScaling,
    ) -> Result<u32, StartError> {
        let message = match parent {
            Some(parent) => Message::OpenEditorIn { parent, scaling },
            None => Message::OpenEditor { scaling },
        };
        self.request(message, RequestKind::Editor)
    }
    pub fn latency(&self) -> u32 {
        self.watch.latency()
    }
    pub fn elapsed_ms(&self) -> u64 {
        self.now()
    }
    pub fn request_close_editor(&mut self) -> Result<u32, StartError> {
        self.request(Message::CloseEditor, RequestKind::Editor)
    }
    pub fn close_editor(&mut self) -> Result<(), StartError> {
        self.request(Message::CloseEditor, RequestKind::Editor)
            .map(|_| ())
    }
    pub fn pid(&self) -> u32 {
        self.process.pid()
    }
    pub fn exit_code(&self) -> Result<Option<u32>, StartError> {
        self.process
            .exit_code()
            .map_err(|error| StartError::new(StartErrorKind::Io, error.to_string()))
    }
    pub fn poll(&mut self, now_ms: u64) -> Vec<ControlEvent> {
        let mut events = Vec::new();
        for _ in 0..64 {
            let pending = self.transport.pending.borrow_mut().pop_front();
            let frame = if let Some(frame) = pending {
                Ok(Some(DecodedFrame::Known(frame)))
            } else if let Ok(frame) = self.transport.receive.try_recv() {
                frame
            } else {
                break;
            };
            match frame {
                Ok(Some(DecodedFrame::Known(frame))) => {
                    let action = self.watch.received(now_ms, &frame);
                    if action == Action::SaveStateNow {
                        let _ = self.get_state();
                    }
                    if action != Action::None {
                        events.push(ControlEvent::Action(action));
                    }
                    events.push(ControlEvent::Message(frame));
                }
                Ok(Some(DecodedFrame::Unknown(_))) => {}
                _ => {
                    if !matches!(
                        self.watch.state(),
                        vst_audio_protocol::WatchState::Stopping
                            | vst_audio_protocol::WatchState::Stopped
                    ) {
                        let _ = self.process.terminate(3);
                    }
                }
            }
        }
        let exit = if self.exit_seen {
            None
        } else {
            self.process.exit_code().ok().flatten()
        };
        if let Some(code) = exit {
            self.exit_seen = true;
            events.push(ControlEvent::Exited(code));
        }
        for action in self.watch.poll(
            now_ms,
            self.region.region().heartbeat(),
            exit.map(|code| code as i32),
        ) {
            match action {
                Action::Kill { .. } | Action::Disable { .. } => {
                    let _ = self.process.terminate(3);
                }
                Action::SaveStateNow => {
                    let _ = self.get_state();
                }
                Action::Spawn => {
                    self.needs_respawn = true;
                    events.push(ControlEvent::NeedsRespawn);
                }
                _ => {}
            }
            events.push(ControlEvent::Action(action));
        }
        events
    }
    /// Preserve this value across respawns so the three-failure window and the
    /// two saved states belong to the plugin session, not one child process.
    pub fn take_watch(&mut self) -> ControlWatch {
        let now = self.now();
        std::mem::replace(&mut self.watch, ControlWatch::new(now))
    }
    /// `now_ms` uses the same caller-owned clock as subsequent `poll` calls.
    /// Keeps this new worker's latency while restoring the prior watch history.
    pub fn restore_watch(&mut self, mut watch: ControlWatch, now_ms: u64) {
        watch.restore_running(now_ms, self.watch.latency());
        self.origin = Instant::now();
        self.clock_base = now_ms;
        self.watch = watch;
    }
    pub fn shutdown(&mut self) -> Result<(), StartError> {
        let _ = self.get_state();
        self.watch.shutdown(self.now());
        self.transport.send(self.next_request, Message::Shutdown)?;
        self.work.signal();
        Ok(())
    }
}

#[derive(Debug)]
pub struct PluginProcess;
impl PluginProcess {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        exe: &Path,
        plugin: &Path,
        format: PluginFormat,
        class: Option<[u8; 16]>,
        rate: u32,
        state: Option<&[u8]>,
    ) -> Result<Started, StartError> {
        Self::start_with_frames(exe, plugin, format, class, rate, rate / 100, state)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn start_with_frames(
        exe: &Path,
        plugin: &Path,
        format: PluginFormat,
        class: Option<[u8; 16]>,
        rate: u32,
        frames: u32,
        state: Option<&[u8]>,
    ) -> Result<Started, StartError> {
        Self::start_configured(
            exe,
            plugin,
            format,
            class,
            &ProcessingOptions {
                sample_rate: rate,
                quantum: frames,
                ..Default::default()
            },
            state,
        )
    }
    pub fn start_configured(
        exe: &Path,
        plugin: &Path,
        format: PluginFormat,
        class: Option<[u8; 16]>,
        options: &ProcessingOptions,
        state: Option<&[u8]>,
    ) -> Result<Started, StartError> {
        if options.slots > MAX_SLOTS || !options.slots.is_power_of_two() || options.slots < 2 {
            return Err(StartError::new(
                StartErrorKind::InvalidInput,
                format!(
                    "ring slots must be a power of two in 2..={MAX_SLOTS}, got {}",
                    options.slots
                ),
            ));
        }
        let rate = options.sample_rate;
        let frames = options.quantum;
        let origin = Instant::now();
        let maximum = Geometry {
            sample_format: options.sample_format,
            lanes: 1,
            slots: options.slots,
            input_channels: 64,
            output_channels: 64,
            max_frames: frames,
            sample_rate: rate,
            latency_bound_frames: rate
                .checked_mul(2)
                .ok_or_else(|| StartError::new(StartErrorKind::InvalidInput, "invalid rate"))?,
        };
        let (process, transport, region, work, done, ready) = spawn(
            exe,
            plugin,
            maximum,
            options.cancel.clone(),
            options.sandbox.as_ref(),
            options.linux_editor,
        )?;
        let sandbox = sandbox_report(&transport, options.sandbox.as_ref())?;
        let mut watch = ControlWatch::new(0);
        let hello = transport.response(0, Duration::from_secs(5))?;
        if hello.message != Message::hello() {
            return Err(StartError::new(StartErrorKind::Protocol, "expected Hello"));
        }
        watch.received(0, &hello);
        transport.send(
            1,
            Message::Load {
                path: plugin
                    .to_str()
                    .ok_or_else(|| {
                        StartError::new(StartErrorKind::InvalidInput, "plugin path must be UTF-8")
                    })?
                    .into(),
                format,
                class_id: class,
            },
        )?;
        let loaded = transport.response(1, Duration::from_secs(60))?;
        watch.received(origin.elapsed().as_millis() as u64, &loaded);
        let Message::Loaded {
            outputs,
            inputs,
            latency_frames,
            ..
        } = loaded.message
        else {
            return Err(StartError::new(StartErrorKind::Protocol, "expected Loaded"));
        };
        transport.send(2, Message::Describe)?;
        let Message::Features(capabilities) =
            transport.response(2, Duration::from_secs(10))?.message
        else {
            return Err(StartError::new(
                StartErrorKind::Protocol,
                "expected capabilities",
            ));
        };
        if options.sample_format == SampleFormat::F64 && !capabilities.f64 {
            return Err(StartError::new(
                StartErrorKind::Start,
                "plugin does not support f64",
            ));
        }
        let (selected_inputs, selected_outputs) = resolve_buses(&inputs, &outputs, options)?;
        let setup = ProcessingSetup {
            sample_rate: rate,
            quantum: frames,
            sample_format: options.sample_format,
            offline: options.offline,
            inputs: selected_inputs,
            outputs: selected_outputs,
        };
        if !setup.valid() {
            return Err(StartError::new(
                StartErrorKind::InvalidInput,
                "invalid processing setup",
            ));
        }
        let input_wave_bits = speaker_bits(&setup.inputs);
        let output_wave_bits = speaker_bits(&setup.outputs);
        if latency_frames > maximum.latency_bound_frames {
            return Err(StartError::new(
                StartErrorKind::Start,
                "unsupported plugin latency",
            ));
        }
        let geometry = Geometry {
            input_channels: u32::from(ProcessingSetup::channels(&setup.inputs)),
            output_channels: u32::from(ProcessingSetup::channels(&setup.outputs)),
            ..maximum
        };
        // The producer borrows the mapping and the events only while it
        // announces the header and waits for the host's Ready; its real-time
        // half, the publisher, owns nothing and leaves with `Started`.
        let publisher = {
            let view = region
                .region_for(geometry)
                .map_err(|e| StartError::new(StartErrorKind::InvalidInput, format!("{e:?}")))?;
            let mut producer = Producer::new(view, [Some(&*work), None], &*ready);
            producer
                .announce_at(geometry, std::process::id(), options.sequence_origin)
                .map_err(|e| StartError::new(StartErrorKind::InvalidInput, format!("{e:?}")))?;
            transport.send(3, Message::Prepare(setup.clone()))?;
            expect_ack(transport.response(3, Duration::from_secs(10))?)?;
            if let Some(blob) = state {
                transport.send(
                    4,
                    Message::SetState {
                        blob: blob.to_vec(),
                    },
                )?;
                expect_ack(transport.response(4, Duration::from_secs(10))?)?;
            }
            transport.send(5, Message::Activate)?;
            expect_ack(transport.response(5, Duration::from_secs(10))?)?;
            if producer.wait_ready(Duration::from_secs(10)) != Waited::Signalled {
                return Err(StartError::new(
                    StartErrorKind::Start,
                    "ring readiness timeout",
                ));
            }
            producer
                .publisher()
                .ok_or_else(|| StartError::new(StartErrorKind::Start, "ring was not announced"))?
        };
        watch.activated(origin.elapsed().as_millis() as u64);
        let mut control = PluginControl {
            process,
            transport,
            region: region.clone(),
            work: work.clone(),
            watch,
            next_request: FIRST_REQUEST,
            origin,
            clock_base: 0,
            exit_seen: false,
            needs_respawn: false,
        };
        let saved = control.get_state()?;
        let state_reply = control.transport.response(saved, Duration::from_secs(10))?;
        control.watch.received(control.now(), &state_reply);
        for frame in control.transport.pending.borrow_mut().drain(..) {
            control.watch.received(control.now(), &frame);
        }
        let latency_frames = control.latency();
        if latency_frames > maximum.latency_bound_frames {
            return Err(StartError::new(
                StartErrorKind::Start,
                "unsupported configured plugin latency",
            ));
        }
        Ok(Started {
            sandbox,
            setup,
            declared_inputs: inputs,
            declared_outputs: outputs,
            capabilities,
            region,
            work,
            done,
            ready,
            publisher,
            geometry,
            input_wave_bits,
            output_wave_bits,
            latency_frames,
            control,
        })
    }
    pub fn scan(
        exe: &Path,
        path: &Path,
        format: PluginFormat,
    ) -> Result<Vec<ClassInfo>, StartError> {
        Self::scan_sandboxed(exe, path, format, None)
    }
    pub fn scan_sandboxed(
        exe: &Path,
        path: &Path,
        format: PluginFormat,
        policy: Option<&crate::SandboxPolicy>,
    ) -> Result<Vec<ClassInfo>, StartError> {
        // Frozen Scan wire message has no format field; VST 3 files/bundles use .vst3.
        if (format == PluginFormat::Vst3)
            != path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("vst3"))
        {
            return Err(StartError::new(
                StartErrorKind::InvalidInput,
                "scan format does not match plugin extension",
            ));
        }
        let geometry = Geometry {
            sample_format: vst_audio_ring::SampleFormat::F32,
            lanes: 1,
            slots: 2,
            input_channels: 2,
            output_channels: 32,
            max_frames: 480,
            sample_rate: 48000,
            latency_bound_frames: 96000,
        };
        let (process, transport, ..) =
            spawn(exe, path, geometry, None, policy, crate::LinuxEditor::Auto)?;
        sandbox_report(&transport, policy)?;
        if transport.response(0, Duration::from_secs(5))?.message != Message::hello() {
            return Err(StartError::new(StartErrorKind::Protocol, "expected Hello"));
        }
        transport.send(
            1,
            Message::Scan {
                path: path
                    .to_str()
                    .ok_or_else(|| {
                        StartError::new(StartErrorKind::InvalidInput, "plugin path must be UTF-8")
                    })?
                    .into(),
            },
        )?;
        let reply = transport.response(1, Duration::from_secs(60))?;
        let Message::Classes { entries } = reply.message else {
            return Err(StartError::new(
                StartErrorKind::Protocol,
                "expected Classes",
            ));
        };
        if !process.wait(Duration::from_secs(2)) {
            return Err(StartError::new(StartErrorKind::Start, "scan did not exit"));
        }
        Ok(entries)
    }
}
fn expect_ack(frame: Frame) -> Result<(), StartError> {
    if frame.message == Message::Ack {
        Ok(())
    } else {
        Err(StartError::new(StartErrorKind::Protocol, "expected Ack"))
    }
}
type Spawned = (
    Arc<ChildProcess>,
    Transport,
    Arc<SharedRegion>,
    Arc<Event>,
    Arc<Event>,
    Arc<Event>,
);
fn sandbox_report(
    transport: &Transport,
    policy: Option<&crate::SandboxPolicy>,
) -> Result<Option<crate::SandboxReport>, StartError> {
    policy
        .map(|policy| {
            let reply = transport.response(0, Duration::from_secs(10))?;
            let Message::Sandboxed { mechanism } = reply.message else {
                return Err(StartError::new(
                    StartErrorKind::Start,
                    "worker did not confirm sandbox installation",
                ));
            };
            Ok(crate::SandboxReport::applied(policy, mechanism))
        })
        .transpose()
}
fn spawn(
    exe: &Path,
    plugin: &Path,
    geometry: Geometry,
    cancel: Option<Arc<AtomicBool>>,
    policy: Option<&crate::SandboxPolicy>,
    linux_editor: crate::LinuxEditor,
) -> Result<Spawned, StartError> {
    let sandbox = vst_audio_transport_sys::sandbox::SandboxLaunch::new(policy, plugin)
        .map_err(|error| StartError::new(StartErrorKind::Io, error.to_string()))?;
    let region = Arc::new(
        SharedRegion::create(
            geometry
                .total_bytes()
                .map_err(|e| StartError::new(StartErrorKind::InvalidInput, format!("{e:?}")))?,
        )
        .map_err(|error| StartError::new(StartErrorKind::Io, error.to_string()))?,
    );
    let input = Pipe::create(false)
        .map_err(|error| StartError::new(StartErrorKind::Io, error.to_string()))?;
    let output = Pipe::create(true)
        .map_err(|error| StartError::new(StartErrorKind::Io, error.to_string()))?;
    let work = Arc::new(
        Event::create().map_err(|error| StartError::new(StartErrorKind::Io, error.to_string()))?,
    );
    let done = Arc::new(
        Event::create().map_err(|error| StartError::new(StartErrorKind::Io, error.to_string()))?,
    );
    let ready = Arc::new(
        Event::create().map_err(|error| StartError::new(StartErrorKind::Io, error.to_string()))?,
    );
    let parent = runtime::parent_handle()
        .map_err(|error| StartError::new(StartErrorKind::Io, error.to_string()))?;
    let values = [
        input.child_value() as u64,
        output.child_value() as u64,
        region.handle_value() as u64,
        work.handle_value(),
        done.handle_value(),
        ready.handle_value(),
        runtime::parent_value(&parent) as u64,
    ];
    let names = [
        "--control-in",
        "--control-out",
        "--region",
        "--work",
        "--done",
        "--ready",
        "--parent",
    ];
    let mut args = Vec::<OsString>::new();
    for (name, value) in names.into_iter().zip(values) {
        args.push(name.into());
        args.push(value.to_string().into());
    }
    args.push("--region-bytes".into());
    args.push(region.bytes().to_string().into());
    args.push("--slots".into());
    args.push(geometry.slots.to_string().into());
    args.push("--linux-editor".into());
    args.push(linux_editor.argument().into());
    sandbox.arguments(&mut args);
    let process = Arc::new(
        runtime::spawn_handles(exe, &args, &values, parent, sandbox)
            .map_err(|error| StartError::new(StartErrorKind::Io, error.to_string()))?,
    );
    drop(input.child);
    drop(output.child);
    let transport = Transport::new(output.parent, input.parent, cancel, process.clone());
    Ok((process, transport, region, work, done, ready))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vst_audio_protocol::Bus;

    fn bus(kind: BusKind) -> BusInfo {
        BusInfo {
            layout: Bus {
                channels: 2,
                arrangement: 3,
            },
            kind,
            name: String::new(),
            default_active: true,
        }
    }

    #[test]
    fn main_only_activates_the_first_main_bus_wherever_it_is() {
        let options = ProcessingOptions {
            buses: Some(BusSelector::main_only()),
            ..Default::default()
        };
        let (inputs, outputs) = resolve_buses(
            &[bus(BusKind::Aux), bus(BusKind::Main), bus(BusKind::Main)],
            &[bus(BusKind::Main), bus(BusKind::Aux)],
            &options,
        )
        .unwrap();
        assert_eq!(
            inputs.iter().map(|bus| bus.active).collect::<Vec<_>>(),
            [false, true, false]
        );
        assert_eq!(
            outputs.iter().map(|bus| bus.active).collect::<Vec<_>>(),
            [true, false]
        );
        assert_eq!(resolve_buses(&[], &[], &options).unwrap(), (vec![], vec![]));
    }

    #[test]
    fn explicit_buses_win_over_the_selector_per_direction() {
        let explicit = vec![bus(BusKind::Aux).configuration(true)];
        let options = ProcessingOptions {
            buses: Some(BusSelector::main_only()),
            inputs: Some(explicit.clone()),
            ..Default::default()
        };
        let (inputs, outputs) = resolve_buses(
            &[bus(BusKind::Aux)],
            &[bus(BusKind::Main), bus(BusKind::Aux)],
            &options,
        )
        .unwrap();
        assert_eq!(inputs, explicit);
        assert_eq!(
            outputs.iter().map(|bus| bus.active).collect::<Vec<_>>(),
            [true, false]
        );
    }

    #[test]
    fn a_selection_must_configure_every_declared_bus() {
        let wrong_length = ProcessingOptions {
            buses: Some(BusSelector::new(|_, _| {
                Ok(BusChoice {
                    inputs: vec![],
                    outputs: vec![],
                })
            })),
            ..Default::default()
        };
        assert_eq!(
            resolve_buses(&[bus(BusKind::Main)], &[], &wrong_length)
                .unwrap_err()
                .to_string(),
            "bus selection must configure every declared bus"
        );
        let failed = ProcessingOptions {
            buses: Some(BusSelector::new(|_, _| Err("no suitable buses".into()))),
            ..Default::default()
        };
        assert!(
            resolve_buses(&[], &[], &failed)
                .unwrap_err()
                .to_string()
                .starts_with("bus selection:")
        );
    }

    #[test]
    fn start_rejects_invalid_slots_before_launching_a_worker() {
        let options = ProcessingOptions {
            slots: 3,
            ..Default::default()
        };
        let error = PluginProcess::start_configured(
            Path::new("unused-worker"),
            Path::new("unused-plugin"),
            PluginFormat::Vst2,
            None,
            &options,
            None,
        )
        .unwrap_err();
        assert_eq!(error.kind(), StartErrorKind::InvalidInput);
        assert_eq!(
            error.to_string(),
            format!("ring slots must be a power of two in 2..={MAX_SLOTS}, got 3")
        );
    }

    #[test]
    fn request_numbers_wrap_to_the_first_application_request() {
        assert_eq!(following_request(FIRST_REQUEST), FIRST_REQUEST + 1);
        assert_eq!(following_request(u32::MAX - 1), u32::MAX);
        assert_eq!(following_request(u32::MAX), FIRST_REQUEST);
        assert!((0..FIRST_REQUEST).all(|reserved| following_request(u32::MAX) != reserved));
    }

    #[test]
    fn without_a_selector_every_declared_bus_is_active() {
        let (inputs, outputs) = resolve_buses(
            &[bus(BusKind::Aux), bus(BusKind::Main)],
            &[bus(BusKind::Aux)],
            &ProcessingOptions::default(),
        )
        .unwrap();
        assert!(inputs.iter().chain(&outputs).all(|bus| bus.active));
    }
}
