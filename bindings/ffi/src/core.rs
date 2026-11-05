use std::{
    cell::Cell,
    collections::VecDeque,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use vst_audio::{
    BlockEndpoint, ClassInfo, Command, Configuration, Host, HostEvent, Instance, Parameter,
    PluginDescriptor, PluginFormat, RenderError, RequestId, SampleFormat, SandboxPolicy,
    StartError, StartErrorKind,
};

#[derive(Debug)]
pub(crate) enum Error {
    InvalidArgument(String),
    Start(StartError),
    Render(RenderError),
    RealtimeOwned,
    RequestTimeout,
    Failed(String),
}

impl Error {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidArgument(message.into())
    }

    pub(crate) fn message(&self) -> String {
        match self {
            Self::InvalidArgument(message) | Self::Failed(message) => message.clone(),
            Self::Start(error) => error.to_string(),
            Self::Render(error) => error.to_string(),
            Self::RealtimeOwned => "the realtime block endpoint has been taken".into(),
            Self::RequestTimeout => "timed out waiting for a control reply".into(),
        }
    }

    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::InvalidArgument(_) => "invalid_argument",
            Self::Start(error) => start_error_kind(error.kind()),
            Self::Render(error) => render_error_kind(*error),
            Self::RealtimeOwned => "realtime_owned",
            Self::RequestTimeout => "request_timeout",
            Self::Failed(_) => "failed",
        }
    }
}

impl From<StartError> for Error {
    fn from(value: StartError) -> Self {
        Self::Start(value)
    }
}

impl From<RenderError> for Error {
    fn from(value: RenderError) -> Self {
        Self::Render(value)
    }
}

fn start_error_kind(kind: StartErrorKind) -> &'static str {
    match kind {
        StartErrorKind::Cancelled => "cancelled",
        StartErrorKind::InvalidInput => "invalid_input",
        StartErrorKind::NotRunning => "not_running",
        StartErrorKind::QueueFull => "queue_full",
        StartErrorKind::Disconnected => "disconnected",
        StartErrorKind::Io => "io",
        StartErrorKind::Protocol => "protocol",
        StartErrorKind::Start => "start",
        _ => "unknown",
    }
}

fn render_error_kind(error: RenderError) -> &'static str {
    match error {
        RenderError::InvalidBuffer => "invalid_buffer",
        RenderError::NotRunning => "not_running",
        RenderError::Recovered => "recovered",
        RenderError::TimedOut => "timed_out",
        RenderError::Lost => "lost",
    }
}

pub(crate) fn plugin_format(path: &Path, format: i32) -> Result<PluginFormat, Error> {
    match format {
        1 => Ok(PluginFormat::Vst2),
        2 => Ok(PluginFormat::Vst3),
        0 => match path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("vst3") => Ok(PluginFormat::Vst3),
            Some("dll" | "so" | "vst") => Ok(PluginFormat::Vst2),
            _ => Err(Error::invalid(
                "format must be 1 (VST2) or 2 (VST3); cannot infer it from this path",
            )),
        },
        _ => Err(Error::invalid("invalid plugin format")),
    }
}

pub(crate) fn sample_format(value: i32) -> Result<SampleFormat, Error> {
    match value {
        0 => Ok(SampleFormat::F32),
        1 => Ok(SampleFormat::F64),
        _ => Err(Error::invalid("invalid sample format")),
    }
}

#[derive(Debug)]
pub(crate) struct HostHandle {
    host: Host,
}

impl HostHandle {
    pub(crate) fn new(worker: PathBuf) -> Self {
        Self {
            host: Host::new(worker),
        }
    }

