use std::collections::{BTreeMap, VecDeque};
use std::time::SystemTime;

use model_core::diagnostics::{CaptureDiagnostic, DiagnosticKind, DiagnosticSeverity};
use model_core::ids::CollectorName;
use model_core::payload::{PayloadContentState, PayloadDirection, PayloadSegment};
use semantic_action_contract::{
    SemanticAction, SemanticActionCompleteness, SemanticActionKind, SemanticActionLink,
    SemanticActionLinkConfidence, SemanticActionLinkRole, SemanticActionStatus, SemanticEvidence,
    SemanticEvidenceKind,
};
use serde_json::Value;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct StreamKey {
    trace_id: model_core::ids::TraceId,
    process: model_core::process::ProcessIdentity,
    stream_key: String,
}

#[derive(Default)]
pub struct LlmExchangeRuntime {
    requests: BTreeMap<StreamKey, VecDeque<SemanticAction>>,
    streaming_responses: BTreeMap<StreamKey, SemanticAction>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LlmExchangeOutput {
    pub actions: Vec<SemanticAction>,
    pub links: Vec<SemanticActionLink>,
    pub diagnostics: Vec<CaptureDiagnostic>,
}

impl LlmExchangeRuntime {
    pub fn observe(&mut self, mut actions: Vec<SemanticAction>) -> LlmExchangeOutput {
        let mut output = LlmExchangeOutput::default();
        for action in actions.drain(..) {
            match action.kind {
                SemanticActionKind::LlmRequest => {
                    let Some(key) = stream_key(&action) else {
                        output.actions.push(action);
                        continue;
                    };
                    let call = make_call(&action, None);
                    self.requests
                        .entry(key)
                        .or_default()
                        .push_back(action.clone());
                    output.actions.push(action);
                    output.actions.push(call);
                }
                SemanticActionKind::LlmResponse => {
                    let Some(key) = stream_key(&action) else {
                        output.actions.push(action);
                        continue;
                    };
                    let is_stream = action
                        .attributes
                        .get("llm.response.stream")
                        .is_some_and(|value| value == "true");
                    let mut action = match (is_stream, self.streaming_responses.remove(&key)) {
                        (true, Some(previous)) => merge_streaming_response(previous, action),
                        _ => action,
                    };
                    let in_progress =
                        is_stream && action.status == SemanticActionStatus::InProgress;
                    let candidate = self.requests.get(&key).and_then(VecDeque::front).cloned();
                    let provider_mismatch = candidate
                        .as_ref()
                        .is_some_and(|request| !providers_compatible(request, &action));
                    let matched = (!provider_mismatch).then_some(candidate).flatten();
                    if is_stream && let Some(request) = &matched {
                        action.action_id = format!("{}:llm.response", request.action_id);
                    }
                    if in_progress {
                        self.streaming_responses.insert(key.clone(), action.clone());
                    }
                    if matched.is_some() && !in_progress {
                        self.requests.get_mut(&key).and_then(VecDeque::pop_front);
                    }
                    if let Some(request) = matched {
                        if action.attributes.get("llm.provider_id").map(String::as_str)
                            == Some("unknown")
                        {
                            for key in ["llm.provider_id", "llm.protocol_id"] {
                                if let Some(value) = request.attributes.get(key) {
                                    action.attributes.insert(key.to_string(), value.clone());
                                }
                            }
                            action.attributes.insert(
                                "llm.parser.match_strength".to_string(),
                                "correlated".to_string(),
                            );
                        }
                        action
                            .attributes
                            .insert("llm.association_state".to_string(), "matched".to_string());
                        let call = make_call(&request, Some(&action));
                        output.links.push(link(
                            &call,
                            &request,
                            SemanticActionLinkRole::LlmCallRequest,
                        ));
                        output.links.push(link(
                            &call,
                            &action,
                            SemanticActionLinkRole::LlmCallResponse,
                        ));
                        output.actions.push(action);
                        output.actions.push(call);
                    } else {
                        if action.attributes.get("llm.provider_id").map(String::as_str)
                            == Some("unknown")
                        {
                            continue;
                        }
                        let association_state = if provider_mismatch {
                            "provider_mismatch"
                        } else {
                            "orphan"
                        };
                        let already_reported = action
                            .attributes
                            .get("llm.association_state")
                            .is_some_and(|value| value == association_state);
                        action.attributes.insert(
                            "llm.association_state".to_string(),
                            association_state.to_string(),
                        );
                        if !already_reported {
                            output.diagnostics.push(CaptureDiagnostic {
                                trace_id: action.trace_id,
                                observed_at: action.start_time,
                                collector: CollectorName::new("llm-semantic-runtime"),
                                kind: DiagnosticKind::CaptureGap,
                                severity: DiagnosticSeverity::Warning,
                                message: format!("llm_response_{association_state}"),
                                dedupe_key: Some(format!(
                                    "llm-response:{}:{}:{}:{}",
                                    action.trace_id.get(),
                                    action
                                        .attributes
                                        .get("payload.stream_key")
                                        .map(String::as_str)
                                        .unwrap_or("unknown"),
                                    action.action_id,
                                    association_state
                                )),
                                dropped: 0,
                                dropped_bytes: 0,
                            });
                        }
                        output.actions.push(action);
                    }
                    if self.requests.get(&key).is_some_and(VecDeque::is_empty) {
                        self.requests.remove(&key);
                    }
                }
                _ => output.actions.push(action),
            }
        }
        output
    }

