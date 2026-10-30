use super::*;

#[cfg(test)]
pub(super) fn write_json<W: Write>(writer: &mut W, trace: &TraceExport) -> Result<(), serde_json::Error> {
    let snapshot_time = now_nanos();
    let trace_id = trace_id_hex(trace.trace_id);
    let parent_map = parent_map(trace);
    let mut links_by_target: HashMap<&str, Vec<&LinkExport>> = HashMap::new();
    for link in &trace.links {
        links_by_target
            .entry(link.target_action_id.as_str())
            .or_default()
            .push(link);
    }

    write!(
        writer,
        "{{\"resourceSpans\":[{{\"resource\":{{\"attributes\":"
    )
    .map_err(serde_json::Error::io)?;
    let mut resource = vec![
        string_attr("service.name", "censorscope"),
        string_attr("censorscope.trace.id", &trace.trace_id.to_string()),
        string_attr("censorscope.trace.id_format", "u64-zero-extended-v1"),
        string_attr("censorscope.trace.display_name", &trace.display_name),
        string_attr("censorscope.trace.profile_name", &trace.profile_name),
        string_attr("censorscope.trace.lifecycle_state", &trace.lifecycle_state),
        string_attr("censorscope.trace.health", &trace.health),
        int_attr("censorscope.trace.created_at_unix_nano", trace.created_at),
        int_attr(
            "censorscope.audit.diagnostic_count",
            trace.audit.diagnostic_count,
        ),
        int_attr(
            "censorscope.audit.diagnostic_warning_count",
            trace.audit.diagnostic_warning_count,
        ),
        int_attr(
            "censorscope.audit.diagnostic_error_count",
            trace.audit.diagnostic_error_count,
        ),
        int_attr("censorscope.audit.dropped", trace.audit.dropped),
        int_attr("censorscope.audit.dropped_bytes", trace.audit.dropped_bytes),
        int_attr(
            "censorscope.audit.open_action_count",
            trace.audit.open_action_count,
        ),
        int_attr(
            "censorscope.audit.partial_action_count",
            trace.audit.partial_action_count,
        ),
        int_attr(
            "censorscope.audit.orphan_action_count",
            trace.audit.orphan_action_count,
        ),
        int_attr(
            "censorscope.audit.ambiguous_action_count",
            trace.audit.ambiguous_action_count,
        ),
        int_attr(
            "censorscope.audit.invalid_link_count",
            trace.audit.invalid_link_count,
        ),
        int_attr(
            "censorscope.audit.coverage_event_count",
            trace.audit.coverage_event_count,
        ),
        int_attr(
            "censorscope.audit.coverage_gap_count",
            trace.audit.coverage_gap_count,
        ),
        int_attr(
            "censorscope.audit.tls_coverage_event_count",
            trace.audit.tls_coverage_event_count,
        ),
        int_attr(
            "censorscope.audit.tls_coverage_gap_count",
            trace.audit.tls_coverage_gap_count,
        ),
        string_attr("censorscope.export.schema_version", OTEL_SCHEMA_VERSION),
        bool_attr("censorscope.export.snapshot", true),
        int_attr("censorscope.export.snapshot_time_unix_nano", snapshot_time),
    ];
    if let Some(started_at) = trace.started_at {
        resource.push(int_attr(
            "censorscope.trace.started_at_unix_nano",
            started_at,
        ));
    }
    serde_json::to_writer(&mut *writer, &resource)?;
    write!(
        writer,
        "}},\"scopeSpans\":[{{\"scope\":{{\"name\":\"{}\",\"version\":\"{}\"}},\"spans\":[",
        OTEL_SCOPE,
        env!("CARGO_PKG_VERSION")
    )
    .map_err(serde_json::Error::io)?;
    for (index, action) in trace.actions.iter().enumerate() {
        if index > 0 {
            writer.write_all(b",").map_err(serde_json::Error::io)?;
        }
        let value = json_span(
            &trace_id,
            action,
            snapshot_time,
            parent_map.get(action.action_id.as_str()).copied(),
            links_by_target.get(action.action_id.as_str()),
        );
        serde_json::to_writer(&mut *writer, &value)?;
    }
    writer.write_all(b"]}]}]}").map_err(serde_json::Error::io)
}

