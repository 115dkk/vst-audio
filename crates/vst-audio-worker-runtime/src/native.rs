//! Executes foreign calls exclusively inside the worker.
use crate::{
    EditPhase, ParameterEdit, Plugin, PluginError, PluginEvents, PluginInfo, Processor,
    host::{Args, AudioBuffer, Phase},
    layout::{default_arrangement, vst2_arrangement},
};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use vst_audio_protocol::{
    Bus, ClassInfo, DecodedFrame, EditorStatus, Frame, MAX_EDITOR_EXTENT, Message, PluginFormat,
    read_frame, write_frame,
};
use vst_audio_ring::{Consumer, Geometry, Signal, State, Wait, Waited};
use vst_audio_transport_sys::{
    Event, SharedRegion,
    runtime::{self, Mmcss},
};
use vst_audio_worker_sys::plugin::{Vst2Plugin, Vst2Processor};
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct StartError(pub String);
fn error(error: impl std::fmt::Display) -> StartError {
    StartError(error.to_string())
}
fn plugin_error(error: impl std::fmt::Display) -> PluginError {
    PluginError(error.to_string())
}

/// The frozen Scan message has no format field: `.vst3` files and bundles are
/// VST 3 and every other path is VST 2, as `Load` accepts them. VST 2 binaries
/// keep no fixed extension (a bundle's inner executable has none), so the loader
/// rather than the extension refuses what is not a plugin (audit #11 TD-95).
/// A window spanning several monitors can outgrow the wire range, and an
/// unencodable reply would end the worker; report the largest extent instead
/// (audit #11 TD-48).
fn wire_status(mut status: EditorStatus) -> EditorStatus {
    status.width = status.width.min(MAX_EDITOR_EXTENT);
    status.height = status.height.min(MAX_EDITOR_EXTENT);
    status
}
fn scan_format(path: &Path) -> PluginFormat {
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("vst3"))
    {
        PluginFormat::Vst3
    } else {
        PluginFormat::Vst2
    }
}

fn loaded<'a>(
    plugin: &'a mut Option<Box<dyn Plugin>>,
) -> Result<&'a mut (dyn Plugin + 'static), StartError> {
    plugin
        .as_deref_mut()
        .ok_or_else(|| StartError("Load required".into()))
}

