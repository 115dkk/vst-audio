#![forbid(unsafe_code)]
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use vst_audio::{
    AudioStatus, Command, Configuration, Fallback, Host, HostEvent, PluginDescriptor, PluginFormat,
    Status,
};

fn host() -> Host {
    Host::new(PathBuf::from(
        std::env::var_os("VST_AUDIO_WORKER").expect("worker required"),
    ))
}

fn fixture() -> PluginDescriptor {
    PluginDescriptor::new(
        PathBuf::from(std::env::var_os("VST_AUDIO_TEST_VST2").expect("fixture required")),
        PluginFormat::Vst2,
    )
}

fn wait_for_latency_change(instance: &vst_audio::Instance, request: vst_audio::RequestId) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut changed = false;
    let mut completed = false;
    while !changed || !completed {
        if let Some(event) = instance.try_next_event() {
            changed |= matches!(event, HostEvent::LatencyChanged(128));
            completed |= matches!(event, HostEvent::Completed { request: id } if id == request);
        } else {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for latency change"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn wait_for_completion(instance: &vst_audio::Instance, request: vst_audio::RequestId) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if matches!(instance.try_next_event(), Some(HostEvent::Completed { request: id }) if id == request)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for completion"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "native fixtures required"]
fn runtime_latency_is_adopted_at_a_block_boundary_and_aligns_fallback() {
    let mut block_instance = host()
        .spawn(&fixture(), &Configuration::default(), None)
        .unwrap();
    let controller = block_instance.controller();
    let mut blocks = block_instance.take_blocks().unwrap();
    assert_eq!(blocks.plugin_latency_frames(), 64);
    assert_eq!(blocks.latency_frames(), 192);

    let restore = controller
        .request(Command::RestoreState(vec![1, 8]))
        .unwrap();
    wait_for_latency_change(&block_instance, restore);

    assert_eq!(blocks.plugin_latency_frames(), 64);
    assert_eq!(blocks.latency_frames(), 192);
    let input = vec![0.0; 2 * 128];
    let mut output = vec![0.0; 6 * 128];
    blocks.process(&input, &mut output);
    assert_eq!(blocks.plugin_latency_frames(), 128);
    assert_eq!(blocks.latency_frames(), 256);

    let configuration = Configuration {
        fallback: Fallback::DelayedInput,
        ..Configuration::default()
    };
    let mut audio_instance = host().spawn(&fixture(), &configuration, None).unwrap();
    let controller = audio_instance.controller();
    let mut audio = audio_instance.take_audio().unwrap();
    let restore = controller
        .request(Command::RestoreState(vec![1, 8]))
        .unwrap();
    wait_for_latency_change(&audio_instance, restore);

    let zeros = [0.0; 128];
    let mut rendered = [[0.0; 128]; 6];
    let mut rendered_refs: Vec<_> = rendered.iter_mut().map(|plane| &mut plane[..]).collect();
    audio.process(&[&zeros, &zeros], &mut rendered_refs);
    assert_eq!(audio.latency_frames(), 384);

    let crash = controller
        .request(Command::RestoreState(vec![1, 1]))
        .unwrap();
    wait_for_completion(&audio_instance, crash);
    let deadline = Instant::now() + Duration::from_secs(10);
    while controller.status() != Status::Failed {
        audio.process(&[&zeros, &zeros], &mut rendered_refs);
        assert!(Instant::now() < deadline, "worker did not fail");
        std::thread::sleep(Duration::from_millis(5));
    }

    let mut impulse = [0.0; 128];
    impulse[0] = 1.0;
    let mut fallback = Vec::new();
    for block in 0..4 {
        let left = if block == 0 { &impulse } else { &zeros };
        let status = audio.process(&[left, &zeros], &mut rendered_refs);
        assert_eq!(status, AudioStatus::Fallback);
        fallback.extend_from_slice(rendered_refs[0]);
    }
    assert!(fallback[..384].iter().all(|sample| *sample == 0.0));
    assert_eq!(fallback[384], 1.0);
    assert!(fallback[385..].iter().all(|sample| *sample == 0.0));
}
