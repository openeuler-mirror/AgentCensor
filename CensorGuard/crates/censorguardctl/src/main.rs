use censorguard_common::config::{self, DEFAULT_CONFIG_PATH};
use censorguard_common::protocol::{
    Event, Operation, ProtocolError, Request, Response, VERSION, read_json_line, write_json_line,
};
use rustix::process::{
    Pid, PidfdFlags, Signal, WaitOptions, getpid, kill_process, pidfd_open, pidfd_send_signal,
    waitpid,
};
use std::env;
use std::error::Error;
use std::io::{self, BufReader};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

mod output;

// The stopped child is an internal process mode, not a public CLI command.
const SPAWN_HELPER_ENV: &str = "_CENSORGUARD_SPAWN_HELPER";

#[derive(Debug, Eq, PartialEq)]
enum Action {
    Request(Request),
    Status {
        json: bool,
    },
    Doctor {
        config: config::DaemonConfig,
        json: bool,
        verbose: bool,
    },
    Audit {
        socket: PathBuf,
        kind: Option<u32>,
        pid: Option<u32>,
    },
    Spawn {
        domain: Option<String>,
        keep: bool,
        command: Vec<String>,
    },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    if env::var_os(SPAWN_HELPER_ENV).as_deref() == Some(std::ffi::OsStr::new("1")) {
        return spawn_helper(&env::args().skip(1).collect::<Vec<_>>());
    }
    if env::args()
        .nth(1)
        .is_some_and(|argument| argument == "--version" || argument == "-V")
    {
        println!("censorguardctl {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if matches!(env::args().nth(1).as_deref(), Some("-h" | "--help")) {
        println!(
            "CensorGuard control\n\n  status           Show protection and managed processes\n  doctor           Check installation and runtime health\n  policy           Inspect or publish policy groups\n  attach / unattach / bind / remove / spawn / audit\n\nUse status --json or doctor --json for scripts.\nUse doctor --verbose for individual checks.\nCustom configuration: --config PATH before the command."
        );
        return Ok(());
    }
    let (socket, action) = parse_args(env::args().skip(1))?;
    match action {
        Action::Audit { socket, kind, pid } => return audit(&socket, kind, pid),
        Action::Doctor {
            config,
            json,
            verbose,
        } => {
            if !doctor(&config, json, verbose)? {
                std::process::exit(1);
            }
        }
        Action::Status { json } => {
            let response = call(&socket, &request(Operation::Status)).map_err(|error| {
                io::Error::other(format!("Cannot reach CensorGuard: {error}. Check systemctl status censorguardd.service; use --config PATH for a custom instance."))
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else {
                print!("{}", output::status(&response));
                if !response.ok {
                    std::process::exit(1);
                }
            }
            ensure_success(response)?;
        }
        Action::Request(request) => {
            let response = call(&socket, &request)?;
            println!("{}", serde_json::to_string_pretty(&response)?);
            ensure_success(response)?;
        }
        Action::Spawn {
            domain,
            keep,
            command,
        } => std::process::exit(spawn(&socket, domain, keep, &command)?),
    }
    Ok(())
}

fn doctor(
    config: &config::DaemonConfig,
    json: bool,
    verbose: bool,
) -> Result<bool, Box<dyn Error>> {
    use std::net::{SocketAddr, TcpStream};
    let mut checks = Vec::new();
    let mut check = |name: &str, ok: bool, detail: String| {
        checks.push(output::Check {
            component: name.into(),
            ok,
            detail,
        });
    };
    check("config", true, "configuration decoded".into());
    check(
        "bpf_object",
        config.bpf_object.is_file(),
        config.bpf_object.display().to_string(),
    );
    let btf = Path::new("/sys/kernel/btf/vmlinux").is_file();
    check("kernel_btf", btf, "/sys/kernel/btf/vmlinux".into());
    let lsm = std::fs::read_to_string("/sys/kernel/security/lsm").unwrap_or_default();
    check(
        "bpf_lsm",
        lsm.trim().split(',').any(|name| name == "bpf"),
        lsm.trim().into(),
    );
    match call(&config.control_socket, &request(Operation::Status)) {
        Ok(response) => {
            check(
                "daemon",
                response.ok && response.daemon_ready,
                response
                    .error
                    .unwrap_or_else(|| response.daemon_boot_id.clone()),
            );
            check(
                "hooks",
                response.hooks_healthy == Some(true),
                format!("healthy={:?}", response.hooks_healthy),
            );
        }
        Err(error) => check("daemon", false, error.to_string()),
    }
    for (name, path) in [
        ("events", &config.event_socket),
        ("launch", &config.launch_socket),
        ("dsh", &config.dsh_socket),
        ("ui", &config.ui_socket),
    ] {
        match UnixStream::connect(path) {
            Ok(_) => check(name, true, path.display().to_string()),
            Err(error) => check(name, false, error.to_string()),
        }
    }
    check(
        "state",
        config.state_dir.join("current").is_file(),
        config.state_dir.display().to_string(),
    );
    if config.spawn_grpc {
        let address: Result<SocketAddr, _> = config
            .grpc_listen
            .as_deref()
            .unwrap_or("127.0.0.1:50051")
            .parse();
        let available = address.is_ok_and(|address| {
            TcpStream::connect_timeout(&address, Duration::from_secs(2)).is_ok()
        });
        check(
            "grpc",
            available,
            config
                .grpc_listen
                .clone()
                .unwrap_or_else(|| "127.0.0.1:50051".into()),
        );
    }
    let healthy = checks.iter().all(|check| check.ok);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"ok":healthy, "checks":checks}))?
        );
    } else {
        print!("{}", output::doctor(&checks, verbose));
    }
    Ok(healthy)
}