#[derive(Debug)]
struct Vst2 {
    inner: Vst2Plugin,
    processor: Option<Vst2ProcessorAdapter>,
    info: PluginInfo,
}
impl Vst2 {
    fn load(path: &Path, class: Option<[u8; 16]>) -> Result<Self, PluginError> {
        let inner = Vst2Plugin::load_class(path, class).map_err(plugin_error)?;
        let info = inner.info();
        let arrangement = |types: Option<Vec<i32>>, count| -> Result<u64, PluginError> {
            Ok(types
                .map_or_else(
                    || default_arrangement(count),
                    |types| vst2_arrangement(&types),
                )
                .unwrap_or(0))
        };
        let input_arrangement = arrangement(info.input_types, info.inputs)?;
        let output_arrangement = arrangement(info.output_types, info.outputs)?;
        let info = PluginInfo {
            name: info.name,
            vendor: info.vendor,
            latency_frames: info.latency,
            inputs: (info.inputs != 0)
                .then(|| vst_audio_protocol::BusInfo {
                    layout: Bus {
                        channels: info.inputs,
                        arrangement: input_arrangement,
                    },
                    kind: vst_audio_protocol::BusKind::Main,
                    name: String::new(),
                    default_active: true,
                })
                .into_iter()
                .collect(),
            outputs: (info.outputs != 0)
                .then(|| vst_audio_protocol::BusInfo {
                    layout: Bus {
                        channels: info.outputs,
                        arrangement: output_arrangement,
                    },
                    kind: vst_audio_protocol::BusKind::Main,
                    name: String::new(),
                    default_active: true,
                })
                .into_iter()
                .collect(),
        };
        Ok(Self {
            inner,
            processor: None,
            info,
        })
    }
}
impl Processor for Vst2ProcessorAdapter {
    fn process(&mut self, planes: &mut [&mut [f32]], frames: usize) -> bool {
        self.0.process(planes, frames)
    }
    fn process_block(
        &mut self,
        p: &mut [&mut [f32]],
        f: usize,
        c: &vst_audio_ring::Transport,
        i: &[vst_audio_ring::Event],
        o: &mut vst_audio_ring::EventBuffer,
    ) -> bool {
        self.0.process_block(p, f, c, i, o)
    }
    fn process_block_f64(
        &mut self,
        p: &mut [&mut [f64]],
        f: usize,
        c: &vst_audio_ring::Transport,
        i: &[vst_audio_ring::Event],
        o: &mut vst_audio_ring::EventBuffer,
    ) -> bool {
        self.0.process_block_f64(p, f, c, i, o)
    }
}
#[derive(Debug)]
struct Vst2ProcessorAdapter(Vst2Processor);
fn legacy_setup(
    rate: u32,
    max_frames: u32,
    in_channels: u16,
    out_channels: u16,
    info: &PluginInfo,
) -> Result<vst_audio_protocol::ProcessingSetup, StartError> {
    let buses = |declared: &[vst_audio_protocol::BusInfo], channels, direction| {
        let configured = declared
            .iter()
            .map(|bus| bus.layout.clone().into())
            .collect::<Vec<_>>();
        if vst_audio_protocol::ProcessingSetup::channels(&configured) == channels {
            Ok(configured)
        } else {
            Err(StartError(format!(
                "legacy Configure {direction} channels do not match the plugin declaration"
            )))
        }
    };
    Ok(vst_audio_protocol::ProcessingSetup {
        sample_rate: rate,
        quantum: max_frames,
        sample_format: vst_audio_ring::SampleFormat::F32,
        offline: false,
        inputs: buses(&info.inputs, in_channels, "input")?,
        outputs: buses(&info.outputs, out_channels, "output")?,
    })
}

