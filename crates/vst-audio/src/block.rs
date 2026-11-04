use crate::{api::Shared, audio::PreparedAudio};
use std::{
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};
use vst_audio_protocol::{Capabilities, ProcessingSetup};
use vst_audio_ring::{BlockError, Geometry, SampleFormat, SlotReader, SlotWriter};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockStatus {
    /// Block `k - depth`'s result is in the output.
    Delivered,
    /// No result for block `k - depth`; the output is untouched. The caller
    /// plays its own fallback.
    Missed,
    /// Wrong sample format or buffer length; nothing was submitted.
    InvalidBuffer,
}

impl BlockStatus {
    pub fn delivered(self) -> bool {
        self == Self::Delivered
    }
}

/// The result of one non-realtime block call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rendered {
    /// Block `k - depth` was written to the output.
    Delivered,
    /// The pipeline is still priming and the output was left untouched.
    Priming,
}

/// Why a non-realtime block call could not continue losslessly.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RenderError {
    #[error("invalid audio buffer")]
    InvalidBuffer,
    #[error("worker is not running")]
    NotRunning,
    #[error("worker recovered during render")]
    Recovered,
    #[error("render timed out")]
    TimedOut,
    #[error("completed block was lost")]
    Lost,
}

/// The realtime handle for block-synchronous callers: each call submits one
/// quantum (block k) and returns block k - depth. Missed blocks are reported,
/// never concealed. Recovery generations are adopted at the start of a call;
/// the endpoint then restarts at block 0 with its depth. Not `Sync`.
#[derive(Debug)]
pub struct BlockEndpoint {
    pub(crate) active: PreparedAudio,
    shared: Arc<Shared>,
    next: rtrb::Consumer<PreparedAudio>,
    retired: rtrb::Producer<PreparedAudio>,
    depth: u32,
    render_generation: Option<u64>,
    _not_sync: std::marker::PhantomData<std::cell::Cell<()>>,
}

impl BlockEndpoint {
    pub(crate) fn new(
        mut active: PreparedAudio,
        depth: u32,
        shared: Arc<Shared>,
        next: rtrb::Consumer<PreparedAudio>,
        retired: rtrb::Producer<PreparedAudio>,
    ) -> Result<Self, BlockError> {
        active.lane.restart(depth)?;
        Ok(Self {
            active,
            shared,
            next,
            retired,
            depth,
            render_generation: None,
            _not_sync: std::marker::PhantomData,
        })
    }

    pub fn geometry(&self) -> Geometry {
        self.active.geometry
    }

    pub fn layout(&self) -> &ProcessingSetup {
        &self.active.setup
    }

    pub fn capabilities(&self) -> Capabilities {
        self.active.capabilities
    }

    pub fn generation(&self) -> u64 {
        self.active.generation
    }

    pub fn depth(&self) -> u32 {
        self.depth
    }

    pub fn max_depth(&self) -> u32 {
        self.active.lane.max_depth()
    }

    /// Starts again at block 0 with `depth` (1..=max_depth). Realtime-safe.
    pub fn restart(&mut self, depth: u32) -> Result<(), BlockError> {
        self.active.lane.restart(depth)?;
        self.depth = depth;
        self.render_generation = None;
        Ok(())
    }

    /// The active worker's reported plugin latency.
    pub fn plugin_latency_frames(&self) -> u32 {
        self.active.latency
    }

    /// `depth * quantum + plugin latency`.
    pub fn latency_frames(&self) -> usize {
        self.depth as usize * self.active.geometry.max_frames as usize
            + self.active.latency as usize
    }

