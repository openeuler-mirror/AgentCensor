use std::collections::BTreeMap;

use model_core::payload::{PayloadContentState, PayloadSegment};
use model_core::process::SessionIdentity;
use semantic_action_contract::{
    SemanticAction, SemanticActionCompleteness, SemanticActionKind, SemanticActionStatus,
    SemanticEvidence, SemanticEvidenceKind,
};

/// Projects Server-Sent Events from an explicitly identified event-stream
/// payload. Event data is summarized by byte length; the raw body remains in
/// the payload segment layer only.
pub fn project_sse_payload(
    segment: &PayloadSegment,
    session: Option<SessionIdentity>,
) -> Vec<SemanticAction> {
    let Some(bytes) = segment.bytes.as_deref() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(bytes);
    let identified = segment
        .protocol_hint
        .as_deref()
        .is_some_and(|hint| hint.eq_ignore_ascii_case("sse"))
        || text
            .to_ascii_lowercase()
            .contains("content-type: text/event-stream")
        || text
            .lines()
            .any(|line| line.starts_with("data:") || line.starts_with("event:"));
    if !identified {
        return Vec::new();
    }
    let mut actions = Vec::new();
    let stream_id = format!(
        "payload:{}:{}:sse",
        segment.trace_id.get(),
        segment.stream_key.as_deref().unwrap_or("unknown")
    );
    let complete_stream = !matches!(
        segment.content_state,
        PayloadContentState::Truncated | PayloadContentState::Loss
    );
    let mut stream_attributes = BTreeMap::new();
    stream_attributes.insert("censorscope.action.kind".to_string(), "sse.stream".to_string());
    stream_attributes.insert("sse.event_count".to_string(), "0".to_string());
    stream_attributes.insert(
        "sse.content_state".to_string(),
        if complete_stream {
            "complete"
        } else {
            "partial"
        }
        .to_string(),
    );
    actions.push(SemanticAction {
        action_id: stream_id.clone(),
        trace_id: segment.trace_id,
        kind: SemanticActionKind::SseStream,
        title: "SSE stream".to_string(),
        start_time: segment.observed_at,
        end_time: segment.completed.then_some(segment.observed_at),
        process: segment.process,
        status: if matches!(segment.content_state, PayloadContentState::Loss) {
            SemanticActionStatus::Unknown
        } else {
            SemanticActionStatus::Success
        },
        completeness: if complete_stream {
            SemanticActionCompleteness::Complete
        } else {
            SemanticActionCompleteness::Partial
        },
        confidence_millis: complete_stream.then_some(850),
        attributes: stream_attributes,
        evidence: evidence(segment, "sse.stream"),
        session_id: session.clone(),
    });
    let mut event_count = 0_u64;
    for (index, block) in text.split("\n\n").enumerate() {
        let block = block.trim_matches('\n').trim_matches('\r');
        if block.is_empty() || !block.lines().any(|line| line.starts_with("data:")) {
            continue;
        }
        let mut attributes = BTreeMap::new();
        let mut event_name = None;
        let mut data_bytes = 0_usize;
        for line in block.lines() {
            if let Some(value) = line.strip_prefix("event:") {
                event_name = Some(value.trim().to_string());
            } else if let Some(value) = line.strip_prefix("data:") {
                data_bytes += value.trim_start().len();
            }
        }
        if let Some(name) = event_name {
            attributes.insert("sse.event_type".to_string(), name);
        }
        attributes.insert("sse.data_bytes".to_string(), data_bytes.to_string());
        attributes.insert("censorscope.action.kind".to_string(), "sse.event".to_string());
        let action_id = format!("{stream_id}:event:{index}");
        actions.push(SemanticAction {
            action_id,
            trace_id: segment.trace_id,
            kind: SemanticActionKind::SseEvent,
            title: "SSE event".to_string(),
            start_time: segment.observed_at,
            end_time: Some(segment.observed_at),
            process: segment.process,
            status: SemanticActionStatus::Success,
            completeness: if complete_stream {
                SemanticActionCompleteness::Complete
            } else {
                SemanticActionCompleteness::Partial
            },
            confidence_millis: complete_stream.then_some(850),
            attributes,
            evidence: evidence(segment, "sse.event"),
            session_id: session.clone(),
        });
        event_count += 1;
    }
    if let Some(stream) = actions.first_mut() {
        stream
            .attributes
            .insert("sse.event_count".to_string(), event_count.to_string());
    }
    actions
}

fn evidence(segment: &PayloadSegment, role: &str) -> Vec<SemanticEvidence> {
    vec![SemanticEvidence {
        kind: SemanticEvidenceKind::PayloadSegment,
        id: segment.sequence,
        role: role.to_string(),
    }]
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use model_core::ids::TraceId;
    use model_core::payload::{PayloadDirection, PayloadSourceBoundary};
    use model_core::process::ProcessIdentity;

    use super::*;

    fn segment(state: PayloadContentState, body: &[u8]) -> PayloadSegment {
        PayloadSegment {
            trace_id: TraceId::new(5),
            process: ProcessIdentity::new(2),
            session_id: None,
            call_id: None,
            observed_at: SystemTime::UNIX_EPOCH,
            source: PayloadSourceBoundary::Uprobe,
            content_state: state,
            direction: PayloadDirection::Inbound,
            stream_key: Some("stream".to_string()),
            sequence: 8,
            operation_id: None,
            offset: None,
            completed: true,
            original_size: body.len() as u64,
            captured_size: body.len() as u64,
            library: None,
            symbol: None,
            protocol_hint: Some("sse".to_string()),
            loss_reason: None,
            bytes: Some(body.to_vec()),
        }
    }

    #[test]
    fn projects_stream_and_events_without_copying_data() {
        let actions = project_sse_payload(
            &segment(
                PayloadContentState::Complete,
                b"event: message\ndata: {\"x\":1}\n\ndata: done\n\n",
            ),
            None,
        );
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0].kind, SemanticActionKind::SseStream);
        assert_eq!(actions[1].kind, SemanticActionKind::SseEvent);
        assert_eq!(actions[1].attributes["sse.data_bytes"], "7");
        assert!(!actions[1].attributes.contains_key("sse.data"));
    }

    #[test]
    fn truncated_stream_is_partial() {
        let actions = project_sse_payload(
            &segment(PayloadContentState::Truncated, b"data: partial\n"),
            None,
        );
        assert!(
            actions
                .iter()
                .all(|action| action.completeness == SemanticActionCompleteness::Partial)
        );
    }
}