impl Plugin for Vst2 {
    fn info(&self) -> PluginInfo {
        let mut info = self.info.clone();
        info.latency_frames = self.inner.info().latency;
        info
    }
    fn configure(
        &mut self,
        rate: u32,
        frames: u32,
        inputs: u16,
        outputs: u16,
    ) -> Result<(), PluginError> {
        self.inner
            .configure(rate, frames, inputs, outputs)
            .map_err(plugin_error)?;
        self.processor = Some(Vst2ProcessorAdapter(
            self.inner.processor().map_err(plugin_error)?,
        ));
        Ok(())
    }
    fn configure_audio(
        &mut self,
        setup: &vst_audio_protocol::ProcessingSetup,
    ) -> Result<(), PluginError> {
        if !setup.valid()
            || setup.inputs.len() > 1
            || setup.outputs.len() > 1
            || setup.inputs.iter().chain(&setup.outputs).any(|b| !b.active)
        {
            return Err(PluginError(
                "VST2 exposes one active audio port per direction".into(),
            ));
        }
        let unchanged = |requested: &[vst_audio_protocol::BusConfiguration],
                         declared: &[vst_audio_protocol::BusInfo]| {
            requested.len() == declared.len()
                && requested
                    .iter()
                    .zip(declared)
                    .all(|(a, b)| a.layout == b.layout)
        };
        // The application sends the declaration back even when no layout was selected.
        // Keeping that default is not a request to change the plugin's arrangement.
        if !unchanged(&setup.inputs, &self.info.inputs)
            || !unchanged(&setup.outputs, &self.info.outputs)
        {
            self.inner
                .negotiate_layouts(
                    setup.inputs.first().map_or(0, |b| b.layout.arrangement),
                    setup.outputs.first().map_or(0, |b| b.layout.arrangement),
                )
                .map_err(plugin_error)?;
        }
        self.inner
            .configure_precision(
                setup.sample_rate,
                setup.quantum,
                vst_audio_protocol::ProcessingSetup::channels(&setup.inputs),
                vst_audio_protocol::ProcessingSetup::channels(&setup.outputs),
                setup.sample_format,
                setup.offline,
            )
            .map_err(plugin_error)?;
        self.processor = Some(Vst2ProcessorAdapter(
            self.inner.processor().map_err(plugin_error)?,
        ));
        Ok(())
    }
    fn capabilities(&self) -> vst_audio_protocol::Capabilities {
        self.inner.capabilities()
    }
    fn activate(&mut self) -> Result<(), PluginError> {
        self.inner.activate();
        Ok(())
    }
    fn deactivate(&mut self) {
        self.inner.deactivate();
    }
    fn take_processor(&mut self) -> Result<Box<dyn Processor>, PluginError> {
        self.processor
            .take()
            .map(|processor| Box::new(processor) as Box<dyn Processor>)
            .ok_or_else(|| PluginError("processor already taken".into()))
    }
    fn parameters(
        &mut self,
        start: u32,
        count: u16,
    ) -> Result<(u32, Vec<vst_audio_protocol::Parameter>), PluginError> {
        self.inner.parameters(start, count).map_err(plugin_error)
    }
    fn set_parameter(&mut self, id: u32, value: f64) -> Result<(), PluginError> {
        self.inner.set_parameter(id, value).map_err(plugin_error)
    }
    fn get_state(&mut self) -> Result<Vec<u8>, PluginError> {
        self.inner.get_state().map_err(plugin_error)
    }
    fn set_state(&mut self, blob: &[u8]) -> Result<(), PluginError> {
        self.inner.set_state(blob).map_err(plugin_error)
    }
    fn open_editor(
        &mut self,
        scaling: vst_audio_protocol::EditorScaling,
    ) -> Result<(), PluginError> {
        self.inner.open_editor(scaling).map_err(plugin_error)
    }
    fn open_editor_in(
        &mut self,
        parent: vst_audio_protocol::ParentWindow,
        scaling: vst_audio_protocol::EditorScaling,
    ) -> Result<(), PluginError> {
        self.inner
            .open_editor_in(parent, scaling)
            .map_err(plugin_error)
    }
    fn editor_action(
        &mut self,
        action: vst_audio_protocol::EditorAction,
    ) -> Result<vst_audio_protocol::EditorReply, PluginError> {
        self.inner.editor_action(action).map_err(plugin_error)
    }
    fn close_editor(&mut self) -> Result<(), PluginError> {
        self.inner.request_editor_close().map_err(plugin_error)
    }
    fn idle(&mut self) {
        self.inner.idle();
    }
    fn take_events(&mut self) -> PluginEvents {
        self.inner.take_events()
    }
}

fn ring_validation_error(
    expected: Geometry,
    actual: Result<Geometry, vst_audio_ring::HeaderError>,
) -> StartError {
    match actual {
        Ok(actual) => StartError(format!(
            "ring validation failed: expected {expected:?}, actual {actual:?}"
        )),
        Err(error) => StartError(format!("ring validation failed: {error:?}")),
    }
}

fn finish_audio(
    result: Result<(), StartError>,
    audio: Option<thread::JoinHandle<()>>,
) -> Result<(), StartError> {
    let join = audio.map(thread::JoinHandle::join);
    match (result, join) {
        (Err(error), _) => Err(error),
        (Ok(()), Some(Err(_))) => Err(StartError("audio thread panicked".into())),
        (Ok(()), _) => Ok(()),
    }
}

#[allow(clippy::too_many_arguments)]
fn start_audio(
    geometry: Geometry,
    region: Arc<SharedRegion>,
    work: Arc<Event>,
    done: Arc<Event>,
    ready: Arc<Event>,
    stop: Arc<AtomicBool>,
    mut scratch: AudioBuffer,
    mut processor: Box<dyn Processor>,
) -> Result<thread::JoinHandle<()>, StartError> {
    let (validated, validation) = mpsc::sync_channel(1);
    let audio = thread::spawn(move || {
        let _mmcss = match Mmcss::pro_audio() {
            Ok(mmcss) => Some(mmcss),
            Err(failure) => {
                eprintln!("failed to enable Pro Audio MMCSS: {failure}");
                None
            }
        };
        let view = region.region();
        let mut consumer = Consumer::new(view, &*ready, [Some(&*done), None]);
        let result = consumer.validate_and_ready(std::process::id());
        let valid = result == Ok(geometry);
        let _ = validated.send(result);
        if !valid {
            consumer.fault(vst_audio_ring::Fault::LayoutMismatch as u32);
            return;
        }
        while !stop.load(Ordering::Acquire) {
            let wake = work.wait(Duration::from_millis(100));
            consumer.beat();
            if wake == Waited::PeerGone || view.state() != Some(State::Ready) {
                break;
            }
            scratch.drain(&consumer, processor.as_mut());
        }
        processor.stop();
    });
    let actual = validation
        .recv_timeout(Duration::from_secs(5))
        .map_err(error)?;
    if actual != Ok(geometry) {
        return Err(ring_validation_error(geometry, actual));
    }
    Ok(audio)
}

