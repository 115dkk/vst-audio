#![forbid(unsafe_code)]

//! Control framing and deterministic host supervision from ADR 0021.

use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Read, Write};
use std::time::Duration;

pub mod editor;
pub mod layout;
pub mod processing;
pub use editor::{EditorAction, EditorImage, EditorReply, EditorStatus};
pub use processing::{BusConfiguration, Capabilities, ProcessingSetup, SampleFormat};
pub mod state;
#[derive(Clone, Debug, thiserror::Error)]
#[error("{0}")]
pub struct PluginError(pub String);

pub const MAGIC: [u8; 4] = *b"SSRP";
pub const PROTOCOL_MAJOR: u16 = 4;
pub const PROTOCOL_MINOR: u16 = 0;
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_STATE_BYTES: usize = 64 * 1024 * 1024;
/// Longest UTF-8 string in a message. Windows long paths are why it is above
/// MAX_PATH.
pub const MAX_STRING_BYTES: usize = 4096;
const FRAME_META_BYTES: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum PluginFormat {
    Vst2 = 2,
    Vst3 = 3,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bus {
    pub channels: u16,
    pub arrangement: u64,
}

/// What a declared audio bus is for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BusKind {
    Main,
    /// Sidechain inputs and auxiliary outputs.
    Aux,
}

/// A bus as the plugin declares it, before any is activated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BusInfo {
    pub layout: Bus,
    pub kind: BusKind,
    /// The plugin's own name for the bus; may be empty.
    pub name: String,
    /// The plugin asks for this bus to be active unless the host decides otherwise.
    pub default_active: bool,
}