fn parse_args(arguments: impl Iterator<Item = String>) -> Result<(PathBuf, Action), io::Error> {
    let mut values: Vec<_> = arguments.collect();
    let explicit_config = values.first().is_some_and(|value| value == "--config");
    let config_path = if values.first().is_some_and(|value| value == "--config") {
        if values.len() < 2 {
            return Err(invalid_input("--config requires a path"));
        }
        let path = values[1].clone();
        values.drain(..2);
        path
    } else {
        DEFAULT_CONFIG_PATH.to_owned()
    };
    let daemon = if explicit_config || std::path::Path::new(&config_path).exists() {
        config::load(&config_path)
            .map_err(|error| invalid_input(format!("invalid config: {error}")))?
    } else {
        config::DaemonConfig::default()
    };
    let socket = daemon.control_socket.clone();
    let Some(command) = values.first().map(String::as_str) else {
        return Err(invalid_input(
            "usage: censorguardctl [--config PATH] <status|doctor|policy|attach|unattach|bind|remove|spawn|audit> ...\n\
             bind --domain NAME --policy GROUP; remove --domain NAME --policy GROUP",
        ));
    };
    let action = match command {
        "spawn" => parse_spawn(&values[1..])?,
        "status" => {
            validate_options(&values[1..], &[], &["--json"])?;
            Action::Status {
                json: values.iter().any(|value| value == "--json"),
            }
        }
        "audit" => {
            validate_options(&values[1..], &["--kind", "--pid"], &[])?;
            let kind = optional_u32_option(&values[1..], "--kind")?;
            if kind.is_some_and(|kind| !(1..=4).contains(&kind)) {
                return Err(invalid_input("--kind expects 1|2|3|4"));
            }
            let pid = optional_u32_option(&values[1..], "--pid")?;
            Action::Audit {
                socket: daemon.event_socket,
                kind,
                pid,
            }
        }
        "doctor" => {
            validate_options(&values[1..], &[], &["--json", "--verbose"])?;
            let json = values.iter().any(|value| value == "--json");
            let verbose = values.iter().any(|value| value == "--verbose");
            if json && verbose {
                return Err(invalid_input("choose --json or --verbose"));
            }
            Action::Doctor {
                config: daemon,
                json,
                verbose,
            }
        }
        "policy" => parse_policy(&values[1..])?,
        "attach" => {
            let options = &values[1..];
            validate_options(options, &["--pid", "--domain"], &[])?;
            let pid = required_u32_option(options, "--pid")?;
            let domain = string_option(options, "--domain")?;
            Action::Request(Request {
                pid: Some(pid),
                domain,
                seed: true,
                ..request(Operation::Track)
            })
        }
        "unattach" => {
            validate_options(&values[1..], &["--pid"], &[])?;
            Action::Request(Request {
                pid: Some(required_u32_option(&values[1..], "--pid")?),
                ..request(Operation::Unattach)
            })
        }
        "bind" | "remove" => {
            let options = &values[1..];
            validate_options(options, &["--domain", "--policy"], &[])?;
            let domain = string_option(options, "--domain")?
                .ok_or_else(|| invalid_input("bind requires --domain DOMAIN"))?;
            Action::Request(Request {
                domain: Some(domain),
                group: Some(
                    string_option(options, "--policy")?
                        .ok_or_else(|| invalid_input("bind requires --policy GROUP"))?,
                ),
                ..request(if command == "bind" {
                    Operation::Bind
                } else {
                    Operation::Remove
                })
            })
        }
        _ => {
            return Err(invalid_input(format!(
                "invalid command or arguments: {values:?}"
            )));
        }
    };
    Ok((socket, action))
}

