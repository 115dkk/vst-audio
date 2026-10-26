// The generated C++ enum constants are i32 on Windows and u32 on Unix.
#![allow(clippy::unnecessary_cast)]
#![allow(unsafe_code, non_snake_case)]
//! Test-only COM implementation using vst3 0.3.0's generated interfaces.
use std::{
    cell::{RefCell, UnsafeCell},
    ffi::{CStr, c_char, c_void},
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};
use vst3::{Class, ComPtr, ComRef, ComWrapper, Steinberg::Vst::*, Steinberg::*, uid};
const CID: TUID = uid(0x53535233, 0x74657374, 0x6d617472, 0x69783031);
fn narrow(text: &str, output: &mut [c_char]) {
    output.fill(0);
    let capacity = output.len().saturating_sub(1);
    for (dst, src) in output.iter_mut().take(capacity).zip(text.bytes()) {
        *dst = src as c_char;
    }
}
fn wide(text: &str, output: &mut [u16]) {
    output.fill(0);
    let capacity = output.len().saturating_sub(1);
    for (dst, src) in output.iter_mut().take(capacity).zip(text.encode_utf16()) {
        *dst = src;
    }
}
struct Delay {
    samples: [[f32; 64]; 6],
    cursor: usize,
}
struct Matrix {
    mode: AtomicU8,
    probe: [AtomicU8; 3],
    width: AtomicU64,
    active: AtomicBool,
    flood_edits: bool,
    state_width: bool,
    delay: UnsafeCell<Delay>,
    handler: RefCell<Option<ComPtr<IComponentHandler>>>,
    #[cfg(target_os = "linux")]
    context: RefCell<Option<ComPtr<IHostApplication>>>,
    #[cfg(target_os = "linux")]
    lifecycle: std::rc::Rc<std::cell::Cell<[u32; 4]>>,
}
impl Matrix {
    fn new() -> Self {
        Self {
            mode: AtomicU8::new(0),
            probe: [const { AtomicU8::new(0) }; 3],
            width: AtomicU64::new(1.0f64.to_bits()),
            active: AtomicBool::new(false),
            flood_edits: std::env::var_os("VST_AUDIO_TEST_EDIT_FLOOD").is_some(),
            state_width: std::env::var_os("VST_AUDIO_TEST_STATE_WIDTH").is_some(),
            delay: UnsafeCell::new(Delay {
                samples: [[0.0; 64]; 6],
                cursor: 0,
            }),
            handler: RefCell::new(None),
            #[cfg(target_os = "linux")]
            context: RefCell::new(None),
            #[cfg(target_os = "linux")]
            lifecycle: Default::default(),
        }
    }
    unsafe fn restore(&self, state: *mut IBStream) -> tresult {
        // SAFETY: SDK passes a live stream borrowed only for this state call.
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else {
            return kInvalidArgument;
        };
        let mut bytes = [0u8; 2];
        let mut read = 0;
        // SAFETY: two-byte writable output and byte-count output are live for this call.
        let result = unsafe { stream.read(bytes.as_mut_ptr().cast(), 2, &mut read) };
        if result != kResultOk || read != 2 || bytes[0] != 1 || bytes[1] > 6 {
            return kInvalidArgument;
        }
        // TD-08 opt-in state carries Width immediately after the version and mode.
        if self.state_width {
            let mut width = [0u8; 8];
            // SAFETY: eight-byte writable output and byte-count output live for this call.
            let result = unsafe { stream.read(width.as_mut_ptr().cast(), 8, &mut read) };
            let value = f64::from_le_bytes(width);
            if result != kResultOk
                || read != 8
                || !value.is_finite()
                || !(0.0..=1.0).contains(&value)
            {
                return kInvalidArgument;
            }
            self.width.store(value.to_bits(), Ordering::Relaxed);
        }
        if bytes[1] == 5 {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        if bytes[1] == 4 {
            std::process::abort();
        }
        if bytes[1] == 6 {
            for (target, result) in self.probe.iter().zip(crate::probe::run()) {
                target.store(result, Ordering::Relaxed);
            }
        }
        self.mode.store(bytes[1], Ordering::Relaxed);
        kResultOk
    }
    unsafe fn save(&self, state: *mut IBStream) -> tresult {
        // SAFETY: live stream borrowed from SDK for synchronous main-thread state save.
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else {
            return kInvalidArgument;
        };
        let mode = self.mode.load(Ordering::Relaxed);
        let bytes = [
            1,
            mode,
            self.probe[0].load(Ordering::Relaxed),
            self.probe[1].load(Ordering::Relaxed),
            self.probe[2].load(Ordering::Relaxed),
        ];
        let length = if mode == 6 { 5 } else { 2 };
        #[cfg(target_os = "linux")]
        let bytes = if std::env::var_os("VST_AUDIO_VIEW_LIFECYCLE").is_some() {
            bytes[..length as usize]
                .iter()
                .copied()
                .chain(self.lifecycle.get().into_iter().flat_map(u32::to_le_bytes))
                .collect::<Vec<_>>()
        } else {
            bytes[..length as usize].to_vec()
        };
        #[cfg(target_os = "linux")]
        let length = bytes.len() as i32;
        let mut bytes = bytes[..length as usize].to_vec();
        if self.state_width {
            bytes.splice(2..2, self.width.load(Ordering::Relaxed).to_le_bytes());
        }
        let length = bytes.len() as i32;
        let mut written = 0;
        // SAFETY: stream reads length initialized bytes synchronously; count output is writable.
        let result = unsafe { stream.write(bytes.as_mut_ptr().cast(), length, &mut written) };
        if result == kResultOk && written == length {
            kResultOk
        } else {
            kResultFalse
        }
    }
}
impl Class for Matrix {
    type Interfaces = (IComponent, IAudioProcessor, IEditController);
}
impl IPluginBaseTrait for Matrix {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: host initialize supplies its live context for this call.
            let Some(context) = (unsafe { ComRef::from_raw(_context) }) else {
                return kInvalidArgument;
            };
            *self.context.borrow_mut() = context.cast::<IHostApplication>();
            if context.cast::<Linux::IRunLoop>().is_none() {
                return kNoInterface;
            }
        }
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}
impl IComponentTrait for Matrix {
    unsafe fn getControllerClassId(&self, _: *mut TUID) -> tresult {
        kNotImplemented
    }
    unsafe fn setIoMode(&self, _: i32) -> tresult {
        kResultOk
    }
    unsafe fn getBusCount(&self, media: i32, direction: i32) -> i32 {
        i32::from(
            media == MediaTypes_::kAudio as i32
                && [{ BusDirections_::kInput as i32 }, {
                    BusDirections_::kOutput as i32
                }]
                .contains(&direction),
        )
    }
    unsafe fn getBusInfo(
        &self,
        media: i32,
        direction: i32,
        index: i32,
        info: *mut BusInfo,
    ) -> tresult {
        if media != MediaTypes_::kAudio as i32 || index != 0 || info.is_null() {
            return kInvalidArgument;
        }
        let channels = match direction {
            0 => 2,
            1 => 6,
            _ => return kInvalidArgument,
        };
        // SAFETY: non-null SDK BusInfo output for the queried main bus.
        let info = unsafe { &mut *info };
        info.mediaType = media;
        info.direction = direction;
        info.channelCount = channels;
        info.busType = BusTypes_::kMain as i32;
        info.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
        wide(
            if channels == 2 {
                "Stereo input"
            } else {
                "Surround output"
            },
            &mut info.name,
        );
        kResultOk
    }
    unsafe fn getRoutingInfo(&self, _: *mut RoutingInfo, _: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }
    unsafe fn activateBus(&self, media: i32, direction: i32, index: i32, _: TBool) -> tresult {
        if media == MediaTypes_::kAudio as i32 && (0..=1).contains(&direction) && index == 0 {
            kResultOk
        } else {
            kInvalidArgument
        }
    }
    unsafe fn setActive(&self, active: TBool) -> tresult {
        self.active.store(active != 0, Ordering::Relaxed);
        kResultOk
    }
    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        // SAFETY: forwards SDK stream with identical synchronous lifetime contract.
        unsafe { self.restore(state) }
    }
    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        // SAFETY: forwards SDK stream; state uses atomics and can overlap process.
        unsafe { self.save(state) }
    }
}
impl IAudioProcessorTrait for Matrix {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut u64,
        ni: i32,
        outputs: *mut u64,
        no: i32,
    ) -> tresult {
        if ni != 1 || no != 1 || inputs.is_null() || outputs.is_null() {
            return kResultFalse;
        }
        // SAFETY: caller supplied one input arrangement as declared by ni.
        let input = unsafe { *inputs };
        // SAFETY: caller supplied one output arrangement as declared by no.
        let output = unsafe { *outputs };
        if input == SpeakerArr::kStereo && output == SpeakerArr::k51 {
            kResultTrue
        } else {
            kResultFalse
        }
    }
    unsafe fn getBusArrangement(
        &self,
        direction: i32,
        index: i32,
        arrangement: *mut u64,
    ) -> tresult {
        if index != 0 || arrangement.is_null() {
            return kInvalidArgument;
        }
        let value = match direction {
            0 => SpeakerArr::kStereo,
            1 => SpeakerArr::k51,
            _ => return kInvalidArgument,
        };
        // SAFETY: non-null writable arrangement output for the main bus.
        unsafe { *arrangement = value };
        kResultOk
    }
    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 as i32 {
            kResultOk
        } else {
            kResultFalse
        }
    }
    unsafe fn getLatencySamples(&self) -> u32 {
        64
    }
    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        if setup.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: caller provides a live ProcessSetup during inactive configuration.
        let setup = unsafe { &*setup };
        if setup.symbolicSampleSize != SymbolicSampleSizes_::kSample32 as i32
            || setup.maxSamplesPerBlock <= 0
        {
            kInvalidArgument
        } else {
            kResultOk
        }
    }
    unsafe fn setProcessing(&self, _: TBool) -> tresult {
        kResultOk
    }
    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        if data.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: sole processing thread receives a live ProcessData for this block.
        let data = unsafe { &*data };
        if !self.active.load(Ordering::Relaxed)
            || data.symbolicSampleSize != SymbolicSampleSizes_::kSample32 as i32
            || data.numInputs != 1
            || data.numOutputs != 1
            || data.inputs.is_null()
            || data.outputs.is_null()
            || data.numSamples < 0
        {
            return kInvalidArgument;
        }
        match self.mode.load(Ordering::Relaxed) {
            1 => std::process::abort(),
            2 => loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            },
            _ => {}
        }
        // SAFETY: optional SDK parameter changes interface is borrowed for process only.
        if let Some(changes) = unsafe { ComRef::from_raw(data.inputParameterChanges) } {
            // SAFETY: live parameter changes interface, queried synchronously on audio thread.
            let count = unsafe { changes.getParameterCount() };
            for index in 0..count {
                // SAFETY: index from reported parameter queue count.
                let pointer = unsafe { changes.getParameterData(index) };
                // SAFETY: borrowed queue is valid until process returns.
                if let Some(queue) = unsafe { ComRef::from_raw(pointer) } {
                    // SAFETY: live input parameter queue.
                    let id = unsafe { queue.getParameterId() };
                    // SAFETY: live input parameter queue.
                    let points = unsafe { queue.getPointCount() };
                    if id == 0 && points > 0 {
                        let mut offset = 0;
                        let mut value: f64 = 0.0;
                        // SAFETY: valid last-point index and writable offset/value outputs.
                        if unsafe { queue.getPoint(points - 1, &mut offset, &mut value) }
                            == kResultOk
                            && value.is_finite()
                        {
                            self.width
                                .store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
                        }
                    }
                }
            }
        }
        // SAFETY: data declares one live input AudioBusBuffers.
        let input = unsafe { &*data.inputs };
        // SAFETY: data declares one live output AudioBusBuffers.
        let output = unsafe { &*data.outputs };
        if input.numChannels != 2 || output.numChannels != 6 {
            return kInvalidArgument;
        }
        // SAFETY: sample format validated as kSample32 above.
        let inputs = unsafe { input.__field0.channelBuffers32 };
        // SAFETY: sample format validated as kSample32 above.
        let outputs = unsafe { output.__field0.channelBuffers32 };
        if inputs.is_null() || outputs.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: input bus declares two live channel pointers.
        let inputs = unsafe { std::slice::from_raw_parts(inputs, 2) };
        // SAFETY: output bus declares six live channel pointers.
        let outputs = unsafe { std::slice::from_raw_parts(outputs, 6) };
        if inputs.iter().chain(outputs).any(|p| p.is_null()) {
            return kInvalidArgument;
        }
        // SAFETY: delay storage accessed only by the sole audio thread, never state/controller calls.
        let delay = unsafe { &mut *self.delay.get() };
        let width = f64::from_bits(self.width.load(Ordering::Relaxed)) as f32;
        for frame in 0..data.numSamples as usize {
            // SAFETY: input channel has numSamples readable f32s, read before any in-place writes.
            let left = unsafe { *inputs[0].wrapping_add(frame) };
            // SAFETY: input channel has numSamples readable f32s, read before any in-place writes.
            let right = unsafe { *inputs[1].wrapping_add(frame) };
            let center = 0.5 * (left + right);
            let values = [
                left,
                right,
                center,
                center,
                0.5 * left * width,
                0.5 * right * width,
            ];
            for channel in 0..6 {
                let delayed = delay.samples[channel][delay.cursor];
                delay.samples[channel][delay.cursor] = values[channel];
                let value = if self.mode.load(Ordering::Relaxed) == 3 && channel == 2 {
                    f32::NAN
                } else {
                    delayed
                };
                // SAFETY: output channel has numSamples writable f32s; no aliased Rust references exist.
                unsafe { *outputs[channel].wrapping_add(frame) = value };
            }
            delay.cursor = (delay.cursor + 1) % 64;
        }
        kResultOk
    }
    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}
