#![forbid(unsafe_code)]
//! Independent plugin implementations: assertions inspect samples/events, not
//! merely successful loading. Upstream binaries never become test artifacts.
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use vst_audio::*;

#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

fn host() -> Host {
    Host::new(PathBuf::from(
        std::env::var_os("VST_AUDIO_WORKER").expect("worker required"),
    ))
}
fn descriptor(name: &str, format: PluginFormat) -> PluginDescriptor {
    let suffix = if format == PluginFormat::Vst2 {
        "VST2"
    } else {
        "VST3"
    };
    PluginDescriptor::new(
        PathBuf::from(
            std::env::var_os(format!("VST_AUDIO_REAL_{name}_{suffix}"))
                .expect("independent plugin required"),
        ),
        format,
    )
}
fn metric(case: &str, expected: f64, observed: f64) {
    println!("METRIC {{\"case\":\"{case}\",\"expected\":{expected},\"observed\":{observed}}}");
}
fn wait(instance: &Instance, id: RequestId) -> HostEvent {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if let Some(event) = instance.try_next_event() {
            assert!(!matches!(event, HostEvent::Failed { .. }), "{event:?}");
            let request = match &event {
                HostEvent::Completed { request }
                | HostEvent::RequestFailed { request, .. }
                | HostEvent::Parameters { request, .. } => Some(*request),
                _ => None,
            };
            if request == Some(id) {
                return event;
            }
        }
        assert!(Instant::now() < deadline, "request timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn parameter(instance: &Instance, name: &str, value: f64) {
    let mut start = 0;
    let id = loop {
        let request = instance
            .controller()
            .request(Command::ListParameters { start, count: 32 })
            .unwrap();
        let HostEvent::Parameters { total, entries, .. } = wait(instance, request) else {
            panic!("missing parameters")
        };
        if let Some(p) = entries.iter().find(|p| p.name == name && !p.read_only) {
            break p.id;
        }
        start += 32;
        assert!(start < total, "parameter {name} missing");
    };
    let id = instance
        .controller()
        .request(Command::SetParameter { id, value })
        .unwrap();
    assert!(matches!(wait(instance, id), HostEvent::Completed { .. }));
}
fn pause() {
    std::thread::sleep(Duration::from_millis(20));
}

#[test]
#[ignore = "built independent plugins required"]
fn dpf_midi_through_preserves_types_channels_values_and_sample_offsets() {
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let mut instance = host()
            .spawn(&descriptor("MIDI", format), &Configuration::default(), None)
            .unwrap();
        let mut audio = instance.take_audio().unwrap();
        assert_eq!(
            (
                audio.geometry().input_channels,
                audio.geometry().output_channels
            ),
            (0, 0)
        );
        assert_eq!(
            (
                audio.capabilities().input_event_buses,
                audio.capabilities().output_event_buses
            ),
            (1, 1)
        );
        let mut input = Vec::new();
        for channel in 0..16u8 {
            for (index, bytes) in [
                [0x90 | channel, 60, 110],
                [0xa0 | channel, 60, 83],
                [0xb0 | channel, 1, 95],
                [0xd0 | channel, 73, 0],
                [0xe0 | channel, 0, 64],
                [0x80 | channel, 60, 64],
            ]
            .into_iter()
            .enumerate()
            {
                input.push(AudioEvent::midi(
                    u32::from(channel) * 7 + index as u32 + 1,
                    0,
                    bytes,
                ));
            }
        }
        let mut received = [AudioEvent::default(); MAX_EVENTS];
        for block in 0..3 {
            let report = assert_no_alloc::assert_no_alloc(|| {
                audio.process_events(
                    128,
                    &[],
                    &mut [],
                    Transport::default().advanced(block * 128, 48_000),
                    if block == 0 { &input } else { &[] },
                    &mut received,
                )
            });
            if block == 2 {
                assert_eq!(report.status, AudioStatus::Processed);
                assert_eq!(report.events_dropped, 0);
                assert_eq!(report.events_written, input.len());
                assert_eq!(&received[..report.events_written], &input);
                metric(
                    &format!("DPF MIDI {format:?} exact event count"),
                    input.len() as f64,
                    report.events_written as f64,
                );
            }
            pause();
        }
        if format == PluginFormat::Vst3 {
            let mut bytes = [0; MAX_SYSEX_BYTES];
            bytes[..7].copy_from_slice(&[0xf0, 0x7d, 1, 2, 3, 4, 0xf7]);
            let sysex = [AudioEvent {
                sample_offset: 23,
                bus: 0,
                data: EventData::SysEx { len: 7, bytes },
            }];
            for block in 0..3 {
                let report = audio.process_events(
                    128,
                    &[],
                    &mut [],
                    Transport::default(),
                    if block == 0 { &sysex } else { &[] },
                    &mut received,
                );
                if block == 2 {
                    assert_eq!(report.status, AudioStatus::Processed);
                    // Pinned upstream appendEvent excludes V3_EVENT_DATA and its
                    // SysexData conversion is TODO. Record that independent
                    // limitation; host drop counts cannot detect plugin-internal
                    // filtering. Our two native fixtures verify owned SysEx.
                    assert_eq!((report.events_written, report.events_dropped), (0, 0));
                    metric(
                        "known limitation: DPF VST3 SysEx echoes",
                        0.0,
                        report.events_written as f64,
                    );
                }
                pause();
            }
        }
        // This independent VST3 adapter exposes CC/pressure/bend mappings, but
        // deliberately has no program-change assignment. The host must report
        // the unsupported event instead of silently changing a different control.
        let program = [AudioEvent::midi(37, 0, [0xc2, 19, 0])];
        for block in 0..3 {
            let report = audio.process_events(
                128,
                &[],
                &mut [],
                Transport::default(),
                if block == 0 { &program } else { &[] },
                &mut received,
            );
            if block == 2 {
                if format == PluginFormat::Vst3 {
                    assert_eq!((report.events_written, report.events_dropped), (0, 1));
                } else {
                    assert_eq!((report.events_written, report.events_dropped), (1, 0));
                    assert_eq!(received[0], program[0]);
                }
                metric(
                    &format!("DPF MIDI {format:?} program rejected"),
                    if format == PluginFormat::Vst3 {
                        1.0
                    } else {
                        0.0
                    },
                    f64::from(report.events_dropped),
                );
            }
            pause();
        }
    }
}

