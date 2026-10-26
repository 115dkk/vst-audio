#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use vst_audio_ring::{
    Consumer, Event, EventBuffer, Fault, Geometry, HeaderError, InProcessEvent, Pipeline, Producer,
    Publish, Region, RegionBuf, RegionMemory, State, Waited,
};

fn geometry(lanes: u32) -> Geometry {
    Geometry {
        sample_format: vst_audio_ring::SampleFormat::F32,
        lanes,
        slots: 2,
        input_channels: 2,
        output_channels: 6,
        max_frames: 64,
        sample_rate: 48_000,
        latency_bound_frames: 256,
    }
}

#[test]
fn header_layout_is_pinned() {
    assert_eq!(vst_audio_ring::MAGIC.to_le_bytes(), *b"SSRR");
    assert_eq!(vst_audio_ring::LAYOUT_VERSION, 4);
    assert_eq!(vst_audio_ring::HEADER_BYTES, 512);
    assert_eq!(vst_audio_ring::MIN_REGION_BYTES, 640);
    assert_eq!(vst_audio_ring::LATENCY_BOUND_OFFSET, 32);
    assert_eq!(geometry(1).total_bytes().unwrap(), 299_008);
    assert_eq!(geometry(2).total_bytes().unwrap(), 597_376);
}

#[test]
fn mapping_split_is_aligned_and_bounded() {
    let geometry = geometry(1);
    let total = geometry.total_bytes().unwrap();
    let split = geometry.mapping_split(total).unwrap();
    assert_eq!(split, total - vst_audio_ring::COUNTER_BYTES);
    assert_eq!(split % vst_audio_ring::CACHE_LINE, 0);
    assert_eq!(
        geometry.mapping_split(total - 1),
        Err(vst_audio_ring::GeometryError::RegionTooLarge)
    );
}

#[test]
fn slot_bounds_are_validated() {
    for slots in [2, 4, 8, 16] {
        assert!(
            Geometry {
                slots,
                ..geometry(1)
            }
            .validate()
            .is_ok()
        );
    }
    for slots in [0, 1, 3, 5, 6, 7, 9, 15, 17, u32::MAX] {
        assert_eq!(
            Geometry {
                slots,
                ..geometry(1)
            }
            .validate(),
            Err(vst_audio_ring::GeometryError::Slots)
        );
    }
}

#[test]
fn readiness_in_order_late_drop_fault_and_close() {
    let storage = RegionBuf::new(geometry(1)).unwrap();
    let work = InProcessEvent::new();
    let ready = InProcessEvent::new();
    let done = InProcessEvent::new();
    let mut producer = Producer::new(storage.region(), [Some(&work), None], &ready);
    producer.announce(geometry(1), 11).unwrap();
    assert_eq!(storage.region().state(), Some(State::Announced));
    assert_eq!(producer.wait_ready(Duration::ZERO), Waited::TimedOut);

    let order = Arc::new(Mutex::new(Vec::new()));
    // A failed assertion below must fail the test, not leave the consumer
    // waiting for a block that never comes while the scope joins it.
    let deadline = Instant::now() + Duration::from_secs(10);
    thread::scope(|scope| {
        let order = Arc::clone(&order);
        let storage_ref = &storage;
        let ready_ref = &ready;
        let done_ref = &done;
        let work_ref = &work;
        scope.spawn(move || {
            let mut consumer =
                Consumer::new(storage_ref.region(), ready_ref, [Some(done_ref), None]);
            consumer.validate_and_ready(22).unwrap();
            let mut buffer = vec![0.0; geometry(1).samples_per_slot()];
            for _ in 0..3 {
                loop {
                    if let Some(sequence) = consumer.next_published(0) {
                        order.lock().unwrap().push(sequence);
                        assert!(consumer.slot_mut(0, sequence, &mut buffer, |samples| {
                            for sample in samples {
                                *sample *= 0.5;
                            }
                        }));
                        if sequence == 1 {
                            thread::sleep(Duration::from_millis(10));
                        }
                        consumer.complete(0, sequence);
                        break;
                    }
                    if storage_ref.region().state() == Some(State::Closing)
                        || Instant::now() > deadline
                    {
                        return;
                    }
                    let _ = vst_audio_ring::Wait::wait(work_ref, Duration::from_millis(20));
                }
            }
        });

        assert_eq!(
            producer.wait_ready(Duration::from_secs(1)),
            Waited::Signalled
        );
        assert_eq!(
            producer.try_publish(0, 1, |slot| assert!(slot.copy_from(&vec![2.0; 384]))),
            Publish::Published
        );
        assert_eq!(
            producer.try_publish(0, 2, |slot| assert!(slot.copy_from(&vec![4.0; 384]))),
            Publish::Published
        );
        assert_eq!(producer.try_publish(0, 3, |_| {}), Publish::Dropped);
        // Late: 3 fits once the consumer completes 1, however long that takes.
        while producer.try_publish(0, 3, |slot| assert!(slot.copy_from(&vec![6.0; 384])))
            != Publish::Published
        {
            assert!(Instant::now() < deadline, "the consumer never freed a slot");
            let _ = vst_audio_ring::Wait::wait(&done, Duration::from_millis(20));
        }
        while producer.completed(0) < 3 {
            assert!(Instant::now() < deadline, "the consumer never completed 3");
            let _ = vst_audio_ring::Wait::wait(&done, Duration::from_millis(20));
        }
        let mut first = 0.0;
        assert!(producer.try_read(0, 3, |slot| first = slot.read(0, 0)));
        assert_eq!(first, 3.0);
        producer.close();
    });
    assert_eq!(*order.lock().unwrap(), vec![1, 2, 3]);
    assert_eq!(storage.region().state(), Some(State::Closing));
}

