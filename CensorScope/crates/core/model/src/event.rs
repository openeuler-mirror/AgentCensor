//! Normalized process lifecycle events persisted by CensorScope.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::ids::{CollectorName, EventId, TraceId};
use crate::process::{ArgvCapture, ProcessIdentity, SessionIdentity};

/// Common identity and timing fields for a normalized lifecycle event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventEnvelope {
    pub event_id: EventId,
    pub trace_id: TraceId,
    pub observed_at: SystemTime,
    pub process: ProcessIdentity,
    pub collector: CollectorName,
    pub kind: EventKind,
    pub flags: EventFlags,
    /// Session identified from the process environment. `None` marks a
    /// non-session operation.
    pub session_id: Option<SessionIdentity>,
    /// Harness tool call identifier used to correlate low-level observations.
    pub call_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventKind {
    Unknown = 0,
    Process = 1,
    File = 2,
    Net = 3,
    Ipc = 4,
    Stdio = 5,
    Application = 6,
    Resource = 7,
    Control = 8,
    Loss = 9,
}

impl EventKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Process => "process",
            Self::File => "file",
            Self::Net => "net",
            Self::Ipc => "ipc",
            Self::Stdio => "stdio",
            Self::Application => "application",
            Self::Resource => "resource",
            Self::Control => "control",
            Self::Loss => "loss",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EventFlags(u32);

impl EventFlags {
    pub const PARTIAL: Self = Self(1 << 0);
    pub const TRUNCATED: Self = Self(1 << 1);
    pub const LOSS: Self = Self(1 << 2);
    pub const CAPTURE_GAP: Self = Self(1 << 3);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Process operation details emitted by procfs or eBPF collection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessPayload {
    pub operation: String,
    pub parent: Option<ProcessIdentity>,
    pub executable: Option<String>,
    pub argv: Option<ArgvCapture>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilePayload {
    pub operation: String,
    pub path: Option<String>,
    pub fd: Option<i32>,
    pub size: Option<u64>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetPayload {
    pub operation: String,
    pub endpoint: Option<String>,
    pub direction: crate::payload::PayloadDirection,
    pub length: Option<u64>,
    pub result: Option<i64>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IpcPayload {
    pub operation: String,
    pub channel: Option<String>,
    pub direction: crate::payload::PayloadDirection,
    pub length: Option<u64>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StdioPayload {
    pub stream: String,
    pub direction: crate::payload::PayloadDirection,
    pub length: Option<u64>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationPayload {
    pub protocol: String,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourcePayload {
    pub resource: String,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlPayload {
    pub operation: String,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LossPayload {
    pub reason: String,
    pub dropped: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventPayload {
    Process(ProcessPayload),
    File(FilePayload),
    Net(NetPayload),
    Ipc(IpcPayload),
    Stdio(StdioPayload),
    Application(ApplicationPayload),
    Resource(ResourcePayload),
    Control(ControlPayload),
    Loss(LossPayload),
}

impl EventPayload {
    pub const fn kind(&self) -> EventKind {
        match self {
            Self::Process(_) => EventKind::Process,
            Self::File(_) => EventKind::File,
            Self::Net(_) => EventKind::Net,
            Self::Ipc(_) => EventKind::Ipc,
            Self::Stdio(_) => EventKind::Stdio,
            Self::Application(_) => EventKind::Application,
            Self::Resource(_) => EventKind::Resource,
            Self::Control(_) => EventKind::Control,
            Self::Loss(_) => EventKind::Loss,
        }
    }
}

/// Normalized process event passed from ingest to storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainEvent {
    pub envelope: EventEnvelope,
    pub payload: EventPayload,
}

impl DomainEvent {
    pub fn new(mut envelope: EventEnvelope, payload: EventPayload) -> Self {
        envelope.kind = payload.kind();
        Self { envelope, payload }
    }
}
