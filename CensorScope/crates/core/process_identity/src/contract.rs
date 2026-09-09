//! Stable process identity types shared by collectors, runtime, and storage.

use std::collections::BTreeSet;
use std::fmt;

/// Opaque identifier for a Linux PID namespace.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NamespaceIdentity(String);

impl NamespaceIdentity {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable session identifier carried by a process environment (for example
/// `DSH_SESSION_ID` in a model shell call). A session groups the processes of
/// one logical agent conversation; processes without the variable do not
/// belong to any session and are recorded as non-session operations.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionIdentity(String);

impl SessionIdentity {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Daemon-assigned identity for one process lifetime.
///
/// Unlike a PID, this value is not reused while its allocation domain remains valid.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProcessIdentity(u64);

impl ProcessIdentity {
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ProcessIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "process-{}", self.0)
    }
}

/// Coordinates that distinguish a process lifetime in the host PID namespace.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HostProcessCoordinates {
    pub pid: u32,
    pub task_id: Option<u32>,
    pub start_time_ticks: u64,
    pub start_boottime_ns: Option<u64>,
}

impl HostProcessCoordinates {
    pub const fn new(pid: u32, start_time_ticks: u64) -> Self {
        Self {
            pid,
            task_id: None,
            start_time_ticks,
            start_boottime_ns: None,
        }
    }

    pub const fn with_task_id(mut self, task_id: u32) -> Self {
        self.task_id = Some(task_id);
        self
    }

    pub const fn with_start_boottime_ns(mut self, start_boottime_ns: u64) -> Self {
        self.start_boottime_ns = Some(start_boottime_ns);
        self
    }
}

/// Coordinates that distinguish a process lifetime inside a PID namespace.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NamespaceProcessCoordinates {
    pub pid_namespace: NamespaceIdentity,
    pub pid: u32,
    pub start_time_ticks: u64,
}

impl NamespaceProcessCoordinates {
    pub fn new(pid_namespace: NamespaceIdentity, pid: u32, start_time_ticks: u64) -> Self {
        Self {
            pid_namespace,
            pid,
            start_time_ticks,
        }
    }
}

/// Host and namespace coordinates observed for the same process lifetime.
///
/// An observation may initially contain only namespace coordinates and be enriched
/// with host coordinates after procfs resolution.
#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProcessObservation {
    pub host: Option<HostProcessCoordinates>,
    pub namespace: Option<NamespaceProcessCoordinates>,
}

impl ProcessObservation {
    pub fn host(host: HostProcessCoordinates) -> Self {
        Self {
            host: Some(host),
            namespace: None,
        }
    }

    pub fn namespace(namespace: NamespaceProcessCoordinates) -> Self {
        Self {
            host: None,
            namespace: Some(namespace),
        }
    }

    pub fn with_namespace(mut self, namespace: NamespaceProcessCoordinates) -> Self {
        self.namespace = Some(namespace);
        self
    }
}

/// Completeness and consistency of a process identity record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessResolutionState {
    Provisional,
    Resolved,
    Conflicted,
}

/// Canonical coordinates currently associated with a logical process identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessRecord {
    pub identity: ProcessIdentity,
    pub host: Option<HostProcessCoordinates>,
    pub namespaces: BTreeSet<NamespaceProcessCoordinates>,
    pub resolution_state: ProcessResolutionState,
    /// Session identified from the process environment (for example
    /// `DSH_SESSION_ID`). `None` marks a non-session operation.
    pub session_id: Option<SessionIdentity>,
}

impl ProcessRecord {
    pub fn new(identity: ProcessIdentity, observation: ProcessObservation) -> Self {
        let namespaces = observation.namespace.into_iter().collect();
        let resolution_state = if observation.host.is_some() {
            ProcessResolutionState::Resolved
        } else {
            ProcessResolutionState::Provisional
        };
        Self {
            identity,
            host: observation.host,
            namespaces,
            resolution_state,
            session_id: None,
        }
    }

    pub fn observation(&self) -> ProcessObservation {
        ProcessObservation {
            host: self.host.clone(),
            namespace: self.namespaces.iter().next().cloned(),
        }
    }
}

/// Failure returned when procfs cannot provide trustworthy process coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdentityLookupError {
    NotFound { pid: u32 },
    PermissionDenied { pid: u32 },
    Incomplete { pid: u32, detail: String },
}

/// Resolves an operating-system PID into stable process-lifetime coordinates.
pub trait ProcessIdentityReader {
    /// Reads the best available host and namespace identity for `pid`.
    fn read_identity(&self, pid: u32) -> Result<ProcessObservation, IdentityLookupError>;
}
