//! Ownership of daemon endpoints and synchronous lifecycle requests.
use censorguard_common::protocol::{Operation, Response, VERSION, read_json_line, write_json_line};
use nix::fcntl::{Flock, FlockArg, OFlag};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

fn endpoint_lock(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

fn acquire(path: &Path) -> Result<Flock<File>, io::Error> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("lock has no parent"))?;
    fs::create_dir_all(parent)?;
    crate::server::validate_socket_parent(parent)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(OFlag::O_NOFOLLOW.bits())
        .open(path)?;
    Flock::lock(file, FlockArg::LockExclusiveNonblock).map_err(|(_, error)| {
        if error == nix::errno::Errno::EWOULDBLOCK {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "another daemon owns {}; use reload/stop for that instance instead of launch",
                    path.display()
                ),
            )
        } else {
            io::Error::from(error)
        }
    })
}

/// Never unlink the lock inode: a waiter may already have it open.
pub fn acquire_instance(state: &Path, sockets: &[&Path]) -> Result<Vec<Flock<File>>, io::Error> {
    let mut paths = vec![state.join("daemon.lock")];
    paths.extend(sockets.iter().map(|path| endpoint_lock(path)));
    paths.sort();
    paths.dedup();
    let mut locks = Vec::new();
    for path in paths {
        let mut lock = acquire(&path)?;
        lock.set_len(0)?;
        writeln!(lock, "{}", std::process::id())?;
        locks.push(lock);
    }
    for path in sockets {
        ensure_socket_unused(path)?;
    }
    Ok(locks)
}

pub fn ensure_socket_unused(path: &Path) -> Result<(), io::Error> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
        Ok(metadata) if !metadata.file_type().is_socket() => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("refusing to replace non-socket path {}", path.display()),
            ));
        }
        Ok(_) => {}
    }
    match UnixStream::connect(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!(
                "an existing service is listening on {}; stop it or use isolated paths",
                path.display()
            ),
        )),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!("cannot inspect {}: {error}", path.display()),
        )),
    }
}

pub fn check_grpc_port(address: Option<&str>) -> Result<(), io::Error> {
    let address = address.unwrap_or("127.0.0.1:50051");
    let parsed: SocketAddr = address
        .parse()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    TcpListener::bind(parsed).map(|_| ()).map_err(|error| io::Error::new(error.kind(), format!(
        "gRPC address {address} is unavailable: {error}; check systemctl status censorguardd.service and ss -ltnp; use one daemon per endpoint")))
}

/// Only remove sockets this launch created, including on a partial startup failure.
#[derive(Default)]
pub struct SocketCleanup(Vec<(PathBuf, u64, u64)>);
impl SocketCleanup {
    pub fn record(&mut self, path: &Path) -> Result<(), io::Error> {
        let metadata = fs::symlink_metadata(path)?;
        self.0
            .push((path.to_owned(), metadata.dev(), metadata.ino()));
        Ok(())
    }
}
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        for (path, device, inode) in &self.0 {
            if fs::symlink_metadata(path).is_ok_and(|metadata| {
                metadata.file_type().is_socket()
                    && metadata.dev() == *device
                    && metadata.ino() == *inode
            }) {
                let _ = fs::remove_file(path);
            }
        }
    }
}

fn call(socket: &Path, operation: Operation, config: Option<&Path>) -> Result<Response, io::Error> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let request = serde_json::json!({"v": VERSION, "op": operation,
        "policy_file": config.map(|path| path.to_string_lossy().into_owned())});
    write_json_line(&mut stream, &request).map_err(io::Error::other)?;
    let response: Response =
        read_json_line(&mut BufReader::new(stream)).map_err(io::Error::other)?;
    if !response.ok {
        return Err(io::Error::other(response.error.unwrap_or_default()));
    }
    Ok(response)
}

pub fn control(socket: &Path, config: &Path, stop: bool) -> Result<(), io::Error> {
    let operation = if stop {
        Operation::DaemonStop
    } else {
        Operation::DaemonReload
    };
    let response = call(socket, operation, (!stop).then_some(config)).map_err(|error| {
        if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) {
            io::Error::new(error.kind(), format!(
                "no running daemon is reachable at {}; reload/stop require a running instance; start it with launch or systemctl start censorguardd.service", socket.display()))
        } else { error }
    })?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let status = call(socket, Operation::Status, None);
        if stop {
            // Socket removal precedes BPF teardown. Wait for process-owned locks too.
            if status.is_err() && acquire(&endpoint_lock(socket)).is_ok() {
                println!("daemon stopped; policy enforcement and managed gRPC have exited");
                return Ok(());
            }
        } else if let Ok(status) = status
            && status.daemon_ready
            && !status.daemon_boot_id.is_empty()
            && status.daemon_boot_id != response.daemon_boot_id
        {
            println!(
                "daemon restarted; boot_id={}; reattach agents",
                status.daemon_boot_id
            );
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "lifecycle request was accepted but completion was not confirmed within 30s; inspect daemon logs",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn shared_state_or_endpoint_rejects_second_instance() -> Result<(), io::Error> {
        let directory = tempfile::tempdir()?;
        let state = directory.path().join("state");
        let socket = directory.path().join("ctl.sock");
        let first = acquire_instance(&state, &[&socket])?;
        assert!(acquire_instance(&state, &[&directory.path().join("other.sock")]).is_err());
        assert!(acquire_instance(&directory.path().join("other-state"), &[&socket]).is_err());
        drop(first);
        assert!(acquire_instance(&state, &[&socket]).is_ok());
        Ok(())
    }

    #[test]
    fn active_socket_is_preserved_and_stale_socket_can_be_replaced() -> Result<(), io::Error> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("ctl.sock");
        let listener = UnixListener::bind(&path)?;
        assert_eq!(
            ensure_socket_unused(&path).err().map(|error| error.kind()),
            Some(io::ErrorKind::AddrInUse)
        );
        assert!(path.exists());
        drop(listener);
        ensure_socket_unused(&path)?;
        Ok(())
    }

    #[test]
    fn occupied_grpc_address_is_rejected() -> Result<(), io::Error> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?.to_string();
        assert_eq!(
            check_grpc_port(Some(&address))
                .err()
                .map(|error| error.kind()),
            Some(io::ErrorKind::AddrInUse)
        );
        drop(listener);
        check_grpc_port(Some(&address))?;
        Ok(())
    }
}
