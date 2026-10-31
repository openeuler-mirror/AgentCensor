use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};
use semantic_action_contract::{
    ACTION_VALID_ATTRIBUTE, CENSORSCOPE_ASSOCIATION_STATE_ATTRIBUTE,
    LLM_ASSOCIATION_STATE_ATTRIBUTE,
};
use serde_json::{Value, json};

use crate::args::OtelAttributeMode;
use crate::event_metadata::parse_event_metadata;

const OTEL_SCOPE: &str = "censorscope.offline.otel";
const OTEL_SCHEMA_VERSION: &str = "1";
const ACTION_EXPORT_BATCH_SIZE: usize = 512;

#[derive(Clone, Debug)]
struct TraceExport {
    trace_id: u64,
    display_name: String,
    profile_name: String,
    lifecycle_state: String,
    health: String,
    created_at: u64,
    started_at: Option<u64>,
    audit: AuditSummary,
    #[cfg_attr(not(test), allow(dead_code))]
    actions: Vec<ActionExport>,
    #[cfg_attr(not(test), allow(dead_code))]
    links: Vec<LinkExport>,
}

#[derive(Clone, Copy, Debug, Default)]
struct AuditSummary {
    diagnostic_count: u64,
    diagnostic_warning_count: u64,
    diagnostic_error_count: u64,
    dropped: u64,
    dropped_bytes: u64,
    open_action_count: u64,
    partial_action_count: u64,
    orphan_action_count: u64,
    ambiguous_action_count: u64,
    invalid_link_count: u64,
    coverage_event_count: u64,
    coverage_gap_count: u64,
    tls_coverage_event_count: u64,
    tls_coverage_gap_count: u64,
}

#[derive(Clone, Debug)]
struct ActionExport {
    action_id: String,
    kind: String,
    title: String,
    start_time: u64,
    end_time: Option<u64>,
    process_id: u64,
    status: i64,
    completeness: i64,
    confidence_millis: Option<u16>,
    session_id: Option<String>,
    action_valid: bool,
    agent_observed: bool,
    process_parent_conflict: bool,
    association_state: Option<String>,
    attributes: BTreeMap<String, String>,
    evidence: Vec<EvidenceExport>,
}

#[derive(Clone, Debug)]
struct EvidenceExport {
    kind: i64,
    id: u64,
    role: String,
}

#[derive(Clone, Debug)]
struct LinkExport {
    source_action_id: String,
    #[cfg_attr(not(test), allow(dead_code))]
    target_action_id: String,
    role: String,
    confidence: i64,
    valid: bool,
    link_valid: bool,
}

mod json;
mod protobuf;

use protobuf::write_protobuf_streaming;
#[cfg(test)]
use protobuf::{write_protobuf, trace_id_bytes, proto};

use json::write_json_streaming;
#[cfg(test)]
use json::write_json;

pub fn export_to_path(
    database: &Path,
    trace_id: u64,
    output: &Path,
    attributes: OtelAttributeMode,
    protobuf: bool,
) -> Result<(), String> {
    let connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("cannot open read-only database: {error}"))?;
    let details = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("cannot open OTLP detail reader: {error}"))?;
    write_atomic(output, |writer| {
        if protobuf {
            write_protobuf_streaming(writer, &connection, &details, trace_id, attributes)
                .map_err(|error| error.to_string())
        } else {
            write_json_streaming(writer, &connection, &details, trace_id, attributes)
                .map_err(|error| error.to_string())
        }
    })
    .map_err(|error| format!("write OTLP export failed: {error}"))
}

fn coverage_counts(connection: &Connection, trace_id: u64) -> Result<(u64, u64), rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT metadata FROM events WHERE trace_id = ?1 AND protocol LIKE '%.coverage'",
    )?;
    let mut rows = statement.query([trace_id])?;
    let mut count = 0;
    let mut gaps = 0;
    while let Some(row) = rows.next()? {
        let metadata = parse_event_metadata(&row.get::<_, String>(0)?);
        count += 1;
        if metadata.get("state").and_then(Value::as_str) != Some("covered") {
            gaps += 1;
        }
    }
    Ok((count, gaps))
}

