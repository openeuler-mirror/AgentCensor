//! Unix-domain-socket server adapter for daemon-side control handling.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;

use control_contract::command::ControlCommand;
use control_contract::reply::{ControlError, ControlReply};

const REQUEST_BUFFER_BYTES: usize = 1024 * 1024;

/// Kernel-provided identity of the process connected to the control socket.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerCredentials {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

impl PeerCredentials {
    /// Reads Linux `SO_PEERCRED` credentials from an accepted Unix socket.
    pub fn from_stream(stream: &UnixStream) -> std::io::Result<Self> {
        // SAFETY: `libc::ucred` is a plain C POD output buffer and zero is a
        // valid initialization before `getsockopt` fills it.
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: the descriptor is borrowed from a live `UnixStream`; all
        // output pointers reference writable, correctly sized local values.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let pid = u32::try_from(credentials.pid)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Self {
            pid,
            uid: credentials.uid,
            gid: credentials.gid,
        })
    }
}

/// Service implementation invoked for decoded control commands.
pub trait ControlService {
    /// Executes a decoded command when no socket peer identity is available.
    fn handle(&mut self, command: ControlCommand) -> Result<ControlReply, ControlError>;
    /// Executes a decoded command on behalf of an authenticated socket peer.
    fn handle_from_peer(
        &mut self,
        _peer: PeerCredentials,
        command: ControlCommand,
    ) -> Result<ControlReply, ControlError> {
        self.handle(command)
    }
}

/// Decoder and dispatcher for one daemon control service.
pub struct UdsControlServer<S> {
    service: S,
}
/// Incremental nonblocking connection state used by the daemon socket loop.
pub struct UdsControlConnection {
    stream: UnixStream,
    peer: PeerCredentials,
    request: Vec<u8>,
    reply: Option<Vec<u8>>,
    written: usize,
}

impl<S: ControlService> UdsControlServer<S> {
    /// Creates a decoder and dispatcher around a control service.
    pub fn new(service: S) -> Self {
        Self { service }
    }
    /// Handles one complete encoded request without an authenticated socket peer.
    pub fn handle_bytes(&mut self, request: &[u8]) -> Vec<u8> {
        self.handle_bytes_from_peer(request, None)
    }
    fn handle_bytes_from_peer(&mut self, request: &[u8], peer: Option<PeerCredentials>) -> Vec<u8> {
        let response = match uds_control_transport::decode_command(request) {
            Ok(command) => match peer {
                Some(peer) => self.service.handle_from_peer(peer, command),
                None => self.service.handle(command),
            },
            Err(error) => Err(ControlError::new(error.stage, error.message)),
        };
        uds_control_transport::encode_reply(&response)
    }
    /// Serves one blocking socket connection using kernel peer credentials.
    pub fn serve_connection(&mut self, stream: &mut UnixStream) -> std::io::Result<()> {
        let peer = PeerCredentials::from_stream(stream)?;
        let mut request = Vec::new();
        stream.read_to_end(&mut request)?;
        let reply = self.handle_bytes_from_peer(&request, Some(peer));
        stream.write_all(&reply)
    }
    pub fn service_mut(&mut self) -> &mut S {
        &mut self.service
    }
}

impl UdsControlConnection {
    /// Creates nonblocking connection state and captures its kernel peer credentials.
    pub fn new(stream: UnixStream) -> std::io::Result<Self> {
        let peer = PeerCredentials::from_stream(&stream)?;
        Ok(Self {
            stream,
            peer,
            request: Vec::new(),
            reply: None,
            written: 0,
        })
    }
    pub fn raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }
    /// Advances request reading or reply writing until blocked or complete.
    ///
    /// Returns `true` once the connection should be removed from the socket loop.
    pub fn try_progress<S: ControlService>(
        &mut self,
        server: &mut UdsControlServer<S>,
    ) -> std::io::Result<bool> {
        if self.reply.is_none() {
            let mut chunk = [0_u8; 8192];
            loop {
                match self.stream.read(&mut chunk) {
                    Ok(0) if self.request.is_empty() => return Ok(true),
                    Ok(0) => {
                        self.reply =
                            Some(server.handle_bytes_from_peer(&self.request, Some(self.peer)));
                        break;
                    }
                    Ok(count) => {
                        if self.request.len().saturating_add(count) > REQUEST_BUFFER_BYTES {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "control request exceeds 1 MiB",
                            ));
                        }
                        self.request.extend_from_slice(&chunk[..count]);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        return Ok(false);
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        let reply = self.reply.as_ref().expect("reply initialized");
        while self.written < reply.len() {
            match self.stream.write(&reply[self.written..]) {
                Ok(0) => return Ok(true),
                Ok(count) => self.written += count,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use control_contract::command::{ControlCommand, DoctorCommand};
    use control_contract::reply::DoctorReply;
    use model_core::ids::RequestId;

    struct DoctorService;
    impl ControlService for DoctorService {
        fn handle(&mut self, _command: ControlCommand) -> Result<ControlReply, ControlError> {
            Ok(ControlReply::Doctor(DoctorReply {
                available_collectors: vec!["ebpf".into()],
                storage_ready: true,
            }))
        }
    }

    #[test]
    fn handles_doctor_frame() {
        let command = ControlCommand::Doctor(DoctorCommand {
            request_id: RequestId::new(1),
        });
        let mut server = UdsControlServer::new(DoctorService);
        let reply = uds_control_transport::decode_reply(
            &server.handle_bytes(&uds_control_transport::encode_command(&command)),
        )
        .unwrap()
        .unwrap();
        assert!(matches!(reply, ControlReply::Doctor(_)));
    }
}
