//! Platform-independent control state machine and audio block adapter.
use crate::{Plugin, PluginError};
use clap::Parser;
use vst_audio_protocol::{Frame, Message};

#[derive(Parser, Clone, Debug)]
#[command(about = "Isolated plugin host (internal protocol)")]
pub struct Args {
    #[arg(long)]
    pub control_in: usize,
    #[arg(long)]
    pub control_out: usize,
    #[arg(long)]
    pub region: usize,
    #[arg(long)]
    pub region_bytes: usize,
    #[arg(long)]
    pub work: u64,
    #[arg(long)]
    pub done: u64,
    #[arg(long)]
    pub ready: u64,
    #[arg(long)]
    pub parent: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Loaded,
    Configured,
    Active,
    Closed,
}

pub struct Session<P> {
    plugin: P,
    phase: Phase,
}
impl<P> std::fmt::Debug for Session<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("phase", &self.phase)
            .finish_non_exhaustive()
    }
}
impl<P: Plugin> Session<P> {
    pub fn new(plugin: P) -> Self {
        Self {
            plugin,
            phase: Phase::Loaded,
        }
    }
    pub fn plugin_mut(&mut self) -> &mut P {
        &mut self.plugin
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }
    pub fn loaded(&self, request: u32) -> Frame {
        let info = self.plugin.info();
        Frame {
            flags: 0,
            request,
            message: Message::Loaded {
                name: info.name,
                vendor: info.vendor,
                latency_frames: info.latency_frames,
                inputs: info.inputs,
                outputs: info.outputs,
            },
        }
    }
    pub fn serve(&mut self, frame: Frame) -> Frame {
        let message = self
            .handle(frame.message)
            .unwrap_or_else(|error| Message::Fault {
                code: 1,
                text: error.to_string(),
            });
        Frame {
            flags: 0,
            request: frame.request,
            message,
        }
    }
    fn handle(&mut self, message: Message) -> Result<Message, PluginError> {
        match message {
            Message::Configure {
                rate,
                max_frames,
                in_channels,
                out_channels,
            } if self.phase == Phase::Loaded => {
                self.plugin
                    .configure(rate, max_frames, in_channels, out_channels)?;
                self.phase = Phase::Configured;
                Ok(Message::Ack)
            }
            Message::Activate if self.phase == Phase::Configured => {
                self.plugin.activate()?;
                self.phase = Phase::Active;
                Ok(Message::Ack)
            }
            Message::GetState if self.phase != Phase::Closed => Ok(Message::State {
                blob: self.plugin.get_state()?,
            }),
            Message::SetState { blob } if self.phase != Phase::Closed => {
                self.plugin.set_state(&blob)?;
                Ok(Message::Ack)
            }
            Message::OpenEditor if self.phase != Phase::Closed => {
                self.plugin.open_editor()?;
                Ok(Message::Ack)
            }
            Message::Shutdown => {
                self.plugin.deactivate();
                self.phase = Phase::Closed;
                Ok(Message::Ack)
            }
            _ => Err(PluginError("request is invalid in this host phase".into())),
        }
    }
}