fn load_trace_header(
    connection: &Connection,
    trace_id: u64,
) -> Result<TraceExport, rusqlite::Error> {
    let (display_name, profile_name, lifecycle_state, health, created_at, started_at) = connection
        .query_row(
            "SELECT display_name, profile_name, lifecycle_state, health, created_at, started_at
             FROM traces WHERE trace_id = ?1",
            [trace_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    nanos(row.get::<_, i64>(4)?)?,
                    row.get::<_, Option<i64>>(5)?.map(nanos).transpose()?,
                ))
            },
        )?;
    Ok(TraceExport {
        trace_id,
        display_name,
        profile_name,
        lifecycle_state,
        health,
        created_at,
        started_at,
        audit: load_audit_summary(connection, trace_id)?,
        actions: Vec::new(),
        links: Vec::new(),
    })
}

fn load_audit_summary(
    connection: &Connection,
    trace_id: u64,
) -> Result<AuditSummary, rusqlite::Error> {
    let diagnostics = connection.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(CASE WHEN severity = 2 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN severity = 3 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(dropped), 0), COALESCE(SUM(dropped_bytes), 0)
         FROM diagnostics WHERE trace_id = ?1",
        [trace_id],
        |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, u64>(2)?,
                row.get::<_, u64>(3)?,
                row.get::<_, u64>(4)?,
            ))
        },
    )?;
    let coverage = coverage_counts(connection, trace_id)?;
    let actions = connection.query_row(
        "SELECT
            COALESCE(SUM(end_time IS NULL), 0),
            COALESCE(SUM(completeness = 2), 0),
            COALESCE(SUM(EXISTS (
                SELECT 1 FROM semantic_action_attributes a
                WHERE a.trace_id = semantic_actions_read.trace_id
                  AND a.action_id = semantic_actions_read.action_id
                  AND a.attr_key IN (?2, ?3) AND a.attr_value = 'orphan'
            )), 0),
            COALESCE(SUM(EXISTS (
                SELECT 1 FROM semantic_action_attributes a
                WHERE a.trace_id = semantic_actions_read.trace_id
                  AND a.action_id = semantic_actions_read.action_id
                  AND a.attr_key IN (?2, ?3) AND a.attr_value = 'ambiguous'
            )), 0)
         FROM semantic_actions_read WHERE trace_id = ?1",
        rusqlite::params![
            trace_id,
            LLM_ASSOCIATION_STATE_ATTRIBUTE,
            CENSORSCOPE_ASSOCIATION_STATE_ATTRIBUTE
        ],
        |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, u64>(2)?,
                row.get::<_, u64>(3)?,
            ))
        },
    )?;
    let invalid_links = connection.query_row(
        "SELECT COALESCE(SUM(CASE WHEN link_valid_code = 0 THEN 1 ELSE 0 END), 0)
         FROM semantic_action_links_read WHERE trace_id = ?1",
        [trace_id],
        |row| row.get::<_, u64>(0),
    )?;
    Ok(AuditSummary {
        diagnostic_count: diagnostics.0,
        diagnostic_warning_count: diagnostics.1,
        diagnostic_error_count: diagnostics.2,
        dropped: diagnostics.3,
        dropped_bytes: diagnostics.4,
        open_action_count: actions.0,
        partial_action_count: actions.1,
        orphan_action_count: actions.2,
        ambiguous_action_count: actions.3,
        invalid_link_count: invalid_links,
        coverage_event_count: coverage.0,
        coverage_gap_count: coverage.1,
        tls_coverage_event_count: coverage.0,
        tls_coverage_gap_count: coverage.1,
    })
}

fn prepare_action_export_batch(details: &Connection) -> Result<(), rusqlite::Error> {
    details.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS export_action_ids (
             action_id TEXT PRIMARY KEY
         )",
    )
}

