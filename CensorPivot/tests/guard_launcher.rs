//! Real launcher + real Pivot runner, with a protocol fixture in place of the BPF daemon.
//! Run explicitly: CENSORGUARD_EXEC=/path/to/censorguard-exec cargo test --test guard_launcher -- --ignored
use censorpivot::{PivotError, Result, RunnerReply};
use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::time::Duration;

fn exercise(allowed: bool) -> Result<()> {
    let launcher = std::env::var_os("CENSORGUARD_EXEC").ok_or_else(|| {
        PivotError::Invalid("set CENSORGUARD_EXEC to the real launcher binary".into())
    })?;
    let dir = tempfile::tempdir()?;
    let socket = dir.path().join("launch.sock");
    let listener = UnixListener::bind(&socket)?;
    let mut child = Command::new(launcher)
        .arg("--socket")
        .arg(&socket)
        .args([
            "--scope",
            "pivot-integration",
            "--group",
            "censorguard-dsh-default",
            "--failure-policy",
            "deny",
            "--",
        ])
        .arg(env!("CARGO_BIN_EXE_censorpivot"))
        .args(["__batch-runner", "--max-output-bytes", "4096"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let (mut stream, _) = listener.accept()?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let peer = getsockopt(&stream, PeerCredentials).map_err(std::io::Error::from)?;
    let mut frame = String::new();
    BufReader::new(&stream).read_line(&mut frame)?;
    let request: Value = serde_json::from_str(&frame)?;
    assert_eq!(request["method"], "register_self");
    assert_eq!(request["params"]["group"], "censorguard-dsh-default");
    assert_eq!(request["v"], 3);
    // The launcher must still be waiting; the runner cannot exit before registration replies.
    assert!(child.try_wait()?.is_none());
    let reply = if allowed {
        json!({"v":3,"request_id":request["request_id"],"ok":true,"result":{}})
    } else {
        json!({"v":3,"request_id":request["request_id"],"ok":false,
            "error":{"code":"permission_denied","message":"fixture rejected registration"}})
    };
    serde_json::to_writer(&mut stream, &reply)?;
    stream.write_all(b"\n")?;
    let output = child.wait_with_output()?;
    if allowed {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let ready: RunnerReply = serde_json::from_slice(&output.stdout)?;
        assert!(matches!(ready, RunnerReply::Ready { host_pid } if host_pid == peer.pid() as u32));
    } else {
        assert!(!output.status.success());
        assert!(
            output.stdout.is_empty(),
            "runner must not emit Ready on rejected registration"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("fixture rejected registration"));
    }
    Ok(())
}

#[test]
#[ignore = "requires the real CensorGuard launcher binary"]
fn accepted_registration_execs_runner_with_same_pid() -> Result<()> {
    exercise(true)
}

#[test]
#[ignore = "requires the real CensorGuard launcher binary"]
fn denied_registration_never_execs_runner() -> Result<()> {
    exercise(false)
}
