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

// The stopped child is an internal process mode, not a public CLI command.
const SPAWN_HELPER_ENV: &str = "_CENSORGUARD_SPAWN_HELPER";

#[derive(Debug, Eq, PartialEq)]
enum Action {
    Request(Request),
    Audit {
        socket: PathBuf,
        kind: Option<u32>,
        pid: Option<u32>,
    },
    Attach {
        request: Request,
        policy: Option<String>,
    },
    Spawn {
        domain: Option<String>,
        policy: Option<String>,
        keep: bool,
        command: Vec<String>,
    },
}

fn main() -> Result<(), Box<dyn Error>> {
    if env::var_os(SPAWN_HELPER_ENV).as_deref() == Some(std::ffi::OsStr::new("1")) {
        return spawn_helper(&env::args().skip(1).collect::<Vec<_>>());
    }
    if matches!(env::args().nth(1).as_deref(), Some("-h" | "--help")) {
        println!(
            "usage: censorguardctl [--config PATH] <status|tree|policy-dump|reload|set|policy|attach|untrack|bind|spawn|audit> ..."
        );
        return Ok(());
    }
    let (socket, action) = parse_args(env::args().skip(1))?;
    match action {
        Action::Audit { socket, kind, pid } => return audit(&socket, kind, pid),
        Action::Request(request) => {
            let response = call(&socket, &request)?;
            println!("{}", serde_json::to_string_pretty(&response)?);
            ensure_success(response)?;
        }
        Action::Attach {
            request: attach_request,
            policy,
        } => {
            if let Some(group) = policy {
                let response = call(
                    &socket,
                    &Request {
                        domain: attach_request.domain.clone(),
                        group: Some(group),
                        ..request(Operation::Bind)
                    },
                )?;
                ensure_success(response)?;
            }
            let response = call(&socket, &attach_request)?;
            println!("{}", serde_json::to_string_pretty(&response)?);
            ensure_success(response)?;
        }
        Action::Spawn {
            domain,
            policy,
            keep,
            command,
        } => std::process::exit(spawn(&socket, domain, policy, keep, &command)?),
    }
    Ok(())
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
            "usage: censorguardctl [--config PATH] <status|tree|policy-dump|reload|set|policy|attach|untrack|bind|spawn|audit> ...\n\
             set [--enable-file on|off] [--enable-exec on|off] [--enable-net on|off]\n\
             [--audit-file on|off] [--audit-exec on|off] [--audit-net on|off]",
        ));
    };
    let action = match command {
        "spawn" => parse_spawn(&values[1..])?,
        "status" if values.len() == 1 => Action::Request(request(Operation::Status)),
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
        "tree" if values.len() == 1 => Action::Request(request(Operation::Tree)),
        "policy-dump" if values.len() == 1 => Action::Request(request(Operation::PolicyDump)),
        "set" => Action::Request(parse_set(&values[1..])?),
        "policy" => parse_policy(&values[1..])?,
        "reload" => Action::Request(parse_reload(&values[1..])?),
        "attach" => {
            let options = &values[1..];
            validate_options(options, &["--pid", "--domain", "--policy"], &["--seed"])?;
            let pid = required_u32_option(options, "--pid")?;
            let domain = string_option(options, "--domain")?;
            Action::Attach {
                request: Request {
                    pid: Some(pid),
                    domain,
                    seed: options.iter().any(|value| value == "--seed"),
                    ..request(Operation::Track)
                },
                policy: string_option(options, "--policy")?,
            }
        }
        "untrack" => {
            validate_options(&values[1..], &["--pid"], &[])?;
            Action::Request(Request {
                pid: Some(required_u32_option(&values[1..], "--pid")?),
                ..request(Operation::Untrack)
            })
        }
        "bind" => {
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
                ..request(Operation::Bind)
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

fn parse_reload(values: &[String]) -> Result<Request, io::Error> {
    validate_options(values, &["--file", "--policy"], &["--stdin", "--dry-run"])?;
    let policy_file = string_option(values, "--file")?;
    let use_stdin = values.iter().any(|value| value == "--stdin");
    if policy_file.is_some() && use_stdin {
        return Err(invalid_input("--file and --stdin are mutually exclusive"));
    }
    let policy_yaml = if use_stdin {
        let mut yaml = String::new();
        io::Read::read_to_string(&mut io::stdin().lock(), &mut yaml)?;
        Some(yaml)
    } else {
        None
    };
    Ok(Request {
        policy_file,
        policy_yaml,
        group: string_option(values, "--policy")?,
        dry_run: values.iter().any(|value| value == "--dry-run"),
        ..request(Operation::Reload)
    })
}

fn parse_set(values: &[String]) -> Result<Request, io::Error> {
    validate_options(
        values,
        &[
            "--enable-file",
            "--enable-exec",
            "--enable-net",
            "--audit-file",
            "--audit-exec",
            "--audit-net",
        ],
        &[],
    )?;
    let enable_file = bool_option(values, "--enable-file")?;
    let enable_exec = bool_option(values, "--enable-exec")?;
    let enable_net = bool_option(values, "--enable-net")?;
    let audit_file = bool_option(values, "--audit-file")?;
    let audit_exec = bool_option(values, "--audit-exec")?;
    let audit_net = bool_option(values, "--audit-net")?;
    if [
        enable_file,
        enable_exec,
        enable_net,
        audit_file,
        audit_exec,
        audit_net,
    ]
    .iter()
    .all(Option::is_none)
    {
        return Err(invalid_input(
            "set requires at least one switch, e.g. set --audit-file on",
        ));
    }
    Ok(Request {
        enable_file,
        enable_exec,
        enable_net,
        audit_file,
        audit_exec,
        audit_net,
        ..request(Operation::SetSwitches)
    })
}

fn bool_option(values: &[String], name: &str) -> Result<Option<bool>, io::Error> {
    string_option(values, name)?
        .map(|value| match value.as_str() {
            "on" => Ok(true),
            "off" => Ok(false),
            _ => Err(invalid_input(format!(
                "{name} expects on|off, got {value:?}"
            ))),
        })
        .transpose()
}

fn parse_policy(values: &[String]) -> Result<Action, io::Error> {
    let Some(subcommand) = values.first().map(String::as_str) else {
        return Err(invalid_input("usage: censorguardctl policy apply ..."));
    };
    match subcommand {
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
    validate_options(options, &["--domain", "--policy"], &["--keep"])?;
    let command = values[separator + 1..].to_vec();
    if command.is_empty() {
        return Err(invalid_input("spawn command is empty"));
    }
    Ok(Action::Spawn {
        domain: string_option(options, "--domain")?,
        policy: string_option(options, "--policy")?,
        keep: options.iter().any(|value| value == "--keep"),
        command,
    })
}

fn spawn(
    socket: &Path,
    domain: Option<String>,
    policy: Option<String>,
    keep: bool,
    command: &[String],
) -> Result<i32, Box<dyn Error>> {
    if let Some(group) = policy.as_deref() {
        let response = call(
            socket,
            &Request {
                domain: domain.clone(),
                group: Some(group.to_owned()),
                ..request(Operation::Bind)
            },
        )?;
        ensure_success(response)?;
    }
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
                policy: None,
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
