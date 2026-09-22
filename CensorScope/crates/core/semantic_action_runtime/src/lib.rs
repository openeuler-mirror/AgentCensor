//! Built-in, observation-only SemanticAction projection.

mod http;
mod llm;
mod mcp;
mod sse;

use std::collections::BTreeMap;

use model_core::event::{DomainEvent, EventFlags, EventPayload};
use semantic_action_contract::{
    SemanticAction, SemanticActionCompleteness, SemanticActionKind, SemanticActionLink,
    SemanticActionLinkConfidence, SemanticActionLinkRole, SemanticActionStatus, SemanticEvidence,
    SemanticEvidenceKind,
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProjectionBatch {
    pub actions: Vec<SemanticAction>,
    pub links: Vec<SemanticActionLink>,
}

pub use http::project_http1_payload;
pub use llm::project_llm_payload;
pub use mcp::project_mcp_payload;
pub use sse::project_sse_payload;

/// Project one normalized event into stable semantic actions and links.
/// Unknown application/resource/control events are intentionally left for
/// protocol-specific projectors and do not produce guessed content.
pub fn project_event(event: &DomainEvent) -> ProjectionBatch {
    let session = event.envelope.session_id.clone();
    let batch = if let EventPayload::Process(payload) = &event.payload {
        if payload.operation == "exec" {
            project_exec(event, payload)
        } else {
            project_event_inner(event)
        }
    } else {
        project_event_inner(event)
    };
    ProjectionBatch {
        actions: batch
            .actions
            .into_iter()
            .map(|mut action| {
                action.session_id = session.clone();
                action
            })
            .collect(),
        links: batch.links,
    }
}

fn project_event_inner(event: &DomainEvent) -> ProjectionBatch {
    if let EventPayload::Process(payload) = &event.payload {
        if payload.operation == "exec" {
            return project_exec(event, payload);
        }
    }
    let (kind, title, mut attributes) = match &event.payload {
        EventPayload::Process(payload) => {
            let kind = match payload.operation.as_str() {
                "exit" => SemanticActionKind::ProcessExit,
                "fork" => SemanticActionKind::ProcessForkAttempt,
                _ => return ProjectionBatch::default(),
            };
            let title = payload
                .executable
                .clone()
                .unwrap_or_else(|| payload.operation.clone());
            (kind, title, payload.metadata.clone())
        }
        EventPayload::File(payload) => {
            let kind = match payload.operation.as_str() {
                "read" | "readv" | "mmap" => SemanticActionKind::FileRead,
                "write" | "writev" => SemanticActionKind::FileWrite,
                "creat" | "truncate" | "ftruncate" | "mkdirat" | "rmdir" | "unlinkat"
                | "renameat" | "renameat2" | "rename" | "unlink" | "mkdir" => {
                    SemanticActionKind::FileModify
                }
                _ => return ProjectionBatch::default(),
            };
            let title = payload
                .path
                .clone()
                .unwrap_or_else(|| payload.operation.clone());
            (kind, title, payload.metadata.clone())
        }
        EventPayload::Stdio(payload) if payload.stream == "tty" => {
            let mut attributes = payload.metadata.clone();
            attributes.insert("stdio.stream".to_string(), payload.stream.clone());
            (
                SemanticActionKind::FileTtyIo,
                payload.stream.clone(),
                attributes,
            )
        }
        _ => return ProjectionBatch::default(),
    };
    attributes.insert(
        "censorscope.action.kind".to_string(),
        kind.as_str().to_string(),
    );
    let completeness = if event.envelope.flags == EventFlags::empty() {
        SemanticActionCompleteness::Complete
    } else {
        SemanticActionCompleteness::Partial
    };
    let status = if kind == SemanticActionKind::ProcessExit {
        match attributes
            .get("exit_code")
            .and_then(|value| value.parse::<i32>().ok())
        {
            Some(0) => SemanticActionStatus::Success,
            Some(_) => SemanticActionStatus::Error,
            None => SemanticActionStatus::Unknown,
        }
    } else if event.envelope.flags.contains(EventFlags::LOSS) {
        SemanticActionStatus::Unknown
    } else {
        SemanticActionStatus::Success
    };
    let action_id = format!("{}:{}", event.envelope.event_id.get(), kind.as_str());
    ProjectionBatch {
        actions: vec![SemanticAction {
            action_id,
            trace_id: event.envelope.trace_id,
            kind,
            title,
            start_time: event.envelope.observed_at,
            end_time: Some(event.envelope.observed_at),
            process: event.envelope.process,
            status,
            completeness,
            confidence_millis: (completeness == SemanticActionCompleteness::Complete)
                .then_some(1000),
            attributes,
            evidence: event_evidence(event),
            session_id: None,
        }],
        links: Vec::new(),
    }
}

fn project_exec(
    event: &DomainEvent,
    payload: &model_core::event::ProcessPayload,
) -> ProjectionBatch {
    let exec_completeness = completeness(event);
    let command_completeness = if exec_completeness == SemanticActionCompleteness::Complete
        && payload.argv.as_ref().is_some_and(|argv| argv.flags == 0)
    {
        SemanticActionCompleteness::Complete
    } else {
        SemanticActionCompleteness::Partial
    };
    let exec_id = action_id(event, SemanticActionKind::ProcessExec);
    let command_id = action_id(event, SemanticActionKind::CommandInvocation);
    let mut exec_attributes = payload.metadata.clone();
    exec_attributes.insert(
        "censorscope.action.kind".to_string(),
        "process.exec".to_string(),
    );
    let mut command_attributes = BTreeMap::new();
    command_attributes.insert(
        "censorscope.action.kind".to_string(),
        "command.invocation".to_string(),
    );
    if let Some(argv) = &payload.argv {
        command_attributes.insert(
            "command.argv.count".to_string(),
            argv.args.len().to_string(),
        );
        command_attributes.insert("command.argv.flags".to_string(), argv.flags.to_string());
        for (index, arg) in argv.args.iter().enumerate() {
            command_attributes.insert(format!("command.argv.{index}.hex"), hex(arg));
        }
    }
    let evidence = event_evidence(event);
    ProjectionBatch {
        actions: vec![
            SemanticAction {
                action_id: exec_id.clone(),
                trace_id: event.envelope.trace_id,
                kind: SemanticActionKind::ProcessExec,
                title: payload
                    .executable
                    .clone()
                    .unwrap_or_else(|| "exec".to_string()),
                start_time: event.envelope.observed_at,
                end_time: Some(event.envelope.observed_at),
                process: event.envelope.process,
                status: SemanticActionStatus::Success,
                completeness: exec_completeness,
                confidence_millis: (exec_completeness == SemanticActionCompleteness::Complete)
                    .then_some(1000),
                attributes: exec_attributes,
                evidence: evidence.clone(),
                session_id: None,
            },
            SemanticAction {
                action_id: command_id.clone(),
                trace_id: event.envelope.trace_id,
                kind: SemanticActionKind::CommandInvocation,
                title: payload
                    .executable
                    .clone()
                    .unwrap_or_else(|| "command".to_string()),
                start_time: event.envelope.observed_at,
                end_time: Some(event.envelope.observed_at),
                process: event.envelope.process,
                status: SemanticActionStatus::Success,
                completeness: command_completeness,
                confidence_millis: (command_completeness == SemanticActionCompleteness::Complete)
                    .then_some(1000),
                attributes: command_attributes,
                evidence,
                session_id: None,
            },
        ],
        links: vec![SemanticActionLink {
            trace_id: event.envelope.trace_id,
            source_action_id: command_id,
            target_action_id: exec_id,
            role: SemanticActionLinkRole::CommandContainsProcessExec,
            confidence: SemanticActionLinkConfidence::Observed,
            valid: true,
        }],
    }
}

fn completeness(event: &DomainEvent) -> SemanticActionCompleteness {
    if event.envelope.flags == EventFlags::empty() {
        SemanticActionCompleteness::Complete
    } else {
        SemanticActionCompleteness::Partial
    }
}

fn action_id(event: &DomainEvent, kind: SemanticActionKind) -> String {
    format!("{}:{}", event.envelope.event_id.get(), kind.as_str())
}

fn event_evidence(event: &DomainEvent) -> Vec<SemanticEvidence> {
    vec![SemanticEvidence {
        kind: SemanticEvidenceKind::Event,
        id: event.envelope.event_id.get(),
        role: "source".to_string(),
    }]
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::SystemTime;

    use model_core::event::{
        DomainEvent, EventEnvelope, EventFlags, EventPayload, FilePayload, ProcessPayload,
    };
    use model_core::ids::{CollectorName, EventId, TraceId};
    use model_core::process::ProcessIdentity;

    use super::*;

    fn file(flags: EventFlags, operation: &str) -> DomainEvent {
        DomainEvent::new(
            EventEnvelope {
                event_id: EventId::new(4),
                trace_id: TraceId::new(2),
                observed_at: SystemTime::UNIX_EPOCH,
                process: ProcessIdentity::new(8),
                collector: CollectorName::new("test"),
                kind: model_core::event::EventKind::Unknown,
                flags,
                session_id: None,
                call_id: None,
            },
            EventPayload::File(FilePayload {
                operation: operation.to_string(),
                path: Some("/tmp/x".to_string()),
                fd: Some(3),
                size: Some(4),
                metadata: BTreeMap::new(),
            }),
        )
    }

    #[test]
    fn projects_file_observation_with_evidence() {
        let action = project_event(&file(EventFlags::empty(), "write"))
            .actions
            .pop()
            .unwrap();
        assert_eq!(action.kind, SemanticActionKind::FileWrite);
        assert_eq!(action.completeness, SemanticActionCompleteness::Complete);
        assert_eq!(action.evidence[0].id, 4);
    }

    #[test]
    fn event_session_propagates_to_projected_actions() {
        let mut event = file(EventFlags::empty(), "write");
        event.envelope.session_id = Some(model_core::process::SessionIdentity::new("sess-1"));
        let action = project_event(&event).actions.pop().unwrap();
        assert_eq!(
            action.session_id,
            Some(model_core::process::SessionIdentity::new("sess-1"))
        );
    }

    #[test]
    fn partial_event_never_projects_as_complete() {
        let action = project_event(&file(EventFlags::PARTIAL, "read"))
            .actions
            .pop()
            .unwrap();
        assert_eq!(action.completeness, SemanticActionCompleteness::Partial);
        assert_eq!(action.confidence_millis, None);
    }

    #[test]
    fn exec_preserves_raw_argv_boundaries_and_partial_state() {
        let event = DomainEvent::new(
            EventEnvelope {
                event_id: EventId::new(5),
                trace_id: TraceId::new(2),
                observed_at: SystemTime::UNIX_EPOCH,
                process: ProcessIdentity::new(8),
                collector: CollectorName::new("test"),
                kind: model_core::event::EventKind::Unknown,
                flags: EventFlags::PARTIAL.union(EventFlags::TRUNCATED),
                session_id: None,
                call_id: None,
            },
            EventPayload::Process(ProcessPayload {
                operation: "exec".to_string(),
                parent: None,
                executable: Some("/bin/x".to_string()),
                argv: Some(model_core::process::ArgvCapture {
                    args: vec![b"a b".to_vec(), vec![0xff], Vec::new()],
                    flags: model_core::process::ArgvCapture::PARTIAL
                        | model_core::process::ArgvCapture::TRUNCATED,
                }),
                metadata: BTreeMap::new(),
            }),
        );
        let batch = project_event(&event);
        assert_eq!(batch.actions.len(), 2);
        assert_eq!(batch.links.len(), 1);
        let command = batch
            .actions
            .iter()
            .find(|action| action.kind == SemanticActionKind::CommandInvocation)
            .unwrap();
        assert_eq!(command.completeness, SemanticActionCompleteness::Partial);
        assert_eq!(command.attributes["command.argv.0.hex"], "612062");
        assert_eq!(command.attributes["command.argv.1.hex"], "ff");
        assert_eq!(command.attributes["command.argv.2.hex"], "");
    }

    #[test]
    fn transport_event_does_not_claim_http_semantics() {
        let event = DomainEvent::new(
            EventEnvelope {
                event_id: EventId::new(6),
                trace_id: TraceId::new(2),
                observed_at: SystemTime::UNIX_EPOCH,
                process: ProcessIdentity::new(8),
                collector: CollectorName::new("test"),
                kind: model_core::event::EventKind::Unknown,
                flags: EventFlags::empty(),
                session_id: None,
                call_id: None,
            },
            EventPayload::Net(model_core::event::NetPayload {
                operation: "connect".to_string(),
                endpoint: Some("127.0.0.1:80".to_string()),
                direction: model_core::payload::PayloadDirection::Outbound,
                length: None,
                result: Some(0),
                metadata: BTreeMap::new(),
            }),
        );
        assert!(project_event(&event).actions.is_empty());
    }
}
