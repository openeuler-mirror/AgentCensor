//! Stable layouts shared with `bpf/enforce.bpf.c`.
//!
//! These structures deliberately contain no pointers. Keep field order, widths, padding and
//! constants synchronized with the C source. Layout tests make accidental drift visible.

/// Maximum bytes in a path or executable map key.
pub const PATH_KEY_LEN: usize = 64;
/// Number of captured exec tokens.
pub const ARG_TOKEN_COUNT: usize = 4;
/// Bytes per exec token, including the trailing NUL.
pub const ARG_TOKEN_LEN: usize = 32;
/// Marker stored in argument token zero when the executable is identified by inode.
pub const ARG_INODE_MARKER: u8 = 0xff;
/// Byte offset of the encoded `(dev, ino)` pair inside argument token zero.
pub const ARG_INODE_OFFSET: usize = 8;
/// Number of policy slots, including baseline slot zero.
pub const MAX_POLICY_SLOTS: u32 = 64;
/// Baseline policy slot.
pub const BASE_SLOT: u32 = 0;
/// Maximum entries in the primary PID tracking and generation maps.
pub const PID_TRACKED_CAPACITY: usize = 4096;
/// Maximum entries in the degraded pending PID tracking map.
pub const PID_PENDING_CAPACITY: usize = 4096;
/// Maximum concurrently registered runtime scopes.
pub const SCOPE_POLICY_CAPACITY: usize = 4096;

/// File read denial bit.
pub const DENY_READ: u8 = 0x01;
/// File write denial bit.
pub const DENY_WRITE: u8 = 0x02;
/// File deletion denial bit.
pub const DENY_DELETE: u8 = 0x04;
/// File rename/link denial bit.
pub const DENY_RENAME: u8 = 0x08;
/// File attribute denial bit.
pub const DENY_ATTR: u8 = 0x10;
/// All currently defined file denial bits.
pub const DENY_ALL: u8 = 0x1f;
/// Rule action: deny is fail-safe and has priority over allow.
pub const ACTION_DENY: u8 = 0;
/// Rule action: allow (optionally audited).
pub const ACTION_ALLOW: u8 = 1;

/// Global data-plane switches.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct FilterConfig {
    pub enable_file: u32,
    pub enable_exec: u32,
    pub enable_net: u32,
    pub file_mode: u32,
    pub active_bank: u32,
    pub policy_version: u32,
    /// ALLOW audit sampling: 0=disabled, 1=all, N=one per N events.
    pub allow_sample_rate: u32,
    pub audit_file: u32,
    pub audit_exec: u32,
    pub audit_net: u32,
}

/// Fixed path key.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(C)]
pub struct PathKey {
    pub bytes: [u8; PATH_KEY_LEN],
}

impl Default for PathKey {
    fn default() -> Self {
        Self {
            bytes: [0; PATH_KEY_LEN],
        }
    }
}

/// Fixed executable path key.
pub type CommandKey = PathKey;

/// File identity used by non-sleepable LSM hooks.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
#[repr(C)]
pub struct InodeKey {
    pub dev: u64,
    pub ino: u64,
}

/// Fixed exec argument prefix key.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(C)]
pub struct ArgKey {
    pub tokens: [[u8; ARG_TOKEN_LEN]; ARG_TOKEN_COUNT],
}

impl Default for ArgKey {
    fn default() -> Self {
        Self {
            tokens: [[0; ARG_TOKEN_LEN]; ARG_TOKEN_COUNT],
        }
    }
}

/// Address-only IPv4 LPM key.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
#[repr(C)]
pub struct NetLpmKey {
    pub prefix_len: u32,
    pub addr: u32,
    pub port: u16,
    pub pad: u16,
}

/// Port-first IPv4 LPM key.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
#[repr(C)]
pub struct NetPortLpmKey {
    pub prefix_len: u32,
    pub port: u16,
    pub addr: [u8; 4],
    pub pad: u16,
}

/// Address-only IPv6 LPM key.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
#[repr(C)]
pub struct Net6LpmKey {
    pub prefix_len: u32,
    pub addr: [u8; 16],
}

