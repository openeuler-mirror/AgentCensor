use agentcensor_events::{
    AckFrame, EventFrame, EventSource, EventSubject, EventTarget, ExternalEvent, HelloFrame,
    PROTOCOL_VERSION, SCHEMA_VERSION, SecurityCategory, SecurityDecision, SecurityEvent,
    read_frame, write_frame,
};
use censorpivot::events::{EventBridge, EventRegistry};
use censorpivot::{BatchRequest, ToolCall};
use std::collections::BTreeMap;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[test]
fn uncorrelated_events_do_not_block_correlated_delivery() -> Result<(), Box<dyn std::error::Error>>
{
    let temporary = tempfile::tempdir()?;
    let guard_socket = temporary.path().join("guard.sock");
    let scope_socket = temporary.path().join("scope.sock");
    let scope = UnixListener::bind(&scope_socket)?;
    let registry = Arc::new(EventRegistry::default());
    let call = ToolCall {
        id: "call-1".into(),
        program: "cat".into(),
        args: vec!["/etc/shadow".into()],
        cwd: String::new(),
        env: BTreeMap::new(),
        timeout_ms: 1000,
    };
    let request = BatchRequest {
        request_id: "request-1".into(),
        session_id: "session-1".into(),
        trace_id: Some(17),
        branch: "main".into(),
        expected_generation: "generation".into(),
        expected_head_seq: 1,
        guard_scope: "pivot-transaction-1".into(),
        guard_group: "group-1".into(),
        calls: vec![call.clone()],
    };
    registry.register_runner(&request, std::process::id());
    registry.call_start(&request.guard_scope, &call);
    let _bridge = EventBridge::start(&guard_socket, 0o600, scope_socket, registry)?;
    let mut guard = UnixStream::connect(&guard_socket)?;
    guard.set_read_timeout(Some(Duration::from_secs(3)))?;
    write_frame(
        &mut guard,
        &EventFrame::Hello(HelloFrame {
            protocol_version: PROTOCOL_VERSION,
            producer: "censorguard".into(),
            instance_id: "boot-1".into(),
        }),
    )?;
    let mut event = ExternalEvent {
        schema_version: SCHEMA_VERSION,
        source: EventSource::CensorGuard,
        source_instance_id: "boot-1".into(),
        source_sequence: 1,
        occurred_at_ns: u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos())?,
        subject: EventSubject {
            pid: std::process::id(),
            tgid: std::process::id(),
            comm: None,
            domain_id: Some(1),
            domain: Some("unrelated-domain".into()),
        },
        event: SecurityEvent {
            category: SecurityCategory::File,
            operation: "file.open_read".into(),
            decision: SecurityDecision::Deny,
            target: EventTarget {
                path: Some("/etc/shadow".into()),
                ..Default::default()
            },
            policy: None,
        },
        correlation: None,
        attributes: BTreeMap::new(),
    };
    write_frame(&mut guard, &EventFrame::Event(event.clone()))?;
    assert_eq!(
        read_frame(&mut guard)?,
        Some(EventFrame::Ack(AckFrame {
            source_instance_id: "boot-1".into(),
            source_sequence: 1,
        }))
    );
    event.subject.domain = Some(request.guard_scope);
    event.source_sequence = 2;
    write_frame(&mut guard, &EventFrame::Event(event))?;
    scope.set_nonblocking(true)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut sink = loop {
        match scope.accept() {
            Ok((stream, _)) => break stream,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(error) => return Err(error.into()),
        }
    };
    sink.set_read_timeout(Some(Duration::from_secs(3)))?;
    assert!(matches!(read_frame(&mut sink)?, Some(EventFrame::Hello(_))));
    let Some(EventFrame::Event(delivered)) = read_frame(&mut sink)? else {
        return Err("Scope did not receive the event".into());
    };
    let correlation = delivered.correlation.ok_or("missing correlation")?;
    assert_eq!(correlation.trace_id, Some(17));
    assert_eq!(correlation.transaction_id.as_deref(), Some("transaction-1"));
    assert_eq!(correlation.session_id.as_deref(), Some("session-1"));
    assert_eq!(correlation.call_id.as_deref(), Some("call-1"));
    assert_eq!(delivered.event.decision, SecurityDecision::Deny);
    assert_eq!(delivered.event.target.path.as_deref(), Some("/etc/shadow"));
    write_frame(
        &mut sink,
        &EventFrame::Ack(AckFrame {
            source_instance_id: delivered.source_instance_id,
            source_sequence: delivered.source_sequence,
        }),
    )?;
    assert!(matches!(
        read_frame(&mut guard)?,
        Some(EventFrame::Ack(AckFrame {
            source_sequence: 2,
            ..
        }))
    ));
    Ok(())
}
