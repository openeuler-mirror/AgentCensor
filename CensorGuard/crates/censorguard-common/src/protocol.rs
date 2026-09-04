//! Versioned JSON-lines protocol shared by daemon, ctl and audit.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use thiserror::Error;

pub const VERSION: u16 = 3;
pub const MIN_VERSION: u16 = 1;
pub const DEFAULT_CONTROL_SOCKET: &str = "/run/censorguard/ctl.sock";
pub const DEFAULT_EVENT_SOCKET: &str = "/run/censorguard/events.sock";
pub const DEFAULT_LAUNCH_SOCKET: &str = "/run/censorguard/launch.sock";
pub const DEFAULT_DSH_SOCKET: &str = "/run/censorguard/dsh.sock";
pub const DEFAULT_UI_SOCKET: &str = "/run/censorguard/ui.sock";
pub const MAX_LINE_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Track,
    Untrack,
    Status,
    Reload,
    Bind,
    PolicyDump,
    Tree,
    SetSwitches,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcMethod {
    Health,
    GetCapabilities,
    GetPolicy,
    ValidatePolicy,
    ApplyPolicy,
    RollbackPolicy,
    EvaluateIntent,
    RegisterSelf,
    RebindScope,
    CloseScope,
    ListScopes,
    ListTrees,
    GetMetrics,
    SetSwitches,
    AttachSelf,
    StatusSelf,
    TreeSelf,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RpcParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_yaml: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intents: Vec<SecurityIntent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_file: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_exec: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_net: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit_file: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit_exec: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit_net: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_hint: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub seed: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RpcRequest {
    #[serde(rename = "v")]
    pub version: u16,
    pub request_id: String,
    pub method: RpcMethod,
    #[serde(default)]
    pub params: RpcParams,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SecurityIntent {
    File {
        operation: FileIntentOperation,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        destination: Option<String>,
    },
    Exec {
        executable: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        argv: Vec<String>,
    },
    Network {
        host: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scheme: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    UnknownTool {
        tool_name: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileIntentOperation {
    Read,
    Write,
    Delete,
    Rename,
    Attr,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct IntentDecision {
    pub allowed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcErrorCode {
    InvalidRequest,
    UnsupportedVersion,
    PermissionDenied,
    PolicyInvalid,
    PolicyCapacityExceeded,
    RevisionConflict,
    UnknownGroup,
    UnknownScope,
    PidReused,
    ScopeAlreadyRegistered,
    IntentUnsupported,
    Internal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RpcError {
    pub code: RpcErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_revision: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RpcResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Response>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_yaml: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<IntentDecision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RpcResponse {
    #[serde(rename = "v")]
    pub version: u16,
    pub request_id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<RpcResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Operation {
    #[must_use]
    pub const fn requires_root(&self) -> bool {
        matches!(self, Self::Reload | Self::Bind | Self::SetSwitches)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Request {
    #[serde(default = "default_version", rename = "v")]
    pub version: u16,
    pub op: Operation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default)]
    pub seed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_yaml: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_file: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_exec: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_net: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_file: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_exec: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_net: Option<bool>,
}

const fn default_version() -> u16 {
    VERSION
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DomainInfo {
    pub name: String,
    pub id: u64,
    pub slot: u32,
    pub group: String,
    pub roots: usize,
    pub version: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub draining: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeded: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<DomainInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<u32>,
    #[serde(default)]
    pub tracked: usize,
    #[serde(default)]
    pub pid_pending: usize,
    #[serde(default)]
    pub pid_generations: usize,
    #[serde(default)]
    pub pid_tracked_capacity: usize,
    #[serde(default)]
    pub pid_pending_capacity: usize,
    #[serde(default)]
    pub pid_start_update_failures: u64,
    #[serde(default)]
    pub pid_tracked_update_failures: u64,
    #[serde(default)]
    pub pid_pending_update_failures: u64,
    #[serde(default)]
    pub pid_pending_fallbacks: u64,
    #[serde(default)]
    pub scope_policy_lookup_failures: u64,
    #[serde(default)]
    pub scopes: usize,
    #[serde(default)]
    pub scope_capacity: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<DomainInfo>,
    #[serde(default)]
    pub reload_gen: u64,
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub active_bank: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reload_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reload_error: Option<String>,
    #[serde(default)]
    pub event_kernel_dropped: u64,
    #[serde(default)]
    pub event_reader_dropped: u64,
    #[serde(default)]
    pub event_subscriber_dropped: u64,
    #[serde(default)]
    pub event_subscribers: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<GroupDump>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trees: Vec<DomainTree>,
    #[serde(default)]
    pub enable_file: bool,
    #[serde(default)]
    pub enable_exec: bool,
    #[serde(default)]
    pub enable_net: bool,
    #[serde(default)]
    pub audit_file: bool,
    #[serde(default)]
    pub audit_exec: bool,
    #[serde(default)]
    pub audit_net: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dns: Vec<DnsDomainInfo>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub daemon_boot_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks_healthy: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DnsDomainInfo {
    pub domain: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PolicyDefinition {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct GroupDump {
    pub name: String,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<String>,
    pub definition: PolicyDefinition,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TreeNode {
    pub pid: u32,
    pub ppid: u32,
    pub comm: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub root: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DomainTree {
    pub domain: String,
    pub domain_id: u64,
    pub nodes: Vec<TreeNode>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Event {
    pub ts: String,
    pub kind: u32,
    pub op: u32,
    pub pid: u32,
    pub tgid: u32,
    pub allowed: bool,
    #[serde(default, rename = "pv", skip_serializing_if = "is_zero_u32")]
    pub policy_version: u32,
    #[serde(default, rename = "rv", skip_serializing_if = "is_zero_u32")]
    pub rule_version: u32,
    #[serde(default, rename = "did", skip_serializing_if = "is_zero_u64")]
    pub domain_id: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub domain: String,
    pub comm: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Daemon-local monotonic sequence, starting at 1 (0 is the "unset" sentinel).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub sequence: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub daemon_boot_id: String,
    /// Per-subscriber count of events dropped since the last delivered frame.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub dropped_before: u64,
}

const fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

const fn is_false(value: &bool) -> bool {
    !*value
}

const fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("JSON-lines frame is too large: {actual} > {max}")]
    FrameTooLarge { actual: usize, max: usize },
    #[error("unsupported protocol version {actual}; expected {expected}")]
    Version { actual: u16, expected: u16 },
    #[error("end of JSON-lines stream")]
    EndOfStream,
}

pub fn write_json_line(
    mut writer: impl Write,
    value: &impl Serialize,
) -> Result<(), ProtocolError> {
    serde_json::to_writer(&mut writer, value)?;
    writer.write_all(b"\n")?;
    Ok(())
}

pub fn read_json_line<T: for<'de> Deserialize<'de>>(
    reader: &mut impl BufRead,
) -> Result<T, ProtocolError> {
    let mut frame = Vec::new();
    let mut limited = std::io::Read::take(reader, (MAX_LINE_BYTES + 1) as u64);
    let read = limited.read_until(b'\n', &mut frame)?;
    if read == 0 {
        return Err(ProtocolError::EndOfStream);
    }
    if read > MAX_LINE_BYTES {
        return Err(ProtocolError::FrameTooLarge {
            actual: read,
            max: MAX_LINE_BYTES,
        });
    }
    Ok(serde_json::from_slice(&frame)?)
}

pub fn validate_version(actual: u16) -> Result<(), ProtocolError> {
    if (MIN_VERSION..=VERSION).contains(&actual) {
        Ok(())
    } else {
        Err(ProtocolError::Version {
            actual,
            expected: VERSION,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    #[test]
    fn request_round_trip_preserves_legacy_shape() -> Result<(), ProtocolError> {
        let request = Request {
            version: VERSION,
            op: Operation::Track,
            pid: Some(42),
            domain: Some("lab".into()),
            seed: true,
            policy_file: None,
            policy_yaml: None,
            group: None,
            dry_run: false,
            enable_file: None,
            enable_exec: None,
            enable_net: None,
            audit_file: None,
            audit_exec: None,
            audit_net: None,
        };
        let mut bytes = Vec::new();
        write_json_line(&mut bytes, &request)?;
        let decoded: Request = read_json_line(&mut BufReader::new(bytes.as_slice()))?;
        assert_eq!(decoded, request);
        Ok(())
    }

    #[test]
    fn privileged_operations_are_explicit() {
        assert!(Operation::Reload.requires_root());
        assert!(Operation::Bind.requires_root());
        assert!(!Operation::Track.requires_root());
    }

    #[test]
    fn protocol_accepts_supported_versions() {
        assert!(validate_version(1).is_ok());
        assert!(validate_version(2).is_ok());
        assert!(validate_version(3).is_ok());
        assert!(validate_version(4).is_err());
    }
}
