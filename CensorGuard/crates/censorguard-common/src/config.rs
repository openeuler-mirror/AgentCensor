//! Configuration shared by every CensorGuard executable.
//!
//! The policy document remains the source of truth for rules.  The optional
//! `daemon` section is deliberately defined here so clients do not each grow
//! their own socket/path flags.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_CONFIG_PATH: &str = "/etc/censorguard/base.yaml";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    pub bpf_object: PathBuf,
    pub control_socket: PathBuf,
    pub event_socket: PathBuf,
    pub launch_socket: PathBuf,
    pub dsh_socket: PathBuf,
    pub ui_socket: PathBuf,
    pub state_dir: PathBuf,
    pub socket_group: Option<String>,
    pub gateway_user: Option<String>,
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
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            bpf_object: PathBuf::from("/usr/lib/censorguard/enforce.bpf.o"),
            control_socket: PathBuf::from("/run/censorguard/ctl.sock"),
            event_socket: PathBuf::from("/run/censorguard/events.sock"),
            launch_socket: PathBuf::from("/run/censorguard/launch.sock"),
            dsh_socket: PathBuf::from("/run/censorguard/dsh.sock"),
            ui_socket: PathBuf::from("/run/censorguard/ui.sock"),
            state_dir: PathBuf::from("/var/lib/censorguard"),
            socket_group: None,
            gateway_user: None,
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
