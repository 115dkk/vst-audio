#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]
use std::{
    io::BufReader,
    path::PathBuf,
    process::Command as Process,
    time::{Duration, Instant},
};
use vst_audio::{
    Command, Configuration, EditorAction as A, EditorPlatform, EditorScaling, Host, HostEvent,
    Instance, LinuxEditor, PluginDescriptor, PluginFormat,
};
fn spawn(format: PluginFormat, policy: LinuxEditor, sandbox: bool) -> Instance {
    Host::new(PathBuf::from(
        std::env::var_os("VST_AUDIO_WORKER").expect("worker required"),
    ))
    .spawn(
        &PluginDescriptor::new(
            PathBuf::from(
                std::env::var_os(if format == PluginFormat::Vst3 {
                    "VST_AUDIO_TEST_VST3"
                } else {
                    "VST_AUDIO_TEST_VST2"
                })
                .expect("fixture required"),
            ),
            format,
        ),
        &Configuration {
            linux_editor: policy,
            sandbox: sandbox.then(vst_audio::SandboxPolicy::default),
            ..Configuration::default()
        },
        None,
    )
    .unwrap()
}
fn response(instance: &Instance, command: Command) -> HostEvent {
    response_checked(instance, command, false)
}
fn response_checked(instance: &Instance, command: Command, keep_open: bool) -> HostEvent {
    let request = instance.controller().request(command).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(event) = instance.try_next_event() {
            let id = match &event {
                HostEvent::Completed { request }
                | HostEvent::RequestFailed { request, .. }
                | HostEvent::EditorStatus { request, .. }
                | HostEvent::EditorImage { request, .. } => Some(*request),
                HostEvent::State { request, .. } => *request,
                _ => None,
            };
            assert!(!matches!(event, HostEvent::Failed { .. }), "{event:?}");
            assert!(
                !keep_open || !matches!(event, HostEvent::EditorClosed),
                "unexpected close"
            );
            if id == Some(request) {
                return event;
            }
        }
        assert!(Instant::now() < deadline, "response timeout");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn lifecycle(instance: &Instance, expected: [u32; 4]) {
    let HostEvent::State { bytes, .. } = response_checked(instance, Command::SaveState, true)
    else {
        panic!("missing fixture state")
    };
    let (component, _) = vst_audio_protocol::state::decode(&bytes).unwrap();
    let counts: Vec<_> = component[2..]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|n| u32::from_le_bytes(*n))
        .collect();
    eprintln!("view lifecycle [created, attached, removed, dropped]={counts:?}");
    assert_eq!(counts, expected);
}
fn audio_continues(audio: &mut vst_audio::AudioEndpoint) {
    let input = [0.25; 128];
    let mut samples = [[0.0; 128]; 6];
    let mut output: Vec<_> = samples.iter_mut().map(|p| &mut p[..]).collect();
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut processed = 0;
    while processed < 10 {
        if audio.process(&[&input, &input], &mut output) == vst_audio::AudioStatus::Processed {
            processed += 1;
            if processed > 2 {
                assert!((output[0][100] - 0.25).abs() < 1e-6);
            }
        }
        assert!(Instant::now() < deadline, "audio stopped");
        std::thread::sleep(Duration::from_millis(5));
    }
    eprintln!("audio blocks={processed}");
}
fn pixels(label: &str) -> (usize, u32, u32) {
    std::thread::sleep(Duration::from_millis(250));
    let directory = tempfile::tempdir().unwrap();
    assert!(
        Process::new("weston-screenshooter")
            .current_dir(directory.path())
            .status()
            .unwrap()
            .success()
    );
    let path = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "png"))
        .expect("screenshot");
    let mut reader = png::Decoder::new(BufReader::new(std::fs::File::open(path).unwrap()))
        .read_info()
        .unwrap();
    let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buffer).unwrap();
    let channels = info.color_type.samples();
    let mut count = 0;
    let (mut left, mut top, mut right, mut bottom) = (u32::MAX, u32::MAX, 0, 0);
    for (index, pixel) in buffer[..info.buffer_size()]
        .chunks_exact(channels)
        .enumerate()
    {
        if pixel[0].abs_diff(32) <= 2 && pixel[1].abs_diff(64) <= 2 && pixel[2].abs_diff(128) <= 2 {
            let (x, y) = (index as u32 % info.width, index as u32 / info.width);
            count += 1;
            left = left.min(x);
            top = top.min(y);
            right = right.max(x);
            bottom = bottom.max(y);
        }
    }
    let bounds = if count == 0 {
        (0, 0)
    } else {
        (right - left + 1, bottom - top + 1)
    };
    eprintln!("{label}: pixels={count}, bounds={}x{}", bounds.0, bounds.1);
    (count, bounds.0, bounds.1)
}
#[test]
#[ignore = "native fixtures and display required"]
fn native_wayland_pixels_lifecycle_scale_and_sandbox() {
    assert!(std::env::var_os("DISPLAY").is_none());
    let scale: u32 = std::env::var("VST_AUDIO_TEST_SCALE")
        .unwrap()
        .parse()
        .unwrap();
    for sandbox in [false, true] {
        let mut instance = spawn(PluginFormat::Vst3, LinuxEditor::Wayland, sandbox);
        let mut audio = instance.take_audio().unwrap();
        let opened = response(&instance, Command::OpenEditor);
        assert!(matches!(opened, HostEvent::Completed { .. }), "{opened:?}");
        let status = response(&instance, Command::Editor(A::Inspect));
        let HostEvent::EditorStatus { status, .. } = status else {
            panic!("{status:?}")
        };
        assert_eq!(status.platform, EditorPlatform::Wayland);
        assert_eq!(status.scaling, EditorScaling::Plugin);
        assert_eq!(status.scale, scale as f32);
        let (count, width, height) = pixels("shown");
        assert_eq!((width, height), (320 * scale, 200 * scale));
        assert_eq!(count, (width * height) as usize);
        let capture = response(&instance, Command::Editor(A::Capture));
        assert!(
            matches!(capture, HostEvent::RequestFailed { reason, .. } if reason == "editor capture is not available for Wayland editors")
        );
        // Capability discovery creates and drops one unattached probe view.
        lifecycle(&instance, [2, 1, 0, 1]);
        for cycle in 0..2 {
            eprintln!("scale={scale} sandbox={sandbox} cycle={cycle}");
            let hidden = response_checked(&instance, Command::Editor(A::Hide), true);
            assert!(matches!(hidden, HostEvent::EditorStatus { status, .. }
                if !status.visible && status.width == 320 && status.height == 200 && status.scale == scale as f32));
            assert_eq!(pixels("hidden").0, 0);
            response_checked(&instance, Command::Editor(A::Hide), true);
            response_checked(&instance, Command::Editor(A::Focus), true);
            lifecycle(&instance, [cycle + 2, cycle + 1, cycle + 1, cycle + 2]);
            audio_continues(&mut audio);
            let shown = response_checked(&instance, Command::Editor(A::Show), true);
            assert!(
                matches!(shown, HostEvent::EditorStatus { status, .. }
                if status.visible && status.scale == scale as f32),
                "cycle={cycle}"
            );
            assert_eq!(pixels("reshown").0, count);
            response_checked(&instance, Command::Editor(A::Show), true);
            lifecycle(&instance, [cycle + 3, cycle + 2, cycle + 1, cycle + 2]);
            audio_continues(&mut audio);
        }
        response_checked(&instance, Command::Editor(A::Hide), true);
        let hidden_resize = response_checked(
            &instance,
            Command::Editor(A::Resize {
                width: 400,
                height: 240,
            }),
            true,
        );
        assert!(
            matches!(hidden_resize, HostEvent::EditorStatus { status, .. }
            if !status.visible && status.width == 400 && status.height == 240 && status.scale == scale as f32)
        );
        assert_eq!(pixels("hidden resized").0, 0);
        lifecycle(&instance, [4, 3, 3, 4]);
        let shown = response_checked(&instance, Command::Editor(A::Show), true);
        assert!(matches!(shown, HostEvent::EditorStatus { status, .. }
            if status.visible && status.width == 400 && status.height == 240 && status.scale == scale as f32));
        let (count, width, height) = pixels("reshown resized");
        assert_eq!((width, height), (400 * scale, 240 * scale));
        assert_eq!(count, (width * height) as usize);
        lifecycle(&instance, [5, 4, 3, 4]);
        audio_continues(&mut audio);
        response_checked(&instance, Command::Editor(A::Scale(1.5)), true);
        response_checked(&instance, Command::Editor(A::Hide), true);
        response_checked(&instance, Command::Editor(A::Show), true);
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            matches!(response_checked(&instance, Command::Editor(A::Inspect), true),
            HostEvent::EditorStatus { status, .. } if status.scale == 1.5)
        );
        lifecycle(&instance, [6, 5, 4, 5]);
        let resized = response(
            &instance,
            Command::Editor(A::Resize {
                width: 480,
                height: 300,
            }),
        );
        assert!(
            matches!(resized, HostEvent::EditorStatus { .. }),
            "{resized:?}"
        );
        let (count, width, height) = pixels("resized");
        // Weston chooses placement. Growing at the bottom/right may be clipped
        // by the output; at least one complete new axis must remain visible.
        assert!(width <= 480 * scale && height <= 300 * scale);
        assert!(width == 480 * scale || height == 300 * scale);
        assert_eq!(count, (width * height) as usize);
        response(&instance, Command::CloseEditor);
        std::thread::sleep(Duration::from_millis(100));
        let reopened = response(&instance, Command::OpenEditor);
        assert!(
            matches!(reopened, HostEvent::Completed { .. }),
            "{reopened:?}"
        );
        audio_continues(&mut audio);
        response_checked(&instance, Command::Editor(A::Hide), true);
        response(&instance, Command::CloseEditor);
        std::thread::sleep(Duration::from_millis(100));
        assert!(matches!(
            response(&instance, Command::Editor(A::Inspect)),
            HostEvent::RequestFailed { .. }
        ));
    }
}
#[test]
#[ignore = "native fixtures and display required"]
fn failed_wayland_recreation_closes_editor() {
    let mut instance = spawn(PluginFormat::Vst3, LinuxEditor::Auto, false);
    let mut audio = instance.take_audio().unwrap();
    assert!(matches!(
        response(&instance, Command::OpenEditor),
        HostEvent::Completed { .. }
    ));
    response_checked(&instance, Command::Editor(A::Hide), true);
    let request = instance
        .controller()
        .request(Command::Editor(A::Show))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let (mut failed, mut closed) = (false, false);
    while !failed || !closed {
        if let Some(event) = instance.try_next_event() {
            match event {
                HostEvent::RequestFailed { request: id, .. } if id == request => failed = true,
                HostEvent::EditorClosed => closed = true,
                HostEvent::Failed { .. } | HostEvent::EditorStatus { .. } => panic!("{event:?}"),
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "missing failure or EditorClosed");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(matches!(
        response(&instance, Command::Editor(A::Inspect)),
        HostEvent::RequestFailed { .. }
    ));
    assert_eq!(pixels("failed recreation").0, 0);
    audio_continues(&mut audio);
}
#[test]
#[ignore = "native fixtures and display required"]
fn wayland_selection_and_vst2_rejection() {
    let instance = spawn(PluginFormat::Vst3, LinuxEditor::Auto, false);
    let opened = response(&instance, Command::OpenEditor);
    assert!(matches!(opened, HostEvent::Completed { .. }), "{opened:?}");
    assert!(
        matches!(response(&instance, Command::Editor(A::Inspect)), HostEvent::EditorStatus { status, .. } if status.platform == EditorPlatform::Wayland)
    );
    let instance = spawn(PluginFormat::Vst2, LinuxEditor::Wayland, false);
    assert!(
        matches!(response(&instance, Command::OpenEditor), HostEvent::RequestFailed { reason, .. } if reason == "the plugin does not support Wayland editors")
    );
}

#[test]
#[ignore = "native fixtures and display required"]
fn wayland_attach_rejection_falls_back_only_for_auto() {
    assert!(std::env::var_os("VST_AUDIO_REJECT_WAYLAND").is_some());
    let instance = spawn(PluginFormat::Vst3, LinuxEditor::Auto, false);
    let opened = response(&instance, Command::OpenEditor);
    assert!(matches!(opened, HostEvent::Completed { .. }), "{opened:?}");
    let status = response(&instance, Command::Editor(A::Inspect));
    assert!(
        matches!(status, HostEvent::EditorStatus { ref status, .. } if status.platform == EditorPlatform::X11),
        "{status:?}"
    );
    eprintln!("fallback platform=X11");
    let HostEvent::EditorImage { image, .. } = response(&instance, Command::Editor(A::Capture))
    else {
        panic!("X11 capture failed")
    };
    let count = image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[..3] == [32, 64, 128])
        .count();
    eprintln!("fallback pixels={count}");
    assert_eq!(count, 64000);
    let explicit = spawn(PluginFormat::Vst3, LinuxEditor::Wayland, false);
    assert!(matches!(
        response(&explicit, Command::OpenEditor),
        HostEvent::RequestFailed { .. }
    ));
}
#[test]
#[ignore = "native fixtures and display required"]
fn rejected_content_scale_preserves_actual_scale() {
    assert!(std::env::var_os("VST_AUDIO_REJECT_SCALE").is_some());
    let instance = spawn(PluginFormat::Vst3, LinuxEditor::Wayland, false);
    let opened = response(&instance, Command::OpenEditor);
    assert!(matches!(opened, HostEvent::Completed { .. }), "{opened:?}");
    let status = response(&instance, Command::Editor(A::Inspect));
    eprintln!("rejected scale: {status:?}");
    assert!(matches!(status, HostEvent::EditorStatus { status, .. } if status.scale == 1.0));
}
#[test]
#[ignore = "native fixtures and display required"]
fn initial_content_scale_precedes_attachment() {
    let instance = spawn(PluginFormat::Vst3, LinuxEditor::Wayland, false);
    let opened = response(&instance, Command::OpenEditor);
    assert!(matches!(opened, HostEvent::Completed { .. }), "{opened:?}");
    let expected: f32 = std::env::var("VST_AUDIO_EXPECT_ATTACH_SCALE")
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        matches!(response(&instance, Command::Editor(A::Inspect)), HostEvent::EditorStatus { status, .. } if status.scale == expected)
    );
    assert!(pixels("initial scale").0 > 0);
}
#[test]
#[ignore = "native fixtures and display required"]
fn compositor_disconnect_closes_editor_and_preserves_audio() {
    let mut instance = spawn(PluginFormat::Vst3, LinuxEditor::Wayland, false);
    let mut audio = instance.take_audio().unwrap();
    assert!(matches!(
        response(&instance, Command::OpenEditor),
        HostEvent::Completed { .. }
    ));
    let marker = PathBuf::from(std::env::var_os("VST_AUDIO_KILL_MARKER").unwrap());
    let started = Instant::now();
    std::fs::write(&marker, b"ready").unwrap();
    let mut closed = false;
    let mut processed = 0;
    let input = [0.25; 128];
    let mut samples = [[0.0; 128]; 6];
    let mut output: Vec<_> = samples.iter_mut().map(|p| &mut p[..]).collect();
    while !closed || processed < 10 {
        if let Some(event) = instance.try_next_event() {
            assert!(!matches!(event, HostEvent::Failed { .. }), "{event:?}");
            if matches!(event, HostEvent::EditorClosed) {
                closed = true;
                eprintln!("EditorClosed after {} ms", started.elapsed().as_millis());
            }
        }
        if closed
            && audio.process(&[&input, &input], &mut output) == vst_audio::AudioStatus::Processed
        {
            processed += 1;
            if processed > 2 {
                assert!((output[0][100] - 0.25).abs() < 1e-6);
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "disconnect timeout"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    eprintln!("post-disconnect audio blocks={processed}");
}
