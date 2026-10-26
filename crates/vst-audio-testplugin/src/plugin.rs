#![allow(unsafe_code)]
use std::{
    cell::UnsafeCell,
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};
use vst_audio_worker_sys::{
    vst2::*,
    vst2_events::{Events, TimeInfo},
};

mod mode {
    pub const MATRIX: u32 = 0;
    pub const CRASH_PROCESS: u32 = 1;
    pub const HANG_PROCESS: u32 = 2;
    pub const NAN_CENTER_OUTPUT: u32 = 3;
    pub const CRASH_STATE_RESTORE: u32 = 4;
    pub const HANG_STATE_RESTORE: u32 = 5;
    pub const SANDBOX_PROBE: u32 = 6;
    pub const OBSERVE_HOST_CALLBACKS: u32 = 7;
    pub const STATE_RESTORE_LATENCY_CHANGE: u32 = 8;
    pub const STATE_RESTORE_OUTPUT_COUNT_CHANGE: u32 = 9;
    pub const PARAMETER_REFRESH_AND_GESTURE: u32 = 10;
    pub const RESUME_LATENCY_CHANGE: u32 = 11;
    pub const OPAQUE_SSV3_STATE: u32 = 12;
    pub const RESIZE_ON_EDITOR_OPEN: u32 = 13;
    pub const RESIZE_ON_EDITOR_IDLE: u32 = 14;
    pub const SLOW_EDITOR_IDLE: u32 = 15;
    pub const CONFIGURE_LATENCY_CHANGE: u32 = 16;
    pub const AUDIO_OUTPUT_COUNT_CHANGE: u32 = 17;
    pub const RESIZE_ON_EDITOR_CLOSE: u32 = 18;
    pub const NO_DPI_SCALING: u32 = 19;
    pub const DROP_SCALE_BEFORE_EDITOR_OPEN: u32 = 20;
    pub const SHORT_MIDI_HEADER: u32 = 21;
    pub const PARAMETER_ADDED_AT_OPEN: u32 = 22;
    pub const NAN_PARAMETER_GETTER: u32 = 23;
}

mod observation {
    pub const HOST_CAPABILITIES: usize = 0;
    pub const OPEN_SAMPLE_RATE: usize = 1;
    pub const RESUME_SAMPLE_RATE: usize = 2;
    pub const AUDIO_PROCESS_LEVEL: usize = 3;
    pub const EDITOR_IDLE_PROCESS_LEVEL: usize = 4;
    pub const SLOW_IDLE_PHASE: usize = 5;
    pub const PROCESS_CALLS: usize = 6;
    pub const PROCESS_CALLS_DURING_IDLE: usize = 7;
    pub const PROCESS_CALLS_AFTER_OUTPUT_COUNT_CHANGE: usize = 8;
    pub const AUDIO_OUTPUT_COUNT_CHANGED: usize = 9;
    pub const CLOSE_RESIZE_ACCEPTED: usize = 10;
    pub const EDITOR_SCALE_BITS: usize = 11;
    // Slots 12..24 remain zero and are still serialized in observation chunks.
    pub const COUNT: usize = 24;
}

#[repr(C)]
struct Instance {
    effect: AEffect,
    host: Callback,
    width: AtomicU32,
    mode: AtomicU32,
    active: AtomicBool,
    observations: [AtomicU32; observation::COUNT],
    shell: bool,
    shell_children: usize,
    child: i32,
    audio_automate: bool,
    audio_automated: AtomicBool,
    flood_edits: bool,
    control: UnsafeCell<Control>,
    audio: UnsafeCell<Audio>,
}
struct Control {
    chunk: [u8; 128],
    input: Arrangement,
    output: Arrangement,
    rect: Rect,
    canvas: Option<crate::visual::Canvas>,
    next_child: usize,
    idle_once: bool,
}
struct Audio {
    delay: [[f32; 64]; 6],
    cursor: usize,
}