fn json_resource_attributes(trace: &TraceExport, snapshot_time: u64) -> Vec<Value> {
    let mut resource = vec![
        string_attr("service.name", "censorscope"),
        string_attr("censorscope.trace.id", &trace.trace_id.to_string()),
        string_attr("censorscope.trace.id_format", "u64-zero-extended-v1"),
        string_attr("censorscope.trace.display_name", &trace.display_name),
        string_attr("censorscope.trace.profile_name", &trace.profile_name),
        string_attr("censorscope.trace.lifecycle_state", &trace.lifecycle_state),
        string_attr("censorscope.trace.health", &trace.health),
        int_attr("censorscope.trace.created_at_unix_nano", trace.created_at),
        int_attr(
            "censorscope.audit.diagnostic_count",
            trace.audit.diagnostic_count,
        ),
        int_attr(
            "censorscope.audit.diagnostic_warning_count",
            trace.audit.diagnostic_warning_count,
        ),
        int_attr(
            "censorscope.audit.diagnostic_error_count",
            trace.audit.diagnostic_error_count,
        ),
        int_attr("censorscope.audit.dropped", trace.audit.dropped),
        int_attr("censorscope.audit.dropped_bytes", trace.audit.dropped_bytes),
        int_attr(
            "censorscope.audit.open_action_count",
            trace.audit.open_action_count,
        ),
        int_attr(
            "censorscope.audit.partial_action_count",
            trace.audit.partial_action_count,
        ),
        int_attr(
            "censorscope.audit.orphan_action_count",
            trace.audit.orphan_action_count,
        ),
        int_attr(
            "censorscope.audit.ambiguous_action_count",
            trace.audit.ambiguous_action_count,
        ),
        int_attr(
            "censorscope.audit.invalid_link_count",
            trace.audit.invalid_link_count,
        ),
        int_attr(
            "censorscope.audit.coverage_event_count",
            trace.audit.coverage_event_count,
        ),
        int_attr(
            "censorscope.audit.coverage_gap_count",
            trace.audit.coverage_gap_count,
        ),
        int_attr(
            "censorscope.audit.tls_coverage_event_count",
            trace.audit.tls_coverage_event_count,
        ),
        int_attr(
            "censorscope.audit.tls_coverage_gap_count",
            trace.audit.tls_coverage_gap_count,
        ),
        string_attr("censorscope.export.schema_version", OTEL_SCHEMA_VERSION),
        bool_attr("censorscope.export.snapshot", true),
        int_attr("censorscope.export.snapshot_time_unix_nano", snapshot_time),
    ];
    if let Some(started_at) = trace.started_at {
        resource.push(int_attr(
            "censorscope.trace.started_at_unix_nano",
            started_at,
        ));
    }
    resource
}

pub(super) fn write_json_streaming<W: Write>(
    writer: &mut W,
    connection: &Connection,
    details: &Connection,
    trace_id: u64,
    attributes_mode: OtelAttributeMode,
) -> Result<(), Box<dyn std::error::Error>> {
    let snapshot_time = now_nanos();
    let header = load_trace_header(connection, trace_id)?;
    let trace_id_hex = trace_id_hex(trace_id);
    let resource = json_resource_attributes(&header, snapshot_time);
    write!(
        writer,
        "{{\"resourceSpans\":[{{\"resource\":{{\"attributes\":"
    )?;
    serde_json::to_writer(&mut *writer, &resource)?;
    write!(
        writer,
        "}},\"scopeSpans\":[{{\"scope\":{{\"name\":\"{}\",\"version\":\"{}\"}},\"spans\":[",
        OTEL_SCOPE,
        env!("CARGO_PKG_VERSION")
    )?;
    let mut first = true;
    stream_actions(
        connection,
        details,
        &header,
        attributes_mode,
        |_, action, links| {
            if !first {
                writer.write_all(b",")?;
            }
            first = false;
            let link_refs = links.iter().collect::<Vec<_>>();
            let parent = links
                .iter()
                .filter(|link| link.valid && link.link_valid && is_structural_role(&link.role))
                .map(|link| link.source_action_id.as_str())
                .collect::<Vec<_>>();
            let parent = if parent.len() == 1 {
                Some(parent[0])
            } else {
                None
            };
            let value = json_span(
                &trace_id_hex,
                action,
                snapshot_time,
                parent,
                Some(&link_refs),
            );
            serde_json::to_writer(&mut *writer, &value)?;
            Ok(())
        },
    )?;
    writer.write_all(b"]}]}]}")?;
    Ok(())
}

