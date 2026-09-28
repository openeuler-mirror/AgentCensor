use crate::adapters::CommandAdapters;
use crate::config::Config;
use crate::engine::Engine;
use crate::error::{PivotError, Result};
use crate::model::{ClientReply, ClientRequest};
use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
use std::fs;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

pub type ProductionEngine = Engine<CommandAdapters, CommandAdapters, CommandAdapters>;

pub fn serve(engine: &ProductionEngine, adapters: &CommandAdapters, config: &Config) -> Result<()> {
    prepare_socket(&config.socket_path)?;
    let listener = UnixListener::bind(&config.socket_path)?;
    fs::set_permissions(
        &config.socket_path,
        fs::Permissions::from_mode(config.socket_mode),
    )?;
    let _socket_guard = SocketGuard(&config.socket_path);
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
                stream.set_write_timeout(Some(std::time::Duration::from_secs(10)))?;
                let reply = match handle(&mut stream, engine, adapters, config) {
                    Ok(reply) => reply,
                    Err(error) => ClientReply::Error {
                        code: error_code(&error).into(),
                        message: error.to_string(),
                    },
                };
                if let Err(error) = serde_json::to_writer(&mut stream, &reply)
                    .and_then(|_| stream.write_all(b"\n").map_err(serde_json::Error::io))
                {
                    eprintln!("censorpivot: failed to send reply: {error}");
                }
            }
            Err(error) => eprintln!("censorpivot: accept failed: {error}"),
        }
    }
    Ok(())
}

fn handle(
    stream: &mut UnixStream,
    engine: &ProductionEngine,
    adapters: &CommandAdapters,
    config: &Config,
) -> Result<ClientReply> {
    let actor_uid = getsockopt(&*stream, PeerCredentials)
        .map_err(|error| PivotError::Protocol(format!("cannot read peer credentials: {error}")))?
        .uid();
    let mut bytes = Vec::new();
    stream
        .take(config.max_frame_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > config.max_frame_bytes {
        return Err(PivotError::Protocol(
            "request frame exceeds configured limit".into(),
        ));
    }
    let request: ClientRequest = serde_json::from_slice(&bytes)?;
    if let ClientRequest::Execute { batch } = &request
        && !config
            .censorguard
            .allowed_groups
            .contains(&batch.guard_group)
    {
        return Err(PivotError::Invalid(
            "Guard group is not permitted by Pivot configuration".into(),
        ));
    }
    match request {
        ClientRequest::Execute { batch } => engine
            .execute(batch, actor_uid)
            .map(Box::new)
            .map(ClientReply::Transaction),
        ClientRequest::Status { transaction_id } => engine
            .status(transaction_id, actor_uid)
            .map(Box::new)
            .map(ClientReply::Transaction),
        ClientRequest::Recover => engine
            .recover(Some(actor_uid))
            .map(ClientReply::Transactions),
        ClientRequest::Doctor => Ok(ClientReply::Doctor(adapters.doctor_report())),
    }
}

fn prepare_socket(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| PivotError::Invalid("socket_path must have a parent".into()))?;
    fs::create_dir_all(parent)?;
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(PivotError::Conflict(format!(
                "another CensorPivot server is listening on {}",
                path.display()
            )));
        }
        fs::remove_file(path)?;
    }
    Ok(())
}

fn error_code(error: &PivotError) -> &'static str {
    match error {
        PivotError::Invalid(_) => "invalid_request",
        PivotError::Conflict(_) => "conflict",
        PivotError::Component { .. } => "component_failure",
        PivotError::Protocol(_) => "protocol_error",
        PivotError::Io(_) => "io_error",
        PivotError::Json(_) => "invalid_json",
    }
}

struct SocketGuard<'a>(&'a Path);

impl Drop for SocketGuard<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0);
    }
}

pub fn request(
    socket: &Path,
    request: &ClientRequest,
    max_reply_bytes: usize,
) -> Result<ClientReply> {
    let mut stream = UnixStream::connect(socket)?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    stream.shutdown(Shutdown::Write)?;
    let mut bytes = Vec::new();
    stream
        .take(u64::try_from(max_reply_bytes).unwrap_or(u64::MAX) + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_reply_bytes {
        return Err(PivotError::Protocol(
            "reply frame exceeds client limit".into(),
        ));
    }
    Ok(serde_json::from_slice(&bytes)?)
}