impl IEditControllerTrait for Matrix {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        // SAFETY: fresh SDK component state stream; restores only atomics.
        unsafe { self.restore(state) }
    }
    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        // SAFETY: controller state stream uses the same test fixture representation.
        unsafe { self.restore(state) }
    }
    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        // SAFETY: writable controller state stream supplied by host.
        unsafe { self.save(state) }
    }
    unsafe fn getParameterCount(&self) -> i32 {
        1
    }
    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        if index != 0 || info.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: SDK output for parameter zero is a writable ParameterInfo.
        let info = unsafe { &mut *info };
        info.id = 0;
        info.stepCount = 0;
        info.defaultNormalizedValue = 1.0;
        info.unitId = 0;
        info.flags = ParameterInfo_::ParameterFlags_::kCanAutomate;
        wide("Width", &mut info.title);
        wide("Width", &mut info.shortTitle);
        wide("", &mut info.units);
        kResultOk
    }
    unsafe fn getParamStringByValue(&self, id: u32, value: f64, string: *mut String128) -> tresult {
        if id != 0 || string.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: writable fixed-size SDK parameter string output.
        let string = unsafe { &mut *string };
        wide(&value.to_string(), string);
        kResultOk
    }
    unsafe fn getParamValueByString(&self, _: u32, _: *mut TChar, _: *mut f64) -> tresult {
        kNotImplemented
    }
    unsafe fn normalizedParamToPlain(&self, _: u32, value: f64) -> f64 {
        value
    }
    unsafe fn plainParamToNormalized(&self, _: u32, value: f64) -> f64 {
        value
    }
    unsafe fn getParamNormalized(&self, _: u32) -> f64 {
        f64::from_bits(self.width.load(Ordering::Relaxed))
    }
    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        if id != 0 || !value.is_finite() {
            return kInvalidArgument;
        }
        self.width
            .store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        kResultOk
    }
    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        // SAFETY: SDK handler is borrowed; to_com_ptr retains it for controller lifetime.
        *self.handler.borrow_mut() =
            unsafe { ComRef::from_raw(handler) }.map(|handler| handler.to_com_ptr());
        kResultOk
    }
    unsafe fn createView(&self, name: *const c_char) -> *mut IPlugView {
        if name.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: SDK passes a terminated view type identifier.
        if unsafe { CStr::from_ptr(name) } != c"editor" {
            return std::ptr::null_mut();
        }
        #[cfg(target_os = "linux")]
        {
            let mut counts = self.lifecycle.get();
            counts[0] += 1;
            self.lifecycle.set(counts);
        }
        let wrapper = ComWrapper::new(View {
            #[cfg(target_os = "linux")]
            lifecycle: self.lifecycle.clone(),
            canvas: std::cell::RefCell::new(None),
            size: std::cell::Cell::new((320, 200)),
            scale: std::cell::Cell::new(1.0),
            handler: self.handler.borrow().clone(),
            flood_edits: self.flood_edits,
            #[cfg(target_os = "linux")]
            context: self.context.borrow().clone(),
            #[cfg(target_os = "linux")]
            wayland: RefCell::new(None),
            frame: std::cell::RefCell::new(None),
            #[cfg(target_os = "linux")]
            wayland_frame: std::cell::Cell::new(false),
            view_pointer: std::cell::Cell::new(std::ptr::null_mut()),
        });
        let view = wrapper.to_com_ptr::<IPlugView>().expect("view interface");
        wrapper.view_pointer.set(view.as_ptr());
        view.into_raw()
    }
}
struct View {
    #[cfg(target_os = "linux")]
    lifecycle: std::rc::Rc<std::cell::Cell<[u32; 4]>>,
    canvas: std::cell::RefCell<Option<crate::visual::Canvas>>,
    size: std::cell::Cell<(i32, i32)>,
    scale: std::cell::Cell<f32>,
    handler: Option<ComPtr<IComponentHandler>>,
    flood_edits: bool,
    #[cfg(target_os = "linux")]
    context: Option<ComPtr<IHostApplication>>,
    #[cfg(target_os = "linux")]
    wayland: RefCell<Option<crate::wayland::Canvas>>,
    frame: std::cell::RefCell<Option<ComPtr<IPlugFrame>>>,
    #[cfg(target_os = "linux")]
    wayland_frame: std::cell::Cell<bool>,
    view_pointer: std::cell::Cell<*mut IPlugView>,
}
#[cfg(target_os = "linux")]
impl Drop for View {
    fn drop(&mut self) {
        let mut counts = self.lifecycle.get();
        counts[3] += 1;
        self.lifecycle.set(counts);
    }
}
impl Class for View {
    type Interfaces = (IPlugView, IPlugViewContentScaleSupport);
}
impl IPlugViewTrait for View {
    unsafe fn isPlatformTypeSupported(&self, platform: FIDString) -> tresult {
        if platform.is_null() {
            return kInvalidArgument;
        }
        #[cfg(target_os = "linux")]
        // SAFETY: SDK platform identifier is a terminated string.
        if unsafe { CStr::from_ptr(platform) } == c"WaylandSurfaceID" {
            return kResultTrue;
        }
        // SAFETY: SDK platform string is null-terminated.
        if unsafe { CStr::from_ptr(platform) } == vst_audio_worker_sys::editor_platform() {
            kResultTrue
        } else {
            kResultFalse
        }
    }
    unsafe fn attached(&self, parent: *mut c_void, platform: FIDString) -> tresult {
        if parent.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: forwards identical SDK platform string contract.
        if unsafe { self.isPlatformTypeSupported(platform) } != kResultTrue {
            return kResultFalse;
        }
        #[cfg(target_os = "linux")]
        // SAFETY: validated SDK platform string is terminated.
        if unsafe { CStr::from_ptr(platform) } == c"WaylandSurfaceID" {
            if std::env::var_os("VST_AUDIO_REJECT_WAYLAND").is_some()
                || (std::env::var_os("VST_AUDIO_REJECT_RECREATED_VIEW").is_some()
                    && self.lifecycle.get()[1] > 0)
            {
                return kResultFalse;
            }
            if let Ok(expected) = std::env::var("VST_AUDIO_EXPECT_ATTACH_SCALE") {
                let expected: f32 = expected.parse().expect("fixture scale");
                eprintln!("fixture pre-attach scale={}", self.scale.get());
                if self.scale.get() != expected {
                    return kResultFalse;
                }
            }
            let Some(context) = &self.context else {
                return kNoInterface;
            };
            // SAFETY: attached supplies a live parent surface from this host's connection on the UI thread.
            return match unsafe { crate::wayland::Canvas::attach(context, parent) } {
                Ok(canvas) => {
                    *self.wayland.borrow_mut() = Some(canvas);
                    let mut counts = self.lifecycle.get();
                    counts[1] += 1;
                    self.lifecycle.set(counts);
                    kResultOk
                }
                Err(_) => kResultFalse,
            };
        }
        // SAFETY: attached supplies a live platform parent on the UI thread.
        *self.canvas.borrow_mut() =
            unsafe { crate::visual::Canvas::attach(parent, self.size.get().0, self.size.get().1) };
        if self.canvas.borrow().is_none() {
            return kResultFalse;
        }
        // A deterministic editor gesture exercises the host's lock-free automation path.
        if let Some(handler) = &self.handler {
            // SAFETY: attached is on UI thread; retained host handler and known parameter zero.
            let _ = unsafe { handler.beginEdit(0) };
            let edits = if self.flood_edits { 5000 } else { 1 };
            for step in 0..edits {
                let value = if edits == 1 {
                    0.5
                } else {
                    // Ends away from the initial 1.0 so a stale processor shows in audio.
                    0.75 * step as f64 / (edits - 1) as f64
                };
                // SAFETY: UI-thread normalized edit for Width; processor receives it via queue.
                let _ = unsafe { handler.performEdit(0, value) };
            }
            // SAFETY: finishes the preceding UI-thread edit gesture.
            let _ = unsafe { handler.endEdit(0) };
        }
        kResultOk
    }
    unsafe fn removed(&self) -> tresult {
        self.canvas.borrow_mut().take();
        #[cfg(target_os = "linux")]
        if self.wayland.borrow_mut().take().is_some() {
            let mut counts = self.lifecycle.get();
            counts[2] += 1;
            self.lifecycle.set(counts);
        }
        kResultOk
    }
    unsafe fn onWheel(&self, _: f32) -> tresult {
        kResultFalse
    }
    unsafe fn onKeyDown(&self, _: char16, _: i16, _: i16) -> tresult {
        kResultFalse
    }
    unsafe fn onKeyUp(&self, _: char16, _: i16, _: i16) -> tresult {
        kResultFalse
    }
    unsafe fn getSize(&self, rect: *mut ViewRect) -> tresult {
        if rect.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: writable SDK rectangle output for this fixed-size editor.
        unsafe {
            *rect = ViewRect {
                left: 17,
                top: 23,
                right: 17 + self.size.get().0,
                bottom: 23 + self.size.get().1,
            }
        };
        kResultOk
    }
    unsafe fn onSize(&self, rect: *mut ViewRect) -> tresult {
        if rect.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: host supplies a writable SDK rectangle for this callback.
        let rect = unsafe { *rect };
        self.size
            .set((rect.right - rect.left, rect.bottom - rect.top));
        if let Some(canvas) = self.canvas.borrow().as_ref() {
            canvas.resize(self.size.get().0, self.size.get().1);
        }
        #[cfg(target_os = "linux")]
        if let Some(canvas) = self.wayland.borrow().as_ref()
            && canvas.resize(self.size.get().0, self.size.get().1).is_err()
        {
            return kResultFalse;
        }
        // Host rectangles may carry this fixture's non-zero origin; compare the width.
        if std::env::var_os("VST_AUDIO_TEST_VST3_RESIZE").is_some()
            && rect.right - rect.left == 400
            && let Some(frame) = self.frame.borrow().as_ref()
        {
            let mut requested = ViewRect {
                left: 0,
                top: 0,
                right: 520,
                bottom: 300,
            };
            // SAFETY: retained main-thread frame, live writable rectangle. view pointer is borrowed from this live COM wrapper.
            let _ = unsafe { frame.resizeView(self.view_pointer.get(), &mut requested) };
        }
        kResultOk
    }
    unsafe fn onFocus(&self, _: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setFrame(&self, _frame: *mut IPlugFrame) -> tresult {
        // SAFETY: borrowed SDK frame pointer; retain until detached.
        *self.frame.borrow_mut() = unsafe { ComRef::from_raw(_frame) }.map(|f| f.to_com_ptr());
        #[cfg(target_os = "linux")]
        if !_frame.is_null() {
            // SAFETY: setFrame provides a live frame, or null during detach.
            let frame = unsafe { ComRef::from_raw(_frame) }.expect("checked frame");
            if frame.cast::<Linux::IRunLoop>().is_none() {
                return kNoInterface;
            }
            let wayland = self.context.as_ref().is_some_and(|context| {
                let Some(host) = context.cast::<IWaylandHost>() else {
                    return false;
                };
                let Some(frame) = frame.cast::<IWaylandFrame>() else {
                    return false;
                };
                // SAFETY: retained host owns this temporary connection until close below.
                let display = unsafe { host.openWaylandConnection() };
                if display.is_null() {
                    return false;
                }
                // SAFETY: retained frame is queried with the live display from this host.
                let surface = unsafe { frame.getWaylandSurface(display) };
                // SAFETY: balances the successful open after the synchronous frame query.
                unsafe { host.closeWaylandConnection(display) };
                !surface.is_null()
            });
            self.wayland_frame.set(wayland);
        }
        #[cfg(target_os = "linux")]
        if _frame.is_null() {
            self.wayland_frame.set(false);
        }
        kResultOk
    }
    unsafe fn canResize(&self) -> tresult {
        kResultTrue
    }
    unsafe fn checkSizeConstraint(&self, rect: *mut ViewRect) -> tresult {
        if rect.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: caller passes a live writable rectangle for size negotiation.
        let rect = unsafe { &mut *rect };
        rect.right = rect.right.max(rect.left + 320);
        rect.bottom = rect.bottom.max(rect.top + 200);
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        rect.left = 17;
        rect.top = 23;
        rect.right = 17 + width;
        rect.bottom = 23 + height;
        kResultOk
    }
}
impl IPlugViewContentScaleSupportTrait for View {
    unsafe fn setContentScaleFactor(&self, scale: f32) -> tresult {
        if !scale.is_finite() || scale <= 0.0 {
            return kInvalidArgument;
        }
        if std::env::var_os("VST_AUDIO_REJECT_SCALE").is_some() {
            return kResultFalse;
        }
        self.scale.set(scale);
        #[cfg(target_os = "linux")]
        let size = if self.wayland_frame.get() {
            (320, 200)
        } else {
            (
                (320.0 * scale).round() as i32,
                (200.0 * scale).round() as i32,
            )
        };
        #[cfg(not(target_os = "linux"))]
        let size = (
            (320.0 * scale).round() as i32,
            (200.0 * scale).round() as i32,
        );
        self.size.set(size);
        eprintln!("fixture VST3 scale={scale}, size={}x{}", size.0, size.1);
        if let Some(canvas) = self.canvas.borrow().as_ref() {
            canvas.resize(size.0, size.1);
        }
        // Wayland keeps logical 320x200 view units; the compositor maps the
        // existing buffer to output pixels. Resizing its buffer here would apply
        // the same scale twice.
        if let Some(frame) = self.frame.borrow().as_ref() {
            let mut rect = ViewRect {
                left: 0,
                top: 0,
                right: size.0,
                bottom: size.1,
            };
            // SAFETY: retained UI-thread frame and this live view's borrowed COM pointer.
            let _ = unsafe { frame.resizeView(self.view_pointer.get(), &mut rect) };
        }
        kResultOk
    }
}
struct Factory;
impl Class for Factory {
    type Interfaces = (IPluginFactory2,);
}
impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        if info.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: factory-info output is a writable SDK structure.
        let info = unsafe { &mut *info };
        narrow("Test fixtures", &mut info.vendor);
        narrow("", &mut info.url);
        narrow("", &mut info.email);
        info.flags = 0;
        kResultOk
    }
    unsafe fn countClasses(&self) -> i32 {
        1
    }
    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        if index != 0 || info.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: index zero output is a writable SDK class-info structure.
        let info = unsafe { &mut *info };
        info.cid = CID;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
        narrow("Audio Module Class", &mut info.category);
        narrow("Test matrix", &mut info.name);
        kResultOk
    }
    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        object: *mut *mut c_void,
    ) -> tresult {
        if cid.is_null() || iid.is_null() || object.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: required writable interface output initialized even on failure.
        unsafe { *object = std::ptr::null_mut() };
        // SAFETY: SDK class ID contains 16 bytes; read_unaligned avoids alignment assumptions.
        let id = unsafe { cid.cast::<TUID>().read_unaligned() };
        if id != CID {
            return kInvalidArgument;
        }
        let instance = ComWrapper::new(Matrix::new())
            .to_com_ptr::<FUnknown>()
            .expect("component unknown");
        let pointer = instance.as_ptr();
        // SAFETY: retained COM object exposes its FUnknown vtable for queryInterface.
        let table = unsafe { (*pointer).vtbl };
        // SAFETY: the live COM instance retains its static FUnknown vtable.
        let vtable = unsafe { &*table };
        // SAFETY: caller supplies SDK IID and writable owned-interface output; query retains result.
        unsafe { (vtable.queryInterface)(pointer, iid as *const TUID, object) }
    }
}
impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
        if index != 0 || info.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: writable extended class-info output for the only factory class.
        let info = unsafe { &mut *info };
        info.cid = CID;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
        info.classFlags = 0;
        narrow("Audio Module Class", &mut info.category);
        narrow("Test matrix", &mut info.name);
        narrow("Fx|Spatial", &mut info.subCategories);
        narrow("Test fixtures", &mut info.vendor);
        narrow("1.0.0", &mut info.version);
        narrow("VST 3.8", &mut info.sdkVersion);
        kResultOk
    }
}
static MODULE_READY: AtomicBool = AtomicBool::new(false);
// SAFETY: unique Windows SDK module initialization export.
#[unsafe(no_mangle)]
pub extern "system" fn InitDll() -> bool {
    MODULE_READY.store(true, Ordering::SeqCst);
    true
}
// SAFETY: unique Windows SDK module shutdown export; objects already released.
#[unsafe(no_mangle)]
pub extern "system" fn ExitDll() -> bool {
    MODULE_READY.store(false, Ordering::SeqCst);
    true
}
// SAFETY: unique Linux SDK module entry, context is only checked, not dereferenced.
#[unsafe(no_mangle)]
pub extern "system" fn ModuleEntry(context: *mut c_void) -> bool {
    MODULE_READY.store(!context.is_null(), Ordering::SeqCst);
    !context.is_null()
}
// SAFETY: unique Linux SDK module exit.
#[unsafe(no_mangle)]
pub extern "system" fn ModuleExit() -> bool {
    MODULE_READY.store(false, Ordering::SeqCst);
    true
}
// SAFETY: unique macOS SDK module entry; requires the real CFBundle context.
#[unsafe(no_mangle)]
pub extern "system" fn bundleEntry(context: *mut c_void) -> bool {
    MODULE_READY.store(!context.is_null(), Ordering::SeqCst);
    !context.is_null()
}
// SAFETY: unique macOS SDK module exit.
#[unsafe(no_mangle)]
pub extern "system" fn bundleExit() -> bool {
    MODULE_READY.store(false, Ordering::SeqCst);
    true
}
// SAFETY: unique SDK factory export returning one owned COM reference.
#[unsafe(no_mangle)]
pub extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    if !MODULE_READY.load(Ordering::SeqCst) {
        return std::ptr::null_mut();
    }
    ComWrapper::new(Factory)
        .to_com_ptr::<IPluginFactory>()
        .expect("factory interface")
        .into_raw()
}
