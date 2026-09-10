//! Shared Unix-socket framing for the CensorScope control plane.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use control_contract::command::{
    CallEndCommand, CallStartCommand, ControlCommand, DoctorCommand, ListTracesCommand, ProcessRef,
    TrackAddCommand, TrackRemoveCommand,
};
use control_contract::reply::{
    ControlError, ControlReply, DoctorReply, TraceListItem, TrackAddReply,
};
use control_contract::selector::TraceSelector;
use model_core::ids::{ProfileName, RequestId, TraceId, TraceName};
use model_core::process::NamespaceIdentity;
use model_core::trace::{TraceHealth, TraceLifecycleState};

/// Error raised while encoding or decoding a control-plane frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlCodecError {
    pub stage: String,
    pub message: String,
}

impl ControlCodecError {
    fn new(stage: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            stage: stage.into(),
            message: message.into(),
        }
    }
}

/// Encode one control command into the newline-delimited UDS frame format.
pub fn encode_command(command: &ControlCommand) -> Vec<u8> {
    let mut fields = Vec::new();
    match command {
        ControlCommand::TrackAdd(c) => {
            fields.extend(["track-add".into(), c.request_id.get().to_string()]);
            fields.push(c.root.namespace_pid.to_string());
            fields.push(c.root.pid_namespace.as_str().to_string());
            fields.push(c.display_name.to_string());
            fields.push(c.profile_name.to_string());
            fields.push(c.tags.len().to_string());
            fields.extend(c.tags.iter().cloned());
            match c.trace_id {
                Some(trace_id) => fields.push(trace_id.get().to_string()),
                None => fields.push("0".into()),
            }
        }
        ControlCommand::TrackRemove(c) => {
            fields.extend(["track-remove".into(), c.request_id.get().to_string()]);
            encode_selector(&mut fields, &c.selector);
        }
        ControlCommand::ListTraces(c) => {
            fields.extend(["trace-list".into(), c.request_id.get().to_string()]);
            match &c.selector {
                Some(selector) => {
                    fields.push("1".into());
                    encode_selector(&mut fields, selector);
                }
                None => fields.push("0".into()),
            }
        }
        ControlCommand::Doctor(c) => {
            fields.extend(["doctor".into(), c.request_id.get().to_string()])
        }
        ControlCommand::CallStart(c) => fields.extend([
            "call-start".into(),
            c.request_id.get().to_string(),
            c.trace_id.get().to_string(),
            c.session_id.clone().unwrap_or_default(),
            c.call_id.clone(),
            c.host_pid.to_string(),
            c.started_at.to_string(),
        ]),
        ControlCommand::CallEnd(c) => fields.extend([
            "call-end".into(),
            c.request_id.get().to_string(),
            c.trace_id.get().to_string(),
            c.session_id.clone().unwrap_or_default(),
            c.call_id.clone(),
            c.host_pid.to_string(),
            c.ended_at.to_string(),
            c.status.clone(),
        ]),
    }
    encode_fields(&fields)
}

/// Decode and validate a client command frame.
pub fn decode_command(bytes: &[u8]) -> Result<ControlCommand, ControlCodecError> {
    let fields = decode_fields(bytes)?;
    let opcode = field(&fields, 0)?.as_str();
    let request_id = RequestId::new(parse_u64(field(&fields, 1)?, "request_id")?);
    match opcode {
        "track-add" => {
            let root = ProcessRef::new(
                parse_u32(field(&fields, 2)?, "root_pid")?,
                NamespaceIdentity::new(field(&fields, 3)?),
            );
            let display_name = TraceName::new(field(&fields, 4)?);
            let profile_name = ProfileName::new(field(&fields, 5)?);
            let count = parse_usize(field(&fields, 6)?, "tag_count")?;
            let mut tags = BTreeSet::new();
            for index in 0..count {
                tags.insert(field(&fields, 7 + index)?.clone());
            }
            // Sentinel field: "0" marks "no trace to continue"; otherwise the
            // raw numeric trace id.
            let trace_id = fields
                .get(7 + count)
                .and_then(|value| {
                    if value == "0" {
                        None
                    } else {
                        value.parse::<u64>().ok().map(TraceId::new)
                    }
                });
            Ok(ControlCommand::TrackAdd(TrackAddCommand {
                request_id,
                root,
                display_name,
                profile_name,
                tags,
                trace_id,
            }))
        }
        "track-remove" => Ok(ControlCommand::TrackRemove(TrackRemoveCommand {
            request_id,
            selector: decode_selector(&fields, 2)?,
        })),
        "trace-list" => {
            let has_selector = field(&fields, 2)? == "1";
            let selector = has_selector
                .then(|| decode_selector(&fields, 3))
                .transpose()?;
            Ok(ControlCommand::ListTraces(ListTracesCommand {
                request_id,
                selector,
            }))
        }
        "doctor" => Ok(ControlCommand::Doctor(DoctorCommand { request_id })),
        "call-start" => Ok(ControlCommand::CallStart(CallStartCommand {
            request_id,
            trace_id: TraceId::new(parse_u64(field(&fields, 2)?, "trace_id")?),
            session_id: nonempty(field(&fields, 3)?),
            call_id: field(&fields, 4)?.clone(),
            host_pid: parse_u32(field(&fields, 5)?, "host_pid")?,
            started_at: parse_u64(field(&fields, 6)?, "started_at")?,
        })),
        "call-end" => Ok(ControlCommand::CallEnd(CallEndCommand {
            request_id,
            trace_id: TraceId::new(parse_u64(field(&fields, 2)?, "trace_id")?),
            session_id: nonempty(field(&fields, 3)?),
            call_id: field(&fields, 4)?.clone(),
            host_pid: parse_u32(field(&fields, 5)?, "host_pid")?,
            ended_at: parse_u64(field(&fields, 6)?, "ended_at")?,
            status: field(&fields, 7)?.clone(),
        })),
        _ => Err(ControlCodecError::new(
            "decode",
            format!("unknown opcode {opcode}"),
        )),
    }
}