fn forward_notifications(
    output: &mut impl std::io::Write,
    events: PluginEvents,
    reported_latency: &mut Option<u32>,
) -> Result<bool, StartError> {
    let mut send = |message| {
        write_frame(
            &mut *output,
            &Frame {
                flags: 0,
                request: 0,
                message,
            },
        )
        .map_err(error)
    };
    if let Some(text) = events.fault {
        send(Message::Fault {
            code: 4,
            text: text.into(),
        })?;
        return Ok(true);
    }
    let mut pending_perform: Option<ParameterEdit> = None;
    for edit in events.parameter_edits {
        if edit.phase == EditPhase::Perform
            && pending_perform.is_some_and(|pending| pending.id == edit.id)
        {
            pending_perform = Some(edit);
            continue;
        }
        if let Some(pending) = pending_perform.take() {
            send(Message::ParameterEdit {
                id: pending.id,
                phase: pending.phase,
                value: pending.value,
            })?;
        }
        if edit.phase == EditPhase::Perform {
            pending_perform = Some(edit);
        } else {
            send(Message::ParameterEdit {
                id: edit.id,
                phase: edit.phase,
                value: edit.value,
            })?;
        }
    }
    if let Some(pending) = pending_perform {
        send(Message::ParameterEdit {
            id: pending.id,
            phase: pending.phase,
            value: pending.value,
        })?;
    }
    if events.params_changed {
        send(Message::ParamsChanged)?;
    }
    if let Some(frames) = events.latency_changed
        && Some(frames) != *reported_latency
    {
        send(Message::LatencyChanged { frames })?;
        *reported_latency = Some(frames);
    }
    // A minimized window reports 0 x 0, which is not a resize; neither that nor
    // an extent past the wire range may end the worker (audit #11 TD-48).
    if let Some((width, height)) = events.editor_resized
        && width != 0
        && height != 0
    {
        send(Message::EditorResized {
            width: width.min(MAX_EDITOR_EXTENT),
            height: height.min(MAX_EDITOR_EXTENT),
        })?;
    }
    if events.editor_closed {
        send(Message::EditorClosed)?;
    }
    Ok(false)
}

