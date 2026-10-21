#![forbid(unsafe_code)]
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use vst_audio::*;

#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

#[test]
#[ignore = "native fixtures required"]
fn repeated_crash_recovery_keeps_variable_callbacks_and_healthy_peer_running() {
    let host = Host::new(PathBuf::from(
        std::env::var_os("VST_AUDIO_WORKER").expect("worker required"),
    ));
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let plugin = PluginDescriptor::new(
            PathBuf::from(
                std::env::var_os(if format == PluginFormat::Vst2 {
                    "VST_AUDIO_TEST_VST2"
                } else {
                    "VST_AUDIO_TEST_VST3"
                })
                .unwrap(),
            ),
            format,
        );
        let config = Configuration {
            recovery: RecoveryPolicy::Automatic {
                attempts: 8,
                backoff_ms: 20,
            },
            ..Default::default()
        };
        let mut a = host.spawn(&plugin, &config, None).unwrap();
        let mut b = host
            .spawn(&plugin, &Configuration::default(), None)
            .unwrap();
        let mut audio_a = a.take_audio().unwrap();
        let mut audio_b = b.take_audio().unwrap();
        let saved = a.controller().request(Command::SaveState).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if matches!(a.try_next_event(),Some(HostEvent::State{request,..})if request==Some(saved))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "initial recovery checkpoint missing"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        // Save two distinct valid snapshots. Independent later crashes must
        // restore the latest (mode 3), not roll back merely because a previous
        // crash has already used one attempt from the session budget.
        let latest = if format == PluginFormat::Vst3 {
            vst_audio::state::encode(&[1, 3], &[1, 3]).unwrap()
        } else {
            vec![1, 3]
        };
        let edit = a
            .controller()
            .request(Command::RestoreState(latest.clone()))
            .unwrap();
        loop {
            if matches!(a.try_next_event(),Some(HostEvent::Completed{request})if request==edit) {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let saved = a.controller().request(Command::SaveState).unwrap();
        loop {
            if matches!(a.try_next_event(),Some(HostEvent::State{request,..})if request==Some(saved))
            {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let source = [0.25; 256];
        let mut output_a = [[0.0; 256]; 6];
        let mut output_b = [[0.0; 256]; 6];
        let mut blocks = 0;
        let mut completed = 0;
        let mut frames = 0;
        let mut injections = 0;
        let mut recovered = 0;
        let mut last_injected_generation = 0;
        let deadline = Instant::now() + Duration::from_secs(45);
        while completed < 400 || recovered < 4 {
            assert!(
                Instant::now() < deadline,
                "{format:?}: {completed} blocks, {recovered} recoveries"
            );
            let count = [1, 7, 37, 83, 129, 256][blocks % 6];
            let input = [&source[..count], &source[..count]];
            let mut left = output_a.each_mut().map(|p| &mut p[..count]);
            let mut right = output_b.each_mut().map(|p| &mut p[..count]);
            let (status, _) = assert_no_alloc::assert_no_alloc(|| {
                let status = audio_b.process(&input, &mut right);
                let failed = audio_a.process(&input, &mut left);
                (status, failed)
            });
            frames += count;
            if status == AudioStatus::Processed && frames > 1024 {
                completed += 1;
                assert!(right[0].iter().all(|v| (*v - 0.25).abs() < 1e-6));
            }
            assert_eq!(b.status(), Status::Running);
            for _ in 0..32 {
                match a.try_next_event() {
                    Some(HostEvent::Recovered { generation }) => {
                        assert!(generation > last_injected_generation);
                        recovered += 1;
                        a.controller().request(Command::SaveState).unwrap();
                    }
                    Some(HostEvent::State { bytes, .. }) => {
                        assert_eq!(bytes, latest, "latest successful checkpoint was lost")
                    }
                    Some(HostEvent::NotificationsLost) => {
                        panic!("stress consumer lost lifecycle notifications")
                    }
                    Some(_) => {}
                    None => break,
                }
            }
            if injections < 4
                && blocks > 20
                && blocks % 24 == 0
                && a.status() == Status::Running
                && audio_a.generation() > last_injected_generation
            {
                let state = if format == PluginFormat::Vst3 {
                    vst_audio::state::encode(&[1, 1], &[1, 1]).unwrap()
                } else {
                    vec![1, 1]
                };
                a.controller()
                    .request(Command::RestoreState(state))
                    .unwrap();
                last_injected_generation = audio_a.generation();
                injections += 1;
            }
            blocks += 1;
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(injections, 4);
        assert!(audio_a.generation() >= 5);
        println!(
            "STRESS {format:?}: {completed} numeric healthy callbacks, {frames} frames, {recovered} successful recoveries"
        );
    }
}
