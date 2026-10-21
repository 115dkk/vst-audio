// The generated C++ enum constants are i32 on Windows and u32 on Unix.
#![allow(clippy::unnecessary_cast)]
#![allow(unsafe_code, non_snake_case)]
//! Test-only COM implementation using vst3 0.3.0's generated interfaces.
use std::{
    cell::RefCell,
    ffi::{CStr, c_char, c_void},
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};
use vst3::{Class, ComPtr, ComRef, ComWrapper, Steinberg::Vst::*, Steinberg::*, uid};
const CID: TUID = uid(0x56534155, 0x61647631, 0x73796e74, 1);
const CID2: TUID = uid(0x56534155, 0x61647631, 0x6d756c74, 2);
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
struct Matrix {
    mode: AtomicU8,
    width: AtomicU64,
    active: AtomicBool,
    kind: u8,
    gate: AtomicU64,
    handler: RefCell<Option<ComPtr<IComponentHandler>>>,
}
impl Matrix {
    fn new(kind: u8) -> Self {
        Self {
            mode: AtomicU8::new(0),
            width: AtomicU64::new(1.0f64.to_bits()),
            active: AtomicBool::new(false),
            kind,
            gate: AtomicU64::new(0),
            handler: RefCell::new(None),
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
        if result != kResultOk || read != 2 || bytes[0] != 1 || bytes[1] > 4 {
            return kInvalidArgument;
        }
        if bytes[1] == 4 {
            std::process::abort();
        }
        self.mode.store(bytes[1], Ordering::Relaxed);
        kResultOk
    }
    unsafe fn save(&self, state: *mut IBStream) -> tresult {
        // SAFETY: live stream borrowed from SDK for synchronous main-thread state save.
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else {
            return kInvalidArgument;
        };
        let mut bytes = [1, self.mode.load(Ordering::Relaxed)];
        let mut written = 0;
        // SAFETY: stream reads two stack bytes synchronously, count output is writable.
        let result = unsafe { stream.write(bytes.as_mut_ptr().cast(), 2, &mut written) };
        if result == kResultOk && written == 2 {
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
        if !(0..=1).contains(&direction) {
            return 0;
        }
        if media == 1 {
            return 1;
        }
        if media != 0 {
            return 0;
        }
        if self.kind == 1 {
            2
        } else if direction == 0 {
            0
        } else {
            1
        }
    }
    unsafe fn getBusInfo(
        &self,
        media: i32,
        direction: i32,
        index: i32,
        info: *mut BusInfo,
    ) -> tresult {
        // SAFETY: same media/direction contract as this component's bus query.
        let count = unsafe { self.getBusCount(media, direction) };
        if index < 0 || index >= count || info.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: SDK provides a writable bus record for the enumerated index.
        let info = unsafe { &mut *info };
        info.mediaType = media;
        info.direction = direction;
        info.channelCount = if media == 1 {
            16
        } else if index == 0 {
            2
        } else {
            1
        };
        info.busType = i32::from(index != 0);
        info.flags = 1;
        wide(
            if index == 0 { "Main" } else { "Sidechain" },
            &mut info.name,
        );
        kResultOk
    }
    unsafe fn getRoutingInfo(&self, _: *mut RoutingInfo, _: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }
    unsafe fn activateBus(&self, media: i32, direction: i32, index: i32, _: TBool) -> tresult {
        // SAFETY: forwards the identical component bus query contract.
        if index >= 0 && index < unsafe { self.getBusCount(media, direction) } {
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
    unsafe fn setBusArrangements(&self, _: *mut u64, ni: i32, _: *mut u64, no: i32) -> tresult {
        if (self.kind == 0 && ni == 0 && no == 1) || (self.kind == 1 && ni == 2 && no == 2) {
            kResultOk
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
        // SAFETY: identical component query contract, no pointer arguments.
        let count = unsafe { self.getBusCount(0, direction) };
        if index < 0 || index >= count || arrangement.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: SDK gives one writable arrangement output.
        unsafe { *arrangement = if index == 0 { 3 } else { 4 } };
        kResultOk
    }
    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if (0..=1).contains(&size) {
            kResultOk
        } else {
            kResultFalse
        }
    }
    unsafe fn getLatencySamples(&self) -> u32 {
        0
    }
    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        if setup.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: SDK provides a readable setup record.
        let setup = unsafe { &*setup };
        if setup.maxSamplesPerBlock > 0 && (0..=1).contains(&setup.symbolicSampleSize) {
            kResultOk
        } else {
            kInvalidArgument
        }
    }
    unsafe fn setProcessing(&self, _: TBool) -> tresult {
        kResultOk
    }
    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        if data.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: SDK provides a readable block descriptor and its live buffers.
        let data = unsafe { &*data };
        if data.numSamples < 0 || data.processContext.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: host supplied processing context for this block.
        let context = unsafe { &*data.processContext };
        // SAFETY: optional event list is borrowed only through this call.
        let events = unsafe { ComRef::from_raw(data.inputEvents) };
        // SAFETY: optional output event list is borrowed for synchronous output.
        let output_events = unsafe { ComRef::from_raw(data.outputEvents) };
        // SAFETY: zero is a valid initial representation of the plain Event union.
        let empty: Event = unsafe { std::mem::zeroed() };
        let mut notes = [empty; 256];
        let mut count = 0;
        if let Some(events) = events {
            // SAFETY: live list owned by the host.
            count = unsafe { events.getEventCount() }.clamp(0, 256) as usize;
            for (index, event) in notes.iter_mut().enumerate().take(count) {
                // SAFETY: validated list index and writable Event output.
                let _ = unsafe { events.getEvent(index as i32, event) };
                if let Some(out) = &output_events {
                    let mut echo = *event;
                    if echo.r#type == 3 {
                        // SAFETY: the poly-pressure tag selects this union member.
                        let pressure = unsafe { echo.__field0.polyPressure };
                        echo.r#type = 65535;
                        echo.__field0.midiCCOut = LegacyMIDICCOutEvent {
                            controlNumber: 131,
                            channel: pressure.channel as i8,
                            value: pressure.pitch as i8,
                            value2: (pressure.pressure * 127.0).round() as i8,
                        };
                    }
                    // SAFETY: copied event and any data payload remain readable until this callback returns.
                    let _ = unsafe { out.addEvent(&mut echo) };
                }
            }
        }
        let mut points = [(0, 0.0); 256];
        let mut point_count = 0;
        // SAFETY: optional parameter list is borrowed for the duration of process.
        if let Some(changes) = unsafe { ComRef::from_raw(data.inputParameterChanges) } {
            // SAFETY: live host parameter list.
            let n = unsafe { changes.getParameterCount() }.clamp(0, 256);
            for i in 0..n {
                // SAFETY: enumerated parameter queue index.
                let pointer = unsafe { changes.getParameterData(i) };
                // SAFETY: nullable borrowed queue from the host.
                if let Some(queue) = unsafe { ComRef::from_raw(pointer) } {
                    // SAFETY: live queue accessor.
                    let id = unsafe { queue.getParameterId() };
                    // SAFETY: live queue accessor.
                    let n = unsafe { queue.getPointCount() }.clamp(0, 256);
                    if id != 0 {
                        continue;
                    }
                    for j in 0..n {
                        if point_count == 256 {
                            break;
                        }
                        let mut offset = 0;
                        let mut value = 0.0;
                        // SAFETY: enumerated point and writable outputs.
                        if unsafe { queue.getPoint(j, &mut offset, &mut value) } == kResultOk {
                            points[point_count] = (offset, value);
                            point_count += 1;
                        }
                    }
                }
            }
        }
        for frame in 0..data.numSamples as usize {
            for (offset, value) in &points[..point_count] {
                if *offset == frame as i32 {
                    self.width.store(value.to_bits(), Ordering::Relaxed);
                }
            }
            for event in &notes[..count] {
                if event.sampleOffset != frame as i32 {
                    continue;
                }
                if event.r#type == 0 {
                    // SAFETY: NoteOn tag selects the union member.
                    self.gate.store(
                        f64::from(unsafe { event.__field0.noteOn }.velocity).to_bits(),
                        Ordering::Relaxed,
                    );
                } else if event.r#type == 1 {
                    self.gate.store(0, Ordering::Relaxed);
                }
            }
            let gain = f64::from_bits(self.width.load(Ordering::Relaxed));
            let gate = f64::from_bits(self.gate.load(Ordering::Relaxed));
            if self.kind == 0 {
                for channel in 0..2 {
                    let value = if channel == 0 {
                        0.123456789012345 * gate * gain
                    } else {
                        (context.projectTimeSamples + frame as i64) as f64 * 0.000001 * gate
                    };
                    // SAFETY: fixture declares one stereo output; helper validates bus/channel presence.
                    unsafe { write_sample(data, 0, channel, frame, value) };
                }
            } else {
                // SAFETY: fixture declared two input buses; helper handles inactive sidechains.
                let side = unsafe { read_sample(data, 1, 0, frame) };
                // SAFETY: main stereo input channel zero.
                let left = unsafe { read_sample(data, 0, 0, frame) };
                // SAFETY: main stereo input channel one.
                let right = unsafe { read_sample(data, 0, 1, frame) };
                // SAFETY: main output left is declared by this component.
                unsafe { write_sample(data, 0, 0, frame, left * gain + side * 2.0) };
                // SAFETY: main output right is declared by this component.
                unsafe { write_sample(data, 0, 1, frame, right * gain) };
                // SAFETY: auxiliary output may be inactive; helper checks before writing.
                unsafe { write_sample(data, 1, 0, frame, side * 3.0) };
            }
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
        if !(0..2).contains(&index) || info.is_null() {
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
        ComWrapper::new(View {
            handler: self.handler.borrow().clone(),
        })
        .to_com_ptr::<IPlugView>()
        .expect("view interface")
        .into_raw()
    }
}
struct View {
    handler: Option<ComPtr<IComponentHandler>>,
}
impl Class for View {
    type Interfaces = (IPlugView,);
}
impl IPlugViewTrait for View {
    unsafe fn isPlatformTypeSupported(&self, platform: FIDString) -> tresult {
        if platform.is_null() {
            return kInvalidArgument;
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
        // A deterministic editor gesture exercises the host's lock-free automation path.
        if let Some(handler) = &self.handler {
            // SAFETY: attached is on UI thread; retained host handler and known parameter zero.
            let _ = unsafe { handler.beginEdit(0) };
            // SAFETY: UI-thread normalized edit for Width; processor receives it via queue.
            let _ = unsafe { handler.performEdit(0, 0.5) };
            // SAFETY: finishes the preceding UI-thread edit gesture.
            let _ = unsafe { handler.endEdit(0) };
        }
        kResultOk
    }
    unsafe fn removed(&self) -> tresult {
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
                left: 0,
                top: 0,
                right: 320,
                bottom: 200,
            }
        };
        kResultOk
    }
    unsafe fn onSize(&self, _: *mut ViewRect) -> tresult {
        kResultOk
    }
    unsafe fn onFocus(&self, _: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setFrame(&self, _frame: *mut IPlugFrame) -> tresult {
        #[cfg(target_os = "linux")]
        if !_frame.is_null() {
            // SAFETY: setFrame provides a live frame, or null during detach.
            let frame = unsafe { ComRef::from_raw(_frame) }.expect("checked frame");
            if frame.cast::<Linux::IRunLoop>().is_none() {
                return kNoInterface;
            }
        }
        kResultOk
    }
    unsafe fn canResize(&self) -> tresult {
        kResultFalse
    }
    unsafe fn checkSizeConstraint(&self, _: *mut ViewRect) -> tresult {
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
        2
    }
    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        if !(0..2).contains(&index) || info.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: index zero output is a writable SDK class-info structure.
        let info = unsafe { &mut *info };
        info.cid = if index == 0 { CID } else { CID2 };
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
        narrow("Audio Module Class", &mut info.category);
        narrow(
            if index == 0 {
                "Test synth"
            } else {
                "Test sidechain"
            },
            &mut info.name,
        );
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
        if id != CID && id != CID2 {
            return kInvalidArgument;
        }
        let instance = ComWrapper::new(Matrix::new(u8::from(id == CID2)))
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
        if !(0..2).contains(&index) || info.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: writable extended class-info output for the only factory class.
        let info = unsafe { &mut *info };
        info.cid = if index == 0 { CID } else { CID2 };
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
        info.classFlags = 0;
        narrow("Audio Module Class", &mut info.category);
        narrow(
            if index == 0 {
                "Test synth"
            } else {
                "Test sidechain"
            },
            &mut info.name,
        );
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

unsafe fn read_sample(data: &ProcessData, bus: usize, channel: usize, frame: usize) -> f64 {
    if bus >= data.numInputs as usize || data.inputs.is_null() {
        return 0.0;
    }
    // SAFETY: enumerated bus lies in the host-provided array.
    let bus = unsafe { &*data.inputs.wrapping_add(bus) };
    if channel >= bus.numChannels as usize {
        return 0.0;
    }
    if data.symbolicSampleSize == 0 {
        // SAFETY: configured sample precision selects channelBuffers32.
        let pointers = unsafe { bus.__field0.channelBuffers32 };
        // SAFETY: declared channel pointer exists and frame is within numSamples.
        let pointer = unsafe { *pointers.wrapping_add(channel) };
        // SAFETY: this frame is readable before in-place output writes.
        f64::from(unsafe { *pointer.wrapping_add(frame) })
    } else {
        // SAFETY: configured sample precision selects channelBuffers64.
        let pointers = unsafe { bus.__field0.channelBuffers64 };
        // SAFETY: channel is declared and frame is in range.
        let pointer = unsafe { *pointers.wrapping_add(channel) };
        // SAFETY: read the input before overwriting in place.
        unsafe { *pointer.wrapping_add(frame) }
    }
}
unsafe fn write_sample(data: &ProcessData, bus: usize, channel: usize, frame: usize, value: f64) {
    if bus >= data.numOutputs as usize || data.outputs.is_null() {
        return;
    }
    // SAFETY: bus index is within the host's declared array.
    let bus = unsafe { &*data.outputs.wrapping_add(bus) };
    if channel >= bus.numChannels as usize {
        return;
    }
    if data.symbolicSampleSize == 0 {
        // SAFETY: configured f32 precision selects this union member.
        let pointers = unsafe { bus.__field0.channelBuffers32 };
        // SAFETY: output channel index is declared.
        let pointer = unsafe { *pointers.wrapping_add(channel) };
        // SAFETY: frame is within the host's writable output buffer.
        unsafe { *pointer.wrapping_add(frame) = value as f32 };
    } else {
        // SAFETY: configured f64 precision selects this union member.
        let pointers = unsafe { bus.__field0.channelBuffers64 };
        // SAFETY: output channel index is declared.
        let pointer = unsafe { *pointers.wrapping_add(channel) };
        // SAFETY: frame is within the writable output buffer.
        unsafe { *pointer.wrapping_add(frame) = value };
    }
}
