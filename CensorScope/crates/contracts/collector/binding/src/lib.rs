//! Trace-to-collector binding contracts.

use std::time::SystemTime;

use collector_capability::CollectorDescriptor;
use config_core::trace_snapshot::CaptureProfileSnapshot;
use model_core::capability::CapabilityRequest;
use model_core::ids::TraceId;
use model_core::process::{ProcessIdentity, ProcessObservation};

/// Immutable inputs used when a collector starts tracking a trace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceBindingRequest {
    pub trace_id: TraceId,
    pub root_identity: ProcessIdentity,
    pub root_observation: ProcessObservation,
    pub root_namespace_pid: u32,
    pub profile_snapshot: CaptureProfileSnapshot,
    pub requested_capabilities: Vec<CapabilityRequest>,
}

/// Handle proving that a collector binding was established.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceBindingHandle {
    pub collector: CollectorDescriptor,
    pub bound_at: SystemTime,
}
