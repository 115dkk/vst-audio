#![forbid(unsafe_code)]

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use vst_audio::{
    Command, Configuration, Host, PluginDescriptor, PluginFormat, RenderError, Rendered, Status,
};

const FRAMES: usize = 128;
const SOURCE_BLOCKS: usize = 64;
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

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

fn configuration() -> Configuration {
    Configuration {
        slots: Some(8),
        offline: true,
        ..Configuration::default()
    }
}

fn input_sample(channel: usize, frame: usize) -> f32 {
    match channel {
        0 => ((frame % 251) as f32 - 125.0) * 0.001,
        1 => ((frame * 7 % 257) as f32 - 128.0) * 0.0007,
        _ => unreachable!(),
    }
}

fn input_block(block: usize) -> Vec<f32> {
    let mut input = vec![0.0; 2 * FRAMES];
    for channel in 0..2 {
        for frame in 0..FRAMES {
            input[channel * FRAMES + frame] = input_sample(channel, block * FRAMES + frame);
        }
    }
    input
}

fn verify_output(output: &[f32], block: usize) {
    for frame in 0..FRAMES {
        let delayed = (block * FRAMES + frame).checked_sub(64);
        let left = delayed.map_or(0.0, |index| input_sample(0, index));
        let right = delayed.map_or(0.0, |index| input_sample(1, index));
        let expected = [
            left,
            right,
            0.5 * (left + right),
            0.5 * (left + right),
            0.5 * left,
            0.5 * right,
        ];
        for (channel, expected) in expected.into_iter().enumerate() {
            let actual = output[channel * FRAMES + frame];
            assert!(
                (actual - expected).abs() < 1e-6,
                "block={block}, channel={channel}, frame={frame}: {actual} != {expected}"
            );
        }
    }
}

fn render_at_depth(depth: u32) {
    let mut instance = host().spawn(&fixture(), &configuration(), None).unwrap();
    let mut blocks = instance.take_blocks().unwrap();
    blocks.restart(depth).unwrap();
    let silence = vec![0.0; 2 * FRAMES];
    let mut output = vec![-99.0; 6 * FRAMES];

    for call in 0..SOURCE_BLOCKS + depth as usize {
        output.fill(-99.0);
        let input = if call < SOURCE_BLOCKS {
            input_block(call)
        } else {
            silence.clone()
        };
        let rendered = blocks
            .process_wait(&input, &mut output, CALL_TIMEOUT)
            .unwrap();
        if call < depth as usize {
            assert_eq!(rendered, Rendered::Priming);
            assert!(output.iter().all(|sample| *sample == -99.0));
        } else {
            assert_eq!(rendered, Rendered::Delivered);
            verify_output(&output, call - depth as usize);
        }
    }
}

#[test]
#[ignore = "native fixtures required"]
fn lossless_render_delivers_every_block_at_minimum_and_maximum_depth() {
    render_at_depth(1);

    let mut instance = host().spawn(&fixture(), &configuration(), None).unwrap();
    let max_depth = instance.take_blocks().unwrap().max_depth();
    drop(instance);
    render_at_depth(max_depth);
}

#[test]
#[ignore = "native fixtures required"]
fn stopped_worker_never_delivers() {
    let mut instance = host().spawn(&fixture(), &configuration(), None).unwrap();
    let controller = instance.controller();
    let mut blocks = instance.take_blocks().unwrap();
    controller.request(Command::Shutdown).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while controller.status() != Status::Stopped {
        assert!(Instant::now() < deadline, "worker did not stop");
        std::thread::sleep(Duration::from_millis(5));
    }

    let input = vec![0.0; 2 * FRAMES];
    let mut output = vec![-1.0; 6 * FRAMES];
    let timeout = Duration::from_millis(200);
    let started = Instant::now();
    let result = blocks.process_wait(&input, &mut output, timeout);
    assert!(matches!(
        result,
        Err(RenderError::NotRunning | RenderError::TimedOut | RenderError::Recovered)
    ));
    assert!(started.elapsed() <= timeout + Duration::from_secs(1));
    assert!(output.iter().all(|sample| *sample == -1.0));
}

#[test]
#[ignore = "native fixtures required"]
fn hung_worker_times_out() {
    let state = [1, 2];
    let mut instance = host()
        .spawn(&fixture(), &configuration(), Some(&state))
        .unwrap();
    let mut blocks = instance.take_blocks().unwrap();
    let input = vec![0.0; 2 * FRAMES];
    let mut output = vec![-1.0; 6 * FRAMES];
    assert_eq!(
        blocks.process_wait(&input, &mut output, CALL_TIMEOUT),
        Ok(Rendered::Priming)
    );

    let timeout = Duration::from_millis(200);
    let started = Instant::now();
    assert_eq!(
        blocks.process_wait(&input, &mut output, timeout),
        Err(RenderError::TimedOut)
    );
    assert!(started.elapsed() <= timeout + Duration::from_secs(1));
    assert!(output.iter().all(|sample| *sample == -1.0));
}