fn load_action_details_batch(
    details: &Connection,
    trace_id: u64,
    actions: &mut [ActionExport],
    attributes_mode: OtelAttributeMode,
) -> Result<HashMap<String, Vec<LinkExport>>, rusqlite::Error> {
    details.execute("DELETE FROM temp.export_action_ids", [])?;
    {
        let mut insert =
            details.prepare("INSERT INTO temp.export_action_ids (action_id) VALUES (?1)")?;
        for action in actions.iter() {
            insert.execute([&action.action_id])?;
        }
    }
    let positions = actions
        .iter()
        .enumerate()
        .map(|(index, action)| (action.action_id.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut attributes = details.prepare(
        "SELECT a.action_id, a.attr_key, a.attr_value
         FROM semantic_action_attributes a
         JOIN temp.export_action_ids ids ON ids.action_id = a.action_id
         WHERE a.trace_id = ?1
           AND (?2 = 1 OR a.attr_key IN (?3, ?4))
         ORDER BY a.action_id, a.attr_key",
    )?;
    for row in attributes.query_map(
        rusqlite::params![
            trace_id,
            i64::from(attributes_mode == OtelAttributeMode::Full),
            LLM_ASSOCIATION_STATE_ATTRIBUTE,
            CENSORSCOPE_ASSOCIATION_STATE_ATTRIBUTE
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    )? {
        let (action_id, key, value) = row?;
        if let Some(&index) = positions.get(&action_id) {
            let action = &mut actions[index];
            if key == LLM_ASSOCIATION_STATE_ATTRIBUTE
                || key == CENSORSCOPE_ASSOCIATION_STATE_ATTRIBUTE
            {
                action.association_state = Some(value.clone());
            }
            if attributes_mode == OtelAttributeMode::Full {
                action.attributes.insert(key, value);
            }
        }
    }
    let mut evidence = details.prepare(
        "SELECT e.action_id, e.evidence_kind, e.evidence_id, e.evidence_role
         FROM semantic_action_evidence_read e
         JOIN temp.export_action_ids ids ON ids.action_id = e.action_id
         WHERE e.trace_id = ?1
         ORDER BY e.action_id, e.evidence_kind, e.evidence_id, e.evidence_role",
    )?;
    for row in evidence.query_map([trace_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            EvidenceExport {
                kind: row.get(1)?,
                id: row.get(2)?,
                role: row.get(3)?,
            },
        ))
    })? {
        let (action_id, item) = row?;
        if let Some(&index) = positions.get(&action_id) {
            actions[index].evidence.push(item);
        }
    }
    let mut links_by_target = HashMap::<String, Vec<LinkExport>>::new();
    let mut links = details.prepare(
        "SELECT l.target_action_id, l.source_action_id, l.role_name,
                l.confidence, l.valid, l.link_valid_code
         FROM semantic_action_links_read l
         JOIN temp.export_action_ids ids ON ids.action_id = l.target_action_id
         WHERE l.trace_id = ?1
           AND EXISTS (
               SELECT 1 FROM semantic_actions_read source
               WHERE source.trace_id = l.trace_id
                 AND source.action_id = l.source_action_id
           )
         ORDER BY l.target_action_id, l.source_action_id, l.role_name",
    )?;
    for row in links.query_map([trace_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            LinkExport {
                source_action_id: row.get(1)?,
                target_action_id: row.get(0)?,
                role: row.get(2)?,
                confidence: row.get(3)?,
                valid: row.get::<_, Option<i64>>(4)?.unwrap_or(0) != 0,
                link_valid: row.get::<_, Option<i64>>(5)?.unwrap_or(1) != 0,
            },
        ))
    })? {
        let (target, link) = row?;
        links_by_target.entry(target).or_default().push(link);
    }
    Ok(links_by_target)
}

