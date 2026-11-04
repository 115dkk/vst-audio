#![deny(unsafe_code)]

use std::{env, path::PathBuf, ptr};
use vst_audio_ffi::*;

const TIMEOUT_MS: u32 = 10_000;
const FRAMES: usize = 1061;
const TAIL: usize = 193;

fn path_bytes(variable: &str) -> Vec<u8> {
    PathBuf::from(env::var_os(variable).expect("fixture path required"))
        .to_string_lossy()
        .into_owned()
        .into_bytes()
}

#[allow(unsafe_code)]
fn last_error(kind: bool) -> String {
    let length = if kind {
        // SAFETY: A null buffer with zero capacity queries the current error length.
        unsafe { vsta_last_error_kind(ptr::null_mut(), 0) }
    } else {
        // SAFETY: A null buffer with zero capacity queries the current error length.
        unsafe { vsta_last_error(ptr::null_mut(), 0) }
    };
    let mut bytes = vec![0; length];
    let copied = if kind {
        // SAFETY: `bytes` has `length` writable bytes for the duration of the call.
        unsafe { vsta_last_error_kind(bytes.as_mut_ptr(), bytes.len()) }
    } else {
        // SAFETY: `bytes` has `length` writable bytes for the duration of the call.
        unsafe { vsta_last_error(bytes.as_mut_ptr(), bytes.len()) }
    };
    assert_eq!(copied, length);
    String::from_utf8(bytes).unwrap()
}

#[allow(unsafe_code)]
fn new_host(worker: &[u8]) -> *mut VstaHost {
    let mut host = ptr::null_mut();
    assert_eq!(
        /* SAFETY: The test owns the live handles and provides buffers for the duration of this ABI call. */
        unsafe { vsta_host_new(worker.as_ptr(), worker.len(), &mut host) },
        VSTA_OK,
        "{}",
        last_error(false)
    );
    assert!(!host.is_null());
    host
}

#[allow(unsafe_code)]
fn class_name(info: &VstaClassInfo) -> String {
    // SAFETY: `info` came from a live class list and its name pointer remains valid.
    let bytes = unsafe { std::slice::from_raw_parts(info.name, info.name_len) };
    std::str::from_utf8(bytes).unwrap().to_owned()
}

fn signal() -> Vec<f32> {
    let mut input = vec![0.0; 2 * FRAMES];
    for frame in 0..FRAMES {
        input[frame] = ((frame % 251) as f32 - 125.0) / 256.0;
        input[FRAMES + frame] = ((frame * 7 % 257) as f32 - 128.0) / 512.0;
    }
    input
}

fn expected(input: &[f32]) -> Vec<f32> {
    let length = FRAMES + TAIL;
    let mut output = vec![0.0; 6 * length];
    for frame in 0..FRAMES {
        let left = input[frame];
        let right = input[FRAMES + frame];
        let samples = [
            left,
            right,
            (left + right) / 2.0,
            (left + right) / 2.0,
            left / 2.0,
            right / 2.0,
        ];
        for (channel, sample) in samples.into_iter().enumerate() {
            output[channel * length + frame] = sample;
        }
    }
    output
}

#[allow(unsafe_code)]
fn parameter_at(list: *const VstaParameterList, index: usize) -> VstaParameter {
    let mut parameter = VstaParameter {
        id: 0,
        name: ptr::null(),
        name_len: 0,
        unit: ptr::null(),
        unit_len: 0,
        value: 0.0,
        default_value: 0.0,
        has_default: 0,
        steps: 0,
        has_steps: 0,
        automatable: 0,
        read_only: 0,
        hidden: 0,
    };
    assert_eq!(
        /* SAFETY: The test owns the live handles and provides buffers for the duration of this ABI call. */
        unsafe { vsta_parameter_list_get(list, index, &mut parameter) },
        VSTA_OK,
        "{}",
        last_error(false)
    );
    parameter
}

