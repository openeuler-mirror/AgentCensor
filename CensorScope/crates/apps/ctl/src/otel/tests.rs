use super::*;

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
