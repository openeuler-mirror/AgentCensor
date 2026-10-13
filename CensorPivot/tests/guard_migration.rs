use censorpivot::migrate::migrate_guard;
use serde_json::json;
use std::fs;
use std::path::PathBuf;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture(
    root: &std::path::Path,
) -> Result<(PathBuf, PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let daemon_path = root.join("censord.json");
    let pivot_path = root.join("pivot.json");
    let yaml_path = root.join("guard.yaml");
    fs::write(&yaml_path, "rules:\n  - file deny+audit /etc/shadow\n")?;
    fs::write(
        &daemon_path,
        serde_json::to_vec(&json!({"censorguard":{
        "policy":yaml_path, "launch_socket":root.join("launch.sock"), "state_dir":root.join("state"),
        "extra_args":["--socket-group", "operators", "--gateway-user", "operator", "--spawn-grpc", "--grpc-bin", "/custom/grpc", "--audit-exec", "false"]
    }, "poll_interval_ms":17}))?,
    )?;
    fs::write(
        &pivot_path,
        serde_json::to_vec(&json!({"censorguard":{
            "launch_socket":root.join("launch.sock"), "exec_command":"/usr/bin/censorguard-exec", "allowed_groups":["custom"]
        }}))?,
    )?;
    Ok((daemon_path, pivot_path, yaml_path))
}

#[test]
fn upgrade_retains_rules_custom_paths_and_permissions_and_is_idempotent() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let temporary = tempfile::tempdir()?;
    let (daemon, pivot, yaml) = fixture(temporary.path())?;
    fs::set_permissions(&yaml, fs::Permissions::from_mode(0o640))?;
    let old = fs::read(&yaml)?;
    assert_eq!(migrate_guard(&daemon, &pivot)?.len(), 3);
    let parsed: censorguard_common::config::ConfigDocument =
        serde_yaml::from_slice(&fs::read(&yaml)?)?;
    assert_eq!(parsed.rules, ["file deny+audit /etc/shadow"]);
    assert_eq!(
        parsed.daemon.launch_socket,
        temporary.path().join("launch.sock")
    );
    assert_eq!(parsed.daemon.socket_group.as_deref(), Some("operators"));
    assert!(parsed.daemon.spawn_grpc);
    assert!(!parsed.daemon.audit_exec);
    assert_eq!(parsed.daemon.grpc_bin, Some(PathBuf::from("/custom/grpc")));
    assert_eq!(fs::metadata(&yaml)?.permissions().mode() & 0o777, 0o640);
    assert_eq!(
        fs::read(format!("{}.pre-guard-0.5.0.bak", yaml.display()))?,
        old
    );
    assert_eq!(
        censorpivot::daemon::DaemonConfig::load(&daemon)?.poll_interval_ms,
        17
    );
    assert_eq!(
        censorpivot::config::Config::load(&pivot)?
            .censorguard
            .allowed_groups,
        ["custom"]
    );
    assert!(migrate_guard(&daemon, &pivot)?.is_empty());
    Ok(())
}

#[test]
fn unsupported_legacy_options_leave_all_files_untouched() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let (daemon, pivot, yaml) = fixture(temporary.path())?;
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&daemon)?)?;
    config["censorguard"]["extra_args"] = json!(["--unknown-custom-option"]);
    fs::write(&daemon, serde_json::to_vec(&config)?)?;
    let before = [fs::read(&daemon)?, fs::read(&pivot)?, fs::read(&yaml)?];
    assert!(migrate_guard(&daemon, &pivot).is_err());
    assert_eq!(
        before,
        [fs::read(&daemon)?, fs::read(&pivot)?, fs::read(&yaml)?]
    );
    assert_eq!(fs::read_dir(temporary.path())?.count(), 3);
    Ok(())
}