#[test]
fn publisher_and_owned_region_cross_threads_after_announcement() {
    let storage = Arc::new(RegionBuf::new(geometry(1)).unwrap());
    let ready = Arc::new(InProcessEvent::new());
    let work = Arc::new(InProcessEvent::new());
    // The producer only announces; its real-time half outlives it.
    let publisher = {
        let mut producer = Producer::new(storage.region(), [None, None], ready.as_ref());
        producer.announce(geometry(1), 1).unwrap();
        producer.publisher().unwrap()
    };
    let consumer_storage = Arc::clone(&storage);
    let consumer_ready = Arc::clone(&ready);
    let deadline = Instant::now() + Duration::from_secs(10);
    let consumer = thread::spawn(move || {
        let mut consumer = Consumer::new(
            consumer_storage.region(),
            consumer_ready.as_ref(),
            [None, None],
        );
        consumer.validate_and_ready(2).unwrap();
        let mut buffer = vec![0.0; geometry(1).samples_per_slot()];
        for sequence in 1..=3 {
            while consumer.next_published(0) != Some(sequence) {
                assert!(
                    Instant::now() < deadline,
                    "publisher never announced {sequence}"
                );
                thread::yield_now();
            }
            assert!(consumer.slot_mut(0, sequence, &mut buffer, |samples| samples[0] *= 2.0));
            consumer.complete(0, sequence);
        }
    });
    // The publishing thread holds nothing but the publisher and owned memory.
    let memory: Box<dyn RegionMemory> = Box::new(Arc::clone(&storage));
    let publishing = thread::spawn(move || {
        while publisher.state(memory.region()) != Some(State::Ready) {
            assert!(Instant::now() < deadline, "consumer never became ready");
            thread::yield_now();
        }
        let mut results = Vec::new();
        for sequence in 1..=3_u32 {
            assert_eq!(
                publisher.try_publish(memory.region(), 0, sequence, Some(work.as_ref()), |slot| {
                    slot.write(0, 0, sequence as f32 * 0.25);
                }),
                Publish::Published
            );
            while publisher.completed(memory.region(), 0) != sequence {
                assert!(
                    Instant::now() < deadline,
                    "consumer never completed {sequence}"
                );
                thread::yield_now();
            }
            assert!(publisher.try_read(memory.region(), 0, sequence, |slot| {
                results.push(slot.read(0, 0));
            }));
        }
        results
    });
    consumer.join().unwrap();
    assert_eq!(publishing.join().unwrap(), [0.5, 1.0, 1.5]);
}