/// Encode a successful reply or structured control error.
pub fn encode_reply(reply: &Result<ControlReply, ControlError>) -> Vec<u8> {
    let mut fields = Vec::new();
    match reply {
        Err(error) => fields.extend(["error".into(), error.code.clone(), error.message.clone()]),
        Ok(ControlReply::TrackAdded(r)) => fields.extend([
            "track-added".into(),
            r.trace_id.get().to_string(),
            r.lifecycle_state.as_display_str().into(),
        ]),
        Ok(ControlReply::TrackRemoved) => fields.push("track-removed".into()),
        Ok(ControlReply::TraceList(items)) => {
            fields.extend(["trace-list".into(), items.len().to_string()]);
            for item in items {
                fields.extend([
                    item.trace_id.get().to_string(),
                    item.display_name.to_string(),
                    item.root_pid.to_string(),
                    item.root_pid_namespace
                        .as_ref()
                        .map(|v| v.as_str())
                        .unwrap_or_default()
                        .into(),
                    item.root_container_id.clone().unwrap_or_default(),
                    item.lifecycle_state.as_display_str().into(),
                    format!("{:?}", item.health),
                    system_time_to_secs(item.created_at).to_string(),
                    item.tags.len().to_string(),
                ]);
                fields.extend(item.tags.iter().cloned());
            }
        }
        Ok(ControlReply::Doctor(r)) => {
            fields.extend(["doctor".into(), r.available_collectors.len().to_string()])
        }
        Ok(ControlReply::CallStarted) => fields.push("call-started".into()),
        Ok(ControlReply::CallEnded) => fields.push("call-ended".into()),
    }
    if let Ok(ControlReply::Doctor(r)) = reply {
        fields.extend(r.available_collectors.iter().cloned());
        fields.push(r.storage_ready.to_string());
    }
    encode_fields(&fields)
}

/// Decode a daemon response frame.
pub fn decode_reply(bytes: &[u8]) -> Result<Result<ControlReply, ControlError>, ControlCodecError> {
    let fields = decode_fields(bytes)?;
    match field(&fields, 0)?.as_str() {
        "error" => Ok(Err(ControlError::new(
            field(&fields, 1)?,
            field(&fields, 2)?,
        ))),
        "track-added" => Ok(Ok(ControlReply::TrackAdded(TrackAddReply {
            trace_id: TraceId::new(parse_u64(field(&fields, 1)?, "trace_id")?),
            lifecycle_state: parse_lifecycle(field(&fields, 2)?)?,
        }))),
        "track-removed" => Ok(Ok(ControlReply::TrackRemoved)),
        "call-started" => Ok(Ok(ControlReply::CallStarted)),
        "call-ended" => Ok(Ok(ControlReply::CallEnded)),
        "trace-list" => {
            let count = parse_usize(field(&fields, 1)?, "trace_count")?;
            let mut cursor = 2;
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                let trace_id = TraceId::new(parse_u64(field(&fields, cursor)?, "trace_id")?);
                let display_name = TraceName::new(field(&fields, cursor + 1)?);
                let root_pid = parse_u32(field(&fields, cursor + 2)?, "root_pid")?;
                let root_pid_namespace = match field(&fields, cursor + 3)?.as_str() {
                    "" => None,
                    v => Some(NamespaceIdentity::new(v)),
                };
                let root_container_id = match field(&fields, cursor + 4)?.as_str() {
                    "" => None,
                    v => Some(v.to_string()),
                };
                let lifecycle_state = parse_lifecycle(field(&fields, cursor + 5)?)?;
                let health = parse_health(field(&fields, cursor + 6)?)?;
                let created_at = UNIX_EPOCH
                    + Duration::from_secs(parse_u64(field(&fields, cursor + 7)?, "created_at")?);
                let tag_count = parse_usize(field(&fields, cursor + 8)?, "tag_count")?;
                let mut tags = BTreeSet::new();
                for index in 0..tag_count {
                    tags.insert(field(&fields, cursor + 9 + index)?.clone());
                }
                items.push(TraceListItem {
                    trace_id,
                    display_name,
                    root_pid,
                    root_pid_namespace,
                    root_container_id,
                    lifecycle_state,
                    health,
                    tags,
                    created_at,
                });
                cursor += 9 + tag_count;
            }
            Ok(Ok(ControlReply::TraceList(items)))
        }
        "doctor" => {
            let count = parse_usize(field(&fields, 1)?, "collector_count")?;
            let mut collectors = Vec::with_capacity(count);
            for index in 0..count {
                collectors.push(field(&fields, 2 + index)?.clone());
            }
            let storage_ready = field(&fields, 2 + count)? == "true";
            Ok(Ok(ControlReply::Doctor(DoctorReply {
                available_collectors: collectors,
                storage_ready,
            })))
        }
        other => Err(ControlCodecError::new(
            "decode",
            format!("unknown reply {other}"),
        )),
    }
}

