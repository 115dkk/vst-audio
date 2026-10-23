#![forbid(unsafe_code)]
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use vst_audio::{
    AudioStatus, BlockError, BlockStatus, Command, Configuration, EditPhase, Host, HostEvent,
    PluginDescriptor, PluginFormat, RecoveryPolicy, Status,
};

#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

fn descriptor(format: PluginFormat) -> PluginDescriptor {
    PluginDescriptor::new(
        PathBuf::from(
            std::env::var_os(if format == PluginFormat::Vst2 {
                "VST_AUDIO_TEST_VST2"
            } else {
                "VST_AUDIO_TEST_VST3"
            })
            .expect("fixture required"),
        ),
        format,
    )
}
fn host() -> Host {
    Host::new(PathBuf::from(
        std::env::var_os("VST_AUDIO_WORKER").expect("worker required"),
    ))
}
fn state(format: PluginFormat, mode: u8) -> Vec<u8> {
    if format == PluginFormat::Vst3 {
        vst_audio::state::encode(&[1, mode], &[1, mode]).unwrap()
    } else {
        vec![1, mode]
    }
}

#[test]
#[ignore = "native fixtures required"]
fn crash_a_preserves_parent_and_processing_b_without_allocating() {
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let host = host();
        let config = Configuration::default();
        let mut a = host
            .spawn(&descriptor(format), &config, Some(&state(format, 1)))
            .unwrap();
        let mut b = host.spawn(&descriptor(format), &config, None).unwrap();
        let mut audio_a = a.take_audio().unwrap();
        let mut audio_b = b.take_audio().unwrap();
        assert!(b.take_audio().is_none());
        let left = [0.25f32; 128];
        let right = [0.125f32; 128];
        let mut samples = [[0.0f32; 128]; 6];
        let mut refs: Vec<_> = samples.iter_mut().map(|p| &mut p[..]).collect();
        audio_a.process(&[&left, &right], &mut refs);
        let mut processed = 0;
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            let result =
                assert_no_alloc::assert_no_alloc(|| audio_b.process(&[&left, &right], &mut refs));
            if result == AudioStatus::Processed {
                processed += 1;
                if processed > 2 {
                    assert!((refs[0][100] - 0.25).abs() < 1e-6);
                    assert!((refs[5][100] - 0.0625).abs() < 1e-6);
                }
            }
            if processed > 30 && a.status() == Status::Failed {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(processed > 20, "healthy B stopped making progress");
        assert_eq!(a.status(), Status::Failed);
        assert_eq!(b.status(), Status::Running);
    }
}

#[test]
#[ignore = "native fixtures required"]
fn hang_is_detected_without_gui_polling_and_drop_stops_worker() {
    let config = Configuration::default();
    let format = PluginFormat::Vst3;
    let mut instance = host()
        .spawn(&descriptor(format), &config, Some(&state(format, 2)))
        .unwrap();
    let controller = instance.controller();
    let mut audio = instance.take_audio().unwrap();
    let input = [0.0f32; 128];
    let mut output = [[0.0f32; 128]; 6];
    let mut refs: Vec<_> = output.iter_mut().map(|p| &mut p[..]).collect();
    audio.process(&[&input, &input], &mut refs);
    let deadline = Instant::now() + Duration::from_secs(4);
    while controller.status() == Status::Running && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(controller.status(), Status::Failed);
    assert_eq!(
        audio.process(&[&input, &input], &mut refs),
        AudioStatus::Fallback
    );
    assert!(refs.iter().all(|p| p.iter().all(|v| *v == 0.0)));
    drop(instance);

    let healthy = host().spawn(&descriptor(format), &config, None).unwrap();
    let controller = healthy.controller();
    drop(healthy);
    let deadline = Instant::now() + Duration::from_secs(4);
    while controller.status() == Status::Running && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(controller.status(), Status::Stopped);
}

