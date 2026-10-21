#![allow(unsafe_code)]
//! VST 2 calls are serialized with a non-waiting audio admission gate. Control
//! closes admission and waits for an in-flight block; audio never waits and
//! leaves blocks pending during control work. Foreign callbacks touch only atomics.
use crate::{editor::EditorWindow, library::Library, vst2::*};
use std::{
    ffi::c_void,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct PluginError(pub String);
#[derive(Clone, Debug)]
pub struct Info {
    pub name: String,
    pub vendor: String,
    pub inputs: u16,
    pub outputs: u16,
    pub latency: u32,
    pub input_types: Option<Vec<i32>>,
    pub output_types: Option<Vec<i32>>,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Events {
    pub params: bool,
    pub latency: bool,
    pub closed: bool,
}
#[derive(Debug)]
struct CallbackState {
    flags: AtomicU32,
    rate: AtomicU32,
    frames: AtomicU32,
    width: AtomicU32,
    height: AtomicU32,
    events: crate::vst2_events::CallbackEvents,
    offline: AtomicBool,
}
#[derive(Debug)]
struct Shared {
    effect: usize,
    admitted: AtomicBool,
    processing: AtomicBool,
    active: AtomicBool,
    closed: AtomicBool,
    callback: CallbackState,
    _library: Library,
}
// SAFETY: the library stays loaded; raw plugin access is serialized by admission
// and processing, and callbacks access only atomics. No borrowed Rust data crosses threads.
unsafe impl Send for Shared {}
// SAFETY: all plugin calls are serialized by the admission protocol; the rest is atomic.
unsafe impl Sync for Shared {}
impl Shared {
    fn control(&self) -> Control<'_> {
        self.admitted.store(false, Ordering::SeqCst);
        while self.processing.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        Control(self)
    }
    fn effect(&self) -> *mut AEffect {
        self.effect as *mut AEffect
    }
}
struct Control<'a>(&'a Shared);
impl Drop for Control<'_> {
    fn drop(&mut self) {
        self.0.admitted.store(true, Ordering::SeqCst);
    }
}
impl Control<'_> {
    fn effect(&self) -> &AEffect {
        // SAFETY: admission is closed and the audio thread has left its foreign call.
        unsafe { &*self.0.effect() }
    }
    fn call(
        &self,
        opcode: i32,
        index: i32,
        value: isize,
        pointer: *mut c_void,
        option: f32,
    ) -> isize {
        let Some(dispatch) = self.effect().dispatcher else {
            return 0;
        };
        // SAFETY: validated dispatcher; caller supplies the payload required by this opcode.
        unsafe { dispatch(self.0.effect(), opcode, index, value, pointer, option) }
    }
    fn text(&self, opcode: i32) -> String {
        self.text_at(opcode, 0)
    }
    fn text_at(&self, opcode: i32, index: i32) -> String {
        let mut buffer = [0u8; 256];
        self.call(opcode, index, 0, buffer.as_mut_ptr().cast(), 0.0);
        let length = buffer
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(buffer.len());
        String::from_utf8_lossy(&buffer[..length]).into_owned()
    }
}
impl Drop for Shared {
    fn drop(&mut self) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        // Load-error cleanup: no processor exists and this is the main thread.
        let control = self.control();
        if self.active.swap(false, Ordering::SeqCst) {
            control.call(STOP_PROCESS, 0, 0, std::ptr::null_mut(), 0.0);
            control.call(MAINS_CHANGED, 0, 0, std::ptr::null_mut(), 0.0);
        }
        control.call(CLOSE, 0, 0, std::ptr::null_mut(), 0.0);
    }
}

