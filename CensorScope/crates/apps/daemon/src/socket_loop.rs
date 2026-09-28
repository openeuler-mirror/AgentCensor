//! Nonblocking Unix-socket event loop for control requests and spool draining.

use std::fs::{self, Permissions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;

use config_core::daemon::SocketPermissions;
use uds_control_server::UdsControlConnection;

use crate::bootstrap::LocalDaemonServer;

/// Idle poll timeout in milliseconds: the longest a durable record waits for
/// the next independent drain step when no control socket is active.
const IDLE_POLL_MS: libc::c_int = 10;

/// Failure raised while binding or serving the daemon control socket.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonRunError {
    pub stage: String,
    pub message: String,
}

impl DaemonRunError {
    fn new(stage: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            stage: stage.into(),
            message: message.into(),
        }
    }
}

impl LocalDaemonServer {
    /// Serve control connections until the supplied shutdown predicate becomes true.
    pub fn serve_forever_until<S, R>(
        &mut self,
        socket_path: &Path,
        permissions: SocketPermissions,
        pending_connection_max: u32,
        mut should_stop: S,
        on_ready: R,
    ) -> Result<(), DaemonRunError>
    where
        S: FnMut() -> bool,
        R: FnOnce() -> Result<(), DaemonRunError>,
    {
        let listener = bind_listener(socket_path, permissions)?;
        let max = usize::try_from(pending_connection_max)
            .map_err(|error| DaemonRunError::new("pending_connection_max", error.to_string()))?;
        let mut connections = Vec::new();
        on_ready()?;
        while !should_stop() {
            let mut pollfds = vec![libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            }];
            pollfds.extend(connections.iter().map(|connection: &UdsControlConnection| {
                libc::pollfd {
                    fd: connection.raw_fd(),
                    events: libc::POLLIN | libc::POLLOUT,
                    revents: 0,
                }
            }));
            // The timeout bounds how long a durable record waits for the next
            // spool drain while control connections remain independently paced.
            // SAFETY: `pollfds` remains exclusively borrowed for the syscall,
            // its length matches the initialized array, and descriptors come
            // from live listener/connection objects.
            let result =
                unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as _, IDLE_POLL_MS) };
            if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return Err(DaemonRunError::new(
                    "poll",
                    io::Error::last_os_error().to_string(),
                ));
            }
            if pollfds[0].revents & libc::POLLIN != 0 {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) if connections.len() >= max => drop(stream),
                        Ok((stream, _)) => {
                            stream.set_nonblocking(true).map_err(|error| {
                                DaemonRunError::new("control_nonblocking", error.to_string())
                            })?;
                            connections.push(UdsControlConnection::new(stream).map_err(
                                |error| DaemonRunError::new("control_peer", error.to_string()),
                            )?);
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                        Err(error) => return Err(DaemonRunError::new("accept", error.to_string())),
                    }
                }
            }
            let mut index = 1;
            connections.retain_mut(|connection| {
                let ready = pollfds.get(index).is_some_and(|poll| poll.revents != 0);
                index += 1;
                if connection.is_idle_expired() {
                    return false;
                }
                if !ready {
                    return true;
                }
                match self.progress_control_connection(connection) {
                    Ok(done) => !done,
                    Err(error) => {
                        tracing::warn!(error = %error, "control connection failed");
                        false
                    }
                }
            });
        }
        Ok(())
    }
}

fn bind_listener(
    path: &Path,
    permissions: SocketPermissions,
) -> Result<UnixListener, DaemonRunError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| DaemonRunError::new("bind", error.to_string()))?;
    }
    let listener =
        UnixListener::bind(path).map_err(|error| DaemonRunError::new("bind", error.to_string()))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| DaemonRunError::new("bind", error.to_string()))?;
    fs::set_permissions(path, Permissions::from_mode(permissions.mode))
        .map_err(|error| DaemonRunError::new("permissions", error.to_string()))?;
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::*;
    use config_core::daemon::DEFAULT_SOCKET_MODE;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn control_socket_uses_operator_accessible_default_mode() {
        let path = std::env::temp_dir().join(format!(
            "censorscope-socket-permissions-{}",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let listener = bind_listener(
            &path,
            SocketPermissions {
                mode: DEFAULT_SOCKET_MODE,
            },
        )
        .expect("bind control socket");
        assert_eq!(
            fs::metadata(&path).expect("socket metadata").mode() & 0o777,
            0o666
        );
        drop(listener);
        fs::remove_file(path).expect("remove control socket");
    }
}
