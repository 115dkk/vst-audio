#![forbid(unsafe_code)]
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use vst_audio::*;
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;
fn host() -> Host {
    let worker = PathBuf::from(std::env::var_os("VST_AUDIO_WORKER").unwrap());
    let host = Host::new(&worker);
    if std::env::var_os("VST_AUDIO_CROSS_ARCH").is_some() {
        let architectures = plugin_architectures(&worker, PluginFormat::Vst2).unwrap();
        assert_eq!(architectures.len(), 1);
        host.with_worker(architectures[0], worker)
    } else {
        host
    }
}

#[test]
#[ignore = "native fixtures required"]
fn invalid_event_routes_and_parameter_ids_preserve_the_instance() {
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let mut instance = host()
            .spawn(&plugin(format), &Configuration::default(), None)
            .unwrap();
        let mut audio = instance.take_audio().unwrap();
        let mut left = [0.0; 128];
        let mut right = [0.0; 128];
        let mut received = [AudioEvent::default(); 4];
        let invalid = [AudioEvent::midi(0, 1, [0x90, 60, 127])];
        let report = audio.process_events(
            128,
            &[],
            &mut [&mut left, &mut right],
            Transport::default(),
            &invalid,
            &mut received,
        );
        assert_eq!(report.status, AudioStatus::InvalidEvents);
        let events = [
            AudioEvent {
                sample_offset: 0,
                bus: 0,
                data: EventData::Parameter {
                    id: u32::MAX,
                    value: 0.5,
                },
            },
            AudioEvent::midi(11, 0, [0x90, 60, 127]),
            AudioEvent::midi(27, 0, [0xa0, 60, 95]),
        ];
        for block in 0..3 {
            let report = assert_no_alloc::assert_no_alloc(|| {
                audio.process_events(
                    128,
                    &[],
                    &mut [&mut left, &mut right],
                    Transport::default(),
                    if block == 0 { &events } else { &[] },
                    &mut received,
                )
            });
            if block == 2 {
                assert_eq!(report.status, AudioStatus::Processed);
                assert_eq!((report.events_written, report.events_dropped), (2, 1));
                assert_eq!(&received[..2], &events[1..]);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(instance.status(), Status::Running);
    }
}

#[test]
#[ignore = "native fixtures required"]
fn sysex_ownership_transport_splits_and_output_overflow_are_explicit() {
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let mut instance = host()
            .spawn(
                &plugin(format),
                &Configuration {
                    sample_format: SampleFormat::F64,
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        let mut audio = instance.take_audio().unwrap();
        let mut bytes = [0; MAX_SYSEX_BYTES];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = (index % 128) as u8;
        }
        bytes[0] = 0xf0;
        bytes[MAX_SYSEX_BYTES - 1] = 0xf7;
        let sysex = AudioEvent {
            sample_offset: 17,
            bus: 0,
            data: EventData::SysEx {
                len: MAX_SYSEX_BYTES as u16,
                bytes,
            },
        };
        let context = Transport {
            sample_position: 1000,
            ..Default::default()
        };
        let jump = Transport {
            sample_position: 2000,
            tempo: 137.0,
            ..Default::default()
        };
        let events = [
            AudioEvent::midi(0, 0, [0x90, 60, 127]),
            sysex,
            AudioEvent {
                sample_offset: 64,
                bus: 0,
                data: EventData::Transport(jump),
            },
        ];
        let mut left = [0.0; 128];
        let mut right = [0.0; 128];
        let mut received = [AudioEvent::default(); 4];
        for block in 0..3 {
            let report = assert_no_alloc::assert_no_alloc(|| {
                audio.process_events_f64(
                    128,
                    &[],
                    &mut [&mut left, &mut right],
                    context,
                    if block == 0 { &events } else { &[] },
                    &mut received,
                )
            });
            if block == 2 {
                assert_eq!(report.status, AudioStatus::Processed);
                assert_eq!((report.events_written, report.events_dropped), (2, 0));
                assert_eq!(received[0], events[0]);
                assert_eq!(received[1], sysex);
                for (frame, value) in right.iter().enumerate() {
                    let sample = if frame < 64 {
                        1000 + frame
                    } else {
                        2000 + frame - 64
                    };
                    assert!(
                        (value - sample as f64 * 0.000001).abs() < 1e-14,
                        "{format:?} transport frame {frame}: {value}"
                    );
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let burst: Vec<_> = (0..MAX_EVENTS)
            .map(|i| {
                AudioEvent::midi(
                    (i / 2) as u32,
                    0,
                    [if i % 2 == 0 { 0x90 } else { 0x80 }, 60, 100],
                )
            })
            .collect();
        for block in 0..3 {
            let report = assert_no_alloc::assert_no_alloc(|| {
                audio.process_events_f64(
                    128,
                    &[],
                    &mut [&mut left, &mut right],
                    context,
                    if block == 0 { &burst } else { &[] },
                    &mut received,
                )
            });
            if block == 2 {
                assert_eq!(report.status, AudioStatus::Processed);
                assert_eq!(
                    (report.events_written, report.events_dropped),
                    (4, (MAX_EVENTS - 4) as u32)
                );
                assert_eq!(&received[..], &burst[..4]);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
fn plugin(format: PluginFormat) -> PluginDescriptor {
    PluginDescriptor::new(
        PathBuf::from(
            std::env::var_os(if format == PluginFormat::Vst2 {
                "VST_AUDIO_ADVANCED_VST2"
            } else {
                "VST_AUDIO_ADVANCED_VST3"
            })
            .unwrap(),
        ),
        format,
    )
}

#[test]
#[ignore = "native fixtures required"]
fn f64_instruments_midi_automation_and_transport_are_sample_accurate() {
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let config = Configuration {
            sample_format: SampleFormat::F64,
            ..Default::default()
        };
        let mut instance = host().spawn(&plugin(format), &config, None).unwrap();
        let mut audio = instance.take_audio().unwrap();
        assert_eq!(audio.geometry().input_channels, 0);
        assert!(audio.capabilities().f64);
        assert_eq!(audio.latency_frames(), 256);
        let events = [
            AudioEvent::midi(15, 0, [0x90, 60, 127]),
            AudioEvent {
                sample_offset: 37,
                bus: 0,
                data: EventData::Parameter { id: 0, value: 0.25 },
            },
            AudioEvent::midi(89, 0, [0x80, 60, 0]),
        ];
        let mut left = [0.0; 128];
        let mut right = [0.0; 128];
        let mut received = [AudioEvent::default(); 32];
        // A fresh worker's first block can outlast the 10 ms gap below on a
        // loaded runner; run silent blocks until one comes back processed.
        let deadline = Instant::now() + Duration::from_secs(5);
        while audio
            .process_events_f64(
                128,
                &[],
                &mut [&mut left, &mut right],
                Transport::default(),
                &[],
                &mut received,
            )
            .status
            != AudioStatus::Processed
        {
            assert!(
                Instant::now() < deadline,
                "{format:?} never processed a block"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        for block in 0..3 {
            let context = Transport {
                sample_position: 1000 + block * 128,
                continuous_position: block * 128,
                tempo: 137.0,
                ..Default::default()
            };
            let report = assert_no_alloc::assert_no_alloc(|| {
                audio.process_events_f64(
                    128,
                    &[],
                    &mut [&mut left, &mut right],
                    context,
                    if block == 0 { &events } else { &[] },
                    &mut received,
                )
            });
            if block == 2 {
                assert_eq!(report.status, AudioStatus::Processed);
                for (frame, value) in left.iter().enumerate() {
                    let expected = if (15..37).contains(&frame) {
                        0.123456789012345
                    } else if (37..89).contains(&frame) {
                        0.123456789012345 * 0.25
                    } else {
                        0.0
                    };
                    assert!(
                        (value - expected).abs() < 1e-14,
                        "{format:?} frame {frame}: {value} != {expected}"
                    );
                }
                assert!((right[20] - 0.001020).abs() < 1e-14);
                assert_eq!(report.events_written, 2);
                assert_eq!(received[0].sample_offset, 15);
                assert_eq!(received[1].sample_offset, 89);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
#[ignore = "native fixtures required"]
fn sidechains_and_auxiliary_buses_preserve_native_order() {
    let host = host();
    let mut plugin = plugin(PluginFormat::Vst3);
    let classes = host.scan(&plugin).unwrap();
    assert_eq!(classes.len(), 2);
    plugin.class_id = Some(
        classes
            .iter()
            .find(|c| c.name == "Test sidechain")
            .unwrap()
            .id,
    );
    let mut instance = host
        .spawn(
            &plugin,
            &Configuration {
                sample_format: SampleFormat::F64,
                ..Default::default()
            },
            None,
        )
        .unwrap();
    let mut audio = instance.take_audio().unwrap();
    assert_eq!(audio.layout().inputs.len(), 2);
    assert_eq!(audio.geometry().input_channels, 3);
    assert_eq!(audio.geometry().output_channels, 3);
    let left = [0.123456789012345; 128];
    let right = [0.75; 128];
    let side = [0.25; 128];
    let mut a = [0.0; 128];
    let mut b = [0.0; 128];
    let mut c = [0.0; 128];
    for block in 0..4 {
        let status = audio.process_f64(&[&left, &right, &side], &mut [&mut a, &mut b, &mut c]);
        if block >= 2 {
            assert_eq!(status, AudioStatus::Processed);
            assert!((a[0] - (left[0] + 0.5)).abs() < 1e-14);
            assert_eq!(b[0], 0.75);
            assert_eq!(c[0], 0.75);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "native fixtures required"]
fn bus_selection_after_load_leaves_the_sidechain_and_auxiliary_output_inactive() {
    let host = host();
    let mut plugin = plugin(PluginFormat::Vst3);
    let classes = host.scan(&plugin).unwrap();
    plugin.class_id = Some(
        classes
            .iter()
            .find(|class| class.name == "Test sidechain")
            .unwrap()
            .id,
    );
    let configuration = Configuration {
        sample_format: SampleFormat::F64,
        buses: Some(BusSelector::main_only()),
        ..Default::default()
    };
    let mut instance = host.spawn(&plugin, &configuration, None).unwrap();
    let mut audio = instance.take_audio().unwrap();
    assert_eq!(audio.layout().inputs.len(), 2);
    assert!(!audio.layout().inputs[1].active);
    assert!(!audio.layout().outputs[1].active);
    assert_eq!(audio.geometry().input_channels, 2);
    assert_eq!(audio.geometry().output_channels, 2);
    let left = [0.123456789012345; 128];
    let right = [0.75; 128];
    let mut a = [0.0; 128];
    let mut b = [0.0; 128];
    for block in 0..4 {
        let status = audio.process_f64(&[&left, &right], &mut [&mut a, &mut b]);
        if block >= 2 {
            assert_eq!(status, AudioStatus::Processed);
            assert_eq!(a, left);
            assert_eq!(b, right);
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let started = PluginProcess::start_configured(
        PathBuf::from(std::env::var_os("VST_AUDIO_WORKER").unwrap()).as_path(),
        &plugin.path,
        plugin.format,
        plugin.class_id,
        &ProcessingOptions {
            sample_format: SampleFormat::F64,
            buses: Some(BusSelector::main_only()),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    assert_eq!(
        started
            .declared_inputs
            .iter()
            .map(|bus| bus.kind)
            .collect::<Vec<_>>(),
        [BusKind::Main, BusKind::Aux]
    );
    assert_eq!(
        started
            .declared_inputs
            .iter()
            .map(|bus| bus.name.as_str())
            .collect::<Vec<_>>(),
        ["Main", "Sidechain"]
    );
}

#[test]
#[ignore = "native fixtures required"]
fn variable_callback_sizes_keep_sample_and_event_positions() {
    let mut instance = host()
        .spawn(&plugin(PluginFormat::Vst3), &Configuration::default(), None)
        .unwrap();
    let mut audio = instance.take_audio().unwrap();
    let sizes = [17, 31, 80, 128, 41, 87];
    let mut position = 0;
    let mut samples = Vec::new();
    let mut event_position = None;
    for size in sizes {
        let mut l = vec![0.0; size];
        let mut r = vec![0.0; size];
        let events = if position == 0 {
            vec![AudioEvent::midi(5, 0, [0x90, 60, 127])]
        } else {
            vec![]
        };
        let mut output = [AudioEvent::default(); 16];
        let report = assert_no_alloc::assert_no_alloc(|| {
            audio.process_events(
                size,
                &[],
                &mut [&mut l, &mut r],
                Transport::default().advanced(position, 48000),
                &events,
                &mut output,
            )
        });
        for event in &output[..report.events_written] {
            event_position = Some(position + event.sample_offset as usize);
        }
        samples.extend(l);
        position += size;
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(event_position, Some(261));
    assert!(samples[..261].iter().all(|v| *v == 0.0));
    assert!((samples[261] - 0.12345679).abs() < 1e-7);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "native fixtures and Xvfb required"]
fn x11_disconnect_closes_editor_and_preserves_audio() {
    const READY: &str = "VST_AUDIO_TEST_X11_DISCONNECT_READY";
    if let Some(ready) = std::env::var_os(READY) {
        let plugin = PluginDescriptor::new(
            PathBuf::from(std::env::var_os("VST_AUDIO_TEST_VST3").unwrap()),
            PluginFormat::Vst3,
        );
        let mut instance = host()
            .spawn(
                &plugin,
                &Configuration {
                    linux_editor: LinuxEditor::X11,
                    recovery: RecoveryPolicy::Manual,
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        let mut audio = instance.take_audio().unwrap();
        let generation = audio.generation();
        let request = instance.controller().request(Command::OpenEditor).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(event) = instance.try_next_event() {
                match event {
                    HostEvent::Completed { request: id } if id == request => break,
                    HostEvent::Failed { .. } | HostEvent::RequestFailed { .. } => {
                        panic!("editor open failed: {event:?}");
                    }
                    _ => {}
                }
            }
            assert!(Instant::now() < deadline, "editor open timed out");
            std::thread::sleep(Duration::from_millis(5));
        }

        // The basic fixture uses x11rb, not Xlib's fatal I/O-error handler.
        // Both its child connection and the host connection target this server.
        let left = [0.25; 128];
        let right = [-0.125; 128];
        let mut samples = [[0.0; 128]; 6];
        let mut output: Vec<_> = samples.iter_mut().map(|p| &mut p[..]).collect();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut warmed = 0;
        while warmed < 4 {
            if audio.process(&[&left, &right], &mut output) == AudioStatus::Processed {
                warmed += 1;
            }
            assert!(Instant::now() < deadline, "pre-disconnect audio timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
        std::fs::write(ready, b"editor open and audio running").unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut closed = 0;
        let mut processed = 0;
        while closed == 0 || processed < 10 {
            while let Some(event) = instance.try_next_event() {
                match event {
                    HostEvent::EditorClosed => closed += 1,
                    HostEvent::Failed { .. }
                    | HostEvent::RecoveryRequired
                    | HostEvent::Recovered { .. }
                    | HostEvent::Stopped => panic!("worker did not survive: {event:?}"),
                    _ => {}
                }
            }
            if audio.process(&[&left, &right], &mut output) == AudioStatus::Processed && closed != 0
            {
                processed += 1;
                for (plane, expected) in output
                    .iter()
                    .zip([0.25, -0.125, 0.0625, 0.0625, 0.0625, -0.03125])
                {
                    assert!(plane.iter().all(|value| (*value - expected).abs() < 1e-6));
                }
            }
            assert!(
                Instant::now() < deadline,
                "missing EditorClosed or continued audio after X11 disconnect: closed={closed}, processed={processed}"
            );
            assert_eq!(instance.status(), Status::Running);
            assert_eq!(audio.generation(), generation, "worker must not restart");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(closed, 1, "one disconnect must emit one EditorClosed");
        eprintln!(
            "EditorClosed={closed}; post-disconnect numeric blocks={processed}; generation={generation}"
        );
        return;
    }

    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let display_file = directory.path().join("display");
    let ready = directory.path().join("ready");
    let mut server = ChildGuard(
        std::process::Command::new("Xvfb")
            .args([
                "-displayfd",
                "1",
                "-screen",
                "0",
                "800x600x24",
                "-nolisten",
                "tcp",
                "-ac",
            ])
            .stdout(std::fs::File::create(&display_file).unwrap())
            .spawn()
            .expect("private Xvfb required"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let display = loop {
        let display = std::fs::read_to_string(&display_file).unwrap();
        if display.ends_with('\n') {
            let number: u32 = display.trim().parse().unwrap();
            break format!(":{number}");
        }
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "Xvfb exited during startup"
        );
        assert!(Instant::now() < deadline, "Xvfb startup timed out");
        std::thread::sleep(Duration::from_millis(5));
    };
    // Only the child test and its worker inherit DISPLAY; other tests keep theirs.
    let mut child = ChildGuard(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "x11_disconnect_closes_editor_and_preserves_audio",
                "--ignored",
                "--nocapture",
            ])
            .env("DISPLAY", display)
            .env(READY, &ready)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ready.exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "child failed before X11 disconnect"
        );
        assert!(Instant::now() < deadline, "child never opened its editor");
        std::thread::sleep(Duration::from_millis(5));
    }
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(
                status.success(),
                "X11 disconnect regression failed: {status}"
            );
            break;
        }
        assert!(Instant::now() < deadline, "child hung after X11 disconnect");
        std::thread::sleep(Duration::from_millis(5));
    }
}