fn parse_policy(values: &[String]) -> Result<Action, io::Error> {
    let Some(subcommand) = values.first().map(String::as_str) else {
        return Err(invalid_input(
            "usage: censorguardctl policy apply|remove ...",
        ));
    };
    match subcommand {
        "remove" => {
            validate_options(&values[1..], &["--policy"], &[])?;
            Ok(Action::Request(Request {
                group: Some(required_string_option(&values[1..], "--policy")?),
                ..request(Operation::Remove)
            }))
        }
        "apply" => {
            let options = &values[1..];
            validate_options(options, &["--policy", "--file"], &["--stdin", "--dry-run"])?;
            let group = required_string_option(options, "--policy")?;
            let file = string_option(options, "--file")?;
            let use_stdin = options.iter().any(|value| value == "--stdin");
            if file.is_some() && use_stdin {
                return Err(invalid_input("--file and --stdin are mutually exclusive"));
            }
            let policy_yaml = if use_stdin {
                let mut yaml = String::new();
                io::Read::read_to_string(&mut io::stdin().lock(), &mut yaml)?;
                yaml
            } else if let Some(file) = file {
                std::fs::read_to_string(file)?
            } else {
                return Err(invalid_input("policy apply requires --file or --stdin"));
            };
            Ok(Action::Request(Request {
                group: Some(group),
                policy_yaml: Some(policy_yaml),
                dry_run: options.iter().any(|value| value == "--dry-run"),
                ..request(Operation::Reload)
            }))
        }
        _ => Err(invalid_input(format!(
            "invalid policy command {subcommand:?}; expected apply"
        ))),
    }
}

fn parse_spawn(values: &[String]) -> Result<Action, io::Error> {
    let separator = values
        .iter()
        .position(|value| value == "--")
        .ok_or_else(|| invalid_input("spawn requires -- before the command"))?;
    let options = &values[..separator];
    validate_options(options, &["--domain"], &["--keep"])?;
    let command = values[separator + 1..].to_vec();
    if command.is_empty() {
        return Err(invalid_input("spawn command is empty"));
    }
    Ok(Action::Spawn {
        domain: string_option(options, "--domain")?,
        keep: options.iter().any(|value| value == "--keep"),
        command,
    })
}

