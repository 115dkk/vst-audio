#![forbid(unsafe_code)]
//! Offline Python host. Foreign code only runs in the worker process.
use numpy::{IntoPyArray, PyReadonlyArray2, ndarray::Array2};
use pyo3::{
    create_exception,
    exceptions::{PyException, PyTypeError, PyValueError},
    prelude::*,
    types::PyBytes,
};
use std::{
    collections::VecDeque,
    path::PathBuf,
    time::{Duration, Instant},
};
use vst_audio::{
    BlockEndpoint, Command, Configuration, HostEvent, Instance, PluginDescriptor, PluginFormat,
    RequestId, SampleFormat,
};

create_exception!(vst_audio, VstAudioError, PyException);
create_exception!(vst_audio, WorkerNotFoundError, VstAudioError);
create_exception!(vst_audio, StartError, VstAudioError);
create_exception!(vst_audio, RenderError, VstAudioError);
create_exception!(vst_audio, RequestTimeout, VstAudioError);

fn start_error(error: vst_audio::StartError) -> PyErr {
    let kind = match error.kind() {
        vst_audio::StartErrorKind::Cancelled => "cancelled",
        vst_audio::StartErrorKind::InvalidInput => "invalid_input",
        vst_audio::StartErrorKind::NotRunning => "not_running",
        vst_audio::StartErrorKind::QueueFull => "queue_full",
        vst_audio::StartErrorKind::Disconnected => "disconnected",
        vst_audio::StartErrorKind::Io => "io",
        vst_audio::StartErrorKind::Protocol => "protocol",
        vst_audio::StartErrorKind::Start => "start",
        _ => "unknown",
    };
    let result = StartError::new_err(error.to_string());
    Python::attach(|py| {
        result
            .value(py)
            .setattr("kind", kind)
            .expect("exception attribute")
    });
    result
}

fn render_error(error: vst_audio::RenderError) -> PyErr {
    let kind = match error {
        vst_audio::RenderError::InvalidBuffer => "invalid_buffer",
        vst_audio::RenderError::NotRunning => "not_running",
        vst_audio::RenderError::Recovered => "recovered",
        vst_audio::RenderError::TimedOut => "timed_out",
        vst_audio::RenderError::Lost => "lost",
    };
    let result = RenderError::new_err(error.to_string());
    Python::attach(|py| {
        result
            .value(py)
            .setattr("kind", kind)
            .expect("exception attribute")
    });
    result
}

fn duration(seconds: f64) -> PyResult<Duration> {
    Duration::try_from_secs_f64(seconds)
        .map_err(|_| PyValueError::new_err("timeout must be finite, nonnegative and representable"))
}

fn descriptor(path: PathBuf, format: &str, class_id: Option<&[u8]>) -> PyResult<PluginDescriptor> {
    let format = match format {
        "vst2" => PluginFormat::Vst2,
        "vst3" => PluginFormat::Vst3,
        _ => return Err(PyValueError::new_err("format must be 'vst2' or 'vst3'")),
    };
    let class_id = class_id
        .map(|id| {
            id.try_into()
                .map_err(|_| PyValueError::new_err("class_id must contain exactly 16 bytes"))
        })
        .transpose()?;
    Ok(PluginDescriptor {
        path,
        format,
        class_id,
    })
}

/// A class reported by an isolated scan.
#[pyclass(frozen, module = "vst_audio")]
struct PluginClass {
    #[pyo3(get)]
    name: String,
    #[pyo3(get)]
    vendor: String,
    #[pyo3(get)]
    category: String,
    class_id: [u8; 16],
}
#[pymethods]
impl PluginClass {
    #[getter]
    fn id<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.class_id)
    }
}
impl From<vst_audio::ClassInfo> for PluginClass {
    fn from(value: vst_audio::ClassInfo) -> Self {
        Self {
            class_id: value.id,
            name: value.name,
            vendor: value.vendor,
            category: value.category,
        }
    }
}

