//! Deterministic per-trace payload retention.
//!
//! The first payloads remain immutable. Once a trace budget is exceeded, the
//! first overflowing segment is truncated to the remaining budget and all
//! later segments are metadata-only. This layer never invents content.

use std::collections::BTreeMap;

use model_core::ids::TraceId;
use model_core::payload::{PayloadContentState, PayloadSegment};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PayloadRetentionConfig {
    pub max_trace_bytes: u64,
    pub max_segment_bytes: u64,
}

impl Default for PayloadRetentionConfig {
    fn default() -> Self {
        Self {
            max_trace_bytes: 64 * 1024 * 1024,
            max_segment_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PayloadLoss {
    pub trace_id: TraceId,
    pub original_size: u64,
    pub captured_size: u64,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PayloadRetentionStats {
    pub stored_bytes: u64,
    pub metadata_only_segments: u64,
    pub metadata_only_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetainedPayload {
    pub segment: PayloadSegment,
    pub loss: Option<PayloadLoss>,
}

#[derive(Clone, Debug)]
struct TraceRetentionState {
    stored_bytes: u64,
    overflowed: bool,
    metadata_only_segments: u64,
    metadata_only_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct PayloadRetention {
    config: PayloadRetentionConfig,
    traces: BTreeMap<TraceId, TraceRetentionState>,
}

impl PayloadRetention {
    pub fn new(config: PayloadRetentionConfig) -> Self {
        Self {
            config,
            traces: BTreeMap::new(),
        }
    }

    pub fn stored_bytes(&self, trace_id: TraceId) -> u64 {
        self.traces
            .get(&trace_id)
            .map(|state| state.stored_bytes)
            .unwrap_or(0)
    }

    pub fn stats(&self, trace_id: TraceId) -> PayloadRetentionStats {
        self.traces
            .get(&trace_id)
            .map(|state| PayloadRetentionStats {
                stored_bytes: state.stored_bytes,
                metadata_only_segments: state.metadata_only_segments,
                metadata_only_bytes: state.metadata_only_bytes,
            })
            .unwrap_or_default()
    }

    pub fn accept(&mut self, mut segment: PayloadSegment) -> RetainedPayload {
        let state = self
            .traces
            .entry(segment.trace_id)
            .or_insert(TraceRetentionState {
                stored_bytes: 0,
                overflowed: false,
                metadata_only_segments: 0,
                metadata_only_bytes: 0,
            });

        if state.overflowed || segment.bytes.is_none() {
            state.metadata_only_segments = state.metadata_only_segments.saturating_add(1);
            state.metadata_only_bytes = state
                .metadata_only_bytes
                .saturating_add(segment.original_size);
            if segment.bytes.is_none() && segment.content_state == PayloadContentState::Complete {
                segment.content_state = PayloadContentState::MetadataOnly;
                segment.captured_size = 0;
                if segment.loss_reason.is_none() {
                    segment.loss_reason = Some("payload bytes unavailable".to_string());
                }
            }
            if state.overflowed && segment.bytes.is_some() {
                segment.bytes = None;
                segment.captured_size = 0;
                segment.content_state = PayloadContentState::MetadataOnly;
                segment.loss_reason = Some("trace payload budget exhausted".to_string());
            }
            return RetainedPayload {
                segment,
                loss: None,
            };
        }

        let original_bytes = segment.bytes.take().unwrap_or_default();
        let declared = segment.original_size.max(original_bytes.len() as u64);
        let segment_limit = self.config.max_segment_bytes.min(declared);
        let mut allowed = original_bytes.len().min(segment_limit as usize) as u64;
        let remaining = self
            .config
            .max_trace_bytes
            .saturating_sub(state.stored_bytes);
        let mut loss = None;

        let segment_overflow = declared > segment_limit;
        let trace_overflow = allowed as u64 > remaining;
        if segment_overflow || trace_overflow {
            allowed = allowed.min(remaining) as u64;
            if trace_overflow {
                state.overflowed = true;
            }
            segment.content_state = PayloadContentState::Truncated;
            segment.loss_reason = Some(if segment_overflow {
                "segment payload limit exceeded".to_string()
            } else {
                "trace payload budget exceeded".to_string()
            });
            loss = Some(PayloadLoss {
                trace_id: segment.trace_id,
                original_size: declared,
                captured_size: allowed,
                reason: segment.loss_reason.clone().unwrap_or_default(),
            });
        } else if segment.content_state == PayloadContentState::Complete
            && declared <= original_bytes.len() as u64
        {
            segment.content_state = PayloadContentState::Complete;
        }

        let captured = allowed as usize;
        segment.bytes = Some(original_bytes[..captured].to_vec());
        segment.original_size = declared;
        segment.captured_size = allowed;
        state.stored_bytes = state.stored_bytes.saturating_add(allowed);
        RetainedPayload { segment, loss }
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use model_core::ids::TraceId;
    use model_core::payload::{
        PayloadContentState, PayloadDirection, PayloadSegment, PayloadSourceBoundary,
    };
    use model_core::process::ProcessIdentity;

    use super::*;

    fn segment(trace_id: TraceId, sequence: u64, bytes: &[u8]) -> PayloadSegment {
        PayloadSegment {
            trace_id,
            process: ProcessIdentity::new(1),
            session_id: None,
            call_id: None,
            observed_at: SystemTime::UNIX_EPOCH,
            source: PayloadSourceBoundary::Stdio,
            content_state: PayloadContentState::Complete,
            direction: PayloadDirection::Outbound,
            stream_key: Some("s".into()),
            sequence,
            operation_id: None,
            offset: None,
            completed: true,
            original_size: bytes.len() as u64,
            captured_size: bytes.len() as u64,
            library: None,
            symbol: None,
            protocol_hint: Some("http".into()),
            loss_reason: None,
            bytes: Some(bytes.to_vec()),
        }
    }

    #[test]
    fn keeps_earliest_payload_and_truncates_first_overflow() {
        let trace = TraceId::new(9);
        let mut retention = PayloadRetention::new(PayloadRetentionConfig {
            max_trace_bytes: 5,
            max_segment_bytes: 64,
        });
        let first = retention.accept(segment(trace, 1, b"abc"));
        assert_eq!(first.segment.bytes.as_deref(), Some(&b"abc"[..]));
        let second = retention.accept(segment(trace, 2, b"WXYZ"));
        assert_eq!(second.segment.bytes.as_deref(), Some(&b"WX"[..]));
        assert_eq!(second.segment.content_state, PayloadContentState::Truncated);
        assert!(second.loss.is_some());
        let third = retention.accept(segment(trace, 3, b"later"));
        assert_eq!(third.segment.bytes, None);
        assert_eq!(
            third.segment.content_state,
            PayloadContentState::MetadataOnly
        );
        assert_eq!(retention.stored_bytes(trace), 5);
        assert_eq!(
            retention.stats(trace),
            PayloadRetentionStats {
                stored_bytes: 5,
                metadata_only_segments: 1,
                metadata_only_bytes: 5,
            }
        );
    }

    #[test]
    fn segment_limit_is_explicitly_truncated() {
        let trace = TraceId::new(10);
        let mut retention = PayloadRetention::new(PayloadRetentionConfig {
            max_trace_bytes: 100,
            max_segment_bytes: 2,
        });
        let result = retention.accept(segment(trace, 1, b"abcd"));
        assert_eq!(result.segment.bytes.as_deref(), Some(&b"ab"[..]));
        assert_eq!(result.segment.content_state, PayloadContentState::Truncated);
        assert!(result.loss.is_some());
    }

    #[test]
    fn unlimited_trace_budget_does_not_overflow() {
        let trace = TraceId::new(11);
        let mut retention = PayloadRetention::new(PayloadRetentionConfig {
            max_trace_bytes: u64::MAX,
            max_segment_bytes: u64::MAX,
        });
        let result = retention.accept(segment(trace, 1, b"complete"));
        assert_eq!(result.segment.content_state, PayloadContentState::Complete);
        assert_eq!(result.segment.bytes.as_deref(), Some(&b"complete"[..]));
        assert_eq!(retention.stored_bytes(trace), 8);
    }
}
