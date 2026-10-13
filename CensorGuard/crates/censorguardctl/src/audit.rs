use censorguard_common::protocol::{Event, ProtocolError, read_json_line};
use std::error::Error;
use std::io::{self, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct Filter {
    pub kind: Option<u32>,
    pub pid: Option<u32>,
    pub domain: Option<String>,
    pub allowed: Option<bool>,
}

impl Filter {
    fn matches(&self, event: &Event) -> bool {
        self.kind.is_none_or(|kind| kind == event.kind)
            && self.pid.is_none_or(|pid| pid == event.tgid)
            && self
                .domain
                .as_ref()
                .is_none_or(|domain| domain == &event.domain)
            && self.allowed.is_none_or(|allowed| allowed == event.allowed)
    }

    fn description(&self) -> String {
        let mut parts = vec![self.kind.map_or("all kinds", event_kind_name).to_owned()];
        if let Some(domain) = &self.domain {
            parts.push(format!("domain={}", single_line(domain)));
        }
        if let Some(allowed) = self.allowed {
            parts.push(if allowed { "ALLOW" } else { "DENY" }.into());
        }
        if let Some(pid) = self.pid {
            parts.push(format!("PID={pid}"));
        }
        parts.join(", ")
    }
}

pub(crate) fn parse_kind(value: &str) -> Result<u32, io::Error> {
    match value {
        "1" | "file" => Ok(1),
        "2" | "exec" => Ok(2),
        "3" | "net" => Ok(3),
        "4" | "guard" => Ok(4),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--kind expects file|exec|net|guard (or 1|2|3|4)",
        )),
    }
}

pub(crate) fn follow(path: &Path, filter: &Filter) -> Result<(), Box<dyn Error>> {
    let stream = UnixStream::connect(path).map_err(|error| io::Error::other(format!(
        "Cannot reach CensorGuard audit: {error}. Check censorguardctl status and --config PATH."
    )))?;
    let mut reader = BufReader::new(stream);
    let mut output = io::stdout().lock();
    writeln!(
        output,
        "CensorGuard | Live audit ({})",
        filter.description()
    )?;
    writeln!(output)?;
    writeln!(
        output,
        "KIND   OP             PID      DECISION DOMAIN           COMM             DETAIL"
    )?;
    output.flush()?;
    loop {
        let event: Event = match read_json_line(&mut reader) {
            Ok(event) => event,
            Err(ProtocolError::EndOfStream) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        // Loss affects this subscriber even when the following event is filtered out.
        if event.dropped_before > 0 {
            writeln!(
                output,
                "! Audit stream lost {} events before sequence {}.",
                event.dropped_before, event.sequence
            )?;
            output.flush()?;
        }
        if filter.matches(&event) {
            write_event(&mut output, &event)?;
            output.flush()?;
        }
    }
}

fn write_event(output: &mut impl Write, event: &Event) -> io::Result<()> {
    let detail = if event.args.is_empty() {
        single_line(&event.detail)
    } else {
        format!(
            "{} args={}",
            single_line(&event.detail),
            event
                .args
                .iter()
                .map(|arg| single_line(arg))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    writeln!(
        output,
        "{:<6} {:<14} {:<8} {:<8} {:<16} {:<16} {}",
        event_kind_name(event.kind),
        event_operation_name(event.kind, event.op),
        event.tgid,
        if event.allowed { "ALLOW" } else { "DENY" },
        single_line(&event.domain),
        single_line(&event.comm),
        detail
    )
}

fn single_line(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| {
            if character.is_control() {
                character.escape_default().collect::<Vec<_>>()
            } else {
                vec![character]
            }
        })
        .collect()
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
