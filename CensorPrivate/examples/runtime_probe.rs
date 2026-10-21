//! Acceptance helper executed only inside the privileged runtime integration test.
use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    os::unix::net::UnixStream,
    time::Duration,
};
fn main() {
    assert_eq!(std::process::id(), 1);
    let status = fs::read_to_string("/proc/self/status").unwrap();
    assert!(status
        .lines()
        .any(|line| line.starts_with("Uid:")
            && line.split_whitespace().skip(1).all(|id| id == "65534")));
    assert!(status
        .lines()
        .any(|line| line.starts_with("CapEff:") && line.ends_with("0000000000000000")));
    assert!(status
        .lines()
        .any(|line| line.starts_with("NoNewPrivs:") && line.ends_with('1')));
    assert!(status
        .lines()
        .any(|line| line.starts_with("Seccomp:") && line.ends_with('2')));
    assert!(fs::read("/etc/censorprivacy-host-private-sentinel").is_err());
    assert!(fs::write("/usr/cp-runtime-write-probe", b"forbidden").is_err());
    assert!(fs::write("/etc/cp-runtime-write-probe", b"forbidden").is_err());
    assert!(UnixStream::connect("/tmp/host.sock").is_err());
    let address = std::env::var("CENSORPRIVACY_GATEWAY_IP").unwrap();
    let port = std::env::var("PROBE_GATEWAY_PORT").unwrap();
    let target: SocketAddr = format!("{address}:{port}").parse().unwrap();
    let mut connection = TcpStream::connect_timeout(&target, Duration::from_secs(2)).unwrap();
    connection
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let token = std::env::var("CENSORPRIVACY_TOKEN").unwrap();
    write!(connection,"GET /ready HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = String::new();
    connection.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
    let forbidden: SocketAddr = format!("{address}:{}", target.port().saturating_add(1))
        .parse()
        .unwrap();
    assert!(TcpStream::connect_timeout(&forbidden, Duration::from_millis(200)).is_err());
    assert!(TcpStream::connect_timeout(
        &"1.1.1.1:443".parse().unwrap(),
        Duration::from_millis(200)
    )
    .is_err());
    fs::write("isolation-ok", b"uid,capabilities,seccomp,mount,network").unwrap();
}