fn spawn(
    socket: &Path,
    domain: Option<String>,
    keep: bool,
    command: &[String],
) -> Result<i32, Box<dyn Error>> {
    let mut child = Command::new(env::current_exe()?)
        .env(SPAWN_HELPER_ENV, "1")
        .args(command)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    let pid = child.id();
    let pidfd = match open_pidfd(pid) {
        Ok(pidfd) => pidfd,
        Err(error) => {
            terminate(&mut child, None);
            return Err(error.into());
        }
    };

    if let Err(error) = wait_stopped(pid) {
        terminate(&mut child, Some(&pidfd));
        return Err(error.into());
    }
    let response = match call(
        socket,
        &Request {
            pid: Some(pid),
            domain,
            ..request(Operation::Track)
        },
    ) {
        Ok(response) if response.ok => response,
        Ok(response) => {
            terminate(&mut child, Some(&pidfd));
            return Err(invalid_input(
                response
                    .error
                    .unwrap_or_else(|| "track request failed".into()),
            )
            .into());
        }
        Err(error) => {
            terminate(&mut child, Some(&pidfd));
            return Err(error);
        }
    };
    if let Err(error) = send_signal(&pidfd, Signal::CONT) {
        untrack(socket, pid);
        terminate(&mut child, Some(&pidfd));
        return Err(error.into());
    }
    if let Some(domain) = response.domain {
        println!(
            "tracking pid={pid} domain={} slot={} group={}",
            domain.name, domain.slot, domain.group
        );
    }

    let status = child.wait();
    if !keep {
        untrack(socket, pid);
    }
    // Transparently propagate the child's exit status: an interactive shell or a
    // command killed by Ctrl-C (130) must not surface as a ctl error.
    let status = status?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
}

fn wait_stopped(pid: u32) -> Result<(), io::Error> {
    let status =
        rustix::io::retry_on_intr(|| waitpid(Some(process_id(pid)?), WaitOptions::UNTRACED))
            .map_err(io::Error::from)?
            .ok_or_else(|| io::Error::other(format!("waitpid returned no status for pid {pid}")))?
            .1;
    if status.stopped() && status.stopping_signal() == Some(Signal::STOP.as_raw()) {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "spawn helper pid {pid} failed before its SIGSTOP barrier: {status:?}"
        )))
    }
}

fn open_pidfd(pid: u32) -> Result<OwnedFd, io::Error> {
    pidfd_open(process_id(pid)?, PidfdFlags::empty()).map_err(io::Error::from)
}

fn process_id(pid: u32) -> Result<Pid, rustix::io::Errno> {
    let raw = i32::try_from(pid).map_err(|_| rustix::io::Errno::RANGE)?;
    Pid::from_raw(raw).ok_or(rustix::io::Errno::RANGE)
}

fn send_signal(pidfd: &OwnedFd, signal: Signal) -> Result<(), io::Error> {
    pidfd_send_signal(pidfd, signal).map_err(io::Error::from)
}

