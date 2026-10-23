#![allow(unsafe_code)]
use std::{
    cell::UnsafeCell,
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};
use vst_audio_worker_sys::vst2::*;

#[repr(C)]
struct Instance {
    effect: AEffect,
    host: Callback,
    chunk: UnsafeCell<[u8; 2]>,
    width: AtomicU32,
    gate: UnsafeCell<f64>,
    events: vst_audio_worker_sys::vst2_events::CallbackEvents,
    active: AtomicBool,
    input: Arrangement,
    output: Arrangement,
    rect: Rect,
}

// SAFETY: the entry symbol has the VST 2 ABI, has no competing definition, and
// returns an owned instance whose lifetime is ended by CLOSE.
#[unsafe(no_mangle)]
pub extern "C" fn VSTPluginMain(host: Callback) -> *mut AEffect {
    let instance = Box::new(Instance {
        effect: AEffect {
            dispatcher: Some(dispatch),
            set_parameter: Some(set_parameter),
            get_parameter: Some(get_parameter),
            num_programs: 1,
            num_params: 1,
            num_inputs: 0,
            num_outputs: 2,
            flags: HAS_EDITOR | REPLACING | PROGRAM_CHUNKS | (1 << 12),
            initial_delay: 0,
            unique_id: i32::from_be_bytes(*b"SSTP"),
            version: 1,
            process_replacing: Some(process),
            process_double: Some(process64),
            ..AEffect::default()
        },
        host,
        chunk: UnsafeCell::new([1, 0]),
        width: AtomicU32::new(1.0f32.to_bits()),
        gate: UnsafeCell::new(0.0),
        events: Default::default(),
        active: AtomicBool::new(false),
        input: Arrangement::new(0, &[]),
        output: Arrangement::new(ARR_STEREO, &[1, 2]),
        rect: Rect {
            top: 0,
            left: 0,
            bottom: 240,
            right: 320,
        },
    });
    Box::into_raw(instance).cast()
}

