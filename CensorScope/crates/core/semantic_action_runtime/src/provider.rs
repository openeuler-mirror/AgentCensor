use std::collections::BTreeMap;
use std::sync::OnceLock;

use model_core::payload::PayloadDirection;
use semantic_action_contract::SemanticActionKind;
use serde_json::Value;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderProjection {
    pub kind: SemanticActionKind,
    pub provider_id: &'static str,
    pub protocol_id: &'static str,
    pub match_strength: &'static str,
    pub model: Option<String>,
    pub finish_reason: Option<String>,
    pub done: bool,
}

#[derive(Clone, Debug)]
pub struct ProviderCandidate {
    pub projection: ProviderProjection,
    pub score: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderMatch {
    Matched(ProviderProjection),
    Ambiguous(Vec<&'static str>),
}

pub trait ProviderCodec: Send + Sync {
    fn project(
        &self,
        value: &Value,
        direction: PayloadDirection,
        path: Option<&str>,
        headers: &BTreeMap<String, String>,
        status: Option<u16>,
        event_type: Option<&str>,
        terminal_marker: Option<&str>,
    ) -> Option<ProviderCandidate>;
}

struct FunctionCodec {
    project_fn: fn(
        &Value,
        PayloadDirection,
        Option<&str>,
        &BTreeMap<String, String>,
        Option<u16>,
        Option<&str>,
        Option<&str>,
    ) -> Option<ProviderCandidate>,
}

impl ProviderCodec for FunctionCodec {
    fn project(
        &self,
        value: &Value,
        direction: PayloadDirection,
        path: Option<&str>,
        headers: &BTreeMap<String, String>,
        status: Option<u16>,
        event_type: Option<&str>,
        terminal_marker: Option<&str>,
    ) -> Option<ProviderCandidate> {
        (self.project_fn)(
            value,
            direction,
            path,
            headers,
            status,
            event_type,
            terminal_marker,
        )
    }
}

pub struct ProviderRegistry {
    codecs: Vec<Box<dyn ProviderCodec>>,
}

impl ProviderRegistry {
    pub fn builtin() -> Self {
        let mut registry = Self { codecs: Vec::new() };
        registry.register_fn(anthropic_candidate);
        registry.register_fn(gemini_candidate);
        registry.register_fn(openai_candidate);
        registry.register_fn(compatible_candidate);
        registry.register_fn(error_candidate);
        registry
    }

    pub fn register<C>(&mut self, codec: C)
    where
        C: ProviderCodec + 'static,
    {
        self.codecs.push(Box::new(codec));
    }

    fn register_fn(
        &mut self,
        project_fn: fn(
            &Value,
            PayloadDirection,
            Option<&str>,
            &BTreeMap<String, String>,
            Option<u16>,
            Option<&str>,
            Option<&str>,
        ) -> Option<ProviderCandidate>,
    ) {
        self.register(FunctionCodec { project_fn });
    }

    fn project(
        &self,
        value: &Value,
        direction: PayloadDirection,
        path: Option<&str>,
        headers: &BTreeMap<String, String>,
        status: Option<u16>,
        event_type: Option<&str>,
        terminal_marker: Option<&str>,
    ) -> Option<ProviderProjection> {
        let mut candidates = self
            .codecs
            .iter()
            .filter_map(|codec| {
                codec.project(
                    value,
                    direction,
                    path,
                    headers,
                    status,
                    event_type,
                    terminal_marker,
                )
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.score));
        let strongest = candidates.first()?;
        if candidates.get(1).is_some_and(|candidate| {
            candidate.score == strongest.score
                && candidate.projection.provider_id != strongest.projection.provider_id
        }) {
            return None;
        }
        Some(strongest.projection.clone())
    }
}

pub fn builtin_provider_registry() -> &'static ProviderRegistry {
    static REGISTRY: OnceLock<ProviderRegistry> = OnceLock::new();
    REGISTRY.get_or_init(ProviderRegistry::builtin)
}

pub fn project_provider_message_with_registry(
    registry: &ProviderRegistry,
    value: &Value,
    direction: PayloadDirection,
    path: Option<&str>,
    headers: &BTreeMap<String, String>,
    status: Option<u16>,
    event_type: Option<&str>,
    terminal_marker: Option<&str>,
) -> Option<ProviderProjection> {
    registry.project(
        value,
        direction,
        path,
        headers,
        status,
        event_type,
        terminal_marker,
    )
}

pub(crate) fn match_provider_message_with_registry(
    registry: &ProviderRegistry,
    value: &Value,
    direction: PayloadDirection,
    path: Option<&str>,
    headers: &BTreeMap<String, String>,
    status: Option<u16>,
    event_type: Option<&str>,
    terminal_marker: Option<&str>,
) -> ProviderMatch {
    let mut candidates = registry
        .codecs
        .iter()
        .filter_map(|codec| {
            codec.project(
                value,
                direction,
                path,
                headers,
                status,
                event_type,
                terminal_marker,
            )
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.score));
    let Some(strongest) = candidates.first() else {
        return ProviderMatch::Ambiguous(Vec::new());
    };
    let ambiguous = candidates
        .iter()
        .take_while(|candidate| candidate.score == strongest.score)
        .map(|candidate| candidate.projection.provider_id)
        .collect::<Vec<_>>();
    if ambiguous.len() > 1 {
        return ProviderMatch::Ambiguous(ambiguous);
    }
    ProviderMatch::Matched(strongest.projection.clone())
}

fn error_candidate(
    value: &Value,
    direction: PayloadDirection,
    _path: Option<&str>,
    _headers: &BTreeMap<String, String>,
    status: Option<u16>,
    _event_type: Option<&str>,
    _terminal_marker: Option<&str>,
) -> Option<ProviderCandidate> {
    let error = value.get("error")?.as_object()?;
    if direction != PayloadDirection::Inbound
        || !status.is_some_and(|status| status >= 400)
        || !error.contains_key("message")
    {
        return None;
    }
    Some(candidate(
        direction,
        "unknown",
        "http_error_candidate",
        10,
        model(value),
        None,
        true,
    ))
}

fn anthropic_candidate(
    value: &Value,
    direction: PayloadDirection,
    path: Option<&str>,
    headers: &BTreeMap<String, String>,
    _status: Option<u16>,
    event_type: Option<&str>,
    _terminal_marker: Option<&str>,
) -> Option<ProviderCandidate> {
    let object = value.as_object()?;
    let path_match = path.is_some_and(|path| path.ends_with("/v1/messages"));
    let header_match = headers.contains_key("anthropic-version");
    let event_match = event_type.is_some_and(|event| {
        matches!(
            event,
            "message_start"
                | "content_block_start"
                | "content_block_delta"
                | "content_block_stop"
                | "message_delta"
                | "message_stop"
        )
    });
    let body_match = object
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.starts_with("message") || kind.starts_with("content_block"))
        || object.contains_key("stop_reason")
        || object
            .get("usage")
            .and_then(Value::as_object)
            .is_some_and(|usage| {
                usage.contains_key("input_tokens") || usage.contains_key("output_tokens")
            });
    let shape_match = match direction {
        PayloadDirection::Outbound => object.contains_key("messages"),
        PayloadDirection::Inbound => {
            body_match
                || event_match
                || (object.contains_key("content") && object.contains_key("role"))
        }
        _ => false,
    };
    if !shape_match || !(path_match || header_match || body_match || event_match) {
        return None;
    }
    let finish_reason = object
        .get("stop_reason")
        .and_then(Value::as_str)
        .or_else(|| {
            object
                .get("delta")
                .and_then(|delta| delta.get("stop_reason"))
                .and_then(Value::as_str)
        })
        .map(str::to_string);
    Some(candidate(
        direction,
        "anthropic",
        "anthropic.messages",
        if path_match || header_match { 100 } else { 90 },
        model(value),
        finish_reason.clone(),
        finish_reason.is_some() || event_type == Some("message_stop"),
    ))
}

