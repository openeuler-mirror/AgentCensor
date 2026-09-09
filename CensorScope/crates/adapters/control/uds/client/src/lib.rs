//! Unix-domain-socket client adapter for control-plane consumers.

use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use control_contract::command::ControlCommand;
use control_contract::reply::{ControlError, ControlReply};

/// Transport that sends one complete request and receives one complete reply.
pub trait RoundTripTransport {
    /// Sends one encoded request and waits for its complete encoded reply.
    fn send(&mut self, request: Vec<u8>) -> Result<Vec<u8>, String>;
}

/// Unix-domain-socket implementation of the control transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UdsSocketTransport {
    socket_path: PathBuf,
}

impl UdsSocketTransport {
    /// Creates a transport that opens a fresh connection for each request.
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl RoundTripTransport for UdsSocketTransport {
    fn send(&mut self, request: Vec<u8>) -> Result<Vec<u8>, String> {
        let mut stream =
            UnixStream::connect(&self.socket_path).map_err(|error| error.to_string())?;
        stream
            .write_all(&request)
            .and_then(|_| stream.shutdown(Shutdown::Write))
            .map_err(|error| error.to_string())?;

        let mut reply = Vec::new();
        stream
            .read_to_end(&mut reply)
            .map_err(|error| error.to_string())?;
        Ok(reply)
    }
}

/// Typed client facade over a control-plane transport.
pub struct UdsControlClient<T> {
    transport: T,
}

impl<T> UdsControlClient<T>
where
    T: RoundTripTransport,
{
    /// Wraps a request/reply transport with typed control-plane encoding.
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    /// Sends one typed command and decodes its typed daemon reply.
    pub fn send(&mut self, command: ControlCommand) -> Result<ControlReply, ControlError> {
        let request = uds_control_transport::encode_command(&command);
        let bytes = self
            .transport
            .send(request)
            .map_err(|error| ControlError::new("transport", error))?;
        uds_control_transport::decode_reply(&bytes)
            .map_err(|error| ControlError::new(error.stage, error.message))?
    }
}
