use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use agentcensor_events::{
    AckFrame, EventFrame, PROTOCOL_VERSION, read_frame, validate_event, write_frame,
};

use super::EventRegistry;

pub struct EventBridge {
    socket_path: PathBuf,
}

impl EventBridge {
    pub fn start(
        guard_socket: &Path,
        guard_socket_mode: u32,
        scope_socket: PathBuf,
        registry: Arc<EventRegistry>,
    ) -> io::Result<Self> {
        prepare_socket(guard_socket)?;
        let listener = UnixListener::bind(guard_socket)?;
        fs::set_permissions(guard_socket, fs::Permissions::from_mode(guard_socket_mode))?;
        let sink = Arc::new(ScopeSink::new(scope_socket));
        thread::Builder::new()
            .name("censorpivot-guard-events".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(stream) => {
                            let registry = Arc::clone(&registry);
                            let sink = Arc::clone(&sink);
                            let _ = thread::Builder::new()
                                .name("censorpivot-guard-event-client".into())
                                .spawn(move || handle_guard(stream, &registry, &sink));
                        }
                        Err(error) => eprintln!("censorpivot: Guard event accept failed: {error}"),
                    }
                }
            })?;
        Ok(Self {
            socket_path: guard_socket.to_path_buf(),
        })
    }
}

impl Drop for EventBridge {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket_path);
    }
}

fn handle_guard(mut stream: UnixStream, registry: &EventRegistry, sink: &ScopeSink) {
    let Some(EventFrame::Hello(hello)) = read_frame(&mut stream).ok().flatten() else {
        return;
    };
    if hello.protocol_version != PROTOCOL_VERSION {
        return;
    }
    while let Ok(Some(frame)) = read_frame(&mut stream) {
        let events = match frame {
            EventFrame::Event(event) => vec![event],
            EventFrame::Batch(events) => events,
            _ => continue,
        };
        for mut event in events {
            if validate_event(&event).is_err() {
                continue;
            }
            event.correlation = registry.correlate(&event);
            let ack = AckFrame {
                source_instance_id: event.source_instance_id.clone(),
                source_sequence: event.source_sequence,
            };
            if event
                .correlation
                .as_ref()
                .and_then(|correlation| correlation.trace_id)
                .is_none()
            {
                if write_frame(&mut stream, &EventFrame::Ack(ack)).is_err() {
                    return;
                }
                continue;
            }
            if let Err(error) = sink.send(event) {
                eprintln!("censorpivot: Scope event forwarding failed: {error}");
                return;
            }
            if write_frame(&mut stream, &EventFrame::Ack(ack)).is_err() {
                return;
            }
        }
    }
}

struct ScopeSink {
    socket: PathBuf,
    connection: Mutex<Option<UnixStream>>,
}

impl ScopeSink {
    fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            connection: Mutex::new(None),
        }
    }

    fn send(&self, event: agentcensor_events::ExternalEvent) -> io::Result<()> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| io::Error::other("Scope event connection lock poisoned"))?;
        for _ in 0..2 {
            if connection.is_none() {
                *connection = Some(self.connect()?);
            }
            let Some(stream) = connection.as_mut() else {
                continue;
            };
            let result = write_frame(&mut *stream, &EventFrame::Event(event.clone()))
                .and_then(|()| read_frame(&mut *stream));
            match result {
                Ok(Some(EventFrame::Ack(AckFrame {
                    source_instance_id,
                    source_sequence,
                }))) if source_instance_id == event.source_instance_id
                    && source_sequence == event.source_sequence =>
                {
                    return Ok(());
                }
                _ => *connection = None,
            }
        }
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "Scope did not acknowledge the event",
        ))
    }

    fn connect(&self) -> io::Result<UnixStream> {
        let mut stream = UnixStream::connect(&self.socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        write_frame(
            &mut stream,
            &EventFrame::Hello(agentcensor_events::HelloFrame {
                protocol_version: PROTOCOL_VERSION,
                producer: "censorpivot".into(),
                instance_id: std::process::id().to_string(),
            }),
        )
        .map_err(io::Error::other)?;
        Ok(stream)
    }
}

fn prepare_socket(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("event socket {} is active", path.display()),
            ));
        }
        fs::remove_file(path)?;
    }
    Ok(())
}