fn terminate(child: &mut Child, pidfd: Option<&OwnedFd>) {
    let killed = pidfd.is_some_and(|fd| send_signal(fd, Signal::KILL).is_ok());
    if !killed {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn untrack(socket: &Path, pid: u32) {
    let _ = call(
        socket,
        &Request {
            pid: Some(pid),
            ..request(Operation::Untrack)
        },
    );
}

fn spawn_helper(command: &[String]) -> Result<(), Box<dyn Error>> {
    let (program, arguments) = command
        .split_first()
        .ok_or_else(|| invalid_input("spawn helper command is empty"))?;
    kill_process(getpid(), Signal::STOP).map_err(io::Error::from)?;
    Err(Command::new(program)
        .args(arguments)
        .env_remove(SPAWN_HELPER_ENV)
        .exec()
        .into())
}

fn request(op: Operation) -> Request {
    Request {
        version: VERSION,
        op,
        pid: None,
        domain: None,
        seed: false,
        policy_file: None,
        policy_yaml: None,
        group: None,
        dry_run: false,
        enable_file: None,
        enable_exec: None,
        enable_net: None,
        audit_file: None,
        audit_exec: None,
        audit_net: None,
    }
}

fn required_u32_option(values: &[String], name: &str) -> Result<u32, io::Error> {
    required_string_option(values, name)?
        .parse()
        .map_err(|_| invalid_input(format!("{name} must be an unsigned integer")))
}

fn required_string_option(values: &[String], name: &str) -> Result<String, io::Error> {
    string_option(values, name)?.ok_or_else(|| invalid_input(format!("{name} is required")))
}

fn string_option(values: &[String], name: &str) -> Result<Option<String>, io::Error> {
    let Some(index) = values.iter().position(|value| value == name) else {
        return Ok(None);
    };
    values
        .get(index + 1)
        .cloned()
        .map(Some)
        .ok_or_else(|| invalid_input(format!("{name} requires a value")))
}

fn validate_options(values: &[String], valued: &[&str], flags: &[&str]) -> Result<(), io::Error> {
    let mut seen = std::collections::HashSet::new();
    let mut index = 0;
    while let Some(option) = values.get(index) {
        if !seen.insert(option.as_str()) {
            return Err(invalid_input(format!("duplicate option {option:?}")));
        }
        if valued.contains(&option.as_str()) {
            values
                .get(index + 1)
                .filter(|value| !value.is_empty() && !value.starts_with("--"))
                .ok_or_else(|| invalid_input(format!("{option} requires a value")))?;
            index += 2;
        } else if flags.contains(&option.as_str()) {
            index += 1;
        } else {
            return Err(invalid_input(format!("unknown argument {option:?}")));
        }
    }
    Ok(())
}

fn call(path: &Path, request: &Request) -> Result<Response, Box<dyn Error>> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    write_json_line(&mut stream, request)?;
    Ok(read_json_line(&mut BufReader::new(stream))?)
}

fn audit(path: &Path, kind: Option<u32>, pid: Option<u32>) -> Result<(), Box<dyn Error>> {
    let stream = UnixStream::connect(path)?;
    let mut reader = BufReader::new(stream);
    println!("KIND   OP             PID      RESULT DOMAIN           COMM             DETAIL");
    loop {
        let event: Event = match read_json_line(&mut reader) {
            Ok(event) => event,
            Err(ProtocolError::EndOfStream) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if kind.is_some_and(|value| value != event.kind)
            || pid.is_some_and(|value| value != event.tgid)
        {
            continue;
        }
        let result = if event.allowed { "ALLOW" } else { "DENY" };
        let detail = if event.args.is_empty() {
            event.detail
        } else {
            format!("{} args={}", event.detail, event.args.join(" "))
        };
        println!(
            "{:<6} {:<14} {:<8} {:<6} {:<16} {:<16} {}",
            event_kind_name(event.kind),
            event_operation_name(event.kind, event.op),
            event.tgid,
            result,
            event.domain,
            event.comm,
            detail
        );
    }
}

fn event_kind_name(kind: u32) -> &'static str {
    match kind {
        1 => "FILE",
        2 => "EXEC",
        3 => "NET",
        4 => "GUARD",
        _ => "OTHER",
    }
}

fn event_operation_name(kind: u32, op: u32) -> &'static str {
    match (kind, op) {
        (1, 0) => "file",
        (1, 1) => "open_read",
        (1, 2) => "open_write",
        (1, 3) => "truncate",
        (1, 4) => "unlink",
        (1, 5) => "rmdir",
        (1, 6) => "rename",
        (1, 7) => "ftruncate",
        (1, 8) => "link",
        (1, 9) => "symlink",
        (1, 10) => "mkdir",
        (1, 11) => "chmod",
        (1, 12) => "chown",
        (1, 13) => "setxattr",
        (1, 14) => "mmap_write",
        (1, 15) => "removexattr",
        (1, 16) => "setacl",
        (1, 17) => "mknod",
        (1, 18) => "mprotect",
        (1, 19) => "fd_read",
        (1, 20) => "fd_write",
        (2, _) => "exec",
        (3, 0) => "connect",
        (3, 1) => "bind",
        (3, 2) => "sendmsg",
        (3, 3) => "accept",
        (3, 4) => "recvmsg",
        (4, 1) => "kill",
        (4, 2) => "ptrace",
        (4, 3) => "traceme",
        (4, 4) => "bpf",
        _ => "unknown",
    }
}

