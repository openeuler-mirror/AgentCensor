//! Operator configuration required by the CensorScope process tracker.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use storage_factory::StorageConfig;

use crate::capture_profile::{CaptureLevel, CaptureProfile};

pub const DEFAULT_OPERATOR_CONFIG_PATH: &str = "/etc/censorscope/censorscoped.conf";
pub const DEFAULT_CONTROL_PENDING_CONNECTION_MAX: u32 = 256;
pub const DEFAULT_ACTIVE_TRACE_MAX: u32 = 128;
/// Default commit size of the writer thread's event accumulator (rows).
///
/// Larger batches amortize the per-commit cost over more rows, which is what
/// keeps the writer ahead of a fast capture; the accumulator holds at most this
/// many rows, so the value also bounds the memory a commit can hold.
pub const DEFAULT_WRITER_BATCH_ITEMS: u32 = 2048;
/// Default idle window (ms) before a partial writer accumulator is committed.
pub const DEFAULT_WRITER_IDLE_TIMEOUT_MS: u64 = 8;
/// Default cadence (seconds) of the writer thread's PASSIVE checkpoint.
pub const DEFAULT_WRITER_CHECKPOINT_INTERVAL_SECS: u64 = 60;
/// Default bounded bulk-queue capacity; kept bounded so a slow writer cannot
/// balloon daemon RAM into an OOM on no-swap hosts.
pub const DEFAULT_WRITER_QUEUE_CAP_ITEMS: u32 = 65_536;
/// Default memory budget for rows waiting on the writer, in bytes.
///
/// Reaching it makes the event loop wait for the writer rather than discard
/// rows, so overload is absorbed by the kernel transport -- sized by
/// `ebpf.event_ring_buffer_max_bytes` -- and reported as per-class transport
/// loss instead of as whole queued batches disappearing here.
pub const DEFAULT_WRITER_QUEUE_BUDGET_BYTES: u64 = 256 * 1024 * 1024;
/// Default maximum on-disk size of the collector spool.
pub const DEFAULT_WRITER_SPOOL_MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Default permissions for the daemon control socket.
///
/// The daemon may run with elevated privileges while `censorscopectl` runs as
/// the operator's account; peer-identity checks still gate access, so the
/// socket must be reachable by non-root users.
pub const DEFAULT_SOCKET_MODE: u32 = 0o666;
/// Environment variable name used to identify the session of an observed
/// process (for example `DSH_SESSION_ID` injected into model shell calls).
pub const DEFAULT_SESSION_ENV_NAME: &str = "DSH_SESSION_ID";

/// Result of initializing an operator configuration file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperatorConfigInitStatus {
    Created,
    ExistingValid,
    Overwritten,
}

/// Unix permission bits applied to the daemon control socket.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SocketPermissions {
    pub mode: u32,
}

/// RLIMIT_MEMLOCK policy applied before loading eBPF maps and programs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemlockRlimit {
    Inherit,
    Unlimited,
    Bytes(u64),
}

impl FromStr for MemlockRlimit {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "inherit" => Ok(Self::Inherit),
            "unlimited" => Ok(Self::Unlimited),
            value => value
                .strip_prefix("bytes:")
                .ok_or_else(|| "expected inherit, unlimited, or bytes:<n>".to_string())?
                .parse::<u64>()
                .map(Self::Bytes)
                .map_err(|error| error.to_string()),
        }
    }
}

impl std::fmt::Display for MemlockRlimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inherit => formatter.write_str("inherit"),
            Self::Unlimited => formatter.write_str("unlimited"),
            Self::Bytes(bytes) => write!(formatter, "bytes:{bytes}"),
        }
    }
}

/// Operator choice for enabling, disabling, or probing eBPF support.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EbpfEnabledMode {
    True,
    False,
    Auto,
}

/// Controls how often process mappings are rescanned for newly loaded TLS images.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TlsScanDebounce {
    None,
    Image,
    Interval,
}

