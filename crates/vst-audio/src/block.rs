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
