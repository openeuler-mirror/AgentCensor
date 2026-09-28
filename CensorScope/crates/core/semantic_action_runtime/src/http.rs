use std::collections::BTreeMap;

use model_core::payload::{PayloadContentState, PayloadDirection, PayloadSegment};
use model_core::process::SessionIdentity;
use semantic_action_contract::{
    SemanticAction, SemanticActionCompleteness, SemanticActionKind, SemanticActionStatus,
    SemanticEvidence, SemanticEvidenceKind,
};

/// Parse a bounded HTTP/1.1 plaintext segment. This parser only projects
/// headers that are actually present; it never reconstructs body text from
/// metadata or from an incomplete segment.
pub fn project_http1_payload(
    segment: &PayloadSegment,
    session: Option<SessionIdentity>,
) -> Option<SemanticAction> {
    let bytes = segment.bytes.as_deref()?;
    if let Some(action) = project_http2_payload(segment, bytes, session.clone()) {
        return Some(action);
    }
    let separator = bytes.windows(4).position(|window| window == b"\r\n\r\n")?;
    let header_end = separator + 4;
    let header_text = std::str::from_utf8(&bytes[..separator]).ok()?;
    let mut lines = header_text.split("\r\n");
    let start_line = lines.next()?;
    let (kind, title, mut attributes) = if start_line.starts_with("HTTP/") {
        let mut parts = start_line.splitn(3, ' ');
        let protocol = parts.next()?.to_string();
        let status = parts.next()?.parse::<u16>().ok()?;
        let reason = parts.next().unwrap_or_default().to_string();
        let mut attributes = BTreeMap::new();
        attributes.insert("http.protocol".to_string(), protocol);
        attributes.insert("http.status_code".to_string(), status.to_string());
        if !reason.is_empty() {
            attributes.insert("http.reason".to_string(), reason.clone());
        }
        (
            SemanticActionKind::HttpMessage,
            format!("HTTP {status}"),
            attributes,
        )
    } else {
        let mut parts = start_line.splitn(3, ' ');
        let method = parts.next()?.to_string();
        let path = parts.next()?.to_string();
        let protocol = parts.next()?.to_string();
        if !protocol.starts_with("HTTP/") || method.is_empty() || path.is_empty() {
            return None;
        }
        let mut attributes = BTreeMap::new();
        attributes.insert("http.protocol".to_string(), protocol);
        attributes.insert("http.method".to_string(), method.clone());
        attributes.insert("http.path".to_string(), path.clone());
        (
            SemanticActionKind::HttpMessage,
            format!("{method} {path}"),
            attributes,
        )
    };
    for line in lines {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        attributes.insert(
            format!("http.header.{}", key.trim().to_ascii_lowercase()),
            value.trim().to_string(),
        );
    }
    let declared_length = attributes
        .get("http.header.content-length")
        .and_then(|value| value.parse::<usize>().ok());
    let chunked = attributes
        .get("http.header.transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"));
    let body_available = bytes.len().saturating_sub(header_end);
    let chunked_complete = !chunked || chunked_body_complete(&bytes[header_end..]);
    let body_complete = declared_length.is_none_or(|length| body_available >= length)
        && chunked_complete
        && !matches!(
            segment.content_state,
            PayloadContentState::Truncated | PayloadContentState::Loss
        );
    let completeness = if body_complete {
        SemanticActionCompleteness::Complete
    } else {
        SemanticActionCompleteness::Partial
    };
    attributes.insert("http.headers".to_string(), header_text.to_string());
    attributes.insert("http.body_bytes".to_string(), body_available.to_string());
    attributes.insert(
        "http.content_state".to_string(),
        if body_complete { "complete" } else { "partial" }.to_string(),
    );
    attributes.insert(
        "censorscope.action.kind".to_string(),
        kind.as_str().to_string(),
    );
    let direction = match segment.direction {
        PayloadDirection::Outbound => "outbound",
        PayloadDirection::Inbound => "inbound",
        _ => "unknown",
    };
    attributes.insert("http.direction".to_string(), direction.to_string());
    Some(SemanticAction {
        action_id: format!(
            "payload:{}:{}:{}",
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
        status: if matches!(segment.content_state, PayloadContentState::Loss) {
            SemanticActionStatus::Unknown
        } else {
            SemanticActionStatus::Success
        },
        completeness,
        confidence_millis: (completeness == SemanticActionCompleteness::Complete).then_some(900),
        attributes,
        evidence: vec![SemanticEvidence {
            kind: SemanticEvidenceKind::PayloadSegment,
            id: segment.sequence,
            role: "http.plaintext".to_string(),
        }],
        session_id: session,
    })
}

