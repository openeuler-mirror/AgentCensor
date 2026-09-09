//! Control-plane command contracts.

use model_core::ids::{ProfileName, RequestId, TraceId, TraceName};
use model_core::process::NamespaceIdentity;
use std::collections::BTreeSet;

use crate::selector::TraceSelector;

/// Process reference sent by clients, including namespace PID identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessRef {
    pub namespace_pid: u32,
    pub pid_namespace: NamespaceIdentity,
}

impl ProcessRef {
    pub fn new(namespace_pid: u32, pid_namespace: NamespaceIdentity) -> Self {
        Self {
            namespace_pid,
            pid_namespace,
        }
    }
}

/// Request to create a trace for an already-running root process, optionally
/// continuing an existing trace id across daemon restarts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackAddCommand {
    pub request_id: RequestId,
    pub root: ProcessRef,
    pub display_name: TraceName,
    pub profile_name: ProfileName,
    pub tags: BTreeSet<String>,
    /// Reuse a persisted trace id so restarted root processes continue the
    /// same trace instead of starting a fresh observation window.
    pub trace_id: Option<TraceId>,
}

/// Request to stop tracking traces selected by the client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackRemoveCommand {
    pub request_id: RequestId,
    pub selector: TraceSelector,
}

/// Request to list only traces currently owned by this daemon instance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListTracesCommand {
    pub request_id: RequestId,
    pub selector: Option<TraceSelector>,
}

/// Readiness probe for the daemon control plane and storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoctorCommand {
    pub request_id: RequestId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallStartCommand {
    pub request_id: RequestId,
    pub trace_id: TraceId,
    pub session_id: Option<String>,
    pub call_id: String,
    pub host_pid: u32,
    pub started_at: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallEndCommand {
    pub request_id: RequestId,
    pub trace_id: TraceId,
    pub session_id: Option<String>,
    pub call_id: String,
    pub host_pid: u32,
    pub ended_at: u64,
    pub status: String,
}

/// Commands accepted over the Unix control socket.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlCommand {
    TrackAdd(TrackAddCommand),
    TrackRemove(TrackRemoveCommand),
    ListTraces(ListTracesCommand),
    Doctor(DoctorCommand),
    CallStart(CallStartCommand),
    CallEnd(CallEndCommand),
}
