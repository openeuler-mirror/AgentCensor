use std::{
    io::Write,
    process::{Command, Stdio},
};

#[test]
fn unified_command_exposes_service_tools_and_management() {
    for subcommand in ["", "serve", "web", "compute", "mcp", "run", "tls-run"] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_censorprivacy"));
        if !subcommand.is_empty() {
            command.arg(subcommand);
        }
        let output = command.arg("--help").output().unwrap();
        assert!(output.status.success(), "{subcommand}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("censorprivacy"));
    }
    let output = Command::new(env!("CARGO_BIN_EXE_censorprivacy"))
        .args(["help", "managed"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("dashboard"));
}

#[test]
fn mcp_subcommand_initializes_over_stdio_without_a_separate_binary() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_censorprivacy"))
        .arg("mcp")
        .env("CENSORPRIVACY_URL", "http://127.0.0.1:18765")
        .env("CENSORPRIVACY_TOKEN", "fictional-unified-cli-test-token")
        .env("CENSORPRIVACY_RUN_ID", "unified-cli-test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n"
    ).as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let messages: Vec<serde_json::Value> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["result"]["serverInfo"]["name"], "censorprivacy");
    assert_eq!(messages[1]["result"]["tools"][0]["name"], "private_compute");
}
