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
