//! Explicit diagnostics for collector gaps and loss; absence is never treated
//! as a complete observation.

use std::time::SystemTime;

use crate::ids::{CollectorName, TraceId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticSeverity {
    Unknown = 0,
    Info = 1,
    Warning = 2,
    Error = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticKind {
    Unknown = 0,
    BufferLoss = 1,
    CaptureGap = 2,
    ReadFailure = 3,
    Truncated = 4,
    AttachFailure = 5,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureDiagnostic {
    pub trace_id: TraceId,
    pub observed_at: SystemTime,
    pub collector: CollectorName,
    pub kind: DiagnosticKind,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub dropped: u64,
    pub dropped_bytes: u64,
}

impl CaptureDiagnostic {
    pub fn loss(
        trace_id: TraceId,
        observed_at: SystemTime,
        collector: CollectorName,
        message: impl Into<String>,
        dropped: u64,
        dropped_bytes: u64,
    ) -> Self {
        Self {
            trace_id,
            observed_at,
            collector,
            kind: DiagnosticKind::BufferLoss,
            severity: DiagnosticSeverity::Warning,
            message: message.into(),
            dropped,
            dropped_bytes,
        }
    }
}
