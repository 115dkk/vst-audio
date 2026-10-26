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
