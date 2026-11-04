#![forbid(unsafe_code)]

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use vst_audio::{
    Command, Configuration, Host, PluginDescriptor, PluginFormat, RenderError, Rendered,
    SampleFormat, Status,
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

fn matrix_signal(frames: usize) -> Vec<f32> {
    let mut input = vec![0.0; 2 * frames];
    for frame in 0..frames {
        input[frame] = ((frame % 251) as f32 - 125.0) / 256.0;
        input[frames + frame] = ((frame * 7 % 257) as f32 - 128.0) / 512.0;
    }
    input
}

fn matrix_expected(input: &[f32], frames: usize, tail: usize) -> Vec<f32> {
    let length = frames + tail;
    let mut output = vec![0.0; 6 * length];
    for frame in 0..frames {
        let left = input[frame];
        let right = input[frames + frame];
        let expected = [
            left,
            right,
            (left + right) / 2.0,
            (left + right) / 2.0,
            left / 2.0,
            right / 2.0,
        ];
        for (channel, sample) in expected.into_iter().enumerate() {
            output[channel * length + frame] = sample;
        }
    }
    output
}

fn matrix_fixture(format: PluginFormat) -> PluginDescriptor {
    let variable = match format {
        PluginFormat::Vst2 => "VST_AUDIO_TEST_VST2",
        PluginFormat::Vst3 => "VST_AUDIO_TEST_VST3",
    };
    PluginDescriptor::new(
        PathBuf::from(std::env::var_os(variable).expect("fixture required")),
        format,
    )
}

#[test]
#[ignore = "native fixtures required"]
fn complete_render_is_aligned_exact_and_reusable() {
    const RENDER_FRAMES: usize = 1061;
    const TAIL: usize = 193;
    let input = matrix_signal(RENDER_FRAMES);
    let expected = matrix_expected(&input, RENDER_FRAMES, TAIL);

    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let mut instance = host()
            .spawn(&matrix_fixture(format), &configuration(), None)
            .unwrap();
        let mut blocks = instance.take_blocks().unwrap();
        for _ in 0..2 {
            let mut output = vec![-99.0; expected.len()];
            blocks
                .render(&input, RENDER_FRAMES, TAIL, &mut output, CALL_TIMEOUT)
                .unwrap();
            assert_eq!(output, expected, "format={format:?}");
        }
    }
}

#[test]
#[ignore = "native fixtures required"]
fn complete_render_validates_lengths_and_zero_length() {
    let mut instance = host().spawn(&fixture(), &configuration(), None).unwrap();
    let mut blocks = instance.take_blocks().unwrap();
    let mut output = vec![0.0; 6];
    assert_eq!(
        blocks.render(&[0.0], 1, 0, &mut output, CALL_TIMEOUT),
        Err(RenderError::InvalidBuffer)
    );
    assert_eq!(
        blocks.render(&[0.0; 2], 1, 0, &mut [], CALL_TIMEOUT),
        Err(RenderError::InvalidBuffer)
    );
    assert_eq!(blocks.render(&[], 0, 0, &mut [], CALL_TIMEOUT), Ok(()));
}

#[test]
#[ignore = "native fixtures required"]
fn complete_render_f64_preserves_known_transfer() {
    let path = PathBuf::from(
        std::env::var_os("VST_AUDIO_ADVANCED_VST3").expect("advanced fixture required"),
    );
    let descriptor = PluginDescriptor::new(path, PluginFormat::Vst3);
    let host = host();
    let class = host
        .scan(&descriptor)
        .unwrap()
        .into_iter()
        .find(|class| class.name == "Test sidechain")
        .expect("sidechain effect");
    let descriptor = PluginDescriptor {
        class_id: Some(class.id),
        ..descriptor
    };
    let config = Configuration {
        sample_format: SampleFormat::F64,
        ..configuration()
    };
    let mut instance = host.spawn(&descriptor, &config, None).unwrap();
    let mut blocks = instance.take_blocks().unwrap();
    let channels = blocks.geometry().input_channels as usize;
    let output_channels = blocks.geometry().output_channels as usize;
    let frames = 1061;
    let tail = 193;
    let length = frames + tail;
    let mut input = vec![0.0; channels * frames];
    for frame in 0..frames {
        input[frame] = frame as f64 * 0.000000123456789012345;
        input[frames + frame] = -input[frame] * 0.987654321012345;
    }
    let mut expected = vec![0.0; output_channels * length];
    for channel in 0..2 {
        expected[channel * length..channel * length + frames]
            .copy_from_slice(&input[channel * frames..(channel + 1) * frames]);
    }
    let mut output = vec![-99.0; expected.len()];
    blocks
        .render_f64(&input, frames, tail, &mut output, CALL_TIMEOUT)
        .unwrap();
    assert_eq!(output, expected);
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
