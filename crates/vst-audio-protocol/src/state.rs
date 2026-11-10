//! Versioned VST 3 state envelopes and stable comparison of plugin state blobs.
use crate::PluginError;
use std::ops::Range;

const MAGIC: &[u8; 5] = b"SSV3\x01";
const LIMIT: usize = crate::MAX_STATE_BODY_BYTES;
const EOCD_SIGNATURE: &[u8; 4] = b"PK\x05\x06";
const CENTRAL_SIGNATURE: &[u8; 4] = b"PK\x01\x02";
const LOCAL_SIGNATURE: &[u8; 4] = b"PK\x03\x04";
// Bound signature scanning work for state supplied by an untrusted plugin.
const MAX_EOCD_CANDIDATES: usize = 64;
// Bound central-directory walking work for state supplied by an untrusted plugin.
const MAX_CENTRAL_ENTRIES: usize = 1024;

pub fn encode(component: &[u8], controller: &[u8]) -> Result<Vec<u8>, PluginError> {
    if component
        .len()
        .saturating_add(controller.len())
        .saturating_add(13)
        > LIMIT
    {
        return Err(PluginError("VST 3 state too large".into()));
    }
    let mut blob = Vec::with_capacity(13 + component.len() + controller.len());
    blob.extend_from_slice(MAGIC);
    for bytes in [component, controller] {
        blob.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        blob.extend_from_slice(bytes);
    }
    Ok(blob)
}

pub fn decode(blob: &[u8]) -> Result<(&[u8], &[u8]), PluginError> {
    let invalid = || PluginError("invalid VST 3 state magic, version or framing".into());
    if blob.len() > LIMIT || !blob.starts_with(MAGIC) {
        return Err(invalid());
    }
    let mut rest = &blob[MAGIC.len()..];
    fn part<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
        let length = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?) as usize;
        *rest = &rest[4..];
        let bytes = rest.get(..length)?;
        *rest = &rest[length..];
        Some(bytes)
    }
    let component = part(&mut rest).ok_or_else(invalid)?;
    let controller = part(&mut rest).ok_or_else(invalid)?;
    if !rest.is_empty() {
        return Err(invalid());
    }
    Ok((component, controller))
}

