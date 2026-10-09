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
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::thread;
use std::time::Duration;

#[derive(Debug)]
struct Args {
    config: PathBuf,
    reload: bool,
    bpf_object: PathBuf,
    control_socket: PathBuf,
    event_socket: PathBuf,
    launch_socket: PathBuf,
    dsh_socket: PathBuf,
    ui_socket: PathBuf,
    state_dir: PathBuf,
    socket_group: Option<String>,
    gateway_user: Option<String>,
    check_config: bool,
    duration_seconds: Option<u64>,
    dns_refresh_seconds: u64,
    dsh_self_groups: Vec<String>,
    spawn_grpc: bool,
    grpc_bin: Option<PathBuf>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args(env::args().skip(1))?;
    if args.reload {
        load(&args.config)?;
        let mut stream = UnixStream::connect(&args.control_socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        censorguard_common::protocol::write_json_line(
            &mut stream,
            &serde_json::json!({
                "v": censorguard_common::protocol::VERSION, "op": "daemon_reload"
            }),
        )?;
        let reply: censorguard_common::protocol::Response =
            censorguard_common::protocol::read_json_line(&mut BufReader::new(stream))?;
        if !reply.ok {
            return Err(invalid_input(reply.error.unwrap_or_default()).into());
        }
        println!("daemon restart requested; wait for READY and reattach agents");
        return Ok(());
    }
    let mut signals = SigSet::empty();
    signals.add(Signal::SIGHUP);
    signals.add(Signal::SIGINT);
    signals.add(Signal::SIGTERM);
    pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&signals), None)?;
    let (canonical_policy, initial_revision) = if args.check_config {
        (load(&args.config)?, 1)
    } else {
        // Startup must have a real baseline/config input; never silently launch empty.
        load(&args.config)?;
        load_startup_policy(&args.config, &args.state_dir)?
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
    let installed = kernel.install_policy_version(
        &policy,
        initial_revision,
        [
            policy.daemon.enable_file,
            policy.daemon.enable_exec,
            policy.daemon.enable_net,
        ],
        [
            policy.daemon.audit_file,
            policy.daemon.audit_exec,
            policy.daemon.audit_net,
        ],
    )?;
    kernel.protect_pid(std::process::id())?;
    // NativeKernel::load fails unless every required hook attached, so reaching this
    // point means the enforcement surface is fully mounted.
    let hooks_healthy = true;
    let boot_id = read_boot_id();
    let runtime = Arc::new(Runtime::new(RuntimeInit {
        kernel: Arc::clone(&kernel),
        policy: Arc::clone(&policy),
        installed,
        dns_cache,
        dns_status,
        revision_dir: args.state_dir.clone(),
        config_path: fs::canonicalize(&args.config)?,
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

    println!(
        "[READY] {} required hooks attached; control={} events={} launch={} dsh={} ui={}",
        manifest.required().len(),
        args.control_socket.display(),
        args.event_socket.display(),
        args.launch_socket.display(),
        args.dsh_socket.display(),
        args.ui_socket.display()
    );

    wait_for_shutdown(args.duration_seconds, &shutdown_receiver, &runtime)?;
    grpc_shutdown.store(true, Ordering::SeqCst);

    let _ = fs::remove_file(&args.control_socket);
    let _ = fs::remove_file(&args.event_socket);
    let _ = fs::remove_file(&args.launch_socket);
    let _ = fs::remove_file(&args.dsh_socket);
    let _ = fs::remove_file(&args.ui_socket);
    if let Some(supervisor) = _grpc {
        let _ = supervisor.join();
    }
    eprintln!("censorguardd shutdown complete");
    if runtime.restart_requested() {
        eprintln!("restarting daemon with {}", args.config.display());
        let error = Command::new(env::current_exe()?)
            .arg("launch")
            .arg("--config")
            .arg(&args.config)
            .exec();
        return Err(error.into());
    }
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
                    Ok(Signal::SIGHUP) => {
                        if let Err(error) = runtime.request_restart() {
                            eprintln!("restart failed: {error}");
                        }
                    }
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
    runtime: &Runtime,
) -> Result<(), io::Error> {
    let deadline =
        duration_seconds.map(|seconds| std::time::Instant::now() + Duration::from_secs(seconds));
    loop {
        if runtime.restart_requested()
            || deadline.is_some_and(|time| std::time::Instant::now() >= time)
        {
            return Ok(());
        }
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(()) => return Ok(()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("signal handler stopped unexpectedly"));
            }
        }
    }
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, io::Error> {
    let mut values: Vec<String> = args.collect();
    let reload = values.first().is_some_and(|value| value == "reload");
    if values
        .first()
        .is_some_and(|value| matches!(value.as_str(), "launch" | "reload"))
    {
        values.remove(0);
    } else if !values
        .first()
        .is_some_and(|value| matches!(value.as_str(), "--help" | "-h"))
    {
        return Err(invalid_input(
            "expected launch or reload; usage: censorguardd launch|reload [--config PATH]",
        ));
    }
    let parsed = parse_args_with_values(values.into_iter())?;
    if reload && (parsed.check_config || parsed.duration_seconds.is_some()) {
        return Err(invalid_input("reload accepts only --config PATH"));
    }
    Ok(Args { reload, ..parsed })
}

fn parse_args_with_values(mut args: impl Iterator<Item = String>) -> Result<Args, io::Error> {
    let mut config = None;

    let mut check_config = false;
    let mut duration_seconds = None;
    let mut seen = std::collections::HashSet::new();
    while let Some(argument) = args.next() {
        if !seen.insert(argument.clone()) {
            return Err(invalid_input(format!("duplicate option {argument:?}")));
        }
        match argument.as_str() {
            "--config" => config = Some(next_value(&mut args, "--config")?.into()),
            "--check-config" => check_config = true,
            "--duration" => {
                duration_seconds = Some(parse_u64(next_value(&mut args, "--duration")?)?)
            }
            "-h" | "--help" => {
                println!(
                    "usage: censorguardd launch [--config PATH] [--check-config]\n\
                     [--duration SECONDS]\n\
                     censorguardd reload [--config PATH]\n\
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
    Ok(Args {
        config,
        reload: false,
        bpf_object,
        control_socket,
        event_socket,
        launch_socket,
        dsh_socket,
        ui_socket,
        state_dir,
        socket_group,
        gateway_user,
        check_config,
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
    let persisted = compile_yaml(&bytes)?;
    let base = load(config)?;
    let mut root = persisted.to_root();
    let changed = root.rules != base.to_root().rules || root.daemon != base.daemon;
    root.rules = base.to_root().rules;
    root.daemon = base.daemon;
    let revision = if changed {
        revision
            .checked_add(1)
            .ok_or_else(|| invalid_input("revision exhausted"))?
    } else {
        revision
    };
    Ok((censorguard_policy::compile(root)?, revision))
}

fn print_summary(policy: &CompiledPolicy) {
    println!(
        "switches: enable file={} exec={} net={}; audit file={} exec={} net={}",
        policy.daemon.enable_file,
        policy.daemon.enable_exec,
        policy.daemon.enable_net,
        policy.daemon.audit_file,
        policy.daemon.audit_exec,
        policy.daemon.audit_net,
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

/// Validate the new startup input before dropping the running enforcement plane.
fn validate_restart_config(path: &Path) -> Result<(), io::Error> {
    config::load(path).map_err(|error| invalid_input(error.to_string()))?;
    let policy = load(path).map_err(|error| invalid_input(error.to_string()))?;
    let daemon = &policy.daemon;
    resolve_socket_group(daemon.socket_group.as_deref())?;
    resolve_gateway_user(daemon.gateway_user.as_deref())?;
    fs::File::open(&daemon.bpf_object)?;
    if daemon.spawn_grpc {
        let grpc = daemon.grpc_bin.clone().unwrap_or_else(|| {
            env::current_exe()
                .unwrap_or_default()
                .with_file_name("censorguard-grpc")
        });
        fs::File::open(grpc)?;
    }
    load_startup_policy(path, &daemon.state_dir)
        .map_err(|error| invalid_input(error.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_uses_current_base_settings_and_restores_dynamic_memberships()
    -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let config = directory.path().join("base.yaml");
        let state = directory.path().join("state");
        fs::create_dir_all(state.join("revisions"))?;
        fs::write(&config, "rules:\n  - exec deny /bin/false\n")?;
        let saved = load(&config)?
            .replace_group_yaml("a", b"rules:\n  - exec deny /bin/true\n")?
            .replace_group_yaml("b", b"rules: []")?
            .bind_domain("agent", "a", &[])?
            .bind_domain("agent", "b", &[])?;
        fs::write(state.join("current"), "7\n")?;
        fs::write(
            state.join("revisions/7.yaml"),
            serde_yaml::to_string(&saved.to_root())?,
        )?;
        assert_eq!(load_startup_policy(&config, &state)?.1, 7);
        fs::write(
            &config,
            "daemon:\n  audit_exec: true\n  enable_net: false\nrules:\n  - exec deny /bin/echo\n",
        )?;
        let (restored, revision) = load_startup_policy(&config, &state)?;
        assert_eq!(revision, 8);
        assert_eq!(restored.to_root().rules, vec!["exec deny /bin/echo"]);
        assert_eq!(restored.domains["agent"], "a+b");
        assert_eq!(restored.group_definitions, saved.group_definitions);
        assert!(restored.daemon.audit_exec);
        assert!(!restored.daemon.enable_net);
        assert!(restored.groups.contains_key("a+b"));
        // A stale snapshot never overrides a newer public baseline.
        assert_eq!(fs::read_to_string(state.join("current"))?, "7\n");
        Ok(())
    }

    #[test]
    fn dns_refresh_defaults_to_sixty_seconds() -> Result<(), io::Error> {
        let args = parse_args(["launch".to_owned()].into_iter())?;
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
        let args = parse_args(["launch".to_owned()].into_iter())?;
        assert!(!args.spawn_grpc);
        assert_eq!(args.grpc_bin, None);
        Ok(())
    }

    #[test]
    fn missing_explicit_config_is_rejected() {
        assert!(
            parse_args(
                ["launch", "--config", "/does-not-exist/censorguard.yaml"]
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
            "--track-pid",
            "--domain",
        ] {
            assert!(parse_args(["launch", flag, "value"].into_iter().map(str::to_owned)).is_err());
        }
    }
}