fn chunked_body_complete(body: &[u8]) -> bool {
    // A chunked body ends with a zero-size chunk plus an optional trailer and
    // blank line; only that terminal framing makes it complete.
    let mut cursor = 0;
    loop {
        let Some(relative_end) = body[cursor..]
            .windows(2)
            .position(|window| window == b"\r\n")
        else {
            return false;
        };
        let line_end = cursor + relative_end;
        let line = &body[cursor..line_end];
        let size_text = line
            .split(|byte| *byte == b';')
            .next()
            .and_then(|value| std::str::from_utf8(value).ok())
            .map(str::trim);
        let Ok(size) = size_text.unwrap_or_default().to_string().parse::<usize>() else {
            return false;
        };
        cursor = line_end + 2;
        if size == 0 {
            return body[cursor..].starts_with(b"\r\n")
                || body[cursor..]
                    .windows(4)
                    .any(|window| window == b"\r\n\r\n");
        }
        let Some(data_end) = cursor.checked_add(size) else {
            return false;
        };
        if data_end + 2 > body.len() || &body[data_end..data_end + 2] != b"\r\n" {
            return false;
        }
        cursor = data_end + 2;
    }
}

fn project_http2_payload(
    segment: &PayloadSegment,
    bytes: &[u8],
    session: Option<SessionIdentity>,
) -> Option<SemanticAction> {
    const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
    let mut cursor = if bytes.starts_with(PREFACE) {
        PREFACE.len()
    } else {
        0
    };
    let mut frames = 0_u64;
    let mut stream_id = None;
    let mut headers = 0_u64;
    let mut data_bytes = 0_u64;
    while cursor + 9 <= bytes.len() {
        let header = &bytes[cursor..cursor + 9];
        let length =
            (usize::from(header[0]) << 16) | (usize::from(header[1]) << 8) | usize::from(header[2]);
        let end = cursor.checked_add(9)?.checked_add(length)?;
        if end > bytes.len() {
            break;
        }
        let frame_type = header[3];
        let id = (u32::from(header[5] & 0x7f) << 24)
            | (u32::from(header[6]) << 16)
            | (u32::from(header[7]) << 8)
            | u32::from(header[8]);
        if id != 0 && stream_id.is_none() {
            stream_id = Some(id);
        }
        match frame_type {
            0x0 => data_bytes = data_bytes.saturating_add(length as u64),
            0x1 | 0x9 => headers = headers.saturating_add(1),
            _ => {}
        }
        frames += 1;
        cursor = end;
    }
    if frames == 0 || (headers == 0 && data_bytes == 0) {
        return None;
    }
    let complete = cursor == bytes.len()
        && !matches!(
            segment.content_state,
            PayloadContentState::Truncated | PayloadContentState::Loss
        );
    let completeness = if complete {
        SemanticActionCompleteness::Complete
    } else {
        SemanticActionCompleteness::Partial
    };
    let mut attributes = BTreeMap::new();
    attributes.insert(
        "censorscope.action.kind".to_string(),
        "http.message".to_string(),
    );
    attributes.insert("http.protocol".to_string(), "h2".to_string());
    attributes.insert("http.frame_count".to_string(), frames.to_string());
    attributes.insert("http.headers_frames".to_string(), headers.to_string());
    attributes.insert("http.data_bytes".to_string(), data_bytes.to_string());
    if let Some(id) = stream_id {
        attributes.insert("http.stream_id".to_string(), id.to_string());
    }
    Some(SemanticAction {
        action_id: format!(
            "payload:{}:{}:{}",
            segment.trace_id.get(),
            segment.stream_key.as_deref().unwrap_or("unknown"),
            segment.sequence
        ),
        trace_id: segment.trace_id,
        kind: SemanticActionKind::HttpMessage,
        title: "HTTP/2 message".to_string(),
        start_time: segment.observed_at,
        end_time: segment.completed.then_some(segment.observed_at),
        process: segment.process,
        status: if matches!(segment.content_state, PayloadContentState::Loss) {
            SemanticActionStatus::Unknown
        } else {
            SemanticActionStatus::Success
        },
        completeness,
        confidence_millis: (completeness == SemanticActionCompleteness::Complete).then_some(850),
        attributes,
        evidence: vec![SemanticEvidence {
            kind: SemanticEvidenceKind::PayloadSegment,
            id: segment.sequence,
            role: "http2.frame".to_string(),
        }],
        session_id: session,
    })
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use model_core::ids::TraceId;
    use model_core::process::ProcessIdentity;

    use super::*;

    fn segment(bytes: &[u8], state: PayloadContentState) -> PayloadSegment {
        PayloadSegment {
            trace_id: TraceId::new(4),
            process: ProcessIdentity::new(9),
            session_id: None,
            call_id: None,
            observed_at: SystemTime::UNIX_EPOCH,
            source: model_core::payload::PayloadSourceBoundary::Uprobe,
            content_state: state,
            direction: PayloadDirection::Outbound,
            stream_key: Some("s".into()),
            sequence: 3,
            operation_id: None,
            offset: None,
            completed: true,
            original_size: bytes.len() as u64,
            captured_size: bytes.len() as u64,
            library: Some("openssl".into()),
            symbol: Some("SSL_write".into()),
            protocol_hint: Some("tls".into()),
            loss_reason: None,
            bytes: Some(bytes.to_vec()),
        }
    }

    #[test]
    fn parses_http_request_headers_without_copying_body() {
        let action = project_http1_payload(
            &segment(
                b"POST /v1/chat HTTP/1.1\r\nHost: api.test\r\nContent-Length: 20\r\n\r\n{}",
                PayloadContentState::Truncated,
            ),
            None,
        )
        .unwrap();
        assert_eq!(action.kind, SemanticActionKind::HttpMessage);
        assert_eq!(action.completeness, SemanticActionCompleteness::Partial);
        assert_eq!(action.attributes["http.method"], "POST");
        assert_eq!(action.attributes["http.header.host"], "api.test");
        assert!(!action.attributes.contains_key("http.body_text"));
    }

    #[test]
    fn metadata_only_payload_does_not_fabricate_http_action() {
        let mut value = segment(b"", PayloadContentState::MetadataOnly);
        value.bytes = None;
        assert!(project_http1_payload(&value, None).is_none());
    }

    #[test]
    fn projects_http2_frames_and_stream_id() {
        let mut bytes = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n".to_vec();
        bytes.extend_from_slice(&[0, 0, 3, 1, 4, 0, 0, 0, 1, b'a', b'b', b'c']);
        let action =
            project_http1_payload(&segment(&bytes, PayloadContentState::Complete), None).unwrap();
        assert_eq!(action.attributes["http.protocol"], "h2");
        assert_eq!(action.attributes["http.stream_id"], "1");
        assert_eq!(action.attributes["http.headers_frames"], "1");
    }

    #[test]
    fn chunked_http_body_is_partial_until_terminal_chunk() {
        let partial = segment(
            b"POST /upload HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n",
            PayloadContentState::Complete,
        );
        assert_eq!(
            project_http1_payload(&partial, None).unwrap().completeness,
            SemanticActionCompleteness::Partial
        );
        let complete = segment(
            b"POST /upload HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n",
            PayloadContentState::Complete,
        );
        assert_eq!(
            project_http1_payload(&complete, None).unwrap().completeness,
            SemanticActionCompleteness::Complete
        );
    }
}