#[test]
#[ignore = "built independent plugins required"]
fn dpf_latency_has_numeric_audio_and_rejects_unsupported_negotiation() {
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let plugin = descriptor("LATENCY", format);
        let mut instance = host()
            .spawn(&plugin, &Configuration::default(), None)
            .unwrap();
        if format == PluginFormat::Vst3 {
            let id = instance
                .controller()
                .request(Command::ListParameters {
                    start: 0,
                    count: 32,
                })
                .unwrap();
            let HostEvent::Parameters { entries, .. } = wait(&instance, id) else {
                panic!("missing parameter attributes")
            };
            let readonly = entries
                .iter()
                .find(|p| p.name == "Latency" && p.read_only)
                .expect("DPF internal latency meter");
            assert!(readonly.hidden);
            let id = instance
                .controller()
                .request(Command::SetParameter {
                    id: readonly.id,
                    value: 0.5,
                })
                .unwrap();
            assert!(matches!(
                wait(&instance, id),
                HostEvent::RequestFailed { .. }
            ));
            metric("DPF VST3 read-only parameter rejected", 1.0, 1.0);
        }
        parameter(&instance, "Latency", 0.0);
        let mut audio = instance.take_audio().unwrap();
        assert_eq!(
            (
                audio.geometry().input_channels,
                audio.geometry().output_channels
            ),
            (1, 1)
        );
        let source: Vec<_> = (0..128).map(|i| (i as f32 - 64.0) / 128.0).collect();
        let mut output = [0.0; 128];
        for block in 0..4 {
            let status =
                assert_no_alloc::assert_no_alloc(|| audio.process(&[&source], &mut [&mut output]));
            if block >= 2 {
                assert_eq!(status, AudioStatus::Processed);
                let error = source
                    .iter()
                    .zip(output)
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0f32, f32::max);
                assert!(error < 1e-6, "{format:?}: {error}");
                if block == 3 {
                    metric(
                        &format!("DPF Latency {format:?} passthrough max error"),
                        0.0,
                        f64::from(error),
                    );
                }
            }
            pause();
        }
        let bad = Configuration {
            inputs: Some(vec![
                Bus {
                    channels: 2,
                    arrangement: 3,
                }
                .into(),
            ]),
            ..Default::default()
        };
        let error = host().spawn(&plugin, &bad, None).unwrap_err();
        assert!(
            error.to_string().contains("arrangement")
                || error.to_string().contains("channel")
                || error.to_string().contains("configuration"),
            "{error}"
        );
        metric(&format!("DPF Latency {format:?} stereo rejected"), 1.0, 1.0);
        assert_eq!(instance.status(), Status::Running);
        let precision = Configuration {
            sample_format: SampleFormat::F64,
            ..Default::default()
        };
        assert!(host().spawn(&plugin, &precision, None).is_err());
        metric(&format!("DPF Latency {format:?} f64 rejected"), 1.0, 1.0);
    }
}

