//! Black-box EqualizerAPO-XT comparison. No engine implementation is linked here.
const RATE: usize = 48_000;
const QUANTUM: usize = 512;
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    for part in bytes.chunks(3) {
        let n = (u32::from(part[0]) << 16)
            | (u32::from(*part.get(1).unwrap_or(&0)) << 8)
            | u32::from(*part.get(2).unwrap_or(&0));
        for i in 0..4 {
            out.push(if i > part.len() {
                '='
            } else {
                ALPHABET[((n >> (18 - i * 6)) & 63) as usize] as char
            });
        }
    }
    out
}
fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(4) {
        return Err("base64 length".into());
    }
    let mut out = Vec::new();
    for (index, part) in text.as_bytes().as_chunks::<4>().0.iter().enumerate() {
        let padding = part.iter().rev().take_while(|&&c| c == b'=').count();
        if padding > 2 || (padding > 0 && (index + 1) * 4 != text.len()) {
            return Err("base64 padding".into());
        }
        let mut n = 0u32;
        for (i, &c) in part.iter().enumerate() {
            let digit = if i >= 4 - padding {
                0
            } else {
                ALPHABET
                    .iter()
                    .position(|&a| a == c)
                    .ok_or("base64 alphabet")? as u32
            };
            n = (n << 6) | digit;
        }
        if (padding == 2 && n & 0xffff != 0) || (padding == 1 && n & 0xff != 0) {
            return Err("base64 padding bits".into());
        }
        out.extend_from_slice(&n.to_be_bytes()[1..4 - padding]);
    }
    Ok(out)
}
#[derive(Debug, PartialEq)]
struct PluginLine {
    raw: String,
    library: String,
    chunk: Option<Vec<u8>>,
    parameters: Vec<(String, f64)>,
}
fn parse_line(line: &str) -> Result<PluginLine, String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in line.trim().chars() {
        if c == '"' {
            quoted = !quoted;
            started = true;
        } else if c.is_whitespace() && !quoted {
            if started {
                tokens.push(std::mem::take(&mut token));
                started = false;
            }
        } else {
            token.push(c);
            started = true;
        }
    }
    if quoted {
        return Err("unclosed quote".into());
    }
    if started {
        tokens.push(token);
    }
    if tokens.len() < 3
        || tokens[0] != "VSTPlugin:"
        || tokens[1] != "Library"
        || !(tokens.len() - 3).is_multiple_of(2)
    {
        return Err("invalid VSTPlugin line".into());
    }
    let mut result = PluginLine {
        raw: line.trim().into(),
        library: tokens[2].clone(),
        chunk: None,
        parameters: Vec::new(),
    };
    for pair in tokens[3..].as_chunks::<2>().0 {
        if pair[0] == "ChunkData" {
            if result.chunk.is_some() {
                return Err("duplicate ChunkData".into());
            }
            result.chunk = Some(base64_decode(&pair[1])?);
        } else {
            let value: f64 = pair[1].parse().map_err(|_| "invalid parameter value")?;
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err("non-normalized parameter".into());
            }
            if result.parameters.iter().any(|(name, _)| name == &pair[0]) {
                return Err("duplicate parameter".into());
            }
            result.parameters.push((pair[0].clone(), value));
        }
    }
    Ok(result)
}
fn float_wav(channels: &[&[f32]]) -> Vec<u8> {
    assert!(!channels.is_empty());
    let frames = channels[0].len();
    assert!(
        channels
            .iter()
            .all(|c| c.len() == frames && c.iter().all(|v| v.is_finite()))
    );
    let count = u16::try_from(channels.len()).unwrap();
    let size = u32::try_from(frames * channels.len() * 4).unwrap();
    let mut bytes = Vec::with_capacity(size as usize + 56);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(size + 48).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&3u16.to_le_bytes());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&(RATE as u32).to_le_bytes());
    bytes.extend_from_slice(&(RATE as u32 * u32::from(count) * 4).to_le_bytes());
    bytes.extend_from_slice(&(count * 4).to_le_bytes());
    bytes.extend_from_slice(&32u16.to_le_bytes());
    bytes.extend_from_slice(b"fact");
    bytes.extend_from_slice(&4u32.to_le_bytes());
    bytes.extend_from_slice(&(frames as u32).to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&size.to_le_bytes());
    for frame in 0..frames {
        for channel in channels {
            bytes.extend_from_slice(&channel[frame].to_le_bytes());
        }
    }
    bytes
}
fn pcm16_wav(bytes: &[u8], channels: usize, frames: usize) -> Result<Vec<Vec<i16>>, String> {
    let u16_at = |p: usize| u16::from_le_bytes(bytes[p..p + 2].try_into().unwrap());
    let u32_at = |p: usize| u32::from_le_bytes(bytes[p..p + 4].try_into().unwrap()) as usize;
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not RIFF WAVE".into());
    }
    let end = u32_at(4).checked_add(8).ok_or("RIFF size overflow")?;
    if end != bytes.len() {
        return Err("RIFF size mismatch".into());
    }
    let mut fmt = false;
    let mut data = None;
    let mut at = 12;
    while at < end {
        if end - at < 8 {
            return Err("truncated chunk header".into());
        }
        let size = u32_at(at + 4);
        let start = at + 8;
        let stop = start
            .checked_add(size)
            .filter(|&s| s <= end)
            .ok_or("truncated chunk")?;
        match &bytes[at..at + 4] {
            b"fmt " => {
                if fmt
                    || size < 16
                    || u16_at(start) != 1
                    || usize::from(u16_at(start + 2)) != channels
                    || u32_at(start + 4) != RATE
                    || u16_at(start + 14) != 16
                    || usize::from(u16_at(start + 12)) != channels * 2
                    || u32_at(start + 8) != RATE * channels * 2
                {
                    return Err("expected 48 kHz PCM16 with requested channels".into());
                }
                fmt = true;
            }
            b"data" if data.replace(&bytes[start..stop]).is_some() => {
                return Err("duplicate data".into());
            }
            _ => {}
        }
        at = stop + (size & 1);
        if at > end {
            return Err("missing chunk padding".into());
        }
    }
    if !fmt {
        return Err("missing fmt".into());
    }
    let data = data.ok_or("missing data")?;
    if channels == 0 || data.len() != frames * channels * 2 {
        return Err("frame count mismatch".into());
    }
    let mut output = vec![Vec::with_capacity(frames); channels];
    for frame in data.chunks_exact(channels * 2) {
        for (channel, sample) in output.iter_mut().zip(frame.as_chunks::<2>().0) {
            channel.push(i16::from_le_bytes(*sample));
        }
    }
    Ok(output)
}

