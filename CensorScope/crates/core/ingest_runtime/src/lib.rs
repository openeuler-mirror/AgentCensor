//! Normalization of eBPF process lifecycle observations.

pub mod fd_lineage;
mod normalize;
pub mod retention;

use collector_event::RawCollectorEvent;
use collector_event::RawPayloadSegment;
use model_core::event::DomainEvent;
use model_core::ids::{EventId, TraceId};
use model_core::payload::PayloadSegment;
use model_core::process::{ProcessIdentity, SessionIdentity};

/// Identity match that associates a raw collector event with a trace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IngestMatch {
    pub trace_id: TraceId,
    pub process: ProcessIdentity,
    pub parent: Option<ProcessIdentity>,
    /// Session identified from the process environment; `None` marks a
    /// non-session operation.
    pub session: Option<SessionIdentity>,
}

/// Convert a raw process observation into the normalized storage event model.
pub fn normalize(
    raw_event: RawCollectorEvent,
    matched: IngestMatch,
    event_id: EventId,
) -> DomainEvent {
    let session = matched.session.clone();
    let mut event = normalize::normalize_event(
        raw_event,
        matched.trace_id,
        matched.process,
        matched.parent,
        event_id,
    );
    event.envelope.session_id = session;
    event
}

/// Resolve collector-side payload identity at the same boundary as events.
pub fn normalize_payload(raw: RawPayloadSegment, matched: IngestMatch) -> PayloadSegment {
    PayloadSegment {
        trace_id: matched.trace_id,
        process: matched.process,
        session_id: matched.session,
        call_id: raw.envelope.call_id.clone(),
        observed_at: raw.envelope.observed_at,
        source: raw.source,
        content_state: raw.content_state,
        direction: raw.direction,
        stream_key: raw.stream_key,
        sequence: raw.sequence,
        operation_id: raw.operation_id,
        offset: raw.offset,
        completed: raw.completed,
        original_size: raw.original_size,
        captured_size: raw.captured_size,
        library: raw.library,
        symbol: raw.symbol,
        protocol_hint: raw.protocol_hint,
        loss_reason: raw.loss_reason,
        bytes: raw.bytes,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::SystemTime;

    use collector_event::{RawEventEnvelope, RawObservationPayload};
    use model_core::event::{EventFlags, EventKind};
    use model_core::ids::{CollectorName, EventId, TraceId};
    use model_core::payload::PayloadDirection;
    use model_core::process::{ArgvCapture, ProcessIdentity, ProcessObservation};

    use super::*;

    fn raw(payload: RawObservationPayload) -> RawCollectorEvent {
        RawCollectorEvent {
            envelope: RawEventEnvelope {
                trace_id: Some(TraceId::new(7)),
                observed_at: SystemTime::UNIX_EPOCH,
                process: ProcessObservation::default(),
                collector: CollectorName::new("test"),
                session_id: None,
                call_id: None,
            },
            payload,
        }
    }

    #[test]
    fn raw_non_process_events_normalize_without_loss_of_kind() {
        let events = [
            (
                RawObservationPayload::File {
                    operation: "read".into(),
                    path: Some("/tmp/x".into()),
                    fd: Some(3),
                    size: Some(4),
                    metadata: BTreeMap::new(),
                },
                EventKind::File,
            ),
            (
                RawObservationPayload::Net {
                    operation: "sendto".into(),
                    endpoint: Some("127.0.0.1:80".into()),
                    direction: PayloadDirection::Outbound,
                    length: Some(4),
                    result: Some(4),
                    metadata: BTreeMap::new(),
                },
                EventKind::Net,
            ),
            (
                RawObservationPayload::Ipc {
                    operation: "pipe".into(),
                    channel: Some("pipe-1".into()),
                    direction: PayloadDirection::Bidirectional,
                    length: None,
                    metadata: BTreeMap::new(),
                },
                EventKind::Ipc,
            ),
            (
                RawObservationPayload::Stdio {
                    stream: "stdout".into(),
                    direction: PayloadDirection::Outbound,
                    length: Some(2),
                    metadata: BTreeMap::new(),
                },
                EventKind::Stdio,
            ),
        ];
        for (payload, expected_kind) in events {
            let event = normalize(
                raw(payload),
                IngestMatch {
                    trace_id: TraceId::new(7),
                    process: ProcessIdentity::new(9),
                    parent: None,
                    session: None,
                },
                EventId::new(1),
            );
            assert_eq!(event.envelope.kind, expected_kind);
        }
    }

    #[test]
    fn argv_capture_flags_become_explicit_event_flags() {
        let event = normalize(
            raw(RawObservationPayload::Process {
                operation: "exec".into(),
                parent: None,
                argv: Some(ArgvCapture {
                    args: vec![vec![0xff, 0x00]],
                    flags: ArgvCapture::PARTIAL
                        | ArgvCapture::TRUNCATED
                        | ArgvCapture::READ_FAILURE
                        | ArgvCapture::LOSS,
                }),
                metadata: BTreeMap::new(),
            }),
            IngestMatch {
                trace_id: TraceId::new(7),
                process: ProcessIdentity::new(1),
                parent: None,
                session: None,
            },
            EventId::new(1),
        );
        assert!(event.envelope.flags.contains(EventFlags::PARTIAL));
        assert!(event.envelope.flags.contains(EventFlags::TRUNCATED));
        assert!(event.envelope.flags.contains(EventFlags::CAPTURE_GAP));
        assert!(event.envelope.flags.contains(EventFlags::LOSS));
    }

    #[test]
    fn capture_gaps_and_truncation_are_explicit_flags() {
        let mut file_metadata = BTreeMap::new();
        file_metadata.insert("path_truncated".to_string(), "true".to_string());
        let file = normalize(
            raw(RawObservationPayload::File {
                operation: "open".into(),
                path: Some("/tmp/".into()),
                fd: Some(3),
                size: None,
                metadata: file_metadata,
            }),
            IngestMatch {
                trace_id: TraceId::new(7),
                process: ProcessIdentity::new(9),
                parent: None,
                session: None,
            },
            EventId::new(2),
        );
        assert!(file.envelope.flags.contains(EventFlags::TRUNCATED));
        assert!(file.envelope.flags.contains(EventFlags::PARTIAL));

        let mut net_metadata = BTreeMap::new();
        net_metadata.insert("endpoint_capture_gap".to_string(), "true".to_string());
        let net = normalize(
            raw(RawObservationPayload::Net {
                operation: "connect".into(),
                endpoint: None,
                direction: PayloadDirection::Unknown,
                length: None,
                result: Some(-1),
                metadata: net_metadata,
            }),
            IngestMatch {
                trace_id: TraceId::new(7),
                process: ProcessIdentity::new(9),
                parent: None,
                session: None,
            },
            EventId::new(3),
        );
        assert!(net.envelope.flags.contains(EventFlags::CAPTURE_GAP));
        assert!(net.envelope.flags.contains(EventFlags::PARTIAL));

        let mut stdio_metadata = BTreeMap::new();
        stdio_metadata.insert("capture_gap".to_string(), "true".to_string());
        let stdio = normalize(
            raw(RawObservationPayload::Stdio {
                stream: "stdout".into(),
                direction: PayloadDirection::Outbound,
                length: Some(4),
                metadata: stdio_metadata,
            }),
            IngestMatch {
                trace_id: TraceId::new(7),
                process: ProcessIdentity::new(9),
                parent: None,
                session: None,
            },
            EventId::new(4),
        );
        assert!(stdio.envelope.flags.contains(EventFlags::CAPTURE_GAP));
    }

    #[test]
    fn missing_process_generation_is_an_explicit_identity_gap() {
        let event = normalize(
            raw(RawObservationPayload::File {
                operation: "read".into(),
                path: Some("/tmp/x".into()),
                fd: Some(3),
                size: Some(1),
                metadata: BTreeMap::new(),
            }),
            IngestMatch {
                trace_id: TraceId::new(7),
                process: ProcessIdentity::new(9),
                parent: None,
                session: None,
            },
            EventId::new(5),
        );
        assert!(event.envelope.flags.contains(EventFlags::PARTIAL));
        assert!(event.envelope.flags.contains(EventFlags::CAPTURE_GAP));
    }

    #[test]
    fn session_carries_into_normalized_event() {
        let session = model_core::process::SessionIdentity::new("sess-1");
        let event = normalize(
            raw(RawObservationPayload::File {
                operation: "read".into(),
                path: Some("/tmp/x".into()),
                fd: Some(3),
                size: Some(1),
                metadata: BTreeMap::new(),
            }),
            IngestMatch {
                trace_id: TraceId::new(7),
                process: ProcessIdentity::new(9),
                parent: None,
                session: Some(session.clone()),
            },
            EventId::new(6),
        );
        assert_eq!(event.envelope.session_id, Some(session));
    }

    #[test]
    fn session_carries_into_normalized_payload() {
        let session = model_core::process::SessionIdentity::new("sess-payload");
        let raw = RawPayloadSegment {
            envelope: RawEventEnvelope {
                trace_id: Some(TraceId::new(7)),
                observed_at: SystemTime::UNIX_EPOCH,
                process: ProcessObservation::default(),
                collector: CollectorName::new("test"),
                session_id: None,
                call_id: None,
            },
            source: model_core::payload::PayloadSourceBoundary::Stdio,
            content_state: model_core::payload::PayloadContentState::Complete,
            direction: PayloadDirection::Outbound,
            stream_key: Some("s".into()),
            sequence: 1,
            operation_id: None,
            offset: None,
            completed: true,
            original_size: 1,
            captured_size: 1,
            library: None,
            symbol: None,
            protocol_hint: None,
            loss_reason: None,
            bytes: Some(vec![1]),
        };
        let segment = normalize_payload(
            raw,
            IngestMatch {
                trace_id: TraceId::new(7),
                process: ProcessIdentity::new(9),
                parent: None,
                session: Some(session.clone()),
            },
        );
        assert_eq!(segment.session_id, Some(session));
    }
}