#[test]
fn late_blocks_are_processed_without_skipping() {
    let storage = RegionBuf::new(geometry(1)).unwrap();
    let event = InProcessEvent::new();
    let mut producer = Producer::new(storage.region(), [Some(&event), None], &event);
    producer.announce(geometry(1), 1).unwrap();
    let mut consumer = Consumer::new(storage.region(), &event, [Some(&event), None]);
    consumer.validate_and_ready(2).unwrap();
    assert_eq!(producer.try_publish(0, 1, |_| {}), Publish::Published);
    assert_eq!(producer.try_publish(0, 2, |_| {}), Publish::Published);
    assert_eq!(consumer.next_published(0), Some(1));
    consumer.complete(0, 1);
    assert_eq!(consumer.next_published(0), Some(2));
    consumer.complete(0, 2);
}

#[test]
fn in_process_event_wakes_for_signal_and_peer_loss_under_race() {
    for peer_loss in [false, true] {
        for _ in 0..1_000 {
            let event = Arc::new(InProcessEvent::new());
            let waiter = Arc::clone(&event);
            // The signal always comes, so a correct event returns at once. A short
            // bound would only measure scheduling: a loaded runner can delay the
            // signalling thread past it, which is not a lost wakeup.
            let waiting = thread::spawn(move || {
                vst_audio_ring::Wait::wait(waiter.as_ref(), Duration::from_secs(5))
            });
            thread::yield_now();
            if peer_loss {
                event.peer_gone();
            } else {
                vst_audio_ring::Signal::signal(&event);
            }
            let result = waiting.join().unwrap();
            assert_eq!(
                result,
                if peer_loss {
                    Waited::PeerGone
                } else {
                    Waited::Signalled
                }
            );
        }
    }
}

#[test]
fn fault_code_and_peer_gone_are_observable() {
    let storage = RegionBuf::new(geometry(1)).unwrap();
    let event = InProcessEvent::new();
    let mut producer = Producer::new(storage.region(), [Some(&event), None], &event);
    producer.announce(geometry(1), 1).unwrap();
    let consumer = Consumer::new(storage.region(), &event, [Some(&event), None]);
    consumer.fault(Fault::EngineFailed as u32);
    assert_eq!(
        producer.wait_ready(Duration::from_secs(1)),
        Waited::PeerGone
    );
    assert_eq!(storage.region().fault_code(), Fault::EngineFailed as u32);
    event.peer_gone();
    assert_eq!(
        vst_audio_ring::Wait::wait(&event, Duration::from_secs(1)),
        Waited::PeerGone
    );
}

#[test]
fn hostile_headers_are_rejected() {
    let cases: &[(usize, u32, HeaderError)] = &[
        (0, 0, HeaderError::Magic),
        (1, 2, HeaderError::Version),
        (2, 0, HeaderError::TotalBytes),
        (
            3,
            3,
            HeaderError::Geometry(vst_audio_ring::GeometryError::LaneCount),
        ),
        (
            4,
            65,
            HeaderError::Geometry(vst_audio_ring::GeometryError::Channels),
        ),
        (
            6,
            65_537,
            HeaderError::Geometry(vst_audio_ring::GeometryError::Frames),
        ),
        (
            7,
            999,
            HeaderError::Geometry(vst_audio_ring::GeometryError::SampleRate),
        ),
        (
            14,
            1,
            HeaderError::Geometry(vst_audio_ring::GeometryError::Slots),
        ),
        (
            14,
            3,
            HeaderError::Geometry(vst_audio_ring::GeometryError::Slots),
        ),
        (16, 0, HeaderError::LaneGeometry),
        (17, 512, HeaderError::LaneGeometry),
        (18, 3, HeaderError::LaneGeometry),
    ];
    for &(index, value, expected) in cases {
        let total = geometry(1).total_bytes().unwrap();
        let prefix: Vec<_> = (0..geometry(1).mapping_split(total).unwrap() / 4)
            .map(|_| std::sync::atomic::AtomicU32::new(0))
            .collect();
        let counters: Vec<_> = (0..vst_audio_ring::COUNTER_WORDS)
            .map(|_| std::sync::atomic::AtomicU64::new(0))
            .collect();
        let event = InProcessEvent::new();
        let region = Region::new(&prefix, &counters);
        let mut producer = Producer::new(region, [Some(&event), None], &event);
        producer.announce(geometry(1), 1).unwrap();
        prefix[index].store(value, std::sync::atomic::Ordering::Relaxed);
        let mut consumer = Consumer::new(region, &event, [Some(&event), None]);
        assert_eq!(consumer.validate_and_ready(2), Err(expected));
    }
}

