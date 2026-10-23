#![forbid(unsafe_code)]
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use vst_audio::*;

#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

fn host_and_plugin() -> (Host, PluginDescriptor) {
    (
        Host::new(PathBuf::from(
            std::env::var_os("VST_AUDIO_WORKER").expect("worker required"),
        )),
        PluginDescriptor::new(
            PathBuf::from(std::env::var_os("VST_AUDIO_TEST_VST3").expect("VST3 required")),
            PluginFormat::Vst3,
        ),
    )
}

fn callback(audio: &mut AudioEndpoint, frames: usize) -> (AudioStatus, Vec<Vec<f32>>) {
    let geometry = audio.geometry();
    let input = vec![0.25; frames];
    let inputs: Vec<&[f32]> = (0..geometry.input_channels)
        .map(|_| input.as_slice())
        .collect();
    let mut planes = vec![vec![1.0; frames]; geometry.output_channels as usize];
    let mut outputs: Vec<&mut [f32]> = planes.iter_mut().map(Vec::as_mut_slice).collect();
    let status = assert_no_alloc::assert_no_alloc(|| audio.process(&inputs, &mut outputs));
    (status, planes)
}

#[test]
#[ignore = "native fixtures required"]
fn long_callbacks_are_processed_and_oversize_calls_leave_state_untouched() {
    let (host, plugin) = host_and_plugin();
    let config = Configuration {
        quantum: 128,
        max_callback_frames: Some(480),
        ..Default::default()
    };
    let mut instance = host.spawn(&plugin, &config, None).unwrap();
    let mut audio = instance.take_audio().unwrap();
    assert_eq!(audio.geometry().max_frames, 128);
    // Depth ceil(480 / 128) = 4: five quanta of reblocking plus the Matrix
    // fixture's 64-frame plugin latency.
    assert_eq!(audio.latency_frames(), 5 * 128 + 64);

    let period = Duration::from_secs_f64(480.0 / 48_000.0);
    let mut deadline = Instant::now();
    for _ in 0..100 {
        deadline += period;
        let _ = callback(&mut audio, 480);
        let now = Instant::now();
        if deadline > now {
            std::thread::sleep(deadline - now);
        }
    }

    let mut measured = 0;
    let mut processed = 0;
    for _ in 0..300 {
        deadline += period;
        let (status, outputs) = callback(&mut audio, 480);
        measured += 1;
        if status == AudioStatus::Processed {
            processed += 1;
            assert!(
                outputs[0]
                    .iter()
                    .all(|sample| (*sample - 0.25).abs() < 1e-6)
            );
        }
        let now = Instant::now();
        if deadline > now {
            std::thread::sleep(deadline - now);
        }
    }
    let ratio = processed as f64 / measured as f64;
    println!(
        "LONG_CALLBACKS: {processed}/{measured} processed ({:.1}%)",
        ratio * 100.0
    );
    assert!(ratio >= 0.95);

    let (status, outputs) = callback(&mut audio, 481);
    assert_eq!(status, AudioStatus::InvalidBuffer);
    assert!(outputs.iter().flatten().all(|sample| *sample == 0.0));
    assert!((0..20).any(|_| {
        let status = callback(&mut audio, 480).0;
        std::thread::sleep(period);
        status == AudioStatus::Processed
    }));
}

#[test]
#[ignore = "native fixtures required"]
fn default_limit_and_invalid_configurations_are_rejected() {
    let (host, plugin) = host_and_plugin();
    let mut instance = host
        .spawn(&plugin, &Configuration::default(), None)
        .unwrap();
    let mut audio = instance.take_audio().unwrap();
    let (status, outputs) = callback(&mut audio, 129);
    assert_eq!(status, AudioStatus::InvalidBuffer);
    assert!(outputs.iter().flatten().all(|sample| *sample == 0.0));
    assert!((0..20).any(|_| {
        let status = callback(&mut audio, 128).0;
        std::thread::sleep(Duration::from_secs_f64(128.0 / 48_000.0));
        status == AudioStatus::Processed
    }));

    for maximum in [0, 16 * 128] {
        let result = host.spawn(
            &plugin,
            &Configuration {
                quantum: 128,
                max_callback_frames: Some(maximum),
                ..Default::default()
            },
            None,
        );
        assert!(result.is_err(), "maximum {maximum} unexpectedly started");
        let message = result.unwrap_err().to_string();
        assert!(message.contains(&maximum.to_string()));
        assert!(message.contains("128"));
    }
}
