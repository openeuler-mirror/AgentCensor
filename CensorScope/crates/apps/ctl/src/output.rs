//! User-facing output projection for control responses.

use std::time::UNIX_EPOCH;

use control_contract::reply::ControlReply;
use serde_json::json;

pub fn format_error_json(code: &str, message: &str) -> String {
    serde_json::to_string(&json!({"ok": false, "code": code, "message": message}))
        .expect("control error serialization")
}

pub fn format_init_json(status: &str, path: &std::path::Path) -> String {
    serde_json::to_string(&json!({"ok": true, "status": status, "path": path}))
        .expect("init response serialization")
}

pub fn format_reply(reply: &ControlReply) -> String {
    match reply {
        ControlReply::TrackAdded(reply) => {
            format!(
                "trace {} entered {} (operation {} {})",
                reply
                    .trace_id
                    .map(|trace_id| trace_id.to_string())
                    .unwrap_or_else(|| "pending".to_string()),
                reply.lifecycle_state,
                reply.operation_id,
                reply.operation_state.as_str()
            )
        }
        ControlReply::TrackRemoved => "root capture removed".to_string(),
        ControlReply::OperationStatus(reply) => format!(
            "operation {} {} trace={}{}",
            reply.operation_id,
            reply.operation_state.as_str(),
            reply
                .trace_id
                .map(|trace_id| trace_id.to_string())
                .unwrap_or_else(|| "pending".to_string()),
            reply
                .error
                .as_deref()
                .map(|error| format!(" error={error}"))
                .unwrap_or_default()
        ),
        ControlReply::CallStarted => "call span started".to_string(),
        ControlReply::CallEnded => "call span ended".to_string(),
        ControlReply::TraceList(items) => items
            .iter()
            .map(|item| {
                format!(
                    "{} {} pid={} pidns={} container={} {}/{:?}",
                    item.trace_id,
                    item.display_name,
                    item.root_pid,
                    item.root_pid_namespace
                        .as_ref()
                        .map(|namespace| namespace.as_str())
                        .unwrap_or("none"),
                    item.root_container_id.as_deref().unwrap_or("none"),
                    item.lifecycle_state,
                    item.health
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        ControlReply::Doctor(reply) => format!(
            "collectors={} storage_ready={}",
            reply.available_collectors.join(","),
            reply.storage_ready
        ),
    }
}

/// Machine-readable JSON projection consumed by automation (for example the
/// dsh-rich-observability plugin at `track-add` time).
pub fn format_reply_json(reply: &ControlReply) -> String {
    let value = match reply {
        ControlReply::TrackAdded(reply) => json!({
            "ok": true,
            "trace_id": reply.trace_id.map(|trace_id| trace_id.get()),
            "lifecycle_state": reply.lifecycle_state.as_display_str(),
            "operation_id": reply.operation_id.get().to_string(),
            "operation_state": reply.operation_state.as_str(),
        }),
        ControlReply::TrackRemoved => json!({ "ok": true }),
        ControlReply::OperationStatus(reply) => json!({
            "ok": true,
            "operation_id": reply.operation_id.get().to_string(),
            "trace_id": reply.trace_id.map(|trace_id| trace_id.get()),
            "lifecycle_state": reply.lifecycle_state.map(|state| state.as_display_str()),
            "operation_state": reply.operation_state.as_str(),
            "error": reply.error,
        }),
        ControlReply::CallStarted => json!({"ok": true, "status": "started"}),
        ControlReply::CallEnded => json!({"ok": true, "status": "ended"}),
        ControlReply::TraceList(items) => json!({
            "ok": true,
            "traces": items
                .iter()
                .map(|item| json!({
                    "trace_id": item.trace_id.get(),
                    "display_name": item.display_name.to_string(),
                    "root_pid": item.root_pid,
                    "root_pid_namespace": item.root_pid_namespace.as_ref().map(|value| value.as_str()),
                    "root_container_id": item.root_container_id,
                    "lifecycle_state": item.lifecycle_state.as_display_str(),
                    "health": match item.health {
                        model_core::trace::TraceHealth::Clean => "clean",
                        model_core::trace::TraceHealth::Degraded => "degraded",
                    },
                    "tags": item.tags.iter().collect::<Vec<_>>(),
                    "created_at": system_time_nanos(item.created_at),
                }))
                .collect::<Vec<_>>(),
        }),
        ControlReply::Doctor(reply) => json!({
            "ok": true,
            "available_collectors": reply.available_collectors,
            "storage_ready": reply.storage_ready,
        }),
    };
    serde_json::to_string(&value).expect("control reply serialization")
}

fn system_time_nanos(value: std::time::SystemTime) -> u64 {
    value
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::time::SystemTime;

    use control_contract::reply::OperationState;
    use control_contract::reply::{ControlReply, TraceListItem, TrackAddReply};
    use model_core::ids::{RequestId, TraceId, TraceName};
    use model_core::process::NamespaceIdentity;
    use model_core::trace::{TraceHealth, TraceLifecycleState};

    use super::{format_error_json, format_reply, format_reply_json};

    #[test]
    fn trace_list_prints_namespace_and_resolved_container_identity() {
        let output = format_reply(&ControlReply::TraceList(vec![TraceListItem {
            trace_id: TraceId::new(7),
            display_name: TraceName::new("kata"),
            root_pid: 42,
            root_pid_namespace: Some(NamespaceIdentity::new("pid:[4026532248]")),
            root_container_id: Some("a".repeat(64)),
            lifecycle_state: TraceLifecycleState::Active,
            health: TraceHealth::Clean,
            tags: BTreeSet::new(),
            created_at: SystemTime::UNIX_EPOCH,
        }]));

        assert!(output.contains("pidns=pid:[4026532248]"));
        assert!(output.contains(&format!("container={}", "a".repeat(64))));
    }

    #[test]
    fn trace_list_marks_unresolved_container_identity() {
        let output = format_reply(&ControlReply::TraceList(vec![TraceListItem {
            trace_id: TraceId::new(8),
            display_name: TraceName::new("host"),
            root_pid: 43,
            root_pid_namespace: None,
            root_container_id: None,
            lifecycle_state: TraceLifecycleState::Active,
            health: TraceHealth::Clean,
            tags: BTreeSet::new(),
            created_at: SystemTime::UNIX_EPOCH,
        }]));

        assert!(output.contains("pidns=none"));
        assert!(output.contains("container=none"));
    }

    #[test]
    fn error_json_is_machine_readable() {
        let value: serde_json::Value =
            serde_json::from_str(&format_error_json("transport", "down")).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["code"], "transport");
        assert_eq!(value["message"], "down");
    }

    #[test]
    fn track_add_json_is_parseable_and_carries_trace_id() {
        let raw = format_reply_json(&ControlReply::TrackAdded(TrackAddReply {
            trace_id: Some(TraceId::new(9)),
            lifecycle_state: TraceLifecycleState::Active,
            operation_id: RequestId::new(10),
            operation_state: OperationState::Active,
            error: None,
        }));
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["trace_id"], 9);
        assert_eq!(value["lifecycle_state"], "Active");
        assert_eq!(value["operation_id"], "10");
        assert_eq!(value["operation_state"], "active");
    }

    #[test]
    fn operation_id_json_preserves_u64_precision() {
        let operation_id = u64::MAX - 7;
        let raw = format_reply_json(&ControlReply::TrackAdded(TrackAddReply {
            trace_id: None,
            lifecycle_state: TraceLifecycleState::Starting,
            operation_id: RequestId::new(operation_id),
            operation_state: OperationState::Preparing,
            error: None,
        }));
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["operation_id"], operation_id.to_string());
    }
}