#[test]
fn cached_geometry_ignores_later_header_mutation() {
    let total = geometry(1).total_bytes().unwrap();
    let prefix: Vec<_> = (0..geometry(1).mapping_split(total).unwrap() / 4)
        .map(|_| std::sync::atomic::AtomicU32::new(0))
        .collect();
    let counters: Vec<_> = (0..vst_audio_ring::COUNTER_WORDS)
        .map(|_| std::sync::atomic::AtomicU64::new(0))
        .collect();
    let event = InProcessEvent::new();
    let region = Region::new(&prefix, &counters);
    let mut producer = Producer::new(region, [Some(&event), None], &event);
    producer.announce(geometry(1), 1).unwrap();
    let mut consumer = Consumer::new(region, &event, [Some(&event), None]);
    consumer.validate_and_ready(2).unwrap();

    prefix[6].store(1, std::sync::atomic::Ordering::Relaxed);
    prefix[16].store(0, std::sync::atomic::Ordering::Relaxed);
    let samples = geometry(1).samples_per_slot();
    assert_eq!(
        producer.try_publish(0, 2, |slot| assert!(slot.copy_from(&vec![7.0; samples]))),
        Publish::Published
    );
    assert_eq!(consumer.next_published(0), Some(1));
    consumer.complete(0, 1);
    assert_eq!(consumer.next_published(0), Some(2));
    let mut buffer = vec![0.0; samples];
    assert!(consumer.slot_mut(0, 2, &mut buffer, |_| {}));
    assert_eq!(buffer[0], 7.0);
}

#[test]
fn corrupt_output_event_packets_are_rejected_and_clear_the_buffer() {
    const CONTEXT_WORDS: usize = 32;
    const PACKET_WORDS: usize = 2 + vst_audio_ring::MAX_EVENTS * 72;
    const EVENT_WORDS: usize = 72;

    let g = geometry(1);
    for (count, kind, sysex_len) in [(257, 1, 0), (1, 5, 0), (1, 4, 0), (1, 4, 257)] {
        let total = g.total_bytes().unwrap();
        let prefix: Vec<_> = (0..g.mapping_split(total).unwrap() / 4)
            .map(|_| std::sync::atomic::AtomicU32::new(0))
            .collect();
        let counters: Vec<_> = (0..vst_audio_ring::COUNTER_WORDS)
            .map(|_| std::sync::atomic::AtomicU64::new(0))
            .collect();
        let event = InProcessEvent::new();
        let region = Region::new(&prefix, &counters);
        let mut producer = Producer::new(region, [None, None], &event);
        producer.announce(g, 1).unwrap();
        let mut consumer = Consumer::new(region, &event, [None, None]);
        consumer.validate_and_ready(2).unwrap();
        assert_eq!(producer.try_publish(0, 1, |_| {}), Publish::Published);
        let mut valid = EventBuffer::default();
        valid.push(Event::midi(3, 0, [0x90, 60, 100]));
        assert!(consumer.output_packet(0, 1, &valid));
        consumer.complete(0, 1);

        let prefix_bytes = g.total_bytes().unwrap() - vst_audio_ring::COUNTER_BYTES;
        let slot_words = (prefix_bytes - vst_audio_ring::HEADER_BYTES) / g.slots as usize / 4;
        let output = vst_audio_ring::HEADER_BYTES / 4
            + slot_words
            + g.samples_per_slot()
            + CONTEXT_WORDS
            + PACKET_WORDS;
        prefix[output].store(count, std::sync::atomic::Ordering::Relaxed);
        prefix[output + 2].store(kind, std::sync::atomic::Ordering::Relaxed);
        prefix[output + 2 + 3].store(sysex_len, std::sync::atomic::Ordering::Relaxed);
        debug_assert!(output + 2 + EVENT_WORDS <= prefix.len());

        let mut decoded = valid.clone();
        assert!(producer.try_read(0, 1, |slot| {
            assert!(!slot.output_events(&mut decoded));
        }));
        assert!(decoded.as_slice().is_empty());
        assert_eq!(decoded.dropped, 0);
    }
}

