#![forbid(unsafe_code)]
use vst_audio_protocol::*;

#[test]
fn parameter_codec_is_bounded_and_preserves_optional_metadata() {
    let messages = [
        Message::ListParameters {
            start: 16,
            count: 32,
        },
        Message::SetParameter {
            id: 99,
            value: 0.25,
        },
        Message::Parameters {
            total: 1,
            entries: vec![Parameter {
                id: 99,
                name: "Gain".into(),
                unit: "dB".into(),
                value: 0.5,
                default: Some(0.25),
                steps: None,
            }],
        },
        Message::RequestError {
            code: 1,
            text: "unknown parameter".into(),
        },
    ];
    for message in messages {
        let frame = Frame {
            flags: 0,
            request: 42,
            message,
        };
        assert_eq!(
            decode_frame(&encode_frame(&frame).unwrap()).unwrap(),
            DecodedFrame::Known(frame)
        );
    }
    for value in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
        let frame = Frame {
            flags: 0,
            request: 1,
            message: Message::SetParameter { id: 0, value },
        };
        assert!(encode_frame(&frame).is_err());
    }
    let frame = Frame {
        flags: 0,
        request: 1,
        message: Message::ListParameters {
            start: 0,
            count: 33,
        },
    };
    assert!(encode_frame(&frame).is_err());
}