fn stream_actions<F>(
    connection: &Connection,
    details: &Connection,
    header: &TraceExport,
    attributes_mode: OtelAttributeMode,
    mut emit: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: FnMut(&TraceExport, &ActionExport, &[LinkExport]) -> Result<(), Box<dyn std::error::Error>>,
{
    prepare_action_export_batch(details)?;
    let mut statement = connection.prepare(
        "SELECT action_id, kind_name, title, start_time, end_time, process_id,
                status, completeness, confidence_millis, session_id,
                action_valid_code, agent_observed, process_parent_conflict
         FROM semantic_actions_read
         WHERE trace_id = ?1
         ORDER BY start_time, action_id",
    )?;
    let mut rows = statement.query([header.trace_id])?;
    loop {
        let mut actions = Vec::with_capacity(ACTION_EXPORT_BATCH_SIZE);
        while actions.len() < ACTION_EXPORT_BATCH_SIZE {
            let Some(row) = rows.next()? else { break };
            actions.push(ActionExport {
                action_id: row.get(0)?,
                kind: row.get(1)?,
                title: row.get(2)?,
                start_time: nanos(row.get::<_, i64>(3)?)?,
                end_time: row.get::<_, Option<i64>>(4)?.map(nanos).transpose()?,
                process_id: row.get(5)?,
                status: row.get(6)?,
                completeness: row.get(7)?,
                confidence_millis: row.get(8)?,
                session_id: row.get(9)?,
                action_valid: row.get::<_, Option<i64>>(10)?.unwrap_or(1) != 0,
                agent_observed: row.get::<_, Option<i64>>(11)?.unwrap_or(0) != 0,
                process_parent_conflict: row.get::<_, Option<i64>>(12)?.unwrap_or(0) != 0,
                association_state: None,
                attributes: BTreeMap::new(),
                evidence: Vec::new(),
            });
        }
        if actions.is_empty() {
            break;
        }
        let links =
            load_action_details_batch(details, header.trace_id, &mut actions, attributes_mode)?;
        for action in &actions {
            emit(
                &header,
                action,
                links
                    .get(&action.action_id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
fn load_trace(
    connection: &Connection,
    trace_id: u64,
    attributes_mode: OtelAttributeMode,
) -> Result<TraceExport, rusqlite::Error> {
    let meta = connection.query_row(
        "SELECT display_name, profile_name, lifecycle_state, health, created_at, started_at
         FROM traces WHERE trace_id = ?1",
        [trace_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                nanos(row.get::<_, i64>(4)?)?,
                row.get::<_, Option<i64>>(5)?.map(nanos).transpose()?,
            ))
        },
    )?;

    let mut actions = Vec::new();
    let mut statement = connection.prepare(
        "SELECT action_id, kind_name, title, start_time, end_time, process_id,
                status, completeness, confidence_millis, session_id,
                action_valid_code, agent_observed, process_parent_conflict
         FROM semantic_actions_read
         WHERE trace_id = ?1
         ORDER BY start_time, action_id",
    )?;
    let rows = statement.query_map([trace_id], |row| {
        Ok(ActionExport {
            action_id: row.get(0)?,
            kind: row.get(1)?,
            title: row.get(2)?,
            start_time: nanos(row.get::<_, i64>(3)?)?,
            end_time: row.get::<_, Option<i64>>(4)?.map(nanos).transpose()?,
            process_id: row.get(5)?,
            status: row.get(6)?,
            completeness: row.get(7)?,
            confidence_millis: row.get(8)?,
            session_id: row.get(9)?,
            action_valid: row.get::<_, Option<i64>>(10)?.unwrap_or(1) != 0,
            agent_observed: row.get::<_, Option<i64>>(11)?.unwrap_or(0) != 0,
            process_parent_conflict: row.get::<_, Option<i64>>(12)?.unwrap_or(0) != 0,
            association_state: None,
            attributes: BTreeMap::new(),
            evidence: Vec::new(),
        })
    })?;
    for row in rows {
        actions.push(row?);
    }

    let mut action_positions = HashMap::with_capacity(actions.len());
    for (index, action) in actions.iter().enumerate() {
        action_positions.insert(action.action_id.clone(), index);
    }

    if attributes_mode == OtelAttributeMode::Full {
        let mut statement = connection.prepare(
            "SELECT action_id, attr_key, attr_value
             FROM semantic_action_attributes
             WHERE trace_id = ?1
             ORDER BY action_id, attr_key",
        )?;
        for row in statement.query_map([trace_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })? {
            let (action_id, key, value) = row?;
            if let Some(index) = action_positions.get(&action_id) {
                if key == LLM_ASSOCIATION_STATE_ATTRIBUTE
                    || key == CENSORSCOPE_ASSOCIATION_STATE_ATTRIBUTE
                {
                    actions[*index].association_state = Some(value.clone());
                }
                actions[*index].attributes.insert(key, value);
            }
        }
    } else {
        let mut statement = connection.prepare(
            "SELECT action_id, attr_key, attr_value
             FROM semantic_action_attributes
             WHERE trace_id = ?1
               AND attr_key IN (?2, ?3)
             ORDER BY action_id, attr_key",
        )?;
        for row in statement.query_map(
            rusqlite::params![
                trace_id,
                LLM_ASSOCIATION_STATE_ATTRIBUTE,
                CENSORSCOPE_ASSOCIATION_STATE_ATTRIBUTE
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )? {
            let (action_id, _key, value) = row?;
            if let Some(index) = action_positions.get(&action_id) {
                actions[*index].association_state = Some(value);
            }
        }
    }

    let mut statement = connection.prepare(
        "SELECT action_id, evidence_kind, evidence_id, evidence_role
         FROM semantic_action_evidence_read
         WHERE trace_id = ?1
         ORDER BY action_id, evidence_kind, evidence_id, evidence_role",
    )?;
    for row in statement.query_map([trace_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            EvidenceExport {
                kind: row.get(1)?,
                id: row.get(2)?,
                role: row.get(3)?,
            },
        ))
    })? {
        let (action_id, evidence) = row?;
        if let Some(index) = action_positions.get(&action_id) {
            actions[*index].evidence.push(evidence);
        }
    }

    let mut links = Vec::new();
    let mut statement = connection.prepare(
        "SELECT source_action_id, target_action_id, role_name, confidence, valid, link_valid_code
         FROM semantic_action_links_read
         WHERE trace_id = ?1
         ORDER BY source_action_id, target_action_id, role",
    )?;
    for row in statement.query_map([trace_id], |row| {
        Ok(LinkExport {
            source_action_id: row.get(0)?,
            target_action_id: row.get(1)?,
            role: row.get(2)?,
            confidence: row.get(3)?,
            valid: row.get::<_, Option<i64>>(4)?.unwrap_or(0) != 0,
            link_valid: row.get::<_, Option<i64>>(5)?.unwrap_or(1) != 0,
        })
    })? {
        links.push(row?);
    }

    let mut span_ids = HashMap::with_capacity(actions.len());
    for action in &actions {
        let span_id = stable_hash(action.action_id.as_bytes());
        if let Some(previous) = span_ids.insert(span_id, action.action_id.as_str()) {
            return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                std::io::Error::other(format!(
                    "span id collision between actions {previous} and {}",
                    action.action_id
                )),
            )));
        }
    }

    let diagnostic_summary = connection.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(CASE WHEN severity = 2 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN severity = 3 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(dropped), 0), COALESCE(SUM(dropped_bytes), 0)
         FROM diagnostics WHERE trace_id = ?1",
        [trace_id],
        |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, u64>(2)?,
                row.get::<_, u64>(3)?,
                row.get::<_, u64>(4)?,
            ))
        },
    )?;
    let coverage_summary = coverage_counts(connection, trace_id)?;
    let mut audit = AuditSummary {
        diagnostic_count: diagnostic_summary.0,
        diagnostic_warning_count: diagnostic_summary.1,
        diagnostic_error_count: diagnostic_summary.2,
        dropped: diagnostic_summary.3,
        dropped_bytes: diagnostic_summary.4,
        invalid_link_count: links.iter().filter(|link| !link.link_valid).count() as u64,
        coverage_event_count: coverage_summary.0,
        coverage_gap_count: coverage_summary.1,
        tls_coverage_event_count: coverage_summary.0,
        tls_coverage_gap_count: coverage_summary.1,
        ..AuditSummary::default()
    };
    for action in &actions {
        if action.end_time.is_none() {
            audit.open_action_count += 1;
        }
        if action.completeness == 2 {
            audit.partial_action_count += 1;
        }
        match action.association_state.as_deref() {
            Some("orphan") => audit.orphan_action_count += 1,
            Some("ambiguous") => audit.ambiguous_action_count += 1,
            _ => {}
        }
    }

    Ok(TraceExport {
        trace_id,
        display_name: meta.0,
        profile_name: meta.1,
        lifecycle_state: meta.2,
        health: meta.3,
        created_at: meta.4,
        started_at: meta.5,
        audit,
        actions,
        links,
    })
}

