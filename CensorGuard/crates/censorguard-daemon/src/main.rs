mod event;
mod procfs;
mod runtime;
mod server;

use censorguard_common::config::{self, DEFAULT_CONFIG_PATH};
use censorguard_kernel::{HookManifest, NativeKernel};
use censorguard_policy::{CompileWarning, CompiledPolicy, compile_yaml, load};
use event::EventHub;
use nix::sys::signal::{SigSet, SigmaskHow, Signal, pthread_sigmask};
use nix::unistd::{Gid, Group, User};
use runtime::{Runtime, RuntimeInit};
use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::thread;
use std::time::Duration;

#[derive(Debug)]
struct Args {
    policy_free: bool,
    config: PathBuf,
    bpf_object: PathBuf,
    control_socket: PathBuf,
    event_socket: PathBuf,
    launch_socket: PathBuf,
    dsh_socket: PathBuf,
    ui_socket: PathBuf,
    state_dir: PathBuf,
    state_dir_ephemeral: bool,
    socket_group: Option<String>,
    gateway_user: Option<String>,
    check_config: bool,
    track_pid: Option<u32>,
    domain: String,
    duration_seconds: Option<u64>,
    dns_refresh_seconds: u64,
    dsh_self_groups: Vec<String>,
    spawn_grpc: bool,
    grpc_bin: Option<PathBuf>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args(env::args().skip(1))?;
    let mut signals = SigSet::empty();
    signals.add(Signal::SIGHUP);
    signals.add(Signal::SIGINT);
    signals.add(Signal::SIGTERM);
    pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&signals), None)?;
    let (canonical_policy, initial_revision) =
        if (!args.policy_free || args.check_config) && args.config.exists() {
            if args.check_config {
                (load(&args.config)?, 1)
            } else {
                load_startup_policy(&args.config, &args.state_dir)?
            }
        } else {
            // Policy-free daemon startup: ctl applies the canonical rules document later.
            (compile_yaml(b"rules: []")?, 1)
        };
    let (resolved_policy, dns_cache, dns_status) =
        canonical_policy.resolve_dns(&Default::default());
    let policy = Arc::new(resolved_policy);
    print_summary(&policy);
    if args.check_config {
        println!("configuration is valid; kernel state was not changed");
        return Ok(());
    }
    let socket_group = resolve_socket_group(args.socket_group.as_deref())?;
    let gateway_uid = resolve_gateway_user(args.gateway_user.as_deref())?;

    // All policy dimensions stay attached so a later atomic reload can enable a dimension that
    // was disabled at startup. The filter_config map keeps disabled hooks fail-open and cheap.
    let manifest = HookManifest::for_policy([true, true, true]);
    let kernel = Arc::new(NativeKernel::load(&args.bpf_object, &manifest)?);
    let installed =
        kernel.install_policy_version(&policy, initial_revision, [true; 3], [false; 3])?;
    kernel.protect_pid(std::process::id())?;
    // NativeKernel::load fails unless every required hook attached, so reaching this
    // point means the enforcement surface is fully mounted.
    let hooks_healthy = true;
    let boot_id = read_boot_id();
    let runtime = Arc::new(Runtime::new(RuntimeInit {
        kernel: Arc::clone(&kernel),
        config_path: args.config.clone(),
        policy: Arc::clone(&policy),
        installed,
        dns_cache,
        dns_status,
        revision_dir: args.state_dir.clone(),
        initial_revision,
        boot_id: boot_id.clone(),
        hooks_healthy,
        dsh_self_groups: args.dsh_self_groups.iter().cloned().collect(),
    })?);
    let hub = Arc::new(EventHub::new(boot_id));

    let _event_pump =
        event::start_event_pump(Arc::clone(&kernel), Arc::clone(&runtime), Arc::clone(&hub))?;
    let _control = server::start_control(
        &args.control_socket,
        socket_group,
        Arc::clone(&runtime),
        Arc::clone(&hub),
        gateway_uid,
    )?;
    let _events = server::start_events(&args.event_socket, socket_group, Arc::clone(&hub))?;
    let _launch = server::start_launch(&args.launch_socket, socket_group, Arc::clone(&runtime))?;
    let _dsh = server::start_dsh(&args.dsh_socket, Arc::clone(&runtime), Arc::clone(&hub))?;
    let _ui = server::start_ui(&args.ui_socket, Arc::clone(&runtime), Arc::clone(&hub))?;
    let _reconciler = start_reconciler(Arc::clone(&runtime))?;
    let _dns = start_dns_refresher(
        Arc::clone(&runtime),
        Duration::from_secs(args.dns_refresh_seconds),
    )?;
    let (shutdown_sender, shutdown_receiver) = sync_channel(1);
    let _signals = start_signal_handler(Arc::clone(&runtime), signals, shutdown_sender)?;
    let grpc_shutdown = Arc::new(AtomicBool::new(false));

    let _grpc = if args.spawn_grpc {
        Some(start_grpc_supervisor(
            args.grpc_bin,
            args.config.clone(),
            Arc::clone(&grpc_shutdown),
        )?)
    } else {
        None
    };

    if let Some(pid) = args.track_pid {
        let result = runtime.track(pid, &args.domain, true, 0)?;
        println!(
            "migration track: pid={pid} domain={} slot={} seeded={}",
            result.domain.name, result.domain.slot, result.seeded
        );
    }

    println!(
        "[READY] {} required hooks attached; control={} events={} launch={} dsh={} ui={}",
        manifest.required().len(),
        args.control_socket.display(),
        args.event_socket.display(),
        args.launch_socket.display(),
        args.dsh_socket.display(),
        args.ui_socket.display()
    );

    wait_for_shutdown(args.duration_seconds, &shutdown_receiver)?;
    grpc_shutdown.store(true, Ordering::SeqCst);

    let _ = fs::remove_file(&args.control_socket);
    let _ = fs::remove_file(&args.event_socket);
    let _ = fs::remove_file(&args.launch_socket);
    let _ = fs::remove_file(&args.dsh_socket);
    let _ = fs::remove_file(&args.ui_socket);
    if args.state_dir_ephemeral {
        let _ = fs::remove_dir_all(&args.state_dir);
    }
    eprintln!("censorguardd shutdown complete");
    Ok(())
}

