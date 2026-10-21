//! Typed protocol adapters classify mutable text, structural data and business numbers.
use crate::{
    error::{Error, Result},
    json,
    references::TOKEN_RE,
};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Api {
    Chat,
    Messages,
    Responses,
}
impl Api {
    pub fn parse(name: &str) -> Result<Self> {
        match name.to_ascii_lowercase().as_str() {
            "chat" | "openai_chat" | "chat_completions" => Ok(Self::Chat),
            "messages" | "anthropic" | "anthropic_messages" => Ok(Self::Messages),
            "responses" | "openai_responses" => Ok(Self::Responses),
            _ => Err(Error::ProtocolBlocked),
        }
    }
    pub fn path(self) -> &'static str {
        match self {
            Self::Chat => "/v1/chat/completions",
            Self::Messages => "/v1/messages",
            Self::Responses => "/v1/responses",
        }
    }
}

pub trait RequestProtector {
    fn text(&self, text: &str, path: &str) -> Result<String>;
    fn structural(&self, text: &str, path: &str) -> Result<()>;
    fn number(&self, value: &serde_json::Number, path: &str) -> Result<()>;
}

const CHAT_FIELDS: &[&str] = &[
    "model",
    "messages",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "function_call",
    "functions",
    "response_format",
    "prediction",
    "web_search_options",
    "verbosity",
    "reasoning_effort",
    "reasoning",
    "stream",
    "stream_options",
    "user",
    "metadata",
    "store",
    "max_tokens",
    "max_completion_tokens",
    "temperature",
    "top_p",
    "frequency_penalty",
    "presence_penalty",
    "n",
    "seed",
    "logprobs",
    "top_logprobs",
    "stop",
    "service_tier",
    "modalities",
    "audio",
    "safety_identifier",
    "prompt_cache_key",
    "prompt_cache_retention",
    "thinking",
];
const MESSAGES_FIELDS: &[&str] = &[
    "model",
    "messages",
    "max_tokens",
    "system",
    "metadata",
    "stop_sequences",
    "stream",
    "temperature",
    "top_p",
    "top_k",
    "tools",
    "tool_choice",
    "thinking",
    "service_tier",
    "inference_geo",
    "cache_control",
];
const RESPONSES_FIELDS: &[&str] = &[
    "model",
    "input",
    "instructions",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "max_output_tokens",
    "temperature",
    "top_p",
    "text",
    "truncation",
    "metadata",
    "store",
    "stream",
    "stream_options",
    "reasoning",
    "user",
    "safety_identifier",
    "prompt_cache_key",
    "prompt_cache_retention",
    "include",
    "service_tier",
    "background",
    "prompt",
    "previous_response_id",
    "conversation",
];
const FORBIDDEN_TYPES: &[&str] = &[
    "image",
    "image_url",
    "input_image",
    "output_image",
    "input_audio",
    "audio",
    "output_audio",
    "input_file",
    "file",
    "container_upload",
    "video",
    "input_video",
    "thinking",
    "redacted_thinking",
    "reasoning",
    "compaction",
    "compaction_trigger",
    "item_reference",
    "computer_screenshot",
    "computer_call_output",
    "image_generation_call",
    "web_search",
    "web_search_preview",
    "file_search",
    "mcp",
    "computer",
    "code_interpreter",
    "base64",
    "url",
];
const FORBIDDEN_KEYS: &[&str] = &[
    "encrypted_content",
    "signature",
    "file_id",
    "file_data",
    "image_url",
    "audio_url",
    "video_url",
    "previous_response_id",
    "conversation",
    "attachments",
    "audio",
    "video",
];
const BLOCK_TYPES: &[&str] = &[
    "text",
    "input_text",
    "output_text",
    "refusal",
    "message",
    "tool_use",
    "tool_result",
    "function_call",
    "function_call_output",
    "custom_tool_call",
    "custom_tool_call_output",
    "document",
    "search_result",
    "server_tool_use",
    "mcp_tool_use",
    "mcp_tool_result",
];
const STRUCTURAL_KEYS: &[&str] = &[
    "model",
    "role",
    "type",
    "name",
    "tool_call_id",
    "call_id",
    "id",
    "object",
    "tool_use_id",
    "output_schema",
    "format",
    "finish_reason",
    "stop_reason",
];
const CONTROLS: &[&str] = &[
    "max_tokens",
    "max_completion_tokens",
    "max_output_tokens",
    "temperature",
    "top_p",
    "top_k",
    "frequency_penalty",
    "presence_penalty",
    "n",
    "seed",
    "logprobs",
    "top_logprobs",
    "budget_tokens",
];
const SCHEMA_SYNTAX: &[&str] = &[
    "type",
    "pattern",
    "format",
    "enum",
    "const",
    "required",
    "additionalProperties",
    "minItems",
    "maxItems",
    "minLength",
    "maxLength",
    "minProperties",
    "maxProperties",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "$ref",
];
const SCHEMA_COUNTS: &[&str] = &[
    "minItems",
    "maxItems",
    "minLength",
    "maxLength",
    "minProperties",
    "maxProperties",
];
const ROLES: &[&str] = &[
    "system",
    "developer",
    "user",
    "assistant",
    "tool",
    "function",
];

