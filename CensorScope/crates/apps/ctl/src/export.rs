use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Map, Value, json};

fn ns(value: i64) -> Value {
    Value::String(value.to_string())
}

fn optional_ns(value: Option<i64>) -> Value {
    value.map_or(Value::Null, ns)
}

fn call_action_predicate(alias: &str) -> String {
    format!(
        "(?3 IS NULL OR EXISTS ( \
            SELECT 1 FROM semantic_action_evidence_read sae \
            WHERE sae.trace_id = {alias}.trace_id AND sae.action_id = {alias}.action_id \
              AND ((sae.evidence_kind = 1 AND EXISTS ( \
                    SELECT 1 FROM events ev \
                    WHERE ev.trace_id = sae.trace_id AND ev.event_id = sae.evidence_id \
                      AND ev.session_id = ?2 AND ev.call_id = ?3)) \
                OR (sae.evidence_kind = 3 AND EXISTS ( \
                    SELECT 1 FROM payload_segments ps \
                    WHERE ps.trace_id = sae.trace_id AND ps.sequence = sae.evidence_id \
                      AND ps.session_id = ?2 AND ps.call_id = ?3)))))",
        alias = alias
    )
}

/// Fill `path` on fd-relative I/O (read/write/readv/writev/…) from the owning
/// process's last bind of that fd: eBPF only reports a path at open time, so
/// without this pass those rows would show `fd N` and no file name.
fn enrich_fd_paths(events: &mut [Value]) {
    use std::collections::HashMap;
    #[derive(Clone)]
    struct Bound {
        path: String,
    }
    let mut by_fd: HashMap<(u64, u64, i64), Bound> = HashMap::new();
    for event in events.iter_mut() {
        let Some(trace) = event.get("trace_id").and_then(Value::as_u64) else {
            continue;
        };
        let Some(process) = event.get("process_id").and_then(Value::as_u64) else {
            continue;
        };
        let fd = event.get("fd").and_then(Value::as_i64);
        let operation = event.get("operation").and_then(Value::as_str).unwrap_or("");
        let path = event.get("path").and_then(Value::as_str);
        let is_file = event.get("kind_name").and_then(Value::as_str) == Some("file");
        match operation {
            "open" | "openat" | "openat2" | "creat" => {
                if let (Some(fd), Some(path)) = (fd, path) {
                    by_fd.insert(
                        (trace, process, fd),
                        Bound {
                            path: path.to_string(),
                        },
                    );
                }
            }
            "close" => {
                if let Some(fd) = fd {
                    by_fd.remove(&(trace, process, fd));
                }
            }
            "dup" | "dup2" | "dup3" => {
                if let (Some(fd), Some(target)) = (fd, target_fd_of(event)) {
                    if let Some(bound) = by_fd.get(&(trace, process, fd)) {
                        by_fd.insert(
                            (trace, process, target),
                            Bound {
                                path: bound.path.clone(),
                            },
                        );
                    }
                }
            }
            "read" | "write" | "readv" | "writev" | "pread" | "pread64" | "pwrite" | "pwrite64"
            | "mmap" | "ftruncate" | "fsync" | "fdatasync" | "lseek" | "fallocate"
                if is_file =>
            {
                // Absent means SQL NULL; an explicit JSON null is the same.
                let has_path = event
                    .get("path")
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.is_empty());
                if !has_path {
                    if let Some(fd) = fd {
                        if let Some(bound) = by_fd.get(&(trace, process, fd)) {
                            event["path"] = json!(bound.path.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Read `metadata.target_fd` (number or numeric string) off a dup-family event.
fn target_fd_of(event: &Value) -> Option<i64> {
    let metadata = event.get("metadata")?;
    metadata
        .get("target_fd")
        .and_then(Value::as_i64)
        .or_else(|| {
            metadata
                .get("target_fd")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<i64>().ok())
        })
}

/// Export a snapshot using an already validated operator configuration.
/// The caller supplies the configured database path and destination; export
/// is intentionally unbounded (no size cap).
pub fn export_to_path(
    database: &Path,
    trace_id: u64,
    session_id: &str,
    call_id: Option<&str>,
    full: bool,
    no_internal: bool,
    page_size: Option<usize>,
    after_event: Option<u64>,
    output: &Path,
) -> Result<(), String> {
    let connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("cannot open read-only database: {error}"))?;
    let result = export_with_paging(
        &connection,
        Some(trace_id),
        Some(session_id),
        call_id,
        full,
        no_internal,
        usize::MAX,
        page_size,
        after_event,
    )
    .map_err(|error| format!("export failed: {error}"))?;
    write_atomic_json(output, &result).map_err(|error| format!("export write failed: {error}"))
}

/// Rows the dsh plugin integration treats as self-noise and hides from call
/// views: `tls.coverage` diagnostics and the censorscope-host plugin's
/// housekeeping (`censorscopectl` runs as a subprocess under the tracked root).
fn is_internal_event(event: &Value) -> bool {
    if event.get("protocol").and_then(Value::as_str) == Some("tls.coverage") {
        return true;
    }
    if let Some(executable) = event.get("executable").and_then(Value::as_str) {
        let name = executable.rsplit('/').next().unwrap_or(executable);
        if name == "censorscopectl" {
            return true;
        }
    }
    false
}

fn write_atomic_json(path: &Path, value: &Value) -> Result<(), std::io::Error> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("snapshot.json");
    let temporary = path.with_file_name(format!(".{file_name}.tmp-{}-{nonce}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)
}

#[cfg(test)]
fn export(
    connection: &Connection,
    trace_id: Option<u64>,
    full: bool,
    max_bytes: usize,
) -> Result<Value, rusqlite::Error> {
    export_with_paging(
        connection, trace_id, None, None, full, false, max_bytes, None, None,
    )
}

fn export_with_paging(
    connection: &Connection,
    trace_id: Option<u64>,
    session_id: Option<&str>,
    call_id: Option<&str>,
    full: bool,
    no_internal: bool,
    max_bytes: usize,
    page_size: Option<usize>,
    after_event: Option<u64>,
) -> Result<Value, rusqlite::Error> {
    connection.execute_batch("BEGIN DEFERRED")?;
    let result = export_with_paging_inner(
        connection,
        trace_id,
        session_id,
        call_id,
        full,
        no_internal,
        max_bytes,
        page_size,
        after_event,
    );
    match result {
        Ok(value) => {
            connection.execute_batch("COMMIT")?;
            Ok(value)
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn export_with_paging_inner(
    connection: &Connection,
    trace_id: Option<u64>,
    session_id: Option<&str>,
    call_id: Option<&str>,
    full: bool,
    no_internal: bool,
    max_bytes: usize,
    page_size: Option<usize>,
    after_event: Option<u64>,
) -> Result<Value, rusqlite::Error> {
    let mut traces = query_json(
        connection,
        "SELECT trace_id, display_name, profile_name, lifecycle_state, health, created_at
         FROM traces
         WHERE (?1 IS NULL OR trace_id = ?1)
           AND (?2 IS NULL OR trace_id IN (
                SELECT trace_id FROM events_read WHERE session_id = ?2
                UNION SELECT trace_id FROM semantic_actions_read WHERE session_id = ?2
                UNION SELECT trace_id FROM payload_segments WHERE session_id = ?2
           ))
         ORDER BY trace_id",
        trace_id,
        session_id,
        |row| {
            Ok(json!({
                "trace_id": row.get::<_, u64>(0)?,
                "display_name": row.get::<_, String>(1)?,
                "profile_name": row.get::<_, String>(2)?,
                "lifecycle_state": row.get::<_, String>(3)?,
                "health": row.get::<_, String>(4)?,
                "created_at": ns(row.get::<_, i64>(5)?),
            }))
        },
    )?;
    let processes = query_json(
            connection,
            "SELECT process_id, host_pid, host_task_id, host_start_ticks, host_start_boottime_ns, resolution_state, session_id
             FROM processes
             WHERE (?1 IS NULL OR process_id IN (SELECT process_id FROM memberships WHERE trace_id = ?1)
                    OR NOT EXISTS (SELECT 1 FROM memberships WHERE trace_id = ?1))
               AND (?2 IS NULL OR session_id = ?2 OR process_id IN (
                    SELECT process_id FROM events_read WHERE session_id = ?2
                    UNION SELECT process_id FROM payload_segments WHERE session_id = ?2
                    UNION SELECT process_id FROM semantic_actions_read WHERE session_id = ?2
                    UNION SELECT process_id FROM processes WHERE session_id = ?2
               ))
             ORDER BY process_id",
            trace_id,
            session_id,
            |row| {
                Ok(json!({
                    "process_id": row.get::<_, u64>(0)?, "host_pid": row.get::<_, Option<u32>>(1)?,
                    "host_task_id": row.get::<_, Option<u32>>(2)?, "host_start_ticks": row.get::<_, Option<u64>>(3)?,
                    "host_start_boottime_ns": row.get::<_, Option<u64>>(4)?.map(|value| value.to_string()), "resolution_state": row.get::<_, String>(5)?,
                    "session_id": row.get::<_, Option<String>>(6)?
                }))
            },
        )?;
    let memberships = query_json(
            connection,
            "SELECT trace_id, process_id, inherited_from_process_id, observed_at, capture_enabled, propagation_enabled, membership_state, exit_code, exit_observed_at, exit_observation_source
             FROM memberships
             WHERE (?1 IS NULL OR trace_id = ?1)
               AND (?2 IS NULL OR process_id IN (
                    SELECT process_id FROM events_read WHERE session_id = ?2
                    UNION SELECT process_id FROM payload_segments WHERE session_id = ?2
                    UNION SELECT process_id FROM semantic_actions_read WHERE session_id = ?2
                    UNION SELECT process_id FROM processes WHERE session_id = ?2
               ))
            ORDER BY trace_id, process_id",
            trace_id,
            session_id,
            |row| {
                Ok(json!({
                    "trace_id": row.get::<_, u64>(0)?, "process_id": row.get::<_, u64>(1)?,
                    "inherited_from_process_id": row.get::<_, Option<u64>>(2)?, "observed_at": optional_ns(row.get::<_, Option<i64>>(3)?),
                    "capture_enabled": row.get::<_, i64>(4)? != 0, "propagation_enabled": row.get::<_, i64>(5)? != 0,
                    "membership_state": row.get::<_, String>(6)?, "exit_code": row.get::<_, Option<i32>>(7)?,
                    "exit_observed_at": optional_ns(row.get::<_, Option<i64>>(8)?), "exit_observation_source": row.get::<_, Option<String>>(9)?
                }))
            },
        )?;
    let event_after = after_event
        .map(|cursor| format!(" AND event_id > {cursor}"))
        .unwrap_or_default();
    let event_limit = page_size
        .map(|limit| format!(" LIMIT {limit}"))
        .unwrap_or_else(|| " LIMIT -1".to_string());
    let event_sql = format!(
        "SELECT event_id, trace_id, observed_at, process_id, kind, flags, operation,
                path, fd, size_bytes, endpoint, direction, result, stream, protocol,
                loss_reason, dropped, dropped_bytes, session_id, call_id, metadata,
                collector, parent_process_id, executable, argv, argv_flags, channel, resource
         FROM events_read WHERE (?1 IS NULL OR trace_id = ?1)
           AND (?2 IS NULL OR session_id = ?2)
           AND (?3 IS NULL OR call_id = ?3){event_after}
         ORDER BY trace_id, observed_at, event_id{event_limit}"
    );
    let mut events = query_json_with_call(
        connection,
        &event_sql,
        trace_id,
        session_id,
        call_id,
        |row| {
            let kind: i64 = row.get(4)?;
            let operation: String = row.get(6)?;
            let metadata = row.get::<_, String>(20)?;
            let category = observation_category(kind, &operation);
            Ok(json!({
                "event_id": row.get::<_, u64>(0)?, "trace_id": row.get::<_, u64>(1)?,
                "observed_at": ns(row.get::<_, i64>(2)?), "process_id": row.get::<_, u64>(3)?,
                "kind": kind, "kind_name": event_kind_name(kind),
                "observation_category": category,
                "syscall_name": (matches!(kind, 1..=5) && !operation.is_empty()).then_some(operation.clone()),
                "flags": row.get::<_, u32>(5)?,
                "operation": operation, "path": row.get::<_, Option<String>>(7)?,
                "fd": row.get::<_, Option<i32>>(8)?, "size": row.get::<_, Option<u64>>(9)?,
                "endpoint": row.get::<_, Option<String>>(10)?, "direction": row.get::<_, i64>(11)?,
                "result": row.get::<_, Option<i64>>(12)?, "stream": row.get::<_, Option<String>>(13)?,
                "protocol": row.get::<_, Option<String>>(14)?, "loss_reason": row.get::<_, Option<String>>(15)?,
                "dropped": row.get::<_, Option<u64>>(16)?, "dropped_bytes": row.get::<_, Option<u64>>(17)?,
                "session_id": row.get::<_, Option<String>>(18)?,
                "call_id": row.get::<_, Option<String>>(19)?,
                "metadata": serde_json::from_str::<Value>(&metadata).unwrap_or_else(|_| json!({})),
                "collector": row.get::<_, String>(21)?,
                "parent_process_id": row.get::<_, Option<u64>>(22)?,
                "executable": row.get::<_, Option<String>>(23)?,
                "argv": row.get::<_, Option<String>>(24)?,
                "argv_flags": row.get::<_, u32>(25)?,
                "channel": row.get::<_, Option<String>>(26)?,
                "resource": row.get::<_, Option<String>>(27)?,
            }))
        },
    )?;
    // Map process ids once so host-pid enrichment is linear in the page size.
    let event_by_process: std::collections::HashMap<u64, usize> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            event
                .get("process_id")
                .and_then(Value::as_u64)
                .map(|id| (id, index))
        })
        .collect();
    let process_pids = query_json(
        connection,
        "SELECT process_id, host_pid FROM processes",
        None,
        None,
        |row| {
            Ok(
                json!({"process_id": row.get::<_, u64>(0)?, "host_pid": row.get::<_, Option<u32>>(1)?}),
            )
        },
    )?;
    for item in process_pids {
        if let (Some(id), Some(pid)) = (
            item.get("process_id").and_then(Value::as_u64),
            item.get("host_pid").and_then(Value::as_u64),
        ) {
            if let Some(index) = event_by_process.get(&id) {
                events[*index]["host_pid"] = json!(pid);
            }
        }
    }
    enrich_fd_paths(&mut events);
    let payload_limit = page_size
        .map(|limit| format!(" LIMIT {limit}"))
        .unwrap_or_else(|| " LIMIT -1".to_string());
    let payload_bytes = if full { "bytes" } else { "NULL" };
    let payload_sql = format!(
        "SELECT segment_id, trace_id, process_id, session_id, observed_at, source, content_state,
                direction, stream_key, sequence, original_size, captured_size,
                library, symbol, protocol_hint, loss_reason, {payload_bytes}, call_id
         FROM payload_segments WHERE (?1 IS NULL OR trace_id = ?1)
           AND (?2 IS NULL OR session_id = ?2)
           AND (?3 IS NULL OR call_id = ?3)
         ORDER BY trace_id, sequence, segment_id{payload_limit}"
    );
    let mut payloads = query_json_with_call(
        connection,
        &payload_sql,
        trace_id,
        session_id,
        call_id,
        |row| {
            let bytes = row.get::<_, Option<Vec<u8>>>(16)?;
            Ok(json!({
                "segment_id": row.get::<_, u64>(0)?, "trace_id": row.get::<_, u64>(1)?,
                "process_id": row.get::<_, u64>(2)?, "session_id": row.get::<_, Option<String>>(3)?,
                "observed_at": ns(row.get::<_, i64>(4)?), "source": row.get::<_, i64>(5)?,
                "content_state": row.get::<_, i64>(6)?, "direction": row.get::<_, i64>(7)?,
                "stream_key": row.get::<_, Option<String>>(8)?, "sequence": row.get::<_, u64>(9)?,
                "original_size": row.get::<_, u64>(10)?, "captured_size": row.get::<_, u64>(11)?,
                "library": row.get::<_, Option<String>>(12)?, "symbol": row.get::<_, Option<String>>(13)?,
                "protocol_hint": row.get::<_, Option<String>>(14)?,
                "loss_reason": row.get::<_, Option<String>>(15)?,
                "bytes_hex": full.then(|| bytes.as_deref().map(hex)).flatten(),
                "call_id": row.get::<_, Option<String>>(17)?,
            }))
        },
    )?;
    let mut diagnostics = query_json(
        connection,
        "SELECT diagnostic_id, trace_id, observed_at, collector, kind, severity,
                message, dropped, dropped_bytes
         FROM diagnostics WHERE (?1 IS NULL OR trace_id = ?1)
           AND (?2 IS NULL OR trace_id IN (
                SELECT trace_id FROM events_read WHERE session_id = ?2
                UNION SELECT trace_id FROM semantic_actions_read WHERE session_id = ?2
           ))
         ORDER BY trace_id, observed_at, diagnostic_id",
        trace_id,
        session_id,
        |row| {
            Ok(json!({
                "diagnostic_id": row.get::<_, u64>(0)?, "trace_id": row.get::<_, u64>(1)?,
                "observed_at": ns(row.get::<_, i64>(2)?), "collector": row.get::<_, String>(3)?,
                "kind": row.get::<_, i64>(4)?, "severity": row.get::<_, i64>(5)?,
                "message": row.get::<_, String>(6)?, "dropped": row.get::<_, u64>(7)?,
                "dropped_bytes": row.get::<_, u64>(8)?,
            }))
        },
    )?;
    let mut attributes_by_action: std::collections::HashMap<(u64, String), Map<String, Value>> =
        std::collections::HashMap::new();
    let mut attrs_stmt = connection.prepare(
        "SELECT trace_id, action_id, attr_key, attr_value
         FROM semantic_action_attributes
         WHERE (?1 IS NULL OR trace_id = ?1)
         ORDER BY trace_id, action_id, attr_key",
    )?;
    for row in attrs_stmt.query_map([trace_id], |row| {
        Ok((
            row.get::<_, u64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })? {
        let (trace, action, key, value) = row?;
        attributes_by_action
            .entry((trace, action))
            .or_default()
            .insert(key, json!(value));
    }
    let mut actions = Vec::new();
    let action_sql = format!(
        "SELECT a.trace_id, a.action_id, a.kind_name, a.title, a.start_time, a.end_time,
                a.process_id, a.status, a.completeness, a.confidence_millis, a.session_id, a.schema_version
         FROM semantic_actions_read a
         WHERE (?1 IS NULL OR a.trace_id = ?1)
           AND (?2 IS NULL OR a.session_id = ?2)
           AND {}
         ORDER BY a.trace_id, a.start_time, a.action_id",
        call_action_predicate("a")
    );
    let mut statement = connection.prepare(&action_sql)?;
    let rows = statement.query_map(params![trace_id, session_id, call_id], |row| {
        let trace: u64 = row.get(0)?;
        let action_id: String = row.get(1)?;
        let mut action = Map::new();
        action.insert("trace_id".into(), json!(trace));
        action.insert("action_id".into(), json!(action_id.clone()));
        action.insert("kind".into(), json!(row.get::<_, String>(2)?));
        action.insert("title".into(), json!(row.get::<_, String>(3)?));
        action.insert("start_time".into(), ns(row.get::<_, i64>(4)?));
        action.insert(
            "end_time".into(),
            optional_ns(row.get::<_, Option<i64>>(5)?),
        );
        action.insert("process_id".into(), json!(row.get::<_, u64>(6)?));
        action.insert("status".into(), json!(row.get::<_, i64>(7)?));
        action.insert("completeness".into(), json!(row.get::<_, i64>(8)?));
        action.insert(
            "confidence_millis".into(),
            json!(row.get::<_, Option<u16>>(9)?),
        );
        action.insert(
            "session_id".into(),
            json!(row.get::<_, Option<String>>(10)?),
        );
        action.insert("schema_version".into(), json!(row.get::<_, i64>(11)?));
        let attrs = attributes_by_action
            .get(&(trace, action_id.clone()))
            .cloned()
            .unwrap_or_default();
        if full {
            action.insert("attributes".into(), Value::Object(attrs));
        } else {
            action.insert("attribute_count".into(), json!(attrs.len()));
        }
        Ok(Value::Object(action))
    })?;
    for row in rows {
        actions.push(row?);
    }

    let mut links = Vec::new();
    let link_sql = format!(
        "SELECT l.trace_id, l.source_action_id, l.target_action_id, l.role_name,
                l.confidence, l.valid, l.schema_version
         FROM semantic_action_links_read l
         WHERE (?1 IS NULL OR l.trace_id = ?1)
           AND (?2 IS NULL OR EXISTS (SELECT 1 FROM semantic_actions_read a
                                      WHERE a.trace_id = l.trace_id AND a.session_id = ?2
                                        AND (a.action_id = l.source_action_id OR a.action_id = l.target_action_id)))
           AND (?3 IS NULL OR EXISTS (SELECT 1 FROM semantic_actions_read a
                                      WHERE a.trace_id = l.trace_id
                                        AND (a.action_id = l.source_action_id OR a.action_id = l.target_action_id)
                                        AND {}))
         ORDER BY l.trace_id, l.source_action_id, l.target_action_id, l.role",
        call_action_predicate("a")
    );
    let mut links_stmt = connection.prepare(&link_sql)?;
    for row in links_stmt.query_map(params![trace_id, session_id, call_id], |row| {
        Ok(json!({
            "trace_id": row.get::<_, u64>(0)?,
            "source_action_id": row.get::<_, String>(1)?,
            "target_action_id": row.get::<_, String>(2)?,
            "role": row.get::<_, String>(3)?,
            "confidence": row.get::<_, i64>(4)?,
            "valid": row.get::<_, i64>(5)? != 0,
            "schema_version": row.get::<_, i64>(6)?,
        }))
    })? {
        links.push(row?);
    }

    let mut evidence = Vec::new();
    let evidence_sql = format!(
        "SELECT e.trace_id, e.action_id, e.evidence_kind, e.evidence_id, e.evidence_role
         FROM semantic_action_evidence_read e
         WHERE (?1 IS NULL OR e.trace_id = ?1)
           AND (?2 IS NULL OR EXISTS (SELECT 1 FROM semantic_actions_read a
                                      WHERE a.trace_id = e.trace_id AND a.action_id = e.action_id
                                        AND a.session_id = ?2))
           AND (?3 IS NULL OR EXISTS (SELECT 1 FROM semantic_actions_read a
                                      WHERE a.trace_id = e.trace_id AND a.action_id = e.action_id
                                        AND {}))
         ORDER BY e.trace_id, e.action_id, e.evidence_kind, e.evidence_id, e.evidence_role",
        call_action_predicate("a")
    );
    let mut evidence_stmt = connection.prepare(&evidence_sql)?;
    for row in evidence_stmt.query_map(params![trace_id, session_id, call_id], |row| {
        Ok(json!({
            "trace_id": row.get::<_, u64>(0)?,
            "action_id": row.get::<_, String>(1)?,
            "kind": row.get::<_, i64>(2)?,
            "id": row.get::<_, u64>(3)?,
            "role": row.get::<_, String>(4)?,
        }))
    })? {
        evidence.push(row?);
    }

    // Optional tool-call scoping: restrict events and payload segments to one
    // call id (call_id is merged onto rows above when the column exists).
    if let Some(call) = call_id {
        events.retain(|event| {
            event
                .get("call_id")
                .and_then(Value::as_str)
                .is_some_and(|value| value == call)
        });
        payloads.retain(|segment| {
            segment
                .get("call_id")
                .and_then(Value::as_str)
                .is_some_and(|value| value == call)
        });
    }
    if no_internal {
        events.retain(|event| !is_internal_event(event));
    }
    if let Some(cursor) = after_event {
        events.retain(|event| {
            event
                .get("event_id")
                .and_then(Value::as_u64)
                .is_some_and(|event_id| event_id > cursor)
        });
    }
    if let Some(limit) = page_size {
        for values in [
            &mut traces,
            &mut events,
            &mut payloads,
            &mut diagnostics,
            &mut actions,
            &mut links,
            &mut evidence,
        ] {
            values.truncate(limit);
        }
    }
    let next_event = events
        .iter()
        .filter_map(|event| event.get("event_id").and_then(Value::as_u64))
        .max();
    let contents_sql = format!(
        "SELECT c.trace_id, c.action_id, c.content_id, c.kind_name, c.state_name,
                c.payload_reference, c.canonical_json, c.schema_version
         FROM semantic_contents_read c
         WHERE (?1 IS NULL OR c.trace_id = ?1)
           AND (?2 IS NULL OR EXISTS (SELECT 1 FROM semantic_actions_read a
                                      WHERE a.trace_id = c.trace_id AND a.action_id = c.action_id
                                        AND a.session_id = ?2))
           AND (?3 IS NULL OR EXISTS (SELECT 1 FROM semantic_actions_read a
                                      WHERE a.trace_id = c.trace_id AND a.action_id = c.action_id
                                        AND {}))
         ORDER BY c.trace_id, c.action_id, c.content_id",
        call_action_predicate("a")
    );
    let contents = query_json_with_call(
        connection,
        &contents_sql,
        trace_id,
        session_id,
        call_id,
        |row| {
            let canonical_json = row.get::<_, Option<String>>(6)?;
            Ok(json!({
                "trace_id": row.get::<_, u64>(0)?,
                "action_id": row.get::<_, String>(1)?,
                "content_id": row.get::<_, String>(2)?,
                "kind": row.get::<_, String>(3)?,
                "state": row.get::<_, String>(4)?,
                "payload_reference": row.get::<_, Option<String>>(5)?,
                "canonical_json": if full { canonical_json } else { None::<String> },
                "schema_version": row.get::<_, i64>(7)?,
            }))
        },
    )?;
    let sessions = query_json(
        connection,
        "SELECT session_id, display_name, first_seen, last_seen, last_trace_id
         FROM sessions_read WHERE (?2 IS NULL OR session_id = ?2)
         ORDER BY session_id",
        None,
        session_id,
        |row| {
            Ok(json!({
                "session_id": row.get::<_, String>(0)?,
                "display_name": row.get::<_, Option<String>>(1)?,
                "first_seen": ns(row.get::<_, i64>(2)?),
                "last_seen": ns(row.get::<_, i64>(3)?),
                "last_trace_id": row.get::<_, Option<u64>>(4)?,
            }))
        },
    )?;
    let call_spans = query_json(
        connection,
        "SELECT trace_id, session_id, call_id, host_pid, started_at, ended_at, status FROM call_spans WHERE (?1 IS NULL OR trace_id = ?1) AND (?2 IS NULL OR session_id = ?2) ORDER BY trace_id, started_at, call_id",
        trace_id,
        session_id,
        |row| {
            Ok(json!({
                "trace_id": row.get::<_, u64>(0)?, "session_id": row.get::<_, Option<String>>(1)?,
                "call_id": row.get::<_, String>(2)?, "host_pid": row.get::<_, u32>(3)?,
                "started_at": ns(row.get::<_, i64>(4)?), "ended_at": optional_ns(row.get::<_, Option<i64>>(5)?), "status": row.get::<_, Option<String>>(6)?
            }))
        },
    )?;

    let mut output = json!({
        "schema_version": 1,
        "mode": if full { "full" } else { "lite" },
        "trace_id": trace_id,
        "session_id": session_id,
        "traces": traces,
        "processes": processes,
        "memberships": memberships,
        "sessions": sessions,
        "call_spans": call_spans,
        "events": events,
        "payload_segments": payloads,
        "diagnostics": diagnostics,
        "actions": actions.clone(),
        "semantic_actions": actions,
        "links": links,
        "evidence": evidence,
        "contents": contents,
        "pagination": {
            "page_size": page_size,
            "after_event": after_event,
            "next_event": next_event,
        },
    });
    let encoded = serde_json::to_vec(&output).expect("JSON serialization");
    if encoded.len() > max_bytes {
        output["truncated"] = json!(true);
        output["original_json_bytes"] = json!(encoded.len());
        output["size_limit_bytes"] = json!(max_bytes);
        if let Some(object) = output.as_object_mut() {
            for key in [
                "events",
                "payload_segments",
                "actions",
                "links",
                "evidence",
                "contents",
            ] {
                object.insert(key.into(), Value::Array(Vec::new()));
            }
        }
    } else {
        output["truncated"] = json!(false);
    }
    Ok(output)
}

fn query_json<F>(
    connection: &Connection,
    sql: &str,
    trace_id: Option<u64>,
    session_id: Option<&str>,
    mut map: F,
) -> Result<Vec<Value>, rusqlite::Error>
where
    F: FnMut(&rusqlite::Row<'_>) -> Result<Value, rusqlite::Error>,
{
    let mut statement = connection.prepare(sql)?;
    // Export queries use different subsets of the optional filters, so bind
    // exactly the parameter count the statement declares: `parameter_count`
    // reports the highest referenced index, which also covers statements
    // that only reference `?2`.
    match statement.parameter_count() {
        0 => statement.query_map([], |row| map(row))?.collect(),
        1 => statement
            .query_map(params![trace_id], |row| map(row))?
            .collect(),
        2 => statement
            .query_map(params![trace_id, session_id], |row| map(row))?
            .collect(),
        count => Err(rusqlite::Error::InvalidParameterCount(2, count)),
    }
}

fn query_json_with_call<F>(
    connection: &Connection,
    sql: &str,
    trace_id: Option<u64>,
    session_id: Option<&str>,
    call_id: Option<&str>,
    mut map: F,
) -> Result<Vec<Value>, rusqlite::Error>
where
    F: FnMut(&rusqlite::Row<'_>) -> Result<Value, rusqlite::Error>,
{
    let mut statement = connection.prepare(sql)?;
    match statement.parameter_count() {
        0 => statement.query_map([], |row| map(row))?.collect(),
        1 => statement
            .query_map(params![trace_id], |row| map(row))?
            .collect(),
        2 => statement
            .query_map(params![trace_id, session_id], |row| map(row))?
            .collect(),
        3 => statement
            .query_map(params![trace_id, session_id, call_id], |row| map(row))?
            .collect(),
        count => Err(rusqlite::Error::InvalidParameterCount(3, count)),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn event_kind_name(kind: i64) -> &'static str {
    match kind {
        1 => "process",
        2 => "file",
        3 => "net",
        4 => "ipc",
        5 => "stdio",
        6 => "application",
        7 => "resource",
        8 => "control",
        9 => "loss",
        _ => "unknown",
    }
}

/// Classify operation-bearing observations explicitly for consumers that need
/// a visible “system call” group instead of inferring it from event kind.
fn observation_category(kind: i64, operation: &str) -> &'static str {
    let _ = operation;
    match kind {
        2 => "file",
        3 => "network",
        1 => "process",
        4 => "ipc",
        5 => "stdio",
        _ => event_kind_name(kind),
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn enrich_fd_paths_attaches_names_from_open_records() {
        // openat binds fd->path; read/write on the fd reuse it; close unbinds; non-file events stay untouched.
        let mut events = vec![
            json!({"trace_id": 7, "process_id": 11, "kind_name": "file", "operation": "openat", "fd": 3, "path": "/tmp/data.txt", "metadata": {}}),
            json!({"trace_id": 7, "process_id": 11, "kind_name": "file", "operation": "read", "fd": 3, "path": null, "metadata": {}}),
            json!({"trace_id": 7, "process_id": 11, "kind_name": "file", "operation": "write", "fd": 3, "path": null, "metadata": {}}),
            json!({"trace_id": 7, "process_id": 11, "kind_name": "file", "operation": "close", "fd": 3, "path": null, "metadata": {}}),
            json!({"trace_id": 7, "process_id": 11, "kind_name": "file", "operation": "write", "fd": 3, "path": null, "metadata": {}}),
            json!({"trace_id": 7, "process_id": 11, "kind_name": "file", "operation": "openat", "fd": 3, "path": "/tmp/other.log", "metadata": {}}),
            json!({"trace_id": 7, "process_id": 11, "kind_name": "file", "operation": "write", "fd": 3, "path": null, "metadata": {}}),
            json!({"trace_id": 7, "process_id": 12, "kind_name": "net", "operation": "write", "fd": 3, "path": null, "metadata": {}}),
        ];
        enrich_fd_paths(&mut events);
        assert_eq!(events[1]["path"], "/tmp/data.txt");
        assert_eq!(events[2]["path"], "/tmp/data.txt");
        assert_eq!(events[3]["path"], Value::Null);
        assert_eq!(events[4]["path"], Value::Null);
        assert_eq!(events[6]["path"], "/tmp/other.log");
        assert_eq!(events[7]["path"], Value::Null);
    }

    #[test]
    fn enrich_fd_paths_follows_dup_target_fd() {
        let mut events = vec![
            json!({"trace_id": 7, "process_id": 11, "kind_name": "file", "operation": "openat", "fd": 5, "path": "/etc/passwd", "metadata": {}}),
            json!({"trace_id": 7, "process_id": 11, "kind_name": "file", "operation": "dup2", "fd": 5, "path": null, "metadata": {"target_fd": "7"}}),
            json!({"trace_id": 7, "process_id": 11, "kind_name": "file", "operation": "write", "fd": 7, "path": null, "metadata": {}}),
        ];
        enrich_fd_paths(&mut events);
        assert_eq!(events[1]["path"], Value::Null);
        assert_eq!(events[2]["path"], "/etc/passwd");
    }

    use super::*;

    fn schema(connection: &Connection) {
        connection
            .execute_batch(
                "CREATE TABLE traces(trace_id INTEGER, display_name TEXT, profile_name TEXT,
                    lifecycle_state TEXT, health TEXT, created_at INTEGER);
                 CREATE TABLE processes(process_id INTEGER, host_pid INTEGER, host_task_id INTEGER,
                    host_start_ticks INTEGER, host_start_boottime_ns INTEGER, resolution_state TEXT,
                    session_id TEXT);
                 CREATE TABLE memberships(trace_id INTEGER, process_id INTEGER, inherited_from_process_id INTEGER,
                    observed_at INTEGER, capture_enabled INTEGER, propagation_enabled INTEGER,
                    membership_state TEXT, exit_code INTEGER, exit_observed_at INTEGER,
                    exit_observation_source TEXT);
                 CREATE TABLE call_spans(trace_id INTEGER, session_id TEXT, call_id TEXT, host_pid INTEGER,
                    started_at INTEGER, ended_at INTEGER, status TEXT);
                 CREATE TABLE events(event_id INTEGER, trace_id INTEGER, observed_at INTEGER,
                    process_id INTEGER, kind INTEGER, flags INTEGER, operation TEXT, path TEXT,
                    fd INTEGER, size_bytes INTEGER, endpoint TEXT, direction INTEGER, result INTEGER,
                    stream TEXT, protocol TEXT, loss_reason TEXT, dropped INTEGER, dropped_bytes INTEGER,
                    session_id TEXT, collector TEXT, parent_process_id INTEGER, executable TEXT,
                    argv TEXT, argv_flags INTEGER, channel TEXT, resource TEXT, call_id TEXT, metadata TEXT);
                 CREATE VIEW events_read AS SELECT * FROM events;
                 CREATE TABLE payload_segments(segment_id INTEGER, trace_id INTEGER, process_id INTEGER,
                    session_id TEXT, observed_at INTEGER, source INTEGER, content_state INTEGER, direction INTEGER,
                    stream_key TEXT, sequence INTEGER, original_size INTEGER, captured_size INTEGER,
                    library TEXT, symbol TEXT, protocol_hint TEXT, loss_reason TEXT, bytes BLOB, call_id TEXT);
                 CREATE TABLE diagnostics(diagnostic_id INTEGER, trace_id INTEGER, observed_at INTEGER,
                    collector TEXT, kind INTEGER, severity INTEGER, message TEXT, dropped INTEGER, dropped_bytes INTEGER);
                 CREATE TABLE semantic_actions_read(trace_id INTEGER, action_id TEXT, kind_name TEXT,
                    title TEXT, start_time INTEGER, end_time INTEGER, process_id INTEGER, status INTEGER,
                    completeness INTEGER, confidence_millis INTEGER, session_id TEXT, schema_version INTEGER);
                 CREATE TABLE semantic_action_attributes(trace_id INTEGER, action_id TEXT, attr_key TEXT, attr_value TEXT);
                 CREATE TABLE semantic_action_links_read(trace_id INTEGER, source_action_id TEXT,
                    target_action_id TEXT, role_name TEXT, confidence INTEGER, valid INTEGER,
                    schema_version INTEGER, role INTEGER);
                 CREATE TABLE semantic_action_evidence_read(trace_id INTEGER, action_id TEXT,
                    evidence_kind INTEGER, evidence_id INTEGER, evidence_role TEXT);
                 CREATE TABLE semantic_contents_read(trace_id INTEGER, action_id TEXT,
                    content_id TEXT, kind_name TEXT, state_name TEXT,
                    payload_reference TEXT, canonical_json TEXT, schema_version INTEGER);
                 CREATE TABLE sessions(session_id TEXT, display_name TEXT, first_seen INTEGER,
                    last_seen INTEGER, last_trace_id INTEGER);
                 CREATE VIEW sessions_read AS SELECT * FROM sessions;",
            )
            .unwrap();
    }

    #[test]
    fn lite_does_not_embed_payload_and_limit_is_explicit() {
        let connection = Connection::open_in_memory().unwrap();
        schema(&connection);
        connection
            .execute(
                "INSERT INTO processes VALUES (2,1234,NULL,NULL,NULL,'resolved','sess-a')",
                [],
            )
            .unwrap();
        connection.execute("INSERT INTO payload_segments VALUES (1,1,2,NULL,0,1,1,2,NULL,1,3,3,NULL,NULL,NULL,NULL,X'616263',NULL)", []).unwrap();
        let lite = export(&connection, Some(1), false, usize::MAX).unwrap();
        assert_eq!(lite["processes"][0]["host_pid"], 1234);
        assert!(lite["payload_segments"][0]["bytes_hex"].is_null());
        let limited = export(&connection, Some(1), true, 1).unwrap();
        assert_eq!(limited["truncated"], true);
        assert_eq!(limited["payload_segments"], json!([]));
        assert_eq!(limited["size_limit_bytes"], 1);
    }

    #[test]
    fn nanosecond_timestamps_are_json_strings() {
        let connection = Connection::open_in_memory().unwrap();
        schema(&connection);
        connection
            .execute(
                "INSERT INTO events VALUES
                 (1,1,9223372036854775000,2,0,0,'read',NULL,NULL,1,NULL,0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'test',NULL,NULL,NULL,0,NULL,NULL,NULL,'{}')",
                [],
            )
            .unwrap();
        let output = export(&connection, Some(1), false, usize::MAX).unwrap();
        assert_eq!(output["events"][0]["observed_at"], "9223372036854775000");
    }

    #[test]
    fn session_id_filters_events_and_actions() {
        let connection = Connection::open_in_memory().unwrap();
        schema(&connection);
        connection
            .execute(
                "INSERT INTO events VALUES
                 (1,1,1,2,0,0,'read',NULL,NULL,1,NULL,0,NULL,NULL,NULL,NULL,NULL,NULL,'sess-a','test',NULL,NULL,NULL,0,NULL,NULL,NULL,'{}'),
                 (2,1,2,2,0,0,'write',NULL,NULL,1,NULL,0,NULL,NULL,NULL,NULL,NULL,NULL,'sess-b','test',NULL,NULL,NULL,0,NULL,NULL,NULL,'{}')",
                [],
            )
            .unwrap();
        connection
            .execute("INSERT INTO sessions VALUES ('sess-a', NULL, 1, 2, 1)", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO payload_segments VALUES
                 (1,1,2,'sess-a',1,1,1,2,NULL,1,1,1,NULL,NULL,NULL,NULL,NULL,NULL),
                 (2,1,2,'sess-b',2,1,1,2,NULL,2,1,1,NULL,NULL,NULL,NULL,NULL,NULL)",
                [],
            )
            .unwrap();
        let output = export_with_paging(
            &connection,
            None,
            Some("sess-a"),
            None,
            false,
            false,
            usize::MAX,
            None,
            None,
        )
        .unwrap();
        assert_eq!(output["events"].as_array().unwrap().len(), 1);
        assert_eq!(output["events"][0]["session_id"], "sess-a");
        assert_eq!(output["payload_segments"].as_array().unwrap().len(), 1);
        assert_eq!(output["payload_segments"][0]["session_id"], "sess-a");
        assert_eq!(output["sessions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn event_cursor_and_page_size_are_reported() {
        let connection = Connection::open_in_memory().unwrap();
        schema(&connection);
        connection
            .execute(
                "INSERT INTO events VALUES
                 (1,1,1,2,0,0,'read',NULL,NULL,1,NULL,0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'test',NULL,NULL,NULL,0,NULL,NULL,NULL,'{}'),
                 (2,1,2,2,0,0,'write',NULL,NULL,1,NULL,0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'test',NULL,NULL,NULL,0,NULL,NULL,NULL,'{}')",
                [],
            )
            .unwrap();
        let page = export_with_paging(
            &connection,
            Some(1),
            None,
            None,
            false,
            false,
            usize::MAX,
            Some(1),
            Some(1),
        )
        .unwrap();
        assert_eq!(page["events"].as_array().unwrap().len(), 1);
        assert_eq!(page["events"][0]["event_id"], 2);
        assert_eq!(page["pagination"]["next_event"], 2);
    }

    #[test]
    fn call_id_filter_restricts_events_and_payload_segments() {
        let connection = Connection::open_in_memory().unwrap();
        schema(&connection);
        connection
            .execute_batch(
                "INSERT INTO events(event_id, trace_id, observed_at, process_id, kind, flags, collector, operation, path,
                     fd, size_bytes, endpoint, direction, result, stream, protocol, loss_reason, dropped,
                     dropped_bytes, session_id, call_id, argv_flags, metadata)
                 VALUES (1,1,1,2,0,0,'test','read',NULL,NULL,NULL,NULL,1,0,NULL,NULL,NULL,0,0,'sess-a','call-a',0,'{}'),
                        (2,1,2,2,0,0,'test','write',NULL,NULL,NULL,NULL,1,0,NULL,NULL,NULL,0,0,'sess-a','call-b',0,'{}');
                 INSERT INTO payload_segments(segment_id, trace_id, process_id, session_id, observed_at, source,
                     content_state, direction, stream_key, sequence, original_size, captured_size,
                     library, symbol, protocol_hint, loss_reason, bytes, call_id)
                 VALUES (1,1,2,'sess-a',1,1,1,1,'k',10,100,100,NULL,NULL,NULL,NULL,NULL,'call-a'),
                        (2,1,2,'sess-a',2,1,1,1,'k',11,100,100,NULL,NULL,NULL,NULL,NULL,'call-b')",
            )
            .unwrap();
        let call_a = export_with_paging(
            &connection,
            Some(1),
            Some("sess-a"),
            Some("call-a"),
            false,
            false,
            usize::MAX,
            None,
            None,
        )
        .unwrap();
        let events = call_a["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["call_id"], "call-a");
        let segments = call_a["payload_segments"].as_array().unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0]["call_id"], "call-a");
        let both = export_with_paging(
            &connection,
            Some(1),
            Some("sess-a"),
            None,
            false,
            false,
            usize::MAX,
            None,
            None,
        )
        .unwrap();
        assert_eq!(both["events"].as_array().unwrap().len(), 2);
        assert_eq!(both["payload_segments"].as_array().unwrap().len(), 2);
    }
}
