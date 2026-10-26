#![forbid(unsafe_code)]

use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};
use vst_audio::*;

const BUS_COUNT_CHANGE: &str = "VST_AUDIO_TEST_VST3_BUS_COUNT_CHANGE";

fn start_sidechain() -> Result<Started, StartError> {
    let worker = PathBuf::from(std::env::var_os("VST_AUDIO_WORKER").expect("worker required"));
    let mut plugin = PluginDescriptor::new(
        PathBuf::from(std::env::var_os("VST_AUDIO_ADVANCED_VST3").expect("advanced VST3 required")),
        PluginFormat::Vst3,
    );
    let host = Host::new(&worker);
    let host = if std::env::var_os("VST_AUDIO_CROSS_ARCH").is_some() {
        let architectures = plugin_architectures(&worker, PluginFormat::Vst2).unwrap();
        assert_eq!(architectures.len(), 1);
        host.with_worker(architectures[0], worker.clone())
    } else {
        host
    };
    plugin.class_id = Some(
        host.scan(&plugin)
            .unwrap()
            .into_iter()
            .find(|class| class.name == "Test sidechain")
            .expect("sidechain class required")
            .id,
    );
    PluginProcess::start_configured(
        &worker,
        &plugin.path,
        plugin.format,
        plugin.class_id,
        &ProcessingOptions::default(),
        None,
    )
}

#[test]
#[ignore = "native fixtures required"]
fn full_parameter_queue_rejects_without_changing_controller_value() {
    let worker = PathBuf::from(std::env::var_os("VST_AUDIO_WORKER").expect("worker required"));
    let plugin = PluginDescriptor::new(
        PathBuf::from(std::env::var_os("VST_AUDIO_TEST_VST3").expect("VST3 fixture required")),
        PluginFormat::Vst3,
    );
    let instance = Host::new(worker)
        .spawn(&plugin, &Configuration::default(), None)
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
            assert!(Instant::now() < deadline, "parameter request timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    let parameters = || {
        let request = control
            .request(vst_audio::Command::ListParameters { start: 0, count: 1 })
            .unwrap();
        let HostEvent::Parameters { entries, .. } = wait(request) else {
            panic!("missing parameter page");
        };
        entries.into_iter().next().expect("fixture parameter")
    };
    let parameter = parameters();
    assert_eq!(parameter.name, "Width");
    assert_eq!(parameter.value, 1.0);
    let mut accepted_value = parameter.value;
    let mut accepted = 0;
    // Never take/process audio: no consumer can drain these accepted edits.
    // Discover overflow through the public reply, not the private queue capacity.
    for attempt in 0..4096 {
        let value = if attempt % 2 == 0 { 0.25 } else { 0.75 };
        let request = control
            .request(vst_audio::Command::SetParameter {
                id: parameter.id,
                value,
            })
            .unwrap();
        match wait(request) {
            HostEvent::Completed { .. } => {
                accepted_value = value;
                accepted += 1;
            }
            HostEvent::RequestFailed { reason, .. } => {
                assert!(reason.contains("parameter queue full"), "{reason}");
                assert!(accepted > 0, "no edit was admitted before overflow");
                assert_ne!(value, accepted_value);
                assert_eq!(
                    parameters().value,
                    accepted_value,
                    "failed SetParameter changed the controller value after {accepted} accepted edits"
                );
                assert_eq!(instance.status(), Status::Running);
                return;
            }
            event => panic!("unexpected parameter response: {event:?}"),
        }
    }
    panic!("parameter queue did not reject a bounded burst without audio processing");
}

#[test]
#[ignore = "native fixtures required"]
fn bus_count_changes_during_negotiation_are_rejected() {
    if let Ok(mode) = std::env::var(BUS_COUNT_CHANGE) {
        let error = start_sidechain().expect_err("changed bus count must reject public start");
        assert!(
            error
                .to_string()
                .contains("plugin changed bus count during arrangement negotiation"),
            "{mode}: expected explicit bus-count error, got {error}"
        );
        return;
    }

    // Establish that the unchanged fixture can start with both declared buses.
    let started = start_sidechain().unwrap();
    assert_eq!(started.setup.inputs.len(), 2);
    assert_eq!(started.setup.outputs.len(), 2);
    drop(started);

    // Each subprocess owns its environment, so parallel tests never mutate the
    // application's process-global environment while workers are being spawned.
    let mut failures = Vec::new();
    for mode in ["input-add", "output-add", "input-drop", "output-drop"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "bus_count_changes_during_negotiation_are_rejected",
                "--ignored",
                "--nocapture",
            ])
            .env(BUS_COUNT_CHANGE, mode)
            .output()
            .unwrap();
        if !output.status.success() {
            failures.push(format!(
                "{mode}: {}\n{}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
