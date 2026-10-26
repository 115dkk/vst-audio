#![forbid(unsafe_code)]
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use vst_audio::{
    AudioStatus, Command, Configuration, ControlEvent, Host, HostEvent, PluginDescriptor,
    PluginFormat, PluginProcess, RecoveryPolicy, Started,
};
use vst_audio_protocol::{EditorAction, Message};
use vst_audio_ring::{Publish, Wait};
fn paths() -> (PathBuf, PathBuf) {
    (
        std::env::var_os("VST_AUDIO_WORKER").unwrap().into(),
        std::env::var_os("VST_AUDIO_TEST_VST2").unwrap().into(),
    )
}
fn start(mode: u8) -> Started {
    let (worker, plugin) = paths();
    PluginProcess::start(
        &worker,
        &plugin,
        PluginFormat::Vst2,
        None,
        48000,
        Some(&[1, mode]),
    )
    .unwrap()
}
fn wait(s: &mut Started, mut accepts: impl FnMut(&Message) -> bool) -> Message {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        for event in s.control.poll(s.control.elapsed_ms()) {
            if let ControlEvent::Message(frame) = event
                && accepts(&frame.message)
            {
                return frame.message;
            }
        }
        assert!(Instant::now() < deadline, "message timeout");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn state(s: &mut Started) -> Vec<u8> {
    s.control.get_state().unwrap();
    let Message::State { blob } = wait(s, |m| matches!(m, Message::State { .. })) else {
        unreachable!()
    };
    blob
}
fn observation(blob: &[u8], index: usize) -> u32 {
    u32::from_le_bytes(blob[2 + index * 4..6 + index * 4].try_into().unwrap())
}
fn block(s: &Started, sequence: u32) -> f32 {
    assert_eq!(
        s.publisher
            .try_publish(s.region.region(), 0, sequence, Some(&*s.work), |slot| {
                for f in 0..slot.frames() {
                    slot.write(0, f, 1.0);
                    slot.write(1, f, 1.0);
                }
            }),
        Publish::Published
    );
    let end = Instant::now() + Duration::from_secs(2);
    while s.publisher.completed(s.region.region(), 0) < sequence {
        assert!(Instant::now() < end);
        s.done.wait(Duration::from_millis(2));
    }
    let mut sample = 0.0;
    assert!(
        s.publisher
            .try_read(s.region.region(), 0, sequence, |slot| sample =
                slot.read(0, 100))
    );
    sample
}
#[test]
#[ignore = "native fixtures required"]
fn short_midi_header_is_dropped_and_worker_keeps_processing() {
    let (worker, plugin) = paths();
    let mut instance = Host::new(worker)
        .spawn(
            &PluginDescriptor::new(plugin, PluginFormat::Vst2),
            &Configuration::default(),
            Some(&[1, 21]),
        )
        .unwrap();
    let mut audio = instance.take_audio().unwrap();
    let generation = audio.generation();
    let input = [0.25f32; 128];
    let mut samples = [[0.0f32; 128]; 6];
    let mut outputs: Vec<_> = samples.iter_mut().map(|plane| &mut plane[..]).collect();
    let mut received = [vst_audio::AudioEvent::default(); 4];
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut processed = 0;
    while processed < 3 {
        let report = audio.process_events(
            128,
            &[&input, &input],
            &mut outputs,
            vst_audio::Transport::default(),
            &[],
            &mut received,
        );
        if report.status == AudioStatus::Processed {
            assert_eq!((report.events_written, report.events_dropped), (0, 1));
            assert_eq!(outputs[0][100], 0.25);
            assert_eq!(outputs[4][100], 0.125);
            assert_eq!(audio.generation(), generation);
            processed += 1;
        }
        assert_eq!(instance.status(), vst_audio::Status::Running);
        assert!(
            Instant::now() < deadline,
            "short MIDI stopped worker processing"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "native fixtures required"]
fn parameter_added_at_open_emits_edit_after_start() {
    let (worker, plugin) = paths();
    let mut descriptor = PluginDescriptor::new(plugin, PluginFormat::Vst2);
    // Fixture mode 22 must be selected before OPEN, not by a later state restore.
    descriptor.class_id = Some([b'V', b'S', b'T', b'2', 0, 0, 0, 22, 0, 0, 0, 0, 0, 0, 0, 0]);
    let mut instance = Host::new(worker)
        .spawn(&descriptor, &Configuration::default(), None)
        .unwrap();
    let mut audio = instance.take_audio().unwrap();
    let input = [0.25f32; 128];
    let mut samples = [[0.0f32; 128]; 6];
    let mut outputs: Vec<_> = samples.iter_mut().map(|plane| &mut plane[..]).collect();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut edits = Vec::new();
    let mut refreshes = 0;
    loop {
        audio.process(&[&input, &input], &mut outputs);
        while let Some(event) = instance.try_next_event() {
            match event {
                HostEvent::ParameterEdited { id, phase, value } => edits.push((id, phase, value)),
                HostEvent::ParametersChanged => refreshes += 1,
                HostEvent::Failed { reason } | HostEvent::RequestFailed { reason, .. } => {
                    panic!("{reason}")
                }
                _ => {}
            }
        }
        if !edits.is_empty() || refreshes != 0 || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        edits,
        vec![(1, vst_audio_protocol::EditPhase::Perform, 0.75)],
        "missing edit for parameter added at OPEN; coarse refreshes={refreshes}"
    );
    assert_eq!(
        refreshes, 0,
        "valid post-OPEN edit must not become a refresh"
    );
}

#[test]
#[ignore = "native fixtures required"]
fn callbacks_time_latency_and_opaque_state() {
    let mut s = start(7);
    block(&s, 1);
    let blob = state(&mut s);
    assert_eq!(observation(&blob, 0), 255);
    assert_eq!(observation(&blob, 1), 48000);
    assert_eq!(observation(&blob, 2), 48000);
    assert_eq!(observation(&blob, 3), 2);
    println!("canDo=255 (first eight yes, remaining seven no), OPEN/resume=48000, DSP level=2");
    s.control.set_state(&[1, 8]).unwrap();
    assert_eq!(
        wait(&mut s, |m| matches!(m, Message::LatencyChanged { .. })),
        Message::LatencyChanged { frames: 128 }
    );
    block(&s, 2);
    s.control.set_state(&[1, 10]).unwrap();
    wait(&mut s, |m| matches!(m, Message::ParamsChanged));
    s.control.set_state(b"SSV3test").unwrap();
    wait(&mut s, |m| matches!(m, Message::Ack));
    assert_eq!(state(&mut s), b"SSV3test");
    if std::env::var_os("VST_AUDIO_TEST_CONFIGURE_LATENCY").is_some() {
        let configured = start(16);
        assert_eq!(configured.latency_frames, 96);
        println!("configure latency=96 (native.rs before/after comparison)");
    }
    let mut resumed = start(11);
    // Startup queues notifications; poll once if the activation notification arrived after Ack.
    for _ in 0..10 {
        resumed.control.poll(resumed.control.elapsed_ms());
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(resumed.control.latency(), 192);
    println!(
        "ioChanged latency=128, processing continued; resume latency=192; SSV3 round-trip exact"
    );
}
#[test]
#[ignore = "native fixtures required"]
fn incompatible_counts_fault() {
    let mut s = start(7);
    block(&s, 1);
    s.control.set_state(&[1, 9]).unwrap();
    let Message::Fault { text, .. } = wait(&mut s, |m| matches!(m, Message::Fault { .. })) else {
        unreachable!()
    };
    assert!(text.contains("restart required"));
    println!("structural change fault: {text}");
}
#[test]
#[ignore = "native fixtures required"]
fn automatic_recovery_replaces_worker_after_structural_fault() {
    let (worker, plugin) = paths();
    let host = Host::new(worker);
    let config = Configuration {
        recovery: RecoveryPolicy::Automatic {
            attempts: 2,
            backoff_ms: 10,
        },
        ..Default::default()
    };
    let mut instance = host
        .spawn(
            &PluginDescriptor::new(plugin, PluginFormat::Vst2),
            &config,
            Some(&[1, 7]),
        )
        .unwrap();
    let mut audio = instance.take_audio().unwrap();
    let previous_generation = audio.generation();
    instance
        .controller()
        .request(Command::RestoreState(vec![1, 9]))
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    let input = [0.25f32; 128];
    let mut samples = [[0.0f32; 128]; 6];
    let mut outputs: Vec<_> = samples.iter_mut().map(|plane| &mut plane[..]).collect();
    let mut recovery_required = false;
    let recovered_generation = loop {
        audio.process(&[&input, &input], &mut outputs);
        if let Some(event) = instance.try_next_event() {
            match event {
                HostEvent::RecoveryRequired => recovery_required = true,
                HostEvent::Recovered { generation } => break generation,
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "automatic recovery timeout");
        std::thread::sleep(Duration::from_millis(2));
    };
    assert!(recovery_required);
    assert!(recovered_generation > previous_generation);

    loop {
        if audio.process(&[&input, &input], &mut outputs) == AudioStatus::Processed {
            assert!(audio.generation() > previous_generation);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "audio did not resume after recovery"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    println!(
        "structural fault recovery: generation {previous_generation} -> {recovered_generation}, audio processed"
    );
}

#[test]
#[ignore = "native fixtures required"]
fn editor_resize_and_idle_do_not_stop_audio() {
    let mut resized = start(13);
    resized.control.open_editor().unwrap();
    wait(&mut resized, |m| matches!(m, Message::Ack));
    resized
        .control
        .editor_action(EditorAction::Inspect)
        .unwrap();
    let Message::EditorStatus(status) =
        wait(&mut resized, |m| matches!(m, Message::EditorStatus(_)))
    else {
        unreachable!()
    };
    assert_eq!((status.width, status.height), (480, 260));
    let mut resized = start(14);
    resized.control.open_editor().unwrap();
    assert_eq!(
        wait(&mut resized, |m| matches!(m, Message::EditorResized { .. })),
        Message::EditorResized {
            width: 500,
            height: 280
        }
    );
    let mut s = start(15);
    s.control.open_editor().unwrap();
    wait(&mut s, |m| matches!(m, Message::Ack));
    let begin = Instant::now();
    let mut sequence = 1;
    while begin.elapsed() < Duration::from_millis(350) {
        block(&s, sequence);
        sequence = sequence.wrapping_add(1);
        std::thread::sleep(Duration::from_millis(2));
    }
    let blob = state(&mut s);
    assert_eq!(observation(&blob, 4), 1);
    assert_eq!(observation(&blob, 5), 2);
    assert!(observation(&blob, 7) > 0);
    println!(
        "post-open=480x260, sizeWindow=500x280, idle thread=1, blocks during 300ms={}",
        observation(&blob, 7)
    );

    let mut closing = start(18);
    closing.control.open_editor().unwrap();
    wait(&mut closing, |m| matches!(m, Message::Ack));
    closing.control.close_editor().unwrap();
    wait(&mut closing, |m| matches!(m, Message::Ack));
    assert_eq!(observation(&state(&mut closing), 10), 0);
}
#[test]
#[ignore = "native fixtures required"]
fn shell_children() {
    if std::env::var_os("VST_AUDIO_TEST_SHELL").is_none() {
        return;
    }
    let (worker, plugin) = paths();
    let classes = PluginProcess::scan(&worker, &plugin, PluginFormat::Vst2).unwrap();
    assert_eq!(classes.len(), 2);
    for (class, gain) in classes.iter().zip([0.5, 0.25]) {
        assert_eq!(&class.id[..4], b"VST2");
        let s = PluginProcess::start(
            &worker,
            &plugin,
            PluginFormat::Vst2,
            Some(class.id),
            48000,
            None,
        )
        .unwrap();
        assert_eq!(block(&s, 1), gain);
        println!("shell {} gain={gain}", class.name);
    }
}
#[test]
#[ignore = "native fixtures required"]
fn shell_child_limit() {
    let Some(expected) = std::env::var("VST_AUDIO_TEST_SHELL_CHILDREN")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
    else {
        return;
    };
    let (worker, plugin) = paths();
    let result = PluginProcess::scan(&worker, &plugin, PluginFormat::Vst2);
    if expected == 1024 {
        assert_eq!(result.unwrap().len(), 1024);
    } else {
        assert_eq!(expected, 1025);
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains("VST2 shell enumeration exceeded 1024 entries"),
            "{error}"
        );
    }
}

#[test]
#[ignore = "native fixtures required"]
fn narrow_arrangement_keeps_declared_channels() {
    if std::env::var_os("VST_AUDIO_TEST_NARROW_ARRANGEMENT").is_none() {
        return;
    }
    let s = start(0);
    let output = &s.declared_outputs[0].layout;
    assert_eq!(output.channels, 6);
    assert_eq!(
        output.arrangement,
        vst_audio_protocol::layout::default_arrangement(6).unwrap()
    );
    assert_eq!(block(&s, 1), 1.0);
    println!("stereo arrangement on 6 declared outputs: 6 channels, types unknown");
}

#[test]
#[ignore = "native fixtures required"]
fn zero_program_parameter_state() {
    if std::env::var_os("VST_AUDIO_TEST_NO_CHUNKS").is_none() {
        return;
    }
    let (worker, plugin) = paths();
    let mut s =
        PluginProcess::start(&worker, &plugin, PluginFormat::Vst2, None, 48000, None).unwrap();
    let saved = state(&mut s);
    let mut expected = 0i32.to_le_bytes().to_vec();
    expected.extend_from_slice(&1.0f32.to_le_bytes());
    assert_eq!(saved, expected);
    s.control.set_state(&saved).unwrap();
    wait(&mut s, |m| matches!(m, Message::Ack));
    assert_eq!(state(&mut s), saved);

    let mut legacy = 27i32.to_le_bytes().to_vec();
    legacy.extend_from_slice(&0.25f32.to_le_bytes());
    s.control.set_state(&legacy).unwrap();
    wait(&mut s, |m| matches!(m, Message::Ack));
    let restored = state(&mut s);
    assert_eq!(&restored[..4], &0i32.to_le_bytes());
    assert_eq!(&restored[4..], &0.25f32.to_le_bytes());

    let mut invalid = 0i32.to_le_bytes().to_vec();
    invalid.extend_from_slice(&2.0f32.to_le_bytes());
    s.control.set_state(&invalid).unwrap();
    wait(&mut s, |m| matches!(m, Message::Fault { .. }));
    println!("zero-program state=8 bytes, legacy program ignored, out-of-range=Fault");
}

#[test]
#[ignore = "native fixtures required"]
fn bad_parameter_getter_does_not_replace_recovery_checkpoint() {
    if std::env::var_os("VST_AUDIO_TEST_NO_CHUNKS").is_none() {
        // Keep the non-chunk environment local to this test and its worker.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "bad_parameter_getter_does_not_replace_recovery_checkpoint",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("VST_AUDIO_TEST_NO_CHUNKS", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "non-chunk regression failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        print!("{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    let (worker, plugin) = paths();
    let mut descriptor = PluginDescriptor::new(plugin, PluginFormat::Vst2);
    descriptor.class_id = Some([b'V', b'S', b'T', b'2', 0, 0, 0, 23, 0, 0, 0, 0, 0, 0, 0, 0]);
    let mut instance = Host::new(worker)
        .spawn(&descriptor, &Configuration::default(), None)
        .unwrap();
    let mut audio = instance.take_audio().unwrap();
    let generation = audio.generation();
    let next_event = |instance: &vst_audio::Instance| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(event) = instance.try_next_event() {
                return event;
            }
            assert!(Instant::now() < deadline, "public state event timeout");
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    let set = instance
        .controller()
        .request(Command::SetParameter { id: 0, value: 0.25 })
        .unwrap();
    loop {
        match next_event(&instance) {
            HostEvent::Completed { request } if request == set => break,
            HostEvent::Failed { reason } | HostEvent::RequestFailed { reason, .. } => {
                panic!("valid parameter edit failed: {reason}")
            }
            _ => {}
        }
    }
    let save = instance.controller().request(Command::SaveState).unwrap();
    let saved = loop {
        match next_event(&instance) {
            HostEvent::State { request, bytes } if request == Some(save) => break bytes,
            HostEvent::Failed { reason } | HostEvent::RequestFailed { reason, .. } => {
                panic!("valid state save failed: {reason}")
            }
            _ => {}
        }
    };
    assert_eq!(saved, [0, 0, 0, 0, 0, 0, 128, 62]); // program 0, width 0.25

    // Class 23 starts valid; this edit switches its getter to mode 23 (NaN).
    let set = instance
        .controller()
        .request(Command::SetParameter { id: 0, value: 0.75 })
        .unwrap();
    loop {
        match next_event(&instance) {
            HostEvent::Completed { request } if request == set => break,
            HostEvent::Failed { reason } | HostEvent::RequestFailed { reason, .. } => {
                panic!("fixture mode switch failed: {reason}")
            }
            _ => {}
        }
    }
    let bad_save = instance.controller().request(Command::SaveState).unwrap();
    let mut request_failed = false;
    loop {
        match next_event(&instance) {
            HostEvent::State { request, bytes } if request == Some(bad_save) => {
                panic!("bad getter became a successful checkpoint: {bytes:?}")
            }
            HostEvent::Completed { request } if request == bad_save => {
                panic!("bad getter SaveState completed successfully")
            }
            // A native GetState error is a Fault: the request it answers fails
            // first (audit #11 TD-11), then the instance. Recover must retain the
            // last valid save.
            HostEvent::RequestFailed { request, reason } if request == bad_save => {
                assert!(reason.contains("invalid parameter state value"), "{reason}");
                request_failed = true;
            }
            HostEvent::Failed { reason } => {
                assert!(
                    request_failed,
                    "the failed SaveState must end before the instance fails"
                );
                assert!(reason.contains("invalid parameter state value"), "{reason}");
                break;
            }
            HostEvent::RequestFailed { reason, .. } => panic!("unexpected request error: {reason}"),
            _ => {}
        }
    }
    assert_eq!(instance.status(), vst_audio::Status::Failed);
    instance
        .controller()
        .request(Command::Recover {
            use_previous: false,
        })
        .unwrap();
    let input = [1.0f32; 128];
    let mut samples = [[0.0f32; 128]; 6];
    let mut outputs: Vec<_> = samples.iter_mut().map(|plane| &mut plane[..]).collect();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut recovered = false;
    let mut processed = 0;
    while !recovered || processed < 3 {
        if audio.process(&[&input, &input], &mut outputs) == AudioStatus::Processed {
            assert!(audio.generation() > generation);
            assert_eq!(outputs[0][100], 0.25);
            assert_eq!(outputs[4][100], 0.03125); // 1 * 0.5 * saved width 0.25 * child gain 0.25
            processed += 1;
        }
        while let Some(event) = instance.try_next_event() {
            match event {
                HostEvent::Recovered {
                    generation: current,
                } => {
                    assert!(current > generation);
                    recovered = true;
                }
                HostEvent::Failed { reason } | HostEvent::RequestFailed { reason, .. } => {
                    panic!("last valid checkpoint recovery failed: {reason}")
                }
                _ => {}
            }
        }
        assert!(
            Instant::now() < deadline,
            "checkpoint recovery audio timeout"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let save = instance.controller().request(Command::SaveState).unwrap();
    loop {
        match next_event(&instance) {
            HostEvent::State { request, bytes } if request == Some(save) => {
                assert_eq!(bytes, saved);
                break;
            }
            HostEvent::Failed { reason } | HostEvent::RequestFailed { reason, .. } => {
                panic!("recovered state save failed: {reason}")
            }
            _ => {}
        }
    }
    println!("bad getter failed; latest checkpoint recovered: program=0, width=0.25, side=0.03125");
}

#[test]
#[ignore = "native fixtures required"]
fn resize_reaches_application_floating_and_embedded() {
    use vst_audio::{Command, Configuration, Host, HostEvent, PluginDescriptor};
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        if format == PluginFormat::Vst3 && std::env::var_os("VST_AUDIO_TEST_VST3_RESIZE").is_none()
        {
            continue;
        }
        for embedded in [false, true] {
            #[cfg(target_os = "macos")]
            if embedded {
                continue;
            }
            let (worker, mut plugin) = paths();
            if format == PluginFormat::Vst3 {
                plugin = std::env::var_os("VST_AUDIO_TEST_VST3").unwrap().into();
            }
            let host = Host::new(worker);
            let state = if format == PluginFormat::Vst2 {
                vec![1, 14]
            } else {
                vst_audio::state::encode(&[1, 0], &[1, 0]).unwrap()
            };
            let instance = host
                .spawn(
                    &PluginDescriptor::new(plugin, format),
                    &Configuration::default(),
                    Some(&state),
                )
                .unwrap();
            #[cfg(any(windows, target_os = "linux"))]
            let parent = vst_audio_test_window::Parent::create(640, 480);
            #[cfg(any(windows, target_os = "linux"))]
            let command = if embedded {
                Command::OpenEditorIn(parent.handle())
            } else {
                Command::OpenEditor
            };
            #[cfg(target_os = "macos")]
            let command = Command::OpenEditor;
            let opened = instance.controller().request(command).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let expected = if format == PluginFormat::Vst2 {
                (500, 280)
            } else {
                (520, 300)
            };
            loop {
                #[cfg(any(windows, target_os = "linux"))]
                parent.pump();
                if let Some(event) = instance.try_next_event() {
                    match event {
                        HostEvent::Completed { request }
                            if request == opened && format == PluginFormat::Vst3 =>
                        {
                            instance
                                .controller()
                                .request(Command::Editor(EditorAction::Resize {
                                    width: 400,
                                    height: 240,
                                }))
                                .unwrap();
                        }
                        HostEvent::EditorResized { width, height } => {
                            assert_eq!((width, height), expected);
                            break;
                        }
                        HostEvent::Failed { reason } | HostEvent::RequestFailed { reason, .. } => {
                            panic!("{reason}")
                        }
                        _ => {}
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "application resize event timeout"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            instance
                .controller()
                .request(Command::Editor(EditorAction::Inspect))
                .unwrap();
            loop {
                if let Some(HostEvent::EditorStatus { status, .. }) = instance.try_next_event() {
                    assert_eq!((status.width, status.height), expected);
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(2));
            }
            println!(
                "{format:?} embedded={embedded}: HostEvent::EditorResized and status={expected:?}"
            );
        }
    }
}
