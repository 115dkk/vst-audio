#![forbid(unsafe_code)]

use std::{path::PathBuf, process::Command};
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
