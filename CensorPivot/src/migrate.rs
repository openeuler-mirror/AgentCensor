//! Explicit, idempotent upgrade of the pre-0.5 Guard configuration layout.
use crate::{PivotError, Result};
use censorguard_common::config::ConfigDocument;
use serde_json::{Map, Value, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

fn invalid(message: impl Into<String>) -> PivotError {
    PivotError::Invalid(message.into())
}

/// Validate every document before saving anything. Existing rules and non-Guard
/// configuration are retained; unknown legacy options require operator review.
pub fn migrate_guard(daemon_path: &Path, pivot_path: &Path) -> Result<Vec<PathBuf>> {
    let mut daemon: Value = serde_json::from_slice(&fs::read(daemon_path)?)?;
    let mut pivot: Value = serde_json::from_slice(&fs::read(pivot_path)?)?;
    pivot
        .as_object_mut()
        .ok_or_else(|| invalid("Pivot config must be an object"))?
        .entry("supervisor_config")
        .or_insert(json!(daemon_path));
    let old = daemon
        .get("censorguard")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("censord config is missing censorguard"))?
        .clone();
    let config_path = old
        .get("config")
        .or_else(|| old.get("policy"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("Guard config/policy path is missing"))?;
    let config_path = PathBuf::from(config_path);
    if !config_path.is_absolute() {
        return Err(invalid("Guard config path must be absolute"));
    }
    let mut document: ConfigDocument =
        serde_yaml::from_slice(&fs::read(&config_path)?).map_err(|error| {
            invalid(format!(
                "{}: {error}; move dynamic groups out of startup YAML before migration",
                config_path.display()
            ))
        })?;
    let mut settings = serde_json::to_value(&document.daemon)?;
    let settings = settings
        .as_object_mut()
        .ok_or_else(|| invalid("invalid daemon settings"))?;
    let mut legacy = false;
    for (key, value) in &old {
        match key.as_str() {
            "daemon" | "control" | "config" => {}
            "policy" => {
                legacy = true;
            }
            "extra_args" => {
                legacy = true;
            }
            key if settings.contains_key(key) => {
                legacy = true;
                settings.insert(key.into(), value.clone());
            }
            key => {
                return Err(invalid(format!(
                    "unknown legacy Guard setting {key}; migrate it explicitly"
                )));
            }
        }
    }
    if let Some(args) = old.get("extra_args") {
        migrate_args(args, settings)?;
    }
    if let (Some(config), Some(policy)) = (old.get("config"), old.get("policy"))
        && config != policy
    {
        return Err(invalid("conflicting Guard config and policy paths"));
    }
    document.daemon = serde_json::from_value(Value::Object(settings.clone()))?;
    let control = old
        .get("control")
        .cloned()
        .unwrap_or(json!("/usr/bin/censorguardctl"));
    daemon["censorguard"] = json!({"daemon":old.get("daemon").cloned().unwrap_or(json!("/usr/sbin/censorguardd")),
        "control":control, "config":config_path});
    let guard = pivot
        .get_mut("censorguard")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| invalid("Pivot config is missing censorguard"))?;
    if let Some(socket) = guard.remove("launch_socket")
        && socket != json!(document.daemon.launch_socket)
    {
        return Err(invalid(
            "Pivot launch_socket differs from the supervisor; align the paths before migrating",
        ));
    }
    guard.remove("exec_command");
    if let Some(path) = guard.get("config")
        && path != &json!(config_path)
    {
        return Err(invalid(
            "Pivot and censord reference different Guard configurations",
        ));
    }
    guard.insert("config".into(), json!(config_path));
    guard.entry("control").or_insert(control);
    let supervisor = serde_json::from_value::<crate::daemon::DaemonConfig>(daemon.clone())?;
    supervisor.validate()?;
    let coordinator = serde_json::from_value::<crate::config::Config>(pivot.clone())?;
    coordinator.validate()?;
    coordinator.validate_supervisor(&supervisor)?;
    let yaml = serde_yaml::to_string(&document).map_err(|error| invalid(error.to_string()))?;
    if document.daemon.dns_refresh_seconds == 0 {
        return Err(invalid("dns_refresh_seconds must be positive"));
    }
    let mut plans = Vec::new();
    if legacy {
        plans.push((config_path, yaml.into_bytes()));
    }
    plans.push((daemon_path.to_owned(), json_bytes(&daemon)?));
    plans.push((pivot_path.to_owned(), json_bytes(&pivot)?));
    let mut changed = Vec::new();
    for (path, bytes) in plans {
        if fs::read(&path)? != bytes {
            changed.push((path, bytes));
        }
    }
    let plans = changed;
    // Back up all affected files before the first replacement. A failed write is
    // recoverable by rerunning migration or restoring the retained originals.
    for (path, _) in &plans {
        backup(path)?;
    }
    for (path, bytes) in &plans {
        replace(path, bytes)?;
    }
    Ok(plans.into_iter().map(|(path, _)| path).collect())
}

fn json_bytes(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn migrate_args(value: &Value, settings: &mut Map<String, Value>) -> Result<()> {
    let args = value
        .as_array()
        .ok_or_else(|| invalid("extra_args must be an array"))?;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index]
            .as_str()
            .ok_or_else(|| invalid("extra_args must contain strings"))?;
        let key = arg
            .strip_prefix("--")
            .ok_or_else(|| invalid(format!("unsupported Guard option {arg}")))?
            .replace('-', "_");
        if !settings.contains_key(&key) {
            return Err(invalid(format!(
                "unsupported legacy Guard option {arg}; migrate it explicitly"
            )));
        }
        if settings[&key].is_boolean() {
            let value = args.get(index + 1).and_then(Value::as_str);
            let enabled = match value {
                Some("true") => {
                    index += 1;
                    true
                }
                Some("false") => {
                    index += 1;
                    false
                }
                _ => true,
            };
            settings.insert(key, json!(enabled));
        } else {
            index += 1;
            let value = args
                .get(index)
                .and_then(Value::as_str)
                .ok_or_else(|| invalid(format!("missing value for {arg}")))?;
            let parsed = match key.as_str() {
                "dns_refresh_seconds" => json!(
                    value
                        .parse::<u64>()
                        .map_err(|_| invalid("invalid DNS refresh interval"))?
                ),
                "dsh_self_groups" => json!(value.split(',').map(str::to_owned).collect::<Vec<_>>()),
                _ => json!(value),
            };
            settings.insert(key, parsed);
        }
        index += 1;
    }
    Ok(())
}

fn backup(path: &Path) -> Result<()> {
    let backup = PathBuf::from(format!("{}.pre-guard-0.5.0.bak", path.display()));
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(fs::metadata(path)?.mode())
        .open(&backup)
    {
        Ok(mut file) => {
            file.write_all(&fs::read(path)?)?;
            file.sync_all()?;
            fs::set_permissions(&backup, fs::metadata(path)?.permissions())?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    let metadata = fs::metadata(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| invalid("config has no parent"))?;
    let temporary = parent.join(format!(".guard-migrate-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(metadata.mode())
            .open(&temporary)?;
        fs::set_permissions(&temporary, metadata.permissions())?;
        if nix::unistd::geteuid().is_root() {
            nix::unistd::chown(
                &temporary,
                Some(nix::unistd::Uid::from_raw(metadata.uid())),
                Some(nix::unistd::Gid::from_raw(metadata.gid())),
            )
            .map_err(std::io::Error::from)?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