/// A snapshot of one plugin parameter.
#[pyclass(frozen, get_all, module = "vst_audio")]
struct Parameter {
    id: u32,
    name: String,
    unit: String,
    value: f64,
    default: Option<f64>,
    steps: Option<u32>,
    automatable: bool,
    read_only: bool,
    hidden: bool,
}
impl From<vst_audio::Parameter> for Parameter {
    fn from(p: vst_audio::Parameter) -> Self {
        Self {
            id: p.id,
            name: p.name,
            unit: p.unit,
            value: p.value,
            default: p.default,
            steps: p.steps,
            automatable: p.automatable,
            read_only: p.read_only,
            hidden: p.hidden,
        }
    }
}

#[pyclass(module = "vst_audio._native")]
struct Host {
    host: vst_audio::Host,
}
#[pymethods]
impl Host {
    #[new]
    fn new(worker: PathBuf) -> Self {
        Self {
            host: vst_audio::Host::new(worker),
        }
    }

    fn scan(
        &self,
        py: Python<'_>,
        path: PathBuf,
        format: &str,
        sandboxed: bool,
    ) -> PyResult<Vec<PluginClass>> {
        let plugin = descriptor(path, format, None)?;
        py.detach(|| {
            if sandboxed {
                self.host
                    .scan_sandboxed(&plugin, &vst_audio::SandboxPolicy::default())
            } else {
                self.host.scan(&plugin)
            }
        })
        .map(|classes| classes.into_iter().map(Into::into).collect())
        .map_err(start_error)
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (path, format, class_id, sample_rate, block_size, sample_format, timeout))]
    fn load(
        &self,
        py: Python<'_>,
        path: PathBuf,
        format: &str,
        class_id: Option<&[u8]>,
        sample_rate: u32,
        block_size: u32,
        sample_format: &str,
        timeout: f64,
    ) -> PyResult<Plugin> {
        let plugin = descriptor(path, format, class_id)?;
        let timeout = duration(timeout)?;
        let sample_format = match sample_format {
            "f32" => SampleFormat::F32,
            "f64" => SampleFormat::F64,
            _ => {
                return Err(PyValueError::new_err(
                    "sample_format must be 'f32' or 'f64'",
                ));
            }
        };
        let config = Configuration {
            sample_rate,
            quantum: block_size,
            sample_format,
            offline: true,
            ..Configuration::default()
        };
        let session = py.detach(|| {
            let task = self.host.spawn_async(plugin, config, None);
            let started = Instant::now();
            loop {
                if let Some(mut instance) = task.try_complete().map_err(start_error)? {
                    let blocks = instance.take_blocks().expect("new instance has blocks");
                    return Ok(Session {
                        instance,
                        blocks,
                        pending: VecDeque::new(),
                    });
                }
                if started.elapsed() >= timeout {
                    task.cancel();
                    return Err(start_error(vst_audio::StartError::new(
                        vst_audio::StartErrorKind::Cancelled,
                        "plugin startup timed out",
                    )));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        })?;
        let geometry = session.blocks.geometry();
        Ok(Plugin {
            input_channels: geometry.input_channels,
            output_channels: geometry.output_channels,
            sample_rate,
            block_size,
            sample_format,
            session: Some(session),
        })
    }
}

struct Session {
    instance: Instance,
    blocks: BlockEndpoint,
    pending: VecDeque<HostEvent>,
}
impl Session {
    fn request(
        &mut self,
        command: Command,
        started: Instant,
        timeout: Duration,
    ) -> PyResult<HostEvent> {
        let id = self
            .instance
            .controller()
            .request(command)
            .map_err(start_error)?;
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
                        HostEvent::RequestFailed { reason, .. } => {
                            Err(VstAudioError::new_err(reason))
                        }
                        event => Ok(event),
                    };
                }
                self.pending.push_back(event);
            }
            if started.elapsed() >= timeout {
                return Err(RequestTimeout::new_err(
                    "timed out waiting for a control reply",
                ));
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

/// One isolated plugin. Use as a context manager, or close explicitly.
#[pyclass(unsendable, module = "vst_audio")]
struct Plugin {
    #[pyo3(get)]
    input_channels: u32,
    #[pyo3(get)]
    output_channels: u32,
    #[pyo3(get)]
    sample_rate: u32,
    #[pyo3(get)]
    block_size: u32,
    sample_format: SampleFormat,
    session: Option<Session>,
}
impl Plugin {
    fn session(&mut self) -> PyResult<&mut Session> {
        self.session
            .as_mut()
            .ok_or_else(|| render_error(vst_audio::RenderError::NotRunning))
    }
    fn control(&mut self, py: Python<'_>, command: Command, timeout: f64) -> PyResult<HostEvent> {
        let timeout = duration(timeout)?;
        let session = self.session()?;
        py.detach(move || session.request(command, Instant::now(), timeout))
    }
}
#[pymethods]
impl Plugin {
    #[getter]
    fn latency(&mut self) -> PyResult<u32> {
        Ok(self.session()?.blocks.plugin_latency_frames())
    }

    /// Render planar audio and optional silent tail, compensating reported latency.
    #[pyo3(signature = (audio, *, tail = 0, timeout = 10.0))]
    fn process<'py>(
        &mut self,
        py: Python<'py>,
        audio: &Bound<'py, PyAny>,
        tail: usize,
        timeout: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let timeout = duration(timeout)?;
        // Copy while holding the GIL. Python-owned memory must not be accessed
        // during detach: another Python thread could mutate it while rendering.
        match self.sample_format {
            SampleFormat::F32 => {
                let input = input_array::<f32>(audio, self.input_channels as usize, "float32")?;
                let frames = input.ncols();
                let channels = self.output_channels as usize;
                let (length, mut output) = output_buffer::<f32>(channels, frames, tail)?;
                let session = self.session()?;
                let output = py.detach(move || {
                    session
                        .blocks
                        .render(
                            input.as_slice().expect("standard layout"),
                            frames,
                            tail,
                            &mut output,
                            timeout,
                        )
                        .map_err(render_error)?;
                    Ok::<_, PyErr>(output)
                })?;
                let output = Array2::from_shape_vec((channels, length), output)
                    .expect("validated output shape");
                Ok(output.into_pyarray(py).into_any())
            }
            SampleFormat::F64 => {
                let input = input_array::<f64>(audio, self.input_channels as usize, "float64")?;
                let frames = input.ncols();
                let channels = self.output_channels as usize;
                let (length, mut output) = output_buffer::<f64>(channels, frames, tail)?;
                let session = self.session()?;
                let output = py.detach(move || {
                    session
                        .blocks
                        .render_f64(
                            input.as_slice().expect("standard layout"),
                            frames,
                            tail,
                            &mut output,
                            timeout,
                        )
                        .map_err(render_error)?;
                    Ok::<_, PyErr>(output)
                })?;
                let output = Array2::from_shape_vec((channels, length), output)
                    .expect("validated output shape");
                Ok(output.into_pyarray(py).into_any())
            }
        }
    }

    /// Return all parameter pages as immutable snapshots.
    #[pyo3(signature = (*, timeout = 10.0))]
    fn parameters(&mut self, py: Python<'_>, timeout: f64) -> PyResult<Vec<Parameter>> {
        let timeout = duration(timeout)?;
        let session = self.session()?;
        py.detach(move || {
            let started = Instant::now();
            let mut result = Vec::new();
            let mut start = 0;
            loop {
                let event = session.request(
                    Command::ListParameters { start, count: 32 },
                    started,
                    timeout,
                )?;
                let HostEvent::Parameters { total, entries, .. } = event else {
                    return Err(VstAudioError::new_err("unexpected parameter reply"));
                };
                let count = entries.len() as u32;
                start = start
                    .checked_add(count)
                    .ok_or_else(|| VstAudioError::new_err("invalid parameter total"))?;
                result.extend(entries.into_iter().map(Into::into));
                if start >= total {
                    return Ok(result);
                }
                if count == 0 {
                    return Err(VstAudioError::new_err("empty parameter page before total"));
                }
            }
        })
    }

    /// Set a normalized parameter value and wait for acknowledgement.
    fn set_parameter(&mut self, py: Python<'_>, id: u32, value: f64) -> PyResult<()> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(PyValueError::new_err(
                "value must be finite and between 0 and 1",
            ));
        }
        self.control(py, Command::SetParameter { id, value }, 10.0)?;
        Ok(())
    }

    /// Save the plugin's opaque state.
    #[pyo3(signature = (*, timeout = 10.0))]
    fn save_state<'py>(&mut self, py: Python<'py>, timeout: f64) -> PyResult<Bound<'py, PyBytes>> {
        match self.control(py, Command::SaveState, timeout)? {
            HostEvent::State { bytes, .. } => Ok(PyBytes::new(py, &bytes)),
            _ => Err(VstAudioError::new_err("unexpected state reply")),
        }
    }

    /// Restore an opaque state previously saved by this plugin.
    #[pyo3(signature = (state, *, timeout = 10.0))]
    fn restore_state(&mut self, py: Python<'_>, state: &[u8], timeout: f64) -> PyResult<()> {
        self.control(py, Command::RestoreState(state.to_vec()), timeout)?;
        Ok(())
    }
    /// Open the worker-owned floating editor.
    fn open_editor(&mut self, py: Python<'_>) -> PyResult<()> {
        self.control(py, Command::OpenEditor, 10.0)?;
        Ok(())
    }
    /// Close the floating editor without stopping processing.
    fn close_editor(&mut self, py: Python<'_>) -> PyResult<()> {
        self.control(py, Command::CloseEditor, 10.0)?;
        Ok(())
    }
    /// Request worker teardown. Repeated calls do nothing.
    fn close(&mut self, py: Python<'_>) {
        let session = self.session.take();
        py.detach(move || drop(session));
    }
    fn __enter__(slf: PyRef<'_, Self>) -> PyResult<PyRef<'_, Self>> {
        if slf.session.is_none() {
            return Err(render_error(vst_audio::RenderError::NotRunning));
        }
        Ok(slf)
    }
    fn __exit__(
        &mut self,
        py: Python<'_>,
        _ty: &Bound<'_, PyAny>,
        _value: &Bound<'_, PyAny>,
        _tb: &Bound<'_, PyAny>,
    ) {
        self.close(py);
    }
}

