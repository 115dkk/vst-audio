//! Variable callback adapter. Only bounded copies/atomics run in this module.
use crate::{
    BlockEndpoint, Fallback, PluginControl, StartError, StartErrorKind, Started, api::Shared,
};
#[cfg(test)]
use crate::{BlockStatus, api::Lifecycle};
use std::sync::Arc;
use vst_audio_protocol::{Capabilities, ProcessingSetup};
use vst_audio_ring::{
    BlockLane, Event as AudioEvent, EventBuffer, EventData, Geometry, MAX_EVENTS, SampleFormat,
    Transport,
};
use vst_audio_transport_sys::{Event, SharedRegion};

#[derive(Debug)]
pub(crate) struct PreparedAudio {
    pub lane: BlockLane<Arc<SharedRegion>, Arc<Event>>,
    pub geometry: Geometry,
    pub latency: u32,
    pub setup: ProcessingSetup,
    pub capabilities: Capabilities,
    pub generation: u64,
}
impl PreparedAudio {
    /// Builds the audio-thread half of a started worker for `generation`.
    /// Returns the control half untouched.
    pub(crate) fn from_started(
        started: Started,
        generation: u64,
    ) -> Result<(Self, PluginControl), vst_audio_ring::BlockError> {
        let Started {
            control,
            region,
            work,
            publisher,
            geometry,
            latency_frames,
            setup,
            capabilities,
            ..
        } = started;
        let lane = BlockLane::new(region, work, publisher, 0, 1)?;
        Ok((
            Self {
                lane,
                geometry,
                latency: latency_frames,
                setup,
                capabilities,
                generation,
            },
            control,
        ))
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioStatus {
    Processed,
    Fallback,
    InvalidBuffer,
    InvalidEvents,
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
    blocks: BlockEndpoint,
    fallback: Fallback,
    max_callback_frames: u32,
    fill: usize,
    output_at: usize,
    ready: bool,
    input32: Vec<f32>,
    input64: Vec<f64>,
    output32: Vec<f32>,
    output64: Vec<f64>,
    pending: Box<EventBuffer>,
    pending_input_events: u32,
    delivered: Box<EventBuffer>,
    event_cursor: usize,
    pending_context: Transport,
    next_context: Transport,
    history32: Vec<f32>,
    history64: Vec<f64>,
    history_len: usize,
    history_at: usize,
    _not_sync: std::marker::PhantomData<std::cell::Cell<()>>,
}
impl AudioEndpoint {
    pub(crate) fn new(
        active: PreparedAudio,
        fallback: Fallback,
        max_callback_frames: u32,
        depth: u32,
        shared: Arc<Shared>,
        next: rtrb::Consumer<PreparedAudio>,
        retired: rtrb::Producer<PreparedAudio>,
    ) -> Result<Self, StartError> {
        let g = active.geometry;
        let q = g.max_frames as usize;
        let blocks = BlockEndpoint::new(active, depth, shared, next, retired).map_err(|error| {
            StartError::new(
                StartErrorKind::InvalidInput,
                format!("invalid block endpoint: {error:?}"),
            )
        })?;
        let history_len = g
            .latency_bound_frames
            .checked_add((depth + 1).saturating_mul(g.max_frames))
            .and_then(|frames| frames.checked_add(1))
            .ok_or_else(|| StartError::new(StartErrorKind::Start, "fallback delay overflow"))?
            as usize;
        let history_samples = if fallback == Fallback::DelayedInput {
            history_len
                .checked_mul(g.output_channels as usize)
                .ok_or_else(|| StartError::new(StartErrorKind::Start, "fallback size overflow"))?
        } else {
            0
        };
        if history_samples > 8 * 1024 * 1024 {
            return Err(StartError::new(
                StartErrorKind::Start,
                "fallback delay exceeds 64 MiB budget",
            ));
        }
        let f32 = g.sample_format == SampleFormat::F32;
        Ok(Self {
            blocks,
            fallback,
            max_callback_frames,
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
            pending_input_events: 0,
            delivered: Box::default(),
            event_cursor: 0,
            pending_context: Transport::default(),
            next_context: Transport::default(),
            history32: vec![0.0; if f32 { history_samples } else { 0 }],
            history64: vec![0.0; if f32 { 0 } else { history_samples }],
            history_len,
            history_at: 0,
            _not_sync: std::marker::PhantomData,
        })
    }
    pub fn geometry(&self) -> Geometry {
        self.blocks.geometry()
    }
    pub fn layout(&self) -> &ProcessingSetup {
        self.blocks.layout()
    }
    pub fn capabilities(&self) -> Capabilities {
        self.blocks.capabilities()
    }
    pub fn generation(&self) -> u64 {
        self.blocks.generation()
    }
    pub fn into_blocks(self) -> BlockEndpoint {
        self.blocks
    }
    /// Reblocking/IPC latency is `(depth + 1) * quantum`, independently of
    /// plugin-reported latency.
    pub fn latency_frames(&self) -> usize {
        self.blocks.latency_frames() + self.blocks.geometry().max_frames as usize
    }
    /// Processes any callback length up to the configured `max_callback_frames`.
    /// Longer callbacks are refused because later quanta could not be processed
    /// in time; latency is `(ceil(max_callback / quantum) + 1) * quantum` plus
    /// plugin latency.
    pub fn process(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]]) -> AudioStatus {
        let frames = outputs
            .first()
            .map(|p| p.len())
            .or_else(|| inputs.first().map(|p| p.len()))
            .unwrap_or(self.blocks.geometry().max_frames as usize);
        self.process_events(frames, inputs, outputs, self.next_context, &[], &mut [])
            .status
    }
    /// Processes any callback length up to the configured `max_callback_frames`.
    pub fn process_f64(&mut self, inputs: &[&[f64]], outputs: &mut [&mut [f64]]) -> AudioStatus {
        let frames = outputs
            .first()
            .map(|p| p.len())
            .or_else(|| inputs.first().map(|p| p.len()))
            .unwrap_or(self.blocks.geometry().max_frames as usize);
        self.process_events_f64(frames, inputs, outputs, self.next_context, &[], &mut [])
            .status
    }
    /// Processes events with any callback length up to `max_callback_frames`.
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
    /// Processes events with any callback length up to `max_callback_frames`.
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
        let g = self.blocks.geometry();
        let q = g.max_frames as usize;
        let mut report = ProcessReport {
            status: AudioStatus::Processed,
            events_written: 0,
            events_dropped: 0,
            generation: self.blocks.generation(),
        };
        let invalid = if T::FORMAT != g.sample_format
            || frames > self.max_callback_frames as usize
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
                        && e.bus >= self.blocks.capabilities().input_event_buses)
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
        let mut frame = 0;
        while frame < frames {
            let segment = (q - self.fill).min(frames - frame);
            if self.fill == 0 {
                self.pending_context = call_context.advanced(frame - context_origin, g.sample_rate);
            }
            let running = self.blocks.running();
            let valid_output = self.ready && running;
            if !valid_output {
                report.status = AudioStatus::Fallback;
            }
            if self.output_at == 0 {
                report.events_dropped =
                    report.events_dropped.saturating_add(self.delivered.dropped);
            }
            for current in frame..frame + segment {
                while event_index < events.len()
                    && events[event_index].sample_offset as usize == current
                {
                    let mut event = events[event_index];
                    event.sample_offset = self.fill as u32 + (current - frame) as u32;
                    if let EventData::Transport(value) = event.data {
                        call_context = value;
                        context_origin = current;
                    }
                    self.pending.push(event);
                    self.pending_input_events = self.pending_input_events.saturating_add(1);
                    event_index += 1;
                }
                let output_at = self.output_at + current - frame;
                while self.event_cursor < self.delivered.as_slice().len()
                    && self.delivered.as_slice()[self.event_cursor].sample_offset as usize
                        <= output_at
                {
                    let mut event = self.delivered.as_slice()[self.event_cursor];
                    self.event_cursor += 1;
                    if valid_output {
                        event.sample_offset = current as u32;
                        let undeclared_bus =
                            matches!(event.data, EventData::Midi(_) | EventData::SysEx { .. })
                                && event.bus >= self.blocks.capabilities().output_event_buses;
                        if undeclared_bus {
                            report.events_dropped = report.events_dropped.saturating_add(1);
                        } else if let Some(destination) =
                            output_events.get_mut(report.events_written)
                        {
                            *destination = event;
                            report.events_written += 1;
                        } else {
                            report.events_dropped = report.events_dropped.saturating_add(1);
                        }
                    }
                }
            }
            let history_read =
                (self.history_at + self.history_len - self.latency_frames()) % self.history_len;
            for (channel, plane) in outputs.iter_mut().enumerate() {
                let output = &mut plane[frame..frame + segment];
                let input = inputs
                    .get(channel)
                    .map(|plane| &plane[frame..frame + segment]);
                let processed_range =
                    channel * q + self.output_at..channel * q + self.output_at + segment;
                let history_range = channel * self.history_len..(channel + 1) * self.history_len;
                let history32 = self
                    .history32
                    .get_mut(history_range.clone())
                    .unwrap_or(&mut []);
                let history64 = self.history64.get_mut(history_range).unwrap_or(&mut []);
                T::render(
                    output,
                    input,
                    self.output32.get(processed_range.clone()).unwrap_or(&[]),
                    self.output64.get(processed_range).unwrap_or(&[]),
                    history32,
                    history64,
                    self.history_at,
                    history_read,
                    valid_output,
                    self.fallback == Fallback::DelayedInput,
                );
            }
            for (channel, plane) in inputs.iter().enumerate() {
                let range = channel * q + self.fill..channel * q + self.fill + segment;
                T::stage(
                    &plane[frame..frame + segment],
                    self.input32.get_mut(range.clone()).unwrap_or(&mut []),
                    self.input64.get_mut(range).unwrap_or(&mut []),
                );
            }
            self.history_at = (self.history_at + segment) % self.history_len;
            self.fill += segment;
            self.output_at += segment;
            frame += segment;
            if self.fill == q {
                report.events_dropped = report.events_dropped.saturating_add(self.finish_quantum());
                self.fill = 0;
                self.output_at = 0;
            }
        }
        self.next_context = call_context.advanced(frames - context_origin, g.sample_rate);
        report.generation = self.blocks.generation();
        report
    }
    fn finish_quantum(&mut self) -> u32 {
        let mut events_dropped = 0u32;
        // Never release the last Arc or a heap buffer on this thread.
        if self.blocks.adopt() {
            events_dropped = events_dropped.saturating_add(self.pending_input_events);
            self.pending.clear();
            self.pending_input_events = 0;
            self.ready = false;
        }
        self.delivered.clear();
        self.event_cursor = 0;
        self.ready = false;
        let geometry = self.blocks.geometry();
        self.pending.sort_by_offset();
        let pending_input_events = self.pending_input_events;
        let (status, published) = self.blocks.process_slots_after_adopt(
            |slot| {
                for channel in 0..geometry.input_channels as usize {
                    let range = channel * slot.frames()..(channel + 1) * slot.frames();
                    if geometry.sample_format == SampleFormat::F32 {
                        let _ = slot.write_plane(channel, &self.input32[range]);
                    } else {
                        let _ = slot.write_plane_f64(channel, &self.input64[range]);
                    }
                }
                for channel in geometry.input_channels as usize..slot.planes() {
                    let _ = slot.clear_plane(channel);
                }
                slot.set_transport(self.pending_context);
                slot.set_events(self.pending.as_slice());
            },
            |slot| {
                for channel in 0..geometry.output_channels as usize {
                    let range = channel * slot.frames()..(channel + 1) * slot.frames();
                    if geometry.sample_format == SampleFormat::F32 {
                        let _ = slot.read_plane(channel, &mut self.output32[range]);
                    } else {
                        let _ = slot.read_plane_f64(channel, &mut self.output64[range]);
                    }
                }
                if !slot.output_events(&mut self.delivered) {
                    self.delivered.clear();
                    self.delivered.dropped = 1;
                }
            },
        );
        self.ready = status.delivered();
        if self.ready {
            self.delivered.sort_by_offset();
        }
        self.pending.clear();
        self.pending_input_events = 0;
        if !published {
            events_dropped = events_dropped.saturating_add(pending_input_events);
        }
        events_dropped
    }
}
trait Sample: Copy {
    const FORMAT: SampleFormat;
    fn from_f64(value: f64) -> Self;
    fn stage(source: &[Self], destination32: &mut [f32], destination64: &mut [f64]);
    #[allow(clippy::too_many_arguments)]
    fn render(
        output: &mut [Self],
        input: Option<&[Self]>,
        processed32: &[f32],
        processed64: &[f64],
        history32: &mut [f32],
        history64: &mut [f64],
        history_at: usize,
        history_read: usize,
        valid: bool,
        delayed: bool,
    );
}
impl Sample for f32 {
    const FORMAT: SampleFormat = SampleFormat::F32;
    fn from_f64(value: f64) -> Self {
        value as f32
    }
    fn stage(source: &[Self], destination32: &mut [f32], _destination64: &mut [f64]) {
        destination32.copy_from_slice(source);
    }
    fn render(
        output: &mut [Self],
        input: Option<&[Self]>,
        processed32: &[f32],
        _processed64: &[f64],
        history32: &mut [f32],
        _history64: &mut [f64],
        history_at: usize,
        history_read: usize,
        valid: bool,
        delayed: bool,
    ) {
        if delayed {
            if let Some(input) = input {
                ring_read(history32, history_read, output);
                ring_write(history32, history_at, input);
                sanitize_written(history32, history_at, input.len());
            } else {
                output.fill(0.0);
            }
        } else {
            output.fill(0.0);
        }
        if valid {
            output.copy_from_slice(processed32);
        }
        output.iter_mut().for_each(|sample| {
            if !sample.is_finite() {
                *sample = 0.0;
            }
        });
    }
}
impl Sample for f64 {
    const FORMAT: SampleFormat = SampleFormat::F64;
    fn from_f64(value: f64) -> Self {
        value
    }
    fn stage(source: &[Self], _destination32: &mut [f32], destination64: &mut [f64]) {
        destination64.copy_from_slice(source);
    }
    fn render(
        output: &mut [Self],
        input: Option<&[Self]>,
        _processed32: &[f32],
        processed64: &[f64],
        _history32: &mut [f32],
        history64: &mut [f64],
        history_at: usize,
        history_read: usize,
        valid: bool,
        delayed: bool,
    ) {
        if delayed {
            if let Some(input) = input {
                ring_read(history64, history_read, output);
                ring_write(history64, history_at, input);
                sanitize_written(history64, history_at, input.len());
            } else {
                output.fill(0.0);
            }
        } else {
            output.fill(0.0);
        }
        if valid {
            output.copy_from_slice(processed64);
        }
        output.iter_mut().for_each(|sample| {
            if !sample.is_finite() {
                *sample = 0.0;
            }
        });
    }
}
fn ring_write<T: Copy>(ring: &mut [T], at: usize, source: &[T]) {
    let first = source.len().min(ring.len() - at);
    ring[at..at + first].copy_from_slice(&source[..first]);
    ring[..source.len() - first].copy_from_slice(&source[first..]);
}
fn ring_read<T: Copy>(ring: &[T], at: usize, destination: &mut [T]) {
    let length = destination.len();
    let first = length.min(ring.len() - at);
    destination[..first].copy_from_slice(&ring[at..at + first]);
    destination[first..].copy_from_slice(&ring[..length - first]);
}
fn sanitize_written<T: SampleValue>(ring: &mut [T], at: usize, length: usize) {
    let first = length.min(ring.len() - at);
    for sample in &mut ring[at..at + first] {
        sample.sanitize();
    }
    for sample in &mut ring[..length - first] {
        sample.sanitize();
    }
}
trait SampleValue {
    fn sanitize(&mut self);
}
impl SampleValue for f32 {
    fn sanitize(&mut self) {
        if !self.is_finite() {
            *self = 0.0;
        }
    }
}
impl SampleValue for f64 {
    fn sanitize(&mut self) {
        if !self.is_finite() {
            *self = 0.0;
        }
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

#[cfg(test)]
struct AudioTestRig {
    endpoint: AudioEndpoint,
    consumer: vst_audio_ring::Consumer<'static>,
    updates: rtrb::Producer<PreparedAudio>,
    shared: Arc<Shared>,
    sequence_origin: u32,
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn audio_test_rig(
    sample_format: SampleFormat,
    fallback: Fallback,
    quantum: u32,
    max_callback_frames: u32,
    slots: u32,
    inputs: u32,
    outputs: u32,
    plugin_latency: u32,
    latency_bound_frames: u32,
    sequence_origin: u32,
) -> AudioTestRig {
    use vst_audio_protocol::{Bus, BusConfiguration};
    use vst_audio_ring::{Consumer, Producer};

    let geometry = Geometry {
        sample_format,
        lanes: 1,
        slots,
        input_channels: inputs,
        output_channels: outputs,
        max_frames: quantum,
        sample_rate: 48_000,
        latency_bound_frames,
    };
    let region = Arc::new(SharedRegion::create(geometry.total_bytes().unwrap()).unwrap());
    let view = region.region_for(geometry).unwrap();
    let work = Arc::new(Event::create().unwrap());
    let consumer_ready = Arc::new(Event::create().unwrap());
    let publisher = {
        let mut producer = Producer::new(view, [Some(&*work), None], &*consumer_ready);
        producer.announce_at(geometry, 1, sequence_origin).unwrap();
        producer.publisher().unwrap()
    };
    let consumer_region: &'static Arc<SharedRegion> = Box::leak(Box::new(region.clone()));
    let consumer_ready: &'static Arc<Event> = Box::leak(Box::new(consumer_ready));
    let mut consumer = Consumer::new(consumer_region.region(), &**consumer_ready, [None, None]);
    consumer.validate_and_ready(2).unwrap();
    let setup = ProcessingSetup {
        sample_rate: geometry.sample_rate,
        quantum,
        sample_format,
        offline: false,
        inputs: vec![BusConfiguration {
            layout: Bus {
                channels: inputs as u16,
                arrangement: 0,
            },
            active: true,
        }],
        outputs: vec![BusConfiguration {
            layout: Bus {
                channels: outputs as u16,
                arrangement: 0,
            },
            active: true,
        }],
    };
    let capabilities = Capabilities {
        f64: true,
        input_event_buses: 1,
        output_event_buses: 1,
        ..Capabilities::default()
    };
    let lane = BlockLane::new(region.clone(), work.clone(), publisher, 0, 1).unwrap();
    let prepared = PreparedAudio {
        lane,
        geometry,
        latency: plugin_latency,
        setup,
        capabilities,
        generation: 1,
    };
    let shared = Shared::for_audio_test(plugin_latency);
    let (updates, next) = rtrb::RingBuffer::new(2);
    let (retired, _retirements) = rtrb::RingBuffer::new(2);
    let depth = max_callback_frames.div_ceil(quantum);
    let endpoint = AudioEndpoint::new(
        prepared,
        fallback,
        max_callback_frames,
        depth,
        shared.clone(),
        next,
        retired,
    )
    .unwrap();
    AudioTestRig {
        endpoint,
        consumer,
        updates,
        shared,
        sequence_origin,
    }
}

#[cfg(test)]
mod golden {
    use super::*;
    use std::{
        hint::black_box,
        sync::atomic::Ordering,
        time::{Duration, Instant},
    };
    use vst_audio_ring::{Consumer, EventBuffer};

    const EXPECTED: &[(&str, u64)] = &[
        ("F32-Silence-q128-m128-2to6-p0-b0", 0x61f560d47c69e994),
        ("F32-Silence-q128-m128-2to6-p0-b64", 0x61f560d47c69e994),
        ("F32-Silence-q128-m128-2to6-p64-b64", 0x8458c1966a1ab7d4),
        ("F32-Silence-q128-m128-2to6-p64-b128", 0x8458c1966a1ab7d4),
        ("F32-Silence-q128-m128-6to2-p0-b0", 0x334a67a7df3a231e),
        ("F32-Silence-q128-m128-6to2-p0-b64", 0x334a67a7df3a231e),
        ("F32-Silence-q128-m128-6to2-p64-b64", 0xe07ee88de0c324de),
        ("F32-Silence-q128-m128-6to2-p64-b128", 0xe07ee88de0c324de),
        ("F32-Silence-q128-m480-2to6-p0-b0", 0xac9d486a3ddf787a),
        ("F32-Silence-q128-m480-2to6-p0-b64", 0xac9d486a3ddf787a),
        ("F32-Silence-q128-m480-2to6-p64-b64", 0xf8a4b74e037bcaba),
        ("F32-Silence-q128-m480-2to6-p64-b128", 0xf8a4b74e037bcaba),
        ("F32-Silence-q128-m480-6to2-p0-b0", 0x387ed2a5c7c41aa2),
        ("F32-Silence-q128-m480-6to2-p0-b64", 0x387ed2a5c7c41aa2),
        ("F32-Silence-q128-m480-6to2-p64-b64", 0xc05e6e3abb489962),
        ("F32-Silence-q128-m480-6to2-p64-b128", 0xc05e6e3abb489962),
        ("F32-Silence-q480-m480-2to6-p0-b0", 0xd94aff9b765784eb),
        ("F32-Silence-q480-m480-2to6-p0-b64", 0xd94aff9b765784eb),
        ("F32-Silence-q480-m480-2to6-p64-b64", 0x2cef0d9d41b61fca),
        ("F32-Silence-q480-m480-2to6-p64-b128", 0x2cef0d9d41b61fca),
        ("F32-Silence-q480-m480-6to2-p0-b0", 0x8d5da8eb140bf813),
        ("F32-Silence-q480-m480-6to2-p0-b64", 0x8d5da8eb140bf813),
        ("F32-Silence-q480-m480-6to2-p64-b64", 0x645639c317f64fc4),
        ("F32-Silence-q480-m480-6to2-p64-b128", 0x645639c317f64fc4),
        ("F32-DelayedInput-q128-m128-2to6-p0-b0", 0xcbe1229d2dff53c5),
        ("F32-DelayedInput-q128-m128-2to6-p0-b64", 0xcbe1229d2dff53c5),
        (
            "F32-DelayedInput-q128-m128-2to6-p64-b64",
            0x72c529fe43f342b0,
        ),
        (
            "F32-DelayedInput-q128-m128-2to6-p64-b128",
            0x72c529fe43f342b0,
        ),
        ("F32-DelayedInput-q128-m128-6to2-p0-b0", 0xac21e53979ae0000),
        ("F32-DelayedInput-q128-m128-6to2-p0-b64", 0xac21e53979ae0000),
        (
            "F32-DelayedInput-q128-m128-6to2-p64-b64",
            0xf5778e90da11edca,
        ),
        (
            "F32-DelayedInput-q128-m128-6to2-p64-b128",
            0xf5778e90da11edca,
        ),
        ("F32-DelayedInput-q128-m480-2to6-p0-b0", 0x1758b81a8cc426d0),
        ("F32-DelayedInput-q128-m480-2to6-p0-b64", 0x1758b81a8cc426d0),
        (
            "F32-DelayedInput-q128-m480-2to6-p64-b64",
            0xeaaf430787cf0fab,
        ),
        (
            "F32-DelayedInput-q128-m480-2to6-p64-b128",
            0xeaaf430787cf0fab,
        ),
        ("F32-DelayedInput-q128-m480-6to2-p0-b0", 0x769d51b41ef39db4),
        ("F32-DelayedInput-q128-m480-6to2-p0-b64", 0x769d51b41ef39db4),
        (
            "F32-DelayedInput-q128-m480-6to2-p64-b64",
            0x80e52995da43199d,
        ),
        (
            "F32-DelayedInput-q128-m480-6to2-p64-b128",
            0x80e52995da43199d,
        ),
        ("F32-DelayedInput-q480-m480-2to6-p0-b0", 0x1a7a4569ecb8bb83),
        ("F32-DelayedInput-q480-m480-2to6-p0-b64", 0x1a7a4569ecb8bb83),
        (
            "F32-DelayedInput-q480-m480-2to6-p64-b64",
            0xfb3664123c0b7d0b,
        ),
        (
            "F32-DelayedInput-q480-m480-2to6-p64-b128",
            0xfb3664123c0b7d0b,
        ),
        ("F32-DelayedInput-q480-m480-6to2-p0-b0", 0xf329e8f35af46169),
        ("F32-DelayedInput-q480-m480-6to2-p0-b64", 0xf329e8f35af46169),
        (
            "F32-DelayedInput-q480-m480-6to2-p64-b64",
            0x6121dfe535b13b5d,
        ),
        (
            "F32-DelayedInput-q480-m480-6to2-p64-b128",
            0x6121dfe535b13b5d,
        ),
        ("F64-Silence-q128-m128-2to6-p0-b0", 0x4adff2a1a15b42e3),
        ("F64-Silence-q128-m128-2to6-p0-b64", 0x4adff2a1a15b42e3),
        ("F64-Silence-q128-m128-2to6-p64-b64", 0xb95a6808b8f0f323),
        ("F64-Silence-q128-m128-2to6-p64-b128", 0xb95a6808b8f0f323),
        ("F64-Silence-q128-m128-6to2-p0-b0", 0x9c3c0ff8619c2830),
        ("F64-Silence-q128-m128-6to2-p0-b64", 0x9c3c0ff8619c2830),
        ("F64-Silence-q128-m128-6to2-p64-b64", 0xb6e208c7d000f670),
        ("F64-Silence-q128-m128-6to2-p64-b128", 0xb6e208c7d000f670),
        ("F64-Silence-q128-m480-2to6-p0-b0", 0x133498a6c02fc34f),
        ("F64-Silence-q128-m480-2to6-p0-b64", 0x133498a6c02fc34f),
        ("F64-Silence-q128-m480-2to6-p64-b64", 0x2cf24d14fb7a458f),
        ("F64-Silence-q128-m480-2to6-p64-b128", 0x2cf24d14fb7a458f),
        ("F64-Silence-q128-m480-6to2-p0-b0", 0x0bbc03543b4d1703),
        ("F64-Silence-q128-m480-6to2-p0-b64", 0x0bbc03543b4d1703),
        ("F64-Silence-q128-m480-6to2-p64-b64", 0x6633f03e49b34343),
        ("F64-Silence-q128-m480-6to2-p64-b128", 0x6633f03e49b34343),
        ("F64-Silence-q480-m480-2to6-p0-b0", 0xffbc312aed1afbe2),
        ("F64-Silence-q480-m480-2to6-p0-b64", 0xffbc312aed1afbe2),
        ("F64-Silence-q480-m480-2to6-p64-b64", 0xae5849f8a949491b),
        ("F64-Silence-q480-m480-2to6-p64-b128", 0xae5849f8a949491b),
        ("F64-Silence-q480-m480-6to2-p0-b0", 0x21ce35cbf2bf1f89),
        ("F64-Silence-q480-m480-6to2-p0-b64", 0x21ce35cbf2bf1f89),
        ("F64-Silence-q480-m480-6to2-p64-b64", 0xd5054bb2f6b4234a),
        ("F64-Silence-q480-m480-6to2-p64-b128", 0xd5054bb2f6b4234a),
        ("F64-DelayedInput-q128-m128-2to6-p0-b0", 0x537f29287b042696),
        ("F64-DelayedInput-q128-m128-2to6-p0-b64", 0x537f29287b042696),
        (
            "F64-DelayedInput-q128-m128-2to6-p64-b64",
            0xd58a4e687e8508ce,
        ),
        (
            "F64-DelayedInput-q128-m128-2to6-p64-b128",
            0xd58a4e687e8508ce,
        ),
        ("F64-DelayedInput-q128-m128-6to2-p0-b0", 0xced55188a64270ce),
        ("F64-DelayedInput-q128-m128-6to2-p0-b64", 0xced55188a64270ce),
        (
            "F64-DelayedInput-q128-m128-6to2-p64-b64",
            0x675a5665d4fbad69,
        ),
        (
            "F64-DelayedInput-q128-m128-6to2-p64-b128",
            0x675a5665d4fbad69,
        ),
        ("F64-DelayedInput-q128-m480-2to6-p0-b0", 0xf46626865cd5aa38),
        ("F64-DelayedInput-q128-m480-2to6-p0-b64", 0xf46626865cd5aa38),
        (
            "F64-DelayedInput-q128-m480-2to6-p64-b64",
            0x6e2d2de29876f84d,
        ),
        (
            "F64-DelayedInput-q128-m480-2to6-p64-b128",
            0x6e2d2de29876f84d,
        ),
        ("F64-DelayedInput-q128-m480-6to2-p0-b0", 0x742cc39d726030ec),
        ("F64-DelayedInput-q128-m480-6to2-p0-b64", 0x742cc39d726030ec),
        (
            "F64-DelayedInput-q128-m480-6to2-p64-b64",
            0x4f40c734200432a2,
        ),
        (
            "F64-DelayedInput-q128-m480-6to2-p64-b128",
            0x4f40c734200432a2,
        ),
        ("F64-DelayedInput-q480-m480-2to6-p0-b0", 0x3ddccfdf4ce9fb46),
        ("F64-DelayedInput-q480-m480-2to6-p0-b64", 0x3ddccfdf4ce9fb46),
        (
            "F64-DelayedInput-q480-m480-2to6-p64-b64",
            0xb36283e7249dfdef,
        ),
        (
            "F64-DelayedInput-q480-m480-2to6-p64-b128",
            0xb36283e7249dfdef,
        ),
        ("F64-DelayedInput-q480-m480-6to2-p0-b0", 0x39c0b741e3e42754),
        ("F64-DelayedInput-q480-m480-6to2-p0-b64", 0x39c0b741e3e42754),
        (
            "F64-DelayedInput-q480-m480-6to2-p64-b64",
            0x5c6de5d492180d51,
        ),
        (
            "F64-DelayedInput-q480-m480-6to2-p64-b128",
            0x5c6de5d492180d51,
        ),
    ];

    struct Hash(u64);

    impl Hash {
        fn new() -> Self {
            Self(0xcbf29ce484222325)
        }

        fn bytes(&mut self, bytes: &[u8]) {
            for byte in bytes {
                self.0 ^= u64::from(*byte);
                self.0 = self.0.wrapping_mul(0x100000001b3);
            }
        }

        fn u32(&mut self, value: u32) {
            self.bytes(&value.to_le_bytes());
        }

        fn u64(&mut self, value: u64) {
            self.bytes(&value.to_le_bytes());
        }
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut value = self.0;
            value ^= value << 13;
            value ^= value >> 7;
            value ^= value << 17;
            self.0 = value;
            value
        }
    }

    trait GoldenSample: Copy + Default {
        const FORMAT: SampleFormat;
        fn from_unit(value: f64) -> Self;
        fn hash(self, hash: &mut Hash);
        fn process(
            endpoint: &mut AudioEndpoint,
            frames: usize,
            inputs: &[&[Self]],
            outputs: &mut [&mut [Self]],
            transport: Transport,
            events: &[AudioEvent],
            output_events: &mut [AudioEvent],
        ) -> ProcessReport;
    }

    impl GoldenSample for f32 {
        const FORMAT: SampleFormat = SampleFormat::F32;

        fn from_unit(value: f64) -> Self {
            value as f32
        }

        fn hash(self, hash: &mut Hash) {
            hash.u32(self.to_bits());
        }

        fn process(
            endpoint: &mut AudioEndpoint,
            frames: usize,
            inputs: &[&[Self]],
            outputs: &mut [&mut [Self]],
            transport: Transport,
            events: &[AudioEvent],
            output_events: &mut [AudioEvent],
        ) -> ProcessReport {
            endpoint.process_events(frames, inputs, outputs, transport, events, output_events)
        }
    }

    impl GoldenSample for f64 {
        const FORMAT: SampleFormat = SampleFormat::F64;

        fn from_unit(value: f64) -> Self {
            value
        }

        fn hash(self, hash: &mut Hash) {
            hash.u64(self.to_bits());
        }

        fn process(
            endpoint: &mut AudioEndpoint,
            frames: usize,
            inputs: &[&[Self]],
            outputs: &mut [&mut [Self]],
            transport: Transport,
            events: &[AudioEvent],
            output_events: &mut [AudioEvent],
        ) -> ProcessReport {
            endpoint.process_events_f64(frames, inputs, outputs, transport, events, output_events)
        }
    }

    fn hash_event(hash: &mut Hash, event: AudioEvent) {
        hash.u32(event.sample_offset);
        hash.u32(u32::from(event.bus));
        match event.data {
            EventData::Midi(bytes) => {
                hash.u32(1);
                hash.bytes(&bytes);
            }
            EventData::SysEx { len, bytes } => {
                hash.u32(2);
                hash.u32(u32::from(len));
                hash.bytes(&bytes);
            }
            EventData::Parameter { id, value } => {
                hash.u32(3);
                hash.u32(id);
                hash.u64(value.to_bits());
            }
            EventData::Transport(value) => {
                hash.u32(4);
                hash.u64(value.sample_position as u64);
                hash.u64(value.continuous_position as u64);
                hash.u64(value.ppq_position.to_bits());
                hash.u64(value.tempo.to_bits());
                hash.u64(value.bar_start.to_bits());
                hash.u64(value.cycle_start.to_bits());
                hash.u64(value.cycle_end.to_bits());
                hash.u32(u32::from(value.numerator));
                hash.u32(u32::from(value.denominator));
                hash.bytes(&[
                    u8::from(value.playing),
                    u8::from(value.recording),
                    u8::from(value.cycling),
                ]);
            }
        }
    }

    fn hash_report(hash: &mut Hash, report: ProcessReport) {
        let status = match report.status {
            AudioStatus::Processed => 0,
            AudioStatus::Fallback => 1,
            AudioStatus::InvalidBuffer => 2,
            AudioStatus::InvalidEvents => 3,
        };
        hash.u32(status);
        hash.u64(report.events_written as u64);
        hash.u32(report.events_dropped);
        hash.u64(report.generation);
    }

    fn step_consumer(rig: &AudioTestRig, maximum: usize) -> usize {
        let geometry = rig.endpoint.geometry();
        let consumer = &rig.consumer;
        let mut processed = 0;
        while processed < maximum {
            let Some(sequence) = consumer.next_published(0) else {
                break;
            };
            let logical_sequence = sequence.wrapping_sub(rig.sequence_origin);
            let mut input_events = EventBuffer::default();
            let mut transport = Transport::default();
            assert!(consumer.input_packet(0, sequence, &mut transport, &mut input_events));
            let mut output_events = EventBuffer::default();
            for mut event in input_events.as_slice().iter().copied() {
                event.sample_offset = (event.sample_offset + logical_sequence % 3)
                    .min(geometry.max_frames.saturating_sub(1));
                if let EventData::Midi(mut bytes) = event.data {
                    bytes[2] = bytes[2].wrapping_add(logical_sequence as u8) & 0x7f;
                    event.data = EventData::Midi(bytes);
                }
                output_events.push(event);
            }
            output_events.sort_by_offset();
            if geometry.sample_format == SampleFormat::F32 {
                let mut buffer = vec![0.0f32; geometry.samples_per_slot()];
                let mut original = buffer.clone();
                assert!(consumer.slot_mut(0, sequence, &mut buffer, |samples| {
                    original.copy_from_slice(samples);
                    for channel in 0..geometry.output_channels as usize {
                        for frame in 0..geometry.max_frames as usize {
                            let input = original[(channel % geometry.input_channels as usize)
                                * geometry.max_frames as usize
                                + frame];
                            samples[channel * geometry.max_frames as usize + frame] = input
                                * (0.5 + 0.125 * channel as f32)
                                + logical_sequence as f32 * 1.0e-4;
                        }
                    }
                    let frame = logical_sequence as usize % geometry.max_frames as usize;
                    samples[(geometry.output_channels as usize - 1)
                        * geometry.max_frames as usize
                        + frame] = if logical_sequence.is_multiple_of(2) {
                        f32::NAN
                    } else {
                        f32::INFINITY
                    };
                }));
            } else {
                let mut buffer = vec![0.0f64; geometry.samples_per_slot()];
                let mut original = buffer.clone();
                assert!(consumer.slot_mut_f64(0, sequence, &mut buffer, |samples| {
                    original.copy_from_slice(samples);
                    for channel in 0..geometry.output_channels as usize {
                        for frame in 0..geometry.max_frames as usize {
                            let input = original[(channel % geometry.input_channels as usize)
                                * geometry.max_frames as usize
                                + frame];
                            samples[channel * geometry.max_frames as usize + frame] = input
                                * (0.5 + 0.125 * channel as f64)
                                + logical_sequence as f64 * 1.0e-4;
                        }
                    }
                    let frame = logical_sequence as usize % geometry.max_frames as usize;
                    samples[(geometry.output_channels as usize - 1)
                        * geometry.max_frames as usize
                        + frame] = if logical_sequence.is_multiple_of(2) {
                        f64::NAN
                    } else {
                        f64::INFINITY
                    };
                }));
            }
            assert!(consumer.output_packet(0, sequence, &output_events));
            consumer.complete(0, sequence);
            processed += 1;
        }
        processed
    }

    fn valid_events(rng: &mut Rng, frames: usize, callback: usize) -> Vec<AudioEvent> {
        let count = (rng.next() % 5) as usize;
        let mut events = Vec::with_capacity(count);
        for index in 0..count {
            let sample_offset = (rng.next() as usize % frames) as u32;
            let data = if (callback + index).is_multiple_of(3) {
                let mut transport = Transport::default();
                transport.sample_position = callback as i64 * 100 + i64::from(sample_offset);
                transport.continuous_position = transport.sample_position;
                transport.tempo = 90.0 + (rng.next() % 100) as f64 * 0.25;
                EventData::Transport(transport)
            } else {
                EventData::Midi([
                    0x90 | (index as u8 & 0x0f),
                    (rng.next() & 0x7f) as u8,
                    (rng.next() & 0x7f) as u8,
                ])
            };
            events.push(AudioEvent {
                sample_offset,
                bus: 0,
                data,
            });
        }
        events.sort_by_key(|event| event.sample_offset);
        events
    }

    #[derive(Clone, Copy)]
    struct Scenario {
        quantum: u32,
        maximum: u32,
        inputs: u32,
        outputs: u32,
        fallback: Fallback,
        plugin_latency: u32,
        latency_bound: u32,
    }

    fn scenario_name<T: GoldenSample>(scenario: Scenario) -> String {
        format!(
            "{:?}-{:?}-q{}-m{}-{}to{}-p{}-b{}",
            T::FORMAT,
            scenario.fallback,
            scenario.quantum,
            scenario.maximum,
            scenario.inputs,
            scenario.outputs,
            scenario.plugin_latency,
            scenario.latency_bound
        )
    }

    fn run_scenario<T: GoldenSample>(scenario: Scenario, sequence_origin: u32) -> (String, u64) {
        let depth = scenario.maximum.div_ceil(scenario.quantum);
        let mut rig = audio_test_rig(
            T::FORMAT,
            scenario.fallback,
            scenario.quantum,
            scenario.maximum,
            (depth + 1).next_power_of_two(),
            scenario.inputs,
            scenario.outputs,
            scenario.plugin_latency,
            scenario.latency_bound,
            sequence_origin,
        );
        let name = scenario_name::<T>(scenario);
        let mut hash = Hash::new();
        hash.u64(rig.endpoint.latency_frames() as u64);
        let mut rng = Rng(0x4d59_5df4_d0f3_3173 ^ u64::from(scenario.quantum));
        for callback in 0..400 {
            match callback % 13 {
                0 | 1 => {}
                2 | 3 | 7 => {
                    step_consumer(&rig, 1);
                }
                _ => {
                    step_consumer(&rig, usize::MAX);
                }
            }
            rig.shared.set_lifecycle(
                if (96..101).contains(&callback) || (303..307).contains(&callback) {
                    Lifecycle::Failed
                } else {
                    Lifecycle::Running
                },
            );
            let too_long = callback == 149;
            let frames = if too_long {
                scenario.maximum as usize + 1
            } else {
                rng.next() as usize % scenario.maximum as usize + 1
            };
            let mut inputs = (0..scenario.inputs)
                .map(|channel| {
                    (0..frames)
                        .map(|frame| {
                            let raw = ((rng.next() >> 16) as i32 as f64) / i32::MAX as f64;
                            T::from_unit(raw * 0.25 + channel as f64 * 0.01 + frame as f64 * 1e-6)
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let mut outputs = (0..scenario.outputs)
                .map(|_| vec![T::from_unit(9.0); frames])
                .collect::<Vec<_>>();
            let mut events = valid_events(&mut rng, frames, callback);
            if callback == 251 {
                events.clear();
                events.push(AudioEvent::midi(0, 1, [0x90, 60, 100]));
            }
            let mut transport = Transport::default();
            transport.sample_position = callback as i64 * 1000;
            transport.continuous_position = transport.sample_position;
            if callback.is_multiple_of(7) {
                transport.tempo = 100.0 + (callback % 17) as f64;
            }
            let mut input_refs = inputs.iter().map(Vec::as_slice).collect::<Vec<_>>();
            if callback == 47 {
                input_refs.pop();
            }
            let mut output_refs = outputs
                .iter_mut()
                .map(Vec::as_mut_slice)
                .collect::<Vec<_>>();
            let mut output_events = vec![AudioEvent::default(); 9];
            let report = T::process(
                &mut rig.endpoint,
                frames,
                &input_refs,
                &mut output_refs,
                transport,
                &events,
                &mut output_events,
            );
            hash_report(&mut hash, report);
            for plane in outputs {
                for sample in plane {
                    sample.hash(&mut hash);
                }
            }
            for event in output_events.into_iter().take(report.events_written) {
                hash_event(&mut hash, event);
            }
            black_box(&mut inputs);
        }
        (name, hash.0)
    }

    fn all_scenarios<T: GoldenSample>(sequence_origin: u32) -> Vec<(String, u64)> {
        let mut results = Vec::new();
        for fallback in [Fallback::Silence, Fallback::DelayedInput] {
            for (quantum, maximum) in [(128, 128), (128, 480), (480, 480)] {
                for (inputs, outputs) in [(2, 6), (6, 2)] {
                    for plugin_latency in [0, 64] {
                        for latency_bound in [plugin_latency, plugin_latency + 64] {
                            results.push(run_scenario::<T>(
                                Scenario {
                                    quantum,
                                    maximum,
                                    inputs,
                                    outputs,
                                    fallback,
                                    plugin_latency,
                                    latency_bound,
                                },
                                sequence_origin,
                            ));
                        }
                    }
                }
            }
        }
        results
    }

    #[test]
    fn golden_outputs_are_unchanged() {
        let mut baseline = all_scenarios::<f32>(0);
        baseline.extend(all_scenarios::<f64>(0));
        for (name, hash) in &baseline {
            println!("(\"{name}\", 0x{hash:016x}),");
        }
        if !EXPECTED.is_empty() {
            assert_eq!(baseline.len(), EXPECTED.len());
            for ((name, hash), (expected_name, expected_hash)) in baseline.iter().zip(EXPECTED) {
                assert_eq!(name, expected_name);
                assert_eq!(hash, expected_hash, "golden mismatch for {name}");
            }
        }

        let mut wrapped = all_scenarios::<f32>(u32::MAX - 3);
        wrapped.extend(all_scenarios::<f64>(u32::MAX - 3));
        assert_eq!(wrapped, baseline, "sequence origin changed golden output");
    }

    fn step_block_consumer(
        consumer: &Consumer<'_>,
        depth: u32,
        block: u32,
        expected_sequence: u32,
    ) {
        let sequence = consumer.next_published(0).unwrap_or_else(|| {
            panic!(
                "no publication for depth {depth}, block {block}, expected sequence {expected_sequence}"
            )
        });
        assert_eq!(sequence, expected_sequence);
        let mut samples = vec![0.0f32; 2 * 4];
        assert!(consumer.slot_mut(0, sequence, &mut samples, |values| {
            for frame in 0..4 {
                values[4 + frame] = values[frame] + 1000.0;
            }
        }));
        consumer.complete(0, sequence);
    }

    #[test]
    fn block_endpoint_returns_block_k_minus_depth_and_restarts() {
        for depth in [1, 7] {
            for origin in [0, u32::MAX - 3] {
                let rig = audio_test_rig(
                    SampleFormat::F32,
                    Fallback::Silence,
                    4,
                    4,
                    8,
                    1,
                    2,
                    0,
                    0,
                    origin,
                );
                let AudioTestRig {
                    endpoint,
                    consumer,
                    shared,
                    ..
                } = rig;
                shared.set_lifecycle(Lifecycle::Running);
                shared.generation.store(1, Ordering::Release);
                let mut endpoint = endpoint.into_blocks();
                endpoint.restart(depth).unwrap();
                let mut output = vec![99.0f32; 8];
                let mut expected_sequence = origin.wrapping_add(1);
                for block in 0..depth + 4 {
                    let input = vec![block as f32; 4];
                    let status = endpoint.process(&input, &mut output);
                    assert_eq!(endpoint.active.lane.next_block(), u64::from(block + 1));
                    if block < depth {
                        assert_eq!(status, BlockStatus::Missed);
                        assert_eq!(output, vec![99.0; 8]);
                    } else {
                        assert_eq!(status, BlockStatus::Delivered);
                        assert_eq!(&output[..4], &vec![(block - depth) as f32; 4]);
                        assert_eq!(&output[4..], &vec![(block - depth) as f32 + 1000.0; 4]);
                    }
                    step_block_consumer(&consumer, depth, block, expected_sequence);
                    expected_sequence = expected_sequence.wrapping_add(1);
                }
                endpoint.restart(depth).unwrap();
                output.fill(77.0);
                for block in 0..depth {
                    assert_eq!(
                        endpoint.process(&[block as f32; 4], &mut output),
                        BlockStatus::Missed
                    );
                    assert_eq!(output, vec![77.0; 8]);
                    step_block_consumer(&consumer, depth, block, expected_sequence);
                    expected_sequence = expected_sequence.wrapping_add(1);
                }
                assert_eq!(
                    endpoint.process(&[0.0; 3], &mut output),
                    BlockStatus::InvalidBuffer
                );
                assert_eq!(
                    endpoint.process(&[0.0; 4], &mut [0.0; 7]),
                    BlockStatus::InvalidBuffer
                );
                assert_eq!(
                    endpoint.process_f64(&[0.0; 4], &mut [0.0; 8]),
                    BlockStatus::InvalidBuffer
                );
            }
        }
    }

    #[test]
    fn delayed_input_reads_history_before_overwriting_it() {
        let quantum = 8;
        let maximum = 13;
        let latency = 5;
        let mut rig = audio_test_rig(
            SampleFormat::F32,
            Fallback::DelayedInput,
            quantum,
            maximum,
            (maximum.div_ceil(quantum) + 1).next_power_of_two(),
            1,
            1,
            latency,
            latency,
            0,
        );
        rig.shared.set_lifecycle(Lifecycle::Failed);
        let delay = rig.endpoint.latency_frames();
        let mut source = Vec::new();
        let mut actual = Vec::new();
        for frames in [7, 11, 13, 5, 9, 12, 6, 13, 8] {
            let input = (0..frames)
                .map(|_| {
                    let value = source.len() as f32 + 1.0;
                    source.push(value);
                    value
                })
                .collect::<Vec<_>>();
            let mut output = vec![99.0; frames];
            assert_eq!(
                rig.endpoint.process(&[&input], &mut [&mut output]),
                AudioStatus::Fallback
            );
            actual.extend(output);
        }
        let expected = (0..source.len())
            .map(|frame| {
                frame
                    .checked_sub(delay)
                    .map_or(0.0, |source_frame| source[source_frame])
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn reports_input_events_dropped_while_processing_gate_is_closed() {
        let mut rig = audio_test_rig(SampleFormat::F32, Fallback::Silence, 4, 4, 2, 1, 1, 0, 0, 0);
        rig.shared.set_lifecycle(Lifecycle::Failed);
        let input = [0.0; 4];
        let mut output = [0.0; 4];
        let event = AudioEvent::midi(2, 0, [0x90, 60, 100]);

        let report = rig.endpoint.process_events(
            4,
            &[&input],
            &mut [&mut output],
            Transport::default(),
            &[event],
            &mut [],
        );

        assert_eq!(report.status, AudioStatus::Fallback);
        assert_eq!(report.events_dropped, 1);
        assert!(rig.consumer.next_published(0).is_none());
    }

    #[test]
    fn reports_input_events_dropped_when_the_ring_is_full() {
        let mut rig = audio_test_rig(SampleFormat::F32, Fallback::Silence, 4, 4, 2, 1, 1, 0, 0, 0);
        let input = [0.0; 4];
        let event = AudioEvent::midi(2, 0, [0x90, 60, 100]);
        for expected_dropped in [0, 0, 1] {
            let mut output = [0.0; 4];
            let report = rig.endpoint.process_events(
                4,
                &[&input],
                &mut [&mut output],
                Transport::default(),
                &[event],
                &mut [],
            );
            assert_eq!(report.events_dropped, expected_dropped);
        }
    }

    #[test]
    fn reports_pending_input_events_dropped_at_recovery_adoption() {
        let mut rig = audio_test_rig(SampleFormat::F32, Fallback::Silence, 4, 4, 2, 1, 1, 0, 0, 0);
        let input = [0.0; 2];
        let mut output = [0.0; 2];
        let event = AudioEvent::midi(1, 0, [0x90, 60, 100]);
        let transport = Transport::default();
        let first = rig.endpoint.process_events(
            2,
            &[&input],
            &mut [&mut output],
            transport,
            &[event],
            &mut [],
        );
        assert_eq!(first.events_dropped, 0);

        let replacement =
            audio_test_rig(SampleFormat::F32, Fallback::Silence, 4, 4, 2, 1, 1, 0, 0, 0);
        let mut next = replacement.endpoint.into_blocks().active;
        next.generation = 2;
        rig.updates.push(next).unwrap();
        rig.shared.generation.store(2, Ordering::Release);

        let report = rig.endpoint.process_events(
            2,
            &[&input],
            &mut [&mut output],
            transport.advanced(2, 48_000),
            &[],
            &mut [],
        );

        assert_eq!(report.events_dropped, 1);
        assert_eq!(report.generation, 2);
    }

    #[test]
    fn drops_midi_and_sysex_from_undeclared_output_event_buses() {
        let mut rig = audio_test_rig(SampleFormat::F32, Fallback::Silence, 4, 4, 2, 1, 1, 0, 0, 0);
        let input = [0.0; 4];
        let mut output = [0.0; 4];
        assert_eq!(
            rig.endpoint
                .process_events(
                    4,
                    &[&input],
                    &mut [&mut output],
                    Transport::default(),
                    &[],
                    &mut [],
                )
                .events_dropped,
            0
        );
        let sequence = rig.consumer.next_published(0).unwrap();
        let mut sysex = [0; vst_audio_ring::MAX_SYSEX_BYTES];
        sysex[0] = 0xf0;
        sysex[1] = 0xf7;
        let mut output_packet = EventBuffer::default();
        output_packet.push(AudioEvent::midi(0, 0, [0x90, 60, 100]));
        output_packet.push(AudioEvent::midi(1, 1, [0x90, 61, 100]));
        output_packet.push(AudioEvent {
            sample_offset: 2,
            bus: 2,
            data: EventData::SysEx {
                len: 2,
                bytes: sysex,
            },
        });
        assert!(rig.consumer.output_packet(0, sequence, &output_packet));
        rig.consumer.complete(0, sequence);

        let mut output_events = [AudioEvent::default(); 3];
        let warmup = rig.endpoint.process_events(
            4,
            &[&input],
            &mut [&mut output],
            Transport::default().advanced(4, 48_000),
            &[],
            &mut output_events,
        );
        assert_eq!((warmup.events_written, warmup.events_dropped), (0, 0));
        let report = rig.endpoint.process_events(
            4,
            &[&input],
            &mut [&mut output],
            Transport::default().advanced(8, 48_000),
            &[],
            &mut output_events,
        );

        assert_eq!((report.events_written, report.events_dropped), (1, 2));
        assert_eq!(output_events[0], AudioEvent::midi(0, 0, [0x90, 60, 100]));
    }

    fn cost_case(fallback: Fallback, quantum: u32, callback: u32, maximum: u32) {
        let depth = maximum.div_ceil(quantum);
        let rig = audio_test_rig(
            SampleFormat::F32,
            fallback,
            quantum,
            maximum,
            (depth + 1).next_power_of_two(),
            2,
            6,
            64,
            128,
            0,
        );
        let mut rig = rig;
        let inputs = [
            vec![0.25f32; callback as usize],
            vec![-0.125; callback as usize],
        ];
        let mut outputs = (0..6)
            .map(|_| vec![0.0f32; callback as usize])
            .collect::<Vec<_>>();
        for _ in 0..300 {
            let mut refs = outputs
                .iter_mut()
                .map(Vec::as_mut_slice)
                .collect::<Vec<_>>();
            black_box(rig.endpoint.process(&[&inputs[0], &inputs[1]], &mut refs));
            step_consumer(&rig, usize::MAX);
        }
        let mut elapsed = Vec::with_capacity(3000);
        for _ in 0..3000 {
            let mut refs = outputs
                .iter_mut()
                .map(Vec::as_mut_slice)
                .collect::<Vec<_>>();
            let start = Instant::now();
            black_box(rig.endpoint.process(&[&inputs[0], &inputs[1]], &mut refs));
            elapsed.push(start.elapsed());
            step_consumer(&rig, usize::MAX);
        }
        elapsed.sort_unstable();
        let median = elapsed[elapsed.len() / 2];
        let p99 = elapsed[elapsed.len() * 99 / 100];
        println!(
            "endpoint_cost fallback={fallback:?} quantum={quantum} callback={callback} maximum={maximum} median_us={:.3} p99_us={:.3} median_ns_per_frame={:.3}",
            duration_us(median),
            duration_us(p99),
            median.as_nanos() as f64 / f64::from(callback)
        );
    }

    fn duration_us(duration: Duration) -> f64 {
        duration.as_nanos() as f64 / 1000.0
    }

    #[test]
    #[ignore = "timing"]
    fn endpoint_cost() {
        for fallback in [Fallback::Silence, Fallback::DelayedInput] {
            cost_case(fallback, 480, 480, 480);
            cost_case(fallback, 128, 480, 512);
        }
    }
}
