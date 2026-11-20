//! Configuration-only startup, real HTTP forwarding, CLI precedence and TTL.
use axum::{body::to_bytes, extract::Request, Json, Router};
use serde_json::{json, Value};
use std::{
    fs,
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

const TOKEN: &str = "config-test-token-01234567890123456789";
struct Fixture {
    directory: PathBuf,
    child: Option<Child>,
}
impl Fixture {
    fn new() -> Self {
        let mut random = [0u8; 16];
        getrandom::getrandom(&mut random).unwrap();
        let directory = std::env::temp_dir().join(format!(
            "cp-config-cli-{:032x}",
            u128::from_be_bytes(random)
        ));
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(directory.join("token"), TOKEN).unwrap();
        fs::set_permissions(directory.join("token"), fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            directory,
            child: None,
        }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_censorprivacy"));
        command
            .current_dir(&self.directory)
            .env_remove("CP_CONFIG_FILE");
        command
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[tokio::test]
async fn yaml_only_startup_masks_selected_categories_and_cli_overrides_connection_and_ner() {
    let mut fixture = Fixture::new();
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let observed = captured.clone();
    let provider = Router::new().fallback(move |request: Request| {
        let observed = observed.clone();
        async move {
            let body: Value = serde_json::from_slice(&to_bytes(request.into_body(), 1_048_576).await.unwrap()).unwrap();
            observed.lock().unwrap().push(body.clone());
            Json(json!({"choices":[{"message":{"role":"assistant","content":body["messages"][0]["content"]}}]}))
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let provider_task = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    // Abort even if a subsequent assertion panics.
    struct Provider(tokio::task::JoinHandle<()>);
    impl Drop for Provider {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _provider = Provider(provider_task);
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    let yaml = format!("server:\n  host: 127.0.0.1\n  port: {port}\n  upstream: http://{address}\n  models: [yaml-model]\n  token_file: token\n  allow_http_test_upstream: true\n  ttl_seconds: 30\ncustom: {{epoch: 1, dictionaries: {{CUSTOMER: [ExampleCustomer]}}, patterns: {{CUSTOMER: ['ticket-[A-Z]+']}}, default_labels: [private]}}\ndetection: {{categories: [EMAIL, CUSTOMER]}}\nner: {{threads: 4, device: cpu, batch_size: 8, model_dir: /nonexistent}}\n");
    fs::write(fixture.directory.join("config.yaml"), &yaml).unwrap();
    let valid = fixture.command().arg("config-check").output().unwrap();
    assert!(
        valid.status.success(),
        "{}",
        String::from_utf8_lossy(&valid.stderr)
    );
    let resolved = fixture
        .command()
        .args(["config-check", "--print-resolved"])
        .output()
        .unwrap();
    assert!(resolved.status.success());
    let resolved: Value = serde_json::from_slice(&resolved.stdout).unwrap();
    assert_eq!(
        resolved["custom"]["dictionaries"]["CUSTOMER"][0],
        "ExampleCustomer"
    );
    assert_eq!(resolved["custom"]["default_labels"][0], "private");
    assert_eq!(resolved["server"]["ttl_seconds"], 30.0);
    assert!(resolved.get("policy").is_none());
    for removed in [
        "mask_money",
        "mask_dates",
        "mask_all_numbers",
        "ttl_seconds",
    ] {
        assert!(resolved["custom"].get(removed).is_none());
    }
    assert!(resolved["custom"].get("file").is_none());
    censorprivacy::config::AppConfig::from_yaml(&resolved.to_string()).unwrap();
    drop(reservation);
    fixture.child = Some(
        fixture
            .command()
            .args(["serve"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    for (threads, batch, model) in [(4, 8, "yaml-model"), (2, 3, "cli-model")] {
        let ready = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                assert!(fixture
                    .child
                    .as_mut()
                    .unwrap()
                    .try_wait()
                    .unwrap()
                    .is_none());
                if let Ok(response) = client
                    .get(format!("{base}/ready"))
                    .bearer_auth(TOKEN)
                    .send()
                    .await
                {
                    if response.status().is_success() {
                        break response.json::<Value>().await.unwrap();
                    }
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
        let first_run = ready["run_id"].clone();
        assert_eq!(ready["ner_threads"], threads);
        assert_eq!(ready["ner_batch_size"], batch);
        let response = client.post(format!("{base}/v1/chat/completions")).bearer_auth(TOKEN)
            .json(&json!({"model":model,"messages":[{"role":"user","content":"a@example.com phone=13800138000 ExampleCustomer ticket-ALPHA"}], "max_tokens":32}))
            .send().await.unwrap();
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
        let wire = captured.lock().unwrap().last().unwrap().clone();
        let content = wire["messages"][0]["content"].as_str().unwrap();
        assert!(!content.contains("a@example.com"));
        assert!(content.contains("13800138000"));
        assert!(!content.contains("ExampleCustomer") && !content.contains("ticket-ALPHA"));
        if model == "yaml-model" {
            // Reload custom rules while keeping deployment settings fixed.
            fs::write(
                fixture.directory.join("config.yaml"),
                yaml.replace("epoch: 1", "epoch: 2")
                    .replace("ExampleCustomer", "NextCustomer")
                    .replace("categories: [EMAIL, CUSTOMER]", "categories: []")
                    .replace("threads: 4", "threads: 7"),
            )
            .unwrap();
            assert!(Command::new("kill")
                .args(["-HUP", &fixture.child.as_ref().unwrap().id().to_string()])
                .status()
                .unwrap()
                .success());
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let response = client.post(format!("{base}/v1/chat/completions")).bearer_auth(TOKEN)
                        .json(&json!({"model":model,"messages":[{"role":"user","content":"NextCustomer ticket-BETA"}]}))
                        .send().await.unwrap();
                    assert!(response.status().is_success(), "{}", response.text().await.unwrap());
                    let protected = captured.lock().unwrap().last().unwrap()["messages"][0]["content"].as_str().unwrap().to_owned();
                    if !protected.contains("NextCustomer") {
                        assert!(!protected.contains("ticket-BETA"));
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            }).await.unwrap();
            let ready: Value = client
                .get(format!("{base}/ready"))
                .bearer_auth(TOKEN)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(ready["ner_threads"], 4);
            assert_eq!(ready["run_id"], first_run);
            fs::write(
                fixture.directory.join("config.yaml"),
                "custom: {epoch: invalid}\n",
            )
            .unwrap();
            assert!(Command::new("kill")
                .args(["-HUP", &fixture.child.as_ref().unwrap().id().to_string()])
                .status()
                .unwrap()
                .success());
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let ready = client
                        .get(format!("{base}/ready"))
                        .bearer_auth(TOKEN)
                        .send()
                        .await
                        .unwrap();
                    if !ready.status().is_success() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            })
            .await
            .unwrap();
            let before = captured.lock().unwrap().len();
            let rejected = client
                .post(format!("{base}/v1/chat/completions"))
                .bearer_auth(TOKEN)
                .json(&json!({"model":model,"messages":[{"role":"user","content":"NextCustomer"}]}))
                .send()
                .await
                .unwrap();
            assert!(!rejected.status().is_success());
            assert_eq!(captured.lock().unwrap().len(), before);
        }
        if model == "cli-model" {
            tokio::time::timeout(Duration::from_secs(4), async {
                loop {
                    let response = client
                        .get(format!("{base}/ready"))
                        .bearer_auth(TOKEN)
                        .send()
                        .await
                        .unwrap();
                    if !response.status().is_success() {
                        assert_eq!(
                            response.json::<Value>().await.unwrap()["error"]["code"],
                            "vault_denied"
                        );
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .unwrap();
        }
        let child = fixture.child.as_mut().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        fixture.child = None;
        if model == "yaml-model" {
            // Deliberately broken YAML connection is replaced by explicit CLI values.
            fs::write(
                fixture.directory.join("config.yaml"),
                yaml.replace(&format!("http://{address}"), "http://127.0.0.1:1")
                    .replace(&format!("port: {port}"), "port: 1")
                    .replace("ttl_seconds: 30", "ttl_seconds: 1")
                    .replace("allow_http_test_upstream: true", "allow_http_test_upstream: true\n  upstream_key_file: /missing/provider.key"),
            )
            .unwrap();
            fixture.child = Some(
                fixture
                    .command()
                    .args([
                        "serve",
                        "--upstream",
                        &format!("http://{address}"),
                        "--port",
                        &port.to_string(),
                        "--model",
                        "cli-model",
                        "--no-upstream-key",
                        "--ner-threads",
                        "2",
                        "--ner-batch-size",
                        "3",
                    ])
                    .stdout(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
        }
    }
}

#[test]
fn config_discovery_env_explicit_and_no_config_are_distinct() {
    let fixture = Fixture::new();
    fs::write(fixture.directory.join("config.yaml"), "typo: true\n").unwrap();
    fs::write(
        fixture.directory.join("valid.yaml"),
        "detection: {categories: []}\n",
    )
    .unwrap();
    assert!(!fixture
        .command()
        .arg("config-check")
        .output()
        .unwrap()
        .status
        .success());
    assert!(fixture
        .command()
        .arg("config-check")
        .env("CP_CONFIG_FILE", fixture.directory.join("valid.yaml"))
        .output()
        .unwrap()
        .status
        .success());
    assert!(fixture
        .command()
        .args(["config-check", "--config", "valid.yaml"])
        .env("CP_CONFIG_FILE", "/missing/config.yaml")
        .output()
        .unwrap()
        .status
        .success());
    assert!(!fixture
        .command()
        .args(["config-check", "--config", "valid.yaml", "--no-config"])
        .output()
        .unwrap()
        .status
        .success());

    // Legacy input is exported into the new schema for systemd snapshots.
    fs::write(
        fixture.directory.join("legacy.yaml"),
        "epoch: 7\nttl_seconds: 123\ndictionaries: {CUSTOMER: [LegacyCustomer]}\nmask_money: false\n",
    )
    .unwrap();
    let snapshot = fixture
        .command()
        .args([
            "config-check",
            "--config",
            "valid.yaml",
            "--policy",
            "legacy.yaml",
            "--print-resolved",
        ])
        .output()
        .unwrap();
    assert!(snapshot.status.success());
    let snapshot: Value = serde_json::from_slice(&snapshot.stdout).unwrap();
    assert_eq!(snapshot["custom"]["epoch"], 7);
    assert_eq!(snapshot["server"]["ttl_seconds"], 123.0);
    assert_eq!(
        snapshot["custom"]["dictionaries"]["CUSTOMER"][0],
        "LegacyCustomer"
    );
    assert!(snapshot.get("policy").is_none());
    censorprivacy::config::AppConfig::from_yaml(&snapshot.to_string()).unwrap();
}

#[tokio::test]
async fn reload_uses_updated_server_ttl_instead_of_startup_ttl() {
    let mut fixture = Fixture::new();
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    let yaml = format!("server: {{port: {port}, upstream: 'http://127.0.0.1:1', models: [test-model], token_file: token, allow_http_test_upstream: true, ttl_seconds: 30}}\ncustom: {{epoch: 1}}\ndetection: {{categories: []}}\n");
    fs::write(fixture.directory.join("config.yaml"), &yaml).unwrap();
    drop(reservation);
    fixture.child = Some(
        fixture
            .command()
            .arg("serve")
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/ready");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(fixture
                .child
                .as_mut()
                .unwrap()
                .try_wait()
                .unwrap()
                .is_none());
            if client
                .get(&url)
                .bearer_auth(TOKEN)
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    fs::write(
        fixture.directory.join("config.yaml"),
        yaml.replace("epoch: 1", "epoch: 2")
            .replace("ttl_seconds: 30", "ttl_seconds: 1"),
    )
    .unwrap();
    assert!(Command::new("kill")
        .args(["-HUP", &fixture.child.as_ref().unwrap().id().to_string()])
        .status()
        .unwrap()
        .success());
    // A failed reload would deny requests immediately; a valid reload remains ready.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(client
        .get(&url)
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let response = client.get(&url).bearer_auth(TOKEN).send().await.unwrap();
            if !response.status().is_success() {
                assert_eq!(
                    response.json::<Value>().await.unwrap()["error"]["code"],
                    "vault_denied"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
}
