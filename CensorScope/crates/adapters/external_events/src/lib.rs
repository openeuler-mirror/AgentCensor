//! Unix-socket ingress for versioned AgentCensor external events.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use agentcensor_events::{
    AckFrame, EventFrame, ExternalEvent, PROTOCOL_VERSION, read_frame, validate_event, write_frame,
};

pub type EventHandler = dyn Fn(ExternalEvent) -> Result<(), String> + Send + Sync + 'static;

pub struct ExternalEventServer {
    stop: Arc<AtomicBool>,
    socket_path: PathBuf,
    join: Option<JoinHandle<()>>,
}

impl ExternalEventServer {
    pub fn start(
        socket_path: &Path,
        socket_mode: u32,
        handler: Arc<EventHandler>,
    ) -> io::Result<Self> {
        prepare_socket(socket_path)?;
        let listener = UnixListener::bind(socket_path)?;
        listener.set_nonblocking(true)?;
        fs::set_permissions(socket_path, fs::Permissions::from_mode(socket_mode))?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let path = socket_path.to_path_buf();
        let join = thread::Builder::new()
            .name("censorscope-external-events".to_string())
            .spawn(move || run(listener, thread_stop, handler))?;
        Ok(Self {
            stop,
            socket_path: path,
            join: Some(join),
        })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = fs::remove_file(&self.socket_path);
    }
}

impl Drop for ExternalEventServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(listener: UnixListener, stop: Arc<AtomicBool>, handler: Arc<EventHandler>) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Err(error) = handle_connection(stream, &stop, &handler) {
                    tracing::warn!(error = %error, "external event connection failed");
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => {
                tracing::warn!(error = %error, "external event accept failed");
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn handle_connection(
    mut stream: UnixStream,
    stop: &AtomicBool,
    handler: &Arc<EventHandler>,
) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(250)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let Some(EventFrame::Hello(hello)) = read_frame(&mut stream).map_err(io::Error::other)? else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "external event connection requires hello",
        ));
    };
    if hello.protocol_version != PROTOCOL_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported external event protocol",
        ));
    }
    while !stop.load(Ordering::Relaxed) {
        let frame = match read_frame(&mut stream) {
            Ok(Some(frame)) => frame,
            Ok(None) => return Ok(()),
            Err(agentcensor_events::ContractError::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(error) => return Err(io::Error::other(error)),
        };
        let events = match frame {
            EventFrame::Event(event) => vec![event],
            EventFrame::Batch(events) => events,
            _ => continue,
        };
        for event in events {
            validate_event(&event).map_err(io::Error::other)?;
            let ack = AckFrame {
                source_instance_id: event.source_instance_id.clone(),
                source_sequence: event.source_sequence,
            };
            handler(event).map_err(io::Error::other)?;
            write_frame(&mut stream, &EventFrame::Ack(ack)).map_err(io::Error::other)?;
        }
    }
    Ok(())
}

fn prepare_socket(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("external event socket {} is active", path.display()),
            ));
        }
        fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicUsize;

    use agentcensor_events::{
        CorrelationContext, EventSource, EventSubject, EventTarget, HelloFrame, SCHEMA_VERSION,
        SecurityCategory, SecurityDecision, SecurityEvent,
    };

    use super::*;

    #[test]
    fn acknowledges_an_event_after_the_handler_accepts_it() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let socket = temporary.path().join("events.sock");
        let accepted = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&accepted);
        let mut server = ExternalEventServer::start(
            &socket,
            0o600,
            Arc::new(move |_| {
                counter.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }),
        )
        .expect("start server");
        let mut stream = UnixStream::connect(&socket).expect("connect");
        write_frame(
            &mut stream,
            &EventFrame::Hello(HelloFrame {
                protocol_version: PROTOCOL_VERSION,
                producer: "test".into(),
                instance_id: "producer".into(),
            }),
        )
        .expect("hello");
        write_frame(&mut stream, &EventFrame::Event(event())).expect("event");
        assert_eq!(
            read_frame(&mut stream).expect("ack"),
            Some(EventFrame::Ack(AckFrame {
                source_instance_id: "guard-boot".into(),
                source_sequence: 7,
            }))
        );
        assert_eq!(accepted.load(Ordering::Relaxed), 1);
        drop(stream);
        server.stop();
    }

    fn event() -> ExternalEvent {
        ExternalEvent {
            schema_version: SCHEMA_VERSION,
            source: EventSource::CensorGuard,
            source_instance_id: "guard-boot".into(),
            source_sequence: 7,
            occurred_at_ns: 10,
            subject: EventSubject {
                pid: 5,
                tgid: 5,
                comm: Some("test".into()),
                domain_id: Some(3),
                domain: Some("pivot-test".into()),
            },
            event: SecurityEvent {
                category: SecurityCategory::File,
                operation: "file.open_read".into(),
                decision: SecurityDecision::Deny,
                target: EventTarget {
                    path: Some("/etc/shadow".into()),
                    ..EventTarget::default()
                },
                policy: None,
            },
            correlation: Some(CorrelationContext {
                trace_id: Some(1),
                ..CorrelationContext::default()
            }),
            attributes: BTreeMap::new(),
        }
    }
}
