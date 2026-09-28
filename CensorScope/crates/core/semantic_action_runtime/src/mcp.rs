use std::collections::BTreeMap;

use model_core::payload::{PayloadContentState, PayloadDirection, PayloadSegment};
use model_core::process::SessionIdentity;
use semantic_action_contract::{
    SemanticAction, SemanticActionCompleteness, SemanticActionKind, SemanticActionStatus,
    SemanticEvidence, SemanticEvidenceKind,
};
use serde_json::Value;

pub fn project_mcp_payload(
    segment: &PayloadSegment,
    session: Option<SessionIdentity>,
) -> Vec<SemanticAction> {
    let Some(bytes) = segment.bytes.as_deref() else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return Vec::new();
    };
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Vec::new();
    }
    let method = object.get("method").and_then(Value::as_str);
    let response = object.contains_key("result") || object.contains_key("error");
    let kind = if response {
        SemanticActionKind::McpResponse
    } else if method == Some("tools/call") {
        SemanticActionKind::McpToolCall
    } else if method.is_some() {
        SemanticActionKind::McpRequest
    } else {
        return Vec::new();
    };
    let complete = matches!(segment.content_state, PayloadContentState::Complete);
    let mut attributes = BTreeMap::new();
    attributes.insert(
        "censorscope.action.kind".to_string(),
        kind.as_str().to_string(),
    );
    attributes.insert("mcp.jsonrpc".to_string(), "2.0".to_string());
    attributes.insert(
        "mcp.direction".to_string(),
        match segment.direction {
            PayloadDirection::Inbound => "inbound",
            PayloadDirection::Outbound => "outbound",
            _ => "unknown",
        }
        .to_string(),
    );
    if let Some(id) = object.get("id") {
        attributes.insert("mcp.request_id".to_string(), id.to_string());
    }
    if let Some(method) = method {
        attributes.insert("mcp.method".to_string(), method.to_string());
    }
    if let Some(name) = object
        .get("params")
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
    {
        attributes.insert("mcp.tool_name".to_string(), name.to_string());
    }
    if object.contains_key("error") {
        attributes.insert("mcp.result_state".to_string(), "error".to_string());
    } else if response {
        attributes.insert("mcp.result_state".to_string(), "success".to_string());
    }
    let title = method
        .map(|method| format!("MCP {method}"))
        .unwrap_or_else(|| "MCP response".to_string());
    vec![SemanticAction {
        action_id: format!(
            "payload:{}:{}:mcp:{}",
            segment.trace_id.get(),
            segment.stream_key.as_deref().unwrap_or("unknown"),
            segment.sequence
        ),
        trace_id: segment.trace_id,
        kind,
        title,
        start_time: segment.observed_at,
        end_time: segment.completed.then_some(segment.observed_at),
        process: segment.process,
        status: if object.contains_key("error") {
            SemanticActionStatus::Error
        } else if matches!(segment.content_state, PayloadContentState::Loss) {
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
            role: "mcp.jsonrpc".to_string(),
        }],
        session_id: session,
    }]
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use model_core::ids::TraceId;
    use model_core::payload::PayloadSourceBoundary;
    use model_core::process::ProcessIdentity;

    use super::*;

    fn segment(direction: PayloadDirection, body: &[u8]) -> PayloadSegment {
        PayloadSegment {
            trace_id: TraceId::new(3),
            process: ProcessIdentity::new(7),
            session_id: None,
            call_id: None,
            observed_at: SystemTime::UNIX_EPOCH,
            source: PayloadSourceBoundary::Stdio,
            content_state: PayloadContentState::Complete,
            direction,
            stream_key: Some("stdio".into()),
            sequence: 2,
            operation_id: None,
            offset: None,
            completed: true,
            original_size: body.len() as u64,
            captured_size: body.len() as u64,
            library: None,
            symbol: None,
            protocol_hint: Some("mcp".into()),
            loss_reason: None,
            bytes: Some(body.to_vec()),
        }
    }

    #[test]
    fn projects_mcp_tool_call() {
        let actions = project_mcp_payload(
            &segment(
                PayloadDirection::Outbound,
                br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search"}}"#,
            ),
            None,
        );
        assert_eq!(actions[0].kind, SemanticActionKind::McpToolCall);
        assert_eq!(actions[0].attributes["mcp.tool_name"], "search");
    }

    #[test]
    fn malformed_json_is_not_mcp() {
        assert!(
            project_mcp_payload(&segment(PayloadDirection::Inbound, b"not-json"), None).is_empty()
        );
    }
}