/// Port-first IPv6 LPM key.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
#[repr(C)]
pub struct Net6PortLpmKey {
    pub prefix_len: u32,
    pub port: u16,
    pub addr: [u8; 16],
    pub pad: u16,
}

/// Inode rule value.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct InodeRuleValue {
    pub mask: u8,
    pub action: u8,
    pub audit: u8,
    pub pad: u8,
    pub version: u32,
}

/// String, command and argument rule value.
pub type StringRuleValue = InodeRuleValue;

/// Network allow/deny value.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct NetRuleValue {
    pub action: u8,
    pub audit: u8,
    pub pad: [u8; 2],
    pub forbid_labels: u32,
    pub version: u32,
}

/// PID-to-runtime-scope map value.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct TrackValue {
    pub scope_id: u64,
}

/// Generation-safe fallback identity used when the primary PID tracking pair cannot be written.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct PendingProcessOp {
    pub scope_id: u64,
    pub child_start_boottime: u64,
}

/// Runtime-scope-to-policy-slot binding. One policy group slot may be shared by many scopes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct ScopePolicyValue {
    pub policy_slot: u32,
    pub generation: u32,
}

/// Monotonic counters for failures and successful pending fallbacks in the BPF PID lifecycle.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct PidTrackingStats {
    pub start_update_failures: u64,
    pub tracked_update_failures: u64,
    pub pending_update_failures: u64,
    pub pending_fallbacks: u64,
    pub scope_policy_lookup_failures: u64,
}

/// Policy slot audit metadata.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct SlotMeta {
    pub version: u32,
    pub policy_group: u32,
    pub switch_ts: u64,
}

/// Raw ring-buffer event. Convert this into the wire protocol before exposing it to clients.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct RawEvent {
    pub cgroup_id: u64,
    pub ts_ns: u64,
    pub pid: u32,
    pub tgid: u32,
    pub kind: u32,
    pub allowed: u32,
    pub op: u32,
    pub policy_version: u32,
    pub rule_version: u32,
    pub _align_domain_id: u32,
    pub domain_id: u64,
    pub comm: [u8; 16],
    pub detail: [u8; 160],
    pub args: [[u8; ARG_TOKEN_LEN]; ARG_TOKEN_COUNT],
}

impl Default for RawEvent {
    fn default() -> Self {
        Self {
            cgroup_id: 0,
            ts_ns: 0,
            pid: 0,
            tgid: 0,
            kind: 0,
            allowed: 0,
            op: 0,
            policy_version: 0,
            rule_version: 0,
            _align_domain_id: 0,
            domain_id: 0,
            comm: [0; 16],
            detail: [0; 160],
            args: [[0; ARG_TOKEN_LEN]; ARG_TOKEN_COUNT],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    #[test]
    fn abi_sizes_match_c() {
        assert_eq!(size_of::<FilterConfig>(), 40);
        assert_eq!(size_of::<PathKey>(), 64);
        assert_eq!(size_of::<InodeKey>(), 16);
        assert_eq!(size_of::<ArgKey>(), 128);
        assert_eq!(size_of::<NetLpmKey>(), 12);
        assert_eq!(size_of::<NetPortLpmKey>(), 12);
        assert_eq!(size_of::<Net6LpmKey>(), 20);
        assert_eq!(size_of::<Net6PortLpmKey>(), 24);
        assert_eq!(size_of::<InodeRuleValue>(), 8);
        assert_eq!(size_of::<NetRuleValue>(), 12);
        assert_eq!(size_of::<TrackValue>(), 8);
        assert_eq!(size_of::<PendingProcessOp>(), 16);
        assert_eq!(size_of::<ScopePolicyValue>(), 8);
        assert_eq!(size_of::<PidTrackingStats>(), 40);
        assert_eq!(size_of::<SlotMeta>(), 16);
        assert_eq!(size_of::<RawEvent>(), 360);
        assert_eq!(align_of::<RawEvent>(), 8);
    }

    #[test]
    fn event_domain_id_keeps_c_alignment_gap() {
        assert_eq!(offset_of!(RawEvent, rule_version), 40);
        assert_eq!(offset_of!(RawEvent, domain_id), 48);
        assert_eq!(offset_of!(RawEvent, comm), 56);
    }
}