    pub fn finalize_trace(&mut self, trace_id: model_core::ids::TraceId) -> Vec<SemanticAction> {
        let keys = self
            .requests
            .keys()
            .filter(|key| key.trace_id == trace_id)
            .cloned()
            .collect::<Vec<_>>();
        let mut calls = Vec::new();
        for key in keys {
            if let Some(requests) = self.requests.remove(&key) {
                calls.extend(requests.iter().map(|request| {
                    let mut call = make_call(request, None);
                    call.status = SemanticActionStatus::Error;
                    call.completeness = SemanticActionCompleteness::Partial;
                    call.end_time = Some(SystemTime::now());
                    call.attributes.insert(
                        "llm.call.finalized_on_trace_close".to_string(),
                        "true".to_string(),
                    );
                    call.attributes
                        .insert("llm.association_state".to_string(), "unclosed".to_string());
                    call
                }));
            }
        }
        self.streaming_responses
            .retain(|key, _| key.trace_id != trace_id);
        calls
    }
}

fn merge_streaming_response(
    previous: SemanticAction,
    mut current: SemanticAction,
) -> SemanticAction {
    current.start_time = previous.start_time.min(current.start_time);
    let previous_chunks = previous
        .attributes
        .get("llm.response.chunk_count")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let new_chunks = current
        .attributes
        .get("llm.response.chunk_count")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    for key in [
        "http.protocol",
        "http.status_code",
        "http.path",
        "http.method",
        "http.stream_id",
        "llm.provider_id",
        "llm.protocol_id",
        "llm.model",
    ] {
        if let Some(value) = previous.attributes.get(key) {
            current.attributes.insert(key.to_string(), value.clone());
        }
    }
    for (key, value) in previous.attributes {
        current.attributes.entry(key).or_insert(value);
    }
    current.attributes.insert(
        "llm.response.chunk_count".to_string(),
        previous_chunks.saturating_add(new_chunks).to_string(),
    );
    for evidence in previous.evidence {
        if !current.evidence.contains(&evidence) {
            current.evidence.push(evidence);
        }
    }
    current
}

pub fn project_llm_http_message(
    segment: &PayloadSegment,
    session: Option<model_core::process::SessionIdentity>,
) -> Vec<SemanticAction> {
    project_llm_http_message_with_diagnostics(segment, session).0
}

pub fn project_llm_http_message_with_diagnostics(
    segment: &PayloadSegment,
    session: Option<model_core::process::SessionIdentity>,
) -> (Vec<SemanticAction>, Vec<CaptureDiagnostic>) {
    project_llm_http_message_with_registry_diagnostics(
        segment,
        session,
        super::provider::builtin_provider_registry(),
    )
}

pub fn project_llm_http_message_with_registry_diagnostics(
    segment: &PayloadSegment,
    session: Option<model_core::process::SessionIdentity>,
    registry: &super::provider::ProviderRegistry,
) -> (Vec<SemanticAction>, Vec<CaptureDiagnostic>) {
    project_llm_http_message_with_decoder_diagnostics(segment, session, registry, None)
}

pub fn project_llm_http_message_with_decoder_diagnostics(
    segment: &PayloadSegment,
    session: Option<model_core::process::SessionIdentity>,
    registry: &super::provider::ProviderRegistry,
    decoder: Option<&mut super::http::HpackDecoder>,
) -> (Vec<SemanticAction>, Vec<CaptureDiagnostic>) {
    let messages = extract_llm_messages(segment, decoder);
    if messages.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut actions = Vec::new();
    let mut diagnostics = Vec::new();
    for message in messages {
        let (mut projected, mut message_diagnostics) =
            project_one_llm_http_message(segment, session.clone(), registry, message);
        actions.append(&mut projected);
        diagnostics.append(&mut message_diagnostics);
    }
    (actions, diagnostics)
}

pub fn project_llm_http2_messages_with_diagnostics(
    segment: &PayloadSegment,
    session: Option<model_core::process::SessionIdentity>,
    messages: Vec<super::http::Http2Message>,
) -> (Vec<SemanticAction>, Vec<CaptureDiagnostic>) {
    let mut actions = Vec::new();
    let mut diagnostics = Vec::new();
    for message in messages {
        let normalized = http2_llm_message(message);
        let (mut projected, mut message_diagnostics) = project_one_llm_http_message(
            segment,
            session.clone(),
            super::provider::builtin_provider_registry(),
            normalized,
        );
        actions.append(&mut projected);
        diagnostics.append(&mut message_diagnostics);
    }
    (actions, diagnostics)
}

fn project_one_llm_http_message(
    segment: &PayloadSegment,
    session: Option<model_core::process::SessionIdentity>,
    registry: &super::provider::ProviderRegistry,
    message: LlmHttpMessage,
) -> (Vec<SemanticAction>, Vec<CaptureDiagnostic>) {
    let Some(parsed) = parse_llm_body(&message.body) else {
        return (Vec::new(), Vec::new());
    };
    let provider = super::provider::match_provider_message_with_registry(
        registry,
        &parsed.value,
        segment.direction,
        message.path.as_deref(),
        &message.headers,
        message.status,
        parsed.event_type.as_deref(),
        parsed.terminal_marker.as_deref(),
    );
    let provider = match provider {
        super::provider::ProviderMatch::Matched(provider) => provider,
        super::provider::ProviderMatch::Ambiguous(providers) if !providers.is_empty() => {
            return (
                Vec::new(),
                vec![CaptureDiagnostic {
                    trace_id: segment.trace_id,
                    observed_at: segment.observed_at,
                    collector: CollectorName::new("llm-semantic-runtime"),
                    kind: DiagnosticKind::CaptureGap,
                    severity: DiagnosticSeverity::Warning,
                    message: format!("llm_provider_ambiguous:{}", providers.join(",")),
                    dedupe_key: Some(format!(
                        "llm-ambiguity:{}:{}:{}:{}",
                        segment.trace_id.get(),
                        segment.stream_key.as_deref().unwrap_or("unknown"),
                        segment.sequence,
                        providers.join(",")
                    )),
                    dropped: 0,
                    dropped_bytes: 0,
                }],
            );
        }
        super::provider::ProviderMatch::Ambiguous(_) => return (Vec::new(), Vec::new()),
    };
    let value = parsed.value;
    let stream = parsed.stream;
    let done = parsed.done || provider.done;
    let chunk_count = parsed.chunk_count;
    let object = value.as_object();
    let kind = provider.kind;
    let mut attributes = BTreeMap::new();
    attributes.insert(
        "censorscope.action.kind".to_string(),
        kind.as_str().to_string(),
    );
    attributes.insert(
        "llm.content_state".to_string(),
        if message.complete {
            "complete"
        } else {
            "partial"
        }
        .to_string(),
    );
    attributes.insert(
        "payload.stream_key".to_string(),
        segment
            .stream_key
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
    );
    if let Some(connection_key) = segment.stream_key.as_deref().and_then(tls_connection_key) {
        attributes.insert("payload.connection_key".to_string(), connection_key);
    }
    attributes.insert(
        "payload.sequence_start".to_string(),
        segment.sequence.to_string(),
    );
    attributes.insert("http.protocol".to_string(), message.protocol.clone());
    attributes.insert(
        "llm.provider_id".to_string(),
        provider.provider_id.to_string(),
    );
    attributes.insert(
        "llm.protocol_id".to_string(),
        provider.protocol_id.to_string(),
    );
    attributes.insert(
        "llm.parser.match_strength".to_string(),
        provider.match_strength.to_string(),
    );
    if let Some(stream_id) = message.stream_id {
        attributes.insert("http.stream_id".to_string(), stream_id.to_string());
    }
    if let Some(model) = provider.model {
        attributes.insert("llm.model".to_string(), model);
    }
    if let Some(finish_reason) = provider.finish_reason {
        attributes.insert("llm.response.finish_reason".to_string(), finish_reason);
    }
    if let Some(path) = &message.path {
        attributes.insert("http.path".to_string(), path.clone());
    }
    if let Some(method) = &message.method {
        attributes.insert("http.method".to_string(), method.clone());
    }
    if let Some(status) = message.status {
        attributes.insert("http.status_code".to_string(), status.to_string());
    }
    for (key, value) in &message.trailers {
        attributes.insert(format!("http.trailer.{key}"), value.clone());
    }
    if let Some(messages) = object
        .and_then(|object| object.get("messages"))
        .and_then(Value::as_array)
    {
        attributes.insert("llm.message_count".to_string(), messages.len().to_string());
    }
    if let Some(contents) = object
        .and_then(|object| object.get("contents"))
        .and_then(Value::as_array)
    {
        attributes.insert("llm.message_count".to_string(), contents.len().to_string());
    }
    if let Some(usage) = object
        .and_then(|object| object.get("usage"))
        .and_then(Value::as_object)
    {
        for key in ["prompt_tokens", "completion_tokens", "total_tokens"] {
            if let Some(value) = usage.get(key).and_then(Value::as_u64) {
                attributes.insert(format!("llm.usage.{key}"), value.to_string());
            }
        }
    }
    if stream {
        attributes.insert("llm.response.stream".to_string(), "true".to_string());
        attributes.insert("llm.response.done".to_string(), done.to_string());
        attributes.insert(
            "llm.response.chunk_count".to_string(),
            chunk_count.to_string(),
        );
    }
    let wire_complete = message.complete || (stream && done);
    let complete = wire_complete
        && matches!(segment.content_state, PayloadContentState::Complete)
        && (!stream || done);
    let stream_identity = message
        .stream_id
        .map(|id| {
            format!(
                "{}:h2:{id}",
                segment.stream_key.as_deref().unwrap_or("unknown")
            )
        })
        .unwrap_or_else(|| {
            segment
                .stream_key
                .as_deref()
                .unwrap_or("unknown")
                .to_string()
        });
    let action_id = if stream {
        format!(
            "payload:{}:{}:llm:response",
            segment.trace_id.get(),
            stream_identity
        )
    } else {
        format!(
            "payload:{}:{}:llm:{}",
            segment.trace_id.get(),
            stream_identity,
            segment.sequence
        )
    };
    (
        vec![SemanticAction {
            action_id,
            trace_id: segment.trace_id,
            kind,
            title: match kind {
                SemanticActionKind::LlmRequest => "LLM request",
                _ => "LLM response",
            }
            .to_string(),
            start_time: segment.observed_at,
            end_time: (complete || (!stream && segment.completed)).then_some(segment.observed_at),
            process: segment.process.clone(),
            status: if stream && !done {
                SemanticActionStatus::InProgress
            } else if !wire_complete || matches!(segment.content_state, PayloadContentState::Loss) {
                SemanticActionStatus::Unknown
            } else if kind == SemanticActionKind::LlmResponse
                && message.status.is_some_and(|status| status >= 400)
            {
                SemanticActionStatus::Error
            } else {
                SemanticActionStatus::Success
            },
            completeness: if complete {
                SemanticActionCompleteness::Complete
            } else {
                SemanticActionCompleteness::Partial
            },
            confidence_millis: complete.then_some(900),
            attributes,
            evidence: vec![SemanticEvidence {
                kind: SemanticEvidenceKind::PayloadSegment,
                id: segment.sequence,
                role: "llm.http_body".to_string(),
            }],
            session_id: session,
        }],
        Vec::new(),
    )
}

#[derive(Clone, Debug)]
struct LlmHttpMessage {
    body: Vec<u8>,
    protocol: String,
    stream_id: Option<u32>,
    complete: bool,
    method: Option<String>,
    path: Option<String>,
    status: Option<u16>,
    headers: BTreeMap<String, String>,
    trailers: BTreeMap<String, String>,
}

fn extract_llm_messages(
    segment: &PayloadSegment,
    decoder: Option<&mut super::http::HpackDecoder>,
) -> Vec<LlmHttpMessage> {
    if segment
        .protocol_hint
        .as_deref()
        .is_some_and(|hint| hint.eq_ignore_ascii_case("http2") || hint.eq_ignore_ascii_case("h2"))
    {
        let messages = if let Some(decoder) = decoder {
            super::http::extract_http2_messages_with_decoder(segment, decoder)
        } else {
            super::http::extract_http2_messages(segment)
        };
        return messages.into_iter().map(http2_llm_message).collect();
    }
    extract_llm_message(segment).into_iter().collect()
}

fn http2_llm_message(message: super::http::Http2Message) -> LlmHttpMessage {
    LlmHttpMessage {
        body: message.body,
        protocol: "h2".to_string(),
        stream_id: Some(message.stream_id),
        complete: message.complete,
        method: message.headers.get(":method").cloned(),
        path: message.headers.get(":path").cloned(),
        status: message
            .headers
            .get(":status")
            .and_then(|value| value.parse::<u16>().ok()),
        headers: message.headers,
        trailers: message.trailers,
    }
}

fn extract_llm_message(segment: &PayloadSegment) -> Option<LlmHttpMessage> {
    if segment
        .protocol_hint
        .as_deref()
        .is_some_and(|hint| hint.eq_ignore_ascii_case("websocket"))
    {
        let message = super::http::extract_websocket_message(segment)?;
        return Some(LlmHttpMessage {
            body: message.body,
            protocol: "websocket".to_string(),
            stream_id: None,
            complete: message.complete,
            method: None,
            path: None,
            status: None,
            headers: BTreeMap::new(),
            trailers: BTreeMap::new(),
        });
    }
    if let Some(message) = super::http::extract_http1_message(segment) {
        return Some(LlmHttpMessage {
            body: message.body,
            protocol: "http/1.x".to_string(),
            stream_id: None,
            complete: message.complete,
            method: message.method,
            path: message.path,
            status: message.status,
            headers: message.headers,
            trailers: BTreeMap::new(),
        });
    }
    if let Some(message) = super::http::extract_http2_message(segment) {
        let method = message.headers.get(":method").cloned();
        let path = message.headers.get(":path").cloned();
        let status = message
            .headers
            .get(":status")
            .and_then(|value| value.parse::<u16>().ok());
        return Some(LlmHttpMessage {
            body: message.body,
            protocol: "h2".to_string(),
            stream_id: Some(message.stream_id),
            complete: message.complete,
            method,
            path,
            status,
            headers: message.headers,
            trailers: message.trailers,
        });
    }
    let body = segment.bytes.as_deref()?;
    let text = std::str::from_utf8(body).ok()?;
    let is_sse = segment.direction == PayloadDirection::Inbound
        && text
            .lines()
            .any(|line| line.starts_with("data:") || line.starts_with("event:"));
    is_sse.then(|| LlmHttpMessage {
        body: body.to_vec(),
        protocol: "sse".to_string(),
        stream_id: None,
        complete: matches!(segment.content_state, PayloadContentState::Complete),
        method: None,
        path: None,
        status: None,
        headers: BTreeMap::new(),
        trailers: BTreeMap::new(),
    })
}

struct ParsedLlmBody {
    value: Value,
    stream: bool,
    done: bool,
    chunk_count: usize,
    event_type: Option<String>,
    terminal_marker: Option<String>,
}

fn parse_llm_body(body: &[u8]) -> Option<ParsedLlmBody> {
    if let Ok(value) = serde_json::from_slice::<Value>(body) {
        return Some(ParsedLlmBody {
            value,
            stream: false,
            done: true,
            chunk_count: 1,
            event_type: None,
            terminal_marker: None,
        });
    }
    let text = std::str::from_utf8(body).ok()?;
    let mut values = Vec::new();
    let mut done = false;
    let mut event_type = None;
    let mut terminal_marker = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("event:") {
            event_type = Some(value.trim());
            if value.trim().eq_ignore_ascii_case("message_stop")
                || value.trim().eq_ignore_ascii_case("done")
            {
                done = true;
                terminal_marker = Some(value.trim().to_string());
            }
        } else if let Some(value) = line.strip_prefix("data:") {
            let value = value.trim();
            if value == "[DONE]" {
                done = true;
                terminal_marker = Some(value.to_string());
                continue;
            }
            if let Ok(json) = serde_json::from_str::<Value>(value) {
                if json
                    .get("choices")
                    .and_then(Value::as_array)
                    .is_some_and(|choices| {
                        choices.iter().any(|choice| {
                            choice
                                .get("finish_reason")
                                .is_some_and(|reason| !reason.is_null())
                                || choice
                                    .get("delta")
                                    .and_then(|delta| delta.get("finish_reason"))
                                    .is_some_and(|reason| !reason.is_null())
                        })
                    })
                {
                    done = true;
                }
                values.push(json);
            }
        }
    }
    if values.is_empty() && !done {
        return None;
    }
    let value = values
        .pop()
        .unwrap_or_else(|| Value::Object(Default::default()));
    Some(ParsedLlmBody {
        value,
        stream: true,
        done,
        chunk_count: values.len() + 1,
        event_type: event_type.map(str::to_string),
        terminal_marker,
    })
}