impl FromStr for TlsScanDebounce {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "none" => Ok(Self::None),
            "image" => Ok(Self::Image),
            "interval" => Ok(Self::Interval),
            _ => Err(format!("invalid ebpf.tls_scan_debounce: {value}")),
        }
    }
}

impl std::fmt::Display for TlsScanDebounce {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::None => "none",
            Self::Image => "image",
            Self::Interval => "interval",
        })
    }
}

impl FromStr for EbpfEnabledMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "true" => Ok(Self::True),
            "false" => Ok(Self::False),
            "auto" => Ok(Self::Auto),
            _ => Err(format!("invalid ebpf.enabled: {value}")),
        }
    }
}

impl std::fmt::Display for EbpfEnabledMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::True => "true",
            Self::False => "false",
            Self::Auto => "auto",
        })
    }
}

/// eBPF loader and map sizing configuration for lifecycle collection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EbpfCollectorConfig {
    pub enabled_mode: EbpfEnabledMode,
    pub enabled: bool,
    pub memlock_rlimit: MemlockRlimit,
    pub tracked_process_max_entries: u32,
    pub pending_operation_max_entries: u32,
    pub event_ring_buffer_max_bytes: u32,
    pub tls_dynamic_loading: bool,
    pub tls_scan_debounce: TlsScanDebounce,
    pub tls_scan_interval_ms: u64,
}

/// Validated daemon configuration consumed by ctl and censorscoped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperatorConfig {
    pub socket_path: PathBuf,
    pub socket_permissions: SocketPermissions,
    pub control_pending_connection_max: u32,
    pub active_trace_max: u32,
    pub pid_file: PathBuf,
    pub log_path: PathBuf,
    pub storage: StorageConfig,
    pub capture_profile: CaptureProfile,
    pub ebpf_config: EbpfCollectorConfig,
    pub startup_wait_ms: u64,
    pub shutdown_wait_ms: u64,
    pub supervision_poll_interval_ms: u64,
    /// Environment variable name identifying the session of an observed
    /// process; processes without the variable are non-session operations.
    pub session_env_name: String,
    pub writer: WriterConfig,
    /// Config-file keys that are still accepted but no longer honored
    /// (for example the removed `payload.max_trace_bytes`). Surfaced so the
    /// daemon can warn at startup instead of silently ignoring an operator's
    /// intent.
    pub deprecated_keys: Vec<String>,
}

/// Persistence writer-thread tuning (censorscoped.conf `[writer]` section).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriterConfig {
    /// Commit a partial event accumulator once this many events accumulate.
    pub batch_items: u32,
    /// Idle window (ms) before a partial accumulator is committed. Zero is
    /// rejected so the writer never busy-spins.
    pub idle_timeout_ms: u64,
    /// Periodic PASSIVE checkpoint cadence (seconds); zero disables the
    /// periodic checkpoint (a final checkpoint still runs at shutdown).
    pub checkpoint_interval_secs: u64,
    /// Bulk-queue capacity in items. A full queue makes the writer endpoint wait
    /// for room and, if none appears, drop the incoming item and count it, so
    /// the loss is reported through the `data.queue_overflow` stage of the loss
    /// ledger. Zero keeps the queue unbounded.
    pub queue_cap_items: u32,
    /// Memory budget for queued rows, in bytes; zero disables the budget.
    ///
    /// Under sustained overload this is what the producer waits on, so the
    /// memory ceiling is a stated number rather than a product of the item cap
    /// and the largest item.
    pub queue_budget_bytes: u64,
    /// Maximum collector spool size in bytes; zero disables the limit.
    pub spool_max_bytes: u64,
}

