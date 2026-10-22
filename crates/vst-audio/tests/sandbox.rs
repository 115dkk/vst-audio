#![forbid(unsafe_code)]
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use vst_audio::{
    ControlEvent, Host, PluginDescriptor, PluginFormat, PluginProcess, ProcessingOptions,
    SandboxPolicy, Started,
};
use vst_audio_protocol::Message;
use vst_audio_ring::{Publish, Wait};

fn response(started: &mut Started, request: u32) -> Message {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            Instant::now() < deadline,
            "sandbox worker response timed out"
        );
        for event in started.control.poll(started.control.elapsed_ms()) {
            if let ControlEvent::Message(frame) = event
                && frame.request == request
            {
                return frame.message;
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
#[ignore = "native fixtures required"]
fn sandbox_probes_and_audio() {
    // The harness supplies these to its single-threaded test subprocess, avoiding
    // unsafe process-global environment mutation in a multi-threaded Rust test.
    let port: u16 = std::env::var("VST_AUDIO_PROBE_PORT")
        .unwrap()
        .parse()
        .unwrap();
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let file = PathBuf::from(std::env::var_os("VST_AUDIO_PROBE_FILE").unwrap());
    let worker = PathBuf::from(std::env::var_os("VST_AUDIO_WORKER").unwrap());
    let unrelated = tempfile::tempdir().unwrap();
    for (label, policy, expected) in [
        ("none", None, [1, 1, 1]),
        ("default", Some(SandboxPolicy::default()), [0, 0, 0]),
        (
            "network",
            Some(SandboxPolicy {
                network: true,
                ..Default::default()
            }),
            [1, 0, 0],
        ),
        (
            "writable-outside",
            Some(SandboxPolicy {
                writable: vec![unrelated.path().into()],
                ..Default::default()
            }),
            [0, 0, 0],
        ),
        (
            "writable",
            Some(SandboxPolicy {
                writable: vec![file.parent().unwrap().into()],
                ..Default::default()
            }),
            [0, 1, 0],
        ),
    ] {
        for (format, key) in [
            (PluginFormat::Vst2, "VST_AUDIO_TEST_VST2"),
            (PluginFormat::Vst3, "VST_AUDIO_TEST_VST3"),
        ] {
            let plugin = PathBuf::from(std::env::var_os(key).unwrap());

            if file.exists() {
                std::fs::remove_file(&file).unwrap();
            }
            let state = if format == PluginFormat::Vst3 {
                vst_audio::state::encode(&[1, 6], &[1, 6]).unwrap()
            } else {
                vec![1, 6]
            };
            let mut started = PluginProcess::start_configured(
                &worker,
                &plugin,
                format,
                None,
                &ProcessingOptions {
                    sandbox: policy.clone(),
                    ..Default::default()
                },
                Some(&state),
            )
            .unwrap_or_else(|error| panic!("{format:?} {label}: {error:?}"));
            assert_eq!(started.sandbox.is_some(), policy.is_some());
            if let Some(report) = &started.sandbox {
                assert!(!report.mechanism.is_empty());
                assert_eq!(report.network_denied, !policy.as_ref().unwrap().network);
                assert!(report.processes_denied && report.writes_restricted);
            }
            let request = started.control.get_state().unwrap();
            let Message::State { blob } = response(&mut started, request) else {
                panic!("expected state");
            };
            let state = if format == PluginFormat::Vst3 {
                vst_audio::state::decode(&blob).unwrap().0.to_vec()
            } else {
                blob
            };
            assert_eq!(
                state,
                [vec![1, 6], expected.to_vec()].concat(),
                "{format:?} {label}"
            );
            assert_eq!(file.exists(), expected[1] == 1);
            let mut connections = 0;
            while listener.accept().is_ok() {
                connections += 1;
            }
            assert_eq!(connections > 0, expected[0] == 1);
            assert_eq!(
                started.publisher.try_publish(
                    started.region.region(),
                    0,
                    1,
                    Some(&*started.work),
                    |slot| {
                        for frame in 0..slot.frames() {
                            slot.write(0, frame, 0.25);
                            slot.write(1, frame, 0.5);
                        }
                    }
                ),
                Publish::Published
            );
            let deadline = Instant::now() + Duration::from_secs(5);
            while started.publisher.completed(started.region.region(), 0) < 1 {
                assert!(
                    Instant::now() < deadline,
                    "sandbox worker failed to process audio"
                );
                started.done.wait(Duration::from_millis(10));
            }
            assert!(
                started
                    .publisher
                    .try_read(started.region.region(), 0, 1, |slot| {
                        assert!((slot.read(0, 100) - 0.25).abs() < 1e-6);
                        assert!((slot.read(1, 100) - 0.5).abs() < 1e-6);
                    })
            );
            let editor = started.control.open_editor().unwrap();
            assert_eq!(response(&mut started, editor), Message::Ack);
            started.control.close_editor().unwrap();
            assert_eq!(started.control.exit_code().unwrap(), None);
            started.control.shutdown().unwrap();
            eprintln!("{format:?} {label}: TCP/file/process={expected:?}; audio and editor passed");
            let descriptor = PluginDescriptor::new(&plugin, format);
            let host = vst_audio::plugin_architectures(&plugin, format)
                .unwrap()
                .into_iter()
                .fold(Host::new(&worker), |host, architecture| {
                    host.with_worker(architecture, &worker)
                });
            assert!(
                !host
                    .scan_sandboxed(&descriptor, &SandboxPolicy::default())
                    .unwrap()
                    .is_empty()
            );
            let invalid = SandboxPolicy {
                writable: vec![file.join("missing")],
                ..Default::default()
            };
            assert!(host.scan_sandboxed(&descriptor, &invalid).is_err());
        }
    }
}
