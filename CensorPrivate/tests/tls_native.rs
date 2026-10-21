//! Real cooperating OpenSSL ABI and curl through the authenticated broker.
#![cfg(target_os = "linux")]
use axum::{body::to_bytes, extract::Request, response::IntoResponse, Json, Router};
use censorprivacy::{
    gate::{GateOptions, PrivacyGate},
    policy::PrivacyPolicy,
    server::{GatewayConfig, GatewayService},
};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
const TOKEN: &str = "tls-native-test-token-01234567890123456789";
struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn openssl(directory: &Path, args: &[&str]) {
    assert!(Command::new("openssl")
        .args(args)
        .current_dir(directory)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires make -C native and C/OpenSSL development headers; scripts/test/native.sh"]
async fn real_tls_fragments_retries_curl_and_local_compute_never_send_raw_data() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = std::env::temp_dir().join(format!("cp-native-test-{}", std::process::id()));
    fs::create_dir(&path).unwrap();
    let temporary = Temp(path);
    let directory = &temporary.0;
    openssl(
        directory,
        &[
            "req",
            "-x509",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
            "-keyout",
            "key",
            "-out",
            "cert",
            "-subj",
            "/CN=localhost",
            "-addext",
            "subjectAltName=IP:127.0.0.1",
            "-days",
            "1",
        ],
    );
    let capture: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = capture.clone();
    let provider = Router::new().fallback(move |request: Request| {
        let seen = seen.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            let Ok(raw) = to_bytes(request.into_body(), 300000).await else {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(json!({"error":"incomplete"})),
                )
                    .into_response();
            };
            let body: Value = serde_json::from_slice(&raw).unwrap();
            let text = body["messages"][0]["content"].as_str().unwrap().to_owned();
            seen.lock().unwrap().push(body);
            (
                [("connection", "close")],
                Json(json!({"choices":[{"message":{"role":"assistant","content":text}}]})),
            )
                .into_response()
        }
    });
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
        fs::read(directory.join("cert")).unwrap(),
        fs::read(directory.join("key")).unwrap(),
    )
    .await
    .unwrap();
    let handle = axum_server::Handle::new();
    let shutdown = handle.clone();
    let provider_task = tokio::spawn(async move {
        axum_server::from_tcp_rustls(listener, tls)
            .handle(handle)
            .serve(provider.into_make_service())
            .await
            .unwrap();
    });
    let gate = PrivacyGate::new(
        PrivacyPolicy::from_json(
            r#"{"mask_all_numbers":true,"dictionaries":{"PERSON":["Alice","张三"]}}"#,
        )
        .unwrap(),
        GateOptions::default(),
    )
    .unwrap();
    let run = gate.run_id().to_owned();
    let mut config = GatewayConfig::new(
        &format!("https://127.0.0.1:{port}"),
        TOKEN.into(),
        None,
        false,
    )
    .unwrap();
    config.enable_tls_direct = true;
    let service = GatewayService::new(gate, config).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let broker = format!("http://{}", listener.local_addr().unwrap());
    let router = service.router();
    let broker_task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    fs::write(directory.join("token"), TOKEN).unwrap();
    fs::set_permissions(directory.join("token"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(directory.join("run"), json!({"run_id":run}).to_string()).unwrap();
    fs::set_permissions(directory.join("run"), fs::Permissions::from_mode(0o600)).unwrap();
    let library = root
        .join("native/libcp_tls_guard.so")
        .canonicalize()
        .unwrap();
    let client = std::env::var_os("CP_NATIVE_TEST_CLIENT")
        .map(PathBuf::from)
        .unwrap_or(root.join("target/native-test-client"))
        .canonicalize()
        .unwrap();
    let mut retry_count = 0;
    for (kind, width, size) in [
        (1, 1, 0),
        (2, 7, 0),
        (1, 0, 64000),
        (2, 0, 64000),
        (3, 0, 64000),
    ] {
        let text = format!("Alice 张三 100.00 {}", "x".repeat(size));
        let body = serde_json::to_vec(
            &json!({"model":"configured-model","messages":[{"role":"user","content":text}]}),
        )
        .unwrap();
        let mut request=format!("POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).into_bytes();
        request.extend_from_slice(&body);
        fs::write(directory.join("request"), request).unwrap();
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_tls-run"));
        command.args([
            "--broker",
            &broker,
            "--token-file",
            directory.join("token").to_str().unwrap(),
            "--run-state-file",
            directory.join("run").to_str().unwrap(),
            "--native-library",
            library.to_str().unwrap(),
            "--workspace",
            directory.to_str().unwrap(),
            "--timeout",
            "30",
            "--",
            client.to_str().unwrap(),
            &port.to_string(),
            directory.join("cert").to_str().unwrap(),
            directory.join("request").to_str().unwrap(),
            &kind.to_string(),
            &width.to_string(),
        ]);
        let output = command.output().await.unwrap();
        if !output.status.success() {
            let status = reqwest::Client::new()
                .get(format!("{broker}/v1/tls/status"))
                .bearer_auth(TOKEN)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap();
            eprintln!("broker counts: {}", status["counts"]);
        }
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if kind == 3 {
            assert!(String::from_utf8_lossy(&output.stderr).contains("retry_mutation_blocked"));
            continue;
        }
        let boundary = output
            .stdout
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap()
            + 4;
        let response: Value = serde_json::from_slice(&output.stdout[boundary..]).unwrap();
        assert_eq!(response["choices"][0]["message"]["content"], text);
        for line in String::from_utf8_lossy(&output.stderr).lines() {
            if let Some(n) = line.strip_prefix("retries=") {
                retry_count += n.parse::<usize>().unwrap();
            }
        }
    }
    assert!(
        retry_count > 0,
        "fixture must actually exercise WANT_READ/WANT_WRITE retry paths"
    );
    fs::write(
        directory.join("curl.json"),
        br#"{"model":"configured-model","messages":[{"role":"user","content":"Alice 100.00"}]}"#,
    )
    .unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_tls-run"))
        .args([
            "--broker",
            &broker,
            "--token-file",
            directory.join("token").to_str().unwrap(),
            "--native-library",
            library.to_str().unwrap(),
            "--workspace",
            directory.to_str().unwrap(),
            "--",
            "/usr/bin/curl",
            "--noproxy",
            "*",
            "--silent",
            "--show-error",
            "--http1.1",
            "--cacert",
            directory.join("cert").to_str().unwrap(),
            "-A",
            "tls-fixture",
            "-H",
            "Content-Type: application/json",
            "--data-binary",
            &format!("@{}", directory.join("curl.json").display()),
            &format!("https://127.0.0.1:{port}/v1/chat/completions"),
        ])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["choices"][0]["message"]["content"], "Alice 100.00");
    let seen = capture.lock().unwrap().clone();
    assert_eq!(seen.len(), 5);
    for value in &seen {
        let text = value["messages"][0]["content"].as_str().unwrap();
        assert!(!text.contains("Alice") && !text.contains("张三") && !text.contains("100.00"));
    }
    let alias = seen[4]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .split_whitespace()
        .last()
        .unwrap();
    let response = reqwest::Client::new()
        .post(format!("{broker}/v1/private_compute"))
        .bearer_auth(TOKEN)
        .json(&json!({"operation":"sum","refs":[alias]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.json::<Value>().await.unwrap()["result"]
        .as_str()
        .unwrap()
        .starts_with("<CP_"));
    broker_task.abort();
    service.close().unwrap();
    shutdown.shutdown();
    provider_task.await.unwrap();
}
