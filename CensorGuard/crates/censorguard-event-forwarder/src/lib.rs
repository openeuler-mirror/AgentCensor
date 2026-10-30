use std::collections::BTreeMap;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agentcensor_events::{
    AckFrame, EventFrame, EventSource, EventSubject, EventTarget, ExternalEvent, HelloFrame,
    PROTOCOL_VERSION, PolicyContext, SCHEMA_VERSION, SecurityCategory, SecurityDecision,
    SecurityEvent, read_frame, write_frame,
};
use censorguard_common::protocol::Event;

pub struct EventForwarder {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl EventForwarder {
    pub fn spawn(socket: PathBuf, receiver: Receiver<Event>) -> Result<Self, io::Error> {
        Self::spawn_with_source(socket, move |timeout| receiver.recv_timeout(timeout))
    }

    pub fn spawn_with_source(
        socket: PathBuf,
        receive: impl FnMut(Duration) -> Result<Event, RecvTimeoutError> + Send + 'static,
    ) -> Result<Self, io::Error> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let join = thread::Builder::new()
            .name("censorguard-event-forwarder".into())
            .spawn(move || run(socket, receive, thread_stop))?;
        Ok(Self {
            stop,
            join: Some(join),
        })
    }
}

impl Drop for EventForwarder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn run(
    socket: PathBuf,
    mut receive: impl FnMut(Duration) -> Result<Event, RecvTimeoutError>,
    stop: Arc<AtomicBool>,
) {
    let mut stream = None;
    while !stop.load(Ordering::Relaxed) {
        let event = match receive(Duration::from_millis(250)) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let external = convert(event);
        let frame = EventFrame::Event(external.clone());
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            if stream.is_none() {
                stream = connect(&socket, &external.source_instance_id);
            }
            let acknowledged = stream.as_mut().is_some_and(|connection| {
                write_frame(&mut *connection, &frame).is_ok()
                    && matches!(
                        read_frame(&mut *connection),
                        Ok(Some(EventFrame::Ack(AckFrame {
                            source_instance_id,
                            source_sequence,
                        }))) if source_instance_id == external.source_instance_id
                            && source_sequence == external.source_sequence
                    )
            });
            if acknowledged {
                break;
            }
            stream = None;
            thread::sleep(Duration::from_millis(500));
        }
    }
}

fn connect(socket: &Path, instance_id: &str) -> Option<UnixStream> {
    let mut stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .ok()?;
    let hello = EventFrame::Hello(HelloFrame {
        protocol_version: PROTOCOL_VERSION,
        producer: "censorguard".into(),
        instance_id: instance_id.to_string(),
    });
    write_frame(&mut stream, &hello).ok()?;
    Some(stream)
}

fn convert(event: Event) -> ExternalEvent {
    let category = match event.kind {
        1 => SecurityCategory::File,
        2 => SecurityCategory::Exec,
        3 => SecurityCategory::Network,
        _ => SecurityCategory::Guard,
    };
    let target = EventTarget {
        path: (event.kind == 1).then(|| event.detail.clone()),
        endpoint: (event.kind == 3).then(|| event.detail.clone()),
        executable: (event.kind == 2).then(|| event.detail.clone()),
        args: event.args,
        detail: (!event.detail.is_empty()).then_some(event.detail),
    };
    ExternalEvent {
        schema_version: SCHEMA_VERSION,
        source: EventSource::CensorGuard,
        source_instance_id: event.daemon_boot_id,
        source_sequence: event.sequence,
        occurred_at_ns: if event.occurred_at_ns == 0 {
            unix_nanos()
        } else {
            event.occurred_at_ns
        },
        subject: EventSubject {
            pid: event.pid,
            tgid: event.tgid,
            comm: (!event.comm.is_empty()).then_some(event.comm),
            domain_id: (event.domain_id != 0).then_some(event.domain_id),
            domain: (!event.domain.is_empty()).then_some(event.domain),
        },
        event: SecurityEvent {
            category,
            operation: operation_name(event.kind, event.op).into(),
            decision: if event.allowed {
                SecurityDecision::Allow
            } else {
                SecurityDecision::Deny
            },
            target,
            policy: Some(PolicyContext {
                policy_version: (event.policy_version != 0).then_some(event.policy_version),
                rule_version: (event.rule_version != 0).then_some(event.rule_version),
                rule_name: None,
            }),
        },
        correlation: None,
        attributes: BTreeMap::from([(
            "guard.dropped_before".into(),
            event.dropped_before.to_string(),
        )]),
    }
}

fn unix_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
        })
}

fn operation_name(kind: u32, op: u32) -> &'static str {
    match (kind, op) {
        (1, 1) => "file.open_read",
        (1, 2) => "file.open_write",
        (1, 3) => "file.truncate",
        (1, 4) => "file.unlink",
        (1, 5) => "file.rmdir",
        (1, 6) => "file.rename",
        (1, 7) => "file.ftruncate",
        (1, 8) => "file.link",
        (1, 9) => "file.symlink",
        (1, 10) => "file.mkdir",
        (1, 11) => "file.chmod",
        (1, 12) => "file.chown",
        (1, 13) => "file.setxattr",
        (1, 14) => "file.mmap_write",
        (1, 15) => "file.removexattr",
        (1, 16) => "file.setacl",
        (1, 17) => "file.mknod",
        (1, 18) => "file.mprotect",
        (1, 19) => "file.fd_read",
        (1, 20) => "file.fd_write",
        (1, _) => "file",
        (2, _) => "exec",
        (3, 0) => "network.connect",
        (3, 1) => "network.bind",
        (3, 2) => "network.sendmsg",
        (3, 3) => "network.accept",
        (3, 4) => "network.recvmsg",
        (3, _) => "network.unknown",
        (4, 1) => "guard.kill",
        (4, 2) => "guard.ptrace",
        (4, 3) => "guard.traceme",
        (4, 4) => "guard.bpf",
        _ => "guard.unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_deny_event() {
        let output = convert(Event {
            ts: String::new(),
            occurred_at_ns: 11,
            kind: 1,
            op: 1,
            pid: 5,
            tgid: 5,
            allowed: false,
            policy_version: 2,
            rule_version: 3,
            domain_id: 9,
            domain: "pivot-test".into(),
            comm: "cat".into(),
            detail: "/etc/shadow".into(),
            args: Vec::new(),
            sequence: 7,
            daemon_boot_id: "boot".into(),
            dropped_before: 0,
        });
        assert_eq!(output.event.operation, "file.open_read");
        assert_eq!(output.event.decision, SecurityDecision::Deny);
        assert_eq!(output.event.target.path.as_deref(), Some("/etc/shadow"));
        assert_eq!(output.occurred_at_ns, 11);
    }
}