#[cfg(windows)]
mod native;
#[cfg(windows)]
pub use native::{Case, compare};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn base64_vectors_and_dpf_state() {
        for (raw, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
        ] {
            assert_eq!(base64_encode(raw.as_bytes()), encoded);
            assert_eq!(base64_decode(encoded).unwrap(), raw.as_bytes());
        }
        let state = b"__dpf_program__\0\x30\0__dpf_parameters_begin__\0low\0\x30\0__dpf_parameters_end__\0\xfe\0";
        assert_eq!(base64_decode(&base64_encode(state)).unwrap(), state);
        for invalid in ["x", "====", "Zg==AAAA", "Zh==", "Zm9=", "Zm?="] {
            assert!(base64_decode(invalid).is_err(), "{invalid}");
        }
    }
    #[test]
    fn float_wav_headers_and_interleaving() {
        let bytes = float_wav(&[&[0.25, -0.5], &[0.75, 0.0]]);
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize,
            bytes.len() - 8
        );
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(&bytes[16..24], &[16, 0, 0, 0, 3, 0, 2, 0]);
        assert_eq!(&bytes[24..28], &48_000u32.to_le_bytes());
        assert_eq!(&bytes[28..32], &384_000u32.to_le_bytes());
        assert_eq!(&bytes[32..36], &[8, 0, 32, 0]);
        assert_eq!(&bytes[36..48], b"fact\x04\0\0\0\x02\0\0\0");
        assert_eq!(&bytes[48..56], b"data\x10\0\0\0");
        let samples: Vec<_> = bytes[56..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|v| f32::from_le_bytes(*v))
            .collect();
        assert_eq!(samples, [0.25, 0.75, -0.5, 0.0]);
    }
    #[test]
    fn pcm16_chunks_and_validation() {
        let mut bytes = b"RIFF\0\0\0\0WAVEJUNK\x01\0\0\0x\0data\x08\0\0\0".to_vec();
        for sample in [-32768i16, 32767, -1, 0] {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes.extend_from_slice(b"fmt \x10\0\0\0\x01\0\x02\0");
        bytes.extend_from_slice(&48_000u32.to_le_bytes());
        bytes.extend_from_slice(&192_000u32.to_le_bytes());
        bytes.extend_from_slice(&[4, 0, 16, 0]);
        let size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        assert_eq!(
            pcm16_wav(&bytes, 2, 2).unwrap(),
            vec![vec![-32768, -1], vec![32767, 0]]
        );
        assert!(pcm16_wav(&bytes, 4, 1).is_err());
        assert!(pcm16_wav(&bytes, 2, 3).is_err());
        for end in 0..bytes.len() {
            assert!(pcm16_wav(&bytes[..end], 2, 2).is_err());
        }
    }
    #[test]
    fn saved_config_lines() {
        for path in [r#"D:\plugins\eq.dll"#, r#""D:\plugin files\eq.dll""#] {
            let line = format!(
                r#"VSTPlugin: Library {path} Master 0.5 Low 0.5 Mid 0.5 High 0.5 "Low-Mid Freq" 0.22 "Mid-High Freq" 0.0526316"#
            );
            let parsed = parse_line(&line).unwrap();
            assert_eq!(parsed.raw, line);
            assert_eq!(parsed.library, path.trim_matches('"'));
            assert_eq!(parsed.parameters.len(), 6);
            assert_eq!(parsed.parameters[4], ("Low-Mid Freq".into(), 0.22));
            assert!(parsed.chunk.is_none());
        }
        for path in [r#"D:\plugins\eq.vst3"#, r#""D:\plugin files\eq.vst3""#] {
            let state = b"__dpf_program__\0\x30\0__dpf_parameters_begin__\0low\0\x30\0__dpf_parameters_end__\0\xfe\0";
            let line = format!(
                r#"VSTPlugin: Library {path} ChunkData "{}""#,
                base64_encode(state)
            );
            let parsed = parse_line(&line).unwrap();
            assert_eq!(parsed.chunk.unwrap(), state);
            assert!(parsed.parameters.is_empty());
        }
        for line in [
            "VSTPlugin: Library",
            "VSTPlugin: Library x Low",
            "VSTPlugin: Library x Low NaN",
            "VSTPlugin: Library \"x",
        ] {
            assert!(parse_line(line).is_err());
        }
    }
}
