use crate::{
    Geometry, Pipeline, Publish, Publisher, RegionMemory, Signal, SlotReader, SlotWriter, State,
};
use std::fmt;

/// Why a [`BlockLane`] cannot be built, restarted or given these buffers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockError {
    /// The lane is outside the geometry.
    Lane,
    /// The depth is outside `1..=slots - 1`.
    Depth,
    /// A planar buffer does not hold `channels * max_frames` samples.
    Buffer,
}

/// What one [`BlockLane`] call did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub struct Exchange {
    /// Block `k - depth` was completed in time and its result was read. False
    /// for the first `depth` blocks after `new` or `restart`, and for a block
    /// that was skipped, dropped, late or overwritten.
    pub delivered: bool,
    /// Block `k` was published. False when the consumer is not `Ready` or is
    /// `slots` blocks behind.
    pub published: bool,
}

/// One lane's block-synchronous client: each call submits block `k` and
/// returns block `k - depth` when the consumer completed it in time. Reads
/// before it publishes, so `depth <= slots - 1` always leaves the consumer a
/// free slot. Neither blocks, locks nor allocates; `work` is only signalled.
pub struct BlockLane<M, S> {
    memory: M,
    work: S,
    publisher: Publisher,
    lane: usize,
    pipeline: Pipeline,
    sequence: u32,
    block: u64,
    generation: u64,
}

impl<M, S> fmt::Debug for BlockLane<M, S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BlockLane")
            .field("geometry", &self.publisher.geometry())
            .field("lane", &self.lane)
            .field("depth", &self.pipeline.depth())
            .field("next_block", &self.block)
            .field("last_sequence", &self.sequence)
            .finish()
    }
}

impl<M: RegionMemory, S: Signal> BlockLane<M, S> {
    pub fn new(
        memory: M,
        work: S,
        publisher: Publisher,
        lane: usize,
        depth: u32,
    ) -> Result<Self, BlockError> {
        let geometry = publisher.geometry();
        if lane >= geometry.lanes as usize {
            return Err(BlockError::Lane);
        }
        if !(1..geometry.slots).contains(&depth) {
            return Err(BlockError::Depth);
        }
        let sequence = publisher.published(memory.region(), lane);
        Ok(Self {
            memory,
            work,
            publisher,
            lane,
            pipeline: Pipeline::with_depth(depth),
            sequence,
            block: 0,
            generation: 0,
        })
    }

    pub fn geometry(&self) -> Geometry {
        self.publisher.geometry()
    }

    pub fn lane(&self) -> usize {
        self.lane
    }

    pub fn depth(&self) -> u32 {
        self.pipeline.depth()
    }

    /// `slots - 1`.
    pub fn max_depth(&self) -> u32 {
        self.geometry().slots - 1
    }

    /// The number the next call gives its block: 0 after `new` and `restart`.
    pub fn next_block(&self) -> u64 {
        self.block
    }

    pub fn memory(&self) -> &M {
        &self.memory
    }

    pub fn state(&self) -> Option<State> {
        self.publisher.state(self.memory.region())
    }

    /// Starts again at block 0 with `depth`. No block submitted before is
    /// returned, and the next `depth` calls deliver nothing. Sequences go on.
    pub fn restart(&mut self, depth: u32) -> Result<(), BlockError> {
        if !(1..=self.max_depth()).contains(&depth) {
            return Err(BlockError::Depth);
        }
        self.pipeline.set_depth(depth);
        self.block = 0;
        self.generation = self.generation.wrapping_add(1);
        Ok(())
    }

    /// Counts block `k` as submitted without publishing or reading anything;
    /// it is never delivered.
    pub fn skip(&mut self) {
        self.block = self.block.wrapping_add(1);
    }