// SAFETY: unique VST2 entry symbol returns an allocation released only by CLOSE.
#[unsafe(no_mangle)]
pub extern "C" fn VSTPluginMain(host: Callback) -> *mut AEffect {
    let shell = std::env::var_os("VST_AUDIO_TEST_SHELL").is_some();
    let shell_children = std::env::var("VST_AUDIO_TEST_SHELL_CHILDREN")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2);
    // SAFETY: current ID callback is valid without an effect during entry.
    let child = unsafe { host(std::ptr::null_mut(), 2, 0, 0, std::ptr::null_mut(), 0.0) } as i32;
    let no_chunks = std::env::var_os("VST_AUDIO_TEST_NO_CHUNKS").is_some();
    let open_counts = std::env::var_os("VST_AUDIO_TEST_OPEN_COUNTS").is_some();
    // JUCE declares its widest channel count and reports a narrower current layout.
    let narrow = std::env::var_os("VST_AUDIO_TEST_NARROW_ARRANGEMENT").is_some();
    let instance = Box::new(Instance {
        effect: AEffect {
            dispatcher: Some(dispatch),
            set_parameter: Some(set_parameter),
            get_parameter: Some(get_parameter),
            num_programs: if no_chunks { 0 } else { 1 },
            num_params: 1,
            num_inputs: 2,
            num_outputs: if open_counts { 2 } else { 6 },
            flags: HAS_EDITOR | REPLACING | if no_chunks { 0 } else { PROGRAM_CHUNKS },
            initial_delay: 64,
            unique_id: if child != 0 {
                child
            } else {
                i32::from_be_bytes(*b"SSTP")
            },
            version: 1,
            process_replacing: Some(process),
            ..AEffect::default()
        },
        host,
        width: AtomicU32::new(1.0f32.to_bits()),
        mode: AtomicU32::new(
            // Select mode 22 through currentEffectID so it is active before OPEN,
            // without an extra environment variable in the native test runner.
            if child == mode::PARAMETER_ADDED_AT_OPEN as i32 {
                mode::PARAMETER_ADDED_AT_OPEN
            } else if std::env::var_os("VST_AUDIO_TEST_CONFIGURE_LATENCY").is_some() {
                mode::CONFIGURE_LATENCY_CHANGE
            } else {
                mode::MATRIX
            },
        ),
        active: AtomicBool::new(false),
        observations: std::array::from_fn(|_| AtomicU32::new(0)),
        shell,
        shell_children,
        child,
        audio_automate: std::env::var_os("VST_AUDIO_TEST_AUDIO_AUTOMATE").is_some(),
        audio_automated: AtomicBool::new(false),
        flood_edits: std::env::var_os("VST_AUDIO_TEST_EDIT_FLOOD").is_some(),
        control: UnsafeCell::new(Control {
            chunk: [0; 128],
            input: Arrangement::new(ARR_STEREO, &[1, 2]),
            output: if narrow {
                Arrangement::new(ARR_STEREO, &[1, 2])
            } else {
                Arrangement::new(ARR_51, &[1, 2, 3, 4, 5, 6])
            },
            rect: Rect {
                top: 0,
                left: 0,
                bottom: 200,
                right: 320,
            },
            canvas: None,
            next_child: 0,
            idle_once: false,
        }),
        audio: UnsafeCell::new(Audio {
            delay: [[0.0; 64]; 6],
            cursor: 0,
        }),
    });
    Box::into_raw(instance).cast()
}
// Only disjoint field references are formed: the foreign header may be mutated
// under lifecycle admission while DSP owns audio and UI owns control.
unsafe fn host(
    effect: *mut AEffect,
    opcode: i32,
    index: i32,
    value: isize,
    pointer: *mut c_void,
) -> isize {
    // SAFETY: live instance, immutable callback pointer copied without borrowing its header.
    let callback = unsafe { (*effect.cast::<Instance>()).host };
    // SAFETY: payload and opcode are supplied by fixture callers using the host ABI.
    unsafe { callback(effect, opcode, index, value, pointer, 0.0) }
}
unsafe fn observe_time(effect: *mut AEffect, slot: usize) {
    // SAFETY: getTime has no payload and returns a thread-local record.
    let time = unsafe { host(effect, 7, 0, 0, std::ptr::null_mut()) } as *const TimeInfo;
    if !time.is_null() {
        // SAFETY: record is live until the next getTime on this thread.
        let rate = unsafe { (*time).sample_rate } as u32;
        // SAFETY: observation atomics outlive calls and are independent of the mutable header.
        let observations = unsafe { &(*effect.cast::<Instance>()).observations };
        observations[slot].store(rate, Ordering::Relaxed);
    }
}
unsafe extern "C" fn dispatch(
    effect: *mut AEffect,
    opcode: i32,
    index: i32,
    value: isize,
    pointer: *mut c_void,
    option: f32,
) -> isize {
    if effect.is_null() {
        return 0;
    }
    if opcode == CLOSE {
        // SAFETY: audio has joined and editor is closed before the sole allocation release.
        drop(unsafe { Box::from_raw(effect.cast::<Instance>()) });
        return 1;
    }
    // processEvents (25) arrives on the audio thread while the control thread may
    // hold the UI state below; this fixture ignores events, so it returns before
    // borrowing anything the control thread owns.
    if opcode == 25 {
        return 0;
    }
    let raw = effect.cast::<Instance>();
    // SAFETY: control dispatcher calls occur on one thread; audio never accesses this field.
    let control = unsafe { (*raw).control.get() };
    // SAFETY: only this control thread accesses the control allocation.
    let control = unsafe { &mut *control };
    // SAFETY: mode is atomic and remains live until CLOSE.
    let mode = unsafe { &(*raw).mode };
    // SAFETY: independent atomics may be accessed by control and audio concurrently.
    let observations = unsafe { &(*raw).observations };
    match opcode {
        OPEN => {
            // SAFETY: OPEN is gated, setting the final advertised output count before buffer creation.
            unsafe { (*effect).num_outputs = 6 };
            if mode.load(Ordering::Relaxed) == mode::PARAMETER_ADDED_AT_OPEN {
                // Audit #11 TD-09: the plugin violates the entry-time parameter
                // declaration by adding ID 1 at OPEN; its edit comes later.
                // SAFETY: OPEN excludes processing and concurrent header readers.
                unsafe { (*effect).num_params = 2 };
            }
            let names = [
                c"sendVstEvents",
                c"sendVstMidiEvent",
                c"receiveVstEvents",
                c"receiveVstMidiEvent",
                c"sendVstTimeInfo",
                c"sizeWindow",
                c"startStopProcess",
                c"shellCategory",
                c"acceptIOChanges",
                c"offline",
                c"openFileSelector",
                c"closeFileSelector",
                c"editFile",
                c"reportConnectionChanges",
                c"sendVstMidiEventFlagIsRealtime",
            ];
            let mut answers = 0;
            for (i, name) in names.iter().enumerate() {
                // SAFETY: capability name is a terminated static string.
                if unsafe { host(effect, 37, 0, 0, name.as_ptr().cast_mut().cast()) } == 1 {
                    answers |= 1 << i;
                }
            }
            observations[observation::HOST_CAPABILITIES].store(answers, Ordering::Relaxed);
            // SAFETY: live effect during OPEN.
            unsafe { observe_time(effect, observation::OPEN_SAMPLE_RATE) };
            1
        }
        SET_SAMPLE_RATE => {
            if mode.load(Ordering::Relaxed) == mode::CONFIGURE_LATENCY_CHANGE {
                // SAFETY: configure excludes processing and header inspection.
                unsafe { (*effect).initial_delay = 96 };
            }
            1
        }
        SET_BLOCK_SIZE | START_PROCESS | STOP_PROCESS | SET_PROGRAM => 1,
        EDIT_IDLE => {
            // SAFETY: process-level query has no payload.
            let level = unsafe { host(effect, 23, 0, 0, std::ptr::null_mut()) } as u32;
            observations[observation::EDITOR_IDLE_PROCESS_LEVEL].store(level, Ordering::Relaxed);
            if !control.idle_once {
                control.idle_once = true;
                if mode.load(Ordering::Relaxed) == mode::RESIZE_ON_EDITOR_IDLE {
                    // SAFETY: valid requested editor dimensions, editor is open.
                    unsafe { host(effect, MASTER_SIZE_WINDOW, 500, 280, std::ptr::null_mut()) };
                }
                if mode.load(Ordering::Relaxed) == mode::SLOW_EDITOR_IDLE {
                    observations[observation::SLOW_IDLE_PHASE].store(1, Ordering::Release);
                    let before = observations[observation::PROCESS_CALLS].load(Ordering::Relaxed);
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    observations[observation::PROCESS_CALLS_DURING_IDLE].store(
                        observations[observation::PROCESS_CALLS].load(Ordering::Relaxed) - before,
                        Ordering::Relaxed,
                    );
                    observations[observation::SLOW_IDLE_PHASE].store(2, Ordering::Release);
                }
            }
            1
        }
        EDIT_OPEN => {
            // SAFETY: editor opening reports one valid parameter gesture synchronously.
            unsafe { host(effect, 43, 0, 0, std::ptr::null_mut()) };
            // SAFETY: immutable fixture option remains live through the editor call.
            let flood_edits = unsafe { (*raw).flood_edits };
            let edits = if flood_edits { 5000 } else { 1 };
            for step in 0..edits {
                let value = if edits == 1 {
                    0.5
                } else {
                    0.75 * step as f32 / (edits - 1) as f32
                };
                // SAFETY: immutable callback pointer remains live until CLOSE.
                let callback = unsafe { (*raw).host };
                // SAFETY: automate carries no borrowed payload.
                unsafe { callback(effect, MASTER_AUTOMATE, 0, 0, std::ptr::null_mut(), value) };
            }
            // SAFETY: editor opening closes the same valid parameter gesture.
            unsafe { host(effect, 44, 0, 0, std::ptr::null_mut()) };
            if mode.load(Ordering::Relaxed) == mode::RESIZE_ON_EDITOR_OPEN {
                control.rect.right = 480;
                control.rect.bottom = 260;
            }
            control.idle_once = false;
            // SAFETY: platform parent supplied by host, used on main thread only.
            control.canvas = unsafe {
                crate::visual::Canvas::attach(
                    pointer,
                    i32::from(control.rect.right - control.rect.left),
                    i32::from(control.rect.bottom - control.rect.top),
                )
            };
            isize::from(control.canvas.is_some())
        }
        EDIT_CLOSE => {
            control.canvas = None;
            if mode.load(Ordering::Relaxed) == mode::RESIZE_ON_EDITOR_CLOSE {
                // SAFETY: callback has no pointer payload; a closing editor must reject resize.
                let accepted =
                    unsafe { host(effect, MASTER_SIZE_WINDOW, 640, 360, std::ptr::null_mut()) };
                observations[observation::CLOSE_RESIZE_ACCEPTED]
                    .store(accepted as u32, Ordering::Relaxed);
            }
            1
        }
        GET_PROGRAM => 0,
        MAINS_CHANGED => {
            // SAFETY: lifecycle flag is atomic and live.
            unsafe { &(*raw).active }.store(value != 0, Ordering::Relaxed);
            if value != 0 {
                // SAFETY: resume is gated and getTime returns a valid snapshot.
                unsafe { observe_time(effect, observation::RESUME_SAMPLE_RATE) };
                if mode.load(Ordering::Relaxed) == mode::RESUME_LATENCY_CHANGE {
                    // SAFETY: resume excludes processing and header readers.
                    unsafe { (*effect).initial_delay = 192 };
                }
            }
            1
        }
        35 => {
            // SAFETY: shell and child are immutable after entry.
            let shell = unsafe { (*raw).shell };
            // SAFETY: child is immutable after entry.
            let child = unsafe { (*raw).child };
            let shell = shell && child == 0;
            if shell { 10 } else { 1 }
        }
        50 if index == 0x50726553 && value == 0x41654373 && pointer.is_null() => {
            if !option.is_finite() || option <= 0.0 {
                return 0;
            }
            // Mode 20 behaves like JUCE 6.0.0 to 6.1.2: a scale that arrives
            // before the editor exists is acknowledged and dropped.
            if mode.load(Ordering::Relaxed) == mode::DROP_SCALE_BEFORE_EDITOR_OPEN
                && control.canvas.is_none()
            {
                return 1;
            }
            let scale = f64::from(option);
            let width = (320.0 * scale).round().clamp(1.0, f64::from(i16::MAX)) as i16;
            let height = (200.0 * scale).round().clamp(1.0, f64::from(i16::MAX)) as i16;
            control.rect.right = control.rect.left.saturating_add(width);
            control.rect.bottom = control.rect.top.saturating_add(height);
            observations[observation::EDITOR_SCALE_BITS].store(option.to_bits(), Ordering::Relaxed);
            eprintln!("fixture VST2 scale={option}, size={width}x{height}");
            if let Some(canvas) = control.canvas.as_ref() {
                canvas.resize(i32::from(width), i32::from(height));
            }
            1
        }
        51 => {
            if mode.load(Ordering::Relaxed) == mode::NO_DPI_SCALING || pointer.is_null() {
                0
            } else {
                // SAFETY: canDo supplies a terminated capability name.
                isize::from(
                    unsafe { std::ffi::CStr::from_ptr(pointer.cast()) }
                        == c"supportsViewDpiScaling",
                )
            }
        }
        70 => {
            // SAFETY: configured child count is immutable after entry.
            let shell_children = unsafe { (*raw).shell_children };
            if pointer.is_null() || control.next_child >= shell_children {
                return 0;
            }
            let i = control.next_child;
            control.next_child += 1;
            let (id, name): (i32, &[u8]) = match i {
                0 => (i32::from_be_bytes(*b"SH01"), b"Shell half\0"),
                1 => (i32::from_be_bytes(*b"SH02"), b"Shell quarter\0"),
                _ => (i as i32 + 1, b"Shell child\0"),
            };
            // SAFETY: shell enumeration provides at least 32 writable name bytes.
            unsafe { std::ptr::copy_nonoverlapping(name.as_ptr(), pointer.cast(), name.len()) };
            id as isize
        }
        GET_EFFECT_NAME | GET_VENDOR_STRING | PARAM_NAME => {
            let text: &[u8] = match opcode {
                GET_EFFECT_NAME => b"Test matrix\0",
                GET_VENDOR_STRING => b"Test fixtures\0",
                _ => b"Width\0",
            };
            if pointer.is_null() {
                return 0;
            }
            // SAFETY: text opcodes provide a sufficient writable string buffer.
            unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), pointer.cast(), text.len()) };
            1
        }
        GET_SPEAKER_ARRANGEMENT => {
            if value == 0 || pointer.is_null() {
                return 0;
            }
            // SAFETY: arrangement query provides these two pointer outputs.
            unsafe { *(value as *mut *mut Arrangement) = &mut control.input };
            // SAFETY: output pointer is writable for one pointer.
            unsafe { *pointer.cast::<*mut Arrangement>() = &mut control.output };
            1
        }
        SET_SPEAKER_ARRANGEMENT => 1,
        GET_CHUNK => {
            // Wire layout: normally [1, mode], followed by mode-specific bytes.
            // Mode 6 returns 5 bytes: the header plus TCP/file/process probe bytes.
            // Mode 12 returns 8 stored bytes without rewriting them; an 8-byte
            // SSV3-prefixed SET_CHUNK payload therefore round-trips exactly.
            // Other modes >= 7 return 98 bytes: header + 24 little-endian u32 slots
            // (slot i occupies 2 + i*4 .. 6 + i*4; unused slots remain zero).
            // All remaining modes return just the 2-byte header.
            if pointer.is_null() {
                return 0;
            }
            let m = mode.load(Ordering::Relaxed) as u8;
            if m != mode::OPAQUE_SSV3_STATE as u8 {
                control.chunk[..2].copy_from_slice(&[1, m]);
            }
            let size = if m == mode::SANDBOX_PROBE as u8 {
                5
            } else if m == mode::OPAQUE_SSV3_STATE as u8 {
                8
            } else if m >= mode::OBSERVE_HOST_CALLBACKS as u8 {
                for (i, observed) in observations.iter().enumerate() {
                    control.chunk[2 + i * 4..6 + i * 4]
                        .copy_from_slice(&observed.load(Ordering::Acquire).to_le_bytes());
                }
                98
            } else {
                2
            };
            // SAFETY: chunk pointer output remains valid until next control call.
            unsafe { *pointer.cast::<*const u8>() = control.chunk.as_ptr() };
            size
        }
        SET_CHUNK => {
            if value < 2 || pointer.is_null() {
                return 0;
            }
            // SAFETY: synchronous SET_CHUNK supplies value readable bytes.
            let chunk = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), value as usize) };
            if chunk.starts_with(b"SSV3") && chunk.len() == 8 {
                control.chunk[..8].copy_from_slice(chunk);
                mode.store(mode::OPAQUE_SSV3_STATE, Ordering::Relaxed);
                return 1;
            }
            if chunk[0] != 1 || chunk[1] > mode::SHORT_MIDI_HEADER as u8 {
                return 0;
            }
            if chunk[1] == mode::HANG_STATE_RESTORE as u8 {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
            if chunk[1] == mode::CRASH_STATE_RESTORE as u8 {
                std::process::abort();
            }
            mode.store(u32::from(chunk[1]), Ordering::Relaxed);
            if chunk[1] == mode::SANDBOX_PROBE as u8 {
                control.chunk[2..5].copy_from_slice(&crate::probe::run());
            }
            if chunk[1] == mode::STATE_RESTORE_LATENCY_CHANGE as u8 {
                // SAFETY: state restore is gated; header mutation precedes callback notification.
                unsafe { (*effect).initial_delay = 128 };
                // SAFETY: ioChanged has no pointer payload.
                unsafe { host(effect, MASTER_IO_CHANGED, 0, 0, std::ptr::null_mut()) };
            }
            if chunk[1] == mode::STATE_RESTORE_OUTPUT_COUNT_CHANGE as u8 {
                // SAFETY: state restore excludes DSP and marks the incompatible layout before return.
                unsafe { (*effect).num_outputs = 4 };
                // SAFETY: ioChanged has no payload.
                unsafe { host(effect, MASTER_IO_CHANGED, 0, 0, std::ptr::null_mut()) };
            }
            if chunk[1] == mode::PARAMETER_REFRESH_AND_GESTURE as u8 {
                // SAFETY: updateDisplay has no payload.
                unsafe { host(effect, 42, 0, 0, std::ptr::null_mut()) };
                // SAFETY: beginEdit addresses valid parameter zero.
                unsafe { host(effect, 43, 0, 0, std::ptr::null_mut()) };
                // SAFETY: endEdit addresses valid parameter zero.
                unsafe { host(effect, 44, 0, 0, std::ptr::null_mut()) };
            }
            1
        }
        EDIT_GET_RECT => {
            if pointer.is_null() {
                return 0;
            }
            // SAFETY: opcode supplies rectangle pointer output; control rect remains live.
            unsafe { *pointer.cast::<*mut Rect>() = &mut control.rect };
            1
        }
        _ => 0,
    }
}
unsafe extern "C" fn set_parameter(effect: *mut AEffect, index: u32, value: f32) {
    if effect.is_null() || index != 0 {
        return;
    }
    // SAFETY: field is atomic; no reference to the foreign header or DSP state is formed.
    unsafe { &(*effect.cast::<Instance>()).width }.store(value.to_bits(), Ordering::Relaxed);
    // SAFETY: the live instance retains its immutable child ID through CLOSE.
    let child = unsafe { (*effect.cast::<Instance>()).child };
    if child == mode::NAN_PARAMETER_GETTER as i32 && value == 0.75 {
        // Audit #11 TD-36: switch to a violating getter only after a valid save.
        // SAFETY: mode is an independent atomic retained until CLOSE.
        unsafe { &(*effect.cast::<Instance>()).mode }
            .store(mode::NAN_PARAMETER_GETTER, Ordering::Relaxed);
    }
    // SAFETY: the live instance retains its immutable callback through CLOSE.
    let callback = unsafe { (*effect.cast::<Instance>()).host };
    // SAFETY: automate addresses parameter zero and carries no borrowed payload.
    unsafe { callback(effect, MASTER_AUTOMATE, 0, 0, std::ptr::null_mut(), value) };
}
unsafe extern "C" fn get_parameter(effect: *mut AEffect, index: u32) -> f32 {
    if effect.is_null() || index != 0 {
        return 0.0;
    }
    // SAFETY: mode is an independent atomic retained until CLOSE.
    if unsafe { &(*effect.cast::<Instance>()).mode }.load(Ordering::Relaxed)
        == mode::NAN_PARAMETER_GETTER
    {
        // Audit #11 TD-36: a VST2 getter violates the normalized-value contract.
        return f32::NAN;
    }
    // SAFETY: atomic parameter is live throughout the call.
    f32::from_bits(unsafe { &(*effect.cast::<Instance>()).width }.load(Ordering::Relaxed))
}
unsafe extern "C" fn process(
    effect: *mut AEffect,
    inputs: *const *const f32,
    outputs: *mut *mut f32,
    frames: i32,
) {
    if effect.is_null() || inputs.is_null() || outputs.is_null() || frames <= 0 {
        return;
    }
    let raw = effect.cast::<Instance>();
    // SAFETY: atomic activation is live; lifecycle changes are gated.
    if !unsafe { &(*raw).active }.load(Ordering::Relaxed) {
        return;
    }
    // SAFETY: audio is accessed exclusively by the single processor thread.
    let audio = unsafe { (*raw).audio.get() };
    // SAFETY: only the unique processing thread accesses audio scratch.
    let audio = unsafe { &mut *audio };
    // SAFETY: mode is an independent atomic.
    let mode = unsafe { &(*raw).mode }.load(Ordering::Relaxed);
    // SAFETY: observation counters are atomic and remain allocated until CLOSE.
    let observations = unsafe { &(*raw).observations };
    observations[observation::PROCESS_CALLS].fetch_add(1, Ordering::Relaxed);
    // SAFETY: immutable fixture mode remains live through processing.
    let audio_automate = unsafe { (*raw).audio_automate };
    // SAFETY: atomic once flag remains live through processing.
    let audio_automated = unsafe { &(*raw).audio_automated };
    if audio_automate && !audio_automated.swap(true, Ordering::Relaxed) {
        // SAFETY: immutable callback pointer remains live until CLOSE.
        let callback = unsafe { (*raw).host };
        // SAFETY: automate carries no borrowed payload.
        unsafe { callback(effect, MASTER_AUTOMATE, 0, 0, std::ptr::null_mut(), 0.5) };
    }
    if mode == mode::PARAMETER_ADDED_AT_OPEN && !audio_automated.swap(true, Ordering::Relaxed) {
        // Defer until processing: a callback inside OPEN precedes the host's
        // post-OPEN header snapshot and would not exercise audit #11 TD-09.
        // SAFETY: the live instance retains this immutable host callback until CLOSE.
        let callback = unsafe { (*raw).host };
        // SAFETY: ID 1 is in the post-OPEN declaration; automate has no pointer payload.
        unsafe { callback(effect, MASTER_AUTOMATE, 1, 0, std::ptr::null_mut(), 0.75) };
    }
    if mode == mode::SHORT_MIDI_HEADER {
        // Audit #11 TD-06: a short MIDI record, the common header and offset/flags
        // only. Its byteSize (8, which excludes type and byteSize) truthfully
        // covers the 16 heap bytes of Box<[i32; 4]>; there is no MIDI payload.
        let header = Box::new([1i32, 8, 0, 0]);
        let mut events = Events {
            count: 1,
            reserved: 0,
            events: [header.as_ptr().cast()],
        };
        // SAFETY: the aligned list and its 16-byte record remain live for this
        // synchronous callback; no payload is read by the fixture.
        unsafe { host(effect, 8, 0, 0, (&mut events as *mut Events<1>).cast()) };
    }
    if mode == mode::STATE_RESTORE_OUTPUT_COUNT_CHANGE
        || (mode == mode::AUDIO_OUTPUT_COUNT_CHANGE
            && observations[observation::AUDIO_OUTPUT_COUNT_CHANGED].load(Ordering::Relaxed) != 0)
    {
        observations[observation::PROCESS_CALLS_AFTER_OUTPUT_COUNT_CHANGE]
            .fetch_add(1, Ordering::Relaxed);
        return;
    }
    // SAFETY: query carries no payload, runs on current processing thread.
    observations[observation::AUDIO_PROCESS_LEVEL].store(
        unsafe { host(effect, 23, 0, 0, std::ptr::null_mut()) } as u32,
        Ordering::Relaxed,
    );
    match mode {
        mode::CRASH_PROCESS => std::process::abort(),
        mode::HANG_PROCESS => loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        },
        _ => {}
    }
    // SAFETY: immutable child ID is set before any processing.
    let child = unsafe { (*raw).child };
    // SAFETY: independent atomic parameter, read before the block.
    let width = f32::from_bits(unsafe { &(*raw).width }.load(Ordering::Relaxed));
    // SAFETY: declared input and output counts are two and six.
    let inputs = unsafe { std::slice::from_raw_parts(inputs, 2) };
    // SAFETY: host provides six output pointers from post-OPEN metadata.
    let outputs = unsafe { std::slice::from_raw_parts(outputs, 6) };
    for frame in 0..frames as usize {
        // SAFETY: channel zero has frames readable samples; read before in-place writes.
        let left = unsafe { *inputs[0].wrapping_add(frame) };
        // SAFETY: channel one has frames readable samples.
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
            let delayed = audio.delay[channel][audio.cursor];
            audio.delay[channel][audio.cursor] = values[channel];
            let sample = if child != 0 {
                values[channel]
                    * if child == i32::from_be_bytes(*b"SH01") {
                        0.5
                    } else {
                        0.25
                    }
            } else if mode == mode::NAN_CENTER_OUTPUT && channel == 2 {
                f32::NAN
            } else {
                delayed
            };
            // SAFETY: each declared output has frames writable samples, allowing in-place input aliases.
            unsafe { *outputs[channel].wrapping_add(frame) = sample };
        }
        audio.cursor = (audio.cursor + 1) % 64;
    }
    if mode == mode::AUDIO_OUTPUT_COUNT_CHANGE {
        // SAFETY: unique processor is at the end of its block; subsequent host
        // header inspection waits for processing to finish after notification.
        unsafe { (*effect).num_outputs = 4 };
        observations[observation::AUDIO_OUTPUT_COUNT_CHANGED].store(1, Ordering::Relaxed);
        // SAFETY: no payload; marks the changed header before returning to host.
        unsafe { host(effect, MASTER_IO_CHANGED, 0, 0, std::ptr::null_mut()) };
    }
}