fn json_span(
    trace_id: &str,
    action: &ActionExport,
    snapshot_time: u64,
    parent: Option<&str>,
    links: Option<&Vec<&LinkExport>>,
) -> Value {
    let end_time = action.end_time.unwrap_or(snapshot_time);
    let mut attributes = vec![
        string_attr("censorscope.action.id", &action.action_id),
        string_attr("censorscope.action.kind", &action.kind),
        string_attr("censorscope.action.status", status_name(action.status)),
        string_attr(
            "censorscope.action.completeness",
            completeness_name(action.completeness),
        ),
        int_attr("censorscope.process.id", action.process_id),
        bool_attr(ACTION_VALID_ATTRIBUTE, action.action_valid),
        bool_attr("censorscope.agent.observed", action.agent_observed),
        bool_attr(
            "censorscope.process.parent_conflict",
            action.process_parent_conflict,
        ),
    ];
    if let Some(confidence) = action.confidence_millis {
        attributes.push(int_attr(
            "censorscope.action.confidence_millis",
            u64::from(confidence),
        ));
    }
    if let Some(session_id) = &action.session_id {
        attributes.push(string_attr("censorscope.session.id", session_id));
    }
    if action.end_time.is_none() {
        attributes.push(bool_attr("censorscope.action.open", true));
    }
    attributes.push(int_attr(
        "censorscope.action.evidence_count",
        action.evidence.len() as u64,
    ));
    attributes.push(string_attr(
        "censorscope.action.audit_state",
        action_audit_state(action),
    ));
    attributes.extend(
        action
            .attributes
            .iter()
            .map(|(key, value)| string_attr(key, value)),
    );
    let events = action
        .evidence
        .iter()
        .map(|evidence| {
            json!({
                "name": "censorscope.evidence",
                "timeUnixNano": action.start_time.to_string(),
                "attributes": [
                    int_attr("censorscope.evidence.kind", evidence.kind as u64),
                    int_attr("censorscope.evidence.id", evidence.id),
                    string_attr("censorscope.evidence.role", &evidence.role),
                ],
            })
        })
        .collect::<Vec<_>>();
    let span_links = links
        .into_iter()
        .flatten()
        .map(|link| {
            json!({
                "traceId": trace_id,
                "spanId": span_id_hex(&link.source_action_id),
                "attributes": [
                    string_attr("censorscope.link.role", &link.role),
                    int_attr("censorscope.link.confidence", link.confidence as u64),
                    bool_attr("censorscope.link.valid", link.valid),
                    bool_attr("censorscope.link.validity", link.link_valid),
                ],
            })
        })
        .collect::<Vec<_>>();
    let mut span = json!({
        "traceId": trace_id,
        "spanId": span_id_hex(&action.action_id),
        "name": action.title,
        "kind": "SPAN_KIND_INTERNAL",
        "startTimeUnixNano": action.start_time.to_string(),
        "endTimeUnixNano": end_time.to_string(),
        "attributes": attributes,
        "events": events,
        "links": span_links,
        "status": { "code": status_code(action.status) },
    });
    if let Some(parent) = parent {
        span["parentSpanId"] = json!(span_id_hex(parent));
    }
    span
}

fn string_attr(key: &str, value: &str) -> Value {
    json!({"key": key, "value": {"stringValue": value}})
}

fn int_attr(key: &str, value: u64) -> Value {
    json!({"key": key, "value": {"intValue": value.to_string()}})
}

fn bool_attr(key: &str, value: bool) -> Value {
    json!({"key": key, "value": {"boolValue": value}})
}