    /// Submits block `k`: lends block `k - depth`'s slot to `read` when the
    /// consumer completed exactly that block of this generation in time, then
    /// lets `write` fill block `k`'s slot, publishes it and signals `work`.
    pub fn process<W, R>(&mut self, write: W, read: R) -> Exchange
    where
        W: FnOnce(&mut SlotWriter<'_>),
        R: FnOnce(SlotReader<'_>),
    {
        let view = self.memory.region();
        let delivered = self
            .pipeline
            .output_sequence(
                self.block,
                self.generation,
                self.publisher.completed(view, self.lane),
            )
            .is_some_and(|sequence| self.publisher.try_read(view, self.lane, sequence, read));

        let sequence = self.sequence.wrapping_add(1);
        let published =
            if self
                .publisher
                .try_publish(view, self.lane, sequence, Some(&self.work), write)
                == Publish::Published
            {
                self.pipeline
                    .published(sequence, self.block, self.generation);
                self.sequence = sequence;
                true
            } else {
                false
            };
        self.block = self.block.wrapping_add(1);
        Exchange {
            delivered,
            published,
        }
    }

    /// Planar convenience: `input` holds `input_channels` planes of
    /// `max_frames`, `output` `output_channels` planes. Slot planes beyond
    /// the inputs are cleared. `output` is untouched unless delivered.
    pub fn process_planes(
        &mut self,
        input: &[f32],
        output: &mut [f32],
    ) -> Result<Exchange, BlockError> {
        let geometry = self.geometry();
        let frames = geometry.max_frames as usize;
        if input.len() != geometry.input_channels as usize * frames
            || output.len() != geometry.output_channels as usize * frames
        {
            return Err(BlockError::Buffer);
        }
        Ok(self.process(
            |slot| {
                for (plane, samples) in input.chunks_exact(frames).enumerate() {
                    let _ = slot.write_plane(plane, samples);
                }
                for plane in geometry.input_channels as usize..geometry.planes() {
                    let _ = slot.clear_plane(plane);
                }
            },
            |slot| {
                for (plane, samples) in output.chunks_exact_mut(frames).enumerate() {
                    let _ = slot.read_plane(plane, samples);
                }
            },
        ))
    }

    pub fn process_planes_f64(
        &mut self,
        input: &[f64],
        output: &mut [f64],
    ) -> Result<Exchange, BlockError> {
        let geometry = self.geometry();
        let frames = geometry.max_frames as usize;
        if input.len() != geometry.input_channels as usize * frames
            || output.len() != geometry.output_channels as usize * frames
        {
            return Err(BlockError::Buffer);
        }
        Ok(self.process(
            |slot| {
                for (plane, samples) in input.chunks_exact(frames).enumerate() {
                    let _ = slot.write_plane_f64(plane, samples);
                }
                for plane in geometry.input_channels as usize..geometry.planes() {
                    let _ = slot.clear_plane(plane);
                }
            },
            |slot| {
                for (plane, samples) in output.chunks_exact_mut(frames).enumerate() {
                    let _ = slot.read_plane_f64(plane, samples);
                }
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Consumer, InProcessEvent, Producer, RegionBuf, SampleFormat};

    fn setup_at(
        slots: u32,
        depth: u32,
        origin: u32,
    ) -> (BlockLane<RegionBuf, InProcessEvent>, InProcessEvent) {
        let geometry = Geometry {
            sample_format: SampleFormat::F32,
            lanes: 1,
            slots,
            input_channels: 1,
            output_channels: 2,
            max_frames: 4,
            sample_rate: 48_000,
            latency_bound_frames: 0,
        };
        let memory = RegionBuf::new(geometry).unwrap();
        let work = InProcessEvent::new();
        let ready = InProcessEvent::new();
        let mut producer = Producer::new(memory.region(), [Some(&work), None], &ready);
        producer.announce_at(geometry, 1, origin).unwrap();
        let publisher = producer.publisher().unwrap();
        Consumer::new(memory.region(), &ready, [None, None])
            .validate_and_ready(2)
            .unwrap();
        (
            BlockLane::new(memory, work.clone(), publisher, 0, depth).unwrap(),
            ready,
        )
    }

    fn setup(slots: u32, depth: u32) -> (BlockLane<RegionBuf, InProcessEvent>, InProcessEvent) {
        setup_at(slots, depth, 0)
    }

    fn consume(lane: &BlockLane<RegionBuf, InProcessEvent>) -> Option<Vec<f32>> {
        let ready = InProcessEvent::new();
        let mut consumer = Consumer::new(lane.memory.region(), &ready, [None, None]);
        consumer.geometry = Some(lane.geometry());
        consumer.layout = crate::layout(lane.geometry());
        let sequence = consumer.next_published(0)?;
        let mut samples = vec![0.0; lane.geometry().samples_per_slot()];
        let mut seen = vec![0.0; samples.len()];
        consumer.slot_mut(0, sequence, &mut samples, |values| {
            seen.copy_from_slice(values);
            for value in values {
                *value += 10.0;
            }
        });
        consumer.complete(0, sequence);
        Some(seen)
    }

    #[test]
    fn depths_deliver_exact_earlier_block() {
        for depth in [1, 7] {
            let (mut lane, _) = setup(8, depth);
            let mut output = vec![-1.0; 8];
            for block in 0..12 {
                let input = vec![block as f32; 4];
                let exchange = lane.process_planes(&input, &mut output).unwrap();
                assert_eq!(exchange.delivered, block >= depth as usize);
                if block >= depth as usize {
                    assert_eq!(output[0], (block - depth as usize) as f32 + 10.0);
                }
                consume(&lane);
            }
        }
    }

    #[test]
    fn late_restart_skip_and_dropped_publish() {
        let (mut lane, _) = setup(4, 1);
        let mut output = vec![99.0; 8];
        let _ = lane.process_planes(&[1.0; 4], &mut output).unwrap();
        assert!(
            !lane
                .process_planes(&[2.0; 4], &mut output)
                .unwrap()
                .delivered
        );
        consume(&lane);
        assert!(
            !lane
                .process_planes(&[3.0; 4], &mut output)
                .unwrap()
                .delivered
        );
        consume(&lane);
        consume(&lane);
        assert!(
            lane.process_planes(&[4.0; 4], &mut output)
                .unwrap()
                .delivered
        );
        lane.restart(3).unwrap();
        for _ in 0..3 {
            assert!(
                !lane
                    .process_planes(&[0.0; 4], &mut output)
                    .unwrap()
                    .delivered
            );
            consume(&lane);
        }
        lane.skip();
        assert_eq!(lane.next_block(), 4);
    }

    #[test]
    fn origin_wraps_at_minimum_and_maximum_depth() {
        for depth in [1, 7] {
            let (mut lane, _) = setup_at(8, depth, u32::MAX - 3);
            let mut output = vec![-1.0; 8];
            for block in 0..3 * depth + 8 {
                let input = vec![block as f32; 4];
                let exchange = lane.process_planes(&input, &mut output).unwrap();
                assert_eq!(exchange.delivered, block >= depth);
                if block >= depth {
                    assert_eq!(output[0], (block - depth) as f32 + 10.0);
                }
                consume(&lane);
            }
        }
    }

    #[test]
    fn validates_buffers_clears_planes_and_wraps() {
        let (mut lane, _) = setup_at(2, 1, u32::MAX - 1);
        let mut output = vec![0.0; 8];
        assert_eq!(
            lane.process_planes(&[], &mut output),
            Err(BlockError::Buffer)
        );
        let _ = lane.process(
            |slot| {
                slot.write_plane(1, &[9.0; 4]);
            },
            |_| {},
        );
        consume(&lane);
        let _ = lane.process_planes(&[1.0; 4], &mut output).unwrap();
        let seen = consume(&lane).unwrap();
        assert_eq!(&seen[4..], &[0.0; 4]);
        assert!(lane.process(|_| {}, |_| {}).published);
        consume(&lane);
        assert!(lane.process(|_| {}, |_| {}).published);
    }
}