fn encode_selector(fields: &mut Vec<String>, selector: &TraceSelector) {
    match selector {
        TraceSelector::TraceId(id) => fields.extend(["trace-id".into(), id.get().to_string()]),
        TraceSelector::RootPid(pid) => fields.extend(["root-pid".into(), pid.to_string()]),
        TraceSelector::Tag(tag) => fields.extend(["tag".into(), tag.clone()]),
        TraceSelector::Name(name) => fields.extend(["name".into(), name.to_string()]),
    }
}

fn decode_selector(fields: &[String], offset: usize) -> Result<TraceSelector, ControlCodecError> {
    match field(fields, offset)?.as_str() {
        "trace-id" => Ok(TraceSelector::TraceId(TraceId::new(parse_u64(
            field(fields, offset + 1)?,
            "trace_id",
        )?))),
        "root-pid" => Ok(TraceSelector::RootPid(parse_u32(
            field(fields, offset + 1)?,
            "root_pid",
        )?)),
        "tag" => Ok(TraceSelector::Tag(field(fields, offset + 1)?.clone())),
        "name" => Ok(TraceSelector::Name(TraceName::new(field(
            fields,
            offset + 1,
        )?))),
        _ => Err(ControlCodecError::new("decode", "unknown selector kind")),
    }
}

fn encode_fields(fields: &[String]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in fields {
        bytes.extend_from_slice(value.len().to_string().as_bytes());
        bytes.push(b'#');
        bytes.extend_from_slice(value.as_bytes());
    }
    bytes
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

fn decode_fields(bytes: &[u8]) -> Result<Vec<String>, ControlCodecError> {
    let mut cursor = 0;
    let mut fields = Vec::new();
    while cursor < bytes.len() {
        let start = cursor;
        while cursor < bytes.len() && bytes[cursor] != b'#' {
            cursor += 1;
        }
        if cursor == bytes.len() {
            return Err(ControlCodecError::new(
                "decode",
                "unterminated field length",
            ));
        }
        let length = std::str::from_utf8(&bytes[start..cursor])
            .map_err(|_| ControlCodecError::new("decode", "invalid field length"))?
            .parse::<usize>()
            .map_err(|_| ControlCodecError::new("decode", "invalid field length"))?;
        cursor += 1;
        if cursor + length > bytes.len() {
            return Err(ControlCodecError::new("decode", "field exceeds frame"));
        }
        fields.push(
            String::from_utf8(bytes[cursor..cursor + length].to_vec())
                .map_err(|_| ControlCodecError::new("decode", "field is not UTF-8"))?,
        );
        cursor += length;
    }
    Ok(fields)
}

fn field(fields: &[String], index: usize) -> Result<&String, ControlCodecError> {
    fields
        .get(index)
        .ok_or_else(|| ControlCodecError::new("decode", format!("missing field {index}")))
}
fn parse_u64(value: &str, name: &str) -> Result<u64, ControlCodecError> {
    value
        .parse()
        .map_err(|e| ControlCodecError::new("decode", format!("invalid {name}: {e}")))
}
fn parse_u32(value: &str, name: &str) -> Result<u32, ControlCodecError> {
    value
        .parse()
        .map_err(|e| ControlCodecError::new("decode", format!("invalid {name}: {e}")))
}
fn parse_usize(value: &str, name: &str) -> Result<usize, ControlCodecError> {
    value
        .parse()
        .map_err(|e| ControlCodecError::new("decode", format!("invalid {name}: {e}")))
}
fn parse_lifecycle(value: &str) -> Result<TraceLifecycleState, ControlCodecError> {
    TraceLifecycleState::from_display_str(value)
        .ok_or_else(|| ControlCodecError::new("decode", "invalid lifecycle state"))
}
fn parse_health(value: &str) -> Result<TraceHealth, ControlCodecError> {
    match value {
        "Clean" => Ok(TraceHealth::Clean),
        "Degraded" => Ok(TraceHealth::Degraded),
        _ => Err(ControlCodecError::new("decode", "invalid trace health")),
    }
}
fn system_time_to_secs(value: SystemTime) -> u64 {
    value
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
