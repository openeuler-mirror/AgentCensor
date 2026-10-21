#![cfg(target_os = "linux")]
use censorprivacy::control;
use std::{fs, os::unix::fs::PermissionsExt, process::Command};
#[test]
fn dry_run_and_internal_bootstrap_never_execute_unisolated_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_privacy-run"))
        .args([
            "--run-id",
            "dry-run",
            "--workspace",
            "/tmp/cp-example",
            "--uid",
            "65534",
            "--gid",
            "65534",
            "--gateway-port",
            "8765",
            "--env",
            "HOME=private-path-marker",
            "--dry-run",
            "--",
            "/bin/false",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["seccomp_required"], true);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-path-marker"));
    let output = Command::new(env!("CARGO_BIN_EXE_privacy-run"))
        .args(["--internal-bootstrap", "0", "1"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("runtime_blocked"));
}
#[test]
fn private_controls_reject_symlinks_public_modes_nonfiles_and_oversize() {
    let directory = std::env::temp_dir().join(format!("cp-controls-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("token");
    fs::write(&path, b"private-control-token-0123456789").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(control::token(&path, censorprivacy_runtime_sys::effective_uid()).is_ok());
    let link = directory.join("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(control::token(&link, 0).is_err());
    assert!(control::private_file(&directory, 0, 8192).is_err());
    assert!(control::private_file(&path, 0, 1).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(control::token(&path, 0).is_err());
    fs::remove_dir_all(directory).unwrap();
}
