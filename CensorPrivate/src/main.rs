use censorprivacy::{
    config::{load_policy, AppConfig, ConfigArgs},
    conversations::ConversationStore,
    detector_web,
    error::{Error, Result},
    gate::{GateOptions, PrivacyGate},
    professional::{NerDevice, ProfessionalDetector},
    server::{GatewayConfig, GatewayService},
};
use clap::{Parser, Subcommand};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{IpAddr, SocketAddr},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};
use zeroize::Zeroizing;

mod cli {
    pub mod compute;
    pub mod mcp;
    pub mod ops;
    pub mod run;
    pub mod tls;
}

#[derive(Parser)]
#[command(
    name = "censorprivacy",
    version,
    about = "CensorPrivacy: local privacy gateway and agent integration",
    after_help = "Managed instance commands:\n  doctor, deps, install, init, start, stop, dashboard, check, systemd\nAgent integration and validation:\n  dsh, xiaoo, demo, verify, self-test\nRun 'censorprivacy help managed' for managed instance options."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Compute locally using private references.
    Compute(cli::compute::Options),
    /// Serve private computation over MCP stdio.
    Mcp,
    /// Launch an agent inside a fail-closed Linux run domain.
    Run(cli::run::Options),
    /// Launch a cooperating dynamic OpenSSL HTTP/1.1 JSON client.
    TlsRun(cli::tls::Options),
    Serve(Box<Serve>),
    /// Open the local privacy detection workspace (no upstream required).
    Web(detector_web::WebArgs),
    /// Validate deployment YAML without starting the model or listener.
    ConfigCheck(ConfigCheck),
    #[command(hide = true)]
    PrivateProgramWorker {
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
}
#[derive(clap::Args)]
struct ConfigCheck {
    #[command(flatten)]
    deployment: ConfigArgs,
    /// Compatibility override; new deployments use custom rules in config.yaml.
    #[arg(long, hide = true)]
    policy: Option<PathBuf>,
    /// Print non-secret configuration with resolved absolute paths.
    #[arg(long)]
    print_resolved: bool,
}
#[derive(clap::Args)]
struct Serve {
    #[command(flatten)]
    deployment: ConfigArgs,
    /// Private JSON manifest of operator-registered local programs.
    #[arg(long)]
    private_tools_file: Option<PathBuf>,
    #[arg(long, hide = true)]
    policy: Option<PathBuf>,
    #[arg(long)]
    upstream: Option<String>,
    #[arg(long)]
    host: Option<IpAddr>,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long)]
    token_file: Option<PathBuf>,
    #[arg(long)]
    upstream_key_file: Option<PathBuf>,
    /// Managed instances may explicitly disable a key inherited from YAML.
    #[arg(long, hide = true, conflicts_with = "upstream_key_file")]
    no_upstream_key: bool,
    #[arg(long)]
    upstream_ca_file: Option<PathBuf>,
    #[arg(long)]
    tls_cert: Option<PathBuf>,
    #[arg(long)]
    tls_key: Option<PathBuf>,
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    allow_http_test_upstream: Option<bool>,
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    tls_direct: Option<bool>,
    /// Local FP32 ONNX bert4ner-base-chinese bundle (no downloads).
    #[arg(long)]
    detector_model_dir: Option<PathBuf>,
    /// Local ONNX Runtime shared library, never downloaded at runtime.
    #[arg(long)]
    detector_ort_library: Option<PathBuf>,
    /// ONNX intra-op threads (1–256); defaults to YAML, CP_NER_THREADS, then 1.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=256))]
    ner_threads: Option<u16>,
    /// cpu (default), cuda[:ids], or cann[:ids]; unnumbered accelerators use device 0; CP_NER_DEVICE overrides the default.
    #[arg(long)]
    ner_device: Option<NerDevice>,
    /// Maximum windows per device inference (1–256); YAML, CP_NER_BATCH_SIZE, then 1.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=256))]
    ner_batch_size: Option<u16>,
    /// Record tagged Agent sessions and serve the local privacy workspace.
    #[arg(long, visible_alias = "xiaoo-records-dir")]
    conversation_records_dir: Option<PathBuf>,
    #[arg(long)]
    sensitive_path: Vec<String>,
    #[arg(long)]
    run_state_file: Option<PathBuf>,
    #[arg(long)]
    model: Vec<String>,
}
fn read_file(path: &Path, private: bool, max: usize) -> Result<Zeroizing<String>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| Error::ServiceUnavailable)?;
    let metadata = file.metadata().map_err(|_| Error::ServiceUnavailable)?;
    if !metadata.is_file()
        || metadata.len() > max as u64
        || (private && metadata.mode() & 0o077 != 0)
    {
        return Err(Error::ServiceUnavailable);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::ServiceUnavailable)?;
    if bytes.len() > max {
        return Err(Error::ResourceLimit);
    }
    Ok(Zeroizing::new(
        std::str::from_utf8(&bytes)
            .map_err(|_| Error::InvalidJson)?
            .to_owned(),
    ))
}
fn write_run_state(path: &Path, run_id: &str) -> Result<()> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let metadata = fs::symlink_metadata(parent).map_err(|_| Error::ServiceUnavailable)?;
    if !metadata.is_dir() || metadata.mode() & 0o077 != 0 {
        return Err(Error::ServiceUnavailable);
    }
    let mut random = [0u8; 16];
    getrandom::getrandom(&mut random).map_err(|_| Error::ServiceUnavailable)?;
    let temporary = parent.join(format!(".cp-run-{:032x}.tmp", u128::from_be_bytes(random)));
    let operation = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)
            .map_err(|_| Error::ServiceUnavailable)?;
        let contents = serde_json::to_vec(&serde_json::json!({"run_id":run_id}))
            .map_err(|_| Error::InvalidJson)?;
        file.write_all(&contents)
            .and_then(|()| file.sync_all())
            .map_err(|_| Error::ServiceUnavailable)?;
        fs::rename(&temporary, path).map_err(|_| Error::ServiceUnavailable)
    })();
    if operation.is_err() {
        let _ = fs::remove_file(temporary);
    }
    operation
}
async fn serve(mut args: Serve) -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let deployment_path = args.deployment.path()?;
    let deployment = deployment_path
        .as_deref()
        .map(AppConfig::load)
        .transpose()?;
    let settings = deployment.clone().unwrap_or_default();
    let host = args
        .host
        .or(settings.server.host)
        .unwrap_or_else(|| "127.0.0.1".parse().unwrap());
    let port = args.port.or(settings.server.port).unwrap_or(8765);
    let upstream = args
        .upstream
        .or(settings.server.upstream.clone())
        .ok_or(Error::ConfigInvalid)?;
    let token_file = args
        .token_file
        .or(settings.server.token_file.clone())
        .ok_or(Error::ConfigInvalid)?;
    if args.model.is_empty() {
        args.model = settings.server.models.clone().unwrap_or_default();
    }
    if args.model.is_empty() || port == 0 {
        return Err(Error::ConfigInvalid);
    }
    args.upstream_key_file = args
        .upstream_key_file
        .or(settings.server.upstream_key_file.clone())
        .filter(|_| !args.no_upstream_key);
    args.upstream_ca_file = args
        .upstream_ca_file
        .or(settings.server.upstream_ca_file.clone());
    args.tls_cert = args.tls_cert.or(settings.server.tls_cert.clone());
    args.tls_key = args.tls_key.or(settings.server.tls_key.clone());
    args.run_state_file = args
        .run_state_file
        .or(settings.server.run_state_file.clone());
    args.private_tools_file = args
        .private_tools_file
        .or(settings.private_tools_file.clone());
    args.conversation_records_dir = args
        .conversation_records_dir
        .or(settings.conversation_records_dir.clone());
    if args.sensitive_path.is_empty() {
        args.sensitive_path = settings
            .detection
            .sensitive_paths
            .clone()
            .unwrap_or_default();
    }
    let policy = match (&args.policy, &deployment) {
        (Some(path), Some(config)) => config.apply_policy(load_policy(Some(path))?)?,
        (Some(path), None) => load_policy(Some(path))?,
        (None, Some(config)) => config.privacy_policy()?,
        (None, None) => load_policy(None)?,
    };
    let options = GateOptions {
        sensitive_paths: args.sensitive_path,
        public_identifiers: args.model,
        max_body_bytes: settings
            .limits
            .max_body_bytes
            .unwrap_or(GateOptions::default().max_body_bytes),
    };
    let mut detector = settings.detector(
        args.ner_threads,
        args.ner_device,
        args.ner_batch_size,
        args.detector_model_dir,
        args.detector_ort_library,
    )?;
    if deployment.is_none() {
        detector.categories = None;
    }
    let ner_threads = detector.ner_threads;
    let ner_device = detector.ner_device.to_string();
    let ner_batch_size = detector.ner_batch_size;
    if args.private_tools_file.is_some() && !host.is_loopback() {
        return Err(Error::ProtocolBlocked);
    }
    if args.conversation_records_dir.is_some()
        && (host != IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) || args.tls_cert.is_some())
    {
        return Err(Error::ProtocolBlocked);
    }
    let detector = Arc::new(ProfessionalDetector::start(detector)?);
    let mut gate =
        PrivacyGate::with_loaded_professional_detector(policy, options, detector.clone())?;
    if let Some(path) = args.private_tools_file {
        gate.configure_private_tools(&read_file(&path, true, 256 * 1024)?)?;
    }
    let run_id = gate.run_id().to_owned();
    let token = read_file(&token_file, true, 8192)?;
    let upstream_key = args
        .upstream_key_file
        .as_deref()
        .map(|path| read_file(path, true, 8192))
        .transpose()?;
    let mut config = GatewayConfig::new(
        &upstream,
        token.trim().to_owned(),
        upstream_key.as_ref().map(|key| key.trim().to_owned()),
        args.allow_http_test_upstream
            .or(settings.server.allow_http_test_upstream)
            .unwrap_or(false),
    )?;
    config.enable_tls_direct = args
        .tls_direct
        .or(settings.server.tls_direct)
        .unwrap_or(false);
    config.upstream_timeout =
        std::time::Duration::from_secs(settings.limits.upstream_timeout_seconds.unwrap_or(60));
    config.max_decoded_bytes = settings
        .limits
        .max_decoded_bytes
        .unwrap_or(config.max_decoded_bytes);
    config.ner_threads = Some(ner_threads);
    config.ner_device = Some(ner_device);
    config.ner_batch_size = Some(ner_batch_size);
    let conversations = args
        .conversation_records_dir
        .map(ConversationStore::open)
        .transpose()?
        .map(Arc::new);
    config.conversations = conversations.clone();
    if let Some(path) = args.upstream_ca_file {
        config.upstream_ca_pem = Some(read_file(&path, false, 1_048_576)?.as_bytes().to_vec());
    }
    let service = Arc::new(GatewayService::new(gate, config)?);
    let listener = std::net::TcpListener::bind(SocketAddr::new(host, port))
        .map_err(|_| Error::ServiceUnavailable)?;
    listener
        .set_nonblocking(true)
        .map_err(|_| Error::ServiceUnavailable)?;
    if let Some(store) = &conversations {
        store.recover_interrupted()?;
    }
    let tls = match (&args.tls_cert, &args.tls_key) {
        (Some(cert), Some(key)) => {
            let certificate = read_file(cert, false, 1_048_576)?;
            let private_key = read_file(key, true, 1_048_576)?;
            Some(
                axum_server::tls_rustls::RustlsConfig::from_pem(
                    certificate.as_bytes().to_vec(),
                    private_key.as_bytes().to_vec(),
                )
                .await
                .map_err(|_| Error::ServiceUnavailable)?,
            )
        }
        (None, None) => None,
        _ => return Err(Error::ProtocolBlocked),
    };
    if let Some(path) = args.run_state_file {
        write_run_state(&path, &run_id)?;
    }
    let reload = if args.policy.is_some() || deployment_path.is_some() {
        let mut signals = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
            .map_err(|_| Error::ServiceUnavailable)?;
        let service = service.clone();
        Some(tokio::spawn(async move {
            while signals.recv().await.is_some() {
                let updated = match (&args.policy, &deployment_path) {
                    (Some(path), _) => load_policy(Some(path)),
                    (None, Some(path)) => {
                        AppConfig::load(path).and_then(|config| config.privacy_policy())
                    }
                    _ => unreachable!(),
                };
                if updated
                    .and_then(|policy| match &deployment {
                        Some(config) if args.policy.is_some() => config.apply_policy(policy),
                        // Keep startup categories, but retain the newly loaded TTL.
                        Some(config) => policy.with_deployment(None, config.categories()?),
                        None => Ok(policy),
                    })
                    .and_then(|policy| service.reload(policy))
                    .is_err()
                {
                    service.disable();
                }
            }
        }))
    } else {
        None
    };
    let handle = axum_server::Handle::new();
    let shutdown_handle = handle.clone();
    let shutdown = tokio::spawn(async move {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => (),
            _ = async { match &mut terminate { Some(signal) => { signal.recv().await; }, None => std::future::pending().await } } => (),
        }
        shutdown_handle.graceful_shutdown(Some(std::time::Duration::from_secs(5)));
    });
    let mut router = service.router();
    if conversations.is_some() {
        let port = listener
            .local_addr()
            .map_err(|_| Error::ServiceUnavailable)?
            .port();
        router = router.merge(detector_web::workspace(detector, conversations, port));
        println!("隐私检测台：http://127.0.0.1:{port}/");
    }
    let result = match tls {
        Some(tls) => {
            axum_server::from_tcp_rustls(listener, tls)
                .handle(handle)
                .serve(router.into_make_service())
                .await
        }
        None => {
            axum_server::from_tcp(listener)
                .handle(handle)
                .serve(router.into_make_service())
                .await
        }
    };
    shutdown.abort();
    if let Some(reload) = reload {
        reload.abort();
    }
    service.close()?;
    result.map_err(|_| Error::ServiceUnavailable)
}

