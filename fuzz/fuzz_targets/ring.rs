#![no_main]
#![forbid(unsafe_code)]
use libfuzzer_sys::fuzz_target;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use vst_audio_ring::*;

fuzz_target!(|data: &[u8]| {
    let geometry = Geometry {
        sample_format: SampleFormat::F64,
        lanes: 1,
        input_channels: 0,
        output_channels: 2,
        max_frames: 8,
        sample_rate: 48000,
        latency_bound_frames: 256,
    };
    let words: Vec<_> = (0..geometry.total_bytes().unwrap() / 4 - 32)
        .map(|_| AtomicU32::new(0))
        .collect();
    let counters: Vec<_> = (0..16).map(|_| AtomicU64::new(0)).collect();
    let region = Region::new(&words, &counters);
    let event = InProcessEvent::new();
    let mut producer = Producer::new(region, [None, None], &event);
    producer.announce(geometry, 1).unwrap();
    // A list of index/value mutations exercises headers and bounded event data.
    for mutation in data.as_chunks::<8>().0 {
        let index = u32::from_le_bytes(mutation[..4].try_into().unwrap()) as usize % words.len();
        words[index].store(
            u32::from_le_bytes(mutation[4..].try_into().unwrap()),
            Ordering::Relaxed,
        );
    }
    let mut consumer = Consumer::new(region, &event, [None, None]);
    if let Ok(accepted) = consumer.validate_and_ready(2) {
        assert!(accepted.total_bytes().unwrap() <= geometry.total_bytes().unwrap());
        let _ = producer.try_publish(0, 1, |slot| {
            slot.write_f64(0, 0, 1.0 + 2.0f64.powi(-40));
            slot.set_transport(Transport::default());
            slot.set_events(&[Event::midi(3, 0, [0x90, 60, 100])]);
        });
        for mutation in data.as_chunks::<8>().0 {
            let index =
                u32::from_le_bytes(mutation[..4].try_into().unwrap()) as usize % words.len();
            words[index].store(
                u32::from_le_bytes(mutation[4..].try_into().unwrap()),
                Ordering::Relaxed,
            );
        }
        let mut context = Transport::default();
        let mut events = EventBuffer::default();
        let _ = consumer.input_packet(0, 1, &mut context, &mut events);
        let _ = consumer.output_packet(0, 1, &events);
        consumer.complete(0, 1);
        let _ = producer.try_read(0, 1, |slot| {
            let _ = slot.output_events(&mut events);
        });
    }
});
