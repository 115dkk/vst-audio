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

#[cfg(target_os = "macos")]
#[test]
#[ignore = "native editor input is unsupported"]
fn editor_input_macos_limitation() {
    metric("known limitation: macOS synthetic editor input", 0.0, 0.0);
}

#[cfg(any(target_os = "linux", windows))]
mod editor_input {
    use super::*;
    use vst_audio_test_window::Parent;

    struct Editor {
        instance: Instance,
        parent: Parent,
        edits: Vec<(u32, EditPhase, f64)>,
        size: (u32, u32),
        closed: bool,
    }
    impl Editor {
        fn open(name: &str, format: PluginFormat, sandbox: bool) -> Self {
            let parent = Parent::create(900, 600);
            let config = Configuration {
                quantum: 512,
                sandbox: sandbox.then(SandboxPolicy::default),
                ..Default::default()
            };
            let instance = host()
                .spawn(&descriptor(name, format), &config, None)
                .unwrap();
            parent.expect_input_process(instance.controller().worker_pid());
            let mut editor = Self {
                instance,
                parent,
                edits: Vec::new(),
                size: (0, 0),
                closed: false,
            };
            editor.request(Command::OpenEditorIn(editor.parent.handle()));
            let event = editor.request(Command::Editor(EditorAction::Inspect));
            let HostEvent::EditorStatus { status, .. } = event else {
                panic!("missing editor status: {event:?}")
            };
            assert!(status.embedded);
            editor.size = (status.width, status.height);
            editor.collect(Duration::from_millis(300));
            assert!(
                editor.edits.is_empty(),
                "editor edited without input: {:?}",
                editor.edits
            );
            editor
        }
        fn observe(&mut self, event: &HostEvent) {
            match event {
                HostEvent::ParameterEdited { id, phase, value } => {
                    self.edits.push((*id, *phase, *value))
                }
                HostEvent::Failed { .. } | HostEvent::RequestFailed { .. } => panic!("{event:?}"),
                _ => {}
            }
        }
        fn request(&mut self, command: Command) -> HostEvent {
            let id = self.instance.controller().request(command).unwrap();
            let deadline = Instant::now() + Duration::from_secs(12);
            loop {
                self.parent.pump();
                if let Some(event) = self.instance.try_next_event() {
                    self.observe(&event);
                    let request = match &event {
                        HostEvent::Completed { request }
                        | HostEvent::Parameters { request, .. }
                        | HostEvent::EditorStatus { request, .. } => Some(*request),
                        HostEvent::State { request, .. } => *request,
                        _ => None,
                    };
                    if request == Some(id) {
                        return event;
                    }
                }
                assert!(Instant::now() < deadline, "editor request timed out");
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        fn collect(&mut self, duration: Duration) {
            let deadline = Instant::now() + duration;
            while Instant::now() < deadline {
                self.parent.pump();
                while let Some(event) = self.instance.try_next_event() {
                    self.observe(&event);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        fn close(&mut self) {
            if !self.closed {
                self.request(Command::CloseEditor);
                self.closed = true;
            }
        }
        fn parameters(&mut self) -> Vec<Parameter> {
            let mut result = Vec::new();
            loop {
                let HostEvent::Parameters { total, entries, .. } =
                    self.request(Command::ListParameters {
                        start: result.len() as u32,
                        count: 32,
                    })
                else {
                    panic!("missing parameters")
                };
                result.extend(entries);
                if result.len() == total as usize {
                    return result;
                }
            }
        }
        fn gesture(&mut self, id: u32, value: f64, multiple: bool, case: &str) {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !self.edits.iter().any(|e| e.1 == EditPhase::End) {
                self.collect(Duration::from_millis(5));
                assert!(
                    Instant::now() < deadline,
                    "gesture timed out: {:?}",
                    self.edits
                );
            }
            self.collect(Duration::from_millis(80));
            let observed: Vec<_> = self
                .edits
                .iter()
                .map(|(id, phase, value)| {
                    format!("{{\"id\":{id},\"phase\":\"{phase:?}\",\"value\":{value}}}")
                })
                .collect();
            println!(
                "METRIC {{\"case\":\"{case} gesture\",\"edits\":[{}]}}",
                observed.join(",")
            );
            assert!(self.edits.iter().all(|edit| edit.0 == id));
            assert_eq!(self.edits.first().unwrap().1, EditPhase::Begin);
            assert_eq!(self.edits.last().unwrap().1, EditPhase::End);
            let performs = &self.edits[1..self.edits.len() - 1];
            assert!(!performs.is_empty());
            assert!(performs.iter().all(|edit| edit.1 == EditPhase::Perform));
            if !multiple {
                assert_eq!(performs.len(), 1);
            }
            assert!((performs.last().unwrap().2 - value).abs() < 1e-5);
            self.edits.clear();
        }
        fn pixel(&self, point: (i32, i32)) -> [u8; 3] {
            let image = self.parent.capture();
            let i = (point.1 as usize * image.width as usize + point.0 as usize) * 4;
            image.rgba[i..i + 3].try_into().unwrap()
        }
        fn color(&mut self, point: (i32, i32), expected: [u8; 3]) -> [u8; 3] {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let actual = self.pixel(point);
                if actual
                    .iter()
                    .zip(expected)
                    .all(|(a, b)| a.abs_diff(b) <= 12)
                {
                    return actual;
                }
                if Instant::now() >= deadline {
                    // Name the colors the capture did contain: a blank editor, the
                    // wrong window and a shifted layout read differently.
                    let image = self.parent.capture();
                    let mut counts = std::collections::HashMap::<[u8; 3], usize>::new();
                    for pixel in image.rgba.chunks_exact(4) {
                        *counts.entry([pixel[0], pixel[1], pixel[2]]).or_default() += 1;
                    }
                    let mut counts: Vec<_> = counts.into_iter().collect();
                    counts.sort_by(|a, b| b.1.cmp(&a.1));
                    counts.truncate(5);
                    panic!(
                        "pixel {point:?}: {actual:?}, expected {expected:?}; capture {}x{} top colors {counts:?}",
                        image.width, image.height
                    );
                }
                self.collect(Duration::from_millis(20));
            }
        }
    }
    impl Drop for Editor {
        fn drop(&mut self) {
            self.close();
        }
    }
    fn grid_values(editor: &mut Editor, on: Option<&str>) -> Vec<(u32, f64)> {
        let parameters = editor.parameters();
        let grid: Vec<_> = parameters
            .iter()
            .filter(|p| p.name.contains('-') && !p.read_only)
            .collect();
        assert_eq!(grid.len(), 9);
        for p in &grid {
            assert_eq!(
                p.value,
                if Some(p.name.as_str()) == on {
                    1.0
                } else {
                    0.0
                },
                "{}",
                p.name
            );
        }
        grid.iter().map(|p| (p.id, p.value)).collect()
    }
    pub(super) fn parameters(format: PluginFormat, sandbox: bool) {
        let mut editor = Editor::open("PARAMETERS", format, sandbox);
        let mut audio = editor.instance.take_audio().unwrap();
        let case = format!("Parameters {format:?} sandbox={sandbox}");
        let parameters = editor.parameters();
        let id = parameters
            .iter()
            .find(|p| p.name == "middle-right")
            .expect("middle-right parameter")
            .id;
        let first = parameters.iter().find(|p| p.name == "top-left").unwrap().id;
        grid_values(&mut editor, None);
        let m = editor.size.0.min(editor.size.1) as i32;
        let point = (m * 5 / 6, m / 2);
        let off = editor.color(point, [77, 128, 204]);
        editor.parent.click(point.0, point.1);
        editor.gesture(id, 1.0, false, &case);
        let values: Vec<_> = grid_values(&mut editor, Some("middle-right"))
            .into_iter()
            .map(|(id, value)| [f64::from(id), value])
            .collect();
        let on = editor.color(point, [204, 128, 77]);
        println!(
            "METRIC {{\"case\":\"{case} pixels\",\"off\":{off:?},\"on\":{on:?},\"values\":{values:?}}}"
        );
        editor.parent.click(point.0, point.1);
        editor.gesture(id, 0.0, false, &case);
        grid_values(&mut editor, None);
        editor.color(point, [77, 128, 204]);
        let before = editor.pixel(point);
        editor.parent.click(m / 3, m / 2);
        editor.collect(Duration::from_millis(500));
        assert!(editor.edits.is_empty());
        grid_values(&mut editor, None);
        assert_eq!(editor.pixel(point), before);
        metric(&format!("{case} gap edits"), 0.0, editor.edits.len() as f64);
        editor.request(Command::SetParameter {
            id: first,
            value: 1.0,
        });
        let host_pixel = editor.color((m / 6, m / 6), [204, 128, 77]);
        editor.collect(Duration::from_millis(500));
        assert!(editor.edits.is_empty());
        let inputs = vec![vec![0.0; 512]; audio.geometry().input_channels as usize];
        let mut outputs = vec![vec![0.0; 512]; audio.geometry().output_channels as usize];
        let input: Vec<_> = inputs.iter().map(Vec::as_slice).collect();
        let mut output: Vec<_> = outputs.iter_mut().map(Vec::as_mut_slice).collect();
        for _ in 0..4 {
            let _ = audio.process(&input, &mut output);
            pause();
        }
        let expected = grid_values(&mut editor, Some("top-left"));
        let HostEvent::State { bytes, .. } = editor.request(Command::SaveState) else {
            panic!("state missing")
        };
        editor.close();
        let mut restored = Editor::open("PARAMETERS", format, sandbox);
        restored.request(Command::RestoreState(bytes));
        restored.collect(Duration::from_millis(100));
        assert_eq!(grid_values(&mut restored, Some("top-left")), expected);
        assert!(restored.edits.is_empty());
        println!(
            "METRIC {{\"case\":\"{case} host-to-UI and state\",\"pixel\":{host_pixel:?},\"edits\":0,\"restored\":true}}"
        );
    }
    fn rms(samples: &[f32]) -> f64 {
        (samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    }
    fn blocks(
        editor: &Editor,
        audio: &mut AudioEndpoint,
        frequency: f64,
        count: usize,
    ) -> Vec<f32> {
        let inputs = audio.geometry().input_channels as usize;
        let outputs = audio.geometry().output_channels as usize;
        let mut recorded = Vec::new();
        for block in 0..count {
            let source: Vec<f32> = (0..512)
                .map(|i| {
                    (10f64.powf(-12.0 / 20.0)
                        * (std::f64::consts::TAU * frequency * (block * 512 + i) as f64 / 48_000.0)
                            .sin()) as f32
                })
                .collect();
            let refs = vec![source.as_slice(); inputs];
            let mut result = vec![vec![0.0; 512]; outputs];
            let mut outs: Vec<_> = result.iter_mut().map(Vec::as_mut_slice).collect();
            let status = audio.process(&refs, &mut outs);
            if block >= 2 {
                assert_eq!(status, AudioStatus::Processed);
                assert!(result[0].iter().all(|s| s.is_finite()));
                recorded.extend_from_slice(&result[0]);
            }
            editor.parent.settle();
        }
        recorded
    }
    pub(super) fn send_note(format: PluginFormat) {
        let mut editor = Editor::open("SENDNOTE", format, false);
        let mut audio = editor.instance.take_audio().unwrap();
        assert_eq!(
            (
                audio.geometry().input_channels,
                audio.geometry().output_channels
            ),
            (0, 1)
        );
        let silence = blocks(&editor, &mut audio, 0.0, 8);
        let peak = |s: &[f32]| s.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        assert!(peak(&silence) < 1e-6);
        // getKeyBounds(9) = (584,8,56,56), MIDI 60+9 = A4.
        editor.parent.press(612, 36);
        let sound = blocks(&editor, &mut audio, 0.0, 20);
        editor.parent.release(612, 36);
        let released = blocks(&editor, &mut audio, 0.0, 8);
        let db = 20.0 * rms(&sound).log10();
        let crossings: Vec<f64> = sound
            .windows(2)
            .enumerate()
            .filter(|(_, s)| s[0] <= 0.0 && s[1] > 0.0)
            .map(|(i, s)| i as f64 - f64::from(s[0]) / f64::from(s[1] - s[0]))
            .collect();
        assert!(crossings.len() > 2, "UI note did not reach processor");
        let hz =
            (crossings.len() - 1) as f64 * 48_000.0 / (crossings.last().unwrap() - crossings[0]);
        println!(
            "METRIC {{\"case\":\"SendNote {format:?}\",\"rms_dbfs\":{db},\"hz\":{hz},\"before_peak\":{},\"release_peak\":{}}}",
            peak(&silence),
            peak(&released[released.len() - 512..])
        );
        assert!(db > -40.0);
        assert!((hz / 440.0 - 1.0).abs() < 0.01);
        assert!(peak(&released[released.len() - 512..]) < 1e-6);
    }
    fn levels(editor: &Editor, audio: &mut AudioEndpoint) -> [f64; 2] {
        [100.0, 8000.0].map(|frequency| {
            let samples = blocks(editor, audio, frequency, 28);
            20.0 * rms(&samples[8 * 512..]).log10()
        })
    }
    pub(super) fn eq(format: PluginFormat) {
        let mut editor = Editor::open("EQ", format, false);
        let mut audio = editor.instance.take_audio().unwrap();
        let parameters = editor.parameters();
        let low = parameters
            .iter()
            .find(|p| p.name == "Low")
            .expect("Low parameter");
        let id = low.id;
        let initial = low.value;
        let before = levels(&editor, &mut audio);
        // Slider start (57,43), end (57,203), image 50x30; default 0 dB is halfway.
        editor.parent.drag((82, 138), (82, 98), 12);
        let after_parameters = editor.parameters();
        let value = after_parameters.iter().find(|p| p.id == id).unwrap().value;
        assert!(value > initial);
        editor.gesture(id, value, true, &format!("3 Band EQ {format:?}"));
        let after = levels(&editor, &mut audio);
        let delta = [after[0] - before[0], after[1] - before[1]];
        println!(
            "METRIC {{\"case\":\"3 Band EQ {format:?} sound\",\"before_db\":{before:?},\"after_db\":{after:?},\"delta_db\":{delta:?},\"low_before\":{initial},\"low_after\":{value}}}"
        );
        assert!(
            delta[0] >= 3.0 && delta[1].abs() <= 0.5,
            "editor-to-processor sound assertion failed: low/high delta {delta:?}"
        );
        editor.parent.drag((360, 100), (360, 60), 12);
        editor.collect(Duration::from_millis(500));
        assert!(editor.edits.is_empty());
        let negative_parameters = editor.parameters();
        assert_eq!(
            negative_parameters
                .iter()
                .map(|p| (p.id, p.value))
                .collect::<Vec<_>>(),
            after_parameters
                .iter()
                .map(|p| (p.id, p.value))
                .collect::<Vec<_>>()
        );
        let negative = levels(&editor, &mut audio);
        for i in 0..2 {
            assert!((negative[i] - after[i]).abs() < 0.05);
        }
        println!(
            "METRIC {{\"case\":\"3 Band EQ {format:?} background drag\",\"edits\":0,\"levels_db\":{negative:?}}}"
        );
    }
}

#[cfg(any(target_os = "linux", windows))]
#[test]
#[ignore = "built independent plugins and native desktop required"]
fn editor_input_parameters_vst2() {
    editor_input::parameters(PluginFormat::Vst2, false);
}
#[cfg(any(target_os = "linux", windows))]
#[test]
#[ignore = "built independent plugins and native desktop required"]
fn editor_input_parameters_vst3() {
    editor_input::parameters(PluginFormat::Vst3, false);
}
#[cfg(target_os = "linux")]
#[test]
#[ignore = "built independent plugins and native desktop required"]
fn editor_input_parameters_sandbox_vst2() {
    editor_input::parameters(PluginFormat::Vst2, true);
}
#[cfg(target_os = "linux")]
#[test]
#[ignore = "built independent plugins and native desktop required"]
fn editor_input_parameters_sandbox_vst3() {
    editor_input::parameters(PluginFormat::Vst3, true);
}
#[cfg(any(target_os = "linux", windows))]
#[test]
#[ignore = "built independent plugins and native desktop required"]
fn editor_input_send_note_vst2() {
    editor_input::send_note(PluginFormat::Vst2);
}
#[cfg(any(target_os = "linux", windows))]
#[test]
#[ignore = "built independent plugins and native desktop required"]
fn editor_input_send_note_vst3() {
    editor_input::send_note(PluginFormat::Vst3);
}
#[cfg(any(target_os = "linux", windows))]
#[test]
#[ignore = "built independent plugins and native desktop required"]
fn editor_input_eq_vst2() {
    editor_input::eq(PluginFormat::Vst2);
}
#[cfg(any(target_os = "linux", windows))]
#[test]
#[ignore = "built independent plugins and native desktop required"]
fn editor_input_eq_vst3() {
    editor_input::eq(PluginFormat::Vst3);
}