#[derive(Debug)]
pub struct Vst2Plugin {
    shared: Arc<Shared>,
    info: Info,
    editor: Option<EditorWindow>,
    closed: bool,
    configured: bool,
    parameter_count: u32,
    processor_taken: bool,
    format: vst_audio_ring::SampleFormat,
    _main: std::marker::PhantomData<std::rc::Rc<()>>,
}
#[derive(Debug)]
pub struct Vst2Processor {
    shared: Arc<Shared>,
    inputs: usize,
    outputs: usize,
    max_frames: usize,
    rate: u32,
    format: vst_audio_ring::SampleFormat,
    parameter_count: u32,
    midi: Box<[crate::vst2_events::MidiEvent; vst_audio_ring::MAX_EVENTS]>,
    sysex: Box<[crate::vst2_events::SysExEvent; vst_audio_ring::MAX_EVENTS]>,
    sysex_bytes: Box<[[u8; vst_audio_ring::MAX_SYSEX_BYTES]; vst_audio_ring::MAX_EVENTS]>,
}
impl Vst2Plugin {
    pub fn load(path: &Path) -> Result<Self, PluginError> {
        let library = Library::load(path).map_err(|e| PluginError(e.to_string()))?;
        let symbol = library
            .symbol(c"VSTPluginMain")
            .or_else(|| library.symbol(c"main"))
            .ok_or_else(|| PluginError("missing VSTPluginMain".into()))?;
        // SAFETY: symbol is the required VST 2 entry point in a disposable host process.
        let entry: Entry = unsafe { std::mem::transmute(symbol) };
        // SAFETY: host callback has the clean-room ABI and static lifetime.
        let effect = unsafe { entry(callback) };
        if effect.is_null() {
            return Err(PluginError("null plugin instance".into()));
        }
        // SAFETY: successful entry returns an AEffect that remains live until CLOSE.
        let fields = unsafe { &*effect };
        if fields.magic != MAGIC
            || fields.dispatcher.is_none()
            || (fields.process_replacing.is_none() && fields.process_double.is_none())
            || !(0..=64).contains(&fields.num_inputs)
            || !(0..=64).contains(&fields.num_outputs)
            || !(0..=16384).contains(&fields.num_params)
        {
            return Err(PluginError("invalid plugin ABI or channel count".into()));
        }
        // Parameter and bus counts are frozen at load; unsupported IO changes
        // are reported for replacement rather than silently resizing RT buffers.
        let parameter_count = fields.num_params as u32;
        let inputs = fields.num_inputs as u16;
        let outputs = fields.num_outputs as u16;
        let shared = Arc::new(Shared {
            effect: effect as usize,
            admitted: AtomicBool::new(true),
            processing: AtomicBool::new(false),
            active: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            callback: CallbackState {
                flags: AtomicU32::new(0),
                rate: AtomicU32::new(48000),
                frames: AtomicU32::new(480),
                width: AtomicU32::new(0),
                height: AtomicU32::new(0),
                events: crate::vst2_events::CallbackEvents::default(),
                offline: AtomicBool::new(false),
            },
            _library: library,
        });
        // SAFETY: before any processing; resvd1 is reserved for the host. Callback state
        // stays at a stable Arc allocation until after effClose and all processors drop.
        unsafe { (*effect).resvd1 = (&shared.callback as *const CallbackState).cast_mut().cast() };
        let control = shared.control();
        control.call(OPEN, 0, 0, std::ptr::null_mut(), 0.0);
        let mut input = std::ptr::null_mut::<Arrangement>();
        let mut output = std::ptr::null_mut::<Arrangement>();
        let arrangements = control.call(
            GET_SPEAKER_ARRANGEMENT,
            0,
            (&mut input as *mut *mut Arrangement) as isize,
            (&mut output as *mut *mut Arrangement).cast(),
            0.0,
        );
        let types =
            |pointer: *mut Arrangement, count: u16| -> Result<Option<Vec<i32>>, PluginError> {
                if arrangements == 0 || pointer.is_null() {
                    return Ok(None);
                }
                // SAFETY: successful arrangement query returns its live header.
                let channels = unsafe { (*pointer).channels };
                if channels != i32::from(count) {
                    return Err(PluginError("speaker arrangement count mismatch".into()));
                }
                // SAFETY: address of the first speaker only; do not reference a fixed-size
                // struct since third-party plugins may allocate exactly channels entries.
                let first = unsafe { std::ptr::addr_of!((*pointer).speakers).cast::<Speaker>() };
                // SAFETY: validated count <= 32; query guarantees that many live speaker records.
                let speakers = unsafe { std::slice::from_raw_parts(first, count as usize) };
                Ok(Some(speakers.iter().map(|speaker| speaker.kind).collect()))
            };
        let info = Info {
            name: control.text(GET_EFFECT_NAME),
            vendor: control.text(GET_VENDOR_STRING),
            inputs,
            outputs,
            latency: control.effect().initial_delay.max(0) as u32,
            input_types: types(input, inputs)?,
            output_types: types(output, outputs)?,
        };
        drop(control);
        Ok(Self {
            shared,
            info,
            editor: None,
            closed: false,
            configured: false,
            parameter_count,
            processor_taken: false,
            format: vst_audio_ring::SampleFormat::F32,
            _main: std::marker::PhantomData,
        })
    }
    pub fn capabilities(&self) -> vst_audio_protocol::Capabilities {
        let control = self.shared.control();
        vst_audio_protocol::Capabilities {
            f64: control.effect().process_double.is_some(),
            input_event_buses: 1,
            output_event_buses: 1,
            editor: control.effect().flags & HAS_EDITOR != 0,
            sysex: true,
            sample_accurate_parameters: true,
        }
    }
    pub fn info(&self) -> Info {
        self.info.clone()
    }
    pub fn configure(
        &mut self,
        rate: u32,
        frames: u32,
        inputs: u16,
        outputs: u16,
    ) -> Result<(), PluginError> {
        self.configure_precision(
            rate,
            frames,
            inputs,
            outputs,
            vst_audio_ring::SampleFormat::F32,
            false,
        )
    }
    pub fn configure_precision(
        &mut self,
        rate: u32,
        frames: u32,
        inputs: u16,
        outputs: u16,
        format: vst_audio_ring::SampleFormat,
        offline: bool,
    ) -> Result<(), PluginError> {
        if self.shared.active.load(Ordering::SeqCst)
            || self.processor_taken
            || rate < 1000
            || frames == 0
            || frames > 65536
            || inputs != self.info.inputs
            || outputs != self.info.outputs
        {
            return Err(PluginError("unsupported configuration".into()));
        }
        let control = self.shared.control();
        if (format == vst_audio_ring::SampleFormat::F64
            && control.effect().process_double.is_none())
            || (format == vst_audio_ring::SampleFormat::F32
                && control.effect().process_replacing.is_none())
        {
            return Err(PluginError("sample precision unsupported".into()));
        }
        self.format = format;
        self.shared
            .callback
            .offline
            .store(offline, Ordering::Relaxed);
        control.call(
            77,
            0,
            isize::from(format == vst_audio_ring::SampleFormat::F64),
            std::ptr::null_mut(),
            0.0,
        );
        self.shared.callback.rate.store(rate, Ordering::Relaxed);
        self.shared.callback.frames.store(frames, Ordering::Relaxed);
        control.call(SET_SAMPLE_RATE, 0, 0, std::ptr::null_mut(), rate as f32);
        control.call(
            SET_BLOCK_SIZE,
            0,
            frames as isize,
            std::ptr::null_mut(),
            0.0,
        );
        self.configured = true;
        Ok(())
    }
    pub fn activate(&mut self) {
        let control = self.shared.control();
        if !self.shared.active.swap(true, Ordering::SeqCst) {
            control.call(MAINS_CHANGED, 0, 1, std::ptr::null_mut(), 0.0);
            control.call(START_PROCESS, 0, 0, std::ptr::null_mut(), 0.0);
        }
    }
    pub fn deactivate(&mut self) {
        let control = self.shared.control();
        if self.shared.active.swap(false, Ordering::SeqCst) {
            control.call(STOP_PROCESS, 0, 0, std::ptr::null_mut(), 0.0);
            control.call(MAINS_CHANGED, 0, 0, std::ptr::null_mut(), 0.0);
        }
    }
    pub fn processor(&mut self) -> Result<Vst2Processor, PluginError> {
        if !self.configured || self.processor_taken {
            return Err(PluginError(
                "processor requires a unique configured instance".into(),
            ));
        }
        self.processor_taken = true;
        Ok(Vst2Processor {
            shared: self.shared.clone(),
            inputs: self.info.inputs as usize,
            outputs: self.info.outputs as usize,
            max_frames: self.shared.callback.frames.load(Ordering::Relaxed) as usize,
            rate: self.shared.callback.rate.load(Ordering::Relaxed),
            format: self.format,
            parameter_count: self.parameter_count,
            midi: Box::new([crate::vst2_events::MidiEvent::default(); vst_audio_ring::MAX_EVENTS]),
            sysex: Box::new(
                [crate::vst2_events::SysExEvent::default(); vst_audio_ring::MAX_EVENTS],
            ),
            sysex_bytes: Box::new(
                [[0; vst_audio_ring::MAX_SYSEX_BYTES]; vst_audio_ring::MAX_EVENTS],
            ),
        })
    }
    pub fn parameters(
        &mut self,
        start: u32,
        count: u16,
    ) -> Result<(u32, Vec<vst_audio_protocol::Parameter>), PluginError> {
        if count == 0 || count > 32 || start > self.parameter_count {
            return Err(PluginError("invalid parameter page".into()));
        }
        let control = self.shared.control();
        let get = control
            .effect()
            .get_parameter
            .ok_or_else(|| PluginError("no parameter getter".into()))?;
        let mut entries = Vec::new();
        for id in start
            ..start
                .saturating_add(u32::from(count))
                .min(self.parameter_count)
        {
            // SAFETY: cached validated parameter index, serialized plugin call.
            let value = f64::from(unsafe { get(self.shared.effect(), id) });
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(PluginError("invalid parameter value".into()));
            }
            entries.push(vst_audio_protocol::Parameter {
                id,
                name: control.text_at(PARAM_NAME, id as i32),
                unit: control.text_at(6, id as i32),
                value,
                default: None,
                steps: None,
                automatable: control.call(26, id as i32, 0, std::ptr::null_mut(), 0.0) == 1,
                read_only: control.effect().set_parameter.is_none(),
                hidden: false,
            });
        }
        Ok((self.parameter_count, entries))
    }
    pub fn set_parameter(&mut self, id: u32, value: f64) -> Result<(), PluginError> {
        if id >= self.parameter_count || !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(PluginError("invalid parameter edit".into()));
        }
        let control = self.shared.control();
        let set = control
            .effect()
            .set_parameter
            .ok_or_else(|| PluginError("no parameter setter".into()))?;
        // SAFETY: validated index/value, admission excludes audio during the call.
        unsafe { set(self.shared.effect(), id, value as f32) };
        Ok(())
    }
    pub fn get_state(&mut self) -> Result<Vec<u8>, PluginError> {
        let control = self.shared.control();
        if control.effect().flags & PROGRAM_CHUNKS != 0 {
            let mut data = std::ptr::null::<u8>();
            let size = control.call(GET_CHUNK, 0, 0, (&mut data as *mut *const u8).cast(), 0.0);
            if !(0..64 * 1024 * 1024 - 8).contains(&size) || (size != 0 && data.is_null()) {
                return Err(PluginError("invalid plugin chunk".into()));
            }
            if size == 0 {
                return Ok(Vec::new());
            }
            // SAFETY: GET_CHUNK guarantees size readable bytes until the next dispatcher call.
            return Ok(unsafe { std::slice::from_raw_parts(data, size as usize) }.to_vec());
        }
        let mut state = (control.call(GET_PROGRAM, 0, 0, std::ptr::null_mut(), 0.0) as i32)
            .to_le_bytes()
            .to_vec();
        let count = control.effect().num_params;
        let get = control
            .effect()
            .get_parameter
            .ok_or_else(|| PluginError("missing getParameter".into()))?;
        for index in 0..count {
            // SAFETY: validated parameter index; exclusive control access to plugin.
            let value = unsafe { get(self.shared.effect(), index as u32) };
            state.extend(value.to_le_bytes());
        }
        Ok(state)
    }
    pub fn set_state(&mut self, blob: &[u8]) -> Result<(), PluginError> {
        if blob.starts_with(b"SSV3") {
            return Err(PluginError(
                "VST 3 state cannot be restored into VST 2".into(),
            ));
        }
        if blob.len() >= 64 * 1024 * 1024 - 8 {
            return Err(PluginError("state too large".into()));
        }
        let control = self.shared.control();
        if control.effect().flags & PROGRAM_CHUNKS != 0 {
            let mut blob = blob.to_vec();
            control.call(
                SET_CHUNK,
                0,
                blob.len() as isize,
                blob.as_mut_ptr().cast(),
                0.0,
            );
        } else {
            let count = control.effect().num_params as usize;
            if blob.len() != 4 + count * 4 {
                return Err(PluginError("invalid parameter state".into()));
            }
            let program = i32::from_le_bytes(blob[..4].try_into().expect("four bytes"));
            if program < 0 || program >= control.effect().num_programs {
                return Err(PluginError("invalid program".into()));
            }
            control.call(SET_PROGRAM, 0, program as isize, std::ptr::null_mut(), 0.0);
            let set = control
                .effect()
                .set_parameter
                .ok_or_else(|| PluginError("missing setParameter".into()))?;
            for (index, value) in blob[4..].as_chunks::<4>().0.iter().enumerate() {
                let value = f32::from_le_bytes(*value);
                // SAFETY: exact state size checked; index is a valid parameter, control exclusive.
                unsafe { set(self.shared.effect(), index as u32, value) };
            }
        }
        Ok(())
    }
    pub fn open_editor(&mut self) -> Result<(), PluginError> {
        if self.editor.is_some() {
            return Ok(());
        }
        let control = self.shared.control();
        if control.effect().flags & HAS_EDITOR == 0 {
            return Err(PluginError("plugin has no editor".into()));
        }
        // effEditGetRect writes a pointer to the plugin's own rectangle, or
        // leaves the out-pointer alone (some plugins return null instead).
        let fallback = Rect {
            top: 0,
            left: 0,
            bottom: 480,
            right: 640,
        };
        let mut rect: *const Rect = &fallback;
        control.call(
            EDIT_GET_RECT,
            0,
            0,
            (&mut rect as *mut *const Rect).cast(),
            0.0,
        );
        // SAFETY: `rect` is still `&fallback`, or null, or the plugin's
        // rectangle, which stays valid until the next dispatcher call.
        let rect = unsafe { rect.as_ref() }.copied().unwrap_or(fallback);
        let (width, height) = (
            i32::from(rect.right) - i32::from(rect.left),
            i32::from(rect.bottom) - i32::from(rect.top),
        );
        let editor = EditorWindow::open(&self.info.name, width, height)
            .map_err(|e| PluginError(e.to_string()))?;
        control.call(EDIT_OPEN, 0, 0, editor.handle_value() as *mut c_void, 0.0);
        self.editor = Some(editor);
        Ok(())
    }
    pub fn editor_action(
        &mut self,
        action: vst_audio_protocol::EditorAction,
    ) -> Result<vst_audio_protocol::EditorReply, PluginError> {
        use vst_audio_protocol::{EditorAction as A, EditorReply, EditorStatus};
        let window = self
            .editor
            .as_ref()
            .ok_or_else(|| PluginError("editor is not open".into()))?;
        match action {
            A::Show => window.show(true).map_err(|e| PluginError(e.to_string()))?,
            A::Hide => window.show(false).map_err(|e| PluginError(e.to_string()))?,
            A::Focus => window.focus().map_err(|e| PluginError(e.to_string()))?,
            A::Inspect => {}
            A::Resize { .. } | A::Scale(_) => {
                return Err(PluginError(
                    "VST2 has no standard host-driven resize or scale contract".into(),
                ));
            }
            A::Capture => {
                return window
                    .capture()
                    .map(EditorReply::Image)
                    .map_err(|e| PluginError(e.to_string()));
            }
        }
        let (visible, width, height) = window.status().map_err(|e| PluginError(e.to_string()))?;
        Ok(EditorReply::Status(EditorStatus {
            visible,
            width,
            height,
            scale: 1.0,
            resize_supported: false,
            scale_supported: false,
        }))
    }
    pub fn request_editor_close(&self) -> Result<(), PluginError> {
        if let Some(editor) = &self.editor {
            editor
                .request_close()
                .map_err(|e| PluginError(e.to_string()))?;
        }
        Ok(())
    }
    pub fn idle(&mut self) {
        if let Some(editor) = &self.editor {
            let control = self.shared.control();
            control.call(EDIT_IDLE, 0, 0, std::ptr::null_mut(), 0.0);
            let width = self.shared.callback.width.swap(0, Ordering::Relaxed);
            let height = self.shared.callback.height.load(Ordering::Relaxed);
            if width != 0 {
                let _ = editor.resize(width as i32, height as i32);
            }
            if editor.pump() {
                control.call(EDIT_CLOSE, 0, 0, std::ptr::null_mut(), 0.0);
                self.editor = None;
                self.closed = true;
            }
        }
    }
    pub fn take_events(&mut self) -> Events {
        let flags = self.shared.callback.flags.swap(0, Ordering::AcqRel);
        if flags & 2 != 0 {
            let control = self.shared.control();
            self.info.latency = control.effect().initial_delay.max(0) as u32;
        }
        Events {
            params: flags & 1 != 0,
            latency: flags & 2 != 0,
            closed: std::mem::take(&mut self.closed),
        }
    }
}
impl Drop for Vst2Plugin {
    fn drop(&mut self) {
        if self.editor.is_some() {
            let control = self.shared.control();
            control.call(EDIT_CLOSE, 0, 0, std::ptr::null_mut(), 0.0);
            self.editor = None;
        }
        self.deactivate();
        let control = self.shared.control();
        control.call(CLOSE, 0, 0, std::ptr::null_mut(), 0.0);
        self.shared.closed.store(true, Ordering::SeqCst);
    }
}
// SAFETY: unique audio owner; pointers in preallocated SysEx records target its
// owned boxed scratch and are used only during an admitted audio call.
unsafe impl Send for Vst2Processor {}
impl Vst2Processor {
    pub fn process(&mut self, planes: &mut [&mut [f32]], frames: usize) -> bool {
        self.process_block(
            planes,
            frames,
            &vst_audio_ring::Transport::default(),
            &[],
            &mut vst_audio_ring::EventBuffer::default(),
        )
    }
    pub fn process_block(
        &mut self,
        planes: &mut [&mut [f32]],
        frames: usize,
        context: &vst_audio_ring::Transport,
        input: &[vst_audio_ring::Event],
        output: &mut vst_audio_ring::EventBuffer,
    ) -> bool {
        self.process_inner(planes, frames, context, input, output)
    }
    pub fn process_block_f64(
        &mut self,
        planes: &mut [&mut [f64]],
        frames: usize,
        context: &vst_audio_ring::Transport,
        input: &[vst_audio_ring::Event],
        output: &mut vst_audio_ring::EventBuffer,
    ) -> bool {
        self.process_inner(planes, frames, context, input, output)
    }
    fn process_inner<T: crate::vst2_events::Sample>(
        &mut self,
        planes: &mut [&mut [T]],
        frames: usize,
        context: &vst_audio_ring::Transport,
        input: &[vst_audio_ring::Event],
        output: &mut vst_audio_ring::EventBuffer,
    ) -> bool {
        use vst_audio_ring::{EventData, MAX_EVENTS, SampleFormat};
        output.clear();
        if T::DOUBLE != (self.format == SampleFormat::F64)
            || frames == 0
            || frames > self.max_frames
            || planes.len() < self.inputs.max(self.outputs)
            || planes.iter().any(|p| p.len() < frames)
            || !context.valid()
            || input.len() > MAX_EVENTS
            || input.iter().any(|e| !e.valid(frames as u32) || e.bus != 0)
            || input
                .windows(2)
                .any(|p| p[0].sample_offset > p[1].sample_offset)
            || self.shared.closed.load(Ordering::SeqCst)
            || !self.shared.admitted.load(Ordering::SeqCst)
            || self.shared.processing.swap(true, Ordering::SeqCst)
        {
            return false;
        }
        let result = (|| {
            if !self.shared.admitted.load(Ordering::SeqCst)
                || !self.shared.active.load(Ordering::SeqCst)
                || self.shared.closed.load(Ordering::SeqCst)
            {
                return false;
            }
            // SAFETY: admission owns the live plugin and serializes all effect access.
            let dispatch =
                unsafe { (*self.shared.effect()).dispatcher }.expect("validated dispatcher");
            // SAFETY: same admitted instance; the optional setter is copied before calls.
            let set = unsafe { (*self.shared.effect()).set_parameter };
            let mut base_context = *context;
            let mut context_origin = 0;
            let mut start = 0;
            while start < frames {
                for event in input.iter().filter(|e| e.sample_offset as usize == start) {
                    match event.data {
                        EventData::Parameter { id, value } => {
                            if id >= self.parameter_count || set.is_none() {
                                output.dropped = output.dropped.saturating_add(1);
                                continue;
                            }
                            let set = set.expect("validated setter");
                            // SAFETY: admitted instance, validated parameter index and normalized value.
                            unsafe { set(self.shared.effect(), id, value as f32) };
                        }
                        EventData::Transport(value) => {
                            base_context = value;
                            context_origin = start;
                        }
                        _ => {}
                    }
                }
                let end = input
                    .iter()
                    .filter(|e| {
                        e.sample_offset as usize > start
                            && matches!(
                                e.data,
                                EventData::Parameter { .. } | EventData::Transport(_)
                            )
                    })
                    .map(|e| e.sample_offset as usize)
                    .next()
                    .unwrap_or(frames);
                self.shared.callback.events.context(
                    base_context.advanced(start - context_origin, self.rate),
                    self.rate,
                    start,
                );
                let mut list = crate::vst2_events::Events::<MAX_EVENTS> {
                    count: 0,
                    reserved: 0,
                    events: [std::ptr::null(); MAX_EVENTS],
                };
                for event in input
                    .iter()
                    .filter(|e| (start..end).contains(&(e.sample_offset as usize)))
                {
                    let i = list.count as usize;
                    match &event.data {
                        EventData::Midi([a, b, c]) => {
                            self.midi[i] = crate::vst2_events::MidiEvent {
                                kind: 1,
                                size: size_of::<crate::vst2_events::MidiEvent>() as i32,
                                offset: event.sample_offset as i32 - start as i32,
                                data: [*a, *b, *c, 0],
                                ..Default::default()
                            };
                            list.events[i] =
                                (&self.midi[i] as *const crate::vst2_events::MidiEvent).cast();
                            list.count += 1;
                        }
                        EventData::SysEx { len, bytes } => {
                            self.sysex_bytes[i].copy_from_slice(bytes);
                            self.sysex[i] = crate::vst2_events::SysExEvent {
                                offset: event.sample_offset as i32 - start as i32,
                                dump_bytes: i32::from(*len),
                                data: self.sysex_bytes[i].as_mut_ptr(),
                                ..Default::default()
                            };
                            list.events[i] =
                                (&self.sysex[i] as *const crate::vst2_events::SysExEvent).cast();
                            list.count += 1;
                        }
                        _ => {}
                    }
                }
                if list.count != 0 {
                    // SAFETY: bounded list points into owned scratch retained through this processing segment.
                    unsafe {
                        dispatch(
                            self.shared.effect(),
                            25,
                            0,
                            0,
                            (&mut list as *mut crate::vst2_events::Events<MAX_EVENTS>).cast(),
                            0.0,
                        )
                    };
                }
                let mut inputs = [std::ptr::null(); 64];
                let mut outputs = [std::ptr::null_mut(); 64];
                for (pointer, plane) in inputs.iter_mut().zip(planes.iter()).take(self.inputs) {
                    *pointer = plane.as_ptr().wrapping_add(start);
                }
                for (pointer, plane) in outputs.iter_mut().zip(planes.iter_mut()).take(self.outputs)
                {
                    *pointer = plane.as_mut_ptr().wrapping_add(start);
                }
                // SAFETY: precision was configured, pointers cover every declared channel and the bounded segment.
                unsafe {
                    T::invoke(
                        self.shared.effect(),
                        inputs.as_ptr(),
                        outputs.as_mut_ptr(),
                        (end - start) as i32,
                    )
                };
                self.shared.callback.events.drain(output, frames);
                start = end;
            }
            true
        })();
        self.shared.processing.store(false, Ordering::SeqCst);
        result
    }
}

