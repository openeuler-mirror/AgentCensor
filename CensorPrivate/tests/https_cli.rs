//! Actual Rust process, HTTPS listener, verified HTTPS upstream, SIGHUP and CLI tools.
use axum::{
    body::{to_bytes, Body},
    extract::Request,
    response::IntoResponse,
    Json, Router,
};
use censorprivacy::{
    gate::{GateOptions, PrivacyGate},
    json,
    policy::PrivacyPolicy,
    server::{GatewayConfig, GatewayService},
};
use serde_json::{json, Value};
use std::{
    fs,
    net::{Ipv4Addr, TcpListener},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tower::ServiceExt;

const TOKEN: &str = "cli-test-token-01234567890123456789";
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let mut random = [0u8; 16];
        getrandom::getrandom(&mut random).unwrap();
        let path = std::env::temp_dir().join(format!(
            "cp-rust-https-{:032x}",
            u128::from_be_bytes(random)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn openssl(directory: &Path, args: &[&str]) {
    let status = Command::new("openssl")
        .args(args)
        .current_dir(directory)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "OpenSSL test certificate generation failed"
    );
}
fn certificates(directory: &Path) {
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
            "ca.private",
            "-out",
            "ca.cert",
            "-subj",
            "/CN=CensorPrivacy Test CA",
            "-days",
            "1",
        ],
    );
    openssl(
        directory,
        &[
            "req",
            "-new",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
            "-keyout",
            "server.private",
            "-out",
            "server.csr",
            "-subj",
            "/CN=localhost",
            "-addext",
            "subjectAltName=DNS:localhost,IP:127.0.0.1",
        ],
    );
    openssl(
        directory,
        &[
            "x509",
            "-req",
            "-in",
            "server.csr",
            "-CA",
            "ca.cert",
            "-CAkey",
            "ca.private",
            "-CAcreateserial",
            "-out",
            "server.cert",
            "-copy_extensions",
            "copyall",
            "-days",
            "1",
        ],
    );
    fs::set_permissions(
        directory.join("server.private"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
}
async fn ready(client: &reqwest::Client, base: &str, process: &mut Process) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(
                process.0.try_wait().unwrap().is_none(),
                "Rust service exited during readiness"
            );
            if let Ok(response) = client
                .get(format!("{base}/ready"))
                .bearer_auth(TOKEN)
                .send()
                .await
            {
                if response.status().is_success() {
                    return response.json::<Value>().await.unwrap();
                }
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("Rust service readiness timed out")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn https_process_reloads_policy_and_runs_private_calculation() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let temporary = Temp::new();
    let directory = &temporary.0;
    certificates(directory);
    let observed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let provider_observed = observed.clone();
    let provider = Router::new().fallback(move |request: Request| {
        let observed = provider_observed.clone();
        async move {
            let raw = to_bytes(request.into_body(), 1_048_576).await.unwrap();
            let value = json::from_slice(&raw).unwrap();
            observed.lock().unwrap().push(value.clone());
            Json(json!({"choices":[{"message":{"content":value["messages"][0]["content"]}}]}))
                .into_response()
        }
    });
    let provider_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    provider_listener.set_nonblocking(true).unwrap();
    let provider_address = provider_listener.local_addr().unwrap();
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
        directory.join("server.cert"),
        directory.join("server.private"),
    )
    .await
    .unwrap();
    let provider_handle = axum_server::Handle::new();
    let running_handle = provider_handle.clone();
    let provider_task = tokio::spawn(async move {
        axum_server::from_tcp_rustls(provider_listener, tls)
            .handle(running_handle)
            .serve(provider.into_make_service())
            .await
            .unwrap();
    });
    let port = {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.local_addr().unwrap().port()
    };
    fs::write(directory.join("token"), TOKEN).unwrap();
    fs::set_permissions(directory.join("token"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(directory.join("policy.yaml"), "epoch: 1\n").unwrap();
    let upstream = format!("https://{provider_address}");
    let mut child = Process(
        Command::new(env!("CARGO_BIN_EXE_censorprivacy"))
            .args([
                "serve",
                "--upstream",
                &upstream,
                "--model",
                "configured-model",
                "--port",
                &port.to_string(),
                "--token-file",
                directory.join("token").to_str().unwrap(),
                "--policy",
                directory.join("policy.yaml").to_str().unwrap(),
                "--run-state-file",
                directory.join("run.json").to_str().unwrap(),
                "--upstream-ca-file",
                directory.join("ca.cert").to_str().unwrap(),
                "--tls-cert",
                directory.join("server.cert").to_str().unwrap(),
                "--tls-key",
                directory.join("server.private").to_str().unwrap(),
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let ca = reqwest::Certificate::from_pem(&fs::read(directory.join("ca.cert")).unwrap()).unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(ca)
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let base = format!("https://127.0.0.1:{port}");
    let document = ready(&client, &base, &mut child).await;
    let state = json::from_slice(&fs::read(directory.join("run.json")).unwrap()).unwrap();
    assert_eq!(document["run_id"], state["run_id"]);
    assert_eq!(
        fs::metadata(directory.join("run.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let untrusted = reqwest::Client::builder().no_proxy().build().unwrap();
    assert!(untrusted
        .get(format!("{base}/ready"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .is_err());
    let response = client.post(format!("{base}/v1/chat/completions")).bearer_auth(TOKEN).json(&json!({"model":"configured-model","messages":[{"role":"user","content":"姓名：张三 奖金100元"}]})).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap()["choices"][0]["message"]["content"],
        "姓名：张三 奖金100元"
    );
    let protected = observed.lock().unwrap()[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!protected.contains("张三"));
    let reference = regex::Regex::new(r"<CP_MONEY_[0-9a-f]{32}>")
        .unwrap()
        .find(&protected)
        .unwrap()
        .as_str()
        .to_owned();
    // The CLI shares the checked HTTP client; its output never includes raw results.
    // Use a local HTTP service for the control-plane tool, as privacy-run does.
    let gate = PrivacyGate::new(PrivacyPolicy::default(), GateOptions::default()).unwrap();
    let money = gate.protect_text("奖金100元", "").unwrap();
    let tool_ref = regex::Regex::new(r"<CP_MONEY_[0-9a-f]{32}>")
        .unwrap()
        .find(&money)
        .unwrap()
        .as_str()
        .to_owned();
    let run = gate.run_id().to_owned();
    let service = GatewayService::new(
        gate,
        GatewayConfig::new(&upstream, TOKEN.into(), None, false).unwrap(),
    )
    .unwrap();
    let tool_listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let tool_base = format!("http://{}", tool_listener.local_addr().unwrap());
    let router = service.router();
    let tool_task = tokio::spawn(async move {
        axum::serve(tool_listener, router).await.unwrap();
    });
    let computed = Command::new(env!("CARGO_BIN_EXE_privcalc"))
        .args(["sum", &tool_ref])
        .env_clear()
        .env("CENSORPRIVACY_URL", &tool_base)
        .env("CENSORPRIVACY_TOKEN", TOKEN)
        .env("CENSORPRIVACY_RUN_ID", &run)
        .output()
        .unwrap();
    assert!(computed.status.success());
    let result = json::from_slice(&computed.stdout).unwrap();
    assert!(result["results"]["sum"]
        .as_str()
        .unwrap()
        .starts_with("<CP_MONEY_"));
    // Provider TLS validation is mandatory even when a valid HTTP response exists.
    let rejected = service
        .router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"messages":[{"role":"user","content":"hello"}]}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), axum::http::StatusCode::BAD_GATEWAY);
    fs::write(directory.join("policy.yaml"), "epoch: 2\n").unwrap();
    let status = Command::new("kill")
        .args(["-HUP", &child.0.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    tokio::time::sleep(Duration::from_millis(100)).await;
    let revoked = client
        .post(format!("{base}/v1/private_compute"))
        .bearer_auth(TOKEN)
        .json(&json!({"operation":"sum","refs":[reference]}))
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), reqwest::StatusCode::BAD_REQUEST);
    fs::write(directory.join("policy.yaml"), "epoch: invalid\n").unwrap();
    let status = Command::new("kill")
        .args(["-HUP", &child.0.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let response = client
                .get(format!("{base}/ready"))
                .bearer_auth(TOKEN)
                .send()
                .await
                .unwrap();
            if !response.status().is_success() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    provider_handle.shutdown();
    provider_task.await.unwrap();
    tool_task.abort();
}
