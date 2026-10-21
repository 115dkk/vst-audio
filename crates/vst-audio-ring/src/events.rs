//! Bounded, pointer-free realtime events and transport snapshots.
pub const MAX_EVENTS: usize = 256;
pub const MAX_SYSEX_BYTES: usize = 256;
pub(crate) const EVENT_WORDS: usize = 72;
pub(crate) const CONTEXT_WORDS: usize = 32;
pub(crate) const PACKET_WORDS: usize = 2 + MAX_EVENTS * EVENT_WORDS;
pub(crate) const METADATA_WORDS: usize = CONTEXT_WORDS + 2 * PACKET_WORDS;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transport {
    pub sample_position: i64,
    pub continuous_position: i64,
    pub ppq_position: f64,
    pub tempo: f64,
    pub bar_start: f64,
    pub cycle_start: f64,
    pub cycle_end: f64,
    pub numerator: u16,
    pub denominator: u16,
    pub playing: bool,
    pub recording: bool,
    pub cycling: bool,
}
impl Default for Transport {
    fn default() -> Self {
        Self {
            sample_position: 0,
            continuous_position: 0,
            ppq_position: 0.0,
            tempo: 120.0,
            bar_start: 0.0,
            cycle_start: 0.0,
            cycle_end: 4.0,
            numerator: 4,
            denominator: 4,
            playing: true,
            recording: false,
            cycling: false,
        }
    }
}
impl Transport {
    pub fn valid(self) -> bool {
        [
            self.ppq_position,
            self.tempo,
            self.bar_start,
            self.cycle_start,
            self.cycle_end,
        ]
        .iter()
        .all(|v| v.is_finite())
            && self.tempo > 0.0
            && self.tempo <= 1_000_000.0
            && (1..=1024).contains(&self.numerator)
            && self.denominator.is_power_of_two()
            && self.denominator <= 1024
            && (!self.cycling || self.cycle_end > self.cycle_start)
    }
    pub fn advanced(mut self, frames: usize, rate: u32) -> Self {
        let frames_i64 = i64::try_from(frames).unwrap_or(i64::MAX);
        self.continuous_position = self.continuous_position.saturating_add(frames_i64);
        if self.playing {
            self.sample_position = self.sample_position.saturating_add(frames_i64);
            self.ppq_position += frames as f64 * self.tempo / (60.0 * f64::from(rate.max(1)));
        }
        self
    }
    pub(crate) fn words(self) -> [u32; CONTEXT_WORDS] {
        let mut words = [0; CONTEXT_WORDS];
        words[0] = u32::from(self.playing)
            | (u32::from(self.recording) << 1)
            | (u32::from(self.cycling) << 2);
        words[1] = u32::from(self.numerator) | (u32::from(self.denominator) << 16);
        for (i, value) in [
            self.sample_position as u64,
            self.continuous_position as u64,
            self.ppq_position.to_bits(),
            self.tempo.to_bits(),
            self.bar_start.to_bits(),
            self.cycle_start.to_bits(),
            self.cycle_end.to_bits(),
        ]
        .into_iter()
        .enumerate()
        {
            words[2 + i * 2] = value as u32;
            words[3 + i * 2] = (value >> 32) as u32;
        }
        words
    }
    pub(crate) fn from_words(words: &[u32]) -> Option<Self> {
        if words.len() < CONTEXT_WORDS || words[0] & !7 != 0 {
            return None;
        }
        let wide = |i: usize| u64::from(words[2 + i * 2]) | (u64::from(words[3 + i * 2]) << 32);
        let value = Self {
            sample_position: wide(0) as i64,
            continuous_position: wide(1) as i64,
            ppq_position: f64::from_bits(wide(2)),
            tempo: f64::from_bits(wide(3)),
            bar_start: f64::from_bits(wide(4)),
            cycle_start: f64::from_bits(wide(5)),
            cycle_end: f64::from_bits(wide(6)),
            numerator: words[1] as u16,
            denominator: (words[1] >> 16) as u16,
            playing: words[0] & 1 != 0,
            recording: words[0] & 2 != 0,
            cycling: words[0] & 4 != 0,
        };
        value.valid().then_some(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EventData {
    /// MIDI 1 channel message. Unused bytes in 2-byte messages must be zero.
    Midi([u8; 3]),
    SysEx {
        len: u16,
        bytes: [u8; MAX_SYSEX_BYTES],
    },
    Parameter {
        id: u32,
        value: f64,
    },
    /// An in-block transport change. Processing splits at this sample offset.
    Transport(Transport),
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Event {
    pub sample_offset: u32,
    pub bus: u16,
    pub data: EventData,
}
impl Default for Event {
    fn default() -> Self {
        Self {
            sample_offset: 0,
            bus: 0,
            data: EventData::Midi([0x80, 0, 0]),
        }
    }
}
impl Event {
    pub fn midi(sample_offset: u32, bus: u16, data: [u8; 3]) -> Self {
        Self {
            sample_offset,
            bus,
            data: EventData::Midi(data),
        }
    }
    pub fn valid(self, frames: u32) -> bool {
        self.sample_offset < frames
            && self.bus < 32
            && match self.data {
                EventData::Midi([status, a, b]) => {
                    (0x80..=0xef).contains(&status)
                        && a < 128
                        && b < 128
                        && (!matches!(status & 0xf0, 0xc0 | 0xd0) || b == 0)
                }
                EventData::SysEx { len, .. } => usize::from(len) <= MAX_SYSEX_BYTES && len > 0,
                EventData::Parameter { value, .. } => {
                    self.bus == 0 && value.is_finite() && (0.0..=1.0).contains(&value)
                }
                EventData::Transport(context) => self.bus == 0 && context.valid(),
            }
    }
    pub(crate) fn words(self) -> [u32; EVENT_WORDS] {
        let mut words = [0; EVENT_WORDS];
        words[1] = self.sample_offset;
        words[2] = u32::from(self.bus);
        match self.data {
            EventData::Midi([a, b, c]) => {
                words[0] = 1;
                words[3] = u32::from_le_bytes([a, b, c, 0]);
            }
            EventData::Parameter { id, value } => {
                words[0] = 2;
                words[3] = id;
                words[4] = value.to_bits() as u32;
                words[5] = (value.to_bits() >> 32) as u32;
            }
            EventData::Transport(context) => {
                words[0] = 3;
                words[4..4 + CONTEXT_WORDS].copy_from_slice(&context.words());
            }
            EventData::SysEx { len, bytes } => {
                words[0] = 4;
                words[3] = u32::from(len);
                for (to, from) in words[4..68].iter_mut().zip(bytes.as_chunks::<4>().0.iter()) {
                    *to = u32::from_le_bytes(*from);
                }
            }
        }
        words
    }
    pub(crate) fn from_words(words: &[u32], frames: u32) -> Option<Self> {
        if words.len() != EVENT_WORDS || words[2] >= 32 {
            return None;
        }
        let data = match words[0] {
            1 => {
                let [a, b, c, d] = words[3].to_le_bytes();
                if d != 0 {
                    return None;
                }
                EventData::Midi([a, b, c])
            }
            2 => EventData::Parameter {
                id: words[3],
                value: f64::from_bits(u64::from(words[4]) | (u64::from(words[5]) << 32)),
            },
            3 => EventData::Transport(Transport::from_words(&words[4..4 + CONTEXT_WORDS])?),
            4 => {
                if words[3] == 0 || words[3] > MAX_SYSEX_BYTES as u32 {
                    return None;
                }
                let mut bytes = [0; MAX_SYSEX_BYTES];
                for (to, from) in bytes
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(words[4..68].iter())
                {
                    to.copy_from_slice(&from.to_le_bytes());
                }
                EventData::SysEx {
                    len: words[3] as u16,
                    bytes,
                }
            }
            _ => return None,
        };
        let event = Self {
            sample_offset: words[1],
            bus: words[2] as u16,
            data,
        };
        event.valid(frames).then_some(event)
    }
}

#[derive(Clone, Debug)]
pub struct EventBuffer {
    events: [Event; MAX_EVENTS],
    len: usize,
    pub dropped: u32,
}
impl Default for EventBuffer {
    fn default() -> Self {
        Self {
            events: [Event::default(); MAX_EVENTS],
            len: 0,
            dropped: 0,
        }
    }
}
impl EventBuffer {
    pub fn clear(&mut self) {
        self.len = 0;
        self.dropped = 0;
    }
    pub fn sort_by_offset(&mut self) {
        for i in 1..self.len {
            let mut j = i;
            while j > 0 && self.events[j - 1].sample_offset > self.events[j].sample_offset {
                self.events.swap(j - 1, j);
                j -= 1;
            }
        }
    }
    pub fn as_slice(&self) -> &[Event] {
        &self.events[..self.len]
    }
    pub fn push(&mut self, event: Event) -> bool {
        if self.len == MAX_EVENTS {
            self.dropped = self.dropped.saturating_add(1);
            return false;
        }
        self.events[self.len] = event;
        self.len += 1;
        true
    }
}
