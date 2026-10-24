use super::*;

fn geometry(slots: u32) -> Geometry {
    Geometry {
        sample_format: SampleFormat::F32,
        lanes: 1,
        slots,
        input_channels: 1,
        output_channels: 1,
        max_frames: 1,
        sample_rate: 48_000,
        latency_bound_frames: 0,
    }
}

#[test]
fn slot_mapping_is_modular_around_wrap() {
    for slots in [2, 4, 8, 16] {
        let layout = layout(geometry(slots)).unwrap();
        let sequences = (0..=40).map(|distance| (u32::MAX - 20).wrapping_add(distance));
        for sequence in sequences {
            assert_eq!(
                layout.slot_offset(0, sequence),
                layout.slot_offset(0, sequence.wrapping_sub(slots))
            );
        }
        for start in [u32::MAX - 20, u32::MAX - 1, 0, 20] {
            let mut offsets = Vec::new();
            for distance in 0..slots {
                offsets.push(layout.slot_offset(0, start.wrapping_add(distance)));
            }
            offsets.sort_unstable();
            offsets.dedup();
            assert_eq!(offsets.len(), slots as usize);
        }
    }
}
