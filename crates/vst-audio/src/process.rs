//! Application-side process proxy. Never calls a plugin ABI.
use crate::watch::ControlWatch;
use std::{
    ffi::OsString,
    io::{Read, Write},
    path::Path,
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};
use vst_audio_protocol::{
    Action, ClassInfo, CodecError, DecodedFrame, Frame, Message, PluginFormat, RequestKind,
    read_frame, write_frame,
};
use vst_audio_protocol::{BusConfiguration, Capabilities, ProcessingSetup, SampleFormat};
use vst_audio_ring::{Geometry, Producer, Publisher, Signal, Waited};
use vst_audio_transport_sys::{
    Event, SharedRegion,
    runtime::{self, ChildProcess, Pipe},
};
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct StartError(pub String);
fn error(error: impl std::fmt::Display) -> StartError {
    StartError(error.to_string())
}

struct Transport {
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
    fn new(mut input: impl Read + Send + 'static, mut output: impl Write + Send + 'static) -> Self {
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
            .map_err(error)
    }
    fn response(&self, request: u32, timeout: Duration) -> Result<Frame, StartError> {
        let deadline = Instant::now() + timeout;
        loop {
            let frame = self
                .receive
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(error)?
                .map_err(error)?;
            let Some(frame) = frame else {
                return Err(StartError("host closed control pipe".into()));
            };
            if let DecodedFrame::Known(frame) = frame {
                if let Message::Fault { code, text } | Message::RequestError { code, text } =
                    &frame.message
                {
                    return Err(StartError(format!("host fault {code}: {text}")));
                }
                if frame.request == request {
                    return Ok(frame);
                }
                if self.pending.borrow().len() >= 64 {
                    return Err(StartError("too many unsolicited startup messages".into()));
                }
                self.pending.borrow_mut().push_back(frame);
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProcessingOptions {
    pub sample_rate: u32,
    pub quantum: u32,
    pub sample_format: SampleFormat,
    pub offline: bool,
    pub inputs: Option<Vec<BusConfiguration>>,
    pub outputs: Option<Vec<BusConfiguration>>,
}
impl Default for ProcessingOptions {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            quantum: 128,
            sample_format: SampleFormat::F32,
            offline: false,
            inputs: None,
            outputs: None,
        }
    }
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
    pub setup: ProcessingSetup,
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
    process: ChildProcess,
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
    fn now(&self) -> u64 {
        self.clock_base
            .saturating_add(self.origin.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
    }
    fn request(&mut self, message: Message, kind: RequestKind) -> Result<u32, StartError> {
        let id = self.next_request;
        self.next_request = self
            .next_request
            .checked_add(1)
            .ok_or_else(|| StartError("request number exhausted".into()))?;
        self.transport.send(id, message)?;
        self.watch.request(self.now(), id, kind);
        Ok(id)
    }
    pub fn parameters(&mut self, start: u32, count: u16) -> Result<u32, StartError> {
        if count == 0 || count > 32 {
            return Err(StartError("parameter page size must be 1..=32".into()));
        }
        self.request(
            Message::ListParameters { start, count },
            RequestKind::StateSave,
        )
    }
    pub fn set_parameter(&mut self, id: u32, value: f64) -> Result<u32, StartError> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(StartError("invalid normalized parameter".into()));
        }
        self.request(Message::SetParameter { id, value }, RequestKind::StateSave)
    }
    pub fn get_state(&mut self) -> Result<u32, StartError> {
        self.request(Message::GetState, RequestKind::StateSave)
    }
    pub fn set_state(&mut self, blob: &[u8]) -> Result<u32, StartError> {
        if blob.len() > vst_audio_protocol::MAX_STATE_BYTES - 8 {
            return Err(StartError("state too large".into()));
        }
        self.request(
            Message::SetState {
                blob: blob.to_vec(),
            },
            RequestKind::StateSave,
        )
    }
    pub fn open_editor(&mut self) -> Result<u32, StartError> {
        self.request(Message::OpenEditor, RequestKind::Editor)
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
        self.process.exit_code().map_err(error)
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
    pub fn restore_watch(&mut self, mut watch: ControlWatch, now_ms: u64) {
        let hello = Frame {
            flags: 0,
            request: 0,
            message: Message::hello(),
        };
        watch.received(now_ms, &hello);
        watch.received(
            now_ms,
            &Frame {
                flags: 0,
                request: 0,
                message: Message::Loaded {
                    name: String::new(),
                    vendor: String::new(),
                    latency_frames: self.latency(),
                    inputs: vec![],
                    outputs: vec![],
                },
            },
        );
        watch.activated(now_ms);
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
        let rate = options.sample_rate;
        let frames = options.quantum;
        let origin = Instant::now();
        let maximum = Geometry {
            sample_format: options.sample_format,
            lanes: 1,
            input_channels: 64,
            output_channels: 64,
            max_frames: frames,
            sample_rate: rate,
            latency_bound_frames: rate
                .checked_mul(2)
                .ok_or_else(|| StartError("invalid rate".into()))?,
        };
        let (process, transport, region, work, done, ready) = spawn(exe, maximum)?;
        let mut watch = ControlWatch::new(0);
        let hello = transport.response(0, Duration::from_secs(5))?;
        if hello.message != Message::hello() {
            return Err(StartError("expected Hello".into()));
        }
        watch.received(0, &hello);
        transport.send(
            1,
            Message::Load {
                path: plugin
                    .to_str()
                    .ok_or_else(|| StartError("plugin path must be UTF-8".into()))?
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
            return Err(StartError("expected Loaded".into()));
        };
        transport.send(2, Message::Describe)?;
        let Message::Features(capabilities) =
            transport.response(2, Duration::from_secs(10))?.message
        else {
            return Err(StartError("expected capabilities".into()));
        };
        if options.sample_format == SampleFormat::F64 && !capabilities.f64 {
            return Err(StartError("plugin does not support f64".into()));
        }
        let setup = ProcessingSetup {
            sample_rate: rate,
            quantum: frames,
            sample_format: options.sample_format,
            offline: options.offline,
            inputs: options
                .inputs
                .clone()
                .unwrap_or_else(|| inputs.into_iter().map(Into::into).collect()),
            outputs: options
                .outputs
                .clone()
                .unwrap_or_else(|| outputs.into_iter().map(Into::into).collect()),
        };
        if !setup.valid() {
            return Err(StartError("invalid processing setup".into()));
        }
        let input_wave_bits = speaker_bits(&setup.inputs);
        let output_wave_bits = speaker_bits(&setup.outputs);
        if latency_frames > maximum.latency_bound_frames {
            return Err(StartError("unsupported plugin latency".into()));
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
                .map_err(|e| error(format!("{e:?}")))?;
            let mut producer = Producer::new(view, [Some(&*work), None], &*ready);
            producer
                .announce(geometry, std::process::id())
                .map_err(|e| error(format!("{e:?}")))?;
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
                return Err(StartError("ring readiness timeout".into()));
            }
            producer
                .publisher()
                .ok_or_else(|| StartError("ring was not announced".into()))?
        };
        watch.activated(origin.elapsed().as_millis() as u64);
        let mut control = PluginControl {
            process,
            transport,
            region: region.clone(),
            work: work.clone(),
            watch,
            next_request: 6,
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
            return Err(StartError("unsupported configured plugin latency".into()));
        }
        Ok(Started {
            setup,
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
        // Frozen Scan wire message has no format field; VST 3 files/bundles use .vst3.
        if (format == PluginFormat::Vst3)
            != path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("vst3"))
        {
            return Err(StartError(
                "scan format does not match plugin extension".into(),
            ));
        }
        let geometry = Geometry {
            sample_format: vst_audio_ring::SampleFormat::F32,
            lanes: 1,
            input_channels: 2,
            output_channels: 32,
            max_frames: 480,
            sample_rate: 48000,
            latency_bound_frames: 96000,
        };
        let (process, transport, ..) = spawn(exe, geometry)?;
        if transport.response(0, Duration::from_secs(5))?.message != Message::hello() {
            return Err(StartError("expected Hello".into()));
        }
        transport.send(
            1,
            Message::Scan {
                path: path
                    .to_str()
                    .ok_or_else(|| StartError("plugin path must be UTF-8".into()))?
                    .into(),
            },
        )?;
        let reply = transport.response(1, Duration::from_secs(60))?;
        let Message::Classes { entries } = reply.message else {
            return Err(StartError("expected Classes".into()));
        };
        if !process.wait(Duration::from_secs(2)) {
            return Err(StartError("scan did not exit".into()));
        }
        Ok(entries)
    }
}
fn expect_ack(frame: Frame) -> Result<(), StartError> {
    if frame.message == Message::Ack {
        Ok(())
    } else {
        Err(StartError("expected Ack".into()))
    }
}
type Spawned = (
    ChildProcess,
    Transport,
    Arc<SharedRegion>,
    Arc<Event>,
    Arc<Event>,
    Arc<Event>,
);
fn spawn(exe: &Path, geometry: Geometry) -> Result<Spawned, StartError> {
    let region = Arc::new(
        SharedRegion::create(
            geometry
                .total_bytes()
                .map_err(|e| error(format!("{e:?}")))?,
        )
        .map_err(error)?,
    );
    let input = Pipe::create(false).map_err(error)?;
    let output = Pipe::create(true).map_err(error)?;
    let work = Arc::new(Event::create().map_err(error)?);
    let done = Arc::new(Event::create().map_err(error)?);
    let ready = Arc::new(Event::create().map_err(error)?);
    let parent = runtime::parent_handle().map_err(error)?;
    let values = [
        input.child_value(),
        output.child_value(),
        region.handle_value(),
        work.handle_value(),
        done.handle_value(),
        ready.handle_value(),
        runtime::parent_value(&parent),
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
    let process = runtime::spawn_handles(exe, &args, &values, parent).map_err(error)?;
    drop(input.child);
    drop(output.child);
    let transport = Transport::new(output.parent, input.parent);
    Ok((process, transport, region, work, done, ready))
}