#[test]
#[ignore = "native fixtures and display required"]
fn gui_contract_is_only_requests_and_events() {
    let task = host().spawn_async(
        descriptor(PluginFormat::Vst3),
        Configuration::default(),
        None,
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let instance = loop {
        if let Some(instance) = task.try_complete().unwrap() {
            break instance;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    let controller = instance.controller();
    let open = controller.request(Command::OpenEditor).unwrap();
    loop {
        if let Some(HostEvent::Completed { request }) = instance.try_next_event()
            && request == open
        {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    controller.request(Command::CloseEditor).unwrap();
    loop {
        if let Some(HostEvent::EditorClosed) = instance.try_next_event() {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(instance.status(), Status::Running);
}

#[test]
#[ignore = "native fixtures and display required"]
fn editor_reports_plugin_parameter_gesture_in_order() {
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let instance = host()
            .spawn(&descriptor(format), &Configuration::default(), None)
            .unwrap();
        let open = instance.controller().request(Command::OpenEditor).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut edits = Vec::new();
        let mut completed = false;
        while (!completed || edits.len() < 3) && Instant::now() < deadline {
            if let Some(event) = instance.try_next_event() {
                match event {
                    HostEvent::Completed { request } if request == open => completed = true,
                    HostEvent::ParameterEdited { id, phase, value } => {
                        edits.push((id, phase, value))
                    }
                    _ => {}
                }
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        assert!(completed);
        assert_eq!(
            edits,
            vec![
                (0, EditPhase::Begin, 0.0),
                (0, EditPhase::Perform, 0.5),
                (0, EditPhase::End, 0.0),
            ]
        );
    }
}

#[test]
#[ignore = "native VST2 fixture required"]
fn audio_thread_automate_is_reported_without_allocating() {
    let mut instance = host()
        .spawn(
            &descriptor(PluginFormat::Vst2),
            &Configuration::default(),
            None,
        )
        .unwrap();
    let mut audio = instance.take_audio().unwrap();
    let input = [0.25; 128];
    let mut output = [[0.0; 128]; 6];
    let mut refs: Vec<_> = output.iter_mut().map(|plane| &mut plane[..]).collect();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        assert_no_alloc::assert_no_alloc(|| audio.process(&[&input, &input], &mut refs));
        while let Some(event) = instance.try_next_event() {
            if matches!(
                event,
                HostEvent::ParameterEdited {
                    id: 0,
                    phase: EditPhase::Perform,
                    value: 0.5,
                }
            ) {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("missing audio-thread parameter edit");
}

#[test]
#[ignore = "native fixtures and display required"]
fn parameter_edit_overflow_keeps_latest_value_and_reports_refresh() {
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let mut instance = host()
            .spawn(&descriptor(format), &Configuration::default(), None)
            .unwrap();
        let open = instance.controller().request(Command::OpenEditor).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut edit_count = 0;
        let mut last = None;
        let mut changed = false;
        let mut completed = false;
        while (!completed || last != Some(0.75) || !changed) && Instant::now() < deadline {
            if let Some(event) = instance.try_next_event() {
                match event {
                    HostEvent::Completed { request } if request == open => completed = true,
                    HostEvent::ParameterEdited {
                        phase: EditPhase::Perform,
                        value,
                        ..
                    } => {
                        edit_count += 1;
                        last = Some(value);
                    }
                    HostEvent::ParametersChanged => changed = true,
                    _ => {}
                }
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        assert!(completed);
        assert_eq!(last, Some(0.75));
        assert!(changed);
        assert!(edit_count > 0);
        eprintln!("{format:?} overflow delivered {edit_count} Perform events");
        if format == PluginFormat::Vst3 {
            // The burst exceeds the processor queue. The processor must still end on
            // the editor's last value (rear channels are 0.5 * input * Width).
            let mut audio = instance.take_audio().unwrap();
            let input = [1.0f32; 128];
            let mut output = [[0.0f32; 128]; 6];
            // The first callbacks may fall back while the worker starts processing.
            let (mut processed, mut rear) = (0, f32::NAN);
            let deadline = Instant::now() + Duration::from_secs(5);
            while processed < 8 && Instant::now() < deadline {
                let mut refs: Vec<_> = output.iter_mut().map(|p| &mut p[..]).collect();
                if audio.process(&[&input, &input], &mut refs) == AudioStatus::Processed {
                    processed += 1;
                    rear = output[4][127];
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(processed, 8, "audio did not process");
            assert!(
                (rear - 0.375).abs() < 1e-6,
                "processor width after burst: {rear}"
            );
            eprintln!("Vst3 processor rear channel after burst={rear}");
        }
    }
}

#[test]
#[ignore = "native fixtures required"]
fn parameter_pages_and_invalid_edits_do_not_kill_instances() {
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let instance = host()
            .spawn(&descriptor(format), &Configuration::default(), None)
            .unwrap();
        let control = instance.controller();
        let wait = |request| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(event) = instance.try_next_event() {
                    let id = match &event {
                        HostEvent::Parameters { request, .. }
                        | HostEvent::Completed { request }
                        | HostEvent::RequestFailed { request, .. } => Some(*request),
                        _ => None,
                    };
                    if id == Some(request) {
                        break event;
                    }
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let page = control
            .request(Command::ListParameters {
                start: 0,
                count: 16,
            })
            .unwrap();
        let HostEvent::Parameters { total, entries, .. } = wait(page) else {
            panic!("missing parameter page");
        };
        assert_eq!(total, 1);
        assert_eq!(entries[0].name, "Width");
        assert_eq!(entries[0].value, 1.0);
        let id = entries[0].id;
        let edit = control
            .request(Command::SetParameter { id, value: 0.25 })
            .unwrap();
        assert!(matches!(wait(edit), HostEvent::Completed { .. }));
        let quiet_deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < quiet_deadline {
            if let Some(event) = instance.try_next_event() {
                assert!(
                    !matches!(event, HostEvent::ParameterEdited { .. }),
                    "app SetParameter was reported as a plugin edit"
                );
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let invalid = control
            .request(Command::SetParameter {
                id: u32::MAX,
                value: 0.5,
            })
            .unwrap();
        assert!(matches!(wait(invalid), HostEvent::RequestFailed { .. }));
        assert_eq!(instance.status(), Status::Running);
        let page = control
            .request(Command::ListParameters { start: 0, count: 1 })
            .unwrap();
        let HostEvent::Parameters { entries, .. } = wait(page) else {
            panic!("missing updated page");
        };
        assert!((entries[0].value - 0.25).abs() < 1e-6);
    }
}

#[test]
#[ignore = "native fixtures required"]
fn recovery_replaces_resources_only_at_an_audio_boundary() {
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let mut instance = host()
            .spawn(&descriptor(format), &Configuration::default(), None)
            .unwrap();
        let controller = instance.controller();
        let mut audio = instance.take_audio().unwrap();
        let crash = controller
            .request(Command::RestoreState(state(format, 1)))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while !matches!(instance.try_next_event(),Some(HostEvent::Completed{request})if request==crash)
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let input = [0.25; 128];
        let mut output = [[0.0; 128]; 6];
        let mut refs: Vec<_> = output.iter_mut().map(|p| &mut p[..]).collect();
        audio.process(&[&input, &input], &mut refs);
        while controller.status() != Status::Failed {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let recover = controller
            .request(Command::Recover {
                use_previous: false,
            })
            .unwrap();
        while !matches!(instance.try_next_event(),Some(HostEvent::Completed{request})if request==recover)
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let half = [0.25; 64];
        let mut short = [[0.0; 64]; 6];
        let mut short_refs: Vec<_> = short.iter_mut().map(|p| &mut p[..]).collect();
        assert_no_alloc::assert_no_alloc(|| audio.process(&[&half, &half], &mut short_refs));
        assert_eq!(audio.generation(), 1);
        assert_no_alloc::assert_no_alloc(|| audio.process(&[&half, &half], &mut short_refs));
        assert_eq!(audio.generation(), 2);
        let mut recovered = false;
        while Instant::now() < deadline {
            let result =
                assert_no_alloc::assert_no_alloc(|| audio.process(&[&input, &input], &mut refs));
            if result == AudioStatus::Processed && refs[0][100] > 0.0 {
                recovered = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(recovered);
        assert!((refs[0][100] - 0.25).abs() < 1e-6);
    }
}

fn watch_configuration() -> Configuration {
    Configuration {
        recovery: RecoveryPolicy::Watch,
        ..Configuration::default()
    }
}

#[test]
#[ignore = "native fixtures required"]
fn watch_policy_respawns_a_crashed_worker_with_its_state() {
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let mut instance = host()
            .spawn(
                &descriptor(format),
                &watch_configuration(),
                Some(&state(format, 0)),
            )
            .unwrap();
        let controller = instance.controller();
        let mut audio = instance.take_audio().unwrap();
        let saved = controller.request(Command::SaveState).unwrap();
        let crash = controller
            .request(Command::RestoreState(state(format, 1)))
            .unwrap();
        let input = [0.25; 128];
        let mut output = [[0.0; 128]; 6];
        let mut refs: Vec<_> = output.iter_mut().map(|plane| &mut plane[..]).collect();
        audio.process(&[&input, &input], &mut refs);
        let started = Instant::now();
        let deadline = started + Duration::from_secs(4);
        let mut saw_saved = false;
        let mut saw_crash_ack = false;
        let mut saw_failed = false;
        let mut recovered_at = None;
        while Instant::now() < deadline {
            if let Some(event) = instance.try_next_event() {
                match event {
                    HostEvent::State { request, .. } if request == Some(saved) => saw_saved = true,
                    HostEvent::Completed { request } if request == crash => saw_crash_ack = true,
                    HostEvent::Failed { .. } => {
                        saw_failed = true;
                        assert_eq!(controller.status(), Status::Recovering);
                    }
                    HostEvent::Recovered { generation: 2 } => {
                        recovered_at = Some(started.elapsed());
                        break;
                    }
                    _ => {}
                }
            }
            audio.process(&[&input, &input], &mut refs);
            std::thread::sleep(Duration::from_millis(5));
        }
        let delay = recovered_at.expect("watch did not recover generation 2");
        eprintln!("{format:?} watch respawn recovered after {delay:?}");
        assert!(saw_saved);
        assert!(saw_crash_ack);
        assert!(saw_failed);
        assert!(delay >= Duration::from_millis(500));
        let processing_deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < processing_deadline {
            if audio.process(&[&input, &input], &mut refs) == AudioStatus::Processed
                && (refs[0][100] - 0.25).abs() < 1e-6
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(controller.status(), Status::Running);
        assert!((refs[0][100] - 0.25).abs() < 1e-6);
    }
}

#[test]
#[ignore = "native fixtures required"]
fn watch_policy_disables_after_three_failures() {
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let mut instance = host()
            .spawn(&descriptor(format), &watch_configuration(), None)
            .unwrap();
        let controller = instance.controller();
        let mut audio = instance.take_audio().unwrap();
        let input = [0.25; 128];
        let mut output = [[0.0; 128]; 6];
        let mut refs: Vec<_> = output.iter_mut().map(|plane| &mut plane[..]).collect();
        for generation in [2, 3] {
            controller
                .request(Command::RestoreState(state(format, 1)))
                .unwrap();
            audio.process(&[&input, &input], &mut refs);
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                audio.process(&[&input, &input], &mut refs);
                if matches!(instance.try_next_event(), Some(HostEvent::Recovered { generation: value }) if value == generation)
                {
                    break;
                }
                assert!(Instant::now() < deadline, "missing generation {generation}");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        controller
            .request(Command::RestoreState(state(format, 1)))
            .unwrap();
        audio.process(&[&input, &input], &mut refs);
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut recovery_required = false;
        while Instant::now() < deadline {
            audio.process(&[&input, &input], &mut refs);
            if matches!(instance.try_next_event(), Some(HostEvent::RecoveryRequired)) {
                recovery_required = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(recovery_required);
        assert_eq!(controller.status(), Status::Failed);
        let pid = controller.worker_pid();
        std::thread::sleep(Duration::from_secs(1));
        assert_eq!(controller.worker_pid(), pid);
        let recover = controller
            .request(Command::Recover { use_previous: true })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            audio.process(&[&input, &input], &mut refs);
            if matches!(instance.try_next_event(), Some(HostEvent::Completed { request }) if request == recover)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "manual recovery did not start a fresh window"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
#[ignore = "native fixtures required"]
fn watch_policy_shutdown_during_backoff_does_not_respawn() {
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let mut instance = host()
            .spawn(&descriptor(format), &watch_configuration(), None)
            .unwrap();
        let controller = instance.controller();
        let mut audio = instance.take_audio().unwrap();
        let input = [0.25; 128];
        let mut output = [[0.0; 128]; 6];
        let mut refs: Vec<_> = output.iter_mut().map(|plane| &mut plane[..]).collect();
        controller
            .request(Command::RestoreState(state(format, 1)))
            .unwrap();
        audio.process(&[&input, &input], &mut refs);
        let deadline = Instant::now() + Duration::from_secs(2);
        while controller.status() != Status::Recovering {
            audio.process(&[&input, &input], &mut refs);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let pid = controller.worker_pid();
        controller.request(Command::Shutdown).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while controller.status() != Status::Stopped {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(700));
        assert_eq!(controller.status(), Status::Stopped);
        assert_eq!(controller.worker_pid(), pid);
    }
}

#[test]
#[ignore = "native fixtures required"]
fn block_endpoint_delivers_and_adopts_recovery() {
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let mut instance = host()
            .spawn(
                &descriptor(format),
                &Configuration {
                    slots: Some(8),
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        let controller = instance.controller();
        let mut blocks = instance.take_blocks().unwrap();
        assert!(instance.take_audio().is_none());
        let input = [0.25; 256];
        let mut invalid = [-1.0; 767];
        assert_eq!(
            blocks.process(&input, &mut invalid),
            BlockStatus::InvalidBuffer
        );
        assert!(invalid.iter().all(|sample| *sample == -1.0));
        assert_eq!(blocks.restart(8), Err(BlockError::Depth));
        blocks.restart(7).unwrap();
        let mut output = [-1.0; 768];
        for _ in 0..7 {
            assert_eq!(
                assert_no_alloc::assert_no_alloc(|| blocks.process(&input, &mut output)),
                BlockStatus::Missed
            );
            assert!(output.iter().all(|sample| *sample == -1.0));
            std::thread::sleep(Duration::from_millis(3));
        }
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let status = assert_no_alloc::assert_no_alloc(|| blocks.process(&input, &mut output));
            if status == BlockStatus::Delivered {
                assert!((output[100] - 0.25).abs() < 1e-6);
                assert!((output[5 * 128 + 100] - 0.125).abs() < 1e-6);
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(3));
        }
        let crash = controller
            .request(Command::RestoreState(state(format, 1)))
            .unwrap();
        while !matches!(instance.try_next_event(),Some(HostEvent::Completed{request})if request==crash)
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = blocks.process(&input, &mut output);
        while controller.status() != Status::Failed {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let recover = controller
            .request(Command::Recover {
                use_previous: false,
            })
            .unwrap();
        while !matches!(instance.try_next_event(),Some(HostEvent::Completed{request})if request==recover)
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut adopted = false;
        while Instant::now() < deadline {
            let _ = assert_no_alloc::assert_no_alloc(|| blocks.process(&input, &mut output));
            if blocks.generation() == 2 {
                adopted = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(3));
        }
        assert!(adopted);
        blocks.restart(1).unwrap();
        let mut delivered = false;
        while Instant::now() < deadline {
            if assert_no_alloc::assert_no_alloc(|| blocks.process(&input, &mut output)).delivered()
            {
                delivered = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(3));
        }
        assert!(delivered);
    }
}

#[test]
#[ignore = "native fixtures required"]
fn cancellation_interrupts_a_hung_state_restore() {
    let task = host().spawn_async(
        descriptor(PluginFormat::Vst3),
        Configuration::default(),
        Some(state(PluginFormat::Vst3, 5)),
    );
    std::thread::sleep(Duration::from_millis(100));
    task.cancel();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match task.try_complete() {
            Err(error) => {
                assert!(error.to_string().contains("cancel"));
                break;
            }
            Ok(None) => {}
            Ok(Some(_)) => panic!("hung state restore unexpectedly completed"),
        };
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "native fixtures and display required"]
fn editor_visibility_focus_scaling_errors_and_pixels_roundtrip() {
    use vst_audio::EditorAction as A;
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let instance = host()
            .spawn(&descriptor(format), &Configuration::default(), None)
            .unwrap();
        let control = instance.controller();
        let wait = |request| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(event) = instance.try_next_event() {
                    let id = match &event {
                        HostEvent::Completed { request }
                        | HostEvent::RequestFailed { request, .. }
                        | HostEvent::EditorStatus { request, .. }
                        | HostEvent::EditorImage { request, .. } => Some(*request),
                        _ => None,
                    };
                    assert!(
                        !matches!(event, HostEvent::Failed { .. } | HostEvent::EditorClosed),
                        "{event:?}"
                    );
                    if id == Some(request) {
                        break event;
                    }
                }
                assert!(Instant::now() < deadline, "editor response timeout");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        assert!(matches!(
            wait(control.request(Command::OpenEditor).unwrap()),
            HostEvent::Completed { .. }
        ));
        for (action, visible) in [
            (A::Inspect, true),
            (A::Hide, false),
            (A::Show, true),
            (A::Focus, true),
        ] {
            let HostEvent::EditorStatus { status, .. } =
                wait(control.request(Command::Editor(action)).unwrap())
            else {
                panic!("missing editor status")
            };
            assert_eq!(status.visible, visible);
            assert!(status.width > 0 && status.height > 0);
            std::thread::sleep(Duration::from_millis(80));
        }
        if format == PluginFormat::Vst3 {
            for (action, w, h, scale) in [
                (
                    A::Resize {
                        width: 120,
                        height: 100,
                    },
                    320,
                    200,
                    1.0,
                ),
                (
                    A::Resize {
                        width: 480,
                        height: 300,
                    },
                    480,
                    300,
                    1.0,
                ),
                (A::Scale(1.5), 480, 300, 1.5),
            ] {
                let HostEvent::EditorStatus { status, .. } =
                    wait(control.request(Command::Editor(action)).unwrap())
                else {
                    panic!("missing positive size response")
                };
                assert_eq!((status.width, status.height, status.scale), (w, h, scale));
                assert!(status.resize_supported && status.scale_supported);
            }
        } else {
            // The VST 2 fixture accepts the 'PreS'/'AeCs' content scale and
            // reports its 320x200 rectangle scaled.
            let HostEvent::EditorStatus { status, .. } =
                wait(control.request(Command::Editor(A::Scale(1.5))).unwrap())
            else {
                panic!("missing VST 2 scale response")
            };
            assert_eq!((status.width, status.height, status.scale), (480, 300, 1.5));
            assert!(status.scale_supported && !status.resize_supported);
            // Mode 19 does not answer supportsViewDpiScaling: no scale contract.
            let plain = host()
                .spawn(
                    &descriptor(format),
                    &Configuration::default(),
                    Some(&state(format, 19)),
                )
                .unwrap();
            let plain_control = plain.controller();
            let plain_wait = |request| {
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    if let Some(event) = plain.try_next_event() {
                        let id = match &event {
                            HostEvent::Completed { request }
                            | HostEvent::RequestFailed { request, .. }
                            | HostEvent::EditorStatus { request, .. } => Some(*request),
                            _ => None,
                        };
                        if id == Some(request) {
                            break event;
                        }
                    }
                    assert!(Instant::now() < deadline, "editor response timeout");
                    std::thread::sleep(Duration::from_millis(10));
                }
            };
            assert!(matches!(
                plain_wait(plain_control.request(Command::OpenEditor).unwrap()),
                HostEvent::Completed { .. }
            ));
            assert!(matches!(
                plain_wait(
                    plain_control
                        .request(Command::Editor(A::Scale(1.5)))
                        .unwrap()
                ),
                HostEvent::RequestFailed { .. }
            ));
            drop(plain);
        }
        // Unsupported or invalid requests leave the editor and audio instance alive.
        assert!(matches!(
            wait(
                control
                    .request(Command::Editor(A::Scale(f32::NAN)))
                    .unwrap()
            ),
            HostEvent::RequestFailed { .. }
        ));
        let HostEvent::EditorImage { image, .. } =
            wait(control.request(Command::Editor(A::Capture)).unwrap())
        else {
            panic!("missing image")
        };
        assert!(image.valid());
        assert!(image.rgba.iter().any(|b| *b != 0), "capture is empty");
        let path = std::path::Path::new("../../test-results/editor");
        std::fs::create_dir_all(path).unwrap();
        let mut ppm = format!("P6\n{} {}\n255\n", image.width, image.height).into_bytes();
        for rgba in image.rgba.as_chunks::<4>().0 {
            ppm.extend_from_slice(&rgba[..3]);
        }
        std::fs::write(path.join(format!("{format:?}.ppm")), ppm).unwrap();
        assert_eq!(instance.status(), Status::Running);
    }
}