fn start_dns_refresher(
    runtime: Arc<Runtime>,
    interval: Duration,
) -> Result<thread::JoinHandle<()>, io::Error> {
    thread::Builder::new()
        .name("censorguard-dns".into())
        .spawn(move || {
            loop {
                thread::sleep(interval);
                match runtime.refresh_dns() {
                    Ok(Some(result)) => eprintln!(
                        "DNS policy refresh: generation={} version={} changed={:?}",
                        result.generation, result.version, result.changed
                    ),
                    Ok(None) => {}
                    Err(error) => {
                        eprintln!("DNS refresh failed; last cache remains active: {error}")
                    }
                }
            }
        })
}

fn start_signal_handler(
    runtime: Arc<Runtime>,
    signals: SigSet,
    shutdown: SyncSender<()>,
) -> Result<thread::JoinHandle<()>, io::Error> {
    thread::Builder::new()
        .name("censorguard-signals".into())
        .spawn(move || {
            loop {
                match signals.wait() {
                    Ok(Signal::SIGHUP) => match runtime.reload(None, None, None, false) {
                        Ok(result) => eprintln!(
                            "SIGHUP reload complete: generation={} version={} changed={:?}",
                            result.generation, result.version, result.changed
                        ),
                        Err(error) => {
                            eprintln!("SIGHUP reload failed; old bank remains active: {error}")
                        }
                    },
                    Ok(Signal::SIGINT | Signal::SIGTERM) => {
                        eprintln!("shutdown requested by signal");
                        let _ = shutdown.send(());
                        return;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        eprintln!("signal waiter stopped: {error}");
                        let _ = shutdown.send(());
                        return;
                    }
                }
            }
        })
}

