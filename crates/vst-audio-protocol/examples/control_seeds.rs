//! Writes one control-corpus seed per protocol message kind with the real
//! encoder, or verifies a corpus directory. tools/fuzz_tests.py runs it.
#![forbid(unsafe_code)]
use std::{env, fs, path::Path};
use vst_audio_protocol::{
    Capabilities, DecodedFrame, EditPhase, EditorAction, EditorImage, EditorPlatform,
    EditorScaling, EditorStatus, Frame, Message, ParentWindow, PluginFormat, ProcessingSetup,
    SampleFormat, decode_frame, encode_frame,
};

fn messages() -> Vec<(&'static str, Message)> {
    vec![
        ("hello", Message::hello()),
        (
            "load",
            Message::Load {
                path: "plugin.vst3".into(),
                format: PluginFormat::Vst3,
                class_id: None,
            },
        ),
        (
            "configure",
            Message::Configure {
                rate: 48_000,
                max_frames: 512,
                in_channels: 2,
                out_channels: 2,
            },
        ),
        ("activate", Message::Activate),
        (
            "set-state",
            Message::SetState {
                blob: b"state".to_vec(),
            },
        ),
        ("get-state", Message::GetState),
        (
            "open-editor",
            Message::OpenEditor {
                scaling: EditorScaling::Auto,
            },
        ),
        ("shutdown", Message::Shutdown),
        (
            "scan",
            Message::Scan {
                path: "plugin.vst3".into(),
            },
        ),
        (
            "loaded",
            Message::Loaded {
                name: "plugin".into(),
                vendor: "vendor".into(),
                latency_frames: 0,
                inputs: vec![],
                outputs: vec![],
            },
        ),
        ("classes", Message::Classes { entries: vec![] }),
        ("latency-changed", Message::LatencyChanged { frames: 64 }),
        (
            "state",
            Message::State {
                blob: b"state".to_vec(),
            },
        ),
        ("params-changed", Message::ParamsChanged),
        (
            "fault",
            Message::Fault {
                code: 1,
                text: "fault".into(),
            },
        ),
        ("ack", Message::Ack),
        ("editor-closed", Message::EditorClosed),
        ("close-editor", Message::CloseEditor),
        (
            "list-parameters",
            Message::ListParameters { start: 0, count: 1 },
        ),
        (
            "parameters",
            Message::Parameters {
                total: 0,
                entries: vec![],
            },
        ),
        ("set-parameter", Message::SetParameter { id: 1, value: 0.5 }),
        (
            "request-error",
            Message::RequestError {
                code: 1,
                text: "error".into(),
            },
        ),
        (
            "prepare",
            Message::Prepare(ProcessingSetup {
                sample_rate: 48_000,
                quantum: 512,
                sample_format: SampleFormat::F32,
                offline: false,
                inputs: vec![],
                outputs: vec![],
            }),
        ),
        ("describe", Message::Describe),
        ("features", Message::Features(Capabilities::default())),
        ("editor", Message::Editor(EditorAction::Show)),
        (
            "editor-status",
            Message::EditorStatus(EditorStatus {
                platform: EditorPlatform::X11,
                scaling: EditorScaling::Auto,
                visible: true,
                embedded: false,
                width: 640,
                height: 480,
                scale: 1.0,
                resize_supported: true,
                scale_supported: true,
            }),
        ),
        (
            "editor-image",
            Message::EditorImage(EditorImage {
                width: 1,
                height: 1,
                rgba: vec![32, 64, 128, 255],
            }),
        ),
        (
            "sandboxed",
            Message::Sandboxed {
                mechanism: "test".into(),
            },
        ),
        (
            "open-editor-in",
            Message::OpenEditorIn {
                parent: ParentWindow::X11 { window: 1 },
                scaling: EditorScaling::Auto,
            },
        ),
        (
            "editor-resized",
            Message::EditorResized {
                width: 640,
                height: 480,
            },
        ),
        (
            "parameter-edit",
            Message::ParameterEdit {
                id: 1,
                phase: EditPhase::Perform,
                value: 0.5,
            },
        ),
    ]
}

fn verify(directory: &Path) {
    let mut kinds = Vec::new();
    for entry in fs::read_dir(directory)
        .expect("read corpus")
        .map(Result::unwrap)
    {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("bin") {
            continue;
        }
        let bytes = fs::read(&path).expect("read seed");
        let DecodedFrame::Known(frame) = decode_frame(&bytes).expect("seed must decode") else {
            panic!("{} decoded as Unknown", path.display());
        };
        assert_eq!(
            encode_frame(&frame).expect("re-encode seed"),
            bytes,
            "{}",
            path.display()
        );
        kinds.push(frame.message.kind());
    }
    kinds.sort_unstable();
    assert_eq!(kinds, (1_u16..=32).collect::<Vec<_>>());
    println!(
        "verified {} Known control seeds (kinds 1..=32)",
        kinds.len()
    );
}

fn main() {
    let mut args = env::args_os().skip(1);
    let mode = args.next().expect("mode");
    let directory = args.next().expect("directory");
    assert!(args.next().is_none(), "unexpected argument");
    let directory = Path::new(&directory);
    if mode == "generate" {
        fs::create_dir_all(directory).expect("create output");
        let messages = messages();
        assert_eq!(messages.len(), 32);
        for (expected_kind, (name, message)) in (1_u16..=32).zip(messages) {
            assert_eq!(message.kind(), expected_kind, "{name}");
            let bytes = encode_frame(&Frame {
                flags: 0,
                request: u32::from(expected_kind),
                message,
            })
            .expect("encode seed");
            assert!(
                matches!(decode_frame(&bytes), Ok(DecodedFrame::Known(_))),
                "{name}"
            );
            fs::write(
                directory.join(format!("known-{expected_kind:02}-{name}.bin")),
                bytes,
            )
            .expect("write seed");
        }
    } else if mode != "verify" {
        panic!("mode must be generate or verify");
    }
    verify(directory);
}
