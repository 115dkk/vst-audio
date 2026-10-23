#![forbid(unsafe_code)]
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
#[cfg(target_os = "linux")]
use vst_audio::EditorScaling;
use vst_audio::{
    Command, Configuration, Host, HostEvent, Instance, ParentWindow, PluginDescriptor, PluginFormat,
};
#[cfg(any(windows, target_os = "linux"))]
use vst_audio_test_window::Parent;

fn spawn(format: PluginFormat, sandbox: bool) -> Instance {
    let host = Host::new(PathBuf::from(
        std::env::var_os("VST_AUDIO_WORKER").expect("worker required"),
    ));
    let descriptor = PluginDescriptor::new(
        PathBuf::from(
            std::env::var_os(if format == PluginFormat::Vst2 {
                "VST_AUDIO_TEST_VST2"
            } else {
                "VST_AUDIO_TEST_VST3"
            })
            .expect("fixture required"),
        ),
        format,
    );
    host.spawn(
        &descriptor,
        &Configuration {
            sandbox: sandbox.then(vst_audio::SandboxPolicy::default),
            ..Configuration::default()
        },
        None,
    )
    .unwrap()
}
fn response(instance: &Instance, command: Command, pump: impl Fn()) -> HostEvent {
    let request = instance.controller().request(command).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        pump();
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
                return event;
            }
        }
        assert!(Instant::now() < deadline, "editor response timeout");
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[cfg(any(windows, target_os = "linux"))]
fn exercise(sandbox: bool) {
    use vst_audio::{AudioStatus, EditorAction as A, Status};
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let parent = Parent::create(640, 480);
        let mut instance = spawn(format, sandbox);
        let mut audio = instance.take_audio().unwrap();
        let send = |command| response(&instance, command, || parent.pump());
        // Switching modes replaces, rather than accumulating, native windows.
        if !sandbox {
            assert!(matches!(
                send(Command::OpenEditor),
                HostEvent::Completed { .. }
            ));
        }
        let opened = send(Command::OpenEditorIn(parent.handle()));
        // Windows refuses an AppContainer child under the application's window.
        #[cfg(windows)]
        if sandbox {
            let HostEvent::RequestFailed { reason, .. } = &opened else {
                panic!("sandboxed Windows embedding must be refused: {opened:?}")
            };
            assert_eq!(
                reason,
                "parent window is not accessible from the sandboxed worker"
            );
            continue;
        }
        assert!(matches!(opened, HostEvent::Completed { .. }), "{opened:?}");
        eprintln!("{format:?}: sandbox={sandbox}, embedded successfully");
        let children = parent.children();
        #[cfg(target_os = "linux")]
        assert_eq!(children.len(), 1);
        #[cfg(windows)]
        assert!(
            children
                .iter()
                .any(|(_, pid)| *pid == Some(instance.controller().worker_pid()))
        );
        for (action, visible) in [
            (A::Inspect, true),
            (A::Hide, false),
            (A::Show, true),
            (A::Focus, true),
        ] {
            let event = send(Command::Editor(action));
            let HostEvent::EditorStatus { status, .. } = event else {
                panic!("{event:?}")
            };
            assert!(status.embedded);
            assert_eq!(status.visible, visible);
            assert!(status.width > 0 && status.height > 0);
            std::thread::sleep(Duration::from_millis(30));
        }
        // Minimizing or switching away from the application's view hides the
        // parent; the editor must stay open and become visible again.
        parent.set_visible(false);
        let deadline = Instant::now() + Duration::from_millis(100);
        while Instant::now() < deadline {
            parent.pump();
            std::thread::sleep(Duration::from_millis(5));
        }
        parent.set_visible(true);
        let event = send(Command::Editor(A::Inspect));
        let HostEvent::EditorStatus { status, .. } = event else {
            panic!("{event:?}")
        };
        assert!(status.embedded && status.visible, "{status:?}");
        if format == PluginFormat::Vst3 {
            let event = send(Command::Editor(A::Resize {
                width: 480,
                height: 300,
            }));
            let HostEvent::EditorStatus { status, .. } = event else {
                panic!("{event:?}")
            };
            assert_eq!((status.width, status.height), (480, 300));
            assert!(status.embedded);
        }
        // Pump the application's input queue while the worker paints its child.
        let deadline = Instant::now() + Duration::from_millis(250);
        while Instant::now() < deadline {
            parent.pump();
            std::thread::sleep(Duration::from_millis(5));
        }
        let image = parent.capture();
        assert!(image.valid());
        #[cfg(target_os = "linux")]
        {
            let pixels = image
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| **p == [32, 64, 128, 255])
                .count();
            eprintln!(
                "{format:?} sandbox={sandbox}: X11 parent capture {}x{}, fixture-color pixels={pixels}",
                image.width, image.height
            );
            assert!(
                pixels >= 320 * 200,
                "fixture pixels missing from parent capture"
            );
        }
        #[cfg(windows)]
        {
            // The Windows fixture uses a STATIC text control: require both glyphs and background.
            let region: Vec<_> = (20..180)
                .flat_map(|y| (20..300).map(move |x| (y * image.width as usize + x) * 4))
                .collect();
            let dark = region
                .iter()
                .filter(|i| image.rgba[**i..**i + 3].iter().all(|v| *v < 80))
                .count();
            let light = region
                .iter()
                .filter(|i| image.rgba[**i..**i + 3].iter().all(|v| *v > 180))
                .count();
            eprintln!("{format:?}: parent capture dark={dark}, light={light}");
            assert!(dark > 100 && light > 1000);
        }
        assert!(matches!(
            send(Command::Editor(A::Capture)),
            HostEvent::EditorImage { .. }
        ));
        if !sandbox {
            assert!(matches!(
                send(Command::OpenEditor),
                HostEvent::Completed { .. }
            ));
            assert!(parent.children().is_empty());
            let HostEvent::EditorStatus { status, .. } = send(Command::Editor(A::Inspect)) else {
                panic!("status required")
            };
            assert!(!status.embedded);
            assert!(matches!(
                send(Command::OpenEditorIn(parent.handle())),
                HostEvent::Completed { .. }
            ));
        }
        let wait_closed = || {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                parent.pump();
                if let Some(event) = instance.try_next_event() {
                    if matches!(event, HostEvent::EditorClosed) {
                        break;
                    }
                    assert!(!matches!(event, HostEvent::Failed { .. }), "{event:?}");
                }
                assert!(Instant::now() < deadline, "editor did not close");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        assert!(matches!(
            send(Command::CloseEditor),
            HostEvent::Completed { .. }
        ));
        wait_closed();
        assert!(parent.children().is_empty());
        assert!(matches!(
            send(Command::OpenEditorIn(parent.handle())),
            HostEvent::Completed { .. }
        ));
        parent.destroy();
        wait_closed();
        for handle in [parent.handle(), invalid_handle()] {
            let event = send(Command::OpenEditorIn(handle));
            let HostEvent::RequestFailed { reason, .. } = event else {
                panic!("{event:?}")
            };
            assert_eq!(reason, "parent window is not a window");
        }
        let other = if cfg!(windows) {
            ParentWindow::X11 { window: 1 }
        } else {
            ParentWindow::Win32 { hwnd: 1 }
        };
        let HostEvent::RequestFailed { reason, .. } = send(Command::OpenEditorIn(other)) else {
            panic!("wrong platform accepted")
        };
        assert_eq!(reason, "parent window platform does not match the worker");
        let input = [0.25; 128];
        let mut samples = [[0.0; 128]; 6];
        let mut output: Vec<_> = samples.iter_mut().map(|p| &mut p[..]).collect();
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut processed = 0;
        while processed < 10 {
            if audio.process(&[&input, &input], &mut output) == AudioStatus::Processed {
                processed += 1;
                if processed > 2 {
                    assert!((output[0][100] - 0.25).abs() < 1e-6);
                }
            }
            assert!(
                Instant::now() < deadline,
                "audio stopped after parent destruction"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(instance.status(), Status::Running);
    }
}
#[cfg(any(windows, target_os = "linux"))]
fn invalid_handle() -> ParentWindow {
    if cfg!(windows) {
        ParentWindow::Win32 { hwnd: 0 }
    } else {
        ParentWindow::X11 { window: 0 }
    }
}
#[cfg(target_os = "linux")]
fn assert_fixture_pixels(image: &vst_audio::EditorImage, expected: usize) {
    let pixels = image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| **pixel == [32, 64, 128, 255])
        .count();
    assert_eq!(
        pixels, expected,
        "fixture canvas did not fill its scaled area"
    );
}