fn main() {
    // Third-party panic text may contain input. Do not print payloads/backtraces.
    std::panic::set_hook(Box::new(|_| {
        eprintln!("{{\"error\":{{\"code\":\"processing_failed\"}}}}")
    }));
    // Namespace bootstrap must run before any async runtime creates threads.
    #[cfg(target_os = "linux")]
    if std::env::args().nth(1).as_deref() == Some("--internal-bootstrap") {
        std::process::exit(cli::run::bootstrap(&std::env::args().collect::<Vec<_>>()));
    }
    cli::ops::dispatch();
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Compute(options) => return cli::compute::run(options),
        Command::Mcp => return cli::mcp::run(),
        Command::Run(options) => std::process::exit(cli::run::run(options)),
        Command::TlsRun(options) => std::process::exit(cli::tls::run(options)),
        Command::PrivateProgramWorker { command } => private_program_worker(command),
        command => run_service(command),
    };
    if let Err(error) = result {
        eprintln!("{{\"error\":{{\"code\":\"{}\"}}}}", error.code());
        std::process::exit(2);
    }
}

#[tokio::main]
async fn run_service(command: Command) -> Result<()> {
    match command {
        Command::Serve(args) => serve(*args).await,
        Command::Web(args) => detector_web::serve(args).await,
        Command::ConfigCheck(args) => {
            let mut config = args.deployment.load()?.ok_or(Error::ConfigInvalid)?;
            if let Some(path) = args.policy {
                let policy = config.apply_policy(load_policy(Some(&path))?)?;
                config.custom = policy.custom_rules();
                config.server.ttl_seconds = Some(policy.ttl().as_secs_f64());
            }
            config.privacy_policy()?;
            if args.print_resolved {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&config).map_err(|_| Error::ConfigInvalid)?
                );
            } else {
                println!("{{\"status\":\"valid\"}}");
            }
            Ok(())
        }
        Command::PrivateProgramWorker { .. }
        | Command::Compute(_)
        | Command::Mcp
        | Command::Run(_)
        | Command::TlsRun(_) => Err(Error::PrivateToolFailed),
    }
}

fn private_program_worker(command: Vec<String>) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        let executable = command.first().ok_or(Error::PrivateToolFailed)?;
        if !Path::new(executable).is_absolute() {
            return Err(Error::PrivateToolFailed);
        }
        let mut program = std::process::Command::new(executable);
        program.args(&command[1..]);
        censorprivacy_runtime_sys::restrict_private_program()
            .map_err(|_| Error::PrivateToolFailed)?;
        let _ = program.exec();
    }
    Err(Error::PrivateToolFailed)
}
