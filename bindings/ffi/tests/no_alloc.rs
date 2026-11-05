#![deny(unsafe_code)]

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use std::{env, path::PathBuf, ptr, thread, time::Duration};
use vst_audio_ffi::*;

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

fn path_bytes(variable: &str) -> Vec<u8> {
    PathBuf::from(env::var_os(variable).expect("fixture path required"))
        .to_string_lossy()
        .into_owned()
        .into_bytes()
}

#[test]
#[ignore = "native worker and VST2 fixture required"]
#[allow(unsafe_code)]
fn realtime_process_f32_does_not_allocate() {
    let worker = path_bytes("VST_AUDIO_WORKER");
    let path = path_bytes("VST_AUDIO_TEST_VST2");
    let mut host = ptr::null_mut();
    assert_eq!(
        // SAFETY: The byte slice and output slot remain valid for the call.
        unsafe { vsta_host_new(worker.as_ptr(), worker.len(), &mut host) },
        VSTA_OK
    );
    let mut plugin = ptr::null_mut();
    assert_eq!(
        // SAFETY: Handles and byte slices remain live, and output is exclusive.
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
                10_000,
                &mut plugin,
                1,
            )
        },
        VSTA_OK
    );
    let mut blocks = ptr::null_mut();
    assert_eq!(
        // SAFETY: Plugin and output are exclusively accessed for this call.
        unsafe { vsta_plugin_take_blocks(plugin, &mut blocks) },
        VSTA_OK
    );
    let input = [0.25f32; 2 * 128];
    let mut output = [0.0f32; 6 * 128];
    for _ in 0..500 {
        // SAFETY: The endpoint and disjoint fixed-size buffers remain live.
        let _ = unsafe {
            vsta_blocks_process_f32(
                blocks,
                input.as_ptr(),
                input.len(),
                output.as_mut_ptr(),
                output.len(),
            )
        };
        thread::sleep(Duration::from_micros(100));
    }
    let mut delivered = 0usize;
    let mut missed = 0usize;
    assert_no_alloc(|| {
        for index in 0..2_000 {
            // SAFETY: The endpoint and disjoint fixed-size buffers remain live.
            match unsafe {
                vsta_blocks_process_f32(
                    blocks,
                    input.as_ptr(),
                    input.len(),
                    output.as_mut_ptr(),
                    output.len(),
                )
            } {
                VSTA_BLOCK_DELIVERED => delivered += 1,
                VSTA_BLOCK_MISSED => missed += 1,
                status => panic!("unexpected block status {status}"),
            }
            if index % 20 == 0 {
                thread::sleep(Duration::from_micros(100));
            }
        }
    });
    assert!(delivered > 0, "expected delivered blocks");
    assert!(missed > 0, "expected missed blocks");
    // SAFETY: Distinct live handles are freed once after processing ended.
    unsafe { vsta_blocks_free(blocks) };
    // SAFETY: Plugin remains live and is freed once.
    unsafe { vsta_plugin_free(plugin) };
    // SAFETY: Host remains live and is freed once.
    unsafe { vsta_host_free(host) };
}
