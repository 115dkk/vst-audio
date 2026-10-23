#![forbid(unsafe_code)]
use vst_audio_ring::*;
#[test]
fn double_precision_and_bounded_packets_round_trip() {
    let geometry = Geometry {
        sample_format: SampleFormat::F64,
        lanes: 1,
        slots: 2,
        input_channels: 0,
        output_channels: 2,
        max_frames: 64,
        sample_rate: 48000,
        latency_bound_frames: 48000,
    };
    let storage = RegionBuf::new(geometry).unwrap();
    let ready = InProcessEvent::new();
    let mut producer = Producer::new(storage.region(), [None, None], &ready);
    producer.announce(geometry, 1).unwrap();
    let mut consumer = Consumer::new(storage.region(), &ready, [None, None]);
    assert_eq!(consumer.validate_and_ready(2).unwrap(), geometry);
    let value = 1.0 + 2.0f64.powi(-40);
    let event = Event::midi(17, 0, [0x90, 60, 100]);
    let context = Transport {
        sample_position: 12345,
        tempo: 137.0,
        ..Default::default()
    };
    producer.try_publish(0, 1, |slot| {
        slot.write_f64(0, 0, value);
        slot.set_transport(context);
        assert!(slot.set_events(&[event]));
    });
    let mut actual = Transport::default();
    let mut events = EventBuffer::default();
    assert!(consumer.input_packet(0, 1, &mut actual, &mut events));
    assert_eq!(actual, context);
    assert_eq!(events.as_slice(), [event]);
    let mut samples = vec![0.0; 128];
    assert!(
        consumer.slot_mut_f64(0, 1, &mut samples, |samples| assert_eq!(
            samples[0].to_bits(),
            value.to_bits()
        ))
    );
    assert!(consumer.output_packet(0, 1, &events));
    consumer.complete(0, 1);
    assert!(producer.try_read(0, 1, |slot| {
        assert_eq!(slot.read_f64(0, 0).to_bits(), value.to_bits());
        let mut output = EventBuffer::default();
        assert!(slot.output_events(&mut output));
        assert_eq!(output.as_slice(), [event]);
    }));
}
