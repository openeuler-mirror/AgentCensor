use crate::{
    error::{Error, Result},
    json,
    protocols::{pointer, validate_envelopes, Api},
};
use serde_json::Value;

pub trait ResponseRestorer {
    fn display_group(&self, values: &[String]) -> Result<Vec<String>>;
    fn tool_arguments(&self, name: &str, arguments: &Value) -> Result<Value>;
    fn redact_json(&self, value: &Value) -> Result<Value>;
}

pub(crate) fn validate(value: &Value) -> Result<()> {
    validate_envelopes(value, "", 0)?;
    fn visit(value: &Value) -> Result<()> {
        match value {
            Value::Object(object) => {
                for (key, child) in object {
                    if ["reasoning_content", "logprobs", "top_logprobs"].contains(&key.as_str())
                        && !child.is_null()
                        && child != ""
                        && child != &serde_json::json!([])
                        && child != &serde_json::json!({})
                    {
                        return Err(Error::ProtocolBlocked);
                    }
                    if key == "input" && value.get("type") == Some(&Value::from("tool_use")) {
                        continue;
                    }
                    visit(child)?;
                }
            }
            Value::Array(values) => {
                for value in values {
                    visit(value)?;
                }
            }
            _ => (),
        }
        Ok(())
    }
    visit(value)
}

fn slots(value: &Value, path: &str) -> Result<Vec<String>> {
    if value.is_string() {
        return Ok(vec![path.into()]);
    }
    let mut paths = Vec::new();
    if let Some(parts) = value.as_array() {
        for (index, part) in parts.iter().enumerate() {
            let part_path = pointer(path, &index.to_string())?;
            if part.is_string() {
                paths.push(part_path);
            } else {
                let field = match part.get("type").and_then(Value::as_str) {
                    Some("text" | "output_text") => "text",
                    Some("refusal") => "refusal",
                    _ => return Err(Error::ProtocolBlocked),
                };
                match part.get(field) {
                    Some(Value::String(_)) => paths.push(pointer(&part_path, field)?),
                    None | Some(Value::Null) => (),
                    _ => return Err(Error::ProtocolBlocked),
                }
            }
        }
    }
    Ok(paths)
}
fn display(result: &mut Value, paths: &[String], restorer: &impl ResponseRestorer) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let values = paths
        .iter()
        .map(|path| {
            result
                .pointer(path)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or(Error::ProtocolBlocked)
        })
        .collect::<Result<Vec<_>>>()?;
    let transformed = restorer.display_group(&values)?;
    if transformed.len() != paths.len() {
        return Err(Error::ProtocolBlocked);
    }
    for (path, text) in paths.iter().zip(transformed) {
        *result.pointer_mut(path).ok_or(Error::ProtocolBlocked)? = text.into();
    }
    Ok(())
}
fn field_display(result: &mut Value, path: &str, restorer: &impl ResponseRestorer) -> Result<()> {
    if let Some(value) = result.pointer(path) {
        let paths = slots(value, path)?;
        display(result, &paths, restorer)?;
    }
    Ok(())
}
fn shape_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, value)| b.get(key).is_some_and(|other| shape_equal(value, other)))
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| shape_equal(a, b))
        }
        (Value::String(_), Value::String(_))
        | (Value::Bool(_), Value::Bool(_))
        | (Value::Null, Value::Null) => true,
        (Value::Number(a), Value::Number(b)) => a.is_f64() == b.is_f64(),
        _ => false,
    }
}
fn tool(
    result: &mut Value,
    path: &str,
    field: &str,
    encoded: bool,
    restorer: &impl ResponseRestorer,
) -> Result<()> {
    let owner = result.pointer(path).ok_or(Error::ProtocolBlocked)?;
    let name = owner.get("name").and_then(Value::as_str).unwrap_or("");
    let Some(arguments) = owner.get(field) else {
        return Ok(());
    };
    let decoded = if encoded {
        json::from_slice(arguments.as_str().ok_or(Error::ProtocolBlocked)?.as_bytes())?
    } else {
        arguments.clone()
    };
    let approved = restorer.tool_arguments(name, &decoded)?;
    if !shape_equal(&decoded, &approved) {
        return Err(Error::ProtocolBlocked);
    }
    *result
        .pointer_mut(&pointer(path, field)?)
        .ok_or(Error::ProtocolBlocked)? = if encoded {
        serde_json::to_string(&approved)
            .map_err(|_| Error::ProtocolBlocked)?
            .into()
    } else {
        approved
    };
    Ok(())
}

pub fn restore_response(api: Api, body: &Value, restorer: &impl ResponseRestorer) -> Result<Value> {
    if !body.is_object() {
        return Err(Error::ProtocolBlocked);
    }
    validate(body)?;
    let mut result = restorer.redact_json(body)?;
    match api {
        Api::Chat => {
            let count = result
                .get("choices")
                .and_then(Value::as_array)
                .ok_or(Error::ProtocolBlocked)?
                .len();
            for index in 0..count {
                let path = format!("/choices/{index}/message");
                if !result.pointer(&path).is_some_and(Value::is_object) {
                    return Err(Error::ProtocolBlocked);
                }
                field_display(&mut result, &pointer(&path, "content")?, restorer)?;
                field_display(&mut result, &pointer(&path, "refusal")?, restorer)?;
                let calls = result
                    .pointer(&pointer(&path, "tool_calls")?)
                    .filter(|value| !value.is_null());
                let count = match calls {
                    Some(calls) => calls.as_array().ok_or(Error::ProtocolBlocked)?.len(),
                    None => 0,
                };
                for tool_index in 0..count {
                    let function = format!("{path}/tool_calls/{tool_index}/function");
                    if !result.pointer(&function).is_some_and(Value::is_object) {
                        return Err(Error::ProtocolBlocked);
                    }
                    tool(&mut result, &function, "arguments", true, restorer)?;
                }
                let function = pointer(&path, "function_call")?;
                if result.pointer(&function).is_some_and(Value::is_object) {
                    tool(&mut result, &function, "arguments", true, restorer)?;
                }
            }
        }
        Api::Messages | Api::Responses => {
            let root = if api == Api::Messages {
                "/content"
            } else {
                "/output"
            };
            let count = result
                .pointer(root)
                .and_then(Value::as_array)
                .ok_or(Error::ProtocolBlocked)?
                .len();
            let mut root_slots = Vec::new();
            for index in 0..count {
                let path = format!("{root}/{index}");
                let block = result.pointer(&path).ok_or(Error::ProtocolBlocked)?;
                match block
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or(Error::ProtocolBlocked)?
                {
                    "text" | "output_text" | "refusal" => {
                        let field = if block["type"] == "refusal" {
                            "refusal"
                        } else {
                            "text"
                        };
                        match block.get(field) {
                            Some(Value::String(_)) => root_slots.push(pointer(&path, field)?),
                            None | Some(Value::Null) => (),
                            _ => return Err(Error::ProtocolBlocked),
                        }
                    }
                    "message" => {
                        if !block.get("content").is_some_and(Value::is_array) {
                            return Err(Error::ProtocolBlocked);
                        }
                        field_display(&mut result, &pointer(&path, "content")?, restorer)?;
                    }
                    "tool_use" => tool(&mut result, &path, "input", false, restorer)?,
                    "function_call" => tool(&mut result, &path, "arguments", true, restorer)?,
                    _ => return Err(Error::ProtocolBlocked),
                }
            }
            display(&mut result, &root_slots, restorer)?;
        }
    }
    restorer.redact_json(&result)
}
