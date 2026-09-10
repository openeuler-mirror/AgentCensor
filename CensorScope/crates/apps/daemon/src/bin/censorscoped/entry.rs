//! Top-level command execution for the `censorscoped` binary.

use std::path::Path;

use config_core::capture_profile::{CaptureLevel, CaptureProfile};
use config_core::daemon::{OperatorConfig, OperatorConfigInitStatus};
use daemon::{LocalDaemonServer, resolve_ebpf_collector_config};

use crate::args::{CensorscopedCommand, parse_args};
use crate::process::{
    DaemonProcessState, cleanup_runtime_files, start_daemon, status_daemon, stop_daemon,
    write_pid_file,
};
use crate::signals;

/// Parses the process arguments and executes the selected daemon command.
pub fn run_from_env() -> Result<(), String> {
    match parse_args(std::env::args().skip(1))? {
        CensorscopedCommand::Init {
            config_path,
            force,
            patch_path,
        } => initialize(&config_path, force, patch_path.as_deref()),
        CensorscopedCommand::Run { config_path, level } => {
            run_foreground(&load_config_for_level(&config_path, level)?)
        }
        CensorscopedCommand::Start { config_path, level } => {
            let config = load_config_for_level(&config_path, level)?;
            start_daemon(&config_path, &config, level)
        }
        CensorscopedCommand::Stop { config_path } => stop_daemon(&OperatorConfig::load(&config_path)?),
        CensorscopedCommand::Restart { config_path, level } => {
            let config = load_config_for_level(&config_path, level)?;
            stop_daemon(&config)?;
            start_daemon(&config_path, &config, level)
        }
        CensorscopedCommand::Status { config_path } => {
            let config = OperatorConfig::load(&config_path)?;
            match status_daemon(&config)? {
                DaemonProcessState::Running { pid } => println!("censorscoped running pid={pid}"),
                DaemonProcessState::Stopped => println!("censorscoped stopped"),
                DaemonProcessState::StalePid { pid } => println!("censorscoped stale pid={pid}"),
                DaemonProcessState::StaleSocket => {
                    println!("censorscoped stale socket={}", config.socket_path.display())
                }
            }
            Ok(())
        }
    }
}

/// Load the operator file and apply the run's collection level in memory.
///
/// The level is per-run state: the operator file is never rewritten and never
/// records a level, so a later `start` without `--level` always falls back to
/// the default level L1.
fn load_config_for_level(config_path: &Path, level: CaptureLevel) -> Result<OperatorConfig, String> {
    let mut config = OperatorConfig::load(config_path)?;
    config.capture_profile = CaptureProfile::for_level(level);
    Ok(config)
}

fn initialize(path: &Path, force: bool, patch: Option<&Path>) -> Result<(), String> {
    let existed = path.exists();
    if existed && !force {
        OperatorConfig::load(path)?;
        println!("config {} already exists and is valid", path.display());
        return Ok(());
    }
    let mut config = OperatorConfig::init()?;
    if let Some(patch) = patch {
        config = config.patch_file(patch)?;
    }
    config.dump_to_path(path, force)?;
    let status = if existed {
        OperatorConfigInitStatus::Overwritten
    } else {
        OperatorConfigInitStatus::Created
    };
    println!(
        "{} config {}",
        if status == OperatorConfigInitStatus::Created {
            "initialized"
        } else {
            "overwrote"
        },
        path.display()
    );
    Ok(())
}

fn run_foreground(config: &OperatorConfig) -> Result<(), String> {
    signals::install_shutdown_handlers()?;
    write_pid_file(&config.pid_file, std::process::id())?;
    let resolution = resolve_ebpf_collector_config(config.ebpf_config.clone());
    if let Some(detail) = &resolution.degrade_detail {
        tracing::warn!(detail = %detail, "eBPF collector unavailable; using snapshot-only tracking");
    }
    let mut server = LocalDaemonServer::build(
        &config.storage,
        config.capture_profile.clone(),
        resolution.config,
        config.writer,
        config.payload_max_trace_bytes,
        config.payload_max_segment_bytes,
        config.active_trace_max,
        config.session_env_name.clone(),
    )
    .map_err(|error| format!("daemon build failed: {}: {}", error.code, error.message))?;
    let result = server.serve_forever_until(
        &config.socket_path,
        config.socket_permissions,
        config.control_pending_connection_max,
        signals::shutdown_requested,
        || {
            println!(
                "daemon listening socket={} storage={}",
                config.socket_path.display(),
                config.storage.path().display()
            );
            Ok(())
        },
    );
    let shutdown = server.shutdown();
    let cleanup = cleanup_runtime_files(config, true);
    result.map_err(|error| format!("daemon run failed: {}: {}", error.stage, error.message))?;
    shutdown
        .map_err(|error| format!("daemon shutdown failed: {}: {}", error.code, error.message))?;
    cleanup
}
