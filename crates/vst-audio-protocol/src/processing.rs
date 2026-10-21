//! Shared processing configuration. Native bus indices are never collapsed.
use crate::Bus;
pub use vst_audio_ring::{
    Event, EventBuffer, EventData, MAX_EVENTS, MAX_SYSEX_BYTES, SampleFormat, Transport,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BusConfiguration {
    pub layout: Bus,
    pub active: bool,
}
impl From<Bus> for BusConfiguration {
    fn from(layout: Bus) -> Self {
        Self {
            layout,
            active: true,
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessingSetup {
    pub sample_rate: u32,
    pub quantum: u32,
    pub sample_format: SampleFormat,
    pub offline: bool,
    pub inputs: Vec<BusConfiguration>,
    pub outputs: Vec<BusConfiguration>,
}
impl ProcessingSetup {
    pub fn channels(buses: &[BusConfiguration]) -> u16 {
        buses
            .iter()
            .filter(|b| b.active)
            .map(|b| u32::from(b.layout.channels))
            .sum::<u32>()
            .min(u32::from(u16::MAX)) as u16
    }
    pub fn valid(&self) -> bool {
        (1000..=1_000_000).contains(&self.sample_rate)
            && (1..=65536).contains(&self.quantum)
            && [&self.inputs, &self.outputs].iter().all(|buses| {
                buses.len() <= 32
                    && Self::channels(buses) <= 64
                    && buses.iter().all(|b| {
                        b.layout.channels <= 64
                            && (b.layout.arrangement == 0
                                || b.layout.arrangement.count_ones()
                                    == u32::from(b.layout.channels))
                    })
            })
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Capabilities {
    pub f64: bool,
    pub input_event_buses: u16,
    pub output_event_buses: u16,
    pub editor: bool,
    pub sysex: bool,
    pub sample_accurate_parameters: bool,
}
