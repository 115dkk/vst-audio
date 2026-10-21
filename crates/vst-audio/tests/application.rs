#![forbid(unsafe_code)]
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use vst_audio::{
    AudioStatus, Command, Configuration, Host, HostEvent, PluginDescriptor, PluginFormat, Status,
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
            assert!(matches!(
                wait(control.request(Command::Editor(A::Scale(1.5))).unwrap()),
                HostEvent::RequestFailed { .. }
            ));
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
