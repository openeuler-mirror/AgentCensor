use crate::{PivotError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub supervisor_config: Option<PathBuf>,
    pub socket_path: PathBuf,
    pub state_dir: PathBuf,
    pub socket_mode: u32,
    pub max_frame_bytes: usize,
    pub max_calls_per_batch: usize,
    pub max_output_bytes: usize,
    pub component_timeout_ms: u64,
    pub runner_timeout_ms: u64,
    pub censorfs: CensorFsConfig,
    pub censorguard: CensorGuardConfig,
    pub censorscope: CensorScopeConfig,
    pub runner_environment: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            supervisor_config: None,
            socket_path: "/run/censorpivot/control.sock".into(),
            state_dir: "/var/lib/censorpivot/transactions".into(),
            socket_mode: 0o660,
            max_frame_bytes: 1024 * 1024,
            max_calls_per_batch: 64,
            max_output_bytes: 256 * 1024,
            component_timeout_ms: 60_000,
            runner_timeout_ms: 15_000,
            censorfs: CensorFsConfig::default(),
            censorguard: CensorGuardConfig::default(),
            censorscope: CensorScopeConfig::default(),
            runner_environment: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CensorFsConfig {
    pub command: PathBuf,
    pub socket: PathBuf,
    pub mounter_command: Vec<String>,
    pub prepare_timeout_ms: u64,
    pub max_diff_file_bytes: u64,
    pub cgroup_root: PathBuf,
    pub cgroup_state_dir: PathBuf,
}

impl Default for CensorFsConfig {
    fn default() -> Self {
        Self {
            command: "/usr/local/bin/censorfs".into(),
            socket: "/run/censorfs/control.sock".into(),
            mounter_command: vec!["/usr/libexec/censorfs/censorfs-mounter".into()],
            prepare_timeout_ms: 30_000,
            max_diff_file_bytes: 256 * 1024,
            cgroup_root: "/sys/fs/cgroup/censorpivot".into(),
            cgroup_state_dir: "/run/censorpivot-cgroups".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CensorGuardConfig {
    pub config: PathBuf,
    pub control: PathBuf,
    pub allowed_groups: Vec<String>,
}

impl Default for CensorGuardConfig {
    fn default() -> Self {
        Self {
            config: "/etc/censorguard/agentcensor.yaml".into(),
            control: "/usr/bin/censorguardctl".into(),
            allowed_groups: vec!["censorguard-dsh-default".into()],
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CensorScopeConfig {
    pub command: PathBuf,
    pub socket: PathBuf,
    pub operator_config: PathBuf,
    pub required: bool,
}

impl Default for CensorScopeConfig {
    fn default() -> Self {
        Self {
            command: "/usr/bin/censorscopectl".into(),
            socket: "/run/censorscope/censorscoped.sock".into(),
            operator_config: "/etc/censorscope/censorscoped.conf".into(),
            required: false,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)?;
        let config: Self = serde_json::from_slice(&bytes)?;
        config.validate()?;
        if let Some(supervisor_path) = &config.supervisor_config {
            let supervisor = crate::daemon::DaemonConfig::load(supervisor_path)?;
            config.validate_supervisor(&supervisor)?;
        }
        Ok(config)
    }

    /// Unified deployment must use the endpoints and policy sources actually
    /// launched by censord. Refuse a split configuration before accepting work.
    pub fn validate_supervisor(&self, supervisor: &crate::daemon::DaemonConfig) -> Result<()> {
        for (name, actual, expected) in [
            (
                "censorfs.socket",
                &self.censorfs.socket,
                &supervisor.censorfs.socket,
            ),
            (
                "censorguard.config",
                &self.censorguard.config,
                &supervisor.censorguard.config,
            ),
            (
                "censorguard.control",
                &self.censorguard.control,
                &supervisor.censorguard.control,
            ),
            (
                "censorscope.socket",
                &self.censorscope.socket,
                &supervisor.censorscope.socket,
            ),
            (
                "censorscope.operator_config",
                &self.censorscope.operator_config,
                &supervisor.censorscope.config,
            ),
            (
                "censorscope.command",
                &self.censorscope.command,
                &supervisor.censorscope.control,
            ),
        ] {
            if actual != expected {
                return Err(PivotError::Invalid(format!(
                    "{name}={} differs from supervisor configuration {}",
                    actual.display(),
                    expected.display()
                )));
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.max_frame_bytes == 0 || self.max_calls_per_batch == 0 || self.max_output_bytes == 0
        {
            return Err(PivotError::Invalid(
                "configured limits must be greater than zero".into(),
            ));
        }
        if self.censorfs.mounter_command.is_empty() {
            return Err(PivotError::Invalid(
                "censorfs.mounter_command must not be empty".into(),
            ));
        }
        if !(1..=300_000).contains(&self.component_timeout_ms)
            || !(1..=300_000).contains(&self.runner_timeout_ms)
            || self.max_output_bytes > 1024 * 1024
            || self.max_frame_bytes > 16 * 1024 * 1024
            || !self.censorfs.cgroup_root.is_absolute()
            || !self.censorfs.cgroup_state_dir.is_absolute()
            || !self.censorguard.config.is_absolute()
            || !self.censorguard.control.is_absolute()
            || self
                .supervisor_config
                .as_ref()
                .is_some_and(|path| !path.is_absolute())
            || self.censorguard.allowed_groups.is_empty()
            || self
                .censorguard
                .allowed_groups
                .iter()
                .any(|group| !crate::guard::valid_name(group))
        {
            return Err(PivotError::Invalid(
                "invalid I/O limits, cgroup paths or allowed Guard groups".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_is_valid() -> Result<()> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config.example.json");
        let config: Config = serde_json::from_slice(&fs::read(path)?)?;
        config.validate()?;
        let supervisor: crate::daemon::DaemonConfig =
            serde_json::from_str(include_str!("../censord.example.json"))?;
        config.validate_supervisor(&supervisor)
    }

    #[test]
    fn mismatched_component_configuration_is_rejected() -> Result<()> {
        let config: Config = serde_json::from_str(include_str!("../config.example.json"))?;
        let supervisor: crate::daemon::DaemonConfig =
            serde_json::from_str(include_str!("../censord.example.json"))?;
        for field in 0..6 {
            let mut changed = config.clone();
            let path = PathBuf::from("/tmp/other-component");
            match field {
                0 => changed.censorfs.socket = path,
                1 => changed.censorguard.config = path,
                2 => changed.censorguard.control = path,
                3 => changed.censorscope.socket = path,
                4 => changed.censorscope.operator_config = path,
                _ => changed.censorscope.command = path,
            }
            assert!(changed.validate_supervisor(&supervisor).is_err());
        }
        Ok(())
    }
}
