//! Capture profiles selected by collection level (L1–L3) or by default.

use std::fmt;
use std::str::FromStr;

use model_core::capability::{Capability, CapabilityRequest};
use model_core::ids::ProfileName;

/// Collection level selected per daemon run via `censorscoped --level`.
///
/// Levels are ordered (L1 < L2 < L3) and each level's capability set is a
/// superset of the lower level. Adding a future level means adding a variant
/// here, its `as_str`/`FromStr` arm, and its `capabilities()` row.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CaptureLevel {
    /// Process basics + basic file operations. Default when no `--level`.
    L1,
    /// L1 + mmap, network endpoints, and TLS plaintext payloads.
    L2,
    /// Full capability set: L2 + IPC channels and stdio chunks.
    L3,
}

impl CaptureLevel {
    pub const ALL: [CaptureLevel; 3] = [CaptureLevel::L1, CaptureLevel::L2, CaptureLevel::L3];

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::L1 => "L1",
            Self::L2 => "L2",
            Self::L3 => "L3",
        }
    }

    /// The capture capabilities this level requests (all required).
    ///
    /// Pipe/pipe2 and socketpair probes stay attached at every level as
    /// internal channel infrastructure (see the design document); they only
    /// become an audit capability from L3 onward.
    pub const fn capabilities(self) -> &'static [Capability] {
        match self {
            Self::L1 => &[
                Capability::ProcLifecycle,
                Capability::ProcExecContext,
                Capability::FsAccessBasic,
            ],
            Self::L2 => &[
                Capability::ProcLifecycle,
                Capability::ProcExecContext,
                Capability::FsAccessBasic,
                Capability::FsMmap,
                Capability::NetTransport,
                Capability::TlsPlaintextPayload,
            ],
            Self::L3 => &[
                Capability::ProcLifecycle,
                Capability::ProcExecContext,
                Capability::FsAccessBasic,
                Capability::FsMmap,
                Capability::NetTransport,
                Capability::TlsPlaintextPayload,
                Capability::IpcPipeFifo,
                Capability::IpcUnixSocket,
                Capability::StdioChunk,
            ],
        }
    }
}

impl FromStr for CaptureLevel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "L1" => Ok(Self::L1),
            "L2" => Ok(Self::L2),
            "L3" => Ok(Self::L3),
            _ => Err(format!(
                "invalid capture level {value:?}; expected one of L1, L2, L3"
            )),
        }
    }
}

impl fmt::Display for CaptureLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The process-lifecycle capture profile used by daemon traces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureProfile {
    pub name: ProfileName,
    pub capabilities: Vec<CapabilityRequest>,
}

impl CaptureProfile {
    /// Build the capture profile for one collection level (daemon run-time).
    ///
    /// The profile name is the level id (`L1`/`L2`/`L3`) so persisted traces
    /// record which collection level produced them.
    pub fn for_level(level: CaptureLevel) -> Self {
        Self {
            name: ProfileName::new(level.as_str()),
            capabilities: level
                .capabilities()
                .iter()
                .copied()
                .map(CapabilityRequest::required)
                .collect(),
        }
    }

    pub fn new(name: ProfileName, capabilities: Vec<CapabilityRequest>) -> Self {
        Self { name, capabilities }
    }

    pub fn supports_host_ebpf_observation(&self) -> bool {
        self.capabilities
            .iter()
            .any(|request| request.capability == Capability::ProcLifecycle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_ordered_and_monotonic() {
        assert!(CaptureLevel::L1 < CaptureLevel::L2);
        assert!(CaptureLevel::L2 < CaptureLevel::L3);
        for (level, superset) in [
            (CaptureLevel::L1, CaptureLevel::L2.capabilities()),
            (CaptureLevel::L2, CaptureLevel::L3.capabilities()),
        ] {
            for capability in level.capabilities() {
                assert!(
                    superset.contains(capability),
                    "{level} capability {capability:?} missing from superset"
                );
            }
        }
    }

    #[test]
    fn level_parse_and_display_round_trip() {
        for level in CaptureLevel::ALL {
            let parsed: CaptureLevel = level.as_str().parse().expect("valid level");
            assert_eq!(parsed, level);
            assert_eq!(parsed.to_string(), level.as_str());
        }
        assert!("L0".parse::<CaptureLevel>().is_err());
        assert!("l1".parse::<CaptureLevel>().is_err());
        assert!("L4".parse::<CaptureLevel>().is_err());
        assert!("".parse::<CaptureLevel>().is_err());
    }

    #[test]
    fn level_profiles_carry_level_id_and_all_required_requests() {
        for level in CaptureLevel::ALL {
            let profile = CaptureProfile::for_level(level);
            assert_eq!(profile.name.as_str(), level.as_str());
            assert_eq!(profile.capabilities.len(), level.capabilities().len());
            assert!(
                profile
                    .capabilities
                    .iter()
                    .all(|request| request.mode == model_core::capability::RequestMode::Required)
            );
        }
    }

    #[test]
    fn l3_is_the_full_capability_set() {
        let l3 = CaptureProfile::for_level(CaptureLevel::L3);
        assert_eq!(l3.capabilities.len(), 9);
        for capability in [
            Capability::ProcLifecycle,
            Capability::ProcExecContext,
            Capability::FsAccessBasic,
            Capability::FsMmap,
            Capability::NetTransport,
            Capability::IpcPipeFifo,
            Capability::IpcUnixSocket,
            Capability::StdioChunk,
            Capability::TlsPlaintextPayload,
        ] {
            assert!(
                l3.capabilities
                    .iter()
                    .any(|request| request.capability == capability),
                "L3 missing {capability:?}"
            );
        }
    }
}