fn stream_key(action: &SemanticAction) -> Option<StreamKey> {
    let payload_stream = action
        .attributes
        .get("payload.connection_key")
        .or_else(|| action.attributes.get("payload.stream_key"))?
        .clone();
    let stream_key = action
        .attributes
        .get("http.stream_id")
        .map(|id| format!("{payload_stream}:h2:{id}"))
        .unwrap_or(payload_stream);
    Some(StreamKey {
        trace_id: action.trace_id,
        process: action.process.clone(),
        stream_key,
    })
}

fn tls_connection_key(stream_key: &str) -> Option<String> {
    let mut parts = stream_key.split(':');
    let protocol = parts.next()?;
    let pid = parts.next()?;
    let connection = parts.next()?;
    let direction = parts.next()?;
    let Ok(direction) = direction.parse::<u8>() else {
        return None;
    };
    if parts.next().is_some()
        || protocol != "tls"
        || pid.parse::<u32>().is_err()
        || connection.parse::<u64>().is_err()
        || direction > 1
    {
        return None;
    }
    Some(format!("tls:{pid}:{connection}"))
}

fn providers_compatible(request: &SemanticAction, response: &SemanticAction) -> bool {
    let request_provider = request
        .attributes
        .get("llm.provider_id")
        .map(String::as_str);
    let response_provider = response
        .attributes
        .get("llm.provider_id")
        .map(String::as_str);
    match (request_provider, response_provider) {
        (Some(left), Some(right)) if left == right => true,
        (Some("openai-compatible"), Some("openai"))
        | (Some("openai"), Some("openai-compatible")) => true,
        (Some("unknown"), _) | (_, Some("unknown")) => true,
        (None, _) | (_, None) => true,
        _ => false,
    }
}

