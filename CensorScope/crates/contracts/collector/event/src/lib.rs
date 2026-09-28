//! Raw observation-event contracts emitted by collectors.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::SystemTime;

use model_core::ids::{CollectorName, TraceId};
use model_core::payload::{PayloadContentState, PayloadDirection, PayloadSourceBoundary};
use model_core::process::{ArgvCapture, ProcessObservation, SessionIdentity};

/// Collector-side identity and timestamp before ingest normalization.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RawEventEnvelope {
    pub trace_id: Option<TraceId>,
    pub observed_at: SystemTime,
    pub process: ProcessObservation,
    pub collector: CollectorName,
    /// SessionId captured by the eBPF collector at event time.
    pub session_id: Option<SessionIdentity>,
    /// Harness tool call identifier propagated from the execution environment.
    pub call_id: Option<String>,
}

/// Raw observation payload variants carried by collector events.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RawObservationPayload {
    Process {
        operation: String,
        parent: Option<ProcessObservation>,
        argv: Option<ArgvCapture>,
        metadata: BTreeMap<String, String>,
    },
    File {
        operation: String,
        path: Option<String>,
        fd: Option<i32>,
        size: Option<u64>,
        metadata: BTreeMap<String, String>,
    },
    Net {
        operation: String,
        endpoint: Option<String>,
        direction: PayloadDirection,
        length: Option<u64>,
        result: Option<i64>,
        metadata: BTreeMap<String, String>,
    },
    Ipc {
        operation: String,
        channel: Option<String>,
        direction: PayloadDirection,
        length: Option<u64>,
        metadata: BTreeMap<String, String>,
    },
    Stdio {
        stream: String,
        direction: PayloadDirection,
        length: Option<u64>,
        metadata: BTreeMap<String, String>,
    },
    Application {
        protocol: String,
        metadata: BTreeMap<String, String>,
    },
    Loss {
        reason: String,
        dropped: u64,
        bytes: u64,
    },
}

/// Event emitted by eBPF and consumed by the ingest runtime.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RawCollectorEvent {
    pub envelope: RawEventEnvelope,
    pub payload: RawObservationPayload,
}

/// Collector-side payload segment before trace/process identity resolution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RawPayloadSegment {
    pub envelope: RawEventEnvelope,
    pub source: PayloadSourceBoundary,
    pub content_state: PayloadContentState,
    pub direction: PayloadDirection,
    pub stream_key: Option<String>,
    pub sequence: u64,
    pub operation_id: Option<u64>,
    pub offset: Option<u64>,
    pub completed: bool,
    pub original_size: u64,
    pub captured_size: u64,
    pub library: Option<String>,
    pub symbol: Option<String>,
    pub protocol_hint: Option<String>,
    pub loss_reason: Option<String>,
    pub bytes: Option<Vec<u8>>,
}
