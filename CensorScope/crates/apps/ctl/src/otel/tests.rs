use prost::Message;
    use super::*;

    fn sample_trace() -> TraceExport {
        TraceExport {
            trace_id: 7,
            display_name: "demo".to_string(),
            profile_name: "default".to_string(),
            lifecycle_state: "active".to_string(),
            health: "clean".to_string(),
            created_at: 1,
            started_at: Some(2),
            audit: AuditSummary::default(),
            actions: vec![ActionExport {
                action_id: "action-1".to_string(),
                kind: "command.invocation".to_string(),
                title: "command".to_string(),
                start_time: 10,
                end_time: None,
                process_id: 11,
                status: 1,
                completeness: 1,
                confidence_millis: None,
                session_id: None,
                action_valid: true,
                agent_observed: false,
                process_parent_conflict: false,
                association_state: None,
                attributes: BTreeMap::new(),
                evidence: vec![],
            }],
            links: vec![],
        }
    }

    #[test]
    fn active_snapshot_json_is_valid_and_marks_open_action() {
        let mut output = Vec::new();
        write_json(&mut output, &sample_trace()).unwrap();
        let document: Value = serde_json::from_slice(&output).unwrap();
        let span = &document["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert_eq!(span["spanId"], span_id_hex("action-1"));
        assert!(
            span["attributes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|attribute| attribute["key"] == "censorscope.action.open")
        );
        assert_eq!(span["status"]["code"], "STATUS_CODE_UNSET");
    }

    #[test]
    fn json_includes_audit_summary_and_action_state() {
        let mut trace = sample_trace();
        trace.audit = AuditSummary {
            diagnostic_count: 2,
            diagnostic_warning_count: 1,
            dropped: 3,
            orphan_action_count: 1,
            ..AuditSummary::default()
        };
        trace.actions[0].association_state = Some("orphan".to_string());
        let mut output = Vec::new();
        write_json(&mut output, &trace).unwrap();
        let document: Value = serde_json::from_slice(&output).unwrap();
        let resource = &document["resourceSpans"][0]["resource"]["attributes"];
        assert!(resource.as_array().unwrap().iter().any(|attribute| {
            attribute["key"] == "censorscope.audit.diagnostic_count"
                && attribute["value"]["intValue"] == "2"
        }));
        let attributes = &document["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"];
        assert!(attributes.as_array().unwrap().iter().any(|attribute| {
            attribute["key"] == "censorscope.action.audit_state"
                && attribute["value"]["stringValue"] == "orphan"
        }));
    }

    #[test]
    fn protobuf_contains_one_resource_and_span() {
        let mut output = Vec::new();
        write_protobuf(&mut output, &sample_trace()).unwrap();
        let request = proto::ExportTraceServiceRequest::decode(output.as_slice()).unwrap();
        assert_eq!(request.resource_spans.len(), 1);
        assert_eq!(request.resource_spans[0].scope_spans[0].spans.len(), 1);
        assert_eq!(
            request.resource_spans[0].scope_spans[0].spans[0].trace_id,
            trace_id_bytes(7)
        );
    }

    #[test]
    fn load_trace_supports_active_and_draining_without_invalid_parents() {
        for lifecycle_state in ["active", "draining"] {
            let connection = Connection::open_in_memory().unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE traces(
                        trace_id INTEGER, display_name TEXT, profile_name TEXT,
                        lifecycle_state TEXT, health TEXT, created_at INTEGER, started_at INTEGER
                     );
                     CREATE TABLE semantic_actions_read(
                        trace_id INTEGER, action_id TEXT, kind_name TEXT, title TEXT, start_time INTEGER,
                        end_time INTEGER, process_id INTEGER, status INTEGER, completeness INTEGER,
                        confidence_millis INTEGER, session_id TEXT, action_valid_code INTEGER,
                        agent_observed INTEGER, process_parent_conflict INTEGER
                     );
                     CREATE TABLE semantic_action_attributes(
                        trace_id INTEGER, action_id TEXT, attr_key TEXT, attr_value TEXT
                     );
                     CREATE TABLE semantic_action_evidence_read(
                        trace_id INTEGER, action_id TEXT, evidence_kind INTEGER, evidence_id INTEGER, evidence_role TEXT
                     );
                     CREATE TABLE semantic_action_links_read(
                        trace_id INTEGER, source_action_id TEXT, target_action_id TEXT, role_name TEXT,
                        confidence INTEGER, valid INTEGER, link_valid_code INTEGER, role INTEGER
                     );
                     CREATE TABLE diagnostics(
                        severity INTEGER, dropped INTEGER, dropped_bytes INTEGER, trace_id INTEGER
                     );
                     CREATE TABLE events(trace_id INTEGER, protocol TEXT, metadata TEXT);",
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO traces VALUES (7, 'demo', 'default', ?1, 'clean', 1, 2)",
                    [lifecycle_state],
                )
                .unwrap();
            for action_id in ["parent", "child"] {
                connection
                    .execute(
                        "INSERT INTO semantic_actions_read VALUES
                         (7, ?1, 'command.invocation', ?1, 10, NULL, 11, 2, 1, NULL,
                          NULL, 1, 0, 0)",
                        [action_id],
                    )
                    .unwrap();
            }
            connection
                .execute(
                    "INSERT INTO semantic_action_links_read VALUES
                     (7, 'parent', 'child', 'agent.performed_action', 900, 1, 0, 1)",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO semantic_action_attributes VALUES
                     (7, 'child', 'llm.association_state', 'orphan')",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO events VALUES (7, 'tls.coverage', '{\"state\":\"gap\"}')",
                    [],
                )
                .unwrap();

            let trace = load_trace(&connection, 7, OtelAttributeMode::Metadata).unwrap();
            assert_eq!(trace.lifecycle_state, lifecycle_state);
            assert_eq!(trace.actions.len(), 2);
            assert_eq!(trace.audit.open_action_count, 2);
            assert_eq!(trace.audit.orphan_action_count, 1);
            assert_eq!(trace.audit.invalid_link_count, 1);
            assert_eq!(trace.audit.coverage_event_count, 1);
            assert_eq!(trace.audit.coverage_gap_count, 1);
            assert_eq!(trace.audit.tls_coverage_gap_count, 1);
            assert!(parent_map(&trace).is_empty());
        }
    }

    fn large_trace_database(action_count: usize) -> (std::path::PathBuf, Connection) {
        let path = std::env::temp_dir().join(format!(
            ".censorscope-otel-test-{}-{}.sqlite",
            std::process::id(),
            now_nanos()
        ));
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE traces(
                    trace_id INTEGER, display_name TEXT, profile_name TEXT,
                    lifecycle_state TEXT, health TEXT, created_at INTEGER, started_at INTEGER
                 );
                 CREATE TABLE semantic_actions_read(
                    trace_id INTEGER, action_id TEXT, kind_name TEXT, title TEXT, start_time INTEGER,
                    end_time INTEGER, process_id INTEGER, status INTEGER, completeness INTEGER,
                    confidence_millis INTEGER, session_id TEXT, action_valid_code INTEGER,
                    agent_observed INTEGER, process_parent_conflict INTEGER
                 );
                 CREATE TABLE semantic_action_attributes(
                    trace_id INTEGER, action_id TEXT, attr_key TEXT, attr_value TEXT
                 );
                 CREATE TABLE semantic_action_evidence_read(
                    trace_id INTEGER, action_id TEXT, evidence_kind INTEGER, evidence_id INTEGER,
                    evidence_role TEXT
                 );
                 CREATE TABLE semantic_action_links_read(
                    trace_id INTEGER, source_action_id TEXT, target_action_id TEXT, role_name TEXT,
                    confidence INTEGER, valid INTEGER, link_valid_code INTEGER
                 );
                 CREATE TABLE diagnostics(
                    severity INTEGER, dropped INTEGER, dropped_bytes INTEGER, trace_id INTEGER
                 );
                 CREATE TABLE events(trace_id INTEGER, protocol TEXT, metadata TEXT);
                 INSERT INTO traces VALUES (7, 'large', 'default', 'draining', 'clean', 1, 2);",
            )
            .unwrap();
        let transaction = connection.unchecked_transaction().unwrap();
        {
            let mut actions = transaction
                .prepare(
                    "INSERT INTO semantic_actions_read VALUES
                     (7, ?1, 'command.invocation', ?1, ?2, ?2, 11, 2, 1, NULL,
                      NULL, 1, 0, 0)",
                )
                .unwrap();
            let mut attributes = transaction
                .prepare(
                    "INSERT INTO semantic_action_attributes VALUES
                     (7, ?1, 'test.attribute', ?2)",
                )
                .unwrap();
            for index in 0..action_count {
                let action_id = format!("action-{index}");
                actions
                    .execute(rusqlite::params![action_id, index as i64 + 10])
                    .unwrap();
                attributes
                    .execute(rusqlite::params![action_id, "value"])
                    .unwrap();
            }
        }
        transaction.commit().unwrap();
        (path, connection)
    }

    #[test]
    fn coverage_audit_decodes_stored_metadata_in_json_and_protobuf_exports() {
        let (path, connection) = large_trace_database(1);
        connection
            .execute_batch(
                "ALTER TABLE semantic_action_links_read ADD COLUMN role INTEGER DEFAULT 1",
            )
            .unwrap();
        for (trace, protocol, metadata) in [
            (
                7,
                "tls.coverage",
                r#"{"capability":"tls-plaintext","state":"covered"}"#,
            ),
            (
                7,
                "tls.coverage",
                r#"{"message":"library unavailable","state":"gap"}"#,
            ),
            (7, "http.coverage", r#"{ "state": "covered" }"#),
            (7, "tls.coverage", r#"{"state":"gap"}"#),
            (
                7,
                "tls.coverage",
                r#"{"message":"state=covered","state":"unknown"}"#,
            ),
            (8, "tls.coverage", r#"{"state":"gap"}"#),
            (7, "http", r#"{"state":"gap"}"#),
        ] {
            connection
                .execute(
                    "INSERT INTO events VALUES (?1, ?2, ?3)",
                    rusqlite::params![trace, protocol, metadata],
                )
                .unwrap();
        }
        let trace = load_trace(&connection, 7, OtelAttributeMode::Metadata).unwrap();
        assert_eq!(trace.audit.coverage_event_count, 5);
        assert_eq!(trace.audit.coverage_gap_count, 3);
        let header = load_trace_header(&connection, 7).unwrap();
        assert_eq!(header.audit.coverage_event_count, 5);
        assert_eq!(header.audit.coverage_gap_count, 3);
        let details = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut output = Vec::new();
        write_json_streaming(
            &mut output,
            &connection,
            &details,
            7,
            OtelAttributeMode::Metadata,
        )
        .unwrap();
        let document: Value = serde_json::from_slice(&output).unwrap();
        let attributes = document["resourceSpans"][0]["resource"]["attributes"]
            .as_array()
            .unwrap();
        assert!(attributes.iter().any(|attribute| attribute["key"]
            == "censorscope.audit.coverage_event_count"
            && attribute["value"]["intValue"] == "5"));
        assert!(attributes.iter().any(|attribute| attribute["key"]
            == "censorscope.audit.coverage_gap_count"
            && attribute["value"]["intValue"] == "3"));
        output.clear();
        write_protobuf_streaming(
            &mut output,
            &connection,
            &details,
            7,
            OtelAttributeMode::Metadata,
        )
        .unwrap();
        let request = proto::ExportTraceServiceRequest::decode(output.as_slice()).unwrap();
        let attributes = &request.resource_spans[0]
            .resource
            .as_ref()
            .unwrap()
            .attributes;
        let count = attributes
            .iter()
            .find(|attribute| attribute.key == "censorscope.audit.coverage_gap_count")
            .unwrap();
        assert_eq!(
            count.value.as_ref().unwrap().value,
            Some(proto::any_value::Value::IntValue(3))
        );
        drop(details);
        drop(connection);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn streaming_export_handles_large_trace_without_building_trace_vector() {
        let (path, connection) = large_trace_database(2_000);
        let details = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();

        let mut json_output = Vec::new();
        write_json_streaming(
            &mut json_output,
            &connection,
            &details,
            7,
            OtelAttributeMode::Metadata,
        )
        .unwrap();
        let document: Value = serde_json::from_slice(&json_output).unwrap();
        assert_eq!(
            document["resourceSpans"][0]["scopeSpans"][0]["spans"]
                .as_array()
                .unwrap()
                .len(),
            2_000
        );

        let mut protobuf_output = Vec::new();
        write_protobuf_streaming(
            &mut protobuf_output,
            &connection,
            &details,
            7,
            OtelAttributeMode::Full,
        )
        .unwrap();
        let request = proto::ExportTraceServiceRequest::decode(protobuf_output.as_slice()).unwrap();
        assert_eq!(request.resource_spans[0].scope_spans[0].spans.len(), 2_000);
        drop(details);
        drop(connection);
        fs::remove_file(path).unwrap();
    }
