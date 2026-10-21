#![cfg(target_os = "linux")]
use axum::Router;
use censorprivacy::{
    error::Error,
    gate::{GateOptions, PrivacyGate},
    policy::PrivacyPolicy,
    runtime::{LinuxRuntime, RuntimePlan, RuntimeSpec},
    server::{GatewayConfig, GatewayService},
};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};
const TOKEN: &str = "runtime-test-token-01234567890123456789";
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires root, Linux namespace/mount/nft capabilities and cargo build --example runtime_probe"]
async fn actual_domain_drops_privileges_enforces_mount_and_network_and_cleans_after_timeout() {
    assert_eq!(censorprivacy_runtime_sys::effective_uid(), 0);
    let _ = rustls::crypto::ring::default_provider().install_default();
    let directory = std::env::temp_dir().join(format!("cp-domain-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(Command::new("chown")
        .args(["65534:65534", directory.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let gate = PrivacyGate::new(PrivacyPolicy::default(), GateOptions::default()).unwrap();
    let run = gate.run_id().to_owned();
    let service = GatewayService::new(
        gate,
        GatewayConfig::new("https://127.0.0.1", TOKEN.into(), None, false).unwrap(),
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let router: Router = service.router();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let probe = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/debug/examples/runtime_probe")
        .canonicalize()
        .unwrap();
    let mut spec = RuntimeSpec {
        run_id: run.clone(),
        command: vec![probe.to_str().unwrap().into()],
        workspace: directory.clone(),
        gateway_port: port,
        uid: 65534,
        gid: 65534,
        environment: BTreeMap::from([
            ("CENSORPRIVACY_TOKEN".into(), TOKEN.into()),
            ("PROBE_GATEWAY_PORT".into(), port.to_string()),
        ]),
        ipv4_subnet: None,
        guard_launcher: None,
        guard_group: "censorprivacy".into(),
        read_only_paths: vec![probe],
    };
    let runtime = LinuxRuntime::default();
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_privacy-run"));
    assert_eq!(
        runtime
            .run(
                spec.clone(),
                &executable,
                |_| async { Ok(true) },
                Some(Duration::from_secs(15)),
                Duration::from_secs(15)
            )
            .await,
        Ok(0)
    );
    assert!(directory.join("isolation-ok").is_file());
    spec.command = vec!["/bin/sh".into(), "-c".into(), "sleep 30 & wait".into()];
    for attempt in 0..3 {
        spec.run_id = format!("{run}-timeout-{attempt}");
        assert_eq!(
            runtime
                .run(
                    spec.clone(),
                    &executable,
                    |_| async { Ok(true) },
                    Some(Duration::from_millis(200)),
                    Duration::from_secs(15)
                )
                .await,
            Err(Error::RuntimeTimeout)
        );
        let document = RuntimePlan::new(spec.clone()).unwrap().document();
        let namespace = document["namespace"].as_str().unwrap();
        let output = Command::new("ip")
            .args(["netns", "list"])
            .stderr(Stdio::null())
            .output()
            .unwrap();
        assert!(!String::from_utf8_lossy(&output.stdout).contains(namespace));
        let output = Command::new("nft")
            .args(["list", "tables"])
            .stderr(Stdio::null())
            .output()
            .unwrap();
        assert!(!String::from_utf8_lossy(&output.stdout)
            .contains(document["nft_table"].as_str().unwrap()));
    }
    server.abort();
    service.close().unwrap();
    fs::remove_dir_all(directory).unwrap();
}
