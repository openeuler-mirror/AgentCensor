//! Passive observation capability models and negotiation metadata.

use std::collections::BTreeSet;

/// Capture capability understood by the CensorScope process collector.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Capability {
    ProcLifecycle,
    ProcExecContext,
    FsAccessBasic,
    FsMmap,
    NetTransport,
    IpcUnixSocket,
    IpcPipeFifo,
    StdioChunk,
    TlsPlaintextPayload,
}

impl Capability {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ProcLifecycle => "proc-lifecycle",
            Self::ProcExecContext => "proc-exec-context",
            Self::FsAccessBasic => "fs-access-basic",
            Self::FsMmap => "fs-mmap",
            Self::NetTransport => "net-transport",
            Self::IpcUnixSocket => "ipc-unix-socket",
            Self::IpcPipeFifo => "ipc-pipe-fifo",
            Self::StdioChunk => "stdio-chunk",
            Self::TlsPlaintextPayload => "tls-plaintext-payload",
        }
    }

    /// Stable attach-plan key used by collectors. This is deliberately an
    /// observation-only mapping; it is not a policy or enforcement action.
    pub const fn attach_key(&self) -> &'static str {
        match self {
            Self::ProcLifecycle => "process.lifecycle",
            Self::ProcExecContext => "process.exec_context",
            Self::FsAccessBasic => "fs.access_basic",
            Self::FsMmap => "fs.mmap",
            Self::NetTransport => "net.transport",
            Self::IpcUnixSocket => "ipc.unix_socket",
            Self::IpcPipeFifo => "ipc.pipe_fifo",
            Self::StdioChunk => "stdio.chunk",
            Self::TlsPlaintextPayload => "net.tls_plaintext_payload",
        }
    }
}

/// How a capability is requested by a capture profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestMode {
    Required,
    BestEffort,
}

/// Collection guarantee advertised by a capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuaranteeClass {
    Event,
    PayloadBestEffort,
    DynamicRuntime,
}

/// Capability requested by a capture profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityRequest {
    pub capability: Capability,
    pub mode: RequestMode,
}

impl CapabilityRequest {
    pub fn required(capability: Capability) -> Self {
        Self {
            capability,
            mode: RequestMode::Required,
        }
    }

    pub fn best_effort(capability: Capability) -> Self {
        Self {
            capability,
            mode: RequestMode::BestEffort,
        }
    }
}

/// Capability advertised by a collector implementation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityDescriptor {
    pub capability: Capability,
    pub guarantee: GuaranteeClass,
    pub description: &'static str,
    pub attach_key: &'static str,
}

impl CapabilityDescriptor {
    pub fn new(capability: Capability) -> Self {
        let guarantee = match capability {
            Capability::TlsPlaintextPayload => GuaranteeClass::DynamicRuntime,
            Capability::StdioChunk => GuaranteeClass::PayloadBestEffort,
            _ => GuaranteeClass::Event,
        };
        let description = match capability {
            Capability::ProcLifecycle => "process fork, exec, and exit lifecycle",
            Capability::ProcExecContext => "exec context and argv observations",
            Capability::FsAccessBasic => "basic file and descriptor access",
            Capability::FsMmap => "memory-map observations",
            Capability::NetTransport => "socket endpoint and transport events",
            Capability::IpcUnixSocket => "Unix socket IPC lineage",
            Capability::IpcPipeFifo => "pipe and FIFO IPC lineage",
            Capability::StdioChunk => "stdio and TTY chunks",
            Capability::TlsPlaintextPayload => "TLS plaintext payload segments",
        };
        Self {
            attach_key: capability.attach_key(),
            capability,
            guarantee,
            description,
        }
    }
}

/// Set-like view used during capability negotiation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CapabilitySet {
    values: BTreeSet<Capability>,
}

impl CapabilitySet {
    pub fn new(values: impl IntoIterator<Item = Capability>) -> Self {
        Self {
            values: values.into_iter().collect(),
        }
    }

    pub fn contains(&self, capability: &Capability) -> bool {
        self.values.contains(capability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_capabilities_have_stable_attach_keys() {
        let capabilities = [
            Capability::ProcLifecycle,
            Capability::ProcExecContext,
            Capability::FsAccessBasic,
            Capability::FsMmap,
            Capability::NetTransport,
            Capability::IpcUnixSocket,
            Capability::IpcPipeFifo,
            Capability::StdioChunk,
            Capability::TlsPlaintextPayload,
        ];
        for capability in capabilities {
            assert!(!capability.as_str().is_empty());
            assert!(!capability.attach_key().is_empty());
            assert_eq!(CapabilityDescriptor::new(capability).capability, capability);
        }
    }

    #[test]
    fn payload_capabilities_are_dynamic_runtime_guarantees() {
        assert_eq!(
            CapabilityDescriptor::new(Capability::TlsPlaintextPayload).guarantee,
            GuaranteeClass::DynamicRuntime
        );
    }

    #[test]
    fn best_effort_requests_do_not_become_required() {
        let request = CapabilityRequest::best_effort(Capability::NetTransport);
        assert_eq!(request.mode, RequestMode::BestEffort);
    }
}