/// Scratch is allocated on the control thread, not inside processing.
#[derive(Debug)]
pub struct AudioBuffer {
    samples: Vec<f32>,
    samples64: Vec<f64>,
    sample_format: vst_audio_ring::SampleFormat,
    input: Box<vst_audio_ring::EventBuffer>,
    output: Box<vst_audio_ring::EventBuffer>,
    frames: usize,
    planes: usize,
}
impl AudioBuffer {
    pub fn new(geometry: vst_audio_ring::Geometry) -> Result<Self, vst_audio_ring::GeometryError> {
        let geometry = geometry.validate()?;
        Ok(Self {
            samples: vec![
                0.0;
                if geometry.sample_format == vst_audio_ring::SampleFormat::F32 {
                    geometry.samples_per_slot()
                } else {
                    0
                }
            ],
            samples64: vec![
                0.0;
                if geometry.sample_format == vst_audio_ring::SampleFormat::F64 {
                    geometry.samples_per_slot()
                } else {
                    0
                }
            ],
            sample_format: geometry.sample_format,
            input: Box::default(),
            output: Box::default(),
            frames: geometry.max_frames as usize,
            planes: geometry.planes(),
        })
    }
    pub fn drain(
        &mut self,
        consumer: &vst_audio_ring::Consumer<'_>,
        processor: &mut dyn crate::Processor,
    ) {
        while let Some(sequence) = consumer.next_published(0) {
            let frames = self.frames;
            let planes = self.planes;
            let mut processed = false;
            let mut context = vst_audio_ring::Transport::default();
            if !consumer.input_packet(0, sequence, &mut context, &mut self.input) {
                consumer.fault(2);
                break;
            }
            self.output.clear();
            let copied = if self.sample_format == vst_audio_ring::SampleFormat::F32 {
                consumer.slot_mut(0, sequence, &mut self.samples, |samples| {
                    let mut refs: [&mut [f32]; 64] = std::array::from_fn(|_| &mut [][..]);
                    for (to, from) in refs.iter_mut().zip(samples.chunks_exact_mut(frames)) {
                        *to = from;
                    }
                    processed = processor.process_block(
                        &mut refs[..planes],
                        frames,
                        &context,
                        self.input.as_slice(),
                        &mut self.output,
                    );
                })
            } else {
                consumer.slot_mut_f64(0, sequence, &mut self.samples64, |samples| {
                    let mut refs: [&mut [f64]; 64] = std::array::from_fn(|_| &mut [][..]);
                    for (to, from) in refs.iter_mut().zip(samples.chunks_exact_mut(frames)) {
                        *to = from;
                    }
                    processed = processor.process_block_f64(
                        &mut refs[..planes],
                        frames,
                        &context,
                        self.input.as_slice(),
                        &mut self.output,
                    );
                })
            };
            if !copied || !processed {
                break;
            }
            let non_finite = self
                .samples
                .iter()
                .filter(|value| !value.is_finite())
                .count()
                + self.samples64.iter().filter(|v| !v.is_finite()).count();
            if non_finite != 0 {
                consumer.set_last_error(non_finite as u64);
            }
            if !consumer.output_packet(0, sequence, &self.output) {
                consumer.fault(3);
                break;
            }
            consumer.complete(0, sequence);
        }
    }
}