impl BusInfo {
    pub fn configuration(&self, active: bool) -> BusConfiguration {
        BusConfiguration {
            layout: self.layout.clone(),
            active,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClassInfo {
    pub id: [u8; 16],
    pub name: String,
    pub vendor: String,
    pub category: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Parameter {
    pub id: u32,
    pub name: String,
    pub unit: String,
    pub value: f64,
    pub default: Option<f64>,
    pub steps: Option<u32>,
    pub automatable: bool,
    pub read_only: bool,
    pub hidden: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Hello {
        magic: [u8; 4],
        major: u16,
        minor: u16,
    },
    Load {
        path: String,
        format: PluginFormat,
        class_id: Option<[u8; 16]>,
    },
    Configure {
        rate: u32,
        max_frames: u32,
        in_channels: u16,
        out_channels: u16,
    },
    Editor(EditorAction),
    EditorStatus(EditorStatus),
    EditorImage(EditorImage),
    Prepare(ProcessingSetup),
    Describe,
    Features(Capabilities),
    Activate,
    SetState {
        blob: Vec<u8>,
    },
    GetState,
    OpenEditor,
    CloseEditor,
    ListParameters {
        start: u32,
        count: u16,
    },
    Parameters {
        total: u32,
        entries: Vec<Parameter>,
    },
    SetParameter {
        id: u32,
        value: f64,
    },
    RequestError {
        code: u32,
        text: String,
    },
    Shutdown,
    Scan {
        path: String,
    },
    Loaded {
        name: String,
        vendor: String,
        latency_frames: u32,
        inputs: Vec<BusInfo>,
        outputs: Vec<BusInfo>,
    },
    Classes {
        entries: Vec<ClassInfo>,
    },
    LatencyChanged {
        frames: u32,
    },
    State {
        blob: Vec<u8>,
    },
    ParamsChanged,
    Fault {
        code: u32,
        text: String,
    },
    /// The reply to a request that returns no data (`Configure`, `Activate`,
    /// `SetState`, `OpenEditor`). It carries the request's number.
    Ack,
    /// The editor window was closed by the user.
    EditorClosed,
}

impl Message {
    pub fn hello() -> Self {
        Self::Hello {
            magic: MAGIC,
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
        }
    }

    fn kind(&self) -> u16 {
        match self {
            Self::Hello { .. } => 1,
            Self::Load { .. } => 2,
            Self::Configure { .. } => 3,
            Self::Editor(_) => 26,
            Self::EditorStatus(_) => 27,
            Self::EditorImage(_) => 28,
            Self::Prepare(_) => 23,
            Self::Describe => 24,
            Self::Features(_) => 25,
            Self::Activate => 4,
            Self::SetState { .. } => 5,
            Self::GetState => 6,
            Self::OpenEditor => 7,
            Self::CloseEditor => 18,
            Self::ListParameters { .. } => 19,
            Self::Parameters { .. } => 20,
            Self::SetParameter { .. } => 21,
            Self::RequestError { .. } => 22,
            Self::Shutdown => 8,
            Self::Scan { .. } => 9,
            Self::Loaded { .. } => 10,
            Self::Classes { .. } => 11,
            Self::LatencyChanged { .. } => 12,
            Self::State { .. } => 13,
            Self::ParamsChanged => 14,
            Self::Fault { .. } => 15,
            Self::Ack => 16,
            Self::EditorClosed => 17,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub flags: u16,
    pub request: u32,
    pub message: Message,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnknownFrame {
    pub kind: u16,
    pub flags: u16,
    pub request: u32,
    pub body_bytes: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DecodedFrame {
    Known(Frame),
    Unknown(UnknownFrame),
}

#[derive(Debug)]
pub enum CodecError {
    Io(io::Error),
    Truncated,
    InvalidLength,
    TooLarge { length: usize, limit: usize },
    StringTooLong,
    InvalidUtf8,
    InvalidValue,
    WrongMagic,
    WrongMajor { got: u16, expected: u16 },
    TrailingBytes,
}

impl fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for CodecError {}

impl From<io::Error> for CodecError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub fn write_frame(writer: &mut impl Write, frame: &Frame) -> Result<(), CodecError> {
    let bytes = encode_frame(frame)?;
    writer.write_all(&bytes)?;
    Ok(())
}

pub fn encode_frame(frame: &Frame) -> Result<Vec<u8>, CodecError> {
    let mut body = Vec::new();
    encode_message(&frame.message, &mut body)?;
    let length = FRAME_META_BYTES
        .checked_add(body.len())
        .ok_or(CodecError::InvalidLength)?;
    let limit = length_limit(frame.message.kind());
    if length > limit {
        return Err(CodecError::TooLarge { length, limit });
    }
    let mut output = Vec::with_capacity(4 + length);
    output.extend_from_slice(&(length as u32).to_le_bytes());
    output.extend_from_slice(&frame.message.kind().to_le_bytes());
    output.extend_from_slice(&frame.flags.to_le_bytes());
    output.extend_from_slice(&frame.request.to_le_bytes());
    output.extend_from_slice(&body);
    Ok(output)
}

pub fn decode_frame(bytes: &[u8]) -> Result<DecodedFrame, CodecError> {
    if bytes.len() < 12 {
        return Err(CodecError::Truncated);
    }
    let length = u32::from_le_bytes(bytes[0..4].try_into().expect("four bytes")) as usize;
    if length < FRAME_META_BYTES {
        return Err(CodecError::InvalidLength);
    }
    if bytes.len() < 4 + length {
        return Err(CodecError::Truncated);
    }
    if bytes.len() != 4 + length {
        return Err(CodecError::TrailingBytes);
    }
    decode_parts(&bytes[4..12], &bytes[12..])
}

/// Reads one frame. Its 8-byte metadata is read first, and the declared length
/// is rejected before allocating the body. `Ok(None)` is a clean EOF before a
/// new frame; a partial frame is an error.
pub fn read_frame(reader: &mut impl Read) -> Result<Option<DecodedFrame>, CodecError> {
    let mut length_bytes = [0_u8; 4];
    let mut first = [0_u8; 1];
    match reader.read(&mut first) {
        Ok(0) => return Ok(None),
        Ok(_) => length_bytes[0] = first[0],
        Err(error) => return Err(CodecError::Io(error)),
    }
    reader.read_exact(&mut length_bytes[1..]).map_err(map_eof)?;
    let length = u32::from_le_bytes(length_bytes) as usize;
    if length < FRAME_META_BYTES {
        return Err(CodecError::InvalidLength);
    }
    let mut meta = [0_u8; FRAME_META_BYTES];
    reader.read_exact(&mut meta).map_err(map_eof)?;
    let kind = u16::from_le_bytes(meta[0..2].try_into().expect("two bytes"));
    let limit = length_limit(kind);
    if length > limit {
        return Err(CodecError::TooLarge { length, limit });
    }
    let mut body = vec![0; length - FRAME_META_BYTES];
    reader.read_exact(&mut body).map_err(map_eof)?;
    decode_parts(&meta, &body).map(Some)
}

fn map_eof(error: io::Error) -> CodecError {
    if error.kind() == io::ErrorKind::UnexpectedEof {
        CodecError::Truncated
    } else {
        CodecError::Io(error)
    }
}

fn length_limit(kind: u16) -> usize {
    if kind == 28 {
        return 16 * 1024 * 1024 + 16;
    }
    if matches!(kind, 5 | 13) {
        MAX_STATE_BYTES
    } else {
        MAX_MESSAGE_BYTES
    }
}

fn decode_parts(meta: &[u8], body: &[u8]) -> Result<DecodedFrame, CodecError> {
    let kind = u16::from_le_bytes(meta[0..2].try_into().expect("two bytes"));
    let flags = u16::from_le_bytes(meta[2..4].try_into().expect("two bytes"));
    let request = u32::from_le_bytes(meta[4..8].try_into().expect("four bytes"));
    let length = FRAME_META_BYTES + body.len();
    let limit = length_limit(kind);
    if length > limit {
        return Err(CodecError::TooLarge { length, limit });
    }
    let Some(message) = decode_message(kind, body)? else {
        return Ok(DecodedFrame::Unknown(UnknownFrame {
            kind,
            flags,
            request,
            body_bytes: body.len(),
        }));
    };
    Ok(DecodedFrame::Known(Frame {
        flags,
        request,
        message,
    }))
}

fn encode_message(message: &Message, out: &mut Vec<u8>) -> Result<(), CodecError> {
    match message {
        Message::Hello {
            magic,
            major,
            minor,
        } => {
            out.extend_from_slice(magic);
            put_u16(out, *major);
            put_u16(out, *minor);
        }
        Message::Load {
            path,
            format,
            class_id,
        } => {
            put_string(out, path)?;
            put_u16(out, *format as u16);
            out.push(u8::from(class_id.is_some()));
            if let Some(id) = class_id {
                out.extend_from_slice(id);
            }
        }
        Message::Configure {
            rate,
            max_frames,
            in_channels,
            out_channels,
        } => {
            put_u32(out, *rate);
            put_u32(out, *max_frames);
            put_u16(out, *in_channels);
            put_u16(out, *out_channels);
        }
        Message::Activate
        | Message::Describe
        | Message::GetState
        | Message::CloseEditor
        | Message::OpenEditor
        | Message::Shutdown
        | Message::ParamsChanged
        | Message::Ack
        | Message::EditorClosed => {}
        Message::SetState { blob } | Message::State { blob } => out.extend_from_slice(blob),
        Message::Scan { path } => put_string(out, path)?,
        Message::Loaded {
            name,
            vendor,
            latency_frames,
            inputs,
            outputs,
        } => {
            put_string(out, name)?;
            put_string(out, vendor)?;
            put_u32(out, *latency_frames);
            put_bus_infos(out, inputs)?;
            put_bus_infos(out, outputs)?;
        }
        Message::Classes { entries } => {
            put_u16(
                out,
                u16::try_from(entries.len()).map_err(|_| CodecError::InvalidValue)?,
            );
            for entry in entries {
                out.extend_from_slice(&entry.id);
                put_string(out, &entry.name)?;
                put_string(out, &entry.vendor)?;
                put_string(out, &entry.category)?;
            }
        }
        Message::LatencyChanged { frames } => put_u32(out, *frames),
        Message::Editor(action) => match action {
            EditorAction::Show => out.push(0),
            EditorAction::Hide => out.push(1),
            EditorAction::Focus => out.push(2),
            EditorAction::Resize { width, height } => {
                out.push(3);
                put_u32(out, *width);
                put_u32(out, *height);
            }
            EditorAction::Scale(scale) => {
                out.push(4);
                put_u32(out, scale.to_bits());
            }
            EditorAction::Inspect => out.push(5),
            EditorAction::Capture => out.push(6),
        },
        Message::EditorStatus(status) => {
            out.push(u8::from(status.visible));
            put_u32(out, status.width);
            put_u32(out, status.height);
            put_u32(out, status.scale.to_bits());
            out.push(u8::from(status.resize_supported));
            out.push(u8::from(status.scale_supported));
        }
        Message::EditorImage(image) => {
            if !image.valid() {
                return Err(CodecError::InvalidValue);
            }
            put_u32(out, image.width);
            put_u32(out, image.height);
            out.extend_from_slice(&image.rgba);
        }
        Message::Prepare(setup) => {
            if !setup.valid() {
                return Err(CodecError::InvalidValue);
            }
            put_u32(out, setup.sample_rate);
            put_u32(out, setup.quantum);
            out.push(setup.sample_format as u8);
            out.push(u8::from(setup.offline));
            for buses in [&setup.inputs, &setup.outputs] {
                out.push(buses.len() as u8);
                for bus in buses {
                    put_u16(out, bus.layout.channels);
                    put_u64(out, bus.layout.arrangement);
                    out.push(u8::from(bus.active));
                }
            }
        }
        Message::Features(caps) => {
            out.push(u8::from(caps.f64));
            put_u16(out, caps.input_event_buses);
            put_u16(out, caps.output_event_buses);
            out.push(u8::from(caps.editor));
            out.push(u8::from(caps.sysex));
            out.push(u8::from(caps.sample_accurate_parameters));
        }
        Message::ListParameters { start, count } => {
            if *count == 0 || *count > 32 {
                return Err(CodecError::InvalidValue);
            }
            put_u32(out, *start);
            put_u16(out, *count);
        }
        Message::SetParameter { id, value } => {
            if !value.is_finite() || !(0.0..=1.0).contains(value) {
                return Err(CodecError::InvalidValue);
            }
            put_u32(out, *id);
            put_u64(out, value.to_bits());
        }
        Message::Parameters { total, entries } => {
            if entries.len() > 32 || *total > 16384 {
                return Err(CodecError::InvalidValue);
            }
            put_u32(out, *total);
            put_u16(out, entries.len() as u16);
            for entry in entries {
                put_u32(out, entry.id);
                out.push(
                    u8::from(entry.automatable)
                        | (u8::from(entry.read_only) << 1)
                        | (u8::from(entry.hidden) << 2),
                );
                put_string(out, &entry.name)?;
                put_string(out, &entry.unit)?;
                put_u64(out, entry.value.to_bits());
                out.push(u8::from(entry.default.is_some()));
                if let Some(value) = entry.default {
                    put_u64(out, value.to_bits());
                }
                out.push(u8::from(entry.steps.is_some()));
                if let Some(steps) = entry.steps {
                    put_u32(out, steps);
                }
            }
        }
        Message::RequestError { code, text } | Message::Fault { code, text } => {
            put_u32(out, *code);
            put_string(out, text)?;
        }
    }
    Ok(())
}

fn decode_message(kind: u16, body: &[u8]) -> Result<Option<Message>, CodecError> {
    let mut cursor = Cursor::new(body);
    let message = match kind {
        1 => {
            let magic = cursor.array::<4>()?;
            let major = cursor.u16()?;
            let minor = cursor.u16()?;
            if magic != MAGIC {
                return Err(CodecError::WrongMagic);
            }
            if major != PROTOCOL_MAJOR {
                return Err(CodecError::WrongMajor {
                    got: major,
                    expected: PROTOCOL_MAJOR,
                });
            }
            Message::Hello {
                magic,
                major,
                minor,
            }
        }
        2 => {
            let path = cursor.string()?;
            let format = match cursor.u16()? {
                2 => PluginFormat::Vst2,
                3 => PluginFormat::Vst3,
                _ => return Err(CodecError::InvalidValue),
            };
            let class_id = match cursor.u8()? {
                0 => None,
                1 => Some(cursor.array::<16>()?),
                _ => return Err(CodecError::InvalidValue),
            };
            Message::Load {
                path,
                format,
                class_id,
            }
        }
        3 => Message::Configure {
            rate: cursor.u32()?,
            max_frames: cursor.u32()?,
            in_channels: cursor.u16()?,
            out_channels: cursor.u16()?,
        },
        4 => Message::Activate,
        5 => Message::SetState {
            blob: body.to_vec(),
        },
        6 => Message::GetState,
        7 => Message::OpenEditor,
        8 => Message::Shutdown,
        9 => Message::Scan {
            path: cursor.string()?,
        },
        10 => Message::Loaded {
            name: cursor.string()?,
            vendor: cursor.string()?,
            latency_frames: cursor.u32()?,
            inputs: cursor.bus_infos()?,
            outputs: cursor.bus_infos()?,
        },
        11 => {
            let count = cursor.u16()? as usize;
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                entries.push(ClassInfo {
                    id: cursor.array::<16>()?,
                    name: cursor.string()?,
                    vendor: cursor.string()?,
                    category: cursor.string()?,
                });
            }
            Message::Classes { entries }
        }
        12 => Message::LatencyChanged {
            frames: cursor.u32()?,
        },
        13 => Message::State {
            blob: body.to_vec(),
        },
        14 => Message::ParamsChanged,
        15 => Message::Fault {
            code: cursor.u32()?,
            text: cursor.string()?,
        },
        16 => Message::Ack,
        17 => Message::EditorClosed,
        18 => Message::CloseEditor,
        26 => Message::Editor(match cursor.u8()? {
            0 => EditorAction::Show,
            1 => EditorAction::Hide,
            2 => EditorAction::Focus,
            3 => EditorAction::Resize {
                width: cursor.u32()?,
                height: cursor.u32()?,
            },
            4 => EditorAction::Scale(f32::from_bits(cursor.u32()?)),
            5 => EditorAction::Inspect,
            6 => EditorAction::Capture,
            _ => return Err(CodecError::InvalidValue),
        }),
        27 => Message::EditorStatus(EditorStatus {
            visible: cursor.boolean()?,
            width: cursor.u32()?,
            height: cursor.u32()?,
            scale: f32::from_bits(cursor.u32()?),
            resize_supported: cursor.boolean()?,
            scale_supported: cursor.boolean()?,
        }),
        28 => {
            let width = cursor.u32()?;
            let height = cursor.u32()?;
            if width == 0
                || height == 0
                || width > 2048
                || height > 2048
                || cursor.remaining() != width as usize * height as usize * 4
            {
                return Err(CodecError::InvalidValue);
            }
            let rgba = cursor.take(cursor.remaining())?.to_vec();
            Message::EditorImage(EditorImage {
                width,
                height,
                rgba,
            })
        }
        23 => {
            let sample_rate = cursor.u32()?;
            let quantum = cursor.u32()?;
            let sample_format = match cursor.u8()? {
                0 => SampleFormat::F32,
                1 => SampleFormat::F64,
                _ => return Err(CodecError::InvalidValue),
            };
            let offline = cursor.boolean()?;
            let mut lists = Vec::new();
            for _ in 0..2 {
                let count = cursor.u8()?;
                if count > 32 {
                    return Err(CodecError::InvalidValue);
                }
                let mut buses = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    buses.push(BusConfiguration {
                        layout: Bus {
                            channels: cursor.u16()?,
                            arrangement: cursor.u64()?,
                        },
                        active: cursor.boolean()?,
                    });
                }
                lists.push(buses);
            }
            let outputs = lists.pop().expect("two lists");
            let inputs = lists.pop().expect("two lists");
            let setup = ProcessingSetup {
                sample_rate,
                quantum,
                sample_format,
                offline,
                inputs,
                outputs,
            };
            if !setup.valid() {
                return Err(CodecError::InvalidValue);
            }
            Message::Prepare(setup)
        }
        24 => Message::Describe,
        25 => Message::Features(Capabilities {
            f64: cursor.boolean()?,
            input_event_buses: cursor.u16()?,
            output_event_buses: cursor.u16()?,
            editor: cursor.boolean()?,
            sysex: cursor.boolean()?,
            sample_accurate_parameters: cursor.boolean()?,
        }),
        19 => {
            let start = cursor.u32()?;
            let count = cursor.u16()?;
            if count == 0 || count > 32 {
                return Err(CodecError::InvalidValue);
            }
            Message::ListParameters { start, count }
        }
        20 => {
            let total = cursor.u32()?;
            let count = cursor.u16()?;
            if total > 16384 || count > 32 {
                return Err(CodecError::InvalidValue);
            }
            let mut entries = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let id = cursor.u32()?;
                let flags = cursor.u8()?;
                if flags & !7 != 0 {
                    return Err(CodecError::InvalidValue);
                }
                let name = cursor.string()?;
                let unit = cursor.string()?;
                let value = cursor.normalized()?;
                let default = match cursor.u8()? {
                    0 => None,
                    1 => Some(cursor.normalized()?),
                    _ => return Err(CodecError::InvalidValue),
                };
                let steps = match cursor.u8()? {
                    0 => None,
                    1 => Some(cursor.u32()?),
                    _ => return Err(CodecError::InvalidValue),
                };
                entries.push(Parameter {
                    automatable: flags & 1 != 0,
                    read_only: flags & 2 != 0,
                    hidden: flags & 4 != 0,
                    id,
                    name,
                    unit,
                    value,
                    default,
                    steps,
                });
            }
            Message::Parameters { total, entries }
        }
        21 => Message::SetParameter {
            id: cursor.u32()?,
            value: cursor.normalized()?,
        },
        22 => Message::RequestError {
            code: cursor.u32()?,
            text: cursor.string()?,
        },
        _ => return Ok(None),
    };
    if !matches!(kind, 5 | 13) && cursor.remaining() != 0 {
        return Err(CodecError::TrailingBytes);
    }
    Ok(Some(message))
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}
fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}
fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_string(out: &mut Vec<u8>, value: &str) -> Result<(), CodecError> {
    if value.len() > MAX_STRING_BYTES {
        return Err(CodecError::StringTooLong);
    }
    put_u16(out, value.len() as u16);
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

fn put_bus_infos(out: &mut Vec<u8>, buses: &[BusInfo]) -> Result<(), CodecError> {
    put_u16(
        out,
        u16::try_from(buses.len()).map_err(|_| CodecError::InvalidValue)?,
    );
    for bus in buses {
        put_u16(out, bus.layout.channels);
        put_u64(out, bus.layout.arrangement);
        out.push(match bus.kind {
            BusKind::Main => 0,
            BusKind::Aux => 1,
        });
        out.push(u8::from(bus.default_active));
        put_string(out, &bus.name)?;
    }
    Ok(())
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }
    fn take(&mut self, count: usize) -> Result<&'a [u8], CodecError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or(CodecError::Truncated)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(CodecError::Truncated)?;
        self.position = end;
        Ok(value)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], CodecError> {
        Ok(self.take(N)?.try_into().expect("exact length"))
    }
    fn boolean(&mut self) -> Result<bool, CodecError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(CodecError::InvalidValue),
        }
    }
    fn u8(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, CodecError> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32, CodecError> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, CodecError> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn normalized(&mut self) -> Result<f64, CodecError> {
        let value = f64::from_bits(self.u64()?);
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(value)
        } else {
            Err(CodecError::InvalidValue)
        }
    }
    fn string(&mut self) -> Result<String, CodecError> {
        let length = self.u16()? as usize;
        if length > MAX_STRING_BYTES {
            return Err(CodecError::StringTooLong);
        }
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| CodecError::InvalidUtf8)
    }
    fn bus_infos(&mut self) -> Result<Vec<BusInfo>, CodecError> {
        let count = self.u16()? as usize;
        let mut buses = Vec::with_capacity(count);
        for _ in 0..count {
            let layout = Bus {
                channels: self.u16()?,
                arrangement: self.u64()?,
            };
            let kind = match self.u8()? {
                0 => BusKind::Main,
                1 => BusKind::Aux,
                _ => return Err(CodecError::InvalidValue),
            };
            let flags = self.u8()?;
            if flags & !1 != 0 {
                return Err(CodecError::InvalidValue);
            }
            buses.push(BusInfo {
                layout,
                kind,
                name: self.string()?,
                default_active: flags & 1 != 0,
            });
        }
        Ok(buses)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatchState {
    Starting,
    Loading,
    Configuring,
    Running,
    Stopping,
    /// Stopped on request; it is never restarted.
    Stopped,
    Backoff,
    Disabled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KillReason {
    HelloTimeout,
    LoadTimeout,
    ConfigureTimeout,
    HeartbeatTimeout,
    RequestTimeout,
    StopTimeout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisableReason {
    ThreeFailuresInTenMinutes,
}

/// What the caller does next. `Kill`, a crash's `EnterFallback` and `Disable`
/// all leave the engine on the fallback path until a later `LeaveFallback`.
/// `Disable` also ends the process if it is still alive (the third failure can
/// be a hang).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Kill { reason: KillReason },
    Spawn,
    Disable { reason: DisableReason },
    EnterFallback,
    LeaveFallback,
    SaveStateNow,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Received {
    Hello,
    Loaded,
    Configured,
    Response { request: u32 },
    ParamsChanged,
    EditorClosed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestKind {
    StateSave,
    Editor,
}

const STARTING_MS: u64 = 5_000;
const LOADING_MS: u64 = 60_000;
const CONFIGURING_MS: u64 = 10_000;
const HEARTBEAT_MS: u64 = 500;
/// A longer gap between two heartbeat observations means the host itself (or
/// the whole machine) was not running, so it is not counted as silence.
const OBSERVER_GAP_MS: u64 = HEARTBEAT_MS / 2;
const REQUEST_MS: u64 = 10_000;
const STOPPING_MS: u64 = 2_000;
const PARAM_QUIET_MS: u64 = 2_000;
const FAILURE_WINDOW_MS: u64 = 10 * 60 * 1_000;
const BACKOFF_MS: [u64; 3] = [500, 2_000, 8_000];

#[derive(Debug)]
pub struct HostWatch {
    state: WatchState,
    entered_at: u64,
    heartbeat_value: u64,
    heartbeat_at: u64,
    observed_at: u64,
    pending_requests: Vec<(u32, u64)>,
    params_changed_at: Option<u64>,
    missed_blocks: u8,
    fallback: bool,
    failures: VecDeque<u64>,
    backoff_attempt: usize,
    spawn_at: Option<u64>,
}

impl HostWatch {
    pub fn new(now_ms: u64) -> Self {
        Self {
            state: WatchState::Starting,
            entered_at: now_ms,
            heartbeat_value: 0,
            heartbeat_at: now_ms,
            observed_at: now_ms,
            pending_requests: Vec::new(),
            params_changed_at: None,
            missed_blocks: 0,
            fallback: false,
            failures: VecDeque::with_capacity(3),
            backoff_attempt: 0,
            spawn_at: None,
        }
    }

    pub fn state(&self) -> WatchState {
        self.state
    }

    pub fn received(&mut self, now_ms: u64, message: Received) -> Action {
        match message {
            Received::Hello if self.state == WatchState::Starting => {
                self.enter(WatchState::Loading, now_ms)
            }
            Received::Loaded if self.state == WatchState::Loading => {
                self.enter(WatchState::Configuring, now_ms)
            }
            Received::Configured if self.state == WatchState::Configuring => {
                self.enter(WatchState::Running, now_ms);
                self.heartbeat_at = now_ms;
                self.observed_at = now_ms;
                return Action::SaveStateNow;
            }
            Received::Response { request } => {
                self.pending_requests.retain(|(id, _)| *id != request);
            }
            Received::ParamsChanged => self.params_changed_at = Some(now_ms),
            Received::EditorClosed => return Action::SaveStateNow,
            _ => {}
        }
        Action::None
    }

    /// Silence is counted only across time the host was observing: when the
    /// host polls late, the worker may have been frozen together with it
    /// (a stalled machine), which is no evidence that the worker stopped.
    pub fn heartbeat(&mut self, now_ms: u64, value: u64) -> Action {
        let gap = now_ms.saturating_sub(self.observed_at);
        if gap > OBSERVER_GAP_MS {
            self.heartbeat_at = self.heartbeat_at.saturating_add(gap).min(now_ms);
        }
        self.observed_at = now_ms;
        if value != self.heartbeat_value {
            self.heartbeat_value = value;
            self.heartbeat_at = now_ms;
        }
        Action::None
    }

    pub fn request_sent(&mut self, now_ms: u64, request: u32, _kind: RequestKind) -> Action {
        self.pending_requests.retain(|(id, _)| *id != request);
        self.pending_requests.push((request, now_ms));
        Action::None
    }

    pub fn block_delivered(&mut self) -> Action {
        self.missed_blocks = 0;
        if self.fallback {
            self.fallback = false;
            Action::LeaveFallback
        } else {
            Action::None
        }
    }

    pub fn block_missed(&mut self) -> Action {
        self.missed_blocks = self.missed_blocks.saturating_add(1);
        if self.missed_blocks == 3 && !self.fallback {
            self.fallback = true;
            Action::EnterFallback
        } else {
            Action::None
        }
    }

    pub fn shutdown(&mut self, now_ms: u64) -> Action {
        self.enter(WatchState::Stopping, now_ms);
        Action::SaveStateNow
    }

    pub fn process_exited(&mut self, now_ms: u64, _exit_code: i32) -> Action {
        match self.state {
            WatchState::Backoff | WatchState::Disabled | WatchState::Stopped => Action::None,
            WatchState::Stopping => {
                self.enter(WatchState::Stopped, now_ms);
                Action::None
            }
            _ => self.failure(now_ms, None),
        }
    }

    pub fn tick(&mut self, now_ms: u64) -> Action {
        if self.state == WatchState::Backoff {
            if self.spawn_at.is_some_and(|deadline| now_ms >= deadline) {
                self.enter(WatchState::Starting, now_ms);
                self.spawn_at = None;
                return Action::Spawn;
            }
            return Action::None;
        }
        if matches!(self.state, WatchState::Disabled | WatchState::Stopped) {
            return Action::None;
        }
        if self.state == WatchState::Running {
            if now_ms.saturating_sub(self.heartbeat_at) >= HEARTBEAT_MS {
                return self.failure(now_ms, Some(KillReason::HeartbeatTimeout));
            }
            if self
                .pending_requests
                .iter()
                .any(|(_, sent)| now_ms.saturating_sub(*sent) >= REQUEST_MS)
            {
                return self.failure(now_ms, Some(KillReason::RequestTimeout));
            }
            if self
                .params_changed_at
                .is_some_and(|changed| now_ms.saturating_sub(changed) >= PARAM_QUIET_MS)
            {
                self.params_changed_at = None;
                return Action::SaveStateNow;
            }
            return Action::None;
        }
        let (deadline, reason) = match self.state {
            WatchState::Starting => (STARTING_MS, KillReason::HelloTimeout),
            WatchState::Loading => (LOADING_MS, KillReason::LoadTimeout),
            WatchState::Configuring => (CONFIGURING_MS, KillReason::ConfigureTimeout),
            WatchState::Stopping => {
                // A host that will not stop is killed; it was leaving anyway,
                // so it is not counted as a failure and is not restarted.
                if now_ms.saturating_sub(self.entered_at) >= STOPPING_MS {
                    self.enter(WatchState::Stopped, now_ms);
                    return Action::Kill {
                        reason: KillReason::StopTimeout,
                    };
                }
                return Action::None;
            }
            _ => return Action::None,
        };
        if now_ms.saturating_sub(self.entered_at) >= deadline {
            self.failure(now_ms, Some(reason))
        } else {
            Action::None
        }
    }

    fn enter(&mut self, state: WatchState, now_ms: u64) {
        self.state = state;
        self.entered_at = now_ms;
    }

    fn failure(&mut self, now_ms: u64, kill: Option<KillReason>) -> Action {
        while self
            .failures
            .front()
            .is_some_and(|time| now_ms.saturating_sub(*time) > FAILURE_WINDOW_MS)
        {
            self.failures.pop_front();
        }
        self.failures.push_back(now_ms);
        if self.failures.len() >= 3 {
            self.state = WatchState::Disabled;
            self.pending_requests.clear();
            self.fallback = true;
            return Action::Disable {
                reason: DisableReason::ThreeFailuresInTenMinutes,
            };
        }
        // The wait grows with each failure and starts again at 0.5 s once no
        // other failure is left inside the window.
        if self.failures.len() == 1 {
            self.backoff_attempt = 0;
        }
        let delay = BACKOFF_MS[self.backoff_attempt.min(BACKOFF_MS.len() - 1)];
        self.backoff_attempt = (self.backoff_attempt + 1).min(BACKOFF_MS.len() - 1);
        self.state = WatchState::Backoff;
        self.entered_at = now_ms;
        self.spawn_at = Some(now_ms.saturating_add(delay));
        self.pending_requests.clear();
        self.params_changed_at = None;
        self.missed_blocks = 0;
        self.fallback = true;
        if let Some(reason) = kill {
            Action::Kill { reason }
        } else {
            Action::EnterFallback
        }
    }
}

/// Two committed state blobs. A failed restore of the newest blob selects the
/// previous blob without destroying either copy.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StateHistory {
    current: Option<Vec<u8>>,
    previous: Option<Vec<u8>>,
    restoring_current: bool,
}

impl StateHistory {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn save(&mut self, blob: Vec<u8>) {
        if self.current.as_ref() == Some(&blob) {
            return;
        }
        self.previous = self.current.replace(blob);
        self.restoring_current = false;
    }
    pub fn begin_restore(&mut self) -> Option<&[u8]> {
        self.restoring_current = self.current.is_some();
        self.current.as_deref()
    }
    pub fn restore_succeeded(&mut self) {
        self.restoring_current = false;
    }
    pub fn crash_after_restore(&mut self) -> Option<&[u8]> {
        if self.restoring_current {
            self.restoring_current = false;
            self.previous.as_deref()
        } else {
            self.current.as_deref()
        }
    }
    pub fn current(&self) -> Option<&[u8]> {
        self.current.as_deref()
    }
    pub fn previous(&self) -> Option<&[u8]> {
        self.previous.as_deref()
    }
}

pub fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