unsafe extern "C" fn callback(
    effect: *mut AEffect,
    opcode: i32,
    index: i32,
    value: isize,
    pointer: *mut c_void,
    _option: f32,
) -> isize {
    if opcode == MASTER_VERSION {
        return 2400;
    }
    if opcode == MASTER_VENDOR || opcode == MASTER_PRODUCT {
        if pointer.is_null() {
            return 0;
        }
        let text = b"Plugin host\0";
        // SAFETY: callback opcode supplies a writable host-name buffer of at least 64 bytes.
        unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), pointer.cast(), text.len()) };
        return 1;
    }
    if effect.is_null() {
        return 0;
    }
    // SAFETY: callback's effect belongs to the plugin; resvd1 is host-owned storage.
    let state = unsafe { (*effect).resvd1.cast::<CallbackState>() };
    if state.is_null() {
        return 0;
    }
    // SAFETY: host installed a stable CallbackState before effOpen and retains it through effClose.
    let state = unsafe { &*state };
    match opcode {
        7 => state.events.time_pointer(),
        8 => {
            // SAFETY: host processEvents callback supplies a live VST2 event list.
            unsafe { state.events.receive(pointer) }
        }
        23 => {
            if state.offline.load(Ordering::Relaxed) {
                4
            } else {
                2
            }
        }
        MASTER_SAMPLE_RATE => state.rate.load(Ordering::Relaxed) as isize,
        MASTER_BLOCK_SIZE => state.frames.load(Ordering::Relaxed) as isize,
        MASTER_AUTOMATE => {
            state.flags.fetch_or(1, Ordering::Release);
            1
        }
        MASTER_IO_CHANGED => {
            state.flags.fetch_or(2, Ordering::Release);
            1
        }
        MASTER_SIZE_WINDOW if index > 0 && value > 0 && index <= 8192 && value <= 8192 => {
            state.height.store(value as u32, Ordering::Relaxed);
            state.width.store(index as u32, Ordering::Release);
            1
        }
        _ => 0,
    }
}