fn make_call(request: &SemanticAction, response: Option<&SemanticAction>) -> SemanticAction {
    let mut attributes = BTreeMap::new();
    attributes.insert(
        "llm.call.request_action_id".to_string(),
        request.action_id.clone(),
    );
    if let Some(response) = response {
        attributes.insert(
            "llm.call.response_action_id".to_string(),
            response.action_id.clone(),
        );
    }
    if let Some(model) = request.attributes.get("llm.model") {
        attributes.insert("llm.model".to_string(), model.clone());
    }
    for key in ["llm.provider_id", "llm.protocol_id"] {
        if let Some(value) = request.attributes.get(key) {
            attributes.insert(key.to_string(), value.clone());
        }
    }
    attributes.insert(
        "llm.association_state".to_string(),
        if response.is_some() {
            "matched"
        } else {
            "pending"
        }
        .to_string(),
    );
    let mut evidence = request.evidence.clone();
    if let Some(response) = response {
        for item in &response.evidence {
            if !evidence.contains(item) {
                evidence.push(item.clone());
            }
        }
    }
    let completeness = match response {
        Some(response)
            if request.completeness == SemanticActionCompleteness::Complete
                && response.completeness == SemanticActionCompleteness::Complete =>
        {
            SemanticActionCompleteness::Complete
        }
        _ => SemanticActionCompleteness::Partial,
    };
    SemanticAction {
        action_id: format!("{}:llm.call", request.action_id),
        trace_id: request.trace_id,
        kind: SemanticActionKind::LlmCall,
        title: request
            .attributes
            .get("llm.model")
            .map(|model| format!("LLM call {model}"))
            .unwrap_or_else(|| "LLM call".to_string()),
        start_time: request.start_time,
        end_time: response.and_then(|action| action.end_time),
        process: request.process.clone(),
        status: response
            .map(|action| action.status)
            .unwrap_or(SemanticActionStatus::InProgress),
        completeness,
        confidence_millis: None,
        attributes,
        evidence,
        session_id: request.session_id.clone(),
    }
}

