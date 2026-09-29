//! SQLite schema for traces, process-tree membership, and lifecycle events.

use rusqlite::Connection;

const SQLITE_SCHEMA_VERSION_CURRENT: i32 = 2;
const CREATE_TABLES_SQL: &str = r#"
CREATE TABLE process_id_sequence (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    next_process_id INTEGER NOT NULL
);
INSERT INTO process_id_sequence (singleton, next_process_id) VALUES (1, 1);

CREATE TABLE trace_id_sequence (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    next_trace_id INTEGER NOT NULL
);
INSERT INTO trace_id_sequence (singleton, next_trace_id) VALUES (1, 1);

CREATE TABLE event_id_sequence (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    next_event_id INTEGER NOT NULL
);
INSERT INTO event_id_sequence (singleton, next_event_id) VALUES (1, 1);

CREATE TABLE spool_checkpoint (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    last_sequence INTEGER NOT NULL
);
INSERT INTO spool_checkpoint (singleton, last_sequence) VALUES (1, -1);

CREATE TABLE processes (
    process_id INTEGER PRIMARY KEY,
    host_pid INTEGER,
    host_task_id INTEGER,
    host_start_ticks INTEGER,
    host_start_boottime_ns INTEGER,
    resolution_state TEXT NOT NULL,
    session_id TEXT
);

CREATE TABLE process_namespace_aliases (
    process_id INTEGER NOT NULL,
    pid_namespace TEXT NOT NULL,
    namespace_pid INTEGER NOT NULL,
    namespace_start_ticks INTEGER NOT NULL,
    PRIMARY KEY (process_id, pid_namespace, namespace_pid, namespace_start_ticks),
    UNIQUE (pid_namespace, namespace_pid, namespace_start_ticks)
);

CREATE TABLE traces (
    trace_id INTEGER PRIMARY KEY,
    root_process_id INTEGER NOT NULL,
    root_pid_namespace TEXT,
    root_container_id TEXT,
    root_working_directory TEXT,
    display_name TEXT NOT NULL,
    profile_name TEXT NOT NULL,
    tags TEXT NOT NULL,
    lifecycle_state TEXT NOT NULL,
    health TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    completed_at INTEGER,
    exited_at INTEGER,
    failed_at INTEGER
);

CREATE TABLE memberships (
    trace_id INTEGER NOT NULL,
    process_id INTEGER NOT NULL,
    inherited_from_process_id INTEGER,
    observed_at INTEGER,
    capture_enabled INTEGER NOT NULL,
    propagation_enabled INTEGER NOT NULL,
    membership_state TEXT NOT NULL,
    exit_code INTEGER,
    exit_observed_at INTEGER,
    exit_observation_source TEXT,
    PRIMARY KEY (trace_id, process_id)
);

CREATE TABLE events (
    event_id INTEGER PRIMARY KEY,
    trace_id INTEGER NOT NULL,
    observed_at INTEGER NOT NULL,
    process_id INTEGER NOT NULL,
    collector TEXT NOT NULL,
    kind INTEGER NOT NULL,
    flags INTEGER NOT NULL,
    operation TEXT NOT NULL,
    parent_process_id INTEGER,
    executable TEXT,
    argv TEXT,
    argv_flags INTEGER NOT NULL DEFAULT 0,
    path TEXT,
    fd INTEGER,
    size_bytes INTEGER,
    endpoint TEXT,
    direction INTEGER NOT NULL DEFAULT 0,
    result INTEGER,
    channel TEXT,
    stream TEXT,
    protocol TEXT,
    resource TEXT,
    loss_reason TEXT,
    dropped INTEGER,
    dropped_bytes INTEGER,
    session_id TEXT,
    call_id TEXT,
    metadata TEXT NOT NULL
);

CREATE INDEX idx_processes_host_pid ON processes (host_pid);
CREATE INDEX idx_process_alias_namespace_pid
    ON process_namespace_aliases (pid_namespace, namespace_pid);
CREATE INDEX idx_memberships_trace_parent
    ON memberships (trace_id, inherited_from_process_id);
CREATE INDEX idx_events_trace_time ON events (trace_id, observed_at, event_id);
CREATE INDEX idx_events_session ON events (session_id, observed_at, event_id);
CREATE INDEX idx_events_call ON events (trace_id, call_id, observed_at);
CREATE INDEX idx_events_unassigned_trace_time
    ON events (trace_id, observed_at, event_id)
    WHERE call_id IS NULL;

CREATE TABLE call_spans (
    trace_id INTEGER NOT NULL,
    session_id TEXT,
    call_id TEXT NOT NULL,
    host_pid INTEGER NOT NULL,
    started_at INTEGER NOT NULL,
    ended_at INTEGER,
    status TEXT,
    PRIMARY KEY (trace_id, call_id)
);
CREATE INDEX idx_call_spans_pid_time ON call_spans(trace_id, host_pid, started_at, ended_at);
CREATE INDEX idx_call_spans_trace_start
    ON call_spans(trace_id, started_at, call_id);

