//! Configuration shared by every CensorGuard executable.
//!
//! The policy document remains the source of truth for rules.  The optional
//! `daemon` section is deliberately defined here so clients do not each grow
//! their own socket/path flags.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_CONFIG_PATH: &str = "/etc/censorguard/base.yaml";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureMode {
    Permissive,
    #[default]
    Strict,
}

impl std::fmt::Display for FailureMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Permissive => "permissive",
            Self::Strict => "strict",
        })
    }
}

impl std::str::FromStr for FailureMode {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "permissive" => Ok(Self::Permissive),
            "strict" => Ok(Self::Strict),
            _ => Err("failure mode must be permissive or strict"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    pub bpf_object: PathBuf,
    pub control_socket: PathBuf,
    pub event_socket: PathBuf,
    pub event_forward_socket: Option<PathBuf>,
    pub launch_socket: PathBuf,
    pub dsh_socket: PathBuf,
    pub ui_socket: PathBuf,
    pub state_dir: PathBuf,
    pub socket_group: Option<String>,
    pub gateway_user: Option<String>,
    pub dsh_enabled: bool,
    pub dsh_self_groups: Vec<String>,
    pub dns_refresh_seconds: u64,
    pub spawn_grpc: bool,
    pub grpc_bin: Option<PathBuf>,
    pub grpc_listen: Option<String>,
    pub enable_file: bool,
    pub enable_exec: bool,
    pub enable_net: bool,
    pub audit_file: bool,
    pub audit_exec: bool,
    pub audit_net: bool,
    pub failure_mode: FailureMode,
    pub self_protection: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            bpf_object: PathBuf::from("/usr/lib/censorguard/enforce.bpf.o"),
            control_socket: PathBuf::from("/run/censorguard/ctl.sock"),
            event_socket: PathBuf::from("/run/censorguard/events.sock"),
            event_forward_socket: None,
            launch_socket: PathBuf::from("/run/censorguard/launch.sock"),
            dsh_socket: PathBuf::from("/run/censorguard/dsh.sock"),
            ui_socket: PathBuf::from("/run/censorguard/ui.sock"),
            state_dir: PathBuf::from("/var/lib/censorguard"),
            socket_group: None,
            gateway_user: None,
            dsh_enabled: true,
            dsh_self_groups: vec!["censorguard-dsh-default".to_owned()],
            dns_refresh_seconds: 60,
            spawn_grpc: false,
            grpc_bin: None,
            grpc_listen: None,
            enable_file: true,
            enable_exec: true,
            enable_net: true,
            audit_file: false,
            audit_exec: false,
            audit_net: false,
            failure_mode: FailureMode::default(),
            self_protection: true,
        }
    }
}

/// Public startup input contains only daemon settings and immutable base rules.
/// Dynamic group definitions and domain bindings are managed through the control API.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConfigDocument {
    pub daemon: DaemonConfig,
    pub rules: Vec<String>,
}

pub fn load(path: impl AsRef<std::path::Path>) -> Result<DaemonConfig, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let document: ConfigDocument = serde_yaml::from_slice(&bytes)?;
    if document.daemon.dns_refresh_seconds == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "daemon.dns_refresh_seconds must be positive",
        )
        .into());
    }
    Ok(document.daemon)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_failure_mode_is_strict_with_self_protection() -> Result<(), serde_yaml::Error> {
        let document: ConfigDocument = serde_yaml::from_str("rules: []")?;
        assert_eq!(document.daemon.failure_mode, FailureMode::Strict);
        assert!(document.daemon.self_protection);
        assert!(document.daemon.dsh_enabled);
        assert!(
            !document.daemon.audit_file
                && !document.daemon.audit_exec
                && !document.daemon.audit_net
        );
        let permissive: ConfigDocument =
            serde_yaml::from_str("daemon:\n  failure_mode: permissive\nrules: []")?;
        assert_eq!(permissive.daemon.failure_mode, FailureMode::Permissive);
        let strict: ConfigDocument = serde_yaml::from_str(
            "daemon:\n  failure_mode: strict\n  self_protection: true\nrules: []",
        )?;
        assert_eq!(strict.daemon.failure_mode, FailureMode::Strict);
        assert!(strict.daemon.self_protection);
        let unprotected: ConfigDocument =
            serde_yaml::from_str("daemon:\n  self_protection: false\nrules: []")?;
        assert!(!unprotected.daemon.self_protection);
        assert!(
            serde_yaml::from_str::<ConfigDocument>("daemon:\n  failure_mode: permissve").is_err()
        );
        Ok(())
    }

    #[test]
    fn public_config_rejects_group_and_domain_sections() -> Result<(), serde_yaml::Error> {
        for yaml in ["groups: {}", "domains: []", "enable_file: false"] {
            assert!(serde_yaml::from_str::<ConfigDocument>(yaml).is_err());
        }
        let document: ConfigDocument =
            serde_yaml::from_str("daemon:\n  enable_net: false\n  audit_exec: true\nrules: []\n")?;
        assert!(!document.daemon.enable_net);
        assert!(document.daemon.audit_exec);
        assert!(document.daemon.enable_file);
        Ok(())
    }

    #[test]
    fn rejects_unknown_daemon_setting() {
        let parsed = serde_yaml::from_str::<ConfigDocument>(
            "daemon:\n  control_socket: /tmp/ctl.sock\n  unsupported: true\n",
        );
        assert!(matches!(parsed, Err(error) if error.to_string().contains("unsupported")));
    }
}