fn ensure_success(response: Response) -> Result<(), io::Error> {
    if response.ok {
        Ok(())
    } else {
        Err(invalid_input(
            response
                .error
                .unwrap_or_else(|| "daemon rejected request".into()),
        ))
    }
}

fn optional_u32_option(values: &[String], name: &str) -> Result<Option<u32>, io::Error> {
    string_option(values, name)?
        .map(|value| {
            value
                .parse()
                .map_err(|_| invalid_input(format!("{name} must be an unsigned integer")))
        })
        .transpose()
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_preserves_argument_boundaries() -> Result<(), io::Error> {
        let (_, action) = parse_args(
            [
                "spawn",
                "--domain",
                "lab-agent",
                "--",
                "/usr/bin/printf",
                "%s %s",
                "hello world",
            ]
            .into_iter()
            .map(str::to_owned),
        )?;
        assert_eq!(
            action,
            Action::Spawn {
                domain: Some("lab-agent".into()),
                keep: false,
                command: vec![
                    "/usr/bin/printf".into(),
                    "%s %s".into(),
                    "hello world".into()
                ],
            }
        );
        Ok(())
    }

    #[test]
    fn legacy_socket_flag_is_rejected() {
        assert!(
            parse_args(
                ["--sock", "/tmp/ctl.sock", "status"]
                    .into_iter()
                    .map(str::to_owned)
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_removed_aliases_and_malformed_options() {
        for args in [
            vec!["__spawn-helper", "/bin/true"],
            vec!["config", "show"],
            vec!["status", "extra"],
            vec!["audit", "--socket", "/tmp/events.sock"],
            vec!["audit", "--kind", "5"],
            vec!["attach", "--pid", "12", "--name", "worker"],
            vec!["untrack", "--pid", "12", "--extra"],
            vec!["bind", "worker", "--policy", "g"],
            vec!["bind", "--domain", "worker", "--group", "g"],
            vec!["reload", "--group", "g"],
            vec!["reload", "--policy", "g", "--stdin"],
            vec!["attach", "--pid", "12", "--policy", "g"],
            vec!["attach", "--pid", "12", "--seed"],
            vec!["spawn", "--policy", "g", "--", "/bin/true"],
            vec!["reload", "--file", "--dry-run"],
            vec!["policy", "apply", "--name", "g", "--stdin"],
            vec!["set", "--audit-file", "on", "--audit-file", "off"],
            vec!["spawn", "--scope", "worker", "--", "/bin/true"],
        ] {
            assert!(parse_args(args.into_iter().map(str::to_owned)).is_err());
        }
    }

    #[test]
    fn missing_explicit_config_is_rejected() {
        let parsed = parse_args(
            ["--config", "/does-not-exist/censorguard.yaml", "status"]
                .into_iter()
                .map(str::to_owned),
        );
        assert!(matches!(parsed, Err(error) if error.to_string().contains("invalid config")));
    }

    #[test]
    fn bind_uses_only_named_domain_and_policy() -> Result<(), io::Error> {
        let (_, action) = parse_args(
            ["bind", "--domain", "worker", "--policy", "restricted"]
                .into_iter()
                .map(str::to_owned),
        )?;
        assert_eq!(
            action,
            Action::Request(Request {
                domain: Some("worker".into()),
                group: Some("restricted".into()),
                ..request(Operation::Bind)
            })
        );
        Ok(())
    }
}