-- Durable attribution backfill queue. The daemon writes one row per call-end
-- window/sweep job inside the same writer transaction that closes the span, so
-- a crash after the control reply can never lose a pending closure. The writer
-- thread claims pending jobs (pending -> running), executes them against
-- committed events, then marks them done; `running` rows left by a crash are
-- reset to `pending` on daemon startup. Times are nanoseconds since the epoch.
CREATE TABLE call_backfill_queue (
    queue_id INTEGER PRIMARY KEY AUTOINCREMENT,
    trace_id INTEGER NOT NULL,
    session_id TEXT,
    call_id TEXT,
    kind TEXT NOT NULL,
    span_started_ns INTEGER,
    span_ended_ns INTEGER,
    scope_from_ns INTEGER NOT NULL,
    scope_to_ns INTEGER NOT NULL,
    state TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    enqueued_at_ns INTEGER NOT NULL,
    updated_at_ns INTEGER NOT NULL
);
CREATE INDEX idx_backfill_queue_state ON call_backfill_queue (state, queue_id);
CREATE INDEX idx_backfill_queue_trace ON call_backfill_queue (trace_id, state, queue_id);

CREATE VIEW events_read AS SELECT * FROM events;

CREATE TABLE payload_segments (
    segment_id INTEGER PRIMARY KEY AUTOINCREMENT,
    trace_id INTEGER NOT NULL,
    process_id INTEGER NOT NULL,
    session_id TEXT,
    observed_at INTEGER NOT NULL,
    source INTEGER NOT NULL,
    content_state INTEGER NOT NULL,
    direction INTEGER NOT NULL,
    stream_key TEXT,
    sequence INTEGER NOT NULL,
    operation_id INTEGER,
    offset_bytes INTEGER,
    completed INTEGER NOT NULL,
    original_size INTEGER NOT NULL,
    captured_size INTEGER NOT NULL,
    library TEXT,
    symbol TEXT,
    protocol_hint TEXT,
    loss_reason TEXT,
    bytes BLOB,
    call_id TEXT
);

CREATE INDEX idx_payload_segments_trace_sequence
    ON payload_segments (trace_id, sequence, segment_id);
CREATE INDEX idx_payload_segments_session
    ON payload_segments (session_id, observed_at, segment_id);

CREATE TABLE diagnostics (
    diagnostic_id INTEGER PRIMARY KEY AUTOINCREMENT,
    trace_id INTEGER NOT NULL,
    observed_at INTEGER NOT NULL,
    collector TEXT NOT NULL,
    kind INTEGER NOT NULL,
    severity INTEGER NOT NULL,
    message TEXT NOT NULL,
    dedupe_key TEXT,
    dropped INTEGER NOT NULL,
    dropped_bytes INTEGER NOT NULL
);

CREATE INDEX idx_diagnostics_trace_time
    ON diagnostics (trace_id, observed_at, diagnostic_id);

CREATE TABLE semantic_actions (
    trace_id INTEGER NOT NULL,
    action_id TEXT NOT NULL,
    kind INTEGER NOT NULL,
    kind_name TEXT NOT NULL,
    title TEXT NOT NULL,
    start_time INTEGER NOT NULL,
    end_time INTEGER,
    process_id INTEGER NOT NULL,
    status INTEGER NOT NULL,
    completeness INTEGER NOT NULL,
    confidence_millis INTEGER,
    session_id TEXT,
    schema_version INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (trace_id, action_id)
);

CREATE TABLE semantic_action_attributes (
    trace_id INTEGER NOT NULL,
    action_id TEXT NOT NULL,
    attr_key TEXT NOT NULL,
    attr_value TEXT NOT NULL,
    PRIMARY KEY (trace_id, action_id, attr_key)
);

CREATE TABLE semantic_action_evidence (
    trace_id INTEGER NOT NULL,
    action_id TEXT NOT NULL,
    evidence_kind INTEGER NOT NULL,
    evidence_id INTEGER NOT NULL,
    evidence_role TEXT NOT NULL,
    PRIMARY KEY (trace_id, action_id, evidence_kind, evidence_id, evidence_role)
);

CREATE TABLE semantic_action_links (
    trace_id INTEGER NOT NULL,
    source_action_id TEXT NOT NULL,
    target_action_id TEXT NOT NULL,
    role INTEGER NOT NULL,
    role_name TEXT NOT NULL,
    confidence INTEGER NOT NULL,
    valid INTEGER NOT NULL,
    schema_version INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (trace_id, source_action_id, target_action_id, role)
);

CREATE TABLE semantic_contents (
    trace_id INTEGER NOT NULL,
    action_id TEXT NOT NULL,
    content_id TEXT NOT NULL,
    kind INTEGER NOT NULL,
    kind_name TEXT NOT NULL,
    state INTEGER NOT NULL,
    state_name TEXT NOT NULL,
    payload_reference TEXT,
    canonical_json TEXT,
    schema_version INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (trace_id, content_id)
);

CREATE INDEX idx_semantic_contents_trace_action
    ON semantic_contents (trace_id, action_id);

CREATE INDEX idx_semantic_actions_trace_time
    ON semantic_actions (trace_id, start_time, action_id);