#[cfg(test)]
fn parent_map(trace: &TraceExport) -> HashMap<&str, &str> {
    let action_ids = trace
        .actions
        .iter()
        .map(|action| action.action_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut candidates: HashMap<&str, Vec<&str>> = HashMap::new();
    for link in &trace.links {
        if link.valid
            && link.link_valid
            && is_structural_role(&link.role)
            && action_ids.contains(link.source_action_id.as_str())
            && action_ids.contains(link.target_action_id.as_str())
        {
            candidates
                .entry(link.target_action_id.as_str())
                .or_default()
                .push(link.source_action_id.as_str());
        }
    }
    candidates
        .into_iter()
        .filter_map(|(child, parents)| {
            if parents.len() == 1 {
                Some((child, parents[0]))
            } else {
                None
            }
        })
        .collect()
}

fn is_structural_role(role: &str) -> bool {
    role == "agent.performed_action" || role.contains(".contains_")
}

fn action_audit_state(action: &ActionExport) -> &'static str {
    match action.association_state.as_deref() {
        Some("orphan") => "orphan",
        Some("ambiguous") => "ambiguous",
        _ if action.end_time.is_none() => "open",
        _ if action.completeness == 2 => "partial",
        _ if action.completeness == 1 => "complete",
        _ => "unknown",
    }
}