fn link(
    call: &SemanticAction,
    child: &SemanticAction,
    role: SemanticActionLinkRole,
) -> SemanticActionLink {
    SemanticActionLink {
        trace_id: call.trace_id,
        source_action_id: call.action_id.clone(),
        target_action_id: child.action_id.clone(),
        role,
        confidence: SemanticActionLinkConfidence::Observed,
        valid: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_core::ids::TraceId;
    use model_core::payload::{PayloadContentState, PayloadSourceBoundary};
    use model_core::process::ProcessIdentity;

    fn segment(direction: PayloadDirection, bytes: &[u8], sequence: u64) -> PayloadSegment {
        PayloadSegment {
            trace_id: TraceId::new(42),
            process: ProcessIdentity::new(7),
            session_id: None,
            call_id: None,
            observed_at: SystemTime::UNIX_EPOCH,
            source: PayloadSourceBoundary::Uprobe,
            content_state: PayloadContentState::Complete,
            direction,
            stream_key: Some("tls:1:1:1".to_string()),
            sequence,
            operation_id: None,
            offset: None,
            completed: true,
            original_size: bytes.len() as u64,
            captured_size: bytes.len() as u64,
            library: None,
            symbol: None,
            protocol_hint: Some("http".to_string()),
            loss_reason: None,
            bytes: Some(bytes.to_vec()),
        }
    }

    #[test]
    fn http_request_and_response_form_one_call() {
        let request = segment(
            PayloadDirection::Outbound,
            b"POST /v1/chat HTTP/1.1\r\nContent-Length: 46\r\n\r\n{\"model\":\"test\",\"messages\":[{\"role\":\"user\"}]} ",
            1,
        );
        let response = segment(
            PayloadDirection::Inbound,
            b"HTTP/1.1 200 OK\r\nContent-Length: 25\r\n\r\n{\"choices\":[],\"usage\":{}}",
            2,
        );
        let mut runtime = LlmExchangeRuntime::default();
        let first = runtime.observe(project_llm_http_message(&request, None));
        assert!(
            first
                .actions
                .iter()
                .any(|action| action.kind == SemanticActionKind::LlmCall)
        );
        let second = runtime.observe(project_llm_http_message(&response, None));
        let call = second
            .actions
            .iter()
            .find(|action| action.kind == SemanticActionKind::LlmCall)
            .expect("response closes the call");
        assert_eq!(call.status, SemanticActionStatus::Success);
        assert_eq!(second.links.len(), 2);
    }

    #[test]
    fn directional_tls_streams_share_one_exchange_key() {
        let mut request = segment(
            PayloadDirection::Outbound,
            b"POST /v1/chat HTTP/1.1\r\nContent-Length: 30\r\n\r\n{\"model\":\"test\",\"messages\":[]}",
            1,
        );
        request.stream_key = Some("tls:123:456:0".to_string());
        let mut response = segment(
            PayloadDirection::Inbound,
            b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\n\r\n{\"choices\":[]}",
            1,
        );
        response.stream_key = Some("tls:123:456:1".to_string());

        let request_actions = project_llm_http_message(&request, None);
        assert_eq!(
            request_actions[0].attributes["payload.connection_key"],
            "tls:123:456"
        );
        let mut runtime = LlmExchangeRuntime::default();
        runtime.observe(request_actions);
        let output = runtime.observe(project_llm_http_message(&response, None));
        assert_eq!(
            output
                .actions
                .iter()
                .find(|action| action.kind == SemanticActionKind::LlmCall)
                .map(|action| action.status),
            Some(SemanticActionStatus::Success)
        );
    }

    #[test]
    fn open_call_is_error_on_trace_finalize() {
        let request = segment(
            PayloadDirection::Outbound,
            b"POST /v1/chat HTTP/1.1\r\nContent-Length: 30\r\n\r\n{\"model\":\"test\",\"messages\":[]}",
            1,
        );
        let mut runtime = LlmExchangeRuntime::default();
        runtime.observe(project_llm_http_message(&request, None));
        let finalized = runtime.finalize_trace(TraceId::new(42));
        assert_eq!(finalized.len(), 1);
        assert_eq!(finalized[0].status, SemanticActionStatus::Error);
        assert_eq!(
            finalized[0].completeness,
            SemanticActionCompleteness::Partial
        );
    }

    #[test]
    fn sse_response_stays_open_until_done_marker() {
        let request = segment(
            PayloadDirection::Outbound,
            b"POST /v1/chat HTTP/1.1\r\nContent-Length: 30\r\n\r\n{\"model\":\"test\",\"messages\":[]}",
            1,
        );
        let chunk = segment(
            PayloadDirection::Inbound,
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
            2,
        );
        let done = segment(
            PayloadDirection::Inbound,
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: [DONE]\n\n",
            3,
        );
        let mut runtime = LlmExchangeRuntime::default();
        runtime.observe(project_llm_http_message(&request, None));
        let open = runtime.observe(project_llm_http_message(&chunk, None));
        assert_eq!(
            open.actions
                .iter()
                .find(|action| action.kind == SemanticActionKind::LlmResponse)
                .map(|action| action.status),
            Some(SemanticActionStatus::InProgress)
        );
        let closed = runtime.observe(project_llm_http_message(&done, None));
        assert_eq!(
            closed
                .actions
                .iter()
                .find(|action| action.kind == SemanticActionKind::LlmCall)
                .map(|action| action.status),
            Some(SemanticActionStatus::Success)
        );
    }

    #[test]
    fn bare_sse_records_update_one_response_until_done() {
        let request = segment(
            PayloadDirection::Outbound,
            b"POST /v1/chat HTTP/1.1\r\nContent-Length: 30\r\n\r\n{\"model\":\"test\",\"messages\":[]}",
            1,
        );
        let first = segment(
            PayloadDirection::Inbound,
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"a\"},\"finish_reason\":null}]}\n\n",
            2,
        );
        let token = segment(
            PayloadDirection::Inbound,
            b"data: {\"choices\":[{\"delta\":{\"content\":\"b\"},\"finish_reason\":null}]}\n\n",
            3,
        );
        let done = segment(PayloadDirection::Inbound, b"data: [DONE]\n\n", 4);
        let mut runtime = LlmExchangeRuntime::default();
        runtime.observe(project_llm_http_message(&request, None));
        runtime.observe(project_llm_http_message(&first, None));
        runtime.observe(project_llm_http_message(&token, None));
        let closed = runtime.observe(project_llm_http_message(&done, None));
        let response = closed
            .actions
            .iter()
            .find(|action| action.kind == SemanticActionKind::LlmResponse)
            .expect("terminal response");
        assert_eq!(response.status, SemanticActionStatus::Success);
        assert_eq!(response.attributes["llm.response.chunk_count"], "3");
        assert_eq!(response.attributes["http.protocol"], "http/1.x");
        assert_eq!(response.evidence.len(), 3);
        assert_eq!(
            closed
                .actions
                .iter()
                .find(|action| action.kind == SemanticActionKind::LlmCall)
                .map(|action| action.status),
            Some(SemanticActionStatus::Success)
        );
    }

    #[test]
    fn sequential_streaming_calls_on_one_connection_keep_distinct_responses() {
        let first_request = segment(
            PayloadDirection::Outbound,
            b"POST /v1/chat/completions HTTP/1.1\r\nContent-Length: 31\r\n\r\n{\"model\":\"first\",\"messages\":[]}",
            1,
        );
        let first_response = segment(
            PayloadDirection::Inbound,
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n",
            2,
        );
        let second_request = segment(
            PayloadDirection::Outbound,
            b"POST /v1/chat/completions HTTP/1.1\r\nContent-Length: 32\r\n\r\n{\"model\":\"second\",\"messages\":[]}",
            3,
        );
        let second_response = segment(
            PayloadDirection::Inbound,
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n",
            4,
        );
        let mut runtime = LlmExchangeRuntime::default();
        runtime.observe(project_llm_http_message(&first_request, None));
        let first = runtime.observe(project_llm_http_message(&first_response, None));
        runtime.observe(project_llm_http_message(&second_request, None));
        let second = runtime.observe(project_llm_http_message(&second_response, None));
        let response_id = |output: &LlmExchangeOutput| {
            output
                .actions
                .iter()
                .find(|action| action.kind == SemanticActionKind::LlmResponse)
                .map(|action| action.action_id.clone())
                .expect("streaming response")
        };
        assert_ne!(response_id(&first), response_id(&second));
    }

    #[test]
    fn http2_json_request_and_response_match_by_stream_id() {
        let request_body = br#"{"model":"h2-test","messages":[]}"#;
        let response_body = br#"{"choices":[]}"#;
        let request = h2_segment(PayloadDirection::Outbound, request_body, 1, 1);
        let response = h2_segment(PayloadDirection::Inbound, response_body, 1, 2);
        let mut runtime = LlmExchangeRuntime::default();
        let first = runtime.observe(project_llm_http_message(&request, None));
        assert!(
            first
                .actions
                .iter()
                .any(|action| { action.kind == SemanticActionKind::LlmRequest })
        );
        let second = runtime.observe(project_llm_http_message(&response, None));
        assert_eq!(
            second
                .actions
                .iter()
                .find(|action| action.kind == SemanticActionKind::LlmCall)
                .map(|action| action.status),
            Some(SemanticActionStatus::Success)
        );
    }

    #[test]
    fn websocket_json_messages_form_one_call() {
        let request_body = br#"{"model":"ws","messages":[]}"#;
        let response_body = br#"{"choices":[]}"#;
        let request = websocket_segment(PayloadDirection::Outbound, request_body, 1);
        let response = websocket_segment(PayloadDirection::Inbound, response_body, 2);
        let mut runtime = LlmExchangeRuntime::default();
        let first = runtime.observe(project_llm_http_message(&request, None));
        assert!(
            first
                .actions
                .iter()
                .any(|action| action.kind == SemanticActionKind::LlmRequest)
        );
        let second = runtime.observe(project_llm_http_message(&response, None));
        assert_eq!(
            second
                .actions
                .iter()
                .find(|action| action.kind == SemanticActionKind::LlmCall)
                .map(|action| action.status),
            Some(SemanticActionStatus::Success)
        );
    }

    #[test]
    fn unmatched_response_is_explicitly_orphaned() {
        let response = segment(
            PayloadDirection::Inbound,
            b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\n\r\n{\"choices\":[]}",
            1,
        );
        let mut runtime = LlmExchangeRuntime::default();
        let output = runtime.observe(project_llm_http_message(&response, None));
        assert_eq!(
            output
                .actions
                .iter()
                .find(|action| action.kind == SemanticActionKind::LlmResponse)
                .and_then(|action| action.attributes.get("llm.association_state"))
                .map(String::as_str),
            Some("orphan")
        );
        assert_eq!(output.diagnostics.len(), 1);
        assert_eq!(output.diagnostics[0].message, "llm_response_orphan");
    }

    #[test]
    fn incompatible_providers_are_not_linked() {
        let request = segment(
            PayloadDirection::Outbound,
            b"POST /v1/messages HTTP/1.1\r\nanthropic-version: 2023-06-01\r\nContent-Length: 32\r\n\r\n{\"model\":\"claude\",\"messages\":[]}",
            1,
        );
        let response = segment(
            PayloadDirection::Inbound,
            b"HTTP/1.1 200 OK\r\nContent-Length: 17\r\n\r\n{\"candidates\":[]}",
            2,
        );
        let mut runtime = LlmExchangeRuntime::default();
        runtime.observe(project_llm_http_message(&request, None));
        let output = runtime.observe(project_llm_http_message(&response, None));
        assert!(
            output
                .actions
                .iter()
                .all(|action| action.kind != SemanticActionKind::LlmCall)
        );
        assert_eq!(
            output
                .actions
                .iter()
                .find(|action| action.kind == SemanticActionKind::LlmResponse)
                .and_then(|action| action.attributes.get("llm.association_state"))
                .map(String::as_str),
            Some("provider_mismatch")
        );
        assert!(output.links.is_empty());
        assert_eq!(
            output.diagnostics[0].message,
            "llm_response_provider_mismatch"
        );
    }

    #[test]
    fn http_error_is_only_promoted_when_a_request_is_open() {
        let request = segment(
            PayloadDirection::Outbound,
            b"POST /v1/chat/completions HTTP/1.1\r\nContent-Length: 30\r\n\r\n{\"model\":\"test\",\"messages\":[]}",
            1,
        );
        let body = br#"{"error":{"message":"rate limited","type":"rate_limit"}}"#;
        let wire = format!(
            "HTTP/1.1 429 Too Many Requests\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).expect("JSON")
        );
        let response = segment(PayloadDirection::Inbound, wire.as_bytes(), 2);

        let mut runtime = LlmExchangeRuntime::default();
        let uncorrelated = runtime.observe(project_llm_http_message(&response, None));
        assert!(uncorrelated.actions.is_empty());
        assert!(uncorrelated.diagnostics.is_empty());

        runtime.observe(project_llm_http_message(&request, None));
        let correlated = runtime.observe(project_llm_http_message(&response, None));
        let response = correlated
            .actions
            .iter()
            .find(|action| action.kind == SemanticActionKind::LlmResponse)
            .expect("correlated error response");
        assert_eq!(response.status, SemanticActionStatus::Error);
        assert_eq!(response.attributes["llm.provider_id"], "openai");
        assert_eq!(
            response.attributes["llm.parser.match_strength"],
            "correlated"
        );
        assert_eq!(
            correlated
                .actions
                .iter()
                .find(|action| action.kind == SemanticActionKind::LlmCall)
                .map(|action| action.status),
            Some(SemanticActionStatus::Error)
        );
    }

    fn h2_segment(
        direction: PayloadDirection,
        body: &[u8],
        stream_id: u32,
        sequence: u64,
    ) -> PayloadSegment {
        let len = body.len();
        let mut bytes = vec![
            ((len >> 16) & 0xff) as u8,
            ((len >> 8) & 0xff) as u8,
            (len & 0xff) as u8,
            0,
            1,
            ((stream_id >> 24) & 0x7f) as u8,
            (stream_id >> 16) as u8,
            (stream_id >> 8) as u8,
            stream_id as u8,
        ];
        bytes.extend_from_slice(body);
        let mut segment = segment(direction, &bytes, sequence);
        segment.protocol_hint = Some("http2".to_string());
        segment
    }

    fn websocket_segment(
        direction: PayloadDirection,
        body: &[u8],
        sequence: u64,
    ) -> PayloadSegment {
        let mut bytes = vec![0x81, body.len() as u8];
        bytes.extend_from_slice(body);
        let mut segment = segment(direction, &bytes, sequence);
        segment.protocol_hint = Some("websocket".to_string());
        segment
    }
}