unsafe extern "C" fn dispatch(
    effect: *mut AEffect,
    opcode: i32,
    _index: i32,
    value: isize,
    pointer: *mut c_void,
    _option: f32,
) -> isize {
    if effect.is_null() {
        return 0;
    }
    if opcode == CLOSE {
        // SAFETY: CLOSE is called exactly once for the Box allocated by VSTPluginMain.
        drop(unsafe { Box::from_raw(effect.cast::<Instance>()) });
        return 1;
    }
    // SAFETY: header is immutable after entry; mutable fields use atomics or thread-owned UnsafeCell.
    let instance = unsafe { &*effect.cast::<Instance>() };
    match opcode {
        25 => {
            // SAFETY: processEvents carries a valid native list, copied into a bounded fixture queue.
            unsafe { instance.events.receive(pointer) };
            let callback = instance.host;
            // SAFETY: synchronous echo while caller-owned event data is still live.
            unsafe { callback(effect, 8, 0, 0, pointer, 0.0) };
            1
        }
        OPEN | SET_SAMPLE_RATE | SET_BLOCK_SIZE | START_PROCESS | STOP_PROCESS | EDIT_OPEN
        | EDIT_CLOSE | EDIT_IDLE | SET_PROGRAM => 1,
        GET_PROGRAM => 0,
        MAINS_CHANGED => {
            instance.active.store(value != 0, Ordering::Relaxed);
            1
        }
        GET_EFFECT_NAME | GET_VENDOR_STRING | PARAM_NAME => {
            let text: &[u8] = match opcode {
                GET_EFFECT_NAME => b"Test synth\0",
                GET_VENDOR_STRING => b"Test fixtures\0",
                _ => b"Width\0",
            };
            if pointer.is_null() {
                return 0;
            }
            // SAFETY: these opcodes supply a writable string buffer of at least 24 bytes.
            unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), pointer.cast(), text.len()) };
            1
        }
        GET_SPEAKER_ARRANGEMENT => {
            if value == 0 || pointer.is_null() {
                return 0;
            }
            // SAFETY: opcode supplies an input arrangement pointer-out in value.
            unsafe {
                *(value as *mut *mut Arrangement) = std::ptr::addr_of!(instance.input).cast_mut()
            };
            // SAFETY: opcode supplies an output arrangement pointer-out in pointer.
            unsafe {
                *pointer.cast::<*mut Arrangement>() = std::ptr::addr_of!(instance.output).cast_mut()
            };
            1
        }
        SET_SPEAKER_ARRANGEMENT => 1,
        GET_CHUNK => {
            if pointer.is_null() {
                return 0;
            }
            // SAFETY: GET_CHUNK supplies a pointer-out; chunk remains live until next state call.
            unsafe { *pointer.cast::<*const u8>() = instance.chunk.get().cast::<u8>() };
            2
        }
        SET_CHUNK => {
            if value != 2 || pointer.is_null() {
                return 0;
            }
            // SAFETY: SET_CHUNK supplies value readable bytes for this synchronous call.
            let chunk = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), 2) };
            if chunk[0] != 1 || chunk[1] > 4 {
                return 0;
            }
            if chunk[1] == 4 {
                std::process::abort();
            }
            // SAFETY: chunk writes are gated and only the control thread reads the chunk.
            unsafe { *instance.chunk.get() = [chunk[0], chunk[1]] };
            1
        }
        EDIT_GET_RECT => {
            if pointer.is_null() {
                return 0;
            }
            // SAFETY: EDIT_GET_RECT supplies a rectangle pointer-out.
            unsafe { *pointer.cast::<*mut Rect>() = std::ptr::addr_of!(instance.rect).cast_mut() };
            1
        }
        _ => 0,
    }
}
unsafe extern "C" fn set_parameter(effect: *mut AEffect, index: u32, value: f32) {
    if effect.is_null() || index != 0 {
        return;
    }
    // SAFETY: parameter storage is atomic; immutable instance header stays live.
    let instance = unsafe { &*effect.cast::<Instance>() };
    instance
        .width
        .store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    let host = instance.host;
    // SAFETY: callback belongs to the host that created this instance; no pointer payload.
    unsafe { host(effect, MASTER_AUTOMATE, 0, 0, std::ptr::null_mut(), value) };
}
unsafe extern "C" fn get_parameter(effect: *mut AEffect, index: u32) -> f32 {
    if effect.is_null() || index != 0 {
        return 0.0;
    }
    // SAFETY: host serializes parameter access; effect is our live instance.
    f32::from_bits(unsafe { &(*effect.cast::<Instance>()).width }.load(Ordering::Relaxed))
}
unsafe extern "C" fn process(
    effect: *mut AEffect,
    _: *const *const f32,
    outputs: *mut *mut f32,
    frames: i32,
) {
    // SAFETY: VST2 host supplies the fixture instance and f32 output buffers.
    unsafe { render(effect, outputs, frames) };
}
unsafe extern "C" fn process64(
    effect: *mut AEffect,
    _: *const *const f64,
    outputs: *mut *mut f64,
    frames: i32,
) {
    // SAFETY: VST2 host supplies the fixture instance and f64 output buffers.
    unsafe { render(effect, outputs, frames) };
}
trait Sample: Copy {
    fn from_f64(value: f64) -> Self;
}
impl Sample for f32 {
    fn from_f64(value: f64) -> Self {
        value as f32
    }
}
impl Sample for f64 {
    fn from_f64(value: f64) -> Self {
        value
    }
}
unsafe fn render<T: Sample>(effect: *mut AEffect, outputs: *mut *mut T, frames: i32) {
    if effect.is_null() || outputs.is_null() || frames <= 0 {
        return;
    }
    // SAFETY: immutable header is shared; only this processor accesses the audio gate.
    let instance = unsafe { &*effect.cast::<Instance>() };
    // SAFETY: gate is exclusively owned by the unique audio processor.
    let gate = unsafe { &mut *instance.gate.get() };
    let callback = instance.host;
    // SAFETY: time callback returns a readable host time record until its next time query.
    let time = unsafe { callback(effect, 7, 0, 0, std::ptr::null_mut(), 0.0) };
    let position = if time == 0 {
        0.0
    } else {
        // SAFETY: host returned a valid TimeInfo as specified by getTime.
        unsafe { (*(time as *const vst_audio_worker_sys::vst2_events::TimeInfo)).sample_position }
    };
    let mut events = vst_audio_ring::EventBuffer::default();
    instance.events.drain(&mut events, frames as usize);
    // SAFETY: fixture declares two writable output channel pointers.
    let outputs = unsafe { std::slice::from_raw_parts(outputs, 2) };
    for frame in 0..frames as usize {
        for event in events
            .as_slice()
            .iter()
            .filter(|e| e.sample_offset as usize == frame)
        {
            if let vst_audio_ring::EventData::Midi([status, _, velocity]) = event.data {
                match status & 0xf0 {
                    0x90 => *gate = f64::from(velocity) / 127.0,
                    0x80 => *gate = 0.0,
                    _ => {}
                }
            }
        }
        let values = [
            0.123456789012345
                * *gate
                * f64::from(f32::from_bits(instance.width.load(Ordering::Relaxed))),
            (position + frame as f64) * 0.000001 * *gate,
        ];
        for channel in 0..2 {
            // SAFETY: frame lies in the host's declared output span.
            unsafe { *outputs[channel].wrapping_add(frame) = T::from_f64(values[channel]) };
        }
    }
}