    /// Planar: `input` holds `input_channels` planes of `quantum` frames,
    /// `output` `output_channels` planes. F32 geometries only.
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) -> BlockStatus {
        self.adopt();
        let geometry = self.active.geometry;
        if geometry.sample_format != SampleFormat::F32
            || input.len() != geometry.input_channels as usize * geometry.max_frames as usize
            || output.len() != geometry.output_channels as usize * geometry.max_frames as usize
        {
            return BlockStatus::InvalidBuffer;
        }
        self.process_ready(
            |slot| {
                for (plane, samples) in input.chunks_exact(geometry.max_frames as usize).enumerate()
                {
                    let _ = slot.write_plane(plane, samples);
                }
                for plane in geometry.input_channels as usize..geometry.planes() {
                    let _ = slot.clear_plane(plane);
                }
            },
            |slot| {
                for (plane, samples) in output
                    .chunks_exact_mut(geometry.max_frames as usize)
                    .enumerate()
                {
                    let _ = slot.read_plane(plane, samples);
                }
            },
        )
    }

    /// F64 geometries only.
    pub fn process_f64(&mut self, input: &[f64], output: &mut [f64]) -> BlockStatus {
        self.adopt();
        let geometry = self.active.geometry;
        if geometry.sample_format != SampleFormat::F64
            || input.len() != geometry.input_channels as usize * geometry.max_frames as usize
            || output.len() != geometry.output_channels as usize * geometry.max_frames as usize
        {
            return BlockStatus::InvalidBuffer;
        }
        self.process_ready(
            |slot| {
                for (plane, samples) in input.chunks_exact(geometry.max_frames as usize).enumerate()
                {
                    let _ = slot.write_plane_f64(plane, samples);
                }
                for plane in geometry.input_channels as usize..geometry.planes() {
                    let _ = slot.clear_plane(plane);
                }
            },
            |slot| {
                for (plane, samples) in output
                    .chunks_exact_mut(geometry.max_frames as usize)
                    .enumerate()
                {
                    let _ = slot.read_plane_f64(plane, samples);
                }
            },
        )
    }

    /// Lossless non-realtime f32 processing. This call may sleep while waiting
    /// for the due block; never call it on an audio thread. The first `depth`
    /// calls return [`Rendered::Priming`] without changing `output`. Submit
    /// `depth` more blocks (for example, silent blocks) to obtain the final
    /// `depth` rendered blocks.
    pub fn process_wait(
        &mut self,
        input: &[f32],
        output: &mut [f32],
        timeout: Duration,
    ) -> Result<Rendered, RenderError> {
        let started = Instant::now();
        let geometry = self.active.geometry;
        if geometry.sample_format != SampleFormat::F32
            || input.len() != geometry.input_channels as usize * geometry.max_frames as usize
            || output.len() != geometry.output_channels as usize * geometry.max_frames as usize
        {
            return Err(RenderError::InvalidBuffer);
        }
        self.wait_for_due(started, timeout)?;
        let priming = self.active.lane.next_block() < u64::from(self.depth);
        let status = self.process_ready(
            |slot| {
                for (plane, samples) in input.chunks_exact(geometry.max_frames as usize).enumerate()
                {
                    let _ = slot.write_plane(plane, samples);
                }
                for plane in geometry.input_channels as usize..geometry.planes() {
                    let _ = slot.clear_plane(plane);
                }
            },
            |slot| {
                for (plane, samples) in output
                    .chunks_exact_mut(geometry.max_frames as usize)
                    .enumerate()
                {
                    let _ = slot.read_plane(plane, samples);
                }
            },
        );
        match (status, priming) {
            (BlockStatus::Delivered, _) => Ok(Rendered::Delivered),
            (BlockStatus::Missed, true) => Ok(Rendered::Priming),
            (BlockStatus::Missed, false) => Err(RenderError::Lost),
            (BlockStatus::InvalidBuffer, _) => Err(RenderError::InvalidBuffer),
        }
    }

    /// Lossless non-realtime f64 processing. This call may sleep while waiting
    /// for the due block; never call it on an audio thread. The first `depth`
    /// calls return [`Rendered::Priming`] without changing `output`. Submit
    /// `depth` more blocks (for example, silent blocks) to obtain the final
    /// `depth` rendered blocks.
    pub fn process_wait_f64(
        &mut self,
        input: &[f64],
        output: &mut [f64],
        timeout: Duration,
    ) -> Result<Rendered, RenderError> {
        let started = Instant::now();
        let geometry = self.active.geometry;
        if geometry.sample_format != SampleFormat::F64
            || input.len() != geometry.input_channels as usize * geometry.max_frames as usize
            || output.len() != geometry.output_channels as usize * geometry.max_frames as usize
        {
            return Err(RenderError::InvalidBuffer);
        }
        self.wait_for_due(started, timeout)?;
        let priming = self.active.lane.next_block() < u64::from(self.depth);
        let status = self.process_ready(
            |slot| {
                for (plane, samples) in input.chunks_exact(geometry.max_frames as usize).enumerate()
                {
                    let _ = slot.write_plane_f64(plane, samples);
                }
                for plane in geometry.input_channels as usize..geometry.planes() {
                    let _ = slot.clear_plane(plane);
                }
            },
            |slot| {
                for (plane, samples) in output
                    .chunks_exact_mut(geometry.max_frames as usize)
                    .enumerate()
                {
                    let _ = slot.read_plane_f64(plane, samples);
                }
            },
        );
        match (status, priming) {
            (BlockStatus::Delivered, _) => Ok(Rendered::Delivered),
            (BlockStatus::Missed, true) => Ok(Rendered::Priming),
            (BlockStatus::Missed, false) => Err(RenderError::Lost),
            (BlockStatus::InvalidBuffer, _) => Err(RenderError::InvalidBuffer),
        }
    }

    /// Non-realtime. Renders `frames` frames of planar `input` (`input_channels`
    /// planes of `frames` samples) followed by `tail` frames of silence, and
    /// writes `output_channels` planes of `frames + tail` samples to `output`,
    /// aligned with the input: the pipeline and plugin latency
    /// ([`Self::latency_frames`]) is removed. Each block waits up to `timeout`
    /// (see [`Self::process_wait`]).
    pub fn render(
        &mut self,
        input: &[f32],
        frames: usize,
        tail: usize,
        output: &mut [f32],
        timeout: Duration,
    ) -> Result<(), RenderError> {
        self.render_impl(input, frames, tail, output, timeout)
    }

    /// The f64 counterpart of [`Self::render`]. F64 geometries only.
    pub fn render_f64(
        &mut self,
        input: &[f64],
        frames: usize,
        tail: usize,
        output: &mut [f64],
        timeout: Duration,
    ) -> Result<(), RenderError> {
        self.render_impl(input, frames, tail, output, timeout)
    }

    fn render_impl<T: RenderSample>(
        &mut self,
        input: &[T],
        frames: usize,
        tail: usize,
        output: &mut [T],
        timeout: Duration,
    ) -> Result<(), RenderError> {
        let geometry = self.geometry();
        let quantum = geometry.max_frames as usize;
        let input_channels = geometry.input_channels as usize;
        let output_channels = geometry.output_channels as usize;
        let length = frames.checked_add(tail).ok_or(RenderError::InvalidBuffer)?;
        let input_len = input_channels
            .checked_mul(frames)
            .ok_or(RenderError::InvalidBuffer)?;
        let output_len = output_channels
            .checked_mul(length)
            .ok_or(RenderError::InvalidBuffer)?;
        let source_len = input_channels
            .checked_mul(quantum)
            .filter(|len| allocation_fits::<T>(*len))
            .ok_or(RenderError::InvalidBuffer)?;
        let target_len = output_channels
            .checked_mul(quantum)
            .filter(|len| allocation_fits::<T>(*len))
            .ok_or(RenderError::InvalidBuffer)?;
        if geometry.sample_format != T::FORMAT
            || input.len() != input_len
            || output.len() != output_len
            || !allocation_fits::<T>(input_len)
            || !allocation_fits::<T>(output_len)
        {
            return Err(RenderError::InvalidBuffer);
        }
        if length == 0 {
            return Ok(());
        }

        let mut source = vec![T::default(); source_len];
        let mut target = vec![T::default(); target_len];
        let mut position = 0usize;
        let mut latency = self.latency_frames();
        loop {
            source.fill(T::default());
            if position < frames {
                let count = quantum.min(frames - position);
                for channel in 0..input_channels {
                    let input_start = channel * frames + position;
                    let source_start = channel * quantum;
                    source[source_start..source_start + count]
                        .copy_from_slice(&input[input_start..input_start + count]);
                }
            }

            let status = T::process_wait(self, &source, &mut target, timeout)?;
            // Priming has no delivered samples. It still occupies pipeline time;
            // counting this virtual prefix avoids trimming pipeline latency twice.
            if position == 0 {
                latency = self.latency_frames();
            }
            let end = length
                .checked_add(latency)
                .ok_or(RenderError::InvalidBuffer)?;
            if status == Rendered::Delivered {
                let begin = position.max(latency);
                let stop = position.saturating_add(quantum).min(end);
                if begin < stop {
                    let count = stop - begin;
                    for channel in 0..output_channels {
                        let target_start = channel * quantum + begin - position;
                        let output_start = channel * length + begin - latency;
                        output[output_start..output_start + count]
                            .copy_from_slice(&target[target_start..target_start + count]);
                    }
                }
            }
            position = position
                .checked_add(quantum)
                .ok_or(RenderError::InvalidBuffer)?;
            if position >= end {
                break;
            }
        }
        Ok(())
    }

    /// Slot-level access for events and transport (`AudioEndpoint` uses this).
    pub fn process_slots<W, R>(&mut self, write: W, read: R) -> BlockStatus
    where
        W: FnOnce(&mut SlotWriter<'_>),
        R: FnOnce(SlotReader<'_>),
    {
        self.adopt();
        self.process_ready(write, read)
    }

    pub(crate) fn running(&self) -> bool {
        self.gate_open()
    }

    pub(crate) fn process_slots_after_adopt<W, R>(
        &mut self,
        write: W,
        read: R,
    ) -> (BlockStatus, bool)
    where
        W: FnOnce(&mut SlotWriter<'_>),
        R: FnOnce(SlotReader<'_>),
    {
        self.process_ready_with_publication(write, read)
    }

    fn wait_for_due(&mut self, started: Instant, timeout: Duration) -> Result<(), RenderError> {
        loop {
            self.check_render_generation()?;
            if self.active.lane.due_completed() != Some(false) {
                return Ok(());
            }
            if started.elapsed() >= timeout {
                return Err(RenderError::TimedOut);
            }
            std::thread::sleep(Duration::from_micros(100));
        }
    }

    fn check_render_generation(&mut self) -> Result<(), RenderError> {
        if self.adopt() {
            return Err(RenderError::Recovered);
        }
        let generation = self.active.generation;
        match self.render_generation {
            Some(expected) if expected != generation => return Err(RenderError::Recovered),
            None => self.render_generation = Some(generation),
            Some(_) => {}
        }
        if !self.gate_open() {
            let published = self.shared.generation.load(Ordering::Acquire);
            return Err(if self.render_generation != Some(published) {
                RenderError::Recovered
            } else {
                RenderError::NotRunning
            });
        }
        Ok(())
    }

    fn gate_open(&self) -> bool {
        self.shared.lifecycle().processing_allowed()
            && self.shared.generation.load(Ordering::Acquire) == self.active.generation
    }

    fn process_ready<W, R>(&mut self, write: W, read: R) -> BlockStatus
    where
        W: FnOnce(&mut SlotWriter<'_>),
        R: FnOnce(SlotReader<'_>),
    {
        self.process_ready_with_publication(write, read).0
    }

    fn process_ready_with_publication<W, R>(&mut self, write: W, read: R) -> (BlockStatus, bool)
    where
        W: FnOnce(&mut SlotWriter<'_>),
        R: FnOnce(SlotReader<'_>),
    {
        if !self.gate_open() {
            self.active.lane.skip();
            return (BlockStatus::Missed, false);
        }
        let exchange = self.active.lane.process(write, read);
        let status = if exchange.delivered {
            self.shared
                .processed_generation
                .store(self.active.generation, Ordering::Release);
            BlockStatus::Delivered
        } else {
            BlockStatus::Missed
        };
        (status, exchange.published)
    }

    pub(crate) fn adopt(&mut self) -> bool {
        if let Some(latency) = self.shared.runtime_latency(self.active.generation) {
            self.active.latency = latency;
        }

        // Never release the last Arc or a heap buffer on this thread.
        if self.retired.is_full() || self.shared.stop.load(Ordering::Acquire) {
            return false;
        }
        let Ok(next) = self.next.pop() else {
            return false;
        };
        let old = std::mem::replace(&mut self.active, next);
        self.retired
            .push(old)
            .expect("single producer reserved retirement slot");
        self.active
            .lane
            .restart(self.depth)
            .expect("replacement geometry was validated");
        if let Some(latency) = self.shared.runtime_latency(self.active.generation) {
            self.active.latency = latency;
        }
        self.shared
            .applied_generation
            .store(self.active.generation, Ordering::Release);
        true
    }
}

fn allocation_fits<T>(len: usize) -> bool {
    len <= isize::MAX as usize / std::mem::size_of::<T>()
}

trait RenderSample: Copy + Default {
    const FORMAT: SampleFormat;

    fn process_wait(
        blocks: &mut BlockEndpoint,
        input: &[Self],
        output: &mut [Self],
        timeout: Duration,
    ) -> Result<Rendered, RenderError>;
}

impl RenderSample for f32 {
    const FORMAT: SampleFormat = SampleFormat::F32;

    fn process_wait(
        blocks: &mut BlockEndpoint,
        input: &[Self],
        output: &mut [Self],
        timeout: Duration,
    ) -> Result<Rendered, RenderError> {
        blocks.process_wait(input, output, timeout)
    }
}

impl RenderSample for f64 {
    const FORMAT: SampleFormat = SampleFormat::F64;

    fn process_wait(
        blocks: &mut BlockEndpoint,
        input: &[Self],
        output: &mut [Self],
        timeout: Duration,
    ) -> Result<Rendered, RenderError> {
        blocks.process_wait_f64(input, output, timeout)
    }
}
