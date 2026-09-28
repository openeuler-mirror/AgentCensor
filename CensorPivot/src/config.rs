use crate::{PivotError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
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
    pub launch_socket: PathBuf,
    pub exec_command: PathBuf,
    pub allowed_groups: Vec<String>,
}

impl Default for CensorGuardConfig {
    fn default() -> Self {
        Self {
            launch_socket: "/run/censorguard/launch.sock".into(),
            exec_command: "/usr/local/bin/censorguard-exec".into(),
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
            command: "/usr/local/bin/censorscopectl".into(),
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
        Ok(config)
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
            || self.censorguard.allowed_groups.is_empty()
            || self
                .censorguard
                .allowed_groups
                .iter()
                .any(|group| group.is_empty())
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
        Config::load(&path).map(|_| ())
    }
}
