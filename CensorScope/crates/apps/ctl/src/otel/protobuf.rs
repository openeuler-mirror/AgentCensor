use super::*;
use prost::Message;

#[cfg(test)]
pub(super) fn write_protobuf<W: Write>(writer: &mut W, trace: &TraceExport) -> Result<(), std::io::Error> {
    let snapshot_time = now_nanos();
    let parent_map = parent_map(trace);
    let mut links_by_target: HashMap<&str, Vec<&LinkExport>> = HashMap::new();
    for link in &trace.links {
        links_by_target
            .entry(link.target_action_id.as_str())
            .or_default()
            .push(link);
    }
    let spans = trace
        .actions
        .iter()
        .map(|action| {
            proto_span(
                trace,
                action,
                snapshot_time,
                parent_map.get(action.action_id.as_str()).copied(),
                links_by_target.get(action.action_id.as_str()),
            )
        })
        .collect();
    let request = proto::ExportTraceServiceRequest {
        resource_spans: vec![proto::ResourceSpans {
            resource: Some(proto::Resource {
                attributes: {
                    let mut attributes = vec![
                        proto_string_attr("service.name", "censorscope"),
                        proto_string_attr("censorscope.trace.id", &trace.trace_id.to_string()),
                        proto_string_attr("censorscope.trace.id_format", "u64-zero-extended-v1"),
                        proto_string_attr("censorscope.trace.display_name", &trace.display_name),
                        proto_string_attr("censorscope.trace.profile_name", &trace.profile_name),
                        proto_string_attr(
                            "censorscope.trace.lifecycle_state",
                            &trace.lifecycle_state,
                        ),
                        proto_string_attr("censorscope.trace.health", &trace.health),
                        proto_int_attr("censorscope.trace.created_at_unix_nano", trace.created_at),
                        proto_int_attr(
                            "censorscope.audit.diagnostic_count",
                            trace.audit.diagnostic_count,
                        ),
                        proto_int_attr(
                            "censorscope.audit.diagnostic_warning_count",
                            trace.audit.diagnostic_warning_count,
                        ),
                        proto_int_attr(
                            "censorscope.audit.diagnostic_error_count",
                            trace.audit.diagnostic_error_count,
                        ),
                        proto_int_attr("censorscope.audit.dropped", trace.audit.dropped),
                        proto_int_attr(
                            "censorscope.audit.dropped_bytes",
                            trace.audit.dropped_bytes,
                        ),
                        proto_int_attr(
                            "censorscope.audit.open_action_count",
                            trace.audit.open_action_count,
                        ),
                        proto_int_attr(
                            "censorscope.audit.partial_action_count",
                            trace.audit.partial_action_count,
                        ),
                        proto_int_attr(
                            "censorscope.audit.orphan_action_count",
                            trace.audit.orphan_action_count,
                        ),
                        proto_int_attr(
                            "censorscope.audit.ambiguous_action_count",
                            trace.audit.ambiguous_action_count,
                        ),
                        proto_int_attr(
                            "censorscope.audit.invalid_link_count",
                            trace.audit.invalid_link_count,
                        ),
                        proto_int_attr(
                            "censorscope.audit.coverage_event_count",
                            trace.audit.coverage_event_count,
                        ),
                        proto_int_attr(
                            "censorscope.audit.coverage_gap_count",
                            trace.audit.coverage_gap_count,
                        ),
                        proto_int_attr(
                            "censorscope.audit.tls_coverage_event_count",
                            trace.audit.tls_coverage_event_count,
                        ),
                        proto_int_attr(
                            "censorscope.audit.tls_coverage_gap_count",
                            trace.audit.tls_coverage_gap_count,
                        ),
                        proto_string_attr("censorscope.export.schema_version", OTEL_SCHEMA_VERSION),
                        proto_bool_attr("censorscope.export.snapshot", true),
                        proto_int_attr("censorscope.export.snapshot_time_unix_nano", snapshot_time),
                    ];
                    if let Some(started_at) = trace.started_at {
                        attributes.push(proto_int_attr(
                            "censorscope.trace.started_at_unix_nano",
                            started_at,
                        ));
                    }
                    attributes
                },
                ..Default::default()
            }),
            scope_spans: vec![proto::ScopeSpans {
                scope: Some(proto::InstrumentationScope {
                    name: OTEL_SCOPE.to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                }),
                spans,
                ..Default::default()
            }],
            ..Default::default()
        }],
    };
    let mut bytes = Vec::new();
    request.encode(&mut bytes).map_err(std::io::Error::other)?;
    writer.write_all(&bytes)
}