fn input_array<T: numpy::Element + Copy>(
    audio: &Bound<'_, PyAny>,
    channels: usize,
    dtype: &str,
) -> PyResult<Array2<T>> {
    let actual: String = audio
        .getattr("dtype")
        .and_then(|dtype| dtype.str())
        .and_then(|s| s.extract())
        .map_err(|_| PyTypeError::new_err(format!("expected a NumPy array with dtype {dtype}")))?;
    if actual != dtype {
        return Err(PyTypeError::new_err(format!(
            "expected dtype {dtype}, got {actual}"
        )));
    }
    let array: PyReadonlyArray2<'_, T> = audio
        .extract()
        .map_err(|_| PyValueError::new_err("audio must be a 2-D NumPy array"))?;
    let view = array.as_array();
    if view.nrows() != channels {
        return Err(PyValueError::new_err(format!(
            "expected {channels} input channels, got {}",
            view.nrows()
        )));
    }
    // Iteration also accepts non-contiguous views, with the same planar semantics.
    Ok(view.as_standard_layout().into_owned())
}

fn output_buffer<T: Default + Clone>(
    channels: usize,
    frames: usize,
    tail: usize,
) -> PyResult<(usize, Vec<T>)> {
    let length = frames
        .checked_add(tail)
        .ok_or_else(|| PyValueError::new_err("render length overflow"))?;
    let samples = length
        .checked_mul(channels)
        .filter(|n| *n <= isize::MAX as usize / std::mem::size_of::<T>())
        .ok_or_else(|| PyValueError::new_err("render array is too large"))?;
    Ok((length, vec![T::default(); samples]))
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Host>()?;
    m.add_class::<Plugin>()?;
    m.add_class::<PluginClass>()?;
    m.add_class::<Parameter>()?;
    m.add("VstAudioError", m.py().get_type::<VstAudioError>())?;
    m.add(
        "WorkerNotFoundError",
        m.py().get_type::<WorkerNotFoundError>(),
    )?;
    m.add("StartError", m.py().get_type::<StartError>())?;
    m.add("RenderError", m.py().get_type::<RenderError>())?;
    m.add("RequestTimeout", m.py().get_type::<RequestTimeout>())?;
    Ok(())
}
