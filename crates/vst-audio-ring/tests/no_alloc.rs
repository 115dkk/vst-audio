#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use vst_audio_ring::{
    BlockLane, Event, EventBuffer, Geometry, Producer, Publish, RegionBuf, SampleFormat, Signal,
    Wait, Waited,
};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[derive(Clone, Copy)]
struct Noop;

impl Signal for Noop {
    fn signal(&self) {}
}

impl Wait for Noop {
    fn wait(&self, _timeout: Duration) -> Waited {
        Waited::TimedOut
    }
}

#[test]
fn producer_per_block_path_does_not_allocate() {
    let storage = RegionBuf::new(Geometry {
        sample_format: vst_audio_ring::SampleFormat::F32,
        lanes: 1,
        slots: 2,
        input_channels: 2,
        output_channels: 6,
        max_frames: 480,
        sample_rate: 48_000,
        latency_bound_frames: 1_024,
    })
    .unwrap();
    let noop = Noop;
    let mut producer = Producer::new(storage.region(), [Some(&noop), None], &noop);
    producer
        .announce(
            Geometry {
                sample_format: vst_audio_ring::SampleFormat::F32,
                lanes: 1,
                slots: 2,
                input_channels: 2,
                output_channels: 6,
                max_frames: 480,
                sample_rate: 48_000,
                latency_bound_frames: 1_024,
            },
            1,
        )
        .unwrap();
    let mut consumer = vst_audio_ring::Consumer::new(storage.region(), &noop, [None, None]);
    consumer.validate_and_ready(2).unwrap();
    let publisher = producer.publisher().unwrap();
    let touched = AtomicU32::new(0);
    let output_event = Event::midi(17, 0, [0x90, 60, 100]);
    assert_eq!(
        publisher.try_publish(storage.region(), 0, 1, Some(&noop), |slot| {
            slot.write(0, 0, 0.5);
        }),
        Publish::Published
    );
    let mut events = EventBuffer::default();
    events.push(output_event);
    assert!(consumer.output_packet(0, 1, &events));
    consumer.complete(0, 1);
    let mut output = EventBuffer::default();

    assert_no_alloc(|| {
        assert_eq!(publisher.completed(storage.region(), 0), 1);
        assert!(publisher.try_read(storage.region(), 0, 1, |slot| {
            assert_eq!(slot.read(0, 0), 0.5);
            assert!(slot.output_events(&mut output));
            assert_eq!(output.as_slice(), [output_event]);
            touched.fetch_add(1, Ordering::Relaxed);
        }));
    });
    assert_eq!(touched.load(Ordering::Relaxed), 1);
}

#[test]
fn block_lane_planes_do_not_allocate() {
    for sample_format in [SampleFormat::F32, SampleFormat::F64] {
        let geometry = Geometry {
            sample_format,
            lanes: 1,
            slots: 2,
            input_channels: 1,
            output_channels: 1,
            max_frames: 2,
            sample_rate: 48_000,
            latency_bound_frames: 1_024,
        };
        let storage = std::sync::Arc::new(RegionBuf::new(geometry).unwrap());
        let noop = Noop;
        let mut producer = Producer::new(storage.region(), [Some(&noop), None], &noop);
        producer.announce(geometry, 1).unwrap();
        let publisher = producer.publisher().unwrap();
        let mut consumer = vst_audio_ring::Consumer::new(storage.region(), &noop, [None, None]);
        consumer.validate_and_ready(2).unwrap();
        let mut lane = BlockLane::new(storage.clone(), noop, publisher, 0, 1).unwrap();

        match sample_format {
            SampleFormat::F32 => {
                let input = [0.25, 0.5];
                let mut output = [0.0; 2];
                assert!(lane.process_planes(&input, &mut output).unwrap().published);
                let sequence = consumer.next_published(0).unwrap();
                let mut samples = [0.0; 2];
                assert!(consumer.slot_mut(0, sequence, &mut samples, |samples| {
                    samples.copy_from_slice(&[0.75, 1.0]);
                }));
                consumer.complete(0, sequence);
                assert_no_alloc(|| {
                    let exchange = lane.process_planes(&input, &mut output).unwrap();
                    assert!(exchange.delivered);
                    assert_eq!(output, [0.75, 1.0]);
                });
            }
            SampleFormat::F64 => {
                let input = [0.25, 0.5];
                let mut output = [0.0; 2];
                assert!(
                    lane.process_planes_f64(&input, &mut output)
                        .unwrap()
                        .published
                );
                let sequence = consumer.next_published(0).unwrap();
                let mut samples = [0.0; 2];
                assert!(consumer.slot_mut_f64(0, sequence, &mut samples, |samples| {
                    samples.copy_from_slice(&[0.75, 1.0]);
                }));
                consumer.complete(0, sequence);
                assert_no_alloc(|| {
                    let exchange = lane.process_planes_f64(&input, &mut output).unwrap();
                    assert!(exchange.delivered);
                    assert_eq!(output, [0.75, 1.0]);
                });
            }
        }
    }
}
