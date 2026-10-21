//! Incremental SSE framing with per-channel display and secret holdback.
use crate::{
    display::DisplayRestorer,
    error::{Error, Result},
    gate::PrivacyGate,
    json,
    protocols::Api,
    response::{self, ResponseRestorer},
};
use serde_json::{json, Value};
use std::collections::HashMap;
use zeroize::Zeroizing;

struct LiteralRedactor {
    literals: Vec<Zeroizing<String>>,
    max_length: usize,
    pending: Zeroizing<String>,
    mask: Vec<bool>,
    redacting: bool,
}
impl LiteralRedactor {
    fn new(literals: Vec<Zeroizing<String>>) -> Self {
        Self {
            max_length: literals.iter().map(|value| value.len()).max().unwrap_or(0),
            literals,
            pending: Zeroizing::new(String::new()),
            mask: Vec::new(),
            redacting: false,
        }
    }
    fn consume(&mut self, final_chunk: bool) -> String {
        let mut cutoff = if final_chunk {
            self.pending.len()
        } else {
            self.pending
                .len()
                .saturating_sub(self.max_length.saturating_sub(1))
        };
        while !self.pending.is_char_boundary(cutoff) {
            cutoff -= 1;
        }
        for literal in &self.literals {
            let mut cursor = 0;
            while let Some(found) = self.pending[cursor..].find(literal.as_str()) {
                let start = cursor + found;
                self.mask[start..start + literal.len()].fill(true);
                cursor = start
                    + self.pending[start..]
                        .chars()
                        .next()
                        .expect("nonempty secret")
                        .len_utf8();
            }
        }
        let mut output = String::new();
        let mut cursor = 0;
        while cursor < cutoff {
            let masked = self.mask[cursor];
            let mut end = cursor + 1;
            while end < cutoff && self.mask[end] == masked {
                end += 1;
            }
            if masked {
                if !self.redacting {
                    output.push_str("[REDACTED_SECRET]");
                }
                self.redacting = true;
            } else {
                output.push_str(&self.pending[cursor..end]);
                self.redacting = false;
            }
            cursor = end;
        }
        self.pending = Zeroizing::new(self.pending[cutoff..].to_owned());
        self.mask.drain(..cutoff);
        output
    }
    fn feed(&mut self, text: &str) -> String {
        self.pending.push_str(text);
        self.mask.resize(self.pending.len(), false);
        self.consume(false)
    }
    fn finish(&mut self) -> String {
        self.consume(true)
    }
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct Channel {
    id: String,
    field: String,
    part: u64,
}
impl Channel {
    fn new(id: &Value, field: &str, part: u64) -> Result<Self> {
        if !id.is_string() && !id.is_u64() {
            return Err(Error::ProtocolBlocked);
        }
        let id = id.to_string();
        if id.len() > 256 {
            return Err(Error::ResourceLimit);
        }
        Ok(Self {
            id,
            field: field.into(),
            part,
        })
    }
}
struct Text {
    display: DisplayRestorer,
    secrets: LiteralRedactor,
}
struct Tool {
    name: Zeroizing<String>,
    arguments: Zeroizing<String>,
}

pub struct SseTransformer {
    api: Api,
    epoch: u64,
    run_id: String,
    pending: Zeroizing<Vec<u8>>,
    text: HashMap<Channel, Text>,
    tools: HashMap<Channel, Tool>,
    tool_bytes: usize,
    sequence: u64,
    closed: bool,
}
impl SseTransformer {
    pub fn new(api: Api, gate: &PrivacyGate) -> Result<Self> {
        gate.validate()?;
        gate.secret_literals()?; // Enforce the snapshot's secret budget before headers are sent.
        Ok(Self {
            api,
            epoch: gate.epoch(),
            run_id: gate.run_id().into(),
            pending: Zeroizing::new(Vec::new()),
            text: HashMap::new(),
            tools: HashMap::new(),
            tool_bytes: 0,
            sequence: 0,
            closed: false,
        })
    }
    fn validate(&self, gate: &PrivacyGate) -> Result<()> {
        gate.validate()?;
        if gate.epoch() != self.epoch || gate.run_id() != self.run_id {
            return Err(Error::VaultDenied);
        }
        Ok(())
    }
    fn channel_limit(&self) -> Result<()> {
        if self.text.len() + self.tools.len() >= 64 {
            return Err(Error::ResourceLimit);
        }
        Ok(())
    }
    fn text(&mut self, channel: Channel, fragment: &str, gate: &PrivacyGate) -> Result<String> {
        if !self.text.contains_key(&channel) {
            self.channel_limit()?;
            self.text.insert(
                channel.clone(),
                Text {
                    display: DisplayRestorer::new(),
                    secrets: LiteralRedactor::new(gate.secret_literals()?),
                },
            );
        }
        let text = self.text.get_mut(&channel).ok_or(Error::ProtocolBlocked)?;
        text.display
            .feed(&text.secrets.feed(fragment), |reference| {
                gate.restore_token(reference)
            })
    }
    fn flush(&mut self, channel: &Channel, gate: &PrivacyGate) -> Result<String> {
        let Some(mut text) = self.text.remove(channel) else {
            return Ok(String::new());
        };
        Ok(text.display.feed(&text.secrets.finish(), |reference| {
            gate.restore_token(reference)
        })? + &text.display.finish()?)
    }
    fn new_tool(&mut self, channel: Channel, name: &str) -> Result<()> {
        if name.len() > 128 {
            return Err(Error::ResourceLimit);
        }
        let at_limit = self.text.len() + self.tools.len() >= 64;
        if let std::collections::hash_map::Entry::Vacant(entry) = self.tools.entry(channel) {
            if at_limit {
                return Err(Error::ResourceLimit);
            }
            entry.insert(Tool {
                name: Zeroizing::new(name.into()),
                arguments: Zeroizing::new(String::new()),
            });
        }
        Ok(())
    }
    fn args(&mut self, channel: Channel, fragment: &str, name: &str) -> Result<()> {
        self.new_tool(channel.clone(), "")?;
        let tool = self.tools.get_mut(&channel).ok_or(Error::ProtocolBlocked)?;
        if tool.name.len() + name.len() > 128
            || tool.arguments.len() + fragment.len() > 1_048_576
            || self.tool_bytes + fragment.len() > 4_194_304
        {
            return Err(Error::ResourceLimit);
        }
        tool.name.push_str(name);
        tool.arguments.push_str(fragment);
        self.tool_bytes += fragment.len();
        Ok(())
    }
    fn finish_args(&mut self, channel: &Channel, gate: &PrivacyGate) -> Result<(String, String)> {
        let tool = self.tools.remove(channel).ok_or(Error::ProtocolBlocked)?;
        self.tool_bytes -= tool.arguments.len();
        if tool.name.is_empty() {
            return Err(Error::ProtocolBlocked);
        }
        let parsed = gate.redact_json(&json::from_slice(tool.arguments.as_bytes())?)?;
        let approved = gate.redact_json(&gate.tool_arguments(&tool.name, &parsed)?)?;
        Ok((
            gate.redact_known_secrets(&tool.name)?,
            serde_json::to_string(&approved).map_err(|_| Error::ProtocolBlocked)?,
        ))
    }
    fn event(
        &mut self,
        mut data: Value,
        event: Option<&str>,
        gate: &PrivacyGate,
    ) -> Result<String> {
        data = gate.redact_json(&data)?;
        if self.api == Api::Responses {
            data["sequence_number"] = self.sequence.into();
            self.sequence += 1;
        }
        let mut output = String::new();
        if let Some(event) = event {
            output.push_str("event: ");
            output.push_str(event);
            output.push('\n');
        }
        output.push_str("data: ");
        output.push_str(&serde_json::to_string(&data).map_err(|_| Error::ProtocolBlocked)?);
        output.push_str("\n\n");
        Ok(output)
    }
    fn chat_delta(template: &Value, index: &Value, delta: Value) -> Value {
        let mut frame = template.clone();
        if let Some(object) = frame.as_object_mut() {
            object.remove("usage");
        }
        frame["choices"] = json!([{"index":index,"delta":delta,"finish_reason":null}]);
        frame
    }
    fn chat(&mut self, mut data: Value, gate: &PrivacyGate) -> Result<String> {
        let mut before = String::new();
        let count = data
            .get("choices")
            .and_then(Value::as_array)
            .ok_or(Error::ProtocolBlocked)?
            .len();
        for position in 0..count {
            let mut choice = data["choices"][position].clone();
            if !choice.is_object() {
                return Err(Error::ProtocolBlocked);
            }
            let index = choice.get("index").cloned().unwrap_or(json!(0));
            let mut delta = choice
                .get("delta")
                .filter(|value| !value.is_null())
                .cloned()
                .unwrap_or(json!({}));
            if delta
                .as_object()
                .ok_or(Error::ProtocolBlocked)?
                .keys()
                .any(|key| {
                    !["role", "content", "refusal", "tool_calls", "function_call"]
                        .contains(&key.as_str())
                })
            {
                return Err(Error::ProtocolBlocked);
            }
            for field in ["content", "refusal"] {
                if let Some(fragment) = delta.get(field).filter(|value| !value.is_null()) {
                    let fragment = fragment.as_str().ok_or(Error::ProtocolBlocked)?;
                    delta[field] = self
                        .text(Channel::new(&index, field, 0)?, fragment, gate)?
                        .into();
                }
            }
            if let Some(calls) = delta.get_mut("tool_calls").filter(|value| !value.is_null()) {
                for call in calls.as_array_mut().ok_or(Error::ProtocolBlocked)? {
                    let part = call
                        .get("index")
                        .unwrap_or(&json!(0))
                        .as_u64()
                        .ok_or(Error::ProtocolBlocked)?;
                    let function = call
                        .get_mut("function")
                        .and_then(Value::as_object_mut)
                        .ok_or(Error::ProtocolBlocked)?;
                    if function.contains_key("arguments") || function.contains_key("name") {
                        let arguments = function.remove("arguments").unwrap_or(json!(""));
                        let name = function.remove("name").unwrap_or(json!(""));
                        self.args(
                            Channel::new(&index, "tool", part)?,
                            arguments.as_str().ok_or(Error::ProtocolBlocked)?,
                            name.as_str().ok_or(Error::ProtocolBlocked)?,
                        )?;
                    }
                }
            }
            if let Some(function) = delta.get_mut("function_call") {
                let function = function.as_object_mut().ok_or(Error::ProtocolBlocked)?;
                let arguments = function.remove("arguments").unwrap_or(json!(""));
                let name = function.remove("name").unwrap_or(json!(""));
                self.args(
                    Channel::new(&index, "legacy", 0)?,
                    arguments.as_str().ok_or(Error::ProtocolBlocked)?,
                    name.as_str().ok_or(Error::ProtocolBlocked)?,
                )?;
            }
            choice["delta"] = delta;
            data["choices"][position] = choice.clone();
            if choice
                .get("finish_reason")
                .is_some_and(|value| !value.is_null())
            {
                let id = Channel::new(&index, "", 0)?.id;
                let channels: Vec<_> = self
                    .text
                    .keys()
                    .filter(|channel| channel.id == id)
                    .cloned()
                    .collect();
                for channel in channels {
                    let tail = self.flush(&channel, gate)?;
                    if !tail.is_empty() {
                        before.push_str(&self.event(
                            Self::chat_delta(&data, &index, json!({channel.field:tail})),
                            None,
                            gate,
                        )?);
                    }
                }
                let channels: Vec<_> = self
                    .tools
                    .keys()
                    .filter(|channel| channel.id == id)
                    .cloned()
                    .collect();
                for channel in channels {
                    let (name, arguments) = self.finish_args(&channel, gate)?;
                    let delta = if channel.field == "legacy" {
                        json!({"function_call":{"name":name,"arguments":arguments}})
                    } else {
                        json!({"tool_calls":[{"index":channel.part,"function":{"name":name,"arguments":arguments}}]})
                    };
                    before.push_str(&self.event(
                        Self::chat_delta(&data, &index, delta),
                        None,
                        gate,
                    )?);
                }
            }
        }
        Ok(before + &self.event(data, None, gate)?)
    }
    fn messages(&mut self, mut data: Value, gate: &PrivacyGate) -> Result<String> {
        let kind = data
            .get("type")
            .and_then(Value::as_str)
            .ok_or(Error::ProtocolBlocked)?
            .to_owned();
        let index = data.get("index").cloned().unwrap_or(json!(0));
        let mut before = String::new();
        match kind.as_str() {
            "content_block_start" => {
                match data["content_block"]["type"]
                    .as_str()
                    .ok_or(Error::ProtocolBlocked)?
                {
                    "tool_use" => {
                        let name = data["content_block"]["name"]
                            .as_str()
                            .ok_or(Error::ProtocolBlocked)?;
                        self.new_tool(Channel::new(&index, "tool", 0)?, name)?;
                        let input = &data["content_block"]["input"];
                        if !input.is_null() && input != &json!({}) {
                            self.args(Channel::new(&index, "tool", 0)?, &input.to_string(), "")?;
                            data["content_block"]["input"] = json!({});
                        }
                    }
                    "text" => {
                        data["content_block"]["text"] = self
                            .text(
                                Channel::new(&index, "text", 0)?,
                                data["content_block"]
                                    .get("text")
                                    .unwrap_or(&json!(""))
                                    .as_str()
                                    .ok_or(Error::ProtocolBlocked)?,
                                gate,
                            )?
                            .into()
                    }
                    _ => return Err(Error::ProtocolBlocked),
                }
            }
            "content_block_delta" => match data["delta"]["type"]
                .as_str()
                .ok_or(Error::ProtocolBlocked)?
            {
                "text_delta" => {
                    data["delta"]["text"] = self
                        .text(
                            Channel::new(&index, "text", 0)?,
                            data["delta"]
                                .get("text")
                                .unwrap_or(&json!(""))
                                .as_str()
                                .ok_or(Error::ProtocolBlocked)?,
                            gate,
                        )?
                        .into()
                }
                "input_json_delta" => {
                    self.args(
                        Channel::new(&index, "tool", 0)?,
                        data["delta"]
                            .get("partial_json")
                            .unwrap_or(&json!(""))
                            .as_str()
                            .ok_or(Error::ProtocolBlocked)?,
                        "",
                    )?;
                    return Ok(String::new());
                }
                _ => return Err(Error::ProtocolBlocked),
            },
            "content_block_stop" => {
                let tail = self.flush(&Channel::new(&index, "text", 0)?, gate)?;
                if !tail.is_empty() {
                    before.push_str(&self.event(json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":tail}}), Some("content_block_delta"), gate)?);
                }
                let channel = Channel::new(&index, "tool", 0)?;
                if self.tools.contains_key(&channel) {
                    let (_, arguments) = self.finish_args(&channel, gate)?;
                    before.push_str(&self.event(json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":arguments}}), Some("content_block_delta"), gate)?);
                }
            }
            "message_start" => {
                if data
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .and_then(Value::as_array)
                    .is_some_and(|content| !content.is_empty())
                {
                    data["message"] =
                        response::restore_response(Api::Messages, &data["message"], gate)?;
                }
            }
            "message_delta" => {
                if data["delta"]
                    .as_object()
                    .ok_or(Error::ProtocolBlocked)?
                    .keys()
                    .any(|key| !["stop_reason", "stop_sequence"].contains(&key.as_str()))
                {
                    return Err(Error::ProtocolBlocked);
                }
            }
            "message_stop" if !self.tools.is_empty() || !self.text.is_empty() => {
                return Err(Error::ProtocolBlocked)
            }
            "message_stop" | "ping" | "error" => (),
            _ => return Err(Error::ProtocolBlocked),
        }
        Ok(before + &self.event(data, Some(&kind), gate)?)
    }
    fn responses(&mut self, mut data: Value, gate: &PrivacyGate) -> Result<String> {
        let kind = data
            .get("type")
            .and_then(Value::as_str)
            .ok_or(Error::ProtocolBlocked)?
            .to_owned();
        let ident = data
            .get("item_id")
            .or_else(|| data.get("output_index"))
            .cloned()
            .unwrap_or(json!(0));
        let field = if kind.starts_with("response.refusal.") {
            "refusal"
        } else {
            "text"
        };
        let part = data
            .get("content_index")
            .unwrap_or(&json!(0))
            .as_u64()
            .ok_or(Error::ProtocolBlocked)?;
        let channel = Channel::new(&ident, field, part)?;
        let mut before = String::new();
        match kind.as_str() {
            "response.output_text.delta" | "response.refusal.delta" => {
                data["delta"] = self
                    .text(
                        channel,
                        data.get("delta")
                            .unwrap_or(&json!(""))
                            .as_str()
                            .ok_or(Error::ProtocolBlocked)?,
                        gate,
                    )?
                    .into()
            }
            "response.output_text.done" | "response.refusal.done" => {
                let tail = self.flush(&channel, gate)?;
                if let Some(text) = data.get(field).and_then(Value::as_str) {
                    data[field] = gate.restore_display(text)?.into();
                }
                if !tail.is_empty() {
                    let delta_kind = if field == "refusal" {
                        "response.refusal.delta"
                    } else {
                        "response.output_text.delta"
                    };
                    let mut synthetic = data.clone();
                    let object = synthetic.as_object_mut().ok_or(Error::ProtocolBlocked)?;
                    object.remove("text");
                    object.remove("refusal");
                    object.insert("type".into(), delta_kind.into());
                    object.insert("item_id".into(), ident.clone());
                    object.insert("content_index".into(), part.into());
                    object.insert("delta".into(), tail.into());
                    before.push_str(&self.event(synthetic, Some(delta_kind), gate)?);
                }
            }
            "response.output_item.added" => {
                let item = &data["item"];
                match item
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or(Error::ProtocolBlocked)?
                {
                    "function_call" => {
                        let id = item.get("id").unwrap_or(&ident);
                        let tool = Channel::new(id, "tool", 0)?;
                        self.new_tool(
                            tool.clone(),
                            item["name"].as_str().ok_or(Error::ProtocolBlocked)?,
                        )?;
                        if let Some(arguments) = item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .filter(|arguments| !arguments.is_empty())
                        {
                            self.args(tool, arguments, "")?;
                            data["item"]["arguments"] = "".into();
                        }
                    }
                    "message" => {
                        if item
                            .get("content")
                            .and_then(Value::as_array)
                            .is_some_and(|content| !content.is_empty())
                        {
                            data["item"] = response::restore_response(
                                Api::Responses,
                                &json!({"output":[item]}),
                                gate,
                            )?["output"][0]
                                .clone();
                        }
                    }
                    _ => return Err(Error::ProtocolBlocked),
                }
            }
            "response.function_call_arguments.delta" => {
                self.args(
                    Channel::new(&ident, "tool", 0)?,
                    data.get("delta")
                        .unwrap_or(&json!(""))
                        .as_str()
                        .ok_or(Error::ProtocolBlocked)?,
                    "",
                )?;
                return Ok(String::new());
            }
            "response.function_call_arguments.done" => {
                let tool = Channel::new(&ident, "tool", 0)?;
                if !self.tools.contains_key(&tool) {
                    self.args(
                        tool.clone(),
                        data.get("arguments")
                            .unwrap_or(&json!(""))
                            .as_str()
                            .ok_or(Error::ProtocolBlocked)?,
                        "",
                    )?;
                }
                data["arguments"] = self.finish_args(&tool, gate)?.1.into();
            }
            "response.created"
            | "response.in_progress"
            | "response.content_part.added"
            | "response.content_part.done"
            | "response.output_item.done"
            | "response.completed"
            | "response.failed"
            | "response.incomplete"
            | "error" => {
                if matches!(
                    kind.as_str(),
                    "response.content_part.added" | "response.content_part.done"
                ) {
                    let part = data.get_mut("part").ok_or(Error::ProtocolBlocked)?;
                    let field = match part["type"].as_str().ok_or(Error::ProtocolBlocked)? {
                        "text" | "output_text" => "text",
                        "refusal" => "refusal",
                        _ => return Err(Error::ProtocolBlocked),
                    };
                    if let Some(text) = part.get(field).and_then(Value::as_str) {
                        part[field] = gate.restore_display(text)?.into();
                    }
                }
                if kind == "response.output_item.done"
                    && data.get("item").is_some_and(Value::is_object)
                {
                    data["item"] = response::restore_response(
                        Api::Responses,
                        &json!({"output":[data["item"]]}),
                        gate,
                    )?["output"][0]
                        .clone();
                }
                if data
                    .get("response")
                    .and_then(|response| response.get("output"))
                    .is_some_and(Value::is_array)
                {
                    data["response"] =
                        response::restore_response(Api::Responses, &data["response"], gate)?;
                }
                if kind == "response.completed" && (!self.tools.is_empty() || !self.text.is_empty())
                {
                    return Err(Error::ProtocolBlocked);
                }
            }
            _ => return Err(Error::ProtocolBlocked),
        }
        Ok(before + &self.event(data, Some(&kind), gate)?)
    }
    fn block(&mut self, block: &str, gate: &mut PrivacyGate) -> Result<String> {
        let mut data_lines = Vec::new();
        let mut metadata = Vec::new();
        for line in block.lines() {
            if let Some(data) = line.strip_prefix("data:") {
                data_lines.push(data.trim_start_matches(' '));
            } else if !line.starts_with("event:") {
                metadata.push(gate.redact_known_secrets(line)?);
            }
        }
        if data_lines.is_empty() {
            return Ok(gate.redact_known_secrets(block)? + "\n\n");
        }
        let payload = Zeroizing::new(data_lines.join("\n"));
        if payload.as_str() == "[DONE]" {
            if !self.tools.is_empty() {
                return Err(Error::ProtocolBlocked);
            }
            for channel in self.text.keys().cloned().collect::<Vec<_>>() {
                if !self.flush(&channel, gate)?.is_empty() {
                    return Err(Error::ProtocolBlocked);
                }
            }
            self.closed = true;
            return Ok("data: [DONE]\n\n".into());
        }
        let data = json::from_slice(payload.as_bytes())?;
        if !data.is_object() {
            return Err(Error::ProtocolBlocked);
        }
        response::validate(&data)?;
        let observed = gate.redact_json(&data)?;
        let output = match self.api {
            Api::Chat => self.chat(data, gate)?,
            Api::Messages => self.messages(data, gate)?,
            Api::Responses => self.responses(data, gate)?,
        };
        gate.remember_response_ids(&observed)?;
        if !metadata.is_empty() && !output.is_empty() {
            Ok(metadata.join("\n") + "\n" + &output)
        } else {
            Ok(output)
        }
    }
    pub fn feed(&mut self, chunk: &[u8], gate: &mut PrivacyGate) -> Result<Vec<u8>> {
        self.validate(gate)?;
        if self.closed && !chunk.is_empty() {
            return Err(Error::ProtocolBlocked);
        }
        self.pending.extend_from_slice(chunk);
        let mut output = String::new();
        loop {
            let delimiter = self
                .pending
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|index| (index, 2))
                .into_iter()
                .chain(
                    self.pending
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .map(|index| (index, 4)),
                )
                .chain(
                    self.pending
                        .windows(3)
                        .position(|window| window == b"\n\r\n")
                        .map(|index| (index, 3)),
                )
                .min_by_key(|(index, _)| *index);
            let Some((end, width)) = delimiter else {
                break;
            };
            if self.closed || end > 1_048_576 {
                return Err(Error::ProtocolBlocked);
            }
            let block = Zeroizing::new(
                std::str::from_utf8(&self.pending[..end])
                    .map_err(|_| Error::InvalidJson)?
                    .to_owned(),
            );
            self.pending = Zeroizing::new(self.pending[end + width..].to_vec());
            output.push_str(&self.block(&block, gate)?);
        }
        if self.pending.len() > 1_048_576
            || (self.closed && self.pending.iter().any(|b| !b.is_ascii_whitespace()))
        {
            return Err(Error::ProtocolBlocked);
        }
        Ok(output.into_bytes())
    }
    pub fn finish(&mut self, gate: &PrivacyGate) -> Result<Vec<u8>> {
        self.validate(gate)?;
        if self.pending.iter().any(|byte| !byte.is_ascii_whitespace()) || !self.tools.is_empty() {
            return Err(Error::ProtocolBlocked);
        }
        for channel in self.text.keys().cloned().collect::<Vec<_>>() {
            if !self.flush(&channel, gate)?.is_empty() {
                return Err(Error::ProtocolBlocked);
            }
        }
        self.closed = true;
        Ok(Vec::new())
    }
}
