use std::collections::BTreeMap;

use model_core::payload::{PayloadContentState, PayloadDirection, PayloadSegment};
use model_core::process::SessionIdentity;
use semantic_action_contract::{
    SemanticAction, SemanticActionCompleteness, SemanticActionKind, SemanticActionStatus,
    SemanticEvidence, SemanticEvidenceKind,
};
use serde_json::Value;

/// Built-in provider-neutral LLM request/response shape projection.
pub fn project_llm_payload(
    segment: &PayloadSegment,
    session: Option<SessionIdentity>,
) -> Vec<SemanticAction> {
    let Some(bytes) = segment.bytes.as_deref() else {
        return Vec::new();
    };
    let websocket_body = if segment
        .protocol_hint
        .as_deref()
        .is_some_and(|hint| hint.eq_ignore_ascii_case("websocket"))
    {
        decode_websocket_text(bytes)
    } else {
        None
    };
    let decoded = websocket_body.as_deref().unwrap_or(bytes);
    let Ok(value) = serde_json::from_slice::<Value>(decoded) else {
        return Vec::new();
    };
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let is_request = object.get("messages").is_some()
        || object.get("input").is_some()
        || object.get("prompt").is_some();
    let is_response = object.get("choices").is_some()
        || object.get("output").is_some()
        || object.get("content").is_some() && object.get("role").is_some();
    let kind = match (segment.direction, is_request, is_response) {
        (PayloadDirection::Outbound, true, _) => SemanticActionKind::LlmRequest,
        (PayloadDirection::Inbound, _, true) => SemanticActionKind::LlmResponse,
        _ => return Vec::new(),
    };
    let mut attributes = BTreeMap::new();
    attributes.insert("censorscope.action.kind".to_string(), kind.as_str().to_string());
    attributes.insert(
        "llm.content_state".to_string(),
        if matches!(segment.content_state, PayloadContentState::Complete) {
            "complete"
        } else {
            "partial"
        }
        .to_string(),
    );
    if let Some(model) = object.get("model").and_then(Value::as_str) {
        attributes.insert("llm.model".to_string(), model.to_string());
    }
    if let Some(messages) = object.get("messages").and_then(Value::as_array) {
        attributes.insert("llm.message_count".to_string(), messages.len().to_string());
        attributes.insert(
            "llm.user_message_count".to_string(),
            messages
                .iter()
                .filter(|message| message.get("role").and_then(Value::as_str) == Some("user"))
                .count()
                .to_string(),
        );
    }
    if let Some(choices) = object.get("choices").and_then(Value::as_array) {
        attributes.insert("llm.choice_count".to_string(), choices.len().to_string());
        let tool_calls = choices
            .iter()
            .filter_map(|choice| choice.get("message"))
            .filter_map(|message| message.get("tool_calls"))
            .filter_map(Value::as_array)
            .map(Vec::len)
            .sum::<usize>();
        if tool_calls > 0 {
            attributes.insert("llm.tool_call_count".to_string(), tool_calls.to_string());
        }
    }
    if let Some(content) = object.get("content").and_then(Value::as_array) {
        let tool_uses = content
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("tool_use"))
            .count();
        if tool_uses > 0 {
            attributes.insert("llm.tool_call_count".to_string(), tool_uses.to_string());
        }
    }
    if let Some(usage) = object.get("usage").and_then(Value::as_object) {
        for key in ["prompt_tokens", "completion_tokens", "total_tokens"] {
            if let Some(value) = usage.get(key).and_then(Value::as_u64) {
                attributes.insert(format!("llm.usage.{key}"), value.to_string());
            }
        }
    }
    let complete = matches!(segment.content_state, PayloadContentState::Complete);
    Some(SemanticAction {
        action_id: format!(
            "payload:{}:{}:llm:{}",
            segment.trace_id.get(),
            segment.stream_key.as_deref().unwrap_or("unknown"),
            segment.sequence
        ),
        trace_id: segment.trace_id,
        kind,
        title: match kind {
            SemanticActionKind::LlmRequest => "LLM request",
            SemanticActionKind::LlmResponse => "LLM response",
            _ => "LLM message",
        }
        .to_string(),
        start_time: segment.observed_at,
        end_time: segment.completed.then_some(segment.observed_at),
        process: segment.process,
        status: if matches!(segment.content_state, PayloadContentState::Loss) {
            SemanticActionStatus::Unknown
        } else {
            SemanticActionStatus::Success
        },
        completeness: if complete {
            SemanticActionCompleteness::Complete
        } else {
            SemanticActionCompleteness::Partial
        },
        confidence_millis: complete.then_some(800),
        attributes,
        evidence: vec![SemanticEvidence {
            kind: SemanticEvidenceKind::PayloadSegment,
            id: segment.sequence,
            role: "llm.json".to_string(),
        }],
        session_id: session,
    })
    .into_iter()
    .collect()
}

