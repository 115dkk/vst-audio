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

struct Paced {
    measured: usize,
    processed: usize,
    max_late: Duration,
    late_starts: usize,
}

impl Paced {
    fn ratio(&self) -> f64 {
        self.processed as f64 / self.measured as f64
    }
}

impl std::fmt::Display for Paced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}/{} processed ({:.1}%), {} sleeps over 1 ms late (max {:?})",
            self.processed,
            self.measured,
            self.ratio() * 100.0,
            self.late_starts,
            self.max_late
        )
    }
}

/// Calls `frames`-frame callbacks at least one `period` apart and counts
/// processed outputs. A late wake-up delays the following callbacks instead of
/// bunching them, so the result measures the endpoint rather than how far a
/// busy CI runner oversleeps: catching up would hand the worker several
/// periods of work within microseconds. After warm-up the fixture passes plane
/// 0's 0.25 through; before it, its own 64-frame delay shows.
fn paced(
    audio: &mut AudioEndpoint,
    frames: usize,
    count: usize,
    period: Duration,
    warm: bool,
) -> Paced {
    let mut run = Paced {
        measured: 0,
        processed: 0,
        max_late: Duration::ZERO,
        late_starts: 0,
    };
    let mut due = Instant::now();
    for _ in 0..count {
        let start = Instant::now();
        let late = start.saturating_duration_since(due);
        run.max_late = run.max_late.max(late);
        run.late_starts += usize::from(late > Duration::from_millis(1));
        let (status, outputs) = callback(audio, frames);
        run.measured += 1;
        if status == AudioStatus::Processed {
            run.processed += 1;
            assert!(
                !warm
                    || outputs[0]
                        .iter()
                        .all(|sample| (*sample - 0.25).abs() < 1e-6)
            );
        }
        due = start + period;
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        }
    }
    run
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
    paced(&mut audio, 480, 100, period, false);
    let run = paced(&mut audio, 480, 300, period, true);
    println!("LONG_CALLBACKS 480/128: {run}");
    if run.ratio() < 0.95 {
        // Diagnosis only: the same endpoint with twice the time per callback.
        println!(
            "LONG_CALLBACKS 480/128 at half speed: {}",
            paced(&mut audio, 480, 200, 2 * period, true)
        );
    }
    assert!(run.ratio() >= 0.95);

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
    // For comparison with the long callbacks: quantum-sized callbacks at
    // depth 1 have a third of their time to come back.
    let period = Duration::from_secs_f64(128.0 / 48_000.0);
    paced(&mut audio, 128, 300, period, false);
    println!(
        "LONG_CALLBACKS 128/128 (default): {}",
        paced(&mut audio, 128, 900, period, true)
    );
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
