//! Payload admission at the ingest boundary.
//!
//! This layer never invents content and — by explicit product decision — never
//! discards captured plaintext. Captured payload bytes are passed through
//! unchanged; the only rewrite it performs is marking a segment that carries no
//! bytes at all as metadata-only, so downstream consumers cannot mistake an
//! unavailable body for an empty one.
//!
//! There is deliberately no per-trace or per-segment byte budget here: the
//! collector is expected to retain complete plaintext, and capacity is managed
//! by the operator's archival/rotation policy instead of by silently dropping
//! bytes at ingest.

use std::collections::BTreeMap;

use model_core::ids::TraceId;
use model_core::payload::{PayloadContentState, PayloadSegment};

/// Per-trace counters describing metadata-only segments.
///
/// `stored_bytes` is not retained: with no budget to enforce there is nothing
/// to compare against, and the daemon reports capacity needs from its own
/// diagnostics rather than from a counter that no decision depends on.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PayloadRetentionStats {
    pub metadata_only_segments: u64,
    pub metadata_only_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct TraceRetentionState {
    metadata_only_segments: u64,
    metadata_only_bytes: u64,
}

/// Admission filter for captured payload segments.
#[derive(Clone, Debug, Default)]
pub struct PayloadRetention {
    traces: BTreeMap<TraceId, TraceRetentionState>,
}

impl PayloadRetention {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stats(&self, trace_id: TraceId) -> PayloadRetentionStats {
        self.traces
            .get(&trace_id)
            .map(|state| PayloadRetentionStats {
                metadata_only_segments: state.metadata_only_segments,
                metadata_only_bytes: state.metadata_only_bytes,
            })
            .unwrap_or_default()
    }

    /// Accept one captured segment, preserving every captured byte.
    ///
    /// A segment that arrives without bytes is marked metadata-only with an
    /// explicit reason, so "we could not read the body" stays distinguishable
    /// from "the body was empty".
    pub fn accept(&mut self, mut segment: PayloadSegment) -> PayloadSegment {
        if segment.bytes.is_none() {
            let state = self.traces.entry(segment.trace_id).or_default();
            state.metadata_only_segments = state.metadata_only_segments.saturating_add(1);
            state.metadata_only_bytes = state
                .metadata_only_bytes
                .saturating_add(segment.original_size);
            if segment.content_state == PayloadContentState::Complete {
                segment.content_state = PayloadContentState::MetadataOnly;
                segment.captured_size = 0;
                if segment.loss_reason.is_none() {
                    segment.loss_reason = Some("payload bytes unavailable".to_string());
                }
            }
        }
        segment
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use model_core::payload::{PayloadDirection, PayloadSourceBoundary};
    use model_core::process::ProcessIdentity;

    use super::*;

    fn segment(trace_id: TraceId, sequence: u64, bytes: Option<&[u8]>) -> PayloadSegment {
        PayloadSegment {
            trace_id,
            process: ProcessIdentity::new(1),
            session_id: None,
            call_id: None,
            observed_at: SystemTime::UNIX_EPOCH,
            source: PayloadSourceBoundary::Uprobe,
            content_state: PayloadContentState::Complete,
            direction: PayloadDirection::Outbound,
            stream_key: Some("stream".into()),
            sequence,
            operation_id: None,
            offset: None,
            completed: true,
            original_size: bytes.map_or(0, |value| value.len() as u64),
            captured_size: bytes.map_or(0, |value| value.len() as u64),
            library: None,
            symbol: None,
            protocol_hint: Some("http".into()),
            loss_reason: None,
            bytes: bytes.map(|value| value.to_vec()),
        }
    }

    #[test]
    fn captured_bytes_are_never_dropped_or_truncated() {
        let trace = TraceId::new(9);
        let mut retention = PayloadRetention::new();
        // Repeated large segments must all survive: there is no cumulative budget.
        for sequence in 1..=8 {
            let payload = vec![b'x'; 512 * 1024];
            let accepted = retention.accept(segment(trace, sequence, Some(&payload)));
            assert_eq!(accepted.bytes.as_deref(), Some(&payload[..]));
            assert_eq!(accepted.content_state, PayloadContentState::Complete);
            assert_eq!(accepted.captured_size, payload.len() as u64);
            assert_eq!(accepted.loss_reason, None);
        }
        assert_eq!(retention.stats(trace).metadata_only_segments, 0);
    }

    #[test]
    fn payload_bytes_pass_through_byte_for_byte() {
        let trace = TraceId::new(10);
        let mut retention = PayloadRetention::new();
        let payload: Vec<u8> = (0..=255u8).collect();
        let accepted = retention.accept(segment(trace, 1, Some(&payload)));
        assert_eq!(accepted.bytes.as_deref(), Some(&payload[..]));
        assert_eq!(accepted.original_size, 256);
        assert_eq!(accepted.captured_size, 256);
    }

    #[test]
    fn missing_bytes_are_marked_metadata_only_with_reason() {
        let trace = TraceId::new(11);
        let mut retention = PayloadRetention::new();
        let mut unavailable = segment(trace, 1, None);
        unavailable.original_size = 4096;
        let accepted = retention.accept(unavailable);
        assert_eq!(accepted.bytes, None);
        assert_eq!(accepted.content_state, PayloadContentState::MetadataOnly);
        assert_eq!(accepted.captured_size, 0);
        assert_eq!(
            accepted.loss_reason.as_deref(),
            Some("payload bytes unavailable")
        );
        assert_eq!(
            retention.stats(trace),
            PayloadRetentionStats {
                metadata_only_segments: 1,
                metadata_only_bytes: 4096,
            }
        );
    }

    #[test]
    fn explicit_loss_state_is_preserved_with_its_reason() {
        let trace = TraceId::new(12);
        let mut retention = PayloadRetention::new();
        let mut lost = segment(trace, 1, None);
        lost.content_state = PayloadContentState::Loss;
        lost.loss_reason = Some("TLS uprobe capture gap".to_string());
        let accepted = retention.accept(lost);
        // A segment that already declares its own loss keeps that classification
        // and its upstream reason instead of being relabelled.
        assert_eq!(accepted.content_state, PayloadContentState::Loss);
        assert_eq!(
            accepted.loss_reason.as_deref(),
            Some("TLS uprobe capture gap")
        );
    }
}