fn gemini_candidate(
    value: &Value,
    direction: PayloadDirection,
    path: Option<&str>,
    _headers: &BTreeMap<String, String>,
    _status: Option<u16>,
    _event_type: Option<&str>,
    _terminal_marker: Option<&str>,
) -> Option<ProviderCandidate> {
    let object = value.as_object()?;
    let path_match = path.is_some_and(|path| {
        path.contains(":generateContent") || path.contains(":streamGenerateContent")
    });
    let shape_match = match direction {
        PayloadDirection::Outbound => object.contains_key("contents"),
        PayloadDirection::Inbound => {
            object.contains_key("candidates") || object.contains_key("usageMetadata")
        }
        _ => false,
    };
    if !shape_match {
        return None;
    }
    let finish_reason = object
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("finishReason"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(candidate(
        direction,
        "google",
        "google.generate_content",
        if path_match { 100 } else { 90 },
        model(value),
        finish_reason.clone(),
        finish_reason.is_some(),
    ))
}

fn openai_candidate(
    value: &Value,
    direction: PayloadDirection,
    path: Option<&str>,
    _headers: &BTreeMap<String, String>,
    _status: Option<u16>,
    _event_type: Option<&str>,
    terminal_marker: Option<&str>,
) -> Option<ProviderCandidate> {
    let object = value.as_object()?;
    let path = path.unwrap_or_default();
    let chat_path = path.contains("/chat/completions");
    let responses_path = path.ends_with("/responses");
    let object_match = object
        .get("object")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.starts_with("chat.completion") || kind == "response");
    let shape_match = match direction {
        PayloadDirection::Outbound => {
            (chat_path && object.contains_key("messages"))
                || (responses_path && object.contains_key("input"))
        }
        PayloadDirection::Inbound => {
            object.contains_key("choices") || object_match || object.contains_key("output")
        }
        _ => false,
    };
    if !shape_match && terminal_marker != Some("[DONE]") {
        return None;
    }
    let finish_reason = openai_finish_reason(value);
    Some(candidate(
        direction,
        "openai",
        if responses_path || object.get("object").and_then(Value::as_str) == Some("response") {
            "openai.responses"
        } else {
            "openai.chat_completions"
        },
        if chat_path || responses_path || object_match {
            100
        } else {
            90
        },
        model(value),
        finish_reason.clone(),
        finish_reason.is_some() || terminal_marker == Some("[DONE]"),
    ))
}

