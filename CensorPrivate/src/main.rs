use censorprivacy::{
    conversations::ConversationStore,
    detector_web,
    error::{Error, Result},
    gate::{GateOptions, PrivacyGate},
    policy::PrivacyPolicy,
    professional::{
        configured_ner_batch_size, configured_ner_device, configured_ner_threads, NerDevice,
        ProfessionalConfig, ProfessionalDetector,
    },
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

#[derive(Parser)]
#[command(
    name = "censorprivacy",
    version,
    about = "Local fail-closed privacy gateway"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Serve(Box<Serve>),
    /// Open the local privacy detection workspace (no upstream required).
    Web(detector_web::WebArgs),
}
#[derive(clap::Args)]
struct Serve {
    #[arg(long)]
    policy: Option<PathBuf>,
    #[arg(long)]
    upstream: String,
    #[arg(long, default_value = "127.0.0.1")]
    host: IpAddr,
    #[arg(long, default_value_t = 8765)]
    port: u16,
    #[arg(long)]
    token_file: PathBuf,
    #[arg(long)]
    upstream_key_file: Option<PathBuf>,
    #[arg(long)]
    upstream_ca_file: Option<PathBuf>,
    #[arg(long, requires = "tls_key")]
    tls_cert: Option<PathBuf>,
    #[arg(long, requires = "tls_cert")]
    tls_key: Option<PathBuf>,
    #[arg(long)]
    allow_http_test_upstream: bool,
    #[arg(long)]
    tls_direct: bool,
    /// Local FP32 ONNX bert4ner-base-chinese bundle (no downloads).
    #[arg(long)]
    detector_model_dir: Option<PathBuf>,
    /// Local ONNX Runtime shared library, never downloaded at runtime.
    #[arg(long)]
    detector_ort_library: Option<PathBuf>,
    /// ONNX intra-op threads (1–256); defaults to CP_NER_THREADS, then 1.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=256))]
    ner_threads: Option<u16>,
    /// cpu (default), cuda[:ids], or cann[:ids]; unnumbered accelerators use device 0; CP_NER_DEVICE overrides the default.
    #[arg(long)]
    ner_device: Option<NerDevice>,
    /// Maximum windows per device inference (1–256); CP_NER_BATCH_SIZE, then 1.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=256))]
    ner_batch_size: Option<u16>,
    /// Record tagged Agent sessions and serve the local privacy workspace.
    #[arg(long, visible_alias = "xiaoo-records-dir")]
    conversation_records_dir: Option<PathBuf>,
    #[arg(long)]
    sensitive_path: Vec<String>,
    #[arg(long)]
    run_state_file: Option<PathBuf>,
    #[arg(long, required = true)]
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
fn load_policy(path: Option<&Path>) -> Result<PrivacyPolicy> {
    match path {
        None => Ok(PrivacyPolicy::default()),
        Some(path) => PrivacyPolicy::from_yaml(&read_file(path, false, 1_048_576)?),
    }
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
async fn serve(args: Serve) -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let policy = load_policy(args.policy.as_deref())?;
    let options = GateOptions {
        sensitive_paths: args.sensitive_path,
        public_identifiers: args.model,
        ..Default::default()
    };
    let mut detector = ProfessionalConfig {
        ner_threads: configured_ner_threads(args.ner_threads)?,
        ner_device: configured_ner_device(args.ner_device)?,
        ner_batch_size: configured_ner_batch_size(args.ner_batch_size)?,
        ..Default::default()
    };
    let ner_threads = detector.ner_threads;
    let ner_device = detector.ner_device.to_string();
    let ner_batch_size = detector.ner_batch_size;
    if let Some(path) = args.detector_ort_library {
        detector.runtime_library = Some(path);
    }
    if let Some(path) = args.detector_model_dir {
        detector.model_directory = Some(path);
    }
    if args.conversation_records_dir.is_some()
        && (args.host != IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) || args.tls_cert.is_some())
    {
        return Err(Error::ProtocolBlocked);
    }
    let detector = Arc::new(ProfessionalDetector::start(detector)?);
    let mut gate =
        PrivacyGate::with_loaded_professional_detector(policy, options, detector.clone())?;
    let run_id = gate.run_id().to_owned();
    let token = read_file(&args.token_file, true, 8192)?;
    let upstream_key = args
        .upstream_key_file
        .as_deref()
        .map(|path| read_file(path, true, 8192))
        .transpose()?;
    let mut config = GatewayConfig::new(
        &args.upstream,
        token.trim().to_owned(),
        upstream_key.as_ref().map(|key| key.trim().to_owned()),
        args.allow_http_test_upstream,
    )?;
    config.enable_tls_direct = args.tls_direct;
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
    let listener = std::net::TcpListener::bind(SocketAddr::new(args.host, args.port))
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
    let reload = if let Some(path) = args.policy {
        let mut signals = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
            .map_err(|_| Error::ServiceUnavailable)?;
        let service = service.clone();
        Some(tokio::spawn(async move {
            while signals.recv().await.is_some() {
                if load_policy(Some(&path))
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

#[tokio::main]
async fn main() {
    // Third-party panic text may contain input. Do not print payloads/backtraces.
    std::panic::set_hook(Box::new(|_| {
        eprintln!("{{\"error\":{{\"code\":\"processing_failed\"}}}}")
    }));
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Serve(args) => serve(*args).await,
        Command::Web(args) => detector_web::serve(args).await,
    };
    if let Err(error) = result {
        eprintln!("{{\"error\":{{\"code\":\"{}\"}}}}", error.code());
        std::process::exit(2);
    }
}