#[allow(unsafe_code)]
fn exercise_fixture(path_variable: &str, format: i32) {
    let worker = path_bytes("VST_AUDIO_WORKER");
    let path = path_bytes(path_variable);
    let host = new_host(&worker);

    let mut classes = ptr::null_mut();
    assert_eq!(
        /* SAFETY: The test owns the live handles and provides buffers for the duration of this ABI call. */
        unsafe { vsta_host_scan(host, path.as_ptr(), path.len(), format, 0, &mut classes) },
        VSTA_OK,
        "{}",
        last_error(false)
    );
    assert_eq!(
        /* SAFETY: The test owns the live handles and provides buffers for the duration of this ABI call. */
        unsafe { vsta_class_list_len(classes) },
        1
    );
    let mut class = VstaClassInfo {
        id: [0; 16],
        name: ptr::null(),
        name_len: 0,
        vendor: ptr::null(),
        vendor_len: 0,
        category: ptr::null(),
        category_len: 0,
    };
    assert_eq!(
        /* SAFETY: The test owns the live handles and provides buffers for the duration of this ABI call. */
        unsafe { vsta_class_list_get(classes, 0, &mut class) },
        VSTA_OK
    );
    assert_eq!(class_name(&class), "Test matrix");

    let mut plugin = ptr::null_mut();
    assert_eq!(
        /* SAFETY: The test owns the live handles and provides buffers for the duration of this ABI call. */
        unsafe {
            vsta_host_load(
                host,
                path.as_ptr(),
                path.len(),
                format,
                ptr::null(),
                48_000,
                128,
                0,
                TIMEOUT_MS,
                &mut plugin,
            )
        },
        VSTA_OK,
        "{}",
        last_error(false)
    );
    let mut info = VstaPluginInfo {
        input_channels: 0,
        output_channels: 0,
        sample_rate: 0,
        block_size: 0,
        sample_format: -1,
        latency: 0,
    };
    assert_eq!(
        /* SAFETY: The test owns the live handles and provides buffers for the duration of this ABI call. */
        unsafe { vsta_plugin_info(plugin, &mut info) },
        VSTA_OK
    );
    assert_eq!((info.input_channels, info.output_channels), (2, 6));
    assert_eq!((info.sample_rate, info.block_size), (48_000, 128));
    assert_eq!(info.sample_format, 0);
    assert_eq!(info.latency, 64);

    let input = signal();
    let mut output = vec![-99.0; 6 * (FRAMES + TAIL)];
    assert_eq!(
        /* SAFETY: The test owns the live handles and provides buffers for the duration of this ABI call. */
        unsafe {
            vsta_plugin_process_f32(
                plugin,
                input.as_ptr(),
                input.len(),
                FRAMES,
                TAIL,
                output.as_mut_ptr(),
                output.len(),
                TIMEOUT_MS,
            )
        },
        VSTA_OK,
        "{}",
        last_error(false)
    );
    assert_eq!(output, expected(&input));

    let input_f64 = [0.0; 2];
    let mut output_f64 = [0.0; 6];
    assert_eq!(
        /* SAFETY: The test owns the live handles and provides buffers for the duration of this ABI call. */
        unsafe {
            vsta_plugin_process_f64(
                plugin,
                input_f64.as_ptr(),
                input_f64.len(),
                1,
                0,
                output_f64.as_mut_ptr(),
                output_f64.len(),
                TIMEOUT_MS,
            )
        },
        VSTA_RENDER
    );
    assert_eq!(last_error(true), "invalid_buffer");

    let mut parameters = ptr::null_mut();
    assert_eq!(
        /* SAFETY: The test owns the live handles and provides buffers for the duration of this ABI call. */
        unsafe { vsta_plugin_parameters(plugin, TIMEOUT_MS, &mut parameters) },
        VSTA_OK
    );
    // SAFETY: `parameters` is a live list handle owned by this test.
    let count = unsafe { vsta_parameter_list_len(parameters) };
    assert!(count > 0);
    let parameter = parameter_at(parameters, 0);
    assert_eq!(
        /* SAFETY: The test owns the live handles and provides buffers for the duration of this ABI call. */
        unsafe { vsta_plugin_set_parameter(plugin, parameter.id, 0.25, TIMEOUT_MS) },
        VSTA_OK
    );
    /* SAFETY: The test owns the live handles and provides buffers for the duration of this ABI call. */
    unsafe { vsta_parameter_list_free(parameters) };

    parameters = ptr::null_mut();
    assert_eq!(
        /* SAFETY: The test owns the live handles and provides buffers for the duration of this ABI call. */
        unsafe { vsta_plugin_parameters(plugin, TIMEOUT_MS, &mut parameters) },
        VSTA_OK
    );
    // SAFETY: `parameters` is a live list handle owned by this test.
    let updated_count = unsafe { vsta_parameter_list_len(parameters) };
    let updated = (0..updated_count)
        .map(|index| parameter_at(parameters, index))
        .find(|entry| entry.id == parameter.id)
        .unwrap();
    assert_eq!(updated.value, 0.25);
    /* SAFETY: The test owns the live handles and provides buffers for the duration of this ABI call. */
    unsafe { vsta_parameter_list_free(parameters) };

    let mut state = ptr::null_mut();
    assert_eq!(
        /* SAFETY: The test owns the live handles and provides buffers for the duration of this ABI call. */
        unsafe { vsta_plugin_save_state(plugin, TIMEOUT_MS, &mut state) },
        VSTA_OK
    );
    // SAFETY: `state` is a live bytes handle owned by this test.
    let state_len = unsafe { vsta_bytes_len(state) };
    assert!(state_len > 0);
    // SAFETY: `state` remains live while its data pointer is copied.
    let state_data = unsafe { vsta_bytes_data(state) };
    // SAFETY: The bytes handle guarantees `state_len` readable bytes at `state_data`.
    let state_copy = unsafe { std::slice::from_raw_parts(state_data, state_len) }.to_vec();
    assert_eq!(
        /* SAFETY: The test owns the live handles and provides buffers for the duration of this ABI call. */
        unsafe {
            vsta_plugin_restore_state(plugin, state_copy.as_ptr(), state_copy.len(), TIMEOUT_MS)
        },
        VSTA_OK
    );
    let mut restored = ptr::null_mut();
    assert_eq!(
        /* SAFETY: The test owns the live handles and provides buffers for the duration of this ABI call. */
        unsafe { vsta_plugin_save_state(plugin, TIMEOUT_MS, &mut restored) },
        VSTA_OK
    );
    // SAFETY: `restored` is a live bytes handle owned by this test.
    let restored_len = unsafe { vsta_bytes_len(restored) };
    // SAFETY: `restored` remains live while its data pointer is copied.
    let restored_data = unsafe { vsta_bytes_data(restored) };
    // SAFETY: The bytes handle guarantees `restored_len` readable bytes at `restored_data`.
    let restored_copy = unsafe { std::slice::from_raw_parts(restored_data, restored_len) }.to_vec();
    assert_eq!(restored_copy, state_copy);

    // SAFETY: Each pointer is a distinct live handle and is freed exactly once.
    unsafe { vsta_bytes_free(restored) };
    // SAFETY: Each pointer is a distinct live handle and is freed exactly once.
    unsafe { vsta_bytes_free(state) };
    // SAFETY: Each pointer is a distinct live handle and is freed exactly once.
    unsafe { vsta_plugin_free(plugin) };
    // SAFETY: Each pointer is a distinct live handle and is freed exactly once.
    unsafe { vsta_class_list_free(classes) };
    // SAFETY: Each pointer is a distinct live handle and is freed exactly once.
    unsafe { vsta_host_free(host) };
}

