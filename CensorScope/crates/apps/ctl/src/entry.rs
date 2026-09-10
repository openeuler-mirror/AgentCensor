//! Top-level entry boundary for the control application.

use std::path::Path;

use config_core::daemon::{OperatorConfig, OperatorConfigInitStatus};
use uds_control_client::{UdsControlClient, UdsSocketTransport};

use crate::args::{CtlCommand, parse_args};
use crate::dispatch::dispatch;
use crate::export::export_to_path;
use crate::output::{format_error_json, format_init_json, format_reply, format_reply_json};

/// Run ctl from process arguments and print its user-facing response.
pub fn run_from_env() -> Result<i32, String> {
    let invocation = parse_args(std::env::args().skip(1))?;
    let json_mode = invocation.json;
    match invocation.command {
        CtlCommand::Init {
            config_path,
            force,
            patch_path,
        } => match initialize_operator_config_file(&config_path, force, patch_path.as_deref()) {
            Err(error) if json_mode => {
                println!("{}", format_error_json("init", &error));
                return Ok(1);
            }
            Err(error) => return Err(error),
            Ok(status) if json_mode => {
                let status = match status {
                    OperatorConfigInitStatus::Created => "created",
                    OperatorConfigInitStatus::ExistingValid => "existing_valid",
                    OperatorConfigInitStatus::Overwritten => "overwritten",
                };
                println!("{}", format_init_json(status, &config_path));
                return Ok(0);
            }
            Ok(status) => match status {
                OperatorConfigInitStatus::Created => {
                    println!("initialized config {}", config_path.display());
                }
                OperatorConfigInitStatus::ExistingValid => {
                    println!(
                        "config {} already exists and is valid",
                        config_path.display()
                    );
                }
                OperatorConfigInitStatus::Overwritten => {
                    println!("overwrote config {}", config_path.display());
                }
            },
        },
        CtlCommand::Export {
            database,
            trace_id,
            session_id,
            call_id,
            out_path,
            full,
            no_internal,
            page_size,
            after_event,
        } => {
            let result = export_to_path(
                &database,
                trace_id.get(),
                &session_id,
                call_id.as_deref(),
                full,
                no_internal,
                page_size,
                after_event,
                &out_path,
            );
            if let Err(error) = result {
                if json_mode {
                    println!("{}", format_error_json("export", &error));
                    return Ok(1);
                }
                return Err(error);
            }
            if json_mode {
                println!("{}", serde_json::json!({"ok": true, "out_path": out_path}));
            } else {
                println!("exported snapshot to {}", out_path.display());
            }
        }
        command => {
            let socket_path = match invocation.socket_path {
                Some(path) => path,
                None if json_mode => {
                    println!(
                        "{}",
                        format_error_json("transport", "missing control socket path")
                    );
                    return Ok(1);
                }
                None => return Err("missing control socket path".to_string()),
            };
            let transport = UdsSocketTransport::new(socket_path);
            let mut client = UdsControlClient::new(transport);
            let reply = match dispatch(&mut client, invocation.request_id, command) {
                Ok(reply) => reply,
                Err(error) if json_mode => {
                    println!("{}", format_error_json(&error.code, &error.message));
                    return Ok(1);
                }
                Err(error) => {
                    return Err(format!(
                        "control command failed: {}: {}",
                        error.code, error.message
                    ));
                }
            };
            if invocation.json {
                println!("{}", format_reply_json(&reply));
            } else {
                println!("{}", format_reply(&reply));
            }
        }
    }
    Ok(0)
}

fn initialize_operator_config_file(
    path: &Path,
    force: bool,
    patch_path: Option<&Path>,
) -> Result<OperatorConfigInitStatus, String> {
    let existed = path.exists();
    if existed && !force {
        if let Some(patch_path) = patch_path {
            return Err(format!(
                "config {} already exists; pass --force to rewrite it with patch {}",
                path.display(),
                patch_path.display()
            ));
        }
        OperatorConfig::load(path)
            .map_err(|error| format!("validate config {}: {error}", path.display()))?;
        return Ok(OperatorConfigInitStatus::ExistingValid);
    }
    let mut config = OperatorConfig::init()?;
    if let Some(patch_path) = patch_path {
        config = config.patch_file(patch_path)?;
    }
    config.dump_to_path(path, force)?;
    Ok(if existed {
        OperatorConfigInitStatus::Overwritten
    } else {
        OperatorConfigInitStatus::Created
    })
}
