use crate::error::{Result, CensorFsError};
use bincode::Options;
use serde::{de::DeserializeOwned, Serialize};
use std::io::{Read, Write};

pub const FORMAT_VERSION: u16 = 2;
const HEADER_LEN: usize = 53;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum RecordKind {
    Superblock = 1,
    BranchMeta = 2,
    HeadSlot = 3,
    Generation = 4,
    Manifest = 5,
    Object = 6,
    Tx = 7,
    Ticket = 8,
    Candidate = 9,
    Receipt = 10,
    RequestResult = 11,
    MergeIntent = 12,
}

impl RecordKind {
    fn magic(self) -> [u8; 9] {
        let mut value = [0u8; 9];
        value[..8].copy_from_slice(b"CENSORFS");
        value[8] = self as u8;
        value
    }
}

pub fn serialize<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    Ok(bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_little_endian()
        .serialize(value)?)
}

pub fn deserialize<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    Ok(bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_little_endian()
        .reject_trailing_bytes()
        .deserialize(bytes)?)
}

pub fn encode_record<T: Serialize>(kind: RecordKind, value: &T) -> Result<Vec<u8>> {
    let payload = serialize(value)?;
    let digest = blake3::hash(&payload);
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&kind.magic());
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&(kind as u16).to_le_bytes());
    out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    out.extend_from_slice(digest.as_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

pub fn decode_record<T: DeserializeOwned>(kind: RecordKind, bytes: &[u8]) -> Result<T> {
    if bytes.len() < HEADER_LEN {
        return Err(CensorFsError::corrupt("record header is truncated"));
    }
    if bytes[0..9] != kind.magic() {
        return Err(CensorFsError::corrupt("record magic or type mismatch"));
    }
    let version = u16::from_le_bytes(bytes[9..11].try_into().unwrap());
    let encoded_kind = u16::from_le_bytes(bytes[11..13].try_into().unwrap());
    let payload_len = u64::from_le_bytes(bytes[13..21].try_into().unwrap()) as usize;
    if version != FORMAT_VERSION || encoded_kind != kind as u16 {
        return Err(CensorFsError::corrupt("unsupported record version"));
    }
    if bytes.len() != HEADER_LEN + payload_len {
        return Err(CensorFsError::corrupt("record length mismatch"));
    }
    let payload = &bytes[HEADER_LEN..];
    if blake3::hash(payload).as_bytes() != &bytes[21..53] {
        return Err(CensorFsError::corrupt("record digest mismatch"));
    }
    deserialize(payload)
}

pub fn write_journal_frame<T: Serialize>(mut writer: impl Write, value: &T) -> Result<usize> {
    let payload = serialize(value)?;
    let len = u32::try_from(payload.len())
        .map_err(|_| CensorFsError::bad_request("journal record too large"))?;
    let crc = crc32c::crc32c(&payload);
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(&payload)?;
    writer.write_all(&crc.to_le_bytes())?;
    Ok(8 + payload.len())
}

pub fn read_journal_frames<T: DeserializeOwned>(mut reader: impl Read) -> Result<(Vec<T>, u64)> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    let mut offset = 0usize;
    let mut out = Vec::new();
    while offset + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let end = offset
            .saturating_add(4)
            .saturating_add(len)
            .saturating_add(4);
        if end > bytes.len() {
            break;
        }
        let payload = &bytes[offset + 4..offset + 4 + len];
        let expected = u32::from_le_bytes(bytes[offset + 4 + len..end].try_into().unwrap());
        if crc32c::crc32c(payload) != expected {
            break;
        }
        out.push(deserialize(payload)?);
        offset = end;
    }
    Ok((out, offset as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Sample {
        a: u64,
        b: String,
    }

    #[test]
    fn record_is_versioned_and_detects_corruption() {
        let value = Sample {
            a: 7,
            b: "ok".into(),
        };
        let encoded = encode_record(RecordKind::Tx, &value).unwrap();
        assert_eq!(
            decode_record::<Sample>(RecordKind::Tx, &encoded).unwrap(),
            value
        );
        let mut corrupt = encoded;
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(decode_record::<Sample>(RecordKind::Tx, &corrupt).is_err());
    }

    #[test]
    fn journal_ignores_partial_tail() {
        let mut bytes = Vec::new();
        write_journal_frame(
            &mut bytes,
            &Sample {
                a: 1,
                b: "a".into(),
            },
        )
        .unwrap();
        let valid_len = bytes.len() as u64;
        bytes.extend_from_slice(&[3, 0, 0]);
        let (records, end) = read_journal_frames::<Sample>(&bytes[..]).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(end, valid_len);
    }

    #[test]
    fn validated_newtypes_reject_forged_wire_values() {
        let invalid_path = serialize(&b"../escape".to_vec()).unwrap();
        assert!(deserialize::<crate::model::LogicalPath>(&invalid_path).is_err());
        let invalid_branch = serialize(&"../branch".to_string()).unwrap();
        assert!(deserialize::<crate::ids::BranchId>(&invalid_branch).is_err());
    }
}
