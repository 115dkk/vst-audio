#![forbid(unsafe_code)]

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use vst_audio::{
    Command, Configuration, Host, HostEvent, PluginDescriptor, PluginFormat, RequestId,
};

fn host() -> Host {
    Host::new(PathBuf::from(
        std::env::var_os("VST_AUDIO_WORKER").expect("worker required"),
    ))
}

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

fn vst3_state(mode: u8) -> Vec<u8> {
    vst_audio::state::encode(&[1, mode], &[1, mode]).unwrap()
}

fn failing_state() -> Vec<u8> {
    vst_audio::state::encode(&[255, 255], &[255, 255]).unwrap()
}

#[test]
#[ignore = "native fixtures required"]
fn failed_restore_ends_its_request_once_before_instance_failure() {
    for format in [PluginFormat::Vst3] {
        let instance = host()
            .spawn(&descriptor(format), &Configuration::default(), None)
            .unwrap();
        let request = instance
            .controller()
            .request(Command::RestoreState(failing_state()))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut ordered = Vec::new();
        while Instant::now() < deadline {
            if let Some(event) = instance.try_next_event() {
                match event {
                    HostEvent::RequestFailed { request: id, .. } if id == request => {
                        ordered.push(("request", Some(id)))
                    }
                    HostEvent::Failed { .. } => {
                        ordered.push(("instance", None));
                        break;
                    }
                    _ => {}
                }
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        assert_eq!(
            ordered,
            [("request", Some(RequestId(request.0))), ("instance", None)]
        );
        assert!(!matches!(
            instance.try_next_event(),
            Some(HostEvent::RequestFailed { request: id, .. }) if id == request
        ));
    }
}

#[test]
#[ignore = "native VST3 fixture required"]
fn watch_disable_ends_request_waiting_on_timed_out_worker() {
    let mut instance = host()
        .spawn(
            &descriptor(PluginFormat::Vst3),
            &Configuration {
                recovery: vst_audio::RecoveryPolicy::Watch,
                ..Configuration::default()
            },
            None,
        )
        .unwrap();
    let controller = instance.controller();
    let mut audio = instance.take_audio().unwrap();
    let input = [0.0f32; 128];
    let mut output = [[0.0f32; 128]; 6];
    for _ in 0..2 {
        controller
            .request(Command::RestoreState(vst3_state(4)))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(12);
        while !matches!(instance.try_next_event(), Some(HostEvent::Recovered { .. })) {
            let mut refs: Vec<_> = output.iter_mut().map(|plane| &mut plane[..]).collect();
            let _ = audio.process(&[&input, &input], &mut refs);
            assert!(Instant::now() < deadline, "worker did not recover");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let request = controller
        .request(Command::RestoreState(vst3_state(5)))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut request_failures = 0;
    let mut recovery_required = false;
    while Instant::now() < deadline && !recovery_required {
        if let Some(event) = instance.try_next_event() {
            match event {
                HostEvent::RequestFailed { request: id, .. } if id == request => {
                    request_failures += 1
                }
                HostEvent::RecoveryRequired => recovery_required = true,
                _ => {}
            }
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert!(recovery_required, "third failure did not disable Watch");
    assert_eq!(
        request_failures, 1,
        "pending request was not ended exactly once"
    );
}