pub fn main() {
    if let Err(error) = crate::native::host_main(Args::parse()) {
        eprintln!("{error}");
        std::process::exit(3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bus, BusInfo, BusKind, PluginEvents, PluginInfo, Processor};
    use std::io::Cursor;
    use vst_audio_ring::{Consumer, Geometry, InProcessEvent, Producer, RegionBuf};
    #[test]
    fn parses_inherited_handles() {
        let args = Args::try_parse_from([
            "host",
            "--control-in",
            "1",
            "--control-out",
            "2",
            "--region",
            "3",
            "--region-bytes",
            "16384",
            "--work",
            "21474836484",
            "--done",
            "5",
            "--ready",
            "6",
            "--parent",
            "7",
        ])
        .unwrap();
        assert_eq!(args.region_bytes, 16384);
        assert_eq!(args.parent, 7);
        assert_eq!(args.work, 4 | (5u64 << 32));
        assert!(Args::try_parse_from(["host", "--control-in", "-1"]).is_err());
    }
    #[derive(Default)]
    struct Fake {
        state: Vec<u8>,
        active: bool,
    }
    impl Processor for Fake {
        fn process(&mut self, planes: &mut [&mut [f32]], frames: usize) -> bool {
            if !self.active {
                return false;
            }
            for plane in planes {
                for value in &mut plane[..frames] {
                    *value *= 2.0;
                }
            }
            true
        }
    }
    impl Plugin for Fake {
        fn info(&self) -> PluginInfo {
            PluginInfo {
                name: "Fake".into(),
                vendor: "Tests".into(),
                latency_frames: 0,
                inputs: vec![BusInfo {
                    layout: Bus {
                        channels: 2,
                        arrangement: 3,
                    },
                    kind: BusKind::Main,
                    name: String::new(),
                    default_active: true,
                }],
                outputs: vec![BusInfo {
                    layout: Bus {
                        channels: 2,
                        arrangement: 3,
                    },
                    kind: BusKind::Main,
                    name: String::new(),
                    default_active: true,
                }],
            }
        }
        fn configure(&mut self, _: u32, _: u32, _: u16, _: u16) -> Result<(), PluginError> {
            Ok(())
        }
        fn activate(&mut self) -> Result<(), PluginError> {
            self.active = true;
            Ok(())
        }
        fn deactivate(&mut self) {
            self.active = false;
        }
        fn take_processor(&mut self) -> Result<Box<dyn Processor>, PluginError> {
            Ok(Box::new(Fake {
                state: self.state.clone(),
                active: self.active,
            }))
        }
        fn get_state(&mut self) -> Result<Vec<u8>, PluginError> {
            Ok(self.state.clone())
        }
        fn set_state(&mut self, blob: &[u8]) -> Result<(), PluginError> {
            self.state = blob.to_vec();
            Ok(())
        }
        fn open_editor(&mut self) -> Result<(), PluginError> {
            Ok(())
        }
        fn idle(&mut self) {}
        fn take_events(&mut self) -> PluginEvents {
            PluginEvents::default()
        }
    }
    #[test]
    fn closed_admission_keeps_sequences_pending_and_heartbeat_alive() {
        struct Gated {
            admitted: bool,
            seen: Vec<f32>,
        }
        impl Processor for Gated {
            fn process(&mut self, planes: &mut [&mut [f32]], _: usize) -> bool {
                if !self.admitted {
                    return false;
                }
                self.seen.push(planes[0][0]);
                true
            }
        }
        let geometry = Geometry {
            sample_format: vst_audio_ring::SampleFormat::F32,
            lanes: 1,
            input_channels: 2,
            output_channels: 2,
            max_frames: 4,
            sample_rate: 48000,
            latency_bound_frames: 96000,
        };
        let memory = RegionBuf::new(geometry).unwrap();
        let event = InProcessEvent::new();
        let mut producer = Producer::new(memory.region(), [Some(&event), None], &event);
        producer.announce(geometry, 1).unwrap();
        let mut consumer = Consumer::new(memory.region(), &event, [Some(&event), None]);
        consumer.validate_and_ready(2).unwrap();
        let mut buffer = AudioBuffer::new(geometry).unwrap();
        let mut plugin = Gated {
            admitted: false,
            seen: Vec::new(),
        };
        for sequence in 1..=3 {
            let published = producer.try_publish(0, sequence, |slot| {
                slot.copy_from(&[sequence as f32; 8]);
            });
            assert_eq!(
                published,
                if sequence <= 2 {
                    vst_audio_ring::Publish::Published
                } else {
                    vst_audio_ring::Publish::Dropped
                }
            );
            let heartbeat = memory.region().heartbeat();
            consumer.beat();
            buffer.drain(&consumer, &mut plugin);
            assert_eq!(producer.completed(0), 0);
            assert!(memory.region().heartbeat() > heartbeat);
        }
        assert!(plugin.seen.is_empty());
        plugin.admitted = true;
        buffer.drain(&consumer, &mut plugin);
        assert_eq!(producer.completed(0), 2);
        assert_eq!(plugin.seen, [1.0, 2.0]);
        assert_eq!(
            producer.try_publish(0, 3, |slot| {
                slot.copy_from(&[3.0; 8]);
            }),
            vst_audio_ring::Publish::Published
        );
        buffer.drain(&consumer, &mut plugin);
        assert_eq!(producer.completed(0), 3);
        assert_eq!(plugin.seen, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn loaded_main_bus_layouts_come_from_plugin() {
        let mut info = Fake::default().info();
        info.inputs[0].layout = Bus {
            channels: 6,
            arrangement: 63,
        };
        info.outputs[0].layout = Bus {
            channels: 2,
            arrangement: 3,
        };
        assert_eq!(
            crate::layout::main_bus(
                &info
                    .inputs
                    .iter()
                    .map(|b| b.layout.clone())
                    .collect::<Vec<_>>(),
                "input"
            )
            .unwrap(),
            [1, 2, 4, 8, 16, 32]
        );
        assert_eq!(
            crate::layout::main_bus(
                &info
                    .outputs
                    .iter()
                    .map(|b| b.layout.clone())
                    .collect::<Vec<_>>(),
                "output"
            )
            .unwrap(),
            [1, 2]
        );
        info.inputs[0].layout = Bus {
            channels: 1,
            arrangement: 1 << 18,
        };
        assert!(
            crate::layout::main_bus(
                &info
                    .inputs
                    .iter()
                    .map(|b| b.layout.clone())
                    .collect::<Vec<_>>(),
                "input"
            )
            .unwrap_err()
            .to_string()
            .contains("18")
        );
        info.inputs[0].layout = Bus {
            channels: 0,
            arrangement: 0,
        };
        assert!(
            crate::layout::main_bus(
                &info
                    .inputs
                    .iter()
                    .map(|b| b.layout.clone())
                    .collect::<Vec<_>>(),
                "input"
            )
            .unwrap_err()
            .to_string()
            .contains("0 channels")
        );
        info.inputs[0].layout = Bus {
            channels: 3,
            arrangement: 3,
        };
        assert!(
            crate::layout::main_bus(
                &info
                    .inputs
                    .iter()
                    .map(|b| b.layout.clone())
                    .collect::<Vec<_>>(),
                "input"
            )
            .is_err()
        );
        assert!(crate::layout::main_bus(&[], "input").is_err());
    }

    #[test]
    fn frame_session_and_ring_round_trip() {
        let geometry = Geometry {
            sample_format: vst_audio_ring::SampleFormat::F32,
            lanes: 1,
            input_channels: 2,
            output_channels: 2,
            max_frames: 4,
            sample_rate: 48000,
            latency_bound_frames: 96000,
        };
        let memory = RegionBuf::new(geometry).unwrap();
        let event = InProcessEvent::new();
        let mut producer = Producer::new(memory.region(), [Some(&event), None], &event);
        producer.announce(geometry, 1).unwrap();
        let mut session = Session::new(Fake::default());
        let mut wire = Cursor::new(Vec::new());
        let hello = Frame {
            flags: 0,
            request: 0,
            message: Message::hello(),
        };
        vst_audio_protocol::write_frame(&mut wire, &hello).unwrap();
        vst_audio_protocol::write_frame(&mut wire, &session.loaded(1)).unwrap();
        wire.set_position(0);
        assert_eq!(
            vst_audio_protocol::read_frame(&mut wire).unwrap(),
            Some(vst_audio_protocol::DecodedFrame::Known(hello))
        );
        assert!(matches!(
            vst_audio_protocol::read_frame(&mut wire).unwrap(),
            Some(vst_audio_protocol::DecodedFrame::Known(Frame {
                message: Message::Loaded { .. },
                ..
            }))
        ));
        let request = |id, message| Frame {
            flags: 0,
            request: id,
            message,
        };
        let reply = session.serve(request(
            2,
            Message::Configure {
                rate: 48000,
                max_frames: 4,
                in_channels: 2,
                out_channels: 2,
            },
        ));
        assert_eq!(reply, request(2, Message::Ack));
        let mut consumer = Consumer::new(memory.region(), &event, [Some(&event), None]);
        assert_eq!(consumer.validate_and_ready(2).unwrap(), geometry);
        assert_eq!(
            session.serve(request(3, Message::Activate)).message,
            Message::Ack
        );
        let mut buffer = AudioBuffer::new(geometry).unwrap();
        let mut processor = session.plugin_mut().take_processor().unwrap();
        for sequence in 1..=3 {
            assert_eq!(
                producer.try_publish(0, sequence, |slot| {
                    slot.copy_from(&[0.25; 8]);
                }),
                vst_audio_ring::Publish::Published
            );
            buffer.drain(&consumer, processor.as_mut());
            assert!(producer.try_read(0, sequence, |slot| assert_eq!(slot.read(0, 0), 0.5)));
        }
        assert_eq!(
            session
                .serve(request(4, Message::SetState { blob: vec![1, 2] }))
                .message,
            Message::Ack
        );
        assert_eq!(
            session.serve(request(5, Message::GetState)).message,
            Message::State { blob: vec![1, 2] }
        );
        session.serve(request(6, Message::Shutdown));
        assert_eq!(session.phase(), Phase::Closed);
        assert!(!session.plugin_mut().active);
    }
}
