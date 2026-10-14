//! Real Pivot runner, real peer credentials, and a bounded Guard protocol fixture.
use censorguard_common::protocol::{MAX_LINE_BYTES, RpcRequest, read_json_line};
use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::time::Duration;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn exercise(mode: &str) -> TestResult {
    let dir = tempfile::tempdir()?;
    let socket = dir.path().join("launch.sock");
    let marker = dir.path().join("tool-executed");
    let listener = UnixListener::bind(&socket)?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_censorpivot"))
        .args([
            "__batch-runner",
            "--max-output-bytes",
            "4096",
            "--guard-socket",
        ])
        .arg(&socket)
        .args([
            "--guard-domain",
            "pivot-integration",
            "--guard-group",
            "web",
            "--guard-timeout-ms",
            "150",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Queue a tool before registration finishes: it must stay inert until accepted.
    let frame = json!({"session_id":"fixture", "environment":{}, "call":{
        "id":"tool", "program":"/usr/bin/touch", "args":[marker], "cwd":dir.path(), "timeout_ms":1000
    }});
    let mut stdin = child.stdin.take().ok_or("missing runner stdin")?;
    if mode != "accept" {
        writeln!(stdin, "{frame}")?;
    }
    drop(stdin);
    let (mut stream, _) = listener.accept()?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let peer = getsockopt(&stream, PeerCredentials)?;
    assert_eq!(peer.pid() as u32, child.id());
    let request: RpcRequest = read_json_line(&mut BufReader::new(&stream))?;
    assert_eq!(
        request.method,
        censorguard_common::protocol::RpcMethod::RegisterSelf
    );
    assert_eq!(request.params.group.as_deref(), Some("web"));
    assert_eq!(request.params.scope.as_deref(), Some("pivot-integration"));
    assert_eq!(request.version, 3);
    // The runner has already received a tool request, but registration is pending.
    assert!(child.try_wait()?.is_none());
    assert!(!marker.exists());
    let mut reply = json!({"v":3,"request_id":request.request_id,"ok":true,"result":{
        "response":{"ok":true,"domain":{"name":"pivot-integration","id":42,"slot":1,
        "group":"web","policies":["web"],"roots":1,"version":7}}
    }});
    match mode {
        "accept" => {}
        "deny" => {
            reply = json!({"v":3,"request_id":request.request_id,"ok":false,
            "error":{"code":"permission_denied","message":"fixture rejected registration"}})
        }
        "request_id" => reply["request_id"] = json!("wrong"),
        "version" => reply["v"] = json!(2),
        "domain" => reply["result"]["response"]["domain"]["name"] = json!("wrong"),
        "group" => reply["result"]["response"]["domain"]["policies"] = json!(["other"]),
        "receipt" => reply["result"] = json!({}),
        "inner_denied" => reply["result"]["response"]["ok"] = json!(false),
        "oversize" => {
            stream.write_all(&vec![b' '; MAX_LINE_BYTES + 1])?;
        }
        "timeout" => {}
        _ => return Err("unknown fixture mode".into()),
    }
    if !matches!(mode, "timeout" | "oversize") {
        writeln!(stream, "{reply}")?;
    }
    let output = child.wait_with_output()?;
    if mode == "accept" {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let lines: Vec<Value> = BufReader::new(output.stdout.as_slice())
            .lines()
            .map(|line| Ok(serde_json::from_str(&line?)?))
            .collect::<Result<_, Box<dyn std::error::Error>>>()?;
        assert_eq!(lines[0]["kind"], "ready");
        assert_eq!(lines[0]["data"]["host_pid"], peer.pid());
        assert_eq!(lines.len(), 1);
    } else {
        assert!(!output.status.success(), "{mode} unexpectedly accepted");
        assert!(
            output.stdout.is_empty(),
            "{mode}: runner emitted Ready before accepted registration"
        );
        assert!(
            !marker.exists(),
            "{mode}: queued tool executed without protection"
        );
    }
    Ok(())
}

#[test]
fn accepted_registration_protects_the_real_runner_pid_before_tools() -> TestResult {
    exercise("accept")
}
#[test]
fn denied_registration_never_executes_queued_tools() -> TestResult {
    exercise("deny")
}
#[test]
fn invalid_receipts_never_execute_queued_tools() -> TestResult {
    for mode in [
        "request_id",
        "version",
        "domain",
        "group",
        "receipt",
        "inner_denied",
        "oversize",
        "timeout",
    ] {
        exercise(mode)?;
    }
    Ok(())
}
#[test]
fn missing_daemon_and_missing_guard_options_fail_closed() -> TestResult {
    let dir = tempfile::tempdir()?;
    let output = Command::new(env!("CARGO_BIN_EXE_censorpivot"))
        .args([
            "__batch-runner",
            "--max-output-bytes",
            "4096",
            "--guard-socket",
        ])
        .arg(dir.path().join("absent.sock"))
        .args(["--guard-domain", "fixture", "--guard-group", "web"])
        .output()?;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let output = Command::new(env!("CARGO_BIN_EXE_censorpivot"))
        .args(["__batch-runner", "--max-output-bytes", "4096"])
        .output()?;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    Ok(())
}
