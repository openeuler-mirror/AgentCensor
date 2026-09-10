//! Control-plane reply and error contracts.

use std::collections::BTreeSet;
use std::time::SystemTime;

use model_core::ids::{TraceId, TraceName};
use model_core::process::NamespaceIdentity;
use model_core::trace::{TraceHealth, TraceLifecycleState};

/// Display-ready summary of one in-memory trace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceListItem {
    pub trace_id: TraceId,
    pub display_name: TraceName,
    pub root_pid: u32,
    /// Kernel PID namespace captured for the trace root. This query field is
    /// independent of runtime container metadata; authorization additionally
    /// checks the peer's mount namespace.
    pub root_pid_namespace: Option<NamespaceIdentity>,
    /// Runtime container identity resolved by the daemon from the root
    /// process cgroup. `None` means the trace is host-rooted or the runtime
    /// layout did not yield an identity.
    pub root_container_id: Option<String>,
    pub lifecycle_state: TraceLifecycleState,
    pub health: TraceHealth,
    pub tags: BTreeSet<String>,
    pub created_at: SystemTime,
}

/// Result returned after a trace is attached and activated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackAddReply {
    pub trace_id: TraceId,
    pub lifecycle_state: TraceLifecycleState,
}

/// Daemon readiness information returned by `doctor`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoctorReply {
    pub available_collectors: Vec<String>,
    pub storage_ready: bool,
}

/// Successful control-plane responses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlReply {
    TrackAdded(TrackAddReply),
    TrackRemoved,
    TraceList(Vec<TraceListItem>),
    Doctor(DoctorReply),
    CallStarted,
    CallEnded,
}

/// Structured error returned for an invalid or failed control request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlError {
    pub code: String,
    pub message: String,
}

impl ControlError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
