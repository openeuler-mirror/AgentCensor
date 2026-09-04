use censorguard_common::protocol::{DEFAULT_EVENT_SOCKET, Event, ProtocolError, read_json_line};
use std::env;
use std::error::Error;
use std::io::{self, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

#[derive(Debug)]
struct Args {
    socket: PathBuf,
    kind: Option<u32>,
    pid: Option<u32>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut values = env::args().skip(1);
    // `censorguard-audit launch` is the single supported observation entrypoint.
    // Keep the filters as optional flags after the subcommand for scripting parity.
    if values.next().as_deref() != Some("launch") {
        return Err(invalid_input(
            "usage: censorguard-audit launch [--socket PATH] [--kind N] [--pid N]",
        )
        .into());
    }
    let args = parse_args(values)?;
    let stream = UnixStream::connect(&args.socket)?;
    let mut reader = BufReader::new(stream);
    println!("KIND   OP             PID      RESULT DOMAIN           COMM             DETAIL");
    loop {
        let event: Event = match read_json_line(&mut reader) {
            Ok(event) => event,
            Err(ProtocolError::EndOfStream) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if args.kind.is_some_and(|kind| kind != event.kind)
            || args.pid.is_some_and(|pid| pid != event.tgid)
        {
            continue;
        }
        render(&event);
    }
}

fn parse_args(mut values: impl Iterator<Item = String>) -> Result<Args, io::Error> {
    let mut args = Args {
        socket: DEFAULT_EVENT_SOCKET.into(),
        kind: None,
        pid: None,
    };
    while let Some(value) = values.next() {
        match value.as_str() {
            "-h" | "--help" => {
                println!("usage: censorguard-audit launch [--socket PATH] [--kind N] [--pid N]");
                std::process::exit(0);
            }
            "--socket" => args.socket = required_next(&mut values, "--socket")?.into(),
            "--kind" => args.kind = Some(parse_u32(required_next(&mut values, "--kind")?)?),
            "--pid" => args.pid = Some(parse_u32(required_next(&mut values, "--pid")?)?),
            _ => return Err(invalid_input(format!("unknown argument {value:?}"))),
        }
    }
    Ok(args)
}

fn required_next(
    values: &mut impl Iterator<Item = String>,
    name: &str,
) -> Result<String, io::Error> {
    values
        .next()
        .ok_or_else(|| invalid_input(format!("{name} requires a value")))
}

fn parse_u32(value: String) -> Result<u32, io::Error> {
    value
        .parse()
        .map_err(|_| invalid_input(format!("expected an unsigned integer, got {value:?}")))
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn render(event: &Event) {
    let result = if event.allowed { "ALLOW" } else { "DENY" };
    let detail = if event.args.is_empty() {
        event.detail.clone()
    } else {
        format!("{} args={}", event.detail, event.args.join(" "))
    };
    println!(
        "{:<6} {:<14} {:<8} {:<6} {:<16} {:<16} {}",
        kind_name(event.kind),
        operation_name(event.kind, event.op),
        event.tgid,
        result,
        event.domain,
        event.comm,
        detail
    );
}

fn kind_name(kind: u32) -> &'static str {
    match kind {
        1 => "FILE",
        2 => "EXEC",
        3 => "NET",
        4 => "GUARD",
        _ => "OTHER",
    }
}

fn operation_name(kind: u32, op: u32) -> &'static str {
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
        (3, _) => "connect",
        (4, 1) => "kill",
        (4, 2) => "ptrace",
        (4, 3) => "traceme",
        (4, 4) => "bpf",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_operation_names_cover_security_kinds() {
        assert_eq!(operation_name(1, 6), "rename");
        assert_eq!(operation_name(1, 19), "fd_read");
        assert_eq!(operation_name(1, 20), "fd_write");
        assert_eq!(operation_name(2, 0), "exec");
        assert_eq!(operation_name(3, 0), "connect");
        assert_eq!(operation_name(4, 4), "bpf");
    }
}