#[test]
#[ignore = "native fixtures and display required"]
#[cfg(target_os = "linux")]
fn embedded_x11_auto_uses_resource_manager_dpi() {
    let _dpi = vst_audio_test_window::ResourceManager::set(Some("Xft.dpi: 192\n"));
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let parent = Parent::create(800, 500);
        let instance = spawn(format, false);
        let request = instance
            .controller()
            .request(Command::OpenEditorWith {
                parent: Some(parent.handle()),
                scaling: EditorScaling::Auto,
            })
            .unwrap();
        let opened = {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                parent.pump();
                if let Some(event) = instance.try_next_event() {
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
                assert!(Instant::now() < deadline, "editor open timed out");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        assert!(matches!(opened, HostEvent::Completed { .. }), "{opened:?}");
        let HostEvent::EditorStatus { status, .. } = response(
            &instance,
            Command::Editor(vst_audio::EditorAction::Inspect),
            || parent.pump(),
        ) else {
            panic!("missing editor status")
        };
        assert_eq!(status.scaling, EditorScaling::Plugin);
        assert_eq!((status.scale, status.width, status.height), (2.0, 640, 400));
        let deadline = Instant::now() + Duration::from_millis(250);
        while Instant::now() < deadline {
            parent.pump();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_fixture_pixels(&parent.capture(), 640 * 400);
    }
}

#[test]
#[ignore = "native fixtures and display required"]
#[cfg(any(windows, target_os = "linux"))]
fn embedded_editors_render_close_and_preserve_audio() {
    exercise(false);
}
#[test]
#[ignore = "native fixtures and display required"]
#[cfg(any(windows, target_os = "linux"))]
fn sandboxed_embedded_editors() {
    exercise(true);
}
#[test]
#[ignore = "native fixtures and display required"]
#[cfg(target_os = "macos")]
fn embedding_is_explicitly_unsupported_on_macos() {
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let instance = spawn(format, false);
        for parent in [
            ParentWindow::Win32 { hwnd: 0 },
            ParentWindow::X11 { window: 0 },
        ] {
            let event = response(&instance, Command::OpenEditorIn(parent), || {});
            let HostEvent::RequestFailed { reason, .. } = event else {
                panic!("{event:?}")
            };
            assert_eq!(
                reason,
                "embedding into another process's window is not supported on macOS"
            );
        }
    }
}