fn status_name(value: i64) -> &'static str {
    match value {
        2 => "success",
        3 => "error",
        1 => "in_progress",
        _ => "unknown",
    }
}

fn completeness_name(value: i64) -> &'static str {
    match value {
        1 => "complete",
        2 => "partial",
        3 => "inferred",
        _ => "unknown",
    }
}

fn status_code(value: i64) -> &'static str {
    match value {
        2 => "STATUS_CODE_OK",
        3 => "STATUS_CODE_ERROR",
        _ => "STATUS_CODE_UNSET",
    }
}

fn nanos(value: i64) -> Result<u64, rusqlite::Error> {
    u64::try_from(value).map_err(|_| rusqlite::Error::InvalidQuery)
}

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or_default()
}

fn trace_id_hex(trace_id: u64) -> String {
    format!("{trace_id:032x}")
}

fn span_id_hex(action_id: &str) -> String {
    format!("{:016x}", stable_hash(action_id.as_bytes()))
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn write_atomic(
    output: &Path,
    write: impl FnOnce(&mut BufWriter<fs::File>) -> Result<(), String>,
) -> Result<(), String> {
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let nonce = now_nanos();
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("export");
    let temporary = output.with_file_name(format!(".{name}.tmp-{}-{nonce}", std::process::id()));
    let file = fs::File::create(&temporary).map_err(|error| error.to_string())?;
    let mut writer = BufWriter::new(file);
    let result = write(&mut writer).and_then(|_| writer.flush().map_err(|error| error.to_string()));
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    fs::rename(temporary, output).map_err(|error| error.to_string())
}


#[cfg(test)]
mod tests;