impl Default for WriterConfig {
    fn default() -> Self {
        Self {
            batch_items: DEFAULT_WRITER_BATCH_ITEMS,
            idle_timeout_ms: DEFAULT_WRITER_IDLE_TIMEOUT_MS,
            checkpoint_interval_secs: DEFAULT_WRITER_CHECKPOINT_INTERVAL_SECS,
            queue_cap_items: DEFAULT_WRITER_QUEUE_CAP_ITEMS,
            queue_budget_bytes: DEFAULT_WRITER_QUEUE_BUDGET_BYTES,
            spool_max_bytes: DEFAULT_WRITER_SPOOL_MAX_BYTES,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorDocument {
    daemon: DaemonDocument,
    storage: StorageDocument,
    profile: ProfileDocument,
    ebpf: EbpfDocument,
    /// Omitted from generated files; retained so deployed files that still
    /// carry the deprecated `[payload]` keys keep parsing.
    #[serde(default, skip_serializing_if = "PayloadDocument::is_empty")]
    payload: PayloadDocument,
    #[serde(default)]
    session: SessionDocument,
    #[serde(default)]
    writer: WriterDocument,
}

fn default_writer_batch_items() -> u32 {
    DEFAULT_WRITER_BATCH_ITEMS
}

fn default_writer_idle_timeout_ms() -> u64 {
    DEFAULT_WRITER_IDLE_TIMEOUT_MS
}

fn default_writer_checkpoint_interval_secs() -> u64 {
    DEFAULT_WRITER_CHECKPOINT_INTERVAL_SECS
}

fn default_writer_queue_cap_items() -> u32 {
    DEFAULT_WRITER_QUEUE_CAP_ITEMS
}

fn default_writer_queue_budget_bytes() -> u64 {
    DEFAULT_WRITER_QUEUE_BUDGET_BYTES
}

fn default_writer_spool_max_bytes() -> u64 {
    DEFAULT_WRITER_SPOOL_MAX_BYTES
}

impl Default for WriterDocument {
    fn default() -> Self {
        Self {
            batch_items: default_writer_batch_items(),
            idle_timeout_ms: default_writer_idle_timeout_ms(),
            checkpoint_interval_secs: default_writer_checkpoint_interval_secs(),
            queue_cap_items: default_writer_queue_cap_items(),
            queue_budget_bytes: default_writer_queue_budget_bytes(),
            spool_max_bytes: default_writer_spool_max_bytes(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriterDocument {
    #[serde(default = "default_writer_batch_items")]
    batch_items: u32,
    #[serde(default = "default_writer_idle_timeout_ms")]
    idle_timeout_ms: u64,
    #[serde(default = "default_writer_checkpoint_interval_secs")]
    checkpoint_interval_secs: u64,
    #[serde(default = "default_writer_queue_cap_items")]
    queue_cap_items: u32,
    #[serde(default = "default_writer_queue_budget_bytes")]
    queue_budget_bytes: u64,
    #[serde(default = "default_writer_spool_max_bytes")]
    spool_max_bytes: u64,
}

/// The `[payload]` section.
///
/// Both historical keys are **deprecated and no longer honored**: captured
/// plaintext is retained in full and no per-trace or per-segment budget is
/// enforced any more. The keys are still *accepted* so existing operator files
/// keep loading (the section is `deny_unknown_fields`, so dropping the fields
/// outright would make every deployed config fail validation), and they are
/// deliberately `skip_serializing` so a freshly generated config never
/// contains them again.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PayloadDocument {
    /// Deprecated: ignored. Formerly the per-trace retained-payload budget.
    #[serde(default, rename = "max_trace_bytes", skip_serializing)]
    _deprecated_max_trace_bytes: Option<u64>,
    /// Deprecated: ignored. Formerly the per-segment payload cap.
    #[serde(default, rename = "max_segment_bytes", skip_serializing)]
    _deprecated_max_segment_bytes: Option<u64>,
}

impl PayloadDocument {
    /// Whether the document holds no serializable content, so `dump()` can omit
    /// the whole section for newly generated files.
    fn is_empty(&self) -> bool {
        self._deprecated_max_trace_bytes.is_none() && self._deprecated_max_segment_bytes.is_none()
    }

    /// Keys an operator supplied that are accepted but no longer honored.
    fn deprecated_keys(&self) -> Vec<String> {
        let mut keys = Vec::new();
        if self._deprecated_max_trace_bytes.is_some() {
            keys.push("payload.max_trace_bytes".to_string());
        }
        if self._deprecated_max_segment_bytes.is_some() {
            keys.push("payload.max_segment_bytes".to_string());
        }
        keys
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionDocument {
    env_name: String,
}

impl Default for SessionDocument {
    fn default() -> Self {
        Self {
            env_name: DEFAULT_SESSION_ENV_NAME.to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DaemonDocument {
    socket_path: PathBuf,
    socket_mode: u32,
    control_pending_connection_max: u32,
    active_trace_max: u32,
    pid_file: PathBuf,
    log_path: PathBuf,
    startup_wait_ms: u64,
    shutdown_wait_ms: u64,
    supervision_poll_interval_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageDocument {
    backend: String,
    path: PathBuf,
    busy_timeout_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileDocument {
    name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EbpfDocument {
    enabled: String,
    memlock_rlimit: String,
    tracked_process_max_entries: u32,
    pending_operation_max_entries: u32,
    event_ring_buffer_max_bytes: u32,
    #[serde(default)]
    tls_dynamic_loading: bool,
    #[serde(default = "default_tls_scan_debounce")]
    tls_scan_debounce: String,
    #[serde(default = "default_tls_scan_interval_ms")]
    tls_scan_interval_ms: u64,
}

fn default_tls_scan_debounce() -> String {
    "image".to_string()
}

fn default_tls_scan_interval_ms() -> u64 {
    250
}

impl Default for OperatorDocument {
    fn default() -> Self {
        Self {
            daemon: DaemonDocument {
                socket_path: "/run/censorscope/censorscoped.sock".into(),
                socket_mode: DEFAULT_SOCKET_MODE,
                control_pending_connection_max: DEFAULT_CONTROL_PENDING_CONNECTION_MAX,
                active_trace_max: DEFAULT_ACTIVE_TRACE_MAX,
                pid_file: "/run/censorscope/censorscoped.pid".into(),
                log_path: "/var/log/censorscope/censorscoped.log".into(),
                startup_wait_ms: 5_000,
                shutdown_wait_ms: 10_000,
                supervision_poll_interval_ms: 50,
            },
            storage: StorageDocument {
                backend: "sqlite".to_string(),
                path: "/var/lib/censorscope/censorscope.sqlite".into(),
                busy_timeout_ms: 5_000,
            },
            profile: ProfileDocument {
                name: "process-lifecycle".to_string(),
            },
            ebpf: EbpfDocument {
                enabled: "auto".to_string(),
                memlock_rlimit: "unlimited".to_string(),
                // Raised for the dsh integration profile: one trace spans the dsh
                // main process plus every worker/tool process tree.
                tracked_process_max_entries: 262_144,
                pending_operation_max_entries: 262_144,
                // Sized for plaintext capture: one TLS record is ~4 KiB, so the
                // buffer holds only about 15k of them at 64 MiB, which a fast
                // target can produce within a second.
                event_ring_buffer_max_bytes: 256 * 1024 * 1024,
                tls_dynamic_loading: false,
                tls_scan_debounce: default_tls_scan_debounce(),
                tls_scan_interval_ms: default_tls_scan_interval_ms(),
            },
            session: SessionDocument::default(),
            payload: PayloadDocument::default(),
            writer: WriterDocument::default(),
        }
    }
}

impl OperatorConfig {
    /// Load and validate an operator configuration from disk.
    pub fn load(path: &Path) -> Result<Self, String> {
        let raw = fs::read_to_string(path)
            .map_err(|error| format!("read config {}: {error}", path.display()))?;
        Self::parse(&raw)
    }

    /// Build the default CensorScope configuration used by `init`.
    pub fn init() -> Result<Self, String> {
        Self::from_document(OperatorDocument::default())
    }

    /// Parse and validate the supported TOML configuration schema.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let document =
            toml::from_str(raw).map_err(|error| format!("parse operator config: {error}"))?;
        Self::from_document(document)
    }

    /// Apply a TOML patch file and validate the merged configuration.
    pub fn patch_file(&self, patch_path: &Path) -> Result<Self, String> {
        let patch = fs::read_to_string(patch_path)
            .map_err(|error| format!("read config patch {}: {error}", patch_path.display()))?;
        self.patch(&patch)
    }

    pub fn patch(&self, patch: &str) -> Result<Self, String> {
        let mut base: toml::Value = toml::from_str(&self.dump()?)
            .map_err(|error| format!("parse generated config: {error}"))?;
        let patch: toml::Value = toml::from_str(patch)
            .map_err(|error| format!("parse operator config patch: {error}"))?;
        merge_toml(&mut base, patch);
        Self::parse(&toml::to_string_pretty(&base).map_err(|error| error.to_string())?)
    }

    /// Serialize the validated configuration as operator-facing TOML.
    pub fn dump(&self) -> Result<String, String> {
        toml::to_string_pretty(&self.to_document()).map_err(|error| error.to_string())
    }

    /// Write the configuration, optionally replacing an existing file.
    pub fn dump_to_path(&self, path: &Path, overwrite: bool) -> Result<(), String> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| {
                format!("create config directory {}: {error}", parent.display())
            })?;
        }
        let mut options = OpenOptions::new();
        options.write(true);
        if overwrite {
            options.create(true).truncate(true);
        } else {
            options.create_new(true);
        }
        let mut file = options
            .open(path)
            .map_err(|error| format!("write config {}: {error}", path.display()))?;
        file.write_all(self.dump()?.as_bytes())
            .map_err(|error| format!("write config {}: {error}", path.display()))
    }

    fn from_document(document: OperatorDocument) -> Result<Self, String> {
        if document.storage.backend != "sqlite" {
            return Err("storage.backend must be sqlite".to_string());
        }
        let enabled_mode = document.ebpf.enabled.parse()?;
        let enabled = matches!(enabled_mode, EbpfEnabledMode::True);
        let tls_scan_debounce = document.ebpf.tls_scan_debounce.parse()?;
        if document.ebpf.tls_scan_interval_ms == 0 {
            return Err("ebpf.tls_scan_interval_ms must be greater than zero".to_string());
        }
        if document.writer.batch_items == 0 {
            return Err("writer.batch_items must be greater than zero".to_string());
        }
        if document.writer.idle_timeout_ms == 0 {
            return Err("writer.idle_timeout_ms must be greater than zero".to_string());
        }
        Ok(Self {
            socket_path: document.daemon.socket_path,
            socket_permissions: SocketPermissions {
                mode: document.daemon.socket_mode,
            },
            control_pending_connection_max: document.daemon.control_pending_connection_max,
            active_trace_max: document.daemon.active_trace_max,
            pid_file: document.daemon.pid_file,
            log_path: document.daemon.log_path,
            storage: StorageConfig::sqlite(document.storage.path, document.storage.busy_timeout_ms),
            capture_profile: CaptureProfile {
                name: model_core::ids::ProfileName::new(document.profile.name),
                // The operator document predates collection levels and never
                // selected capabilities; the daemon overrides the run-time
                // profile from `--level` (default L1) at start. The L3 list is
                // the full set and keeps the parsed document self-consistent.
                capabilities: CaptureLevel::L3
                    .capabilities()
                    .iter()
                    .copied()
                    .map(model_core::capability::CapabilityRequest::required)
                    .collect(),
            },
            ebpf_config: EbpfCollectorConfig {
                enabled_mode,
                enabled,
                memlock_rlimit: document.ebpf.memlock_rlimit.parse()?,
                tracked_process_max_entries: document.ebpf.tracked_process_max_entries,
                pending_operation_max_entries: document.ebpf.pending_operation_max_entries,
                event_ring_buffer_max_bytes: document.ebpf.event_ring_buffer_max_bytes,
                tls_dynamic_loading: document.ebpf.tls_dynamic_loading,
                tls_scan_debounce,
                tls_scan_interval_ms: document.ebpf.tls_scan_interval_ms,
            },
            startup_wait_ms: document.daemon.startup_wait_ms,
            shutdown_wait_ms: document.daemon.shutdown_wait_ms,
            supervision_poll_interval_ms: document.daemon.supervision_poll_interval_ms,
            session_env_name: document.session.env_name,
            writer: WriterConfig {
                batch_items: document.writer.batch_items,
                idle_timeout_ms: document.writer.idle_timeout_ms,
                checkpoint_interval_secs: document.writer.checkpoint_interval_secs,
                queue_cap_items: document.writer.queue_cap_items,
                queue_budget_bytes: document.writer.queue_budget_bytes,
                spool_max_bytes: document.writer.spool_max_bytes,
            },
            deprecated_keys: document.payload.deprecated_keys(),
        })
    }

    fn to_document(&self) -> OperatorDocument {
        OperatorDocument {
            daemon: DaemonDocument {
                socket_path: self.socket_path.clone(),
                socket_mode: self.socket_permissions.mode,
                control_pending_connection_max: self.control_pending_connection_max,
                active_trace_max: self.active_trace_max,
                pid_file: self.pid_file.clone(),
                log_path: self.log_path.clone(),
                startup_wait_ms: self.startup_wait_ms,
                shutdown_wait_ms: self.shutdown_wait_ms,
                supervision_poll_interval_ms: self.supervision_poll_interval_ms,
            },
            storage: StorageDocument {
                backend: "sqlite".to_string(),
                path: self.storage.path().to_path_buf(),
                busy_timeout_ms: self.storage.sqlite_busy_timeout_ms(),
            },
            profile: ProfileDocument {
                name: self.capture_profile.name.to_string(),
            },
            ebpf: EbpfDocument {
                enabled: self.ebpf_config.enabled_mode.to_string(),
                memlock_rlimit: self.ebpf_config.memlock_rlimit.to_string(),
                tracked_process_max_entries: self.ebpf_config.tracked_process_max_entries,
                pending_operation_max_entries: self.ebpf_config.pending_operation_max_entries,
                event_ring_buffer_max_bytes: self.ebpf_config.event_ring_buffer_max_bytes,
                tls_dynamic_loading: self.ebpf_config.tls_dynamic_loading,
                tls_scan_debounce: self.ebpf_config.tls_scan_debounce.to_string(),
                tls_scan_interval_ms: self.ebpf_config.tls_scan_interval_ms,
            },
            session: SessionDocument {
                env_name: self.session_env_name.clone(),
            },
            // Generated files never carry the removed payload keys; the field
            // stays only so deployed files that still contain them keep parsing.
            payload: PayloadDocument::default(),
            writer: WriterDocument {
                batch_items: self.writer.batch_items,
                idle_timeout_ms: self.writer.idle_timeout_ms,
                checkpoint_interval_secs: self.writer.checkpoint_interval_secs,
                queue_cap_items: self.writer.queue_cap_items,
                queue_budget_bytes: self.writer.queue_budget_bytes,
                spool_max_bytes: self.writer.spool_max_bytes,
            },
        }
    }
}

fn merge_toml(base: &mut toml::Value, patch: toml::Value) {
    match (base, patch) {
        (toml::Value::Table(base), toml::Value::Table(patch)) => {
            for (key, value) in patch {
                match base.get_mut(&key) {
                    Some(base_value) if base_value.is_table() && value.is_table() => {
                        merge_toml(base_value, value)
                    }
                    _ => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, patch) => *base = patch,
    }
}

#[cfg(test)]
mod tests {
    use super::OperatorConfig;

    #[test]
    fn default_config_round_trips() {
        let config = OperatorConfig::init().unwrap();
        assert_eq!(
            OperatorConfig::parse(&config.dump().unwrap()).unwrap(),
            config
        );
        assert_eq!(config.socket_permissions.mode, super::DEFAULT_SOCKET_MODE);
    }

    #[test]
    fn rejects_removed_sections() {
        let mut raw = OperatorConfig::init().unwrap().dump().unwrap();
        raw.push_str("\n[payload]\nenabled = true\n");
        assert!(OperatorConfig::parse(&raw).is_err());
    }

    #[test]
    fn generated_config_omits_the_removed_payload_limits() {
        let config = OperatorConfig::init().unwrap();
        assert!(config.deprecated_keys.is_empty());
        let dumped = config.dump().unwrap();
        assert!(!dumped.contains("max_trace_bytes"), "dump: {dumped}");
        assert!(!dumped.contains("max_segment_bytes"), "dump: {dumped}");
        assert!(!dumped.contains("[payload]"), "dump: {dumped}");
    }

    #[test]
    fn deprecated_payload_limits_still_load_but_are_not_honored() {
        // A deployed file that predates the removal must keep loading: the keys
        // are accepted, reported as deprecated, and never written back.
        let config = OperatorConfig::init()
            .unwrap()
            .patch("[payload]\nmax_trace_bytes = 12345\nmax_segment_bytes = 678\n")
            .unwrap();
        assert_eq!(
            config.deprecated_keys,
            vec![
                "payload.max_trace_bytes".to_string(),
                "payload.max_segment_bytes".to_string()
            ]
        );
        let dumped = config.dump().unwrap();
        assert!(!dumped.contains("max_trace_bytes"), "dump: {dumped}");
        assert!(!dumped.contains("max_segment_bytes"), "dump: {dumped}");
    }

    #[test]
    fn zero_trace_limit_is_accepted_as_a_deprecated_key() {
        let config = OperatorConfig::init()
            .unwrap()
            .patch("[payload]\nmax_trace_bytes = 0\n")
            .unwrap();
        assert_eq!(
            config.deprecated_keys,
            vec!["payload.max_trace_bytes".to_string()]
        );
    }

    #[test]
    fn tls_dynamic_loading_is_opt_in_and_debounce_is_configurable() {
        let config = OperatorConfig::init().unwrap();
        assert!(!config.ebpf_config.tls_dynamic_loading);
        assert_eq!(
            config.ebpf_config.tls_scan_debounce,
            super::TlsScanDebounce::Image
        );
        let patched = config
            .patch(
                "[ebpf]\ntls_dynamic_loading = true\ntls_scan_debounce = \"interval\"\ntls_scan_interval_ms = 1000\n",
            )
            .unwrap();
        assert!(patched.ebpf_config.tls_dynamic_loading);
        assert_eq!(
            patched.ebpf_config.tls_scan_debounce,
            super::TlsScanDebounce::Interval
        );
        assert_eq!(patched.ebpf_config.tls_scan_interval_ms, 1000);
    }

    #[test]
    fn writer_tuning_round_trips_and_validates() {
        let config = OperatorConfig::init().unwrap();
        assert_eq!(config.writer.batch_items, super::DEFAULT_WRITER_BATCH_ITEMS);
        assert_eq!(
            config.writer.idle_timeout_ms,
            super::DEFAULT_WRITER_IDLE_TIMEOUT_MS
        );
        assert_eq!(
            config.writer.queue_cap_items,
            super::DEFAULT_WRITER_QUEUE_CAP_ITEMS
        );
        let patched = config
            .patch(
                "[writer]\nbatch_items = 128\nidle_timeout_ms = 4\ncheckpoint_interval_secs = 30\nqueue_cap_items = 16384\n",
            )
            .unwrap();
        assert_eq!(patched.writer.batch_items, 128);
        assert_eq!(patched.writer.idle_timeout_ms, 4);
        assert_eq!(patched.writer.checkpoint_interval_secs, 30);
        assert_eq!(patched.writer.queue_cap_items, 16384);
        assert!(patched.dump().unwrap().contains("batch_items = 128"));
        // Idle timeout of zero would busy-spin the writer: rejected.
        assert!(config.patch("[writer]\nidle_timeout_ms = 0\n").is_err());
        assert!(config.patch("[writer]\nbatch_items = 0\n").is_err());
    }
}