#[test]
#[ignore = "built independent plugins required"]
fn steinberg_adelay_negotiates_mono_stereo_surround_and_delays_exactly_one_sample() {
    let plugin = descriptor("DELAY", PluginFormat::Vst3);
    for (channels, arrangement) in [(1, 1 << 19), (2, 3), (6, 63)] {
        let config = Configuration {
            inputs: Some(vec![
                Bus {
                    channels,
                    arrangement,
                }
                .into(),
            ]),
            outputs: Some(vec![
                Bus {
                    channels,
                    arrangement,
                }
                .into(),
            ]),
            ..Default::default()
        };
        let mut instance = host().spawn(&plugin, &config, None).unwrap();
        parameter(&instance, "Delay", 0.0);
        let mut audio = instance.take_audio().unwrap();
        assert_eq!(audio.layout().inputs, config.inputs.unwrap());
        assert_eq!(audio.layout().outputs, config.outputs.unwrap());
        let source: Vec<_> = (0..usize::from(channels))
            .map(|channel| {
                (0..128)
                    .map(|i| (i + channel * 3) as f32 / 512.0)
                    .collect::<Vec<_>>()
            })
            .collect();
        let refs: Vec<_> = source.iter().map(|p| p.as_slice()).collect();
        let mut output = vec![vec![0.0; 128]; usize::from(channels)];
        let mut outs: Vec<_> = output.iter_mut().map(|p| p.as_mut_slice()).collect();
        let mut max_error = 0.0f32;
        for block in 0..4 {
            let status = assert_no_alloc::assert_no_alloc(|| audio.process(&refs, &mut outs));
            if block >= 2 {
                assert_eq!(status, AudioStatus::Processed);
                for (channel, plane) in outs.iter().enumerate() {
                    for (i, value) in plane.iter().enumerate() {
                        let expected = if i == 0 {
                            if block == 2 {
                                0.0
                            } else {
                                source[channel][127]
                            }
                        } else {
                            source[channel][i - 1]
                        };
                        max_error = max_error.max((value - expected).abs());
                    }
                }
            }
            pause();
        }
        assert!(max_error < 1e-6, "{channels} channels: {max_error}");
        metric(
            &format!("ADelay {channels} channels max error"),
            0.0,
            f64::from(max_error),
        );
    }
    let mismatch = Configuration {
        inputs: Some(vec![
            Bus {
                channels: 1,
                arrangement: 1 << 19,
            }
            .into(),
        ]),
        outputs: Some(vec![
            Bus {
                channels: 2,
                arrangement: 3,
            }
            .into(),
        ]),
        ..Default::default()
    };
    let error = host().spawn(&plugin, &mismatch, None).unwrap_err();
    assert!(error.to_string().contains("arrangement"), "{error}");
    metric("ADelay mismatched input/output rejected", 1.0, 1.0);
}

#[test]
#[ignore = "built independent plugins required"]
fn mda_dx10_midi_starts_at_requested_sample_and_is_repeatable() {
    let mut plugin = descriptor("MDA", PluginFormat::Vst3);
    let classes = host().scan(&plugin).unwrap();
    assert!(classes.len() >= 20);
    plugin.class_id = Some(
        classes
            .iter()
            .find(|c| c.name.contains("DX10"))
            .expect("mda DX10 class")
            .id,
    );
    let mut recordings = Vec::new();
    for _ in 0..2 {
        let mut instance = host()
            .spawn(&plugin, &Configuration::default(), None)
            .unwrap();
        let mut audio = instance.take_audio().unwrap();
        assert_eq!(
            (
                audio.geometry().input_channels,
                audio.geometry().output_channels
            ),
            (0, 2)
        );
        let events = [
            AudioEvent::midi(0, 0, [0xc0, 1, 0]),
            AudioEvent::midi(31, 0, [0x90, 60, 110]),
            AudioEvent::midi(101, 0, [0x80, 60, 64]),
        ];
        let mut left = [0.0; 128];
        let mut right = [0.0; 128];
        let mut received = [AudioEvent::default(); MAX_EVENTS];
        let mut samples = Vec::new();
        for block in 0..6 {
            let report = assert_no_alloc::assert_no_alloc(|| {
                audio.process_events(
                    128,
                    &[],
                    &mut [&mut left, &mut right],
                    Transport::default().advanced(block * 128, 48_000),
                    if block == 0 { &events } else { &[] },
                    &mut received,
                )
            });
            if block >= 2 {
                assert_eq!(report.status, AudioStatus::Processed);
                assert_eq!(report.events_dropped, 0);
                assert!(left.iter().chain(&right).all(|s| s.is_finite()));
                samples.extend_from_slice(&left);
            }
            pause();
        }
        assert!(samples[..31].iter().all(|s| *s == 0.0));
        let peak = samples[31..101]
            .iter()
            .map(|s| s.abs())
            .fold(0.0f32, f32::max);
        assert!(peak > 1e-5, "instrument did not sound: {peak}");
        metric("mda DX10 prefix silent samples", 31.0, 31.0);
        recordings.push(samples);
    }
    let error = recordings[0]
        .iter()
        .zip(&recordings[1])
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(error < 1e-6, "fresh instances disagree: {error}");
    metric("mda DX10 fresh-instance max error", 0.0, f64::from(error));
}