pub(crate) fn pointer(path: &str, key: &str) -> Result<String> {
    if key.chars().count() > 80 {
        return Err(Error::ProtocolBlocked);
    }
    Ok(format!(
        "{path}/{}",
        key.replace('~', "~0").replace('/', "~1")
    ))
}
fn populated(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        _ => true,
    }
}
pub(crate) fn validate_envelopes(value: &Value, path: &str, depth: usize) -> Result<()> {
    if depth > 64 {
        return Err(Error::ProtocolBlocked);
    }
    match value {
        Value::Object(object) => {
            if object
                .iter()
                .any(|(key, value)| FORBIDDEN_KEYS.contains(&key.as_str()) && populated(value))
            {
                return Err(Error::ProtocolBlocked);
            }
            if let Some(kind) = value.get("type").and_then(Value::as_str) {
                if FORBIDDEN_TYPES.contains(&kind) {
                    return Err(Error::ProtocolBlocked);
                }
                let mut parts = path.rsplit('/');
                let indexed = parts
                    .next()
                    .is_some_and(|part| part.parse::<usize>().is_ok());
                if indexed
                    && matches!(parts.next(), Some("content" | "input"))
                    && !BLOCK_TYPES.contains(&kind)
                {
                    return Err(Error::ProtocolBlocked);
                }
            }
            for (key, value) in object {
                validate_envelopes(value, &pointer(path, key)?, depth + 1)?;
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                validate_envelopes(value, &pointer(path, &index.to_string())?, depth + 1)?;
            }
        }
        _ => (),
    }
    Ok(())
}

fn structural(context: &impl RequestProtector, text: &str, path: &str) -> Result<()> {
    if TOKEN_RE.is_match(text) {
        return Err(Error::ProtocolBlocked);
    }
    context.structural(text, path)
}
fn schema_path(path: &str) -> bool {
    [
        "/tools/",
        "/functions/",
        "/response_format/",
        "/text/format/",
    ]
    .iter()
    .any(|prefix| path.starts_with(prefix))
        && path.split('/').any(|part| {
            matches!(
                part,
                "input_schema" | "parameters" | "schema" | "json_schema"
            )
        })
}
fn schema_syntax(context: &impl RequestProtector, value: &Value, path: &str) -> Result<()> {
    match value {
        Value::String(text) => structural(context, text, path)?,
        Value::Number(number) => structural(context, &number.to_string(), path)?,
        Value::Object(object) => {
            for (key, value) in object {
                structural(context, key, &pointer(path, "<key>")?)?;
                schema_syntax(context, value, &pointer(path, key)?)?;
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                schema_syntax(context, value, &pointer(path, &index.to_string())?)?;
            }
        }
        _ => (),
    }
    Ok(())
}

