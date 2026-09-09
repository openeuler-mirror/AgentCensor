//! CensorScope SemanticAction contract, schema/codebook version 1.
//!
//! This crate contains only observation projections. It deliberately has no
//! decision, plugin, or active-reporting concepts.

use std::collections::BTreeMap;
use std::time::SystemTime;

use model_core::ids::TraceId;
use model_core::process::{ProcessIdentity, SessionIdentity};

pub const SEMANTIC_ACTION_SCHEMA_VERSION: u16 = 1;

/// Structured, display-oriented content kept separate from raw payload bytes.
/// `canonical_json` is a bounded summary or reference, never an implicit copy
/// of a large body; metadata-only payloads must use `MetadataOnly`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SemanticContentKind {
    Unknown = 0,
    HttpSummary,
    SseSummary,
    LlmJson,
    McpJson,
    FilePathSet,
}

impl SemanticContentKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::HttpSummary => "http.summary",
            Self::SseSummary => "sse.summary",
            Self::LlmJson => "llm.json",
            Self::McpJson => "mcp.json",
            Self::FilePathSet => "file.path_set",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticContentState {
    Unknown = 0,
    Complete,
    Partial,
    MetadataOnly,
}

impl SemanticContentState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::MetadataOnly => "metadata_only",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticContent {
    pub trace_id: TraceId,
    pub action_id: String,
    pub content_id: String,
    pub kind: SemanticContentKind,
    pub state: SemanticContentState,
    pub payload_reference: Option<String>,
    pub canonical_json: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SemanticActionKind {
    Unknown = 0,
    ProcessExec,
    ProcessExit,
    ProcessForkAttempt,
    AgentIdentity,
    AgentExit,
    AgentInvocation,
    CommandInvocation,
    FileModify,
    FileRead,
    FileWrite,
    FileTtyIo,
    FileBulkRead,
    FsEnumerate,
    HttpMessage,
    LlmCall,
    LlmRequest,
    LlmResponse,
    McpToolCall,
    McpRequest,
    McpResponse,
    McpStdin,
    McpStdout,
    SseStream,
    SseEvent,
}

impl SemanticActionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::ProcessExec => "process.exec",
            Self::ProcessExit => "process.exit",
            Self::ProcessForkAttempt => "process.fork_attempt",
            Self::AgentIdentity => "agent.identity",
            Self::AgentExit => "agent.exit",
            Self::AgentInvocation => "agent.invocation",
            Self::CommandInvocation => "command.invocation",
            Self::FileModify => "file.modify",
            Self::FileRead => "file.read",
            Self::FileWrite => "file.write",
            Self::FileTtyIo => "file.tty_io",
            Self::FileBulkRead => "file.bulk_read",
            Self::FsEnumerate => "fs.enumerate",
            Self::HttpMessage => "http.message",
            Self::LlmCall => "llm.call",
            Self::LlmRequest => "llm.request",
            Self::LlmResponse => "llm.response",
            Self::McpToolCall => "mcp.tool_call",
            Self::McpRequest => "mcp.request",
            Self::McpResponse => "mcp.response",
            Self::McpStdin => "mcp.stdin",
            Self::McpStdout => "mcp.stdout",
            Self::SseStream => "sse.stream",
            Self::SseEvent => "sse.event",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Unknown,
            Self::ProcessExec,
            Self::ProcessExit,
            Self::ProcessForkAttempt,
            Self::AgentIdentity,
            Self::AgentExit,
            Self::AgentInvocation,
            Self::CommandInvocation,
            Self::FileModify,
            Self::FileRead,
            Self::FileWrite,
            Self::FileTtyIo,
            Self::FileBulkRead,
            Self::FsEnumerate,
            Self::HttpMessage,
            Self::LlmCall,
            Self::LlmRequest,
            Self::LlmResponse,
            Self::McpToolCall,
            Self::McpRequest,
            Self::McpResponse,
            Self::McpStdin,
            Self::McpStdout,
            Self::SseStream,
            Self::SseEvent,
        ]
        .into_iter()
        .find(|kind| kind.as_str() == value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticActionStatus {
    Unknown = 0,
    InProgress,
    Success,
    Error,
}

impl SemanticActionStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::InProgress => "in_progress",
            Self::Success => "success",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticActionCompleteness {
    Unknown = 0,
    Complete,
    Partial,
    Inferred,
}

impl SemanticActionCompleteness {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Inferred => "inferred",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticEvidenceKind {
    Unknown = 0,
    Event,
    PayloadAggregate,
    PayloadSegment,
}

impl SemanticEvidenceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Event => "event",
            Self::PayloadAggregate => "payload_aggregate",
            Self::PayloadSegment => "payload_segment",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SemanticActionLinkRole {
    Unknown = 0,
    AgentPerformedAction,
    CommandContainsCommandInvocation,
    CommandContainsFileAccess,
    CommandContainsLlmCall,
    CommandContainsMcpToolCall,
    CommandContainsProcessExec,
    CommandContainsProcessForkAttempt,
    FileWriteContainsFileEvent,
    LlmCallRequest,
    LlmCallResponse,
    LlmRequestHttpMessage,
    LlmRequestLlmResponse,
    LlmResponseHttpMessage,
    LlmResponseSseStream,
    McpRequestStdout,
    McpResponseStdin,
    McpToolCallRequest,
    McpToolCallResponse,
    SseStreamEvent,
}

impl SemanticActionLinkRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::AgentPerformedAction => "agent.performed_action",
            Self::CommandContainsCommandInvocation => "command.contains_command_invocation",
            Self::CommandContainsFileAccess => "command.contains_file_access",
            Self::CommandContainsLlmCall => "command.contains_llm_call",
            Self::CommandContainsMcpToolCall => "command.contains_mcp_tool_call",
            Self::CommandContainsProcessExec => "command.contains_process_exec",
            Self::CommandContainsProcessForkAttempt => "command.contains_process_fork_attempt",
            Self::FileWriteContainsFileEvent => "file_write.contains_file_event",
            Self::LlmCallRequest => "llm_call.request",
            Self::LlmCallResponse => "llm_call.response",
            Self::LlmRequestHttpMessage => "llm_request.http_message",
            Self::LlmRequestLlmResponse => "llm_request.llm_response",
            Self::LlmResponseHttpMessage => "llm_response.http_message",
            Self::LlmResponseSseStream => "llm_response.sse_stream",
            Self::McpRequestStdout => "mcp_request.stdout",
            Self::McpResponseStdin => "mcp_response.stdin",
            Self::McpToolCallRequest => "mcp_tool_call.request",
            Self::McpToolCallResponse => "mcp_tool_call.response",
            Self::SseStreamEvent => "sse_stream.event",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticActionLinkConfidence {
    Unknown = 0,
    Observed,
    Derived,
}

impl SemanticActionLinkConfidence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Observed => "observed",
            Self::Derived => "derived",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticEvidence {
    pub kind: SemanticEvidenceKind,
    pub id: u64,
    pub role: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticAction {
    pub action_id: String,
    pub trace_id: TraceId,
    pub kind: SemanticActionKind,
    pub title: String,
    pub start_time: SystemTime,
    pub end_time: Option<SystemTime>,
    pub process: ProcessIdentity,
    pub status: SemanticActionStatus,
    pub completeness: SemanticActionCompleteness,
    pub confidence_millis: Option<u16>,
    pub attributes: BTreeMap<String, String>,
    pub evidence: Vec<SemanticEvidence>,
    /// Session identified from the acting process environment. `None` marks a
    /// non-session operation.
    pub session_id: Option<SessionIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticActionLink {
    pub trace_id: TraceId,
    pub source_action_id: String,
    pub target_action_id: String,
    pub role: SemanticActionLinkRole,
    pub confidence: SemanticActionLinkConfidence,
    /// Relationship validity/lineage validity, not a verdict.
    pub valid: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codebook_is_version_one_and_has_no_decision_kind() {
        assert_eq!(SEMANTIC_ACTION_SCHEMA_VERSION, 1);
        assert_eq!(
            SemanticActionKind::parse("http.message"),
            Some(SemanticActionKind::HttpMessage)
        );
        assert_eq!(SemanticActionKind::parse("enforcement.decision"), None);
    }

    #[test]
    fn link_valid_is_independent_of_confidence() {
        let link = SemanticActionLink {
            trace_id: TraceId::new(1),
            source_action_id: "a".into(),
            target_action_id: "b".into(),
            role: SemanticActionLinkRole::LlmCallRequest,
            confidence: SemanticActionLinkConfidence::Derived,
            valid: false,
        };
        assert!(!link.valid);
        assert_eq!(link.confidence, SemanticActionLinkConfidence::Derived);
    }
}