/// Supervisor for the unprivileged gRPC adapter child process.
///
/// The `daemon.spawn_grpc` setting lets one systemd unit own the full DSH delegated
/// surface: the daemon spawns `censorguard-grpc` (default: sibling binary of the
/// daemon's own executable), restarts it after a 1s backoff if it crashes, and terminates it
/// (TERM, then KILL after a 2s grace period) when the daemon shuts down. The
/// adapter keeps its own process boundary, so tonic panics never take down the
/// eBPF enforcement plane.
fn start_grpc_supervisor(
    grpc_bin: Option<PathBuf>,
    config_path: PathBuf,
    shutdown: Arc<AtomicBool>,
) -> Result<thread::JoinHandle<()>, io::Error> {
    let bin = grpc_bin.unwrap_or_else(|| {
        let exe =
            std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/usr/sbin/censorguardd"));
        exe.parent()
            .unwrap_or_else(|| Path::new("/usr/sbin"))
            .join("censorguard-grpc")
    });
    if !bin.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "gRPC adapter not found at {}; build it or set daemon.grpc_bin in the config",
                bin.display()
            ),
        ));
    }
    let mut command = Command::new(&bin);
    // The adapter reads the same config as the daemon.
    command.arg("--config").arg(config_path);
    thread::Builder::new()
        .name("censorguard-grpc".into())
        .spawn(move || {
            loop {
                if shutdown.load(Ordering::SeqCst) {
                    return;
                }
                let mut child = match command.spawn() {
                    Ok(child) => child,
                    Err(error) => {
                        eprintln!("gRPC adapter spawn failed: {error}; retrying in 5s");
                        thread::sleep(Duration::from_secs(5));
                        continue;
                    }
                };
                eprintln!("gRPC adapter spawned: pid={}", child.id());
                let child_pid = nix::unistd::Pid::from_raw(child.id() as i32);
                // Poll the shutdown flag and child liveness with short timeouts.
                loop {
                    if shutdown.load(Ordering::SeqCst) {
                        let _ = nix::sys::signal::kill(child_pid, Signal::SIGTERM);
                        for _ in 0..10 {
                            if shutdown_reaped(&mut child) {
                                eprintln!("gRPC adapter terminated by signal");
                                return;
                            }
                            thread::sleep(Duration::from_millis(200));
                        }
                        let _ = nix::sys::signal::kill(child_pid, Signal::SIGKILL);
                        let _ = child.wait();
                        eprintln!("gRPC adapter killed after grace period");
                        return;
                    }
                    if shutdown_reaped(&mut child) {
                        eprintln!("gRPC adapter exited; restarting in 1s");
                        thread::sleep(Duration::from_secs(1));
                        break;
                    }
                    thread::sleep(Duration::from_millis(200));
                }
            }
        })
}

/// Try to reap the adapter child without blocking; true when it has exited.
fn shutdown_reaped(child: &mut std::process::Child) -> bool {
    match child.try_wait() {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(error) => {
            eprintln!("gRPC adapter wait failed: {error}");
            true
        }
    }
}

fn start_reconciler(runtime: Arc<Runtime>) -> Result<thread::JoinHandle<()>, io::Error> {
    thread::Builder::new()
        .name("censorguard-reconcile".into())
        .spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(5));
                if let Err(error) = runtime.reconcile() {
                    eprintln!("process-tree reconcile failed: {error}");
                }
            }
        })
}