/// Compares plugin state while tolerating archive timestamps that change on read.
///
/// Two reads of an unchanged plugin can differ in the modification times of
/// verified ZIP archives inside the blob, including archives inside a VST 3
/// envelope. Those times are ignored. Anything that cannot be verified as part
/// of a complete ZIP archive compares as different.
pub fn equivalent(a: &[u8], b: &[u8]) -> bool {
    if a == b {
        return true;
    }
    if a.len() != b.len() {
        return false;
    }

    let a_ranges = volatile_ranges(a);
    let b_ranges = volatile_ranges(b);
    if a_ranges.is_empty() || b_ranges.is_empty() || a_ranges != b_ranges {
        return false;
    }

    let mut cursor = 0;
    for range in &a_ranges {
        let Some(a_bytes) = a.get(cursor..range.start) else {
            return false;
        };
        let Some(b_bytes) = b.get(cursor..range.start) else {
            return false;
        };
        if a_bytes != b_bytes {
            return false;
        }
        cursor = range.end;
    }

    matches!(
        (a.get(cursor..), b.get(cursor..)),
        (Some(a_bytes), Some(b_bytes)) if a_bytes == b_bytes
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArchiveError {
    Invalid,
    EntryBudgetExceeded,
}

fn volatile_ranges(blob: &[u8]) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut candidates = 0;
    let mut walked_entries = 0;

    for (eocd, window) in blob.windows(EOCD_SIGNATURE.len()).enumerate() {
        if window != EOCD_SIGNATURE {
            continue;
        }
        if candidates == MAX_EOCD_CANDIDATES {
            return Vec::new();
        }
        candidates += 1;

        match archive_ranges(blob, eocd, &mut walked_entries) {
            Ok(mut archive) => ranges.append(&mut archive),
            Err(ArchiveError::Invalid) => {}
            Err(ArchiveError::EntryBudgetExceeded) => return Vec::new(),
        }
    }

    ranges.sort_unstable_by_key(|range| (range.start, range.end));
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if range.is_empty() {
            continue;
        }
        if let Some(previous) = merged.last_mut()
            && range.start <= previous.end
        {
            previous.end = previous.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    merged
}

fn archive_ranges(
    blob: &[u8],
    eocd: usize,
    walked_entries: &mut usize,
) -> Result<Vec<Range<usize>>, ArchiveError> {
    let fixed_end = eocd.checked_add(22).ok_or(ArchiveError::Invalid)?;
    blob.get(eocd..fixed_end).ok_or(ArchiveError::Invalid)?;

    let disk = read_u16(blob, add(eocd, 4)?)?;
    let central_disk = read_u16(blob, add(eocd, 6)?)?;
    let entries_on_disk = read_u16(blob, add(eocd, 8)?)?;
    let entries = read_u16(blob, add(eocd, 10)?)?;
    let central_size = read_u32(blob, add(eocd, 12)?)?;
    let central_offset = read_u32(blob, add(eocd, 16)?)?;
    let comment_len = usize::from(read_u16(blob, add(eocd, 20)?)?);

    if disk != 0
        || central_disk != 0
        || entries_on_disk != entries
        || entries == 0
        || entries_on_disk == u16::MAX
        || entries == u16::MAX
        || central_size == u32::MAX
        || central_offset == u32::MAX
    {
        return Err(ArchiveError::Invalid);
    }
    let archive_end = fixed_end
        .checked_add(comment_len)
        .ok_or(ArchiveError::Invalid)?;
    blob.get(eocd..archive_end).ok_or(ArchiveError::Invalid)?;

    let central_start = eocd
        .checked_sub(central_size as usize)
        .ok_or(ArchiveError::Invalid)?;
    let base = central_start
        .checked_sub(central_offset as usize)
        .ok_or(ArchiveError::Invalid)?;
    let mut position = central_start;
    let mut ranges = Vec::new();

    for _ in 0..entries {
        if *walked_entries == MAX_CENTRAL_ENTRIES {
            return Err(ArchiveError::EntryBudgetExceeded);
        }
        *walked_entries += 1;

        if !has_signature(blob, position, CENTRAL_SIGNATURE) {
            return Err(ArchiveError::Invalid);
        }
        let fixed_entry_end = position.checked_add(46).ok_or(ArchiveError::Invalid)?;
        blob.get(position..fixed_entry_end)
            .ok_or(ArchiveError::Invalid)?;

        let central_time_start = position.checked_add(12).ok_or(ArchiveError::Invalid)?;
        let central_time_end = position.checked_add(16).ok_or(ArchiveError::Invalid)?;
        let name_len = usize::from(read_u16(blob, add(position, 28)?)?);
        let extra_len = usize::from(read_u16(blob, add(position, 30)?)?);
        let entry_comment_len = usize::from(read_u16(blob, add(position, 32)?)?);
        let local_offset = read_u32(blob, add(position, 42)?)?;
        if local_offset == u32::MAX {
            return Err(ArchiveError::Invalid);
        }

        let name_start = fixed_entry_end;
        let name_end = name_start
            .checked_add(name_len)
            .ok_or(ArchiveError::Invalid)?;
        let extra_end = name_end
            .checked_add(extra_len)
            .ok_or(ArchiveError::Invalid)?;
        let entry_end = extra_end
            .checked_add(entry_comment_len)
            .ok_or(ArchiveError::Invalid)?;
        if entry_end > eocd {
            return Err(ArchiveError::Invalid);
        }
        let central_name = blob
            .get(name_start..name_end)
            .ok_or(ArchiveError::Invalid)?;
        parse_extra_fields(blob, name_end..extra_end, &mut ranges).ok_or(ArchiveError::Invalid)?;

        let local = base
            .checked_add(local_offset as usize)
            .ok_or(ArchiveError::Invalid)?;
        if !has_signature(blob, local, LOCAL_SIGNATURE) {
            return Err(ArchiveError::Invalid);
        }
        let fixed_local_end = local.checked_add(30).ok_or(ArchiveError::Invalid)?;
        blob.get(local..fixed_local_end)
            .ok_or(ArchiveError::Invalid)?;

        let local_time_start = local.checked_add(10).ok_or(ArchiveError::Invalid)?;
        let local_time_end = local.checked_add(14).ok_or(ArchiveError::Invalid)?;
        let local_name_len = usize::from(read_u16(blob, add(local, 26)?)?);
        let local_extra_len = usize::from(read_u16(blob, add(local, 28)?)?);
        let local_name_end = fixed_local_end
            .checked_add(local_name_len)
            .ok_or(ArchiveError::Invalid)?;
        let local_extra_end = local_name_end
            .checked_add(local_extra_len)
            .ok_or(ArchiveError::Invalid)?;
        if local_extra_end > central_start {
            return Err(ArchiveError::Invalid);
        }
        let local_name = blob
            .get(fixed_local_end..local_name_end)
            .ok_or(ArchiveError::Invalid)?;
        if local_name != central_name {
            return Err(ArchiveError::Invalid);
        }
        parse_extra_fields(blob, local_name_end..local_extra_end, &mut ranges)
            .ok_or(ArchiveError::Invalid)?;

        ranges.push(central_time_start..central_time_end);
        ranges.push(local_time_start..local_time_end);
        position = entry_end;
    }

    if position != eocd {
        return Err(ArchiveError::Invalid);
    }
    Ok(ranges)
}

fn add(value: usize, offset: usize) -> Result<usize, ArchiveError> {
    value.checked_add(offset).ok_or(ArchiveError::Invalid)
}

fn read_u16(blob: &[u8], offset: usize) -> Result<u16, ArchiveError> {
    let end = offset.checked_add(2).ok_or(ArchiveError::Invalid)?;
    let bytes = blob.get(offset..end).ok_or(ArchiveError::Invalid)?;
    let bytes = <[u8; 2]>::try_from(bytes).map_err(|_| ArchiveError::Invalid)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32(blob: &[u8], offset: usize) -> Result<u32, ArchiveError> {
    let end = offset.checked_add(4).ok_or(ArchiveError::Invalid)?;
    let bytes = blob.get(offset..end).ok_or(ArchiveError::Invalid)?;
    let bytes = <[u8; 4]>::try_from(bytes).map_err(|_| ArchiveError::Invalid)?;
    Ok(u32::from_le_bytes(bytes))
}

fn has_signature(blob: &[u8], offset: usize, signature: &[u8]) -> bool {
    let Some(end) = offset.checked_add(signature.len()) else {
        return false;
    };
    blob.get(offset..end) == Some(signature)
}

fn parse_extra_fields(
    blob: &[u8],
    area: Range<usize>,
    ranges: &mut Vec<Range<usize>>,
) -> Option<()> {
    let mut position = area.start;
    while position < area.end {
        let header_end = position.checked_add(4)?;
        if header_end > area.end {
            return None;
        }
        let id = read_u16(blob, position).ok()?;
        let size = usize::from(read_u16(blob, position.checked_add(2)?).ok()?);
        let data_end = header_end.checked_add(size)?;
        if data_end > area.end || blob.get(header_end..data_end).is_none() {
            return None;
        }
        if matches!(id, 0x5455 | 0x000A | 0x5855) {
            ranges.push(header_end..data_end);
        }
        position = data_end;
    }
    (position == area.end).then_some(())
}

#[cfg(test)]
pub(crate) mod test_zip {
    #[derive(Clone, Copy, Debug)]
    pub(crate) struct Entry<'a> {
        pub(crate) name: &'a [u8],
        pub(crate) content: &'a [u8],
        pub(crate) local_extra: &'a [u8],
        pub(crate) central_extra: &'a [u8],
    }

    pub(crate) fn extra_field(id: u16, data: &[u8]) -> Vec<u8> {
        let mut field = Vec::with_capacity(4 + data.len());
        push_u16(&mut field, id);
        push_u16(
            &mut field,
            u16::try_from(data.len()).expect("test ZIP extra data fits in u16"),
        );
        field.extend_from_slice(data);
        field
    }

    pub(crate) fn build(
        entries: &[Entry<'_>],
        time: u16,
        date: u16,
        data_descriptor: bool,
    ) -> Vec<u8> {
        let mut blob = Vec::new();
        let mut local_offsets = Vec::with_capacity(entries.len());
        let flags = if data_descriptor { 1_u16 << 3 } else { 0 };
        let crc = 0x1234_5678;

        for entry in entries {
            local_offsets.push(u32::try_from(blob.len()).expect("test ZIP offset fits in u32"));
            let size = u32::try_from(entry.content.len()).expect("test ZIP content fits in u32");
            blob.extend_from_slice(b"PK\x03\x04");
            push_u16(&mut blob, 20);
            push_u16(&mut blob, flags);
            push_u16(&mut blob, 0);
            push_u16(&mut blob, time);
            push_u16(&mut blob, date);
            push_u32(&mut blob, if data_descriptor { 0 } else { crc });
            push_u32(&mut blob, if data_descriptor { 0 } else { size });
            push_u32(&mut blob, if data_descriptor { 0 } else { size });
            push_u16(
                &mut blob,
                u16::try_from(entry.name.len()).expect("test ZIP name fits in u16"),
            );
            push_u16(
                &mut blob,
                u16::try_from(entry.local_extra.len()).expect("test ZIP extra fits in u16"),
            );
            blob.extend_from_slice(entry.name);
            blob.extend_from_slice(entry.local_extra);
            blob.extend_from_slice(entry.content);
            if data_descriptor {
                blob.extend_from_slice(b"PK\x07\x08");
                push_u32(&mut blob, crc);
                push_u32(&mut blob, size);
                push_u32(&mut blob, size);
            }
        }

        let central_offset = u32::try_from(blob.len()).expect("test ZIP offset fits in u32");
        for (entry, local_offset) in entries.iter().zip(local_offsets) {
            let size = u32::try_from(entry.content.len()).expect("test ZIP content fits in u32");
            blob.extend_from_slice(b"PK\x01\x02");
            push_u16(&mut blob, 20);
            push_u16(&mut blob, 20);
            push_u16(&mut blob, flags);
            push_u16(&mut blob, 0);
            push_u16(&mut blob, time);
            push_u16(&mut blob, date);
            push_u32(&mut blob, crc);
            push_u32(&mut blob, size);
            push_u32(&mut blob, size);
            push_u16(
                &mut blob,
                u16::try_from(entry.name.len()).expect("test ZIP name fits in u16"),
            );
            push_u16(
                &mut blob,
                u16::try_from(entry.central_extra.len()).expect("test ZIP extra fits in u16"),
            );
            push_u16(&mut blob, 0);
            push_u16(&mut blob, 0);
            push_u16(&mut blob, 0);
            push_u32(&mut blob, 0);
            push_u32(&mut blob, local_offset);
            blob.extend_from_slice(entry.name);
            blob.extend_from_slice(entry.central_extra);
        }
        let central_size = u32::try_from(blob.len())
            .expect("test ZIP size fits in u32")
            .checked_sub(central_offset)
            .expect("central directory follows local entries");
        let entry_count = u16::try_from(entries.len()).expect("test ZIP entry count fits in u16");
        blob.extend_from_slice(b"PK\x05\x06");
        push_u16(&mut blob, 0);
        push_u16(&mut blob, 0);
        push_u16(&mut blob, entry_count);
        push_u16(&mut blob, entry_count);
        push_u32(&mut blob, central_size);
        push_u32(&mut blob, central_offset);
        push_u16(&mut blob, 0);
        blob
    }

    fn push_u16(blob: &mut Vec<u8>, value: u16) {
        blob.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u32(blob: &mut Vec<u8>, value: u32) {
        blob.extend_from_slice(&value.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::test_zip::{Entry, build, extra_field};
    use super::*;

    const TIME_1: u16 = 0x1020;
    const TIME_2: u16 = 0x3040;
    const DATE_1: u16 = 0x5060;
    const DATE_2: u16 = 0x7080;

    fn zip(time: u16, date: u16, data_descriptor: bool, content: &[u8]) -> Vec<u8> {
        build(
            &[Entry {
                name: b"state.json",
                content,
                local_extra: &[],
                central_extra: &[],
            }],
            time,
            date,
            data_descriptor,
        )
    }

    fn assert_both(expected: bool, a: &[u8], b: &[u8]) {
        assert_eq!(equivalent(a, b), expected);
        assert_eq!(equivalent(b, a), expected);
    }

    fn signature_offset(blob: &[u8], signature: &[u8]) -> usize {
        blob.windows(signature.len())
            .position(|window| window == signature)
            .expect("signature is present in test ZIP")
    }

    fn eocd_offset(blob: &[u8]) -> usize {
        blob.windows(EOCD_SIGNATURE.len())
            .rposition(|window| window == EOCD_SIGNATURE)
            .expect("EOCD is present in test ZIP")
    }

    fn write_u16(blob: &mut [u8], offset: usize, value: u16) {
        blob.get_mut(offset..offset + 2)
            .expect("field is present in test ZIP")
            .copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn vst3_state_framing_round_trip() {
        let blob = encode(&[1, 3], &[4, 5, 6]).unwrap();
        assert_eq!(decode(&blob).unwrap(), (&[1, 3][..], &[4, 5, 6][..]));
        for length in 0..blob.len() {
            assert!(decode(&blob[..length]).is_err());
        }
        assert!(decode(&[1, 3]).is_err());
        let mut corrupt = blob.clone();
        corrupt[4] = 2;
        assert!(decode(&corrupt).is_err());
        corrupt = blob.clone();
        corrupt.extend_from_slice(&[0]);
        assert!(decode(&corrupt).is_err());
        assert_eq!(
            decode(&encode(&[], &[]).unwrap()).unwrap(),
            (&[][..], &[][..])
        );
    }

    #[test]
    fn equivalent_ignores_dos_time_and_date_in_both_zip_layouts() {
        for descriptor in [false, true] {
            let original = zip(TIME_1, DATE_1, descriptor, b"content");
            let new_time = zip(TIME_2, DATE_1, descriptor, b"content");
            let new_time_and_date = zip(TIME_2, DATE_2, descriptor, b"content");
            assert_both(true, &original, &new_time);
            assert_both(true, &original, &new_time_and_date);
        }
    }

    #[test]
    fn equivalent_preserves_prefix_and_trailing_bytes() {
        let wrap = |archive: Vec<u8>, prefix: &[u8], trailing: &[u8]| {
            let mut blob = prefix.to_vec();
            blob.extend_from_slice(&archive);
            blob.extend_from_slice(trailing);
            blob
        };
        let a = wrap(
            zip(TIME_1, DATE_1, true, b"content"),
            b"arbitrary-prefix",
            b"trailing-data",
        );
        let b = wrap(
            zip(TIME_2, DATE_2, true, b"content"),
            b"arbitrary-prefix",
            b"trailing-data",
        );
        assert_both(true, &a, &b);

        let different_prefix = wrap(
            zip(TIME_2, DATE_2, true, b"content"),
            b"Arbitrary-prefix",
            b"trailing-data",
        );
        assert_both(false, &a, &different_prefix);
        let different_trailing = wrap(
            zip(TIME_2, DATE_2, true, b"content"),
            b"arbitrary-prefix",
            b"trailing-datA",
        );
        assert_both(false, &a, &different_trailing);
    }

    #[test]
    fn equivalent_finds_both_archives_inside_vst3_envelope() {
        let a = encode(
            &zip(TIME_1, DATE_1, true, b"component"),
            &zip(TIME_1, DATE_1, true, b"controller"),
        )
        .unwrap();
        let b = encode(
            &zip(TIME_2, DATE_2, true, b"component"),
            &zip(TIME_2, DATE_2, true, b"controller"),
        )
        .unwrap();
        assert_both(true, &a, &b);

        let changed_controller = encode(
            &zip(TIME_2, DATE_2, true, b"component"),
            &zip(TIME_2, DATE_2, true, b"controllex"),
        )
        .unwrap();
        assert_both(false, &a, &changed_controller);
    }

    #[test]
    fn equivalent_ignores_timestamps_in_every_entry() {
        let entries = [
            Entry {
                name: b"first",
                content: b"one",
                local_extra: &[],
                central_extra: &[],
            },
            Entry {
                name: b"second",
                content: b"two",
                local_extra: &[],
                central_extra: &[],
            },
        ];
        let a = build(&entries, TIME_1, DATE_1, false);
        let b = build(&entries, TIME_2, DATE_2, false);
        assert_both(true, &a, &b);
    }

    #[test]
    fn equivalent_ignores_known_timestamp_extra_fields_only() {
        for id in [0x5455, 0x000A, 0x5855] {
            let extra_a = extra_field(id, b"old-time");
            let extra_b = extra_field(id, b"new-time");
            let a = build(
                &[Entry {
                    name: b"state.json",
                    content: b"content",
                    local_extra: &extra_a,
                    central_extra: &extra_a,
                }],
                TIME_1,
                DATE_1,
                false,
            );
            let b = build(
                &[Entry {
                    name: b"state.json",
                    content: b"content",
                    local_extra: &extra_b,
                    central_extra: &extra_b,
                }],
                TIME_1,
                DATE_1,
                false,
            );
            assert_both(true, &a, &b);
        }

        let extra_a = extra_field(0x1234, b"old-time");
        let extra_b = extra_field(0x1234, b"new-time");
        let a = build(
            &[Entry {
                name: b"state.json",
                content: b"content",
                local_extra: &extra_a,
                central_extra: &extra_a,
            }],
            TIME_1,
            DATE_1,
            false,
        );
        let b = build(
            &[Entry {
                name: b"state.json",
                content: b"content",
                local_extra: &extra_b,
                central_extra: &extra_b,
            }],
            TIME_1,
            DATE_1,
            false,
        );
        assert_both(false, &a, &b);
    }

    #[test]
    fn equivalent_compares_names_content_and_blob_lengths() {
        let original = zip(TIME_1, DATE_1, false, b"content");
        let changed_content = zip(TIME_2, DATE_2, false, b"contend");
        assert_both(false, &original, &changed_content);

        let changed_name = build(
            &[Entry {
                name: b"other.json",
                content: b"content",
                local_extra: &[],
                central_extra: &[],
            }],
            TIME_2,
            DATE_2,
            false,
        );
        assert_both(false, &original, &changed_name);
        let changed_length = zip(TIME_2, DATE_2, false, b"content-longer");
        assert_both(false, &original, &changed_length);
    }

    #[test]
    fn equivalent_rejects_unverifiable_archives() {
        let pair = || {
            (
                zip(TIME_1, DATE_1, true, b"content"),
                zip(TIME_2, DATE_2, true, b"content"),
            )
        };

        let (mut a, mut b) = pair();
        for blob in [&mut a, &mut b] {
            let eocd = eocd_offset(blob);
            write_u16(blob, eocd + 8, 2);
            write_u16(blob, eocd + 10, 2);
        }
        assert_both(false, &a, &b);

        let (mut a, mut b) = pair();
        for blob in [&mut a, &mut b] {
            let local = signature_offset(blob, LOCAL_SIGNATURE);
            *blob.get_mut(local).expect("local signature byte exists") ^= 0xff;
        }
        assert_both(false, &a, &b);

        let (mut a, mut b) = pair();
        for blob in [&mut a, &mut b] {
            let local = signature_offset(blob, LOCAL_SIGNATURE);
            *blob.get_mut(local + 30).expect("local name byte exists") ^= 0x20;
        }
        assert_both(false, &a, &b);

        let (mut a, mut b) = pair();
        a.pop();
        b.pop();
        assert_both(false, &a, &b);

        let (mut a, mut b) = pair();
        for blob in [&mut a, &mut b] {
            let eocd = eocd_offset(blob);
            write_u16(blob, eocd + 8, u16::MAX);
            write_u16(blob, eocd + 10, u16::MAX);
        }
        assert_both(false, &a, &b);

        let (mut a, mut b) = pair();
        for blob in [&mut a, &mut b] {
            let eocd = eocd_offset(blob);
            write_u16(blob, eocd + 20, 1);
        }
        assert_both(false, &a, &b);
    }

    #[test]
    fn equivalent_does_not_trust_a_lone_local_signature() {
        let mut a = b"PK\x03\x04not-an-archive".to_vec();
        let mut b = a.clone();
        *a.last_mut().unwrap() = b'a';
        *b.last_mut().unwrap() = b'b';
        assert_both(false, &a, &b);
    }

    #[test]
    fn equivalent_compares_non_zip_blobs_normally() {
        assert_both(true, b"ordinary state", b"ordinary state");
        assert_both(false, b"ordinary state", b"ordinary State");
        assert_both(true, b"", b"");
    }

    #[test]
    fn equivalent_never_panics_on_truncation_or_single_byte_corruption() {
        let a = zip(TIME_1, DATE_1, true, b"content");
        let b = zip(TIME_2, DATE_2, true, b"content");
        for length in 0..=a.len() {
            let truncated_a = a.get(..length).expect("test truncation is in bounds");
            let truncated_b = b.get(..length).expect("test truncation is in bounds");
            let _ = equivalent(truncated_a, truncated_b);
            let _ = equivalent(truncated_b, truncated_a);
        }
        for index in 0..a.len() {
            let mut corrupt = a.clone();
            *corrupt
                .get_mut(index)
                .expect("test corruption is in bounds") ^= 0xff;
            let _ = equivalent(&corrupt, &b);
            let _ = equivalent(&b, &corrupt);
        }
    }

    #[test]
    fn equivalent_abandons_masking_when_work_budgets_are_exceeded() {
        let entry = [Entry {
            name: b"x",
            content: b"",
            local_extra: &[],
            central_extra: &[],
        }];
        let mut many_a = Vec::new();
        let mut many_b = Vec::new();
        for _ in 0..200 {
            many_a.extend_from_slice(&build(&entry, TIME_1, DATE_1, false));
            many_b.extend_from_slice(&build(&entry, TIME_2, DATE_2, false));
        }
        assert_both(false, &many_a, &many_b);

        let signatures_a = EOCD_SIGNATURE.repeat(100);
        let mut signatures_b = signatures_a.clone();
        *signatures_b.last_mut().unwrap() ^= 0xff;
        assert_both(false, &signatures_a, &signatures_b);
    }
}