#[test]
fn producer_and_consumer_ignore_lanes_outside_the_geometry() {
    let storage = RegionBuf::new(geometry(1)).unwrap();
    let event = InProcessEvent::new();
    let mut producer = Producer::new(storage.region(), [Some(&event), None], &event);
    producer.announce(geometry(1), 1).unwrap();
    let mut consumer = Consumer::new(storage.region(), &event, [Some(&event), None]);
    consumer.validate_and_ready(2).unwrap();

    assert_eq!(producer.try_publish(2, 1, |_| {}), Publish::Dropped);
    consumer.complete(2, 7);
    assert_eq!(producer.completed(0), 0);
}

#[test]
fn both_lanes_are_independent() {
    let storage = RegionBuf::new(geometry(2)).unwrap();
    let event = InProcessEvent::new();
    let mut producer = Producer::new(storage.region(), [Some(&event), Some(&event)], &event);
    producer.announce(geometry(2), 1).unwrap();
    let mut consumer = Consumer::new(storage.region(), &event, [Some(&event), Some(&event)]);
    consumer.validate_and_ready(2).unwrap();

    assert_eq!(producer.try_publish(1, 1, |_| {}), Publish::Published);
    assert_eq!(consumer.next_published(0), None);
    assert_eq!(consumer.next_published(1), Some(1));
    consumer.complete(1, 1);
    assert_eq!(producer.completed(0), 0);
    assert_eq!(producer.completed(1), 1);
}

#[test]
fn publisher_drops_a_region_truncated_after_slot_samples() {
    let g = geometry(1);
    let total = g.total_bytes().unwrap();
    let prefix: Vec<_> = (0..g.mapping_split(total).unwrap() / 4)
        .map(|_| std::sync::atomic::AtomicU32::new(0))
        .collect();
    let counters: Vec<_> = (0..vst_audio_ring::COUNTER_WORDS)
        .map(|_| std::sync::atomic::AtomicU64::new(0))
        .collect();
    let event = InProcessEvent::new();
    let full = Region::new(&prefix, &counters);
    let mut producer = Producer::new(full, [None, None], &event);
    producer.announce(g, 1).unwrap();
    let mut consumer = Consumer::new(full, &event, [None, None]);
    consumer.validate_and_ready(2).unwrap();
    let publisher = producer.publisher().unwrap();

    let sample_end = vst_audio_ring::HEADER_BYTES / 4 + g.samples_per_slot();
    let truncated = Region::new(&prefix[..sample_end], &counters);
    assert_eq!(
        publisher.try_publish(truncated, 0, 2, None, |_| {}),
        Publish::Dropped
    );
}

#[test]
fn corrupt_and_truncated_regions_are_rejected_at_boundary() {
    let event = InProcessEvent::new();
    let prefix: Vec<_> = (0..128)
        .map(|_| std::sync::atomic::AtomicU32::new(0))
        .collect();
    let counters: Vec<_> = (0..vst_audio_ring::COUNTER_WORDS)
        .map(|_| std::sync::atomic::AtomicU64::new(0))
        .collect();
    let region = Region::new(&prefix, &counters);
    let mut consumer = Consumer::new(region, &event, [None, None]);
    assert_eq!(consumer.validate_and_ready(1), Err(HeaderError::Magic));

    let tiny_prefix = [std::sync::atomic::AtomicU32::new(0)];
    let tiny = Region::new(&tiny_prefix, &[]);
    let mut consumer = Consumer::new(tiny, &event, [None, None]);
    assert_eq!(consumer.validate_and_ready(1), Err(HeaderError::TooShort));
}

#[test]
fn deeper_ring_reuses_only_completed_slots_and_rejects_overwritten_reads() {
    let g = Geometry {
        slots: 4,
        ..geometry(1)
    };
    let storage = RegionBuf::new(g).unwrap();
    let event = InProcessEvent::new();
    let mut producer = Producer::new(storage.region(), [None, None], &event);
    producer.announce(g, 1).unwrap();
    let mut consumer = Consumer::new(storage.region(), &event, [None, None]);
    consumer.validate_and_ready(2).unwrap();
    for sequence in 1..=4 {
        assert_eq!(
            producer.try_publish(0, sequence, |_| {}),
            Publish::Published
        );
    }
    assert_eq!(producer.try_publish(0, 5, |_| {}), Publish::Dropped);
    consumer.complete(0, 1);
    assert_eq!(producer.try_publish(0, 5, |_| {}), Publish::Published);
    assert!(!producer.try_read(0, 1, |_| {}));
}

