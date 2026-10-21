#![no_main]
#![forbid(unsafe_code)]
use libfuzzer_sys::fuzz_target;
use vst_audio_protocol::{DecodedFrame, decode_frame, encode_frame, read_frame, state};

fuzz_target!(|data: &[u8]| {
    if let Ok(DecodedFrame::Known(frame)) = decode_frame(data) {
        let encoded = encode_frame(&frame).expect("decoded frame must re-encode");
        let Ok(DecodedFrame::Known(roundtrip)) = decode_frame(&encoded) else {
            panic!("roundtrip rejected")
        };
        // Byte equality preserves even NaN diagnostic payload bits.
        assert_eq!(encode_frame(&roundtrip).unwrap(), encoded);
    }
    let _ = read_frame(&mut std::io::Cursor::new(data));
    if let Ok((component, controller)) = state::decode(data) {
        assert_eq!(state::encode(component, controller).unwrap(), data);
    }
});
