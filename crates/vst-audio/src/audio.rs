//! Variable callback adapter. Only bounded copies/atomics run in this module.
use crate::{Fallback, StartError, api::Shared};
use std::sync::{Arc, atomic::Ordering};
use vst_audio_protocol::{Capabilities, ProcessingSetup};
use vst_audio_ring::{
    Event as AudioEvent, EventBuffer, EventData, Geometry, MAX_EVENTS, Pipeline, Publish,
    Publisher, SampleFormat, Transport,
};
use vst_audio_transport_sys::{Event, SharedRegion};

#[derive(Debug)]
pub(crate) struct PreparedAudio {
    pub region: Arc<SharedRegion>,
    pub work: Arc<Event>,
    pub publisher: Publisher,
    pub geometry: Geometry,
    pub latency: u32,
    pub setup: ProcessingSetup,
    pub capabilities: Capabilities,
    pub generation: u64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioStatus {
    Processed,
    Fallback,
    InvalidBuffer,
    InvalidEvents,
    CounterExhausted,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessReport {
    pub status: AudioStatus,
    pub events_written: usize,
    /// Host-detected rejection/overflow. A plugin may also intentionally ignore input.
    pub events_dropped: u32,
    pub generation: u64,
}
#[derive(Debug)]
pub struct AudioEndpoint {
    active: PreparedAudio,
    shared: Arc<Shared>,
    next: rtrb::Consumer<PreparedAudio>,
    retired: rtrb::Producer<PreparedAudio>,
    fallback: Fallback,
    sequence: u32,
    block: u64,
    stream_position: u64,
    pipeline: Pipeline,
    fill: usize,
    output_at: usize,
    ready: bool,
    input32: Vec<f32>,
    input64: Vec<f64>,
    output32: Vec<f32>,
    output64: Vec<f64>,
    pending: Box<EventBuffer>,
    delivered: Box<EventBuffer>,
    event_cursor: usize,
    pending_context: Transport,
    next_context: Transport,
    history: Vec<f64>,
    history_len: usize,
    history_at: usize,
    _not_sync: std::marker::PhantomData<std::cell::Cell<()>>,
}
impl AudioEndpoint {
    pub(crate) fn new(
        active: PreparedAudio,
        fallback: Fallback,
        shared: Arc<Shared>,
        next: rtrb::Consumer<PreparedAudio>,
        retired: rtrb::Producer<PreparedAudio>,
    ) -> Result<Self, StartError> {
        let g = active.geometry;
        let q = g.max_frames as usize;
        let history_len = g.latency_bound_frames as usize + 2 * q + 1;
        let history_samples = if fallback == Fallback::DelayedInput {
            history_len
                .checked_mul(g.output_channels as usize)
                .ok_or_else(|| StartError("fallback size overflow".into()))?
        } else {
            0
        };
        if history_samples > 8 * 1024 * 1024 {
            return Err(StartError("fallback delay exceeds 64 MiB budget".into()));
        }
        let f32 = g.sample_format == SampleFormat::F32;
        Ok(Self {
            active,
            shared,
            next,
            retired,
            fallback,
            sequence: 1,
            block: 0,
            stream_position: 0,
            pipeline: Pipeline::default(),
            fill: 0,
            output_at: 0,
            ready: false,
            input32: vec![
                0.0;
                if f32 {
                    g.input_channels as usize * q
                } else {
                    0
                }
            ],
            input64: vec![
                0.0;
                if f32 {
                    0
                } else {
                    g.input_channels as usize * q
                }
            ],
            output32: vec![
                0.0;
                if f32 {
                    g.output_channels as usize * q
                } else {
                    0
                }
            ],
            output64: vec![
                0.0;
                if f32 {
                    0
                } else {
                    g.output_channels as usize * q
                }
            ],
            pending: Box::default(),
            delivered: Box::default(),
            event_cursor: 0,
            pending_context: Transport::default(),
            next_context: Transport::default(),
            history: vec![0.0; history_samples],
            history_len,
            history_at: 0,
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
    /// Reblocking/IPC adds two quanta, independently of plugin-reported latency.
    pub fn latency_frames(&self) -> usize {
        2 * self.active.geometry.max_frames as usize + self.active.latency as usize
    }
    pub fn process(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]]) -> AudioStatus {
        let frames = outputs
            .first()
            .map(|p| p.len())
            .or_else(|| inputs.first().map(|p| p.len()))
            .unwrap_or(self.active.geometry.max_frames as usize);
        self.process_events(frames, inputs, outputs, self.next_context, &[], &mut [])
            .status
    }
    pub fn process_f64(&mut self, inputs: &[&[f64]], outputs: &mut [&mut [f64]]) -> AudioStatus {
        let frames = outputs
            .first()
            .map(|p| p.len())
            .or_else(|| inputs.first().map(|p| p.len()))
            .unwrap_or(self.active.geometry.max_frames as usize);
        self.process_events_f64(frames, inputs, outputs, self.next_context, &[], &mut [])
            .status
    }
    pub fn process_events(
        &mut self,
        frames: usize,
        inputs: &[&[f32]],
        outputs: &mut [&mut [f32]],
        transport: Transport,
        events: &[AudioEvent],
        output_events: &mut [AudioEvent],
    ) -> ProcessReport {
        self.process_inner(frames, inputs, outputs, transport, events, output_events)
    }
    pub fn process_events_f64(
        &mut self,
        frames: usize,
        inputs: &[&[f64]],
        outputs: &mut [&mut [f64]],
        transport: Transport,
        events: &[AudioEvent],
        output_events: &mut [AudioEvent],
    ) -> ProcessReport {
        self.process_inner(frames, inputs, outputs, transport, events, output_events)
    }
    fn process_inner<T: Sample>(
        &mut self,
        frames: usize,
        inputs: &[&[T]],
        outputs: &mut [&mut [T]],
        transport: Transport,
        events: &[AudioEvent],
        output_events: &mut [AudioEvent],
    ) -> ProcessReport {
        let g = self.active.geometry;
        let q = g.max_frames as usize;
        let mut report = ProcessReport {
            status: AudioStatus::Processed,
            events_written: 0,
            events_dropped: 0,
            generation: self.active.generation,
        };
        let invalid = if T::FORMAT != g.sample_format
            || frames > 65536
            || inputs.len() != g.input_channels as usize
            || outputs.len() != g.output_channels as usize
            || inputs.iter().any(|p| p.len() != frames)
            || outputs.iter().any(|p| p.len() != frames)
        {
            Some(AudioStatus::InvalidBuffer)
        } else if !transport.valid()
            || events.len() > MAX_EVENTS
            || events.iter().any(|e| {
                !e.valid(frames as u32)
                    || (matches!(e.data, EventData::Midi(_) | EventData::SysEx { .. })
                        && e.bus >= self.active.capabilities.input_event_buses)
            })
            || events
                .windows(2)
                .any(|p| p[0].sample_offset > p[1].sample_offset)
        {
            Some(AudioStatus::InvalidEvents)
        } else {
            None
        };
        if let Some(status) = invalid {
            for plane in outputs {
                plane.fill(T::from_f64(0.0));
            }
            report.status = status;
            return report;
        }
        // Preflight packet capacities before touching audio or consuming events.
        let marker = self.fill != 0 && !equivalent(transport, self.next_context);
        let mut count = self.pending.as_slice().len() + usize::from(marker);
        if count > MAX_EVENTS {
            report.status = AudioStatus::InvalidEvents;
            for plane in outputs {
                plane.fill(T::from_f64(0.0));
            }
            return report;
        }
        let mut packet = 0;
        for e in events {
            let current = (self.fill + e.sample_offset as usize) / q;
            if current != packet {
                count = 0;
                packet = current;
            }
            count += 1;
            if count > MAX_EVENTS {
                report.status = AudioStatus::InvalidEvents;
                for plane in outputs {
                    plane.fill(T::from_f64(0.0));
                }
                return report;
            }
        }
        if count > MAX_EVENTS {
            report.status = AudioStatus::InvalidEvents;
            for plane in outputs {
                plane.fill(T::from_f64(0.0));
            }
            return report;
        }
        if marker {
            self.pending.push(AudioEvent {
                sample_offset: self.fill as u32,
                bus: 0,
                data: EventData::Transport(transport),
            });
        }
        let mut event_index = 0;
        let mut call_context = transport;
        let mut context_origin = 0;
        for frame in 0..frames {
            if self.fill == 0 {
                self.pending_context = call_context.advanced(frame - context_origin, g.sample_rate);
            }
            while event_index < events.len() && events[event_index].sample_offset as usize == frame
            {
                let mut e = events[event_index];
                e.sample_offset = self.fill as u32;
                if let EventData::Transport(value) = e.data {
                    call_context = value;
                    context_origin = frame;
                }
                self.pending.push(e);
                event_index += 1;
            }
            let running = matches!(self.shared.status.load(Ordering::Acquire), 0 | 3)
                && self.shared.generation.load(Ordering::Acquire) == self.active.generation;
            let valid_output = self.ready && running;
            if !valid_output {
                report.status = AudioStatus::Fallback;
            }
            for (channel, plane) in outputs.iter_mut().enumerate() {
                let fallback = if self.fallback == Fallback::DelayedInput {
                    let write = channel * self.history_len + self.history_at;
                    let read = channel * self.history_len
                        + (self.history_at + self.history_len - self.latency_frames())
                            % self.history_len;
                    let value = self.history[read];
                    let input = inputs.get(channel).map_or(0.0, |p| p[frame].to_f64());
                    self.history[write] = if input.is_finite() { input } else { 0.0 };
                    value
                } else {
                    0.0
                };
                let value = if valid_output {
                    let index = channel * q + self.output_at;
                    if T::FORMAT == SampleFormat::F32 {
                        f64::from(self.output32[index])
                    } else {
                        self.output64[index]
                    }
                } else {
                    fallback
                };
                plane[frame] = T::from_f64(if value.is_finite() { value } else { 0.0 });
            }
            self.history_at = (self.history_at + 1) % self.history_len;
            if self.output_at == 0 {
                report.events_dropped =
                    report.events_dropped.saturating_add(self.delivered.dropped);
            }
            while self.event_cursor < self.delivered.as_slice().len()
                && self.delivered.as_slice()[self.event_cursor].sample_offset as usize
                    <= self.output_at
            {
                let mut event = self.delivered.as_slice()[self.event_cursor];
                self.event_cursor += 1;
                if valid_output {
                    event.sample_offset = frame as u32;
                    if let Some(destination) = output_events.get_mut(report.events_written) {
                        *destination = event;
                        report.events_written += 1;
                    } else {
                        report.events_dropped = report.events_dropped.saturating_add(1);
                    }
                }
            }
            for (channel, plane) in inputs.iter().enumerate() {
                let index = channel * q + self.fill;
                if T::FORMAT == SampleFormat::F32 {
                    self.input32[index] = plane[frame].to_f64() as f32;
                } else {
                    self.input64[index] = plane[frame].to_f64();
                }
            }
            self.fill += 1;
            self.output_at += 1;
            self.stream_position = self.stream_position.saturating_add(1);
            if self.fill == q {
                if !self.finish_quantum() {
                    report.status = AudioStatus::CounterExhausted;
                }
                self.fill = 0;
                self.output_at = 0;
            }
        }
        self.next_context = call_context.advanced(frames - context_origin, g.sample_rate);
        report.generation = self.active.generation;
        report
    }
    fn finish_quantum(&mut self) -> bool {
        // Never release the last Arc or a heap buffer on this thread.
        if !self.retired.is_full()
            && !self.shared.stop.load(Ordering::Acquire)
            && let Ok(next) = self.next.pop()
        {
            let old = std::mem::replace(&mut self.active, next);
            self.retired
                .push(old)
                .expect("single producer reserved retirement slot");
            self.shared
                .applied_generation
                .store(self.active.generation, Ordering::Release);
            self.pending.clear();
            self.pipeline.clear();
            self.sequence = 1;
            self.block = 0;
            self.ready = false;
        }
        self.delivered.clear();
        self.event_cursor = 0;
        self.ready = false;
        if self.sequence == u32::MAX || self.block == u64::MAX {
            return false;
        }
        if !matches!(self.shared.status.load(Ordering::Acquire), 0 | 3)
            || self.shared.generation.load(Ordering::Acquire) != self.active.generation
        {
            self.pending.clear();
            self.block += 1;
            return true;
        }
        let view = self.active.region.region();
        let publisher = self.active.publisher;
        if let Some(sequence) = self.pipeline.output_sequence(
            self.block,
            self.active.generation,
            publisher.completed(view, 0),
        ) {
            self.ready = publisher.try_read(view, 0, sequence, |slot| {
                for channel in 0..self.active.geometry.output_channels as usize {
                    for frame in 0..self.active.geometry.max_frames as usize {
                        let index = channel * self.active.geometry.max_frames as usize + frame;
                        if self.active.geometry.sample_format == SampleFormat::F32 {
                            self.output32[index] = slot.read(channel, frame);
                        } else {
                            self.output64[index] = slot.read_f64(channel, frame);
                        }
                    }
                }
                if !slot.output_events(&mut self.delivered) {
                    self.delivered.clear();
                    self.delivered.dropped = 1;
                }
            });
            if self.ready {
                self.shared
                    .processed_generation
                    .store(self.active.generation, Ordering::Release);
            }
            self.delivered.sort_by_offset();
        }
        self.pending.sort_by_offset();
        if publisher.try_publish(view, 0, self.sequence, Some(&*self.active.work), |slot| {
            for channel in 0..slot.planes() {
                for frame in 0..slot.frames() {
                    let value = if channel < self.active.geometry.input_channels as usize {
                        let index = channel * slot.frames() + frame;
                        if self.active.geometry.sample_format == SampleFormat::F32 {
                            f64::from(self.input32[index])
                        } else {
                            self.input64[index]
                        }
                    } else {
                        0.0
                    };
                    slot.write_f64(channel, frame, value);
                }
            }
            slot.set_transport(self.pending_context);
            slot.set_events(self.pending.as_slice());
        }) == Publish::Published
        {
            self.pipeline
                .published(self.sequence, self.block, self.active.generation);
            self.sequence += 1;
        }
        self.pending.clear();
        self.block += 1;
        true
    }
}
trait Sample: Copy {
    const FORMAT: SampleFormat;
    fn to_f64(self) -> f64;
    fn from_f64(value: f64) -> Self;
}
impl Sample for f32 {
    const FORMAT: SampleFormat = SampleFormat::F32;
    fn to_f64(self) -> f64 {
        f64::from(self)
    }
    fn from_f64(value: f64) -> Self {
        value as f32
    }
}
impl Sample for f64 {
    const FORMAT: SampleFormat = SampleFormat::F64;
    fn to_f64(self) -> f64 {
        self
    }
    fn from_f64(value: f64) -> Self {
        value
    }
}
fn equivalent(a: Transport, b: Transport) -> bool {
    a.sample_position == b.sample_position
        && a.continuous_position == b.continuous_position
        && (a.ppq_position - b.ppq_position).abs() < 1e-9
        && a.tempo == b.tempo
        && a.bar_start == b.bar_start
        && a.cycle_start == b.cycle_start
        && a.cycle_end == b.cycle_end
        && a.numerator == b.numerator
        && a.denominator == b.denominator
        && a.playing == b.playing
        && a.recording == b.recording
        && a.cycling == b.cycling
}