#[test]
fn wrapping_sequences_round_trip_for_every_geometry_and_origin() {
    for slots in [2, 4, 8, 16] {
        for origin in [u32::MAX - 40, u32::MAX - 1, u32::MAX] {
            let g = Geometry {
                slots,
                ..geometry(1)
            };
            let storage = RegionBuf::new(g).unwrap();
            let event = InProcessEvent::new();
            let mut producer = Producer::new(storage.region(), [None, None], &event);
            producer.announce_at(g, 1, origin).unwrap();
            let mut consumer = Consumer::new(storage.region(), &event, [None, None]);
            consumer.validate_and_ready(2).unwrap();
            for distance in 1..=64u32 {
                let sequence = origin.wrapping_add(distance);
                assert_eq!(
                    producer.try_publish(0, sequence, |slot| {
                        slot.write(0, 0, distance as f32);
                    }),
                    Publish::Published
                );
                assert_eq!(consumer.next_published(0), Some(sequence));
                let mut buffer = vec![0.0; g.samples_per_slot()];
                assert!(consumer.slot_mut(0, sequence, &mut buffer, |_| {}));
                assert_eq!(buffer[0], distance as f32);
                consumer.complete(0, sequence);
                let old = sequence.wrapping_sub(slots);
                assert!(!producer.try_read(0, old, |_| {}));
            }
        }
    }
}

#[test]
fn wrapping_reuse_waits_for_the_completed_slot() {
    let g = Geometry {
        slots: 4,
        ..geometry(1)
    };
    let storage = RegionBuf::new(g).unwrap();
    let event = InProcessEvent::new();
    let mut producer = Producer::new(storage.region(), [None, None], &event);
    let origin = u32::MAX - 2;
    producer.announce_at(g, 1, origin).unwrap();
    let mut consumer = Consumer::new(storage.region(), &event, [None, None]);
    consumer.validate_and_ready(2).unwrap();

    for distance in 1..=g.slots {
        let sequence = origin.wrapping_add(distance);
        assert_eq!(
            producer.try_publish(0, sequence, |_| {}),
            Publish::Published
        );
    }
    let reuse = origin.wrapping_add(g.slots + 1);
    assert_eq!(producer.try_publish(0, reuse, |_| {}), Publish::Dropped);
    consumer.complete(0, reuse.wrapping_sub(g.slots));
    assert_eq!(producer.try_publish(0, reuse, |_| {}), Publish::Published);
}

#[test]
fn pipeline_rule_covers_depth_generation_and_late_results() {
    let mut pipeline = Pipeline::new();
    assert_eq!(pipeline.depth(), 1);
    assert_eq!(pipeline.output_sequence(0, 1, 0), None);
    pipeline.published(1, 0, 1);
    assert_eq!(pipeline.output_sequence(1, 1, 1), Some(1));
    assert_eq!(pipeline.output_sequence(2, 1, 1), None);
    pipeline.published(2, 2, 1);
    assert_eq!(pipeline.output_sequence(3, 1, 1), None);
    assert_eq!(pipeline.output_sequence(4, 1, 2), None);
    pipeline.published(3, 4, 1);
    assert_eq!(pipeline.output_sequence(5, 2, 3), None);

    pipeline.set_depth(4);
    assert_eq!(pipeline.depth(), 4);
    for block in 0..4 {
        assert_eq!(pipeline.output_sequence(block, 7, u32::MAX), None);
    }
    pipeline.published(10, 0, 7);
    assert_eq!(pipeline.output_sequence(4, 7, 10), Some(10));
    pipeline.published(11, 1, 7);
    assert_eq!(pipeline.output_sequence(5, 7, 10), None);
    assert_eq!(pipeline.output_sequence(6, 7, 11), None);

    let mut maximum = Pipeline::with_depth(vst_audio_ring::MAX_SLOTS);
    maximum.published(20, 0, 9);
    assert_eq!(maximum.output_sequence(16, 9, 20), Some(20));
}