fn compatible_candidate(
    value: &Value,
    direction: PayloadDirection,
    _path: Option<&str>,
    _headers: &BTreeMap<String, String>,
    _status: Option<u16>,
    _event_type: Option<&str>,
    terminal_marker: Option<&str>,
) -> Option<ProviderCandidate> {
    let object = value.as_object()?;
    let shape_match = match direction {
        PayloadDirection::Outbound => {
            object.contains_key("messages")
                || object.contains_key("input")
                || object.contains_key("prompt")
        }
        PayloadDirection::Inbound => {
            object.contains_key("choices")
                || object.contains_key("output")
                || (object.contains_key("content") && object.contains_key("role"))
        }
        _ => false,
    };
    if !shape_match && terminal_marker != Some("[DONE]") {
        return None;
    }
    let finish_reason = openai_finish_reason(value);
    Some(candidate(
        direction,
        "openai-compatible",
        "openai_compatible",
        50,
        model(value),
        finish_reason.clone(),
        finish_reason.is_some() || terminal_marker == Some("[DONE]"),
    ))
}

fn candidate(
    direction: PayloadDirection,
    provider_id: &'static str,
    protocol_id: &'static str,
    score: u8,
    model: Option<String>,
    finish_reason: Option<String>,
    done: bool,
) -> ProviderCandidate {
    ProviderCandidate {
        projection: ProviderProjection {
            kind: match direction {
                PayloadDirection::Outbound => SemanticActionKind::LlmRequest,
                _ => SemanticActionKind::LlmResponse,
            },
            provider_id,
            protocol_id,
            match_strength: if score >= 90 {
                "strong"
            } else if score >= 50 {
                "plausible"
            } else {
                "weak"
            },
            model,
            finish_reason,
            done,
        },
        score,
    }
}