    pub(crate) fn scan(
        &self,
        path: PathBuf,
        format: i32,
        sandboxed: bool,
    ) -> Result<Vec<ClassInfo>, Error> {
        let descriptor = PluginDescriptor::new(path.clone(), plugin_format(&path, format)?);
        if sandboxed {
            self.host
                .scan_sandboxed(&descriptor, &SandboxPolicy::default())
        } else {
            self.host.scan(&descriptor)
        }
        .map_err(Into::into)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn load(
        &self,
        path: PathBuf,
        format: i32,
        class_id: Option<[u8; 16]>,
        sample_rate: u32,
        block_size: u32,
        sample_format_value: i32,
        timeout: Duration,
        realtime: bool,
    ) -> Result<Plugin, Error> {
        let format = plugin_format(&path, format)?;
        let sample_format = sample_format(sample_format_value)?;
        let descriptor = PluginDescriptor {
            path,
            format,
            class_id,
        };
        let configuration = Configuration {
            sample_rate,
            quantum: block_size,
            sample_format,
            offline: !realtime,
            ..Configuration::default()
        };
        let task = self.host.spawn_async(descriptor, configuration, None);
        let started = Instant::now();
        loop {
            if let Some(mut instance) = task.try_complete()? {
                let blocks = instance
                    .take_blocks()
                    .ok_or_else(|| Error::Failed("new instance has no block endpoint".into()))?;
                let geometry = blocks.geometry();
                let latency = blocks.plugin_latency_frames();
                return Ok(Plugin {
                    instance,
                    blocks: Some(blocks),
                    pending: VecDeque::new(),
                    input_channels: geometry.input_channels,
                    output_channels: geometry.output_channels,
                    sample_rate,
                    block_size,
                    sample_format,
                    latency,
                    _not_sync: std::marker::PhantomData,
                });
            }
            if started.elapsed() >= timeout {
                task.cancel();
                return Err(Error::Start(StartError::new(
                    StartErrorKind::Cancelled,
                    "plugin startup timed out",
                )));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PluginInfo {
    pub(crate) input_channels: u32,
    pub(crate) output_channels: u32,
    pub(crate) sample_rate: u32,
    pub(crate) block_size: u32,
    pub(crate) sample_format: SampleFormat,
    pub(crate) latency: u32,
}

#[derive(Debug)]
pub(crate) struct Plugin {
    instance: Instance,
    blocks: Option<BlockEndpoint>,
    pending: VecDeque<HostEvent>,
    input_channels: u32,
    output_channels: u32,
    sample_rate: u32,
    block_size: u32,
    sample_format: SampleFormat,
    latency: u32,
    _not_sync: std::marker::PhantomData<Cell<()>>,
}

const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<Plugin>();
};

impl Plugin {
    pub(crate) fn info(&self) -> PluginInfo {
        PluginInfo {
            input_channels: self.input_channels,
            output_channels: self.output_channels,
            sample_rate: self.sample_rate,
            block_size: self.block_size,
            sample_format: self.sample_format,
            latency: self
                .blocks
                .as_ref()
                .map_or(self.latency, BlockEndpoint::plugin_latency_frames),
        }
    }

    pub(crate) fn take_blocks(&mut self) -> Result<BlockEndpoint, Error> {
        self.blocks
            .take()
            .ok_or_else(|| Error::Failed("the block endpoint has already been taken".into()))
    }

    pub(crate) fn process_f32(
        &mut self,
        input: &[f32],
        frames: usize,
        tail: usize,
        output: &mut [f32],
        timeout: Duration,
    ) -> Result<(), Error> {
        self.blocks
            .as_mut()
            .ok_or(Error::RealtimeOwned)?
            .render(input, frames, tail, output, timeout)
            .map_err(Into::into)
    }

    pub(crate) fn process_f64(
        &mut self,
        input: &[f64],
        frames: usize,
        tail: usize,
        output: &mut [f64],
        timeout: Duration,
    ) -> Result<(), Error> {
        self.blocks
            .as_mut()
            .ok_or(Error::RealtimeOwned)?
            .render_f64(input, frames, tail, output, timeout)
            .map_err(Into::into)
    }

    pub(crate) fn parameters(&mut self, timeout: Duration) -> Result<Vec<Parameter>, Error> {
        let started = Instant::now();
        let mut result = Vec::new();
        let mut start = 0u32;
        loop {
            let event = self.request(
                Command::ListParameters { start, count: 32 },
                started,
                timeout,
            )?;
            let HostEvent::Parameters { total, entries, .. } = event else {
                return Err(Error::Failed("unexpected parameter reply".into()));
            };
            let count = u32::try_from(entries.len())
                .map_err(|_| Error::Failed("invalid parameter page".into()))?;
            start = start
                .checked_add(count)
                .ok_or_else(|| Error::Failed("invalid parameter total".into()))?;
            result.extend(entries);
            if start >= total {
                return Ok(result);
            }
            if count == 0 {
                return Err(Error::Failed("empty parameter page before total".into()));
            }
        }
    }

    pub(crate) fn set_parameter(
        &mut self,
        id: u32,
        value: f64,
        timeout: Duration,
    ) -> Result<(), Error> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(Error::invalid("value must be finite and between 0 and 1"));
        }
        self.request(Command::SetParameter { id, value }, Instant::now(), timeout)?;
        Ok(())
    }

    pub(crate) fn save_state(&mut self, timeout: Duration) -> Result<Vec<u8>, Error> {
        match self.request(Command::SaveState, Instant::now(), timeout)? {
            HostEvent::State { bytes, .. } => Ok(bytes),
            _ => Err(Error::Failed("unexpected state reply".into())),
        }
    }

    pub(crate) fn restore_state(&mut self, state: &[u8], timeout: Duration) -> Result<(), Error> {
        self.request(
            Command::RestoreState(state.to_vec()),
            Instant::now(),
            timeout,
        )?;
        Ok(())
    }

    pub(crate) fn open_editor(&mut self, timeout: Duration) -> Result<(), Error> {
        self.request(Command::OpenEditor, Instant::now(), timeout)?;
        Ok(())
    }

    pub(crate) fn close_editor(&mut self, timeout: Duration) -> Result<(), Error> {
        self.request(Command::CloseEditor, Instant::now(), timeout)?;
        Ok(())
    }

    fn request(
        &mut self,
        command: Command,
        started: Instant,
        timeout: Duration,
    ) -> Result<HostEvent, Error> {
        let id = self.instance.controller().request(command)?;
        loop {
            let queued = self
                .pending
                .iter()
                .position(|event| request_id(event) == Some(id));
            let event = queued
                .and_then(|index| self.pending.remove(index))
                .or_else(|| self.instance.try_next_event());
            if let Some(event) = event {
                if request_id(&event) == Some(id) {
                    return match event {
                        HostEvent::RequestFailed { reason, .. } => Err(Error::Failed(reason)),
                        event => Ok(event),
                    };
                }
                self.pending.push_back(event);
            }
            if started.elapsed() >= timeout {
                return Err(Error::RequestTimeout);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

fn request_id(event: &HostEvent) -> Option<RequestId> {
    match event {
        HostEvent::Completed { request }
        | HostEvent::Parameters { request, .. }
        | HostEvent::EditorStatus { request, .. }
        | HostEvent::EditorImage { request, .. }
        | HostEvent::RequestFailed { request, .. } => Some(*request),
        HostEvent::State { request, .. } => *request,
        _ => None,
    }
}