fn decode_websocket_text(bytes: &[u8]) -> Option<Vec<u8>> {
    let first = *bytes.first()?;
    if first & 0x0f != 0x1 {
        return None;
    }
    let second = *bytes.get(1)?;
    let masked = second & 0x80 != 0;
    let mut length = usize::from(second & 0x7f);
    let mut cursor = 2;
    if length == 126 {
        length = usize::from(u16::from_be_bytes([*bytes.get(2)?, *bytes.get(3)?]));
        cursor = 4;
    } else if length == 127 {
        let value = u64::from_be_bytes(bytes.get(2..10)?.try_into().ok()?);
        length = usize::try_from(value).ok()?;
        cursor = 10;
    }
    let mask = if masked {
        let mask = bytes.get(cursor..cursor + 4)?;
        cursor += 4;
        Some(mask)
    } else {
        None
    };
    let payload = bytes.get(cursor..cursor.checked_add(length)?)?;
    if let Some(mask) = mask {
        let mut decoded = Vec::with_capacity(payload.len());
        decoded.extend(
            payload
                .iter()
                .enumerate()
                .map(|(index, byte)| byte ^ mask[index % 4]),
        );
        return Some(decoded);
    }
    Some(payload.to_vec())
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use model_core::ids::TraceId;
    use model_core::payload::{PayloadDirection, PayloadSourceBoundary};
    use model_core::process::ProcessIdentity;

    use super::*;

    fn segment(
        direction: PayloadDirection,
        state: PayloadContentState,
        body: &[u8],
    ) -> PayloadSegment {
        PayloadSegment {
            trace_id: TraceId::new(2),
            process: ProcessIdentity::new(4),
            session_id: None,
            call_id: None,
            observed_at: SystemTime::UNIX_EPOCH,
            source: PayloadSourceBoundary::Uprobe,
            content_state: state,
            direction,
            stream_key: Some("llm".into()),
            sequence: 1,
            operation_id: None,
            offset: None,
            completed: true,
            original_size: body.len() as u64,
            captured_size: body.len() as u64,
            library: None,
            symbol: None,
            protocol_hint: Some("http".into()),
            loss_reason: None,
            bytes: Some(body.to_vec()),
        }
    }

    #[test]
    fn projects_request_shape_without_body_copy() {
        let actions = project_llm_payload(
            &segment(
                PayloadDirection::Outbound,
                PayloadContentState::Complete,
                br#"{"model":"gpt-test","messages":[{"role":"user","content":"hi"}]}"#,
            ),
            None,
        );
        assert_eq!(actions[0].kind, SemanticActionKind::LlmRequest);
        assert_eq!(actions[0].attributes["llm.model"], "gpt-test");
        assert_eq!(actions[0].attributes["llm.user_message_count"], "1");
        assert!(!actions[0].attributes.contains_key("llm.body"));
    }

    #[test]
    fn partial_response_is_not_complete() {
        let actions = project_llm_payload(
            &segment(
                PayloadDirection::Inbound,
                PayloadContentState::Truncated,
                br#"{"choices":[{"message":{"role":"assistant"}}]}"#,
            ),
            None,
        );
        assert_eq!(actions[0].kind, SemanticActionKind::LlmResponse);
        assert_eq!(actions[0].completeness, SemanticActionCompleteness::Partial);
    }

    #[test]
    fn non_object_json_payload_is_ignored() {
        let actions = project_llm_payload(
            &segment(
                PayloadDirection::Outbound,
                PayloadContentState::Complete,
                br#"[{"role":"user","content":"hi"}]"#,
            ),
            None,
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn websocket_unmasked_text_frame_is_supported() {
        let mut body = vec![0x81, 0x3a];
        let json = br#"{"model":"ws","messages":[{"role":"user"}]}"#;
        body[1] = json.len() as u8;
        body.extend_from_slice(json);
        let mut value = segment(
            PayloadDirection::Outbound,
            PayloadContentState::Complete,
            &body,
        );
        value.protocol_hint = Some("websocket".to_string());
        let actions = project_llm_payload(&value, None);
        assert_eq!(actions[0].kind, SemanticActionKind::LlmRequest);
    }
}
