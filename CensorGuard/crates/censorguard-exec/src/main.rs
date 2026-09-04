use censorguard_common::protocol::{
    DEFAULT_LAUNCH_SOCKET, RpcMethod, RpcParams, RpcRequest, RpcResponse, VERSION, read_json_line,
    write_json_line,
};
use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::io::{self, BufReader};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FailurePolicy {
    Deny,
    AllowWithAudit,
}

#[derive(Debug)]
struct Args {
    socket: PathBuf,
    scope: String,
    group: String,
    failure_policy: FailurePolicy,
    command: Vec<OsString>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args(env::args_os().skip(1))?;
    if let Err(error) = register(&args) {
        match args.failure_policy {
            FailurePolicy::Deny => return Err(error),
            FailurePolicy::AllowWithAudit => {
                eprintln!("censorguard-exec: registration failed; fail-open requested: {error}");
            }
        }
    }
    let executable = args
        .command
        .first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "command is empty"))?;
    let mut command = Command::new(executable);
    command.args(&args.command[1..]);
    for key in transient_environment_keys() {
        command.env_remove(key);
    }
    let error = command.exec();
    Err(error.into())
}

fn register(args: &Args) -> Result<(), Box<dyn Error>> {
    let mut stream = UnixStream::connect(&args.socket)?;
    let timeout = Some(Duration::from_secs(10));
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(timeout)?;
    let request_id = request_id();
    let request = RpcRequest {
        version: VERSION,
        request_id: request_id.clone(),
        method: RpcMethod::RegisterSelf,
        params: RpcParams {
            scope: Some(args.scope.clone()),
            group: Some(args.group.clone()),
            ..RpcParams::default()
        },
    };
    write_json_line(&mut stream, &request)?;
    let response: RpcResponse = read_json_line(&mut BufReader::new(&stream))?;
    if response.request_id != request_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "daemon returned a mismatched request_id",
        )
        .into());
    }
    if response.ok {
        Ok(())
    } else {
        let message = response
            .error
            .map_or_else(|| "registration failed".into(), |error| error.message);
        Err(io::Error::new(io::ErrorKind::PermissionDenied, message).into())
    }
}

fn parse_args(arguments: impl Iterator<Item = OsString>) -> Result<Args, io::Error> {
    let values: Vec<_> = arguments.collect();
    let separator = values
        .iter()
        .position(|value| value == "--")
        .ok_or_else(|| invalid_input("expected -- before command"))?;
    let mut socket = PathBuf::from(DEFAULT_LAUNCH_SOCKET);
    let mut scope = None;
    let mut group = String::new();
    let mut failure_policy = FailurePolicy::Deny;
    let mut index = 0;
    while index < separator {
        let option = values[index]
            .to_str()
            .ok_or_else(|| invalid_input("launcher option is not valid UTF-8"))?;
        match option {
            "--socket" | "--scope" | "--group" | "--failure-policy" => {
                let value = values
                    .get(index + 1)
                    .ok_or_else(|| invalid_input(format!("{option} requires a value")))?;
                if index + 1 >= separator {
                    return Err(invalid_input(format!("{option} requires a value")));
                }
                match option {
                    "--socket" => socket = PathBuf::from(value),
                    "--scope" => scope = Some(utf8(value, "--scope")?.to_owned()),
                    "--group" => group = utf8(value, "--group")?.to_owned(),
                    "--failure-policy" => {
                        failure_policy = match utf8(value, "--failure-policy")? {
                            "deny" => FailurePolicy::Deny,
                            "allow-with-audit" => FailurePolicy::AllowWithAudit,
                            _ => {
                                return Err(invalid_input(
                                    "--failure-policy must be deny or allow-with-audit",
                                ));
                            }
                        }
                    }
                    _ => unreachable!(),
                }
                index += 2;
            }
            _ => return Err(invalid_input(format!("unknown launcher option {option}"))),
        }
    }
    let command = values[separator + 1..].to_vec();
    if command.is_empty() {
        return Err(invalid_input("command is empty"));
    }
    Ok(Args {
        socket,
        scope: scope.ok_or_else(|| invalid_input("--scope is required"))?,
        group,
        failure_policy,
        command,
    })
}

fn utf8<'a>(value: &'a OsString, option: &str) -> Result<&'a str, io::Error> {
    value
        .to_str()
        .ok_or_else(|| invalid_input(format!("{option} value is not valid UTF-8")))
}

fn transient_environment_keys() -> Vec<OsString> {
    env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| key.to_string_lossy().starts_with("CENSORGUARD_"))
        .collect()
}

fn request_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("launcher-{}-{nanos}", std::process::id())
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_preserves_command_boundaries() -> Result<(), io::Error> {
        let args = parse_args(
            [
                "--scope",
                "session-1",
                "--group",
                "strict",
                "--",
                "/bin/echo",
                "hello world",
            ]
            .into_iter()
            .map(OsString::from),
        )?;
        assert_eq!(args.scope, "session-1");
        assert_eq!(args.group, "strict");
        assert_eq!(args.command[1], "hello world");
        Ok(())
    }
}
