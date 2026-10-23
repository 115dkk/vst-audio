#![forbid(unsafe_code)]

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use vst_audio::{
    Architecture, AudioStatus, Configuration, Host, PluginDescriptor, PluginFormat, Status,
    plugin_architectures,
};

fn path(name: &str) -> PathBuf {
    std::env::var_os(name)
        .unwrap_or_else(|| panic!("set {name}"))
        .into()
}

#[test]
#[ignore = "cross-architecture fixtures required"]
fn cross_architecture_plugins_run_in_their_own_worker() {
    let architecture = match std::env::var("VST_AUDIO_CROSS_ARCH").unwrap().as_str() {
        "x86" => Architecture::X86,
        "x86_64" => Architecture::X86_64,
        "aarch64" => Architecture::Aarch64,
        other => panic!("unknown architecture: {other}"),
    };
    assert_ne!(
        architecture,
        Architecture::host(),
        "must exercise a different architecture"
    );
    assert!(architecture.runs_on_host());
    let worker = path("VST_AUDIO_CROSS_WORKER");
    let native = Host::new(path("VST_AUDIO_NATIVE_WORKER"));
    assert_eq!(
        plugin_architectures(&worker, PluginFormat::Vst2).unwrap(),
        [architecture]
    );
    assert!(
        plugin_architectures(native.worker_path(), PluginFormat::Vst2)
            .unwrap()
            .contains(&Architecture::host())
    );
    println!(
        "{} application -> {} worker: {}",
        Architecture::host().name(),
        architecture.name(),
        worker.display()
    );
    let host = native.clone().with_worker(architecture, &worker);
    for (format, environment) in [
        (PluginFormat::Vst2, "VST_AUDIO_TEST_VST2"),
        (PluginFormat::Vst3, "VST_AUDIO_TEST_VST3"),
    ] {
        let plugin = PluginDescriptor::new(path(environment), format);
        assert_eq!(
            plugin_architectures(&plugin.path, format).unwrap(),
            [architecture]
        );
        assert_eq!(host.worker_for(&plugin).unwrap(), worker);
        assert!(native.worker_for(&plugin).is_err());
        assert!(!host.scan(&plugin).unwrap().is_empty());
        let state = if format == PluginFormat::Vst3 {
            vst_audio::state::encode(&[1, 0], &[1, 0]).unwrap()
        } else {
            vec![1, 0]
        };
        let config = Configuration::default();
        // Exercise both public startup paths as well as disposable scanning.
        let mut instance = if format == PluginFormat::Vst2 {
            host.spawn(&plugin, &config, Some(&state)).unwrap()
        } else {
            let task = host.spawn_async(plugin, config, Some(state));
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                if let Some(instance) = task.try_complete().unwrap() {
                    break instance;
                }
                assert!(Instant::now() < deadline, "async startup timed out");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let mut audio = instance.take_audio().unwrap();
        assert_eq!(audio.latency_frames(), 2 * 128 + 64);
        assert_eq!(audio.geometry().output_channels, 6);
        let left: [f32; 128] = std::array::from_fn(|frame| (frame as f32 * 0.17).sin() * 0.25);
        let right: [f32; 128] = std::array::from_fn(|frame| (frame as f32 * 0.23).cos() * 0.25);
        let mut samples = [[0.0; 128]; 6];
        let mut outputs: Vec<_> = samples.iter_mut().map(|channel| &mut channel[..]).collect();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut processed = 0;
        while processed < 6 {
            assert!(Instant::now() < deadline, "audio did not progress");
            assert_eq!(instance.status(), Status::Running);
            if audio.process(&[&left, &right], &mut outputs) == AudioStatus::Processed {
                processed += 1;
                // Two adapter quanta plus 64 plugin frames; the repeated input
                // has period 128, so after warm-up the expected index is exact.
                if processed > 2 {
                    for (frame, _) in left.iter().enumerate() {
                        let index = (frame + 128 - 64) % 128;
                        let (left, right) = (left[index], right[index]);
                        let expected = [
                            left,
                            right,
                            0.5 * (left + right),
                            0.5 * (left + right),
                            0.5 * left,
                            0.5 * right,
                        ];
                        for (channel, expected) in expected.into_iter().enumerate() {
                            assert!(
                                (outputs[channel][frame] - expected).abs() < 1e-6,
                                "{format:?} channel {channel} frame {frame}"
                            );
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