fn model(value: &Value) -> Option<String> {
    value
        .get("model")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| message.get("model"))
                .and_then(Value::as_str)
        })
        .map(str::to_string)
}

fn openai_finish_reason(value: &Value) -> Option<String> {
    value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| {
            choice.get("finish_reason").or_else(|| {
                choice
                    .get("delta")
                    .and_then(|delta| delta.get("finish_reason"))
            })
        })
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CustomCodec;

    impl ProviderCodec for CustomCodec {
        fn project(
            &self,
            value: &Value,
            direction: PayloadDirection,
            _path: Option<&str>,
            _headers: &BTreeMap<String, String>,
            _status: Option<u16>,
            _event_type: Option<&str>,
            _terminal_marker: Option<&str>,
        ) -> Option<ProviderCandidate> {
            (direction == PayloadDirection::Outbound && value.get("custom_prompt").is_some()).then(
                || ProviderCandidate {
                    projection: ProviderProjection {
                        kind: SemanticActionKind::LlmRequest,
                        provider_id: "custom-provider",
                        protocol_id: "custom.v1",
                        match_strength: "strong",
                        model: Some("custom-model".to_string()),
                        finish_reason: None,
                        done: false,
                    },
                    score: 110,
                },
            )
        }
    }

    #[test]
    fn anthropic_path_wins_over_compatible_message_shape() {
        let value = serde_json::json!({"model":"claude-test","messages":[]});
        let projection = project_provider_message_with_registry(
            &ProviderRegistry::builtin(),
            &value,
            PayloadDirection::Outbound,
            Some("/v1/messages"),
            &BTreeMap::new(),
            None,
            None,
            None,
        )
        .expect("anthropic request");
        assert_eq!(projection.provider_id, "anthropic");
    }

    #[test]
    fn gemini_response_extracts_finish_reason() {
        let value = serde_json::json!({"candidates":[{"finishReason":"STOP"}]});
        let projection = project_provider_message_with_registry(
            &ProviderRegistry::builtin(),
            &value,
            PayloadDirection::Inbound,
            None,
            &BTreeMap::new(),
            None,
            None,
            None,
        )
        .expect("gemini response");
        assert_eq!(projection.provider_id, "google");
        assert_eq!(projection.finish_reason.as_deref(), Some("STOP"));
        assert!(projection.done);
    }

    #[test]
    fn equally_strong_conflicting_shapes_are_rejected() {
        let value = serde_json::json!({"choices":[],"candidates":[]});
        assert!(
            project_provider_message_with_registry(
                &ProviderRegistry::builtin(),
                &value,
                PayloadDirection::Inbound,
                None,
                &BTreeMap::new(),
                None,
                None,
                None,
            )
            .is_none()
        );
    }

    #[test]
    fn registry_accepts_custom_codec_without_changing_builtin_dispatch() {
        let mut registry = ProviderRegistry::builtin();
        registry.register(CustomCodec);
        let projection = project_provider_message_with_registry(
            &registry,
            &serde_json::json!({"custom_prompt":"hello"}),
            PayloadDirection::Outbound,
            Some("/custom/generate"),
            &BTreeMap::new(),
            None,
            None,
            None,
        )
        .expect("custom codec projection");
        assert_eq!(projection.provider_id, "custom-provider");
        assert_eq!(projection.protocol_id, "custom.v1");
    }
}
