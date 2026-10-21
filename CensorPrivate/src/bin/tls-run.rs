#[cfg(target_os = "linux")]
mod linux {
    use censorprivacy::{
        control,
        error::{Error, Result},
        json,
    };
    use censorprivacy_runtime_sys as sys;
    use clap::Parser;
    use futures_util::StreamExt;
    use serde_json::{json, Value};
    use std::{
        collections::BTreeMap,
        fs::OpenOptions,
        io::Read,
        net::{Ipv4Addr, SocketAddrV4},
        os::unix::fs::{MetadataExt, OpenOptionsExt},
        path::{Path, PathBuf},
        process::{Child, Command},
        time::{Duration, Instant},
    };
    #[derive(Parser)]
    #[command(
        name = "tls-run",
        about = "Launch a cooperating dynamic OpenSSL HTTP/1.1 JSON client"
    )]
    struct Cli {
        #[arg(long, default_value = "http://127.0.0.1:8765")]
        broker: String,
        #[arg(long)]
        token_file: PathBuf,
        #[arg(long)]
        run_state_file: Option<PathBuf>,
        #[arg(long)]
        upstream_key_file: Option<PathBuf>,
        #[arg(
            long,
            default_value = "/usr/local/lib/censorprivacy/libcp_tls_guard.so"
        )]
        native_library: PathBuf,
        #[arg(long, default_value = ".")]
        workspace: PathBuf,
        #[arg(long, value_name = "NAME=VALUE")]
        env: Vec<String>,
        #[arg(long, default_value_t = 120.0)]
        timeout: f64,
        #[arg(long)]
        dry_run: bool,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    }
    struct Session {
        child: Child,
        reaped: bool,
    }
    impl Drop for Session {
        fn drop(&mut self) {
            if !self.reaped {
                let _ = sys::kill_session(&self.child);
                let _ = self.child.wait();
            }
        }
    }
    fn library(path: &Path) -> Result<PathBuf> {
        let path = path.canonicalize().map_err(|_| Error::TlsBlocked)?;
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .map_err(|_| Error::TlsBlocked)?;
        let metadata = file.metadata().map_err(|_| Error::TlsBlocked)?;
        if !metadata.is_file()
            || metadata.mode() & 0o022 != 0
            || ![0, sys::effective_uid()].contains(&metadata.uid())
        {
            return Err(Error::TlsBlocked);
        }
        let mut header = [0u8; 64];
        file.read_exact(&mut header)
            .map_err(|_| Error::TlsBlocked)?;
        let arch = match std::env::consts::ARCH {
            "x86_64" => 62,
            "aarch64" => 183,
            _ => return Err(Error::TlsBlocked),
        };
        if &header[..6] != b"\x7fELF\x02\x01"
            || u16::from_le_bytes([header[16], header[17]]) != 3
            || u16::from_le_bytes([header[18], header[19]]) != arch
        {
            return Err(Error::TlsBlocked);
        }
        Ok(path)
    }
    fn executable(name: &str) -> Result<PathBuf> {
        let path = Path::new(name);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::var_os("PATH")
                .and_then(|paths| {
                    std::env::split_paths(&paths)
                        .map(|directory| directory.join(name))
                        .find(|candidate| candidate.is_file())
                })
                .ok_or(Error::TlsBlocked)?
        };
        let path = path.canonicalize().map_err(|_| Error::TlsBlocked)?;
        let info = path.metadata().map_err(|_| Error::TlsBlocked)?;
        if !info.is_file() || info.mode() & 0o111 == 0 || info.mode() & 0o6000 != 0 {
            return Err(Error::TlsBlocked);
        }
        Ok(path)
    }
    async fn status(base: &str, token: &str) -> Result<Value> {
        let url = url::Url::parse(base).map_err(|_| Error::TlsBlocked)?;
        let address: Ipv4Addr = url
            .host_str()
            .ok_or(Error::TlsBlocked)?
            .parse()
            .map_err(|_| Error::TlsBlocked)?;
        let pool: ipnet::Ipv4Net = "198.18.0.0/15".parse().map_err(|_| Error::TlsBlocked)?;
        if url.scheme() != "http"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "" | "/")
            || !address.is_loopback() && !pool.contains(&address)
        {
            return Err(Error::TlsBlocked);
        }
        let response = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|_| Error::TlsBlocked)?
            .get(format!("{}/v1/tls/status", base.trim_end_matches('/')))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| Error::TlsBlocked)?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(Error::TlsBlocked);
        }
        let mut source = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = source.next().await {
            let chunk = chunk.map_err(|_| Error::TlsBlocked)?;
            if bytes.len() + chunk.len() > 8192 {
                return Err(Error::ResourceLimit);
            }
            bytes.extend_from_slice(&chunk);
        }
        let value = json::from_slice(&bytes)?;
        if value["status"] != "ready"
            || value["transport"] != "tls-direct-http1-json"
            || value["run_id"]
                .as_str()
                .is_none_or(|id| id.is_empty() || id.len() > 128)
        {
            return Err(Error::TlsBlocked);
        }
        let peer: SocketAddrV4 = value["peer"]
            .as_str()
            .ok_or(Error::TlsBlocked)?
            .parse()
            .map_err(|_| Error::TlsBlocked)?;
        if peer.port() == 0 {
            return Err(Error::TlsBlocked);
        }
        Ok(value)
    }
    async fn run(mut cli: Cli) -> Result<i32> {
        let timeout = Duration::try_from_secs_f64(cli.timeout).map_err(|_| Error::TlsBlocked)?;
        if timeout.is_zero() {
            return Err(Error::TlsBlocked);
        }
        cli.command[0] = executable(&cli.command[0])?
            .to_str()
            .ok_or(Error::TlsBlocked)?
            .to_owned();
        let workspace = cli
            .workspace
            .canonicalize()
            .map_err(|_| Error::TlsBlocked)?;
        if !workspace.is_dir() {
            return Err(Error::TlsBlocked);
        }
        let library = library(&cli.native_library)?;
        let uid = sys::effective_uid();
        let token = control::token(&cli.token_file, uid)?;
        let status = status(&cli.broker, &token).await?;
        if let Some(path) = cli.run_state_file {
            let bytes = control::private_file(&path, uid, 8192)?;
            if json::from_slice(&bytes)? != json!({"run_id":status["run_id"]}) {
                return Err(Error::TlsBlocked);
            }
        }
        let mut environment: BTreeMap<String, String> = BTreeMap::new();
        for key in ["PATH", "LANG", "HOME", "TERM"] {
            if let Ok(value) = std::env::var(key) {
                environment.insert(key.into(), value);
            }
        }
        for setting in cli.env {
            let (name, value) = setting.split_once('=').ok_or(Error::TlsBlocked)?;
            if ![
                "SSL_CERT_FILE",
                "SSL_CERT_DIR",
                "CURL_CA_BUNDLE",
                "REQUESTS_CA_BUNDLE",
                "OPENAI_BASE_URL",
                "ANTHROPIC_BASE_URL",
                "OPENAI_MODEL",
                "ANTHROPIC_MODEL",
                "PYTHONPATH",
                "VIRTUAL_ENV",
            ]
            .contains(&name)
                || value.contains('\0')
                || environment.insert(name.into(), value.into()).is_some()
            {
                return Err(Error::TlsBlocked);
            }
        }
        if let Some(path) = cli.upstream_key_file {
            let raw = control::private_file(&path, uid, 8192)?;
            let key = std::str::from_utf8(&raw)
                .map_err(|_| Error::TlsBlocked)?
                .trim();
            if key.is_empty() || !key.bytes().all(|b| (33..=126).contains(&b)) {
                return Err(Error::TlsBlocked);
            }
            environment.insert("OPENAI_API_KEY".into(), key.into());
            environment.insert("ANTHROPIC_API_KEY".into(), key.into());
        }
        for (key, value) in [
            ("LD_PRELOAD", library.to_string_lossy().into_owned()),
            ("CENSORPRIVACY_URL", cli.broker.trim_end_matches('/').into()),
            ("CENSORPRIVACY_TOKEN", token.to_string()),
            (
                "CENSORPRIVACY_TLS_PEER",
                status["peer"].as_str().unwrap_or("").into(),
            ),
            (
                "CENSORPRIVACY_RUN_ID",
                status["run_id"].as_str().unwrap_or("").into(),
            ),
            ("CENSORPRIVACY_MODE", "tls-direct".into()),
        ] {
            environment.insert(key.into(), value);
        }
        if cli.dry_run {
            println!(
                "{}",
                json!({"mode":"tls-direct","run_id":status["run_id"],"peer":status["peer"],"command":{"executable":cli.command[0],"argument_count":cli.command.len()-1},"library":library,"workspace":workspace,"environment_keys":environment.keys().collect::<Vec<_>>(),"supported":"cooperating dynamic OpenSSL, HTTP/1.1 JSON"})
            );
            return Ok(0);
        }
        let mut command = Command::new(&cli.command[0]);
        command
            .args(&cli.command[1..])
            .current_dir(workspace)
            .env_clear()
            .envs(&environment);
        let mut session = Session {
            child: sys::spawn_session(&mut command).map_err(|_| Error::TlsBlocked)?,
            reaped: false,
        };
        let pidfd = sys::pidfd_open(session.child.id()).map_err(|_| Error::TlsBlocked)?;
        use zeroize::Zeroize;
        for value in environment.values_mut() {
            value.zeroize();
        }
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .map_err(|_| Error::TlsBlocked)?;
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(|_| Error::TlsBlocked)?;
        let started = Instant::now();
        let outcome = loop {
            if sys::exited_unreaped(&pidfd).map_err(|_| Error::TlsBlocked)? {
                break Ok(());
            }
            if started.elapsed() >= timeout {
                break Err(Error::RuntimeTimeout);
            }
            tokio::select! {_=tokio::time::sleep(Duration::from_millis(20))=>(),_=interrupt.recv()=>return Ok(130),_=terminate.recv()=>return Ok(143)}
        };
        sys::kill_session(&session.child).map_err(|_| Error::TlsBlocked)?;
        let exit = session.child.wait().map_err(|_| Error::TlsBlocked)?;
        // Avoid a second group kill after the leader has been reaped.
        session.reaped = true;
        outcome?;
        use std::os::unix::process::ExitStatusExt;
        Ok(exit.code().unwrap_or(128 + exit.signal().unwrap_or(0)))
    }
    pub fn main() -> i32 {
        std::panic::set_hook(Box::new(|_| {
            eprintln!("{{\"error\":{{\"code\":\"processing_failed\"}}}}")
        }));
        let cli = Cli::parse();
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => return 125,
        };
        match runtime.block_on(run(cli)) {
            Ok(code) => code,
            Err(error) => {
                eprintln!("{{\"error\":{{\"code\":\"{}\"}}}}", error.code());
                if error == Error::RuntimeTimeout {
                    124
                } else {
                    125
                }
            }
        }
    }
}
fn main() {
    #[cfg(target_os = "linux")]
    std::process::exit(linux::main());
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("Linux required");
        std::process::exit(125);
    }
}