fn wait_for_shutdown(
    duration_seconds: Option<u64>,
    receiver: &Receiver<()>,
) -> Result<(), io::Error> {
    match duration_seconds {
        Some(seconds) => match receiver.recv_timeout(Duration::from_secs(seconds)) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => Ok(()),
            Err(RecvTimeoutError::Disconnected) => {
                Err(io::Error::other("signal handler stopped unexpectedly"))
            }
        },
        None => receiver
            .recv()
            .map_err(|_| io::Error::other("signal handler stopped unexpectedly")),
    }
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, io::Error> {
    // The daemon is a background service; `launch` is an explicit, policy-free
    // entrypoint. Policy loading/application is performed by censorguardctl.
    let mut values: Vec<String> = args.collect();
    let policy_free = if values.first().is_some_and(|value| value == "launch") {
        values.remove(0);
        true
    } else {
        false
    };
    parse_args_with_values(values.into_iter(), policy_free)
}

fn parse_args_with_values(
    mut args: impl Iterator<Item = String>,
    policy_free: bool,
) -> Result<Args, io::Error> {
    let mut config = None;

    let mut check_config = false;
    let mut track_pid = None;
    let mut domain = String::from("default");
    let mut duration_seconds = None;
    let mut seen = std::collections::HashSet::new();
    while let Some(argument) = args.next() {
        if !seen.insert(argument.clone()) {
            return Err(invalid_input(format!("duplicate option {argument:?}")));
        }
        match argument.as_str() {
            "--config" => config = Some(next_value(&mut args, "--config")?.into()),
            "--check-config" => check_config = true,
            "--track-pid" => track_pid = Some(parse_u32(next_value(&mut args, "--track-pid")?)?),
            "--domain" => domain = next_value(&mut args, "--domain")?,
            "--duration" => {
                duration_seconds = Some(parse_u64(next_value(&mut args, "--duration")?)?)
            }
            "-h" | "--help" => {
                println!(
                    "usage: censorguardd launch [--config PATH] [--check-config]\n\
                     [--track-pid PID --domain NAME] [--duration SECONDS]\n\
                     all daemon paths and runtime settings are read from the config file"
                );
                std::process::exit(0);
            }
            _ => return Err(invalid_input(format!("unknown argument {argument:?}"))),
        }
    }
    let explicit_config = config.is_some();
    let config = config.unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH));
    let daemon = if explicit_config || config.exists() {
        config::load(&config).map_err(|error| invalid_input(format!("invalid config: {error}")))?
    } else {
        config::DaemonConfig::default()
    };
    let bpf_object = daemon.bpf_object;
    let control_socket = daemon.control_socket;
    let event_socket = daemon.event_socket;
    let launch_socket = daemon.launch_socket;
    let dsh_socket = daemon.dsh_socket;
    let ui_socket = daemon.ui_socket;
    let state_dir = daemon.state_dir;
    let socket_group = daemon.socket_group;
    let gateway_user = daemon.gateway_user;
    let dsh_self_groups = daemon.dsh_self_groups;
    let dns_refresh_seconds = daemon.dns_refresh_seconds;
    let spawn_grpc = daemon.spawn_grpc;
    let grpc_bin = daemon.grpc_bin;
    let state_dir_ephemeral = false;
    Ok(Args {
        policy_free,
        config,
        bpf_object,
        control_socket,
        event_socket,
        launch_socket,
        dsh_socket,
        ui_socket,
        state_dir,
        state_dir_ephemeral,
        socket_group,
        gateway_user,
        check_config,
        track_pid,
        domain,
        duration_seconds,
        dns_refresh_seconds,
        dsh_self_groups,
        spawn_grpc,
        grpc_bin,
    })
}

fn resolve_socket_group(name: Option<&str>) -> Result<Option<Gid>, io::Error> {
    let Some(name) = name else {
        return Ok(None);
    };
    Group::from_name(name)
        .map_err(io::Error::from)?
        .map(|group| group.gid)
        .ok_or_else(|| invalid_input(format!("socket group {name:?} does not exist")))
        .map(Some)
}

fn resolve_gateway_user(name: Option<&str>) -> Result<Option<u32>, io::Error> {
    let Some(name) = name else {
        return Ok(None);
    };
    User::from_name(name)
        .map_err(io::Error::from)?
        .map(|user| user.uid.as_raw())
        .ok_or_else(|| invalid_input(format!("gateway user {name:?} does not exist")))
        .map(Some)
}

