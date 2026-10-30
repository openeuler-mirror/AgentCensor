use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use crate::contract::MAX_FRAME_BYTES;
use crate::event::ExternalEvent;
use crate::validation::ContractError;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HelloFrame {
    pub protocol_version: u16,
    pub producer: String,
    pub instance_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AckFrame {
    pub source_instance_id: String,
    pub source_sequence: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GapFrame {
    pub source_instance_id: String,
    pub first_missing_sequence: u64,
    pub missing_count: u64,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "data")]
pub enum EventFrame {
    Hello(HelloFrame),
    Event(ExternalEvent),
    Batch(Vec<ExternalEvent>),
    Ack(AckFrame),
    Gap(GapFrame),
}

pub fn write_frame(mut writer: impl Write, frame: &EventFrame) -> Result<(), ContractError> {
    let payload = serde_json::to_vec(frame)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(ContractError::FrameTooLarge(payload.len()));
    }
    let length =
        u32::try_from(payload.len()).map_err(|_| ContractError::FrameTooLarge(payload.len()))?;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}

pub fn read_frame(mut reader: impl Read) -> Result<Option<EventFrame>, ContractError> {
    let mut header = [0_u8; 4];
    match reader.read(&mut header[..1]) {
        Ok(0) => return Ok(None),
        Ok(_) => reader.read_exact(&mut header[1..])?,
        Err(error) => return Err(error.into()),
    }
    let length = u32::from_le_bytes(header) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(ContractError::FrameTooLarge(length));
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload)?;
    Ok(Some(serde_json::from_slice(&payload)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::PROTOCOL_VERSION;

    #[test]
    fn frame_round_trip() {
        let frame = EventFrame::Hello(HelloFrame {
            protocol_version: PROTOCOL_VERSION,
            producer: "test".into(),
            instance_id: "instance".into(),
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &frame).expect("write frame");
        assert_eq!(
            read_frame(bytes.as_slice()).expect("read frame"),
            Some(frame)
        );
    }
}