CREATE INDEX idx_semantic_actions_session
    ON semantic_actions (session_id, start_time, action_id);
CREATE INDEX idx_semantic_links_trace_source
    ON semantic_action_links (trace_id, source_action_id);

-- Session directory: one row per identified session, kept in sync by the
-- daemon as events are ingested.
CREATE TABLE sessions (
    session_id TEXT PRIMARY KEY,
    display_name TEXT,
    first_seen INTEGER NOT NULL,
    last_seen INTEGER NOT NULL,
    last_trace_id INTEGER
);

CREATE INDEX idx_sessions_last_seen ON sessions (last_seen);

CREATE VIEW sessions_read AS
SELECT session_id, display_name, first_seen, last_seen, last_trace_id
FROM sessions;

CREATE VIEW semantic_actions_read AS
SELECT trace_id, action_id, kind, kind_name, title, start_time, end_time,
       process_id, status, completeness, confidence_millis, session_id, schema_version
FROM semantic_actions;

CREATE VIEW semantic_action_links_read AS
SELECT trace_id, source_action_id, target_action_id, role, role_name,
       confidence, valid, schema_version
FROM semantic_action_links;

CREATE VIEW semantic_action_evidence_read AS
SELECT trace_id, action_id, evidence_kind, evidence_id, evidence_role
FROM semantic_action_evidence;

CREATE VIEW semantic_contents_read AS
SELECT trace_id, action_id, content_id, kind_name, state_name,
       payload_reference, canonical_json, schema_version
FROM semantic_contents;
"#;

/// Create the CensorScope schema. Unknown or older layouts are rejected;
/// existing layouts are never migrated.
pub fn initialize(connection: &Connection) -> Result<(), rusqlite::Error> {
    let version = user_version(connection)?;
    if version == SQLITE_SCHEMA_VERSION_CURRENT {
        return validate_current_schema(connection);
    }
    if version == 1 {
        connection.execute_batch(
            "ALTER TABLE diagnostics ADD COLUMN dedupe_key TEXT;
             CREATE UNIQUE INDEX IF NOT EXISTS idx_diagnostics_dedupe
             ON diagnostics (trace_id, dedupe_key)
             WHERE dedupe_key IS NOT NULL;
             PRAGMA user_version = 2;",
        )?;
        return validate_current_schema(connection);
    }
    if version != 0 || user_table_count(connection)? != 0 {
        return Err(rusqlite::Error::InvalidQuery);
    }
    connection.execute_batch(CREATE_TABLES_SQL)?;
    connection.pragma_update(None, "user_version", SQLITE_SCHEMA_VERSION_CURRENT)?;
    validate_current_schema(connection)
}

fn validate_current_schema(connection: &Connection) -> Result<(), rusqlite::Error> {
    for (table, column) in [
        ("trace_id_sequence", "next_trace_id"),
        ("event_id_sequence", "next_event_id"),
        ("processes", "process_id"),
        ("processes", "session_id"),
        ("process_namespace_aliases", "process_id"),
        ("traces", "root_process_id"),
        ("memberships", "process_id"),
        ("events", "operation"),
        ("events", "argv"),
        ("events", "kind"),
        ("events", "path"),
        ("events", "session_id"),
        ("events", "call_id"),
        ("payload_segments", "content_state"),
        ("payload_segments", "session_id"),
        ("payload_segments", "call_id"),
        ("diagnostics", "diagnostic_id"),
        ("diagnostics", "dedupe_key"),
        ("semantic_actions", "action_id"),
        ("semantic_actions", "session_id"),
        ("semantic_action_attributes", "attr_value"),
        ("semantic_action_evidence", "evidence_role"),
        ("semantic_action_links", "valid"),
        ("semantic_contents", "content_id"),
        ("call_backfill_queue", "kind"),
        ("call_backfill_queue", "state"),
        ("call_backfill_queue", "attempts"),
        ("call_backfill_queue", "scope_to_ns"),
    ] {
        if !column_exists(connection, table, column)? {
            return Err(rusqlite::Error::InvalidQuery);
        }
    }
    // Preserve performance indexes when opening databases created before this schema revision.
    connection.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_events_unassigned_trace_time
             ON events (trace_id, observed_at, event_id) WHERE call_id IS NULL;
         CREATE INDEX IF NOT EXISTS idx_call_spans_trace_start
             ON call_spans(trace_id, started_at, call_id);
         CREATE UNIQUE INDEX IF NOT EXISTS idx_diagnostics_dedupe
             ON diagnostics (trace_id, dedupe_key)
             WHERE dedupe_key IS NOT NULL;",
    )?;
    Ok(())
}

fn user_version(connection: &Connection) -> Result<i32, rusqlite::Error> {
    connection.pragma_query_value(None, "user_version", |row| row.get(0))
}

fn user_table_count(connection: &Connection) -> Result<i64, rusqlite::Error> {
    connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )
}

fn column_exists(
    connection: &Connection,
    table: &str,
    column: &str,
) -> Result<bool, rusqlite::Error> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(true);
        }
    }
    Ok(false)
}