pub fn host_main(args: Args) -> Result<(), StartError> {
    runtime::suppress_error_dialogs();
    #[cfg(target_os = "linux")]
    vst_audio_worker_sys::linux_editor::configure(
        &args.linux_editor,
        !args.sandbox || args.sandbox_processes,
    );
    let trace = std::env::var_os("VST_AUDIO_TRACE").is_some();
    if trace {
        eprintln!("worker {} starting", std::process::id());
    }
    #[cfg(target_os = "macos")]
    let _activity = vst_audio_worker_sys::editor::initialize().map_err(error)?;
    if trace {
        eprintln!("worker {} editor runtime ready", std::process::id());
    }
    let input = runtime::inherited_file(args.control_in).map_err(error)?;
    let mut output = runtime::inherited_file(args.control_out).map_err(error)?;
    let parent = Arc::new(runtime::duplicate_value(args.parent, false).map_err(error)?);
    let region = Arc::new(SharedRegion::open(args.region, args.region_bytes).map_err(error)?);
    let work = Arc::new(
        Event::open(args.work)
            .map_err(error)?
            .with_peer(parent.try_clone().map_err(error)?),
    );
    let done = Arc::new(Event::open(args.done).map_err(error)?);
    let ready = Arc::new(Event::open(args.ready).map_err(error)?);
    // The worker uses only its private duplicates. A helper the plugin starts must
    // not inherit the originals either (audit #11 TD-03;
    // plugin_helper_inherits_no_worker_descriptors).
    for value in [args.control_in, args.control_out, args.parent, args.region] {
        runtime::stop_inheritance(value).map_err(error)?;
    }
    for value in [args.work, args.done, args.ready] {
        Event::stop_inheritance(value).map_err(error)?;
    }
    if args.sandbox {
        let policy = vst_audio_transport_sys::sandbox::SandboxPolicy {
            network: args.sandbox_network,
            processes: args.sandbox_processes,
            writable: args.sandbox_writable.clone(),
        };
        let report = vst_audio_transport_sys::sandbox::apply_worker(&policy, &std::env::temp_dir());
        let message = match report {
            Ok(report) => Message::Sandboxed {
                mechanism: report.mechanism,
            },
            Err(reason) => {
                write_frame(
                    &mut output,
                    &Frame {
                        flags: 0,
                        request: 0,
                        message: Message::Fault {
                            code: 1,
                            text: format!("sandbox setup failed: {reason}"),
                        },
                    },
                )
                .map_err(error)?;
                return Err(error(reason));
            }
        };
        write_frame(
            &mut output,
            &Frame {
                flags: 0,
                request: 0,
                message,
            },
        )
        .map_err(error)?;
    }
    // Main-thread writes are acceptable in the disposable host. The app's reader
    // drains continuously; app-side writes use a dedicated thread.
    write_frame(
        &mut output,
        &Frame {
            flags: 0,
            request: 0,
            message: Message::hello(),
        },
    )
    .map_err(error)?;
    if trace {
        eprintln!("worker {} sent hello", std::process::id());
    }
    let (tx, rx) = mpsc::sync_channel(8);
    thread::spawn(move || {
        let mut input = input;
        loop {
            let next = read_frame(&mut input);
            let end = !matches!(next, Ok(Some(_)));
            if tx.send(next).is_err() || end {
                break;
            }
        }
    });
    // Independent of the plugin's main/audio threads: even a hung foreign
    // call must not leave a worker behind after the application exits.
    let parent_watch = parent.clone();
    thread::spawn(move || {
        loop {
            if runtime::process_gone(&parent_watch) {
                std::process::exit(0);
            }
            thread::sleep(Duration::from_millis(50));
        }
    });
    let stop = Arc::new(AtomicBool::new(false));
    let mut audio = None;
    let mut plugin: Option<Box<dyn Plugin>> = None;
    let mut phase: Option<Phase> = None;
    let mut failed = false;
    let mut reported_latency: Option<u32> = None;
    // Always join audio before lifecycle teardown, including malformed frames
    // and broken control pipes (early ? returns must not detach the audio thread).
    let result = (|| -> Result<(), StartError> {
        loop {
            if runtime::process_gone(&parent) {
                break;
            }
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(Ok(Some(DecodedFrame::Known(frame)))) => {
                    if trace {
                        eprintln!(
                            "worker request {} {:?}",
                            frame.request,
                            std::mem::discriminant(&frame.message)
                        );
                    }
                    let request = frame.request;
                    let kind = frame.message.kind();
                    let shutdown = matches!(frame.message, Message::Shutdown);
                    // GetState/SetState failures are fatal because continuing cannot prove that
                    // the recovery checkpoint and the plugin's actual state agree (audit #11 TD-51).
                    let recoverable = matches!(
                        &frame.message,
                        Message::Editor(_)
                            | Message::ListParameters { .. }
                            | Message::SetParameter { .. }
                            | Message::OpenEditor { .. }
                            | Message::OpenEditorIn { .. }
                            | Message::CloseEditor
                    );
                    let message = frame.message;
                    let reply: Result<Message, StartError> = (|| {
                        let message = match message {
                            Message::Configure {
                                rate,
                                max_frames,
                                in_channels,
                                out_channels,
                            } => {
                                let info = loaded(&mut plugin)?.info();
                                Message::Prepare(legacy_setup(
                                    rate,
                                    max_frames,
                                    in_channels,
                                    out_channels,
                                    &info,
                                )?)
                            }
                            other => other,
                        };
                        let next_phase = phase
                            .map(|current| current.accept(&message))
                            .transpose()
                            .map_err(error)?;
                        let reply = match message {
                            Message::Scan { path } => match scan_format(Path::new(&path)) {
                                PluginFormat::Vst3 => {
                                    let module = vst_audio_worker_sys::vst3::Vst3Module::load(
                                        Path::new(&path),
                                    )
                                    .map_err(error)?;
                                    Ok(Message::Classes {
                                        entries: module
                                            .scan()
                                            .map_err(error)?
                                            .into_iter()
                                            .map(|class| ClassInfo {
                                                id: class.id,
                                                name: class.name,
                                                vendor: class.vendor,
                                                category: class.category,
                                            })
                                            .collect(),
                                    })
                                }
                                PluginFormat::Vst2 => {
                                    let scanned =
                                        Vst2Plugin::load(Path::new(&path)).map_err(error)?;
                                    Ok(Message::Classes {
                                        entries: scanned.classes().map_err(error)?,
                                    })
                                }
                            },
                            Message::Load {
                                path,
                                format,
                                class_id,
                            } if plugin.is_none() => {
                                let loaded: Box<dyn Plugin> = match format {
                                    PluginFormat::Vst2 => Box::new(
                                        Vst2::load(Path::new(&path), class_id).map_err(error)?,
                                    ),
                                    PluginFormat::Vst3 => Box::new(
                                        crate::vst3::Vst3::load(Path::new(&path), class_id)
                                            .map_err(error)?,
                                    ),
                                };
                                let info = loaded.info();
                                reported_latency = Some(info.latency_frames);
                                plugin = Some(loaded);
                                phase = Some(Phase::Loaded);
                                Ok(Message::Loaded {
                                    name: info.name,
                                    vendor: info.vendor,
                                    latency_frames: info.latency_frames,
                                    inputs: info.inputs,
                                    outputs: info.outputs,
                                })
                            }
                            Message::Describe => {
                                Ok(Message::Features(loaded(&mut plugin)?.capabilities()))
                            }
                            Message::Prepare(setup) => {
                                if !setup.valid() {
                                    return Err(StartError("invalid audio configuration".into()));
                                }
                                let rate = setup.sample_rate;
                                let max_frames = setup.quantum;
                                let in_channels =
                                    vst_audio_protocol::ProcessingSetup::channels(&setup.inputs);
                                let out_channels =
                                    vst_audio_protocol::ProcessingSetup::channels(&setup.outputs);
                                let plugin = loaded(&mut plugin)?;
                                plugin.configure_audio(&setup).map_err(error)?;
                                let latency = plugin.info().latency_frames;
                                if Some(latency) != reported_latency {
                                    write_frame(
                                        &mut output,
                                        &Frame {
                                            flags: 0,
                                            request: 0,
                                            message: Message::LatencyChanged { frames: latency },
                                        },
                                    )
                                    .map_err(error)?;
                                    reported_latency = Some(latency);
                                }
                                let geometry = Geometry {
                                    sample_format: setup.sample_format,
                                    lanes: 1,
                                    slots: args.slots,
                                    input_channels: in_channels.into(),
                                    output_channels: out_channels.into(),
                                    max_frames,
                                    sample_rate: rate,
                                    latency_bound_frames: region.latency_bound(),
                                };
                                region
                                    .region_for(geometry)
                                    .map_err(|e| error(format!("{e:?}")))?;
                                let scratch = AudioBuffer::new(geometry)
                                    .map_err(|e| error(format!("{e:?}")))?;
                                let processor = plugin.take_processor().map_err(error)?;
                                audio = Some(start_audio(
                                    geometry,
                                    region.clone(),
                                    work.clone(),
                                    done.clone(),
                                    ready.clone(),
                                    stop.clone(),
                                    scratch,
                                    processor,
                                )?);
                                Ok(Message::Ack)
                            }
                            Message::Activate => {
                                plugin
                                    .as_mut()
                                    .expect("configured plugin")
                                    .activate()
                                    .map_err(error)?;
                                Ok(Message::Ack)
                            }
                            Message::SetState { blob } => {
                                loaded(&mut plugin)?.set_state(&blob).map_err(error)?;
                                Ok(Message::Ack)
                            }
                            Message::GetState => Ok(Message::State {
                                blob: loaded(&mut plugin)?.get_state().map_err(error)?,
                            }),
                            Message::Editor(action) => {
                                match loaded(&mut plugin)?.editor_action(action).map_err(error)? {
                                    vst_audio_protocol::EditorReply::Status(s) => {
                                        Ok(Message::EditorStatus(wire_status(s)))
                                    }
                                    vst_audio_protocol::EditorReply::Image(i) => {
                                        Ok(Message::EditorImage(i))
                                    }
                                }
                            }
                            Message::OpenEditor { scaling } => {
                                loaded(&mut plugin)?.open_editor(scaling).map_err(error)?;
                                Ok(Message::Ack)
                            }
                            Message::OpenEditorIn { parent, scaling } => {
                                let plugin = loaded(&mut plugin)?;
                                plugin.open_editor_in(parent, scaling).map_err(|failure| {
                                    // Windows may refuse a Low-integrity child of a higher
                                    // parent; the native error stays in diagnostics.
                                    #[cfg(windows)]
                                    if args.sandbox
                                        && failure.0.starts_with("embedded window creation failed:")
                                    {
                                        eprintln!("sandboxed editor: {failure}");
                                        return StartError(
                                        "parent window is not accessible from the sandboxed worker"
                                            .into(),
                                    );
                                    }
                                    error(failure)
                                })?;
                                Ok(Message::Ack)
                            }
                            Message::CloseEditor => {
                                loaded(&mut plugin)?.close_editor().map_err(error)?;
                                Ok(Message::Ack)
                            }
                            Message::ListParameters { start, count } => {
                                let (total, entries) = loaded(&mut plugin)?
                                    .parameters(start, count)
                                    .map_err(error)?;
                                Ok(Message::Parameters { total, entries })
                            }
                            Message::SetParameter { id, value } => {
                                loaded(&mut plugin)?
                                    .set_parameter(id, value)
                                    .map_err(error)?;
                                Ok(Message::Ack)
                            }
                            Message::Shutdown => Ok(Message::Ack),
                            _ => Err(StartError("invalid host request".into())),
                        }?;
                        if let Some(next_phase) = next_phase {
                            phase = Some(next_phase);
                        }
                        Ok(reply)
                    })();
                    let scan = matches!(&reply, Ok(Message::Classes { .. }));
                    let fault = reply.is_err() && !recoverable;
                    let message = reply.unwrap_or_else(|error| {
                        if recoverable {
                            Message::RequestError {
                                code: 1,
                                text: error.to_string(),
                            }
                        } else {
                            Message::Fault {
                                code: 1,
                                text: format!("{error} (request kind {kind})"),
                            }
                        }
                    });
                    write_frame(
                        &mut output,
                        &Frame {
                            flags: 0,
                            request,
                            message,
                        },
                    )
                    .map_err(error)?;
                    if shutdown || scan || fault {
                        failed = fault;
                        break;
                    }
                }
                Ok(Ok(Some(DecodedFrame::Unknown(_)))) => {}
                Ok(Ok(None)) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(Err(error_value)) => {
                    let _ = write_frame(
                        &mut output,
                        &Frame {
                            flags: 0,
                            request: 0,
                            message: Message::Fault {
                                code: 3,
                                text: error_value.to_string(),
                            },
                        },
                    );
                    return Err(error(error_value));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if let Some(plugin) = plugin.as_mut() {
                plugin.idle();
                if forward_notifications(&mut output, plugin.take_events(), &mut reported_latency)?
                {
                    failed = true;
                    break;
                }
            }
        }
        if failed {
            Err(StartError("host request failed".into()))
        } else {
            Ok(())
        }
    })();
    stop.store(true, Ordering::Release);
    work.signal();
    let result = finish_audio(result, audio);
    if let Some(plugin) = plugin.as_mut() {
        plugin.deactivate();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use vst_audio_protocol::{BusInfo, BusKind};

    #[test]
    fn ring_validation_error_names_expected_and_actual_geometry() {
        let expected = Geometry {
            sample_format: vst_audio_ring::SampleFormat::F32,
            lanes: 1,
            slots: 2,
            input_channels: 0,
            output_channels: 2,
            max_frames: 128,
            sample_rate: 48000,
            latency_bound_frames: 96000,
        };
        let actual = Geometry {
            output_channels: 1,
            ..expected
        };

        let message = ring_validation_error(expected, Ok(actual)).to_string();

        assert!(message.contains("expected"));
        assert!(message.contains("output_channels: 2"));
        assert!(message.contains("actual"));
        assert!(message.contains("output_channels: 1"));
    }

    #[test]
    fn finish_reports_audio_thread_panic_without_hiding_an_earlier_error() {
        let panic = thread::spawn(|| panic!("audio panic"));
        assert_eq!(
            finish_audio(Ok(()), Some(panic)).unwrap_err().to_string(),
            "audio thread panicked"
        );

        let panic = thread::spawn(|| panic!("audio panic"));
        assert_eq!(
            finish_audio(Err(StartError("earlier failure".into())), Some(panic))
                .unwrap_err()
                .to_string(),
            "earlier failure"
        );
    }

    #[test]
    fn scan_treats_every_path_but_vst3_as_vst2() {
        assert_eq!(scan_format(Path::new("plugin.VST3")), PluginFormat::Vst3);
        for path in [
            "plugin.dll",
            "plugin.so",
            "plugin.vst",
            "plugin.unknown",
            "Plugin",
        ] {
            assert_eq!(scan_format(Path::new(path)), PluginFormat::Vst2, "{path}");
        }
    }

    #[test]
    fn oversized_editor_status_still_encodes() {
        use vst_audio_protocol::{EditorPlatform, EditorScaling, EditorStatus, encode_frame};
        let status = wire_status(EditorStatus {
            platform: EditorPlatform::X11,
            scaling: EditorScaling::Off,
            visible: true,
            embedded: false,
            width: 11_520,
            height: 0,
            scale: 1.0,
            resize_supported: true,
            scale_supported: false,
        });
        assert_eq!((status.width, status.height), (8192, 0));
        encode_frame(&Frame {
            flags: 0,
            request: 1,
            message: Message::EditorStatus(status),
        })
        .unwrap();
    }

    #[test]
    fn minimized_or_oversized_editor_resize_keeps_the_worker() {
        let mut output = Vec::new();
        let mut latency = None;
        let minimized = PluginEvents {
            editor_resized: Some((0, 0)),
            ..Default::default()
        };
        assert!(!forward_notifications(&mut output, minimized, &mut latency).unwrap());
        assert!(output.is_empty());
        let spanning = PluginEvents {
            editor_resized: Some((11_520, 600)),
            ..Default::default()
        };
        assert!(!forward_notifications(&mut output, spanning, &mut latency).unwrap());
        let Some(DecodedFrame::Known(frame)) = read_frame(&mut output.as_slice()).unwrap() else {
            panic!("expected one known frame");
        };
        assert_eq!(
            frame.message,
            Message::EditorResized {
                width: 8192,
                height: 600
            }
        );
    }

    #[test]
    fn legacy_configure_preserves_declared_instrument_buses() {
        let info = PluginInfo {
            name: "Instrument".into(),
            vendor: "Tests".into(),
            latency_frames: 0,
            inputs: vec![],
            outputs: vec![BusInfo {
                layout: Bus {
                    channels: 2,
                    arrangement: 3,
                },
                kind: BusKind::Main,
                name: "Main".into(),
                default_active: true,
            }],
        };

        let setup = legacy_setup(48000, 128, 0, 2, &info).unwrap();

        assert!(setup.inputs.is_empty());
        assert_eq!(setup.outputs.len(), 1);
        assert_eq!(setup.outputs[0].layout.arrangement, 3);
        assert_eq!(
            vst_audio_protocol::ProcessingSetup::channels(&setup.outputs),
            2
        );
    }
}
