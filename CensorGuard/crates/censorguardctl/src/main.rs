use censorguard_common::protocol::{
    DEFAULT_CONTROL_SOCKET, Operation, Request, Response, VERSION, read_json_line, write_json_line,
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

#[derive(Debug, Eq, PartialEq)]
enum Action {
    Request(Request),
    ApplyPolicy {
        policy: String,
        domain: Option<String>,
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
    let mut arguments = env::args().skip(1);
    if arguments.next().as_deref() == Some("__spawn-helper") {
        return spawn_helper(&arguments.collect::<Vec<_>>());
    }
    let (socket, action) = parse_args(env::args().skip(1))?;
    match action {
        Action::Request(request) => {
            let response = call(&socket, &request)?;
            println!("{}", serde_json::to_string_pretty(&response)?);
            ensure_success(response)?;
        }
        Action::ApplyPolicy {
            policy,
            domain,
            pid,
        } => {
            let response = call(
                &socket,
                &Request {
                    policy_file: Some(policy),
                    ..request(Operation::Reload)
                },
            )?;
            ensure_success(response)?;
            if let Some(pid) = pid {
                let response = call(
                    &socket,
                    &Request {
                        pid: Some(pid),
                        domain: Some(domain.unwrap_or_else(|| "default".into())),
                        ..request(Operation::Track)
                    },
                )?;
                println!("{}", serde_json::to_string_pretty(&response)?);
                ensure_success(response)?;
            } else if let Some(domain) = domain {
                let response = call(
                    &socket,
                    &Request {
                        domain: Some(domain),
                        group: Some(String::new()),
                        ..request(Operation::Bind)
                    },
                )?;
                println!("{}", serde_json::to_string_pretty(&response)?);
                ensure_success(response)?;
            }
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
    let mut socket = PathBuf::from(DEFAULT_CONTROL_SOCKET);
    if values
        .first()
        .is_some_and(|value| matches!(value.as_str(), "--socket" | "--sock" | "-sock"))
    {
        if values.len() < 2 {
            return Err(invalid_input("--socket requires a path"));
        }
        socket = values[1].clone().into();
        values.drain(..2);
    }
    if values.first().is_some_and(|value| value == "--policy") {
        let policy = values
            .get(1)
            .cloned()
            .ok_or_else(|| invalid_input("--policy requires a file"))?;
        let domain = string_option_alias(&values[2..], &["--domain", "--name"])?;
        let pid = optional_u32_option(&values[2..], "--pid")?;
        if domain.is_none() && pid.is_none() {
            return Err(invalid_input("--policy requires --domain or --pid"));
        }
        if domain.is_some() && pid.is_some() {
            return Err(invalid_input("--domain and --pid are mutually exclusive"));
        }
        return Ok((
            socket,
            Action::ApplyPolicy {
                policy,
                domain,
                pid,
            },
        ));
    }
    let Some(command) = values.first().map(String::as_str) else {
        return Err(invalid_input(
            "usage: censorguardctl [--socket PATH|-sock PATH] --policy FILE (--domain NAME|--pid PID)\n\
             or policy|config|run|spawn|status|attach|untrack|bind|set ...\n\
             set [--enable-file on|off] [--enable-exec on|off] [--enable-net on|off]\n\
             [--audit-file on|off] [--audit-exec on|off] [--audit-net on|off]",
        ));
    };
    let action = match command {
        "spawn" | "run" => parse_spawn(&values[1..])?,
        "status" if values.len() == 1 => Action::Request(request(Operation::Status)),
        "tree" if values.len() == 1 => Action::Request(request(Operation::Tree)),
        "policy-dump" if values.len() == 1 => Action::Request(request(Operation::PolicyDump)),
        "set" => Action::Request(parse_set(&values[1..])?),
        "policy" => parse_policy(&values[1..])?,
        "config" if values.get(1).is_some_and(|value| value == "show") => {
            Action::Request(request(Operation::Status))
        }
        "config" if values.get(1).is_some_and(|value| value == "set") => {
            return Err(invalid_input(
                "config set is not supported by this daemon; edit the policy YAML and use reload",
            ));
        }
        "doctor" if values.len() == 1 => Action::Request(request(Operation::Status)),
        "reload" => Action::Request(parse_reload(&values[1..])?),
        "attach" => {
            let options = &values[1..];
            let pid = required_u32_option(options, "--pid")?;
            let domain = string_option_alias(options, &["--domain", "--name"])?;
            Action::Attach {
                request: Request {
                    pid: Some(pid),
                    domain,
                    seed: options.iter().any(|value| value == "--seed"),
                    ..request(Operation::Track)
                },
                policy: string_option_alias(options, &["--policy", "--group"])?,
            }
        }
        "untrack" => Action::Request(Request {
            pid: Some(required_u32_option(&values[1..], "--pid")?),
            ..request(Operation::Untrack)
        }),
        "bind" => {
            let options = &values[1..];
            let domain = string_option_alias(options, &["--name", "--domain"])?
                .or_else(|| {
                    options
                        .first()
                        .filter(|value| !value.starts_with('-'))
                        .cloned()
                })
                .ok_or_else(|| invalid_input("bind requires --name DOMAIN (or DOMAIN)"))?;
            Action::Request(Request {
                domain: Some(domain),
                group: Some(
                    string_option_alias(options, &["--group", "--policy"])?.ok_or_else(|| {
                        invalid_input("bind requires --policy GROUP (or --group GROUP)")
                    })?,
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
        group: string_option(values, "--group")?,
        dry_run: values.iter().any(|value| value == "--dry-run"),
        ..request(Operation::Reload)
    })
}

fn parse_set(values: &[String]) -> Result<Request, io::Error> {
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
        return Err(invalid_input(
            "usage: censorguardctl policy apply|list|show ...",
        ));
    };
    match subcommand {
        "list" | "show" => Ok(Action::Request(request(Operation::PolicyDump))),
        "apply" => {
            let options = &values[1..];
            let group = required_string_option(options, "--name")?;
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
        "remove" => Err(invalid_input(
            "policy remove is not supported by this daemon; use bind --policy '' to unbind a domain",
        )),
        _ => Err(invalid_input(format!(
            "invalid policy command {subcommand:?}; expected apply, list, show or remove"
        ))),
    }
}

fn parse_spawn(values: &[String]) -> Result<Action, io::Error> {
    let separator = values
        .iter()
        .position(|value| value == "--")
        .ok_or_else(|| invalid_input("spawn requires -- before the command"))?;
    let options = &values[..separator];
    let command = values[separator + 1..].to_vec();
    if command.is_empty() {
        return Err(invalid_input("spawn command is empty"));
    }
    Ok(Action::Spawn {
        domain: string_option_alias(options, &["--domain", "--name"])?,
        policy: string_option_alias(options, &["--policy", "--group"])?,
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
        .arg("__spawn-helper")
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
    Err(Command::new(program).args(arguments).exec().into())
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

fn optional_u32_option(values: &[String], name: &str) -> Result<Option<u32>, io::Error> {
    string_option(values, name)?
        .map(|value| {
            value
                .parse()
                .map_err(|_| invalid_input(format!("{name} must be an unsigned integer")))
        })
        .transpose()
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

fn string_option_alias(values: &[String], names: &[&str]) -> Result<Option<String>, io::Error> {
    for name in names {
        if let Some(value) = string_option(values, name)? {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn call(path: &Path, request: &Request) -> Result<Response, Box<dyn Error>> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    write_json_line(&mut stream, request)?;
    Ok(read_json_line(&mut BufReader::new(stream))?)
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
    fn reference_cli_aliases_parse() -> Result<(), io::Error> {
        let (socket, action) = parse_args(
            [
                "-sock",
                "/tmp/ctl.sock",
                "run",
                "--name",
                "lab-agent",
                "--",
                "/bin/echo",
                "hello world",
            ]
            .into_iter()
            .map(str::to_owned),
        )?;
        assert_eq!(socket, std::path::PathBuf::from("/tmp/ctl.sock"));
        assert_eq!(
            action,
            Action::Spawn {
                domain: Some("lab-agent".into()),
                policy: None,
                keep: false,
                command: vec!["/bin/echo".into(), "hello world".into()],
            }
        );
        Ok(())
    }

    #[test]
    fn canonical_policy_command_accepts_domain_or_pid() -> Result<(), io::Error> {
        let (_, action) = parse_args(
            ["--policy", "base.yaml", "--domain", "worker"]
                .into_iter()
                .map(str::to_owned),
        )?;
        assert_eq!(
            action,
            Action::ApplyPolicy {
                policy: "base.yaml".into(),
                domain: Some("worker".into()),
                pid: None,
            }
        );
        let (_, action) = parse_args(
            ["--policy", "base.yaml", "--pid", "42"]
                .into_iter()
                .map(str::to_owned),
        )?;
        assert_eq!(
            action,
            Action::ApplyPolicy {
                policy: "base.yaml".into(),
                domain: None,
                pid: Some(42),
            }
        );
        Ok(())
    }
}