struct TempSpool {
    path: std::path::PathBuf,
    file: fs::File,
}

impl Drop for TempSpool {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn create_spool(label: &str) -> Result<TempSpool, std::io::Error> {
    let directory = std::env::temp_dir();
    let nonce = now_nanos();
    for attempt in 0..16u32 {
        let path = directory.join(format!(
            ".censorscope-otel-{label}-{}-{nonce}-{attempt}",
            std::process::id(),
        ));
        match fs::OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok(TempSpool { path, file }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "unable to allocate OTLP spool file",
    ))
}

fn write_varint<W: Write>(writer: &mut W, mut value: u64) -> Result<(), std::io::Error> {
    while value >= 0x80 {
        writer.write_all(&[(value as u8) | 0x80])?;
        value >>= 7;
    }
    writer.write_all(&[value as u8])
}

fn write_proto_bytes_field<W: Write>(
    writer: &mut W,
    field: u8,
    bytes: &[u8],
) -> Result<(), std::io::Error> {
    write_varint(writer, u64::from(field) << 3 | 2)?;
    write_varint(writer, bytes.len() as u64)?;
    writer.write_all(bytes)
}

fn copy_spool_field<W: Write>(
    writer: &mut W,
    field: u8,
    spool: &TempSpool,
) -> Result<(), std::io::Error> {
    let length = spool.file.metadata()?.len();
    write_varint(writer, u64::from(field) << 3 | 2)?;
    write_varint(writer, length)?;
    let mut input = fs::File::open(&spool.path)?;
    std::io::copy(&mut input, writer)?;
    Ok(())
}

fn proto_resource_attributes(trace: &TraceExport, snapshot_time: u64) -> Vec<proto::KeyValue> {
    let mut attributes = vec![
        proto_string_attr("service.name", "censorscope"),
        proto_string_attr("censorscope.trace.id", &trace.trace_id.to_string()),
        proto_string_attr("censorscope.trace.id_format", "u64-zero-extended-v1"),
        proto_string_attr("censorscope.trace.display_name", &trace.display_name),
        proto_string_attr("censorscope.trace.profile_name", &trace.profile_name),
        proto_string_attr("censorscope.trace.lifecycle_state", &trace.lifecycle_state),
        proto_string_attr("censorscope.trace.health", &trace.health),
        proto_int_attr("censorscope.trace.created_at_unix_nano", trace.created_at),
        proto_int_attr(
            "censorscope.audit.diagnostic_count",
            trace.audit.diagnostic_count,
        ),
        proto_int_attr(
            "censorscope.audit.diagnostic_warning_count",
            trace.audit.diagnostic_warning_count,
        ),
        proto_int_attr(
            "censorscope.audit.diagnostic_error_count",
            trace.audit.diagnostic_error_count,
        ),
        proto_int_attr("censorscope.audit.dropped", trace.audit.dropped),
        proto_int_attr("censorscope.audit.dropped_bytes", trace.audit.dropped_bytes),
        proto_int_attr(
            "censorscope.audit.open_action_count",
            trace.audit.open_action_count,
        ),
        proto_int_attr(
            "censorscope.audit.partial_action_count",
            trace.audit.partial_action_count,
        ),
        proto_int_attr(
            "censorscope.audit.orphan_action_count",
            trace.audit.orphan_action_count,
        ),
        proto_int_attr(
            "censorscope.audit.ambiguous_action_count",
            trace.audit.ambiguous_action_count,
        ),
        proto_int_attr(
            "censorscope.audit.invalid_link_count",
            trace.audit.invalid_link_count,
        ),
        proto_int_attr(
            "censorscope.audit.coverage_event_count",
            trace.audit.coverage_event_count,
        ),
        proto_int_attr(
            "censorscope.audit.coverage_gap_count",
            trace.audit.coverage_gap_count,
        ),
        proto_int_attr(
            "censorscope.audit.tls_coverage_event_count",
            trace.audit.tls_coverage_event_count,
        ),
        proto_int_attr(
            "censorscope.audit.tls_coverage_gap_count",
            trace.audit.tls_coverage_gap_count,
        ),
        proto_string_attr("censorscope.export.schema_version", OTEL_SCHEMA_VERSION),
        proto_bool_attr("censorscope.export.snapshot", true),
        proto_int_attr("censorscope.export.snapshot_time_unix_nano", snapshot_time),
    ];
    if let Some(started_at) = trace.started_at {
        attributes.push(proto_int_attr(
            "censorscope.trace.started_at_unix_nano",
            started_at,
        ));
    }
    attributes
}

pub(super) fn write_protobuf_streaming<W: Write>(
    writer: &mut W,
    connection: &Connection,
    details: &Connection,
    trace_id: u64,
    attributes_mode: OtelAttributeMode,
) -> Result<(), Box<dyn std::error::Error>> {
    let header = load_trace_header(connection, trace_id)?;
    let snapshot_time = now_nanos();
    let resource = proto::Resource {
        attributes: proto_resource_attributes(&header, snapshot_time),
    };
    let mut resource_bytes = Vec::new();
    resource.encode(&mut resource_bytes)?;

    let mut scope_spool = create_spool("scope")?;
    let scope = proto::InstrumentationScope {
        name: OTEL_SCOPE.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let mut scope_bytes = Vec::new();
    scope.encode(&mut scope_bytes)?;
    write_proto_bytes_field(&mut scope_spool.file, 1, &scope_bytes)?;
    stream_actions(
        connection,
        details,
        &header,
        attributes_mode,
        |_, action, links| {
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
            let span = proto_span(&header, action, snapshot_time, parent, Some(&link_refs));
            let mut span_bytes = Vec::new();
            span.encode(&mut span_bytes)?;
            write_proto_bytes_field(&mut scope_spool.file, 2, &span_bytes)?;
            Ok(())
        },
    )?;
    scope_spool.file.flush()?;

    let mut resource_spool = create_spool("resource")?;
    write_proto_bytes_field(&mut resource_spool.file, 1, &resource_bytes)?;
    copy_spool_field(&mut resource_spool.file, 2, &scope_spool)?;
    resource_spool.file.flush()?;

    copy_spool_field(writer, 1, &resource_spool)?;
    Ok(())
}

fn proto_span(
    trace: &TraceExport,
    action: &ActionExport,
    snapshot_time: u64,
    parent: Option<&str>,
    links: Option<&Vec<&LinkExport>>,
) -> proto::Span {
    let mut attributes = vec![
        proto_string_attr("censorscope.action.id", &action.action_id),
        proto_string_attr("censorscope.action.kind", &action.kind),
        proto_string_attr("censorscope.action.status", status_name(action.status)),
        proto_string_attr(
            "censorscope.action.completeness",
            completeness_name(action.completeness),
        ),
        proto_int_attr("censorscope.process.id", action.process_id),
        proto_bool_attr(ACTION_VALID_ATTRIBUTE, action.action_valid),
        proto_bool_attr("censorscope.agent.observed", action.agent_observed),
        proto_bool_attr(
            "censorscope.process.parent_conflict",
            action.process_parent_conflict,
        ),
    ];
    if let Some(confidence) = action.confidence_millis {
        attributes.push(proto_int_attr(
            "censorscope.action.confidence_millis",
            u64::from(confidence),
        ));
    }
    if let Some(session_id) = &action.session_id {
        attributes.push(proto_string_attr("censorscope.session.id", session_id));
    }
    if action.end_time.is_none() {
        attributes.push(proto_bool_attr("censorscope.action.open", true));
    }
    attributes.push(proto_int_attr(
        "censorscope.action.evidence_count",
        action.evidence.len() as u64,
    ));
    attributes.push(proto_string_attr(
        "censorscope.action.audit_state",
        action_audit_state(action),
    ));
    attributes.extend(
        action
            .attributes
            .iter()
            .map(|(key, value)| proto_string_attr(key, value)),
    );
    let events = action
        .evidence
        .iter()
        .map(|evidence| proto::SpanEvent {
            time_unix_nano: action.start_time,
            name: "censorscope.evidence".to_string(),
            attributes: vec![
                proto_int_attr("censorscope.evidence.kind", evidence.kind as u64),
                proto_int_attr("censorscope.evidence.id", evidence.id),
                proto_string_attr("censorscope.evidence.role", &evidence.role),
            ],
            ..Default::default()
        })
        .collect();
    let span_links = links
        .into_iter()
        .flatten()
        .map(|link| proto::SpanLink {
            trace_id: trace_id_bytes(trace.trace_id),
            span_id: stable_hash(link.source_action_id.as_bytes())
                .to_be_bytes()
                .to_vec(),
            attributes: vec![
                proto_string_attr("censorscope.link.role", &link.role),
                proto_int_attr("censorscope.link.confidence", link.confidence as u64),
                proto_bool_attr("censorscope.link.valid", link.valid),
                proto_bool_attr("censorscope.link.validity", link.link_valid),
            ],
            ..Default::default()
        })
        .collect();
    proto::Span {
        trace_id: trace_id_bytes(trace.trace_id),
        span_id: stable_hash(action.action_id.as_bytes())
            .to_be_bytes()
            .to_vec(),
        parent_span_id: parent
            .map(|value| stable_hash(value.as_bytes()).to_be_bytes().to_vec())
            .unwrap_or_default(),
        name: action.title.clone(),
        kind: 1,
        start_time_unix_nano: action.start_time,
        end_time_unix_nano: action.end_time.unwrap_or(snapshot_time),
        attributes,
        events,
        links: span_links,
        status: Some(proto::Status {
            code: match action.status {
                2 => 1,
                3 => 2,
                _ => 0,
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub(super) fn trace_id_bytes(trace_id: u64) -> Vec<u8> {
    u128::from(trace_id).to_be_bytes().to_vec()
}

fn proto_string_attr(key: &str, value: &str) -> proto::KeyValue {
    proto::KeyValue {
        key: key.to_string(),
        value: Some(proto::AnyValue {
            value: Some(proto::any_value::Value::StringValue(value.to_string())),
        }),
    }
}

fn proto_int_attr(key: &str, value: u64) -> proto::KeyValue {
    proto::KeyValue {
        key: key.to_string(),
        value: Some(proto::AnyValue {
            value: Some(proto::any_value::Value::IntValue(value as i64)),
        }),
    }
}

fn proto_bool_attr(key: &str, value: bool) -> proto::KeyValue {
    proto::KeyValue {
        key: key.to_string(),
        value: Some(proto::AnyValue {
            value: Some(proto::any_value::Value::BoolValue(value)),
        }),
    }
}

pub(super) mod proto {
    use prost::Message;

    #[cfg(test)]
    #[derive(Clone, PartialEq, Message)]
    pub struct ExportTraceServiceRequest {
        #[prost(message, repeated, tag = "1")]
        pub resource_spans: Vec<ResourceSpans>,
    }
    #[cfg(test)]
    #[derive(Clone, PartialEq, Message)]
    pub struct ResourceSpans {
        #[prost(message, optional, tag = "1")]
        pub resource: Option<Resource>,
        #[prost(message, repeated, tag = "2")]
        pub scope_spans: Vec<ScopeSpans>,
    }
    #[derive(Clone, PartialEq, Message)]
    pub struct Resource {
        #[prost(message, repeated, tag = "1")]
        pub attributes: Vec<KeyValue>,
    }
    #[cfg(test)]
    #[derive(Clone, PartialEq, Message)]
    pub struct ScopeSpans {
        #[prost(message, optional, tag = "1")]
        pub scope: Option<InstrumentationScope>,
        #[prost(message, repeated, tag = "2")]
        pub spans: Vec<Span>,
    }
    #[derive(Clone, PartialEq, Message)]
    pub struct InstrumentationScope {
        #[prost(string, tag = "1")]
        pub name: String,
        #[prost(string, tag = "2")]
        pub version: String,
    }
    #[derive(Clone, PartialEq, Message)]
    pub struct Span {
        #[prost(bytes = "vec", tag = "1")]
        pub trace_id: Vec<u8>,
        #[prost(bytes = "vec", tag = "2")]
        pub span_id: Vec<u8>,
        #[prost(bytes = "vec", tag = "4")]
        pub parent_span_id: Vec<u8>,
        #[prost(uint32, tag = "5")]
        pub flags: u32,
        #[prost(string, tag = "6")]
        pub name: String,
        #[prost(enumeration = "span::SpanKind", tag = "7")]
        pub kind: i32,
        #[prost(uint64, tag = "8")]
        pub start_time_unix_nano: u64,
        #[prost(uint64, tag = "9")]
        pub end_time_unix_nano: u64,
        #[prost(message, repeated, tag = "10")]
        pub attributes: Vec<KeyValue>,
        #[prost(message, repeated, tag = "12")]
        pub events: Vec<SpanEvent>,
        #[prost(message, repeated, tag = "13")]
        pub links: Vec<SpanLink>,
        #[prost(message, optional, tag = "15")]
        pub status: Option<Status>,
    }
    pub mod span {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
        pub enum SpanKind {
            Unspecified = 0,
            Internal = 1,
            Server = 2,
            Client = 3,
            Producer = 4,
            Consumer = 5,
        }
    }
    #[derive(Clone, PartialEq, Message)]
    pub struct SpanEvent {
        #[prost(uint64, tag = "1")]
        pub time_unix_nano: u64,
        #[prost(string, tag = "2")]
        pub name: String,
        #[prost(message, repeated, tag = "3")]
        pub attributes: Vec<KeyValue>,
    }
    #[derive(Clone, PartialEq, Message)]
    pub struct SpanLink {
        #[prost(bytes = "vec", tag = "1")]
        pub trace_id: Vec<u8>,
        #[prost(bytes = "vec", tag = "2")]
        pub span_id: Vec<u8>,
        #[prost(message, repeated, tag = "4")]
        pub attributes: Vec<KeyValue>,
    }
    #[derive(Clone, PartialEq, Message)]
    pub struct Status {
        #[prost(string, tag = "2")]
        pub message: String,
        #[prost(enumeration = "status::StatusCode", tag = "3")]
        pub code: i32,
    }
    pub mod status {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
        pub enum StatusCode {
            Unset = 0,
            Ok = 1,
            Error = 2,
        }
    }
    #[derive(Clone, PartialEq, Message)]
    pub struct KeyValue {
        #[prost(string, tag = "1")]
        pub key: String,
        #[prost(message, optional, tag = "2")]
        pub value: Option<AnyValue>,
    }
    #[derive(Clone, PartialEq, Message)]
    pub struct AnyValue {
        #[prost(oneof = "any_value::Value", tags = "1, 2, 3")]
        pub value: Option<any_value::Value>,
    }
    pub mod any_value {
        #[derive(Clone, PartialEq, prost::Oneof)]
        pub enum Value {
            #[prost(string, tag = "1")]
            StringValue(String),
            #[prost(bool, tag = "2")]
            BoolValue(bool),
            #[prost(int64, tag = "3")]
            IntValue(i64),
        }
    }
}