fn visit(
    context: &impl RequestProtector,
    value: &Value,
    path: &str,
    key: &str,
    in_arguments: bool,
) -> Result<Value> {
    let control = !in_arguments
        && CONTROLS.contains(&key)
        && (path.matches('/').count() == 1 || path.starts_with("/thinking/"));
    if control && key != "logprobs" && !value.is_number() {
        return Err(Error::ProtocolBlocked);
    }
    if !in_arguments && schema_path(path) && SCHEMA_SYNTAX.contains(&key) {
        if SCHEMA_COUNTS.contains(&key) && value.as_u64().is_none_or(|n| n > 1_000_000) {
            return Err(Error::ProtocolBlocked);
        }
        schema_syntax(context, value, path)?;
        return Ok(value.clone());
    }
    Ok(match value {
        Value::Object(object) => {
            let mut output = serde_json::Map::new();
            for (field, item) in object {
                structural(context, field, &pointer(path, "<key>")?)?;
                let business = field == "input"
                    && matches!(
                        value.get("type").and_then(Value::as_str),
                        Some("tool_use" | "server_tool_use" | "mcp_tool_use" | "custom_tool_call")
                    );
                output.insert(
                    field.clone(),
                    visit(
                        context,
                        item,
                        &pointer(path, field)?,
                        field,
                        in_arguments || business,
                    )?,
                );
            }
            output.into()
        }
        Value::Array(values) => values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                visit(
                    context,
                    value,
                    &pointer(path, &index.to_string())?,
                    key,
                    in_arguments,
                )
            })
            .collect::<Result<Vec<_>>>()?
            .into(),
        Value::String(text) if key == "arguments" && !in_arguments => {
            let arguments = json::from_slice(text.as_bytes())?;
            serde_json::to_string(&visit(context, &arguments, path, "", true)?)
                .map_err(|_| Error::ProtocolBlocked)?
                .into()
        }
        Value::String(text)
            if STRUCTURAL_KEYS.contains(&key)
                && !in_arguments
                && !path.starts_with("/metadata/") =>
        {
            structural(context, text, path)?;
            value.clone()
        }
        Value::String(text) => context.text(text, path)?.into(),
        Value::Number(number) => {
            if control {
                match key {
                    "max_tokens"
                    | "max_completion_tokens"
                    | "max_output_tokens"
                    | "n"
                    | "top_k"
                    | "budget_tokens"
                        if number.as_u64().is_none_or(|value| value < 1) =>
                    {
                        return Err(Error::ProtocolBlocked)
                    }
                    "temperature" | "top_p" | "frequency_penalty" | "presence_penalty" => {
                        let (low, high) = match key {
                            "temperature" => (0.0, 2.0),
                            "top_p" => (0.0, 1.0),
                            _ => (-2.0, 2.0),
                        };
                        if !number
                            .as_f64()
                            .is_some_and(|number| (low..=high).contains(&number))
                        {
                            return Err(Error::ProtocolBlocked);
                        }
                    }
                    _ => (),
                }
            } else {
                context.number(number, path)?;
            }
            value.clone()
        }
        _ => value.clone(),
    })
}

pub fn protect_request(api: Api, body: &Value, context: &impl RequestProtector) -> Result<Value> {
    let object = body.as_object().ok_or(Error::ProtocolBlocked)?;
    let allowed = match api {
        Api::Chat => CHAT_FIELDS,
        Api::Messages => MESSAGES_FIELDS,
        Api::Responses => RESPONSES_FIELDS,
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(Error::ProtocolBlocked);
    }
    if api == Api::Chat
        && object
            .get("thinking")
            .is_some_and(|value| value != &serde_json::json!({"type":"disabled"}))
    {
        return Err(Error::ProtocolBlocked);
    }
    if object
        .get("modalities")
        .is_some_and(|value| value != &serde_json::json!(["text"]))
    {
        return Err(Error::ProtocolBlocked);
    }
    if object
        .get("logprobs")
        .is_some_and(|v| !v.is_null() && v != false)
        || object
            .get("top_logprobs")
            .is_some_and(|v| !v.is_null() && v != 0)
    {
        return Err(Error::ProtocolBlocked);
    }
    if object
        .get("prompt")
        .and_then(Value::as_object)
        .is_some_and(|prompt| prompt.contains_key("id") || prompt.contains_key("version"))
    {
        return Err(Error::ProtocolBlocked);
    }
    if let Some(include) = object.get("include") {
        let include = include.as_array().ok_or(Error::ProtocolBlocked)?;
        if include.iter().any(|value| {
            value
                .as_str()
                .is_none_or(|s| s.contains("encrypted") || s.contains("logprobs"))
        }) {
            return Err(Error::ProtocolBlocked);
        }
    }
    match api {
        Api::Chat | Api::Messages => {
            let messages = body
                .get("messages")
                .and_then(Value::as_array)
                .ok_or(Error::ProtocolBlocked)?;
            if messages.iter().any(|message| {
                !message.is_object()
                    || message
                        .get("role")
                        .and_then(Value::as_str)
                        .is_none_or(|role| !ROLES.contains(&role))
            }) {
                return Err(Error::ProtocolBlocked);
            }
        }
        Api::Responses
            if body
                .get("input")
                .is_none_or(|value| !value.is_string() && !value.is_array()) =>
        {
            return Err(Error::ProtocolBlocked)
        }
        _ => (),
    }
    validate_envelopes(body, "", 0)?;
    visit(context, body, "", "", false)
}
