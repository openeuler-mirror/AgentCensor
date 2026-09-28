//! Raw payload retention contracts shared by collectors and storage.

use serde::{Deserialize, Serialize};
use std::time::SystemTime;

use crate::ids::TraceId;
use crate::process::{ProcessIdentity, SessionIdentity};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PayloadContentState {
    Unknown = 0,
    Complete = 1,
    Truncated = 2,
    MetadataOnly = 3,
    Loss = 4,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PayloadDirection {
    Unknown = 0,
    Inbound = 1,
    Outbound = 2,
    Bidirectional = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PayloadSourceBoundary {
    Unknown = 0,
    Uprobe = 2,
    Stdio = 3,
}

/// A normalized payload segment. Bytes are optional for metadata-only data.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PayloadSegment {
    pub trace_id: TraceId,
    pub process: ProcessIdentity,
    /// Session resolved from the originating process environment.
    pub session_id: Option<SessionIdentity>,
    /// Harness tool call identifier used to correlate payload observations.
    pub call_id: Option<String>,
    pub observed_at: SystemTime,
    pub source: PayloadSourceBoundary,
    pub content_state: PayloadContentState,
    pub direction: PayloadDirection,
    pub stream_key: Option<String>,
    pub sequence: u64,
    pub operation_id: Option<u64>,
    pub offset: Option<u64>,
    pub completed: bool,
    pub original_size: u64,
    pub captured_size: u64,
    pub library: Option<String>,
    pub symbol: Option<String>,
    pub protocol_hint: Option<String>,
    pub loss_reason: Option<String>,
    pub bytes: Option<Vec<u8>>,
}

impl PayloadSegment {
    pub fn metadata_only(&self) -> bool {
        self.content_state == PayloadContentState::MetadataOnly || self.bytes.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TraceId;
    use crate::process::ProcessIdentity;

    #[test]
    fn metadata_only_segments_never_claim_bytes() {
        let segment = PayloadSegment {
            trace_id: TraceId::new(1),
            process: ProcessIdentity::new(2),
            session_id: None,
            call_id: None,
            observed_at: SystemTime::UNIX_EPOCH,
            source: PayloadSourceBoundary::Stdio,
            content_state: PayloadContentState::MetadataOnly,
            direction: PayloadDirection::Inbound,
            stream_key: Some("stream-1".to_string()),
            sequence: 3,
            operation_id: None,
            offset: None,
            completed: true,
            original_size: 8,
            captured_size: 0,
            library: None,
            symbol: None,
            protocol_hint: Some("http".to_string()),
            loss_reason: Some("trace budget".to_string()),
            bytes: None,
        };
        assert!(segment.metadata_only());
        assert_eq!(segment.captured_size, 0);
    }
}