#[test]
#[ignore = "native worker and VST2 fixture required"]
fn vst2_abi_round_trip() {
    exercise_fixture("VST_AUDIO_TEST_VST2", 1);
}

#[test]
#[ignore = "native worker and VST3 fixture required"]
fn vst3_abi_round_trip() {
    exercise_fixture("VST_AUDIO_TEST_VST3", 2);
}

#[test]
#[ignore = "native worker and fixture paths required"]
#[allow(unsafe_code)]
fn invalid_arguments_start_error_and_short_copy() {
    assert_eq!(vsta_abi_version(), 1);
    assert_eq!(
        // SAFETY: The ABI explicitly accepts null pointers and reports an invalid argument.
        unsafe { vsta_plugin_info(ptr::null(), ptr::null_mut()) },
        VSTA_INVALID_ARGUMENT
    );
    assert_eq!(last_error(true), "invalid_argument");
    let full = last_error(false).into_bytes();
    assert!(!full.is_empty());
    let mut short = [0u8; 2];
    // SAFETY: `short` provides two writable bytes for the duration of the call.
    let length = unsafe { vsta_last_error(short.as_mut_ptr(), short.len()) };
    assert_eq!(length, full.len());
    assert_eq!(short, full[..2]);

    let missing = b"Z:/vst-audio-worker-does-not-exist.exe";
    let host = new_host(missing);
    let path = path_bytes("VST_AUDIO_TEST_VST2");
    let mut plugin = ptr::null_mut();
    assert_eq!(
        /* SAFETY: The test owns the live handles and provides buffers for the duration of this ABI call. */
        unsafe {
            vsta_host_load(
                host,
                path.as_ptr(),
                path.len(),
                1,
                ptr::null(),
                48_000,
                128,
                0,
                TIMEOUT_MS,
                &mut plugin,
            )
        },
        VSTA_START
    );
    assert!(!last_error(false).is_empty());
    assert!(!last_error(true).is_empty());
    // SAFETY: The free function explicitly accepts null as a no-op.
    unsafe { vsta_plugin_free(ptr::null_mut()) };
    // SAFETY: `host` is a live handle and is freed exactly once.
    unsafe { vsta_host_free(host) };
}