fn next_value(args: &mut impl Iterator<Item = String>, name: &str) -> Result<String, io::Error> {
    args.next()
        .ok_or_else(|| invalid_input(format!("{name} requires a value")))
}

/// Per-daemon-boot identifier. The kernel UUID changes on every daemon start, which lets
/// clients (heartbeat, event stream) detect a daemon restart and re-attach immediately.
fn read_boot_id() -> String {
    fs::read_to_string("/proc/sys/kernel/random/uuid")
        .map(|value| value.trim().to_owned())
        .unwrap_or_else(|_| format!("boot-{}", std::process::id()))
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn parse_u32(value: String) -> Result<u32, io::Error> {
    value
        .parse()
        .map_err(|_| invalid_input(format!("expected u32, got {value:?}")))
}

fn parse_u64(value: String) -> Result<u64, io::Error> {
    value
        .parse()
        .map_err(|_| invalid_input(format!("expected u64, got {value:?}")))
}

fn load_startup_policy(
    config: &std::path::Path,
    state_dir: &std::path::Path,
) -> Result<(CompiledPolicy, u32), Box<dyn Error>> {
    let current_path = state_dir.join("current");
    let current = match fs::read_to_string(&current_path) {
        Ok(value) => Some(value),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let Some(current) = current else {
        return Ok((load(config)?, 1));
    };
    let revision = current.trim().parse::<u32>().map_err(|_| {
        invalid_input(format!(
            "invalid revision pointer in {}",
            current_path.display()
        ))
    })?;
    if revision == 0 {
        return Err(invalid_input("persisted policy revision must be positive").into());
    }
    let snapshot = state_dir.join("revisions").join(format!("{revision}.yaml"));
    let bytes = fs::read(&snapshot)?;
    Ok((compile_yaml(&bytes)?, revision))
}

fn print_summary(policy: &CompiledPolicy) {
    println!(
        "switches: enable file=true exec=true net=true; audit file=false exec=false net=false"
    );
    println!(
        "policy: baseline={} groups={} domains={}",
        policy.baseline.is_some(),
        policy.groups.len(),
        policy.domains.len()
    );
    for warning in &policy.warnings {
        match warning {
            CompileWarning::MissingPathFallback { path, error } => {
                eprintln!("warning: {path}: stat failed; open-only string fallback: {error}");
            }
            CompileWarning::MissingCommandFallback { path, error } => {
                eprintln!("warning: {path}: stat failed; exec string fallback: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_refresh_defaults_to_sixty_seconds() -> Result<(), io::Error> {
        let args = parse_args(std::iter::empty())?;
        assert_eq!(args.dns_refresh_seconds, 60);
        Ok(())
    }

    #[test]
    fn unknown_socket_group_is_rejected() {
        assert!(resolve_socket_group(Some("censorguard-group-that-does-not-exist")).is_err());
    }

    #[test]
    fn unknown_gateway_user_is_rejected() {
        assert!(resolve_gateway_user(Some("censorguard-user-that-does-not-exist")).is_err());
    }

    #[test]
    fn spawn_grpc_defaults_to_off() -> Result<(), io::Error> {
        let args = parse_args(std::iter::empty())?;
        assert!(!args.spawn_grpc);
        assert_eq!(args.grpc_bin, None);
        Ok(())
    }

    #[test]
    fn missing_explicit_config_is_rejected() {
        assert!(
            parse_args(
                ["--config", "/does-not-exist/censorguard.yaml"]
                    .into_iter()
                    .map(str::to_owned)
            )
            .is_err()
        );
    }

    #[test]
    fn removed_runtime_flags_are_rejected() {
        for flag in [
            "--bpf-object",
            "--ctl-sock",
            "--event-sock",
            "--state-dir",
            "--spawn-grpc",
            "--hold-seconds",
        ] {
            assert!(parse_args([flag, "value"].into_iter().map(str::to_owned)).is_err());
        }
    }
}
