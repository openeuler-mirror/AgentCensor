//! Linux run domain: a dedicated network, mount, PID and IPC boundary.
pub mod bootstrap;
use crate::{
    error::{Error, Result},
    json,
};
use censorprivacy_runtime_sys as sys;
use ipnet::Ipv4Net;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    future::Future,
    net::Ipv4Addr,
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::fs::MetadataExt,
    },
    path::{Path, PathBuf},
    process::{Child, Stdio},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use zeroize::{Zeroize, Zeroizing};

pub const TOOL_PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";
pub const ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "LANG",
    "LC_ALL",
    "TERM",
    "PYTHONPATH",
    "VIRTUAL_ENV",
    "PYTHONDONTWRITEBYTECODE",
    "NODE_EXTRA_CA_CERTS",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "JAVA_HOME",
    "JAVA_TOOL_OPTIONS",
    "NODE_OPTIONS",
];
#[derive(Clone)]
pub struct RuntimeSpec {
    pub run_id: String,
    pub command: Vec<String>,
    pub workspace: PathBuf,
    pub gateway_port: u16,
    pub uid: u32,
    pub gid: u32,
    pub environment: BTreeMap<String, String>,
    pub ipv4_subnet: Option<Ipv4Net>,
    pub guard_launcher: Option<PathBuf>,
    pub guard_group: String,
    pub read_only_paths: Vec<PathBuf>,
}
impl Drop for RuntimeSpec {
    fn drop(&mut self) {
        for value in self.environment.values_mut() {
            value.zeroize();
        }
        for argument in &mut self.command {
            argument.zeroize();
        }
    }
}
impl RuntimeSpec {
    fn validate(&self) -> Result<()> {
        let path_valid = |path: &Path| {
            path.is_absolute()
                && !path.as_os_str().as_encoded_bytes().contains(&0)
                && !path
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
        };
        if self.run_id.is_empty()
            || self.run_id.len() > 128
            || self.run_id.contains('\0')
            || self.command.is_empty()
            || self.command[0].is_empty()
            || self.command.iter().any(|value| value.contains('\0'))
            || self.gateway_port == 0
            || self.uid == 0
            || self.gid == 0
            || self.uid >= (1 << 31)
            || self.gid >= (1 << 31)
            || !path_valid(&self.workspace)
        {
            return Err(Error::RuntimeBlocked);
        }
        if ["/", "/run", "/tmp", "/var/tmp", "/dev/shm"]
            .iter()
            .any(|path| self.workspace == Path::new(path))
            || ["/proc", "/sys", "/dev"]
                .iter()
                .any(|path| self.workspace.starts_with(path))
        {
            return Err(Error::RuntimeBlocked);
        }
        if self
            .environment
            .iter()
            .any(|(key, value)| key.is_empty() || key.contains(['=', '\0']) || value.contains('\0'))
        {
            return Err(Error::RuntimeBlocked);
        }
        let forbidden = [
            "/", "/home", "/root", "/run", "/var/run", "/tmp", "/var/tmp", "/dev", "/proc", "/sys",
            "/var", "/etc",
        ];
        if self.read_only_paths.iter().any(|path| {
            !path_valid(path)
                || forbidden.iter().any(|root| path == Path::new(root))
                || ["/dev", "/proc", "/sys"]
                    .iter()
                    .any(|root| path.starts_with(root))
        }) {
            return Err(Error::RuntimeBlocked);
        }
        if self
            .guard_launcher
            .as_ref()
            .is_some_and(|path| !path_valid(path))
            || self.guard_group.is_empty()
            || self.guard_group.len() > 64
            || !self
                .guard_group
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(Error::RuntimeBlocked);
        }
        Ok(())
    }
}
#[derive(Clone, Serialize)]
pub struct RuntimeCommand {
    pub argv: Vec<String>,
    pub stdin: Option<String>,
}
impl RuntimeCommand {
    fn new(argv: &[&str]) -> Self {
        Self {
            argv: argv.iter().map(|value| (*value).into()).collect(),
            stdin: None,
        }
    }
}
pub struct RuntimePlan {
    spec: RuntimeSpec,
    namespace: String,
    host_interface: String,
    child_interface: String,
    nft_table: String,
    subnet: Ipv4Net,
    gateway_address: Ipv4Addr,
    child_address: Ipv4Addr,
    setup: Vec<RuntimeCommand>,
}
impl RuntimePlan {
    pub fn new(spec: RuntimeSpec) -> Result<Self> {
        spec.validate()?;
        let digest = Sha256::digest(spec.run_id.as_bytes());
        let key = format!("{digest:x}");
        let namespace = format!("cp-{}", &key[..12]);
        let host_interface = format!("cph{}", &key[..10]);
        let child_interface = format!("cpa{}", &key[..10]);
        let nft_table = format!("censorprivacy_{}", &key[..16]);
        let pool =
            Ipv4Net::new(Ipv4Addr::new(198, 18, 0, 0), 15).map_err(|_| Error::RuntimeBlocked)?;
        let subnet = match spec.ipv4_subnet {
            Some(network) => network,
            None => {
                let offset =
                    u32::from_be_bytes(digest[..4].try_into().map_err(|_| Error::RuntimeBlocked)?)
                        % 32768;
                Ipv4Net::new(Ipv4Addr::from(u32::from(pool.network()) + offset * 4), 30)
                    .map_err(|_| Error::RuntimeBlocked)?
            }
        };
        if subnet.prefix_len() != 30 || subnet.addr() != subnet.network() || !pool.contains(&subnet)
        {
            return Err(Error::RuntimeBlocked);
        }
        let gateway_address = Ipv4Addr::from(u32::from(subnet.network()) + 1);
        let child_address = Ipv4Addr::from(u32::from(subnet.network()) + 2);
        let gateway = gateway_address.to_string();
        let child = child_address.to_string();
        let port = spec.gateway_port.to_string();
        let host_rules = format!("table inet {nft_table} {{\n chain input_guard {{\n  type filter hook input priority -10; policy accept;\n  ip daddr {gateway} tcp dport {port} iifname != \"{host_interface}\" drop\n  iifname \"{host_interface}\" ip saddr {child} ip daddr {gateway} tcp dport {port} ct state {{ new, established }} accept\n  iifname \"{host_interface}\" drop\n }}\n chain forward_guard {{\n  type filter hook forward priority -10; policy accept;\n  iifname \"{host_interface}\" drop\n  oifname \"{host_interface}\" drop\n }}\n}}\n");
        let child_rules = format!("table inet {nft_table} {{\n chain output_guard {{\n  type filter hook output priority 0; policy drop;\n  oifname \"lo\" accept\n  ip saddr {child} ip daddr {gateway} tcp dport {port} ct state {{ new, established }} accept\n }}\n chain input_guard {{\n  type filter hook input priority 0; policy drop;\n  iifname \"lo\" accept\n  ip saddr {gateway} ip daddr {child} tcp sport {port} ct state established accept\n }}\n}}\n");
        let setup = vec![
            RuntimeCommand::new(&["ip", "netns", "add", &namespace]),
            RuntimeCommand::new(&[
                "ip",
                "link",
                "add",
                &host_interface,
                "type",
                "veth",
                "peer",
                "name",
                &child_interface,
            ]),
            RuntimeCommand::new(&["ip", "link", "set", &child_interface, "netns", &namespace]),
            RuntimeCommand::new(&[
                "ip",
                "addr",
                "add",
                &format!("{gateway}/30"),
                "dev",
                &host_interface,
            ]),
            RuntimeCommand::new(&["ip", "link", "set", &host_interface, "up"]),
            RuntimeCommand::new(&["ip", "-n", &namespace, "link", "set", "lo", "up"]),
            RuntimeCommand::new(&[
                "ip",
                "-n",
                &namespace,
                "addr",
                "add",
                &format!("{child}/30"),
                "dev",
                &child_interface,
            ]),
            RuntimeCommand::new(&[
                "ip",
                "-n",
                &namespace,
                "link",
                "set",
                &child_interface,
                "up",
            ]),
            RuntimeCommand {
                argv: vec!["nft".into(), "-f".into(), "-".into()],
                stdin: Some(host_rules),
            },
            RuntimeCommand {
                argv: vec![
                    "ip".into(),
                    "netns".into(),
                    "exec".into(),
                    namespace.clone(),
                    "nft".into(),
                    "-f".into(),
                    "-".into(),
                ],
                stdin: Some(child_rules),
            },
        ];
        Ok(Self {
            spec,
            namespace,
            host_interface,
            child_interface,
            nft_table,
            subnet,
            gateway_address,
            child_address,
            setup,
        })
    }
    pub fn gateway_url(&self) -> String {
        format!("http://{}:{}", self.gateway_address, self.spec.gateway_port)
    }
    pub fn gateway_address(&self) -> Ipv4Addr {
        self.gateway_address
    }
    pub fn document(&self) -> Value {
        json!({"run_id":self.spec.run_id,"namespace":self.namespace,"host_interface":self.host_interface,"child_interface":self.child_interface,
            "nft_table":self.nft_table,"subnet":self.subnet.to_string(),"gateway_address":self.gateway_address.to_string(),"child_address":self.child_address.to_string(),
            "gateway_url":self.gateway_url(),"uid":self.spec.uid,"gid":self.spec.gid,"workspace":self.spec.workspace,"command":self.spec.command,
            "environment_keys":self.spec.environment.keys().collect::<Vec<_>>(),"strict_mount_view":true,"strict_backend":"private-chroot",
            "read_only_paths":self.spec.read_only_paths,"seccomp_required":true,"setup":self.setup})
    }
}

pub struct CommandResult {
    pub success: bool,
    pub stdout: Vec<u8>,
}
pub trait RuntimeRunner: Send + Sync {
    fn namespace_identity(&self, name: &str) -> Result<(u64, u64)> {
        identity(Path::new("/run/netns").join(name))
    }
    fn execute(
        &self,
        command: &RuntimeCommand,
    ) -> impl Future<Output = Result<CommandResult>> + Send;
}
pub struct SystemRunner;
impl RuntimeRunner for SystemRunner {
    async fn execute(&self, command: &RuntimeCommand) -> Result<CommandResult> {
        let program = trusted_tool(command.argv.first().ok_or(Error::RuntimeBlocked)?)?;
        let mut process = Command::new(program);
        process
            .args(&command.argv[1..])
            .env_clear()
            .env("PATH", TOOL_PATH)
            .env("LANG", "C.UTF-8")
            .kill_on_drop(true)
            .stdin(if command.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = process.spawn().map_err(|_| Error::RuntimeBlocked)?;
        if let Some(input) = &command.stdin {
            child
                .stdin
                .take()
                .ok_or(Error::RuntimeBlocked)?
                .write_all(input.as_bytes())
                .await
                .map_err(|_| Error::RuntimeBlocked)?;
        }
        let mut stdout = child
            .stdout
            .take()
            .ok_or(Error::RuntimeBlocked)?
            .take(1_048_577);
        let output = tokio::time::timeout(Duration::from_secs(15), async {
            let mut bytes = Vec::new();
            stdout
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| Error::RuntimeBlocked)?;
            if bytes.len() > 1_048_576 {
                return Err(Error::ResourceLimit);
            }
            let status = child.wait().await.map_err(|_| Error::RuntimeBlocked)?;
            Ok(CommandResult {
                success: status.success(),
                stdout: bytes,
            })
        })
        .await
        .map_err(|_| Error::RuntimeBlocked)?;
        if output.is_err() {
            let _ = child.kill().await;
        }
        output
    }
}
pub fn trusted_tool(name: &str) -> Result<PathBuf> {
    if name.contains('/') {
        return Err(Error::RuntimeBlocked);
    }
    for directory in TOOL_PATH.split(':') {
        let path = Path::new(directory).join(name);
        if let Ok(metadata) = path.metadata() {
            if metadata.is_file()
                && metadata.uid() == 0
                && metadata.mode() & 0o022 == 0
                && metadata.mode() & 0o111 != 0
            {
                return Ok(path);
            }
        }
    }
    Err(Error::RuntimeBlocked)
}
fn identity(path: impl AsRef<Path>) -> Result<(u64, u64)> {
    let info = fs::metadata(path).map_err(|_| Error::RuntimeBlocked)?;
    Ok((info.dev(), info.ino()))
}
fn scan(path: &Path, writable: bool, remaining: &mut usize) -> Result<()> {
    let mut pending = vec![(path.to_path_buf(), 0usize)];
    while let Some((path, depth)) = pending.pop() {
        if *remaining == 0 || depth > 128 {
            return Err(Error::ResourceLimit);
        }
        *remaining -= 1;
        let metadata = fs::symlink_metadata(&path).map_err(|_| Error::RuntimeBlocked)?;
        if metadata.is_file() {
            if writable && metadata.nlink() != 1 {
                return Err(Error::RuntimeBlocked);
            }
        } else if metadata.is_dir() {
            for entry in fs::read_dir(path).map_err(|_| Error::RuntimeBlocked)? {
                if pending.len() >= *remaining {
                    return Err(Error::ResourceLimit);
                }
                pending.push((entry.map_err(|_| Error::RuntimeBlocked)?.path(), depth + 1));
            }
        } else if !metadata.file_type().is_symlink() {
            return Err(Error::RuntimeBlocked);
        }
    }
    Ok(())
}
pub(crate) fn readonly_layout(
    spec: &RuntimeSpec,
) -> Result<(Vec<PathBuf>, BTreeMap<PathBuf, PathBuf>)> {
    let mut paths = Vec::new();
    let mut aliases = BTreeMap::new();
    // Mount the public runtime trees explicitly. Binding all of /usr can expose
    // site-specific service sockets and administrative data under /usr/local.
    for path in [
        "/usr/bin",
        "/usr/sbin",
        "/usr/lib",
        "/usr/lib64",
        "/usr/share",
        "/usr/local/bin",
        "/usr/local/lib",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
    ] {
        let path = PathBuf::from(path);
        match fs::canonicalize(&path) {
            Ok(real) => {
                if real != path {
                    aliases.insert(path, real.clone());
                }
                paths.push(real);
            }
            Err(_) if path == Path::new("/usr/bin") || path == Path::new("/bin") => {
                return Err(Error::RuntimeBlocked)
            }
            Err(_) => (),
        }
    }
    for path in &spec.read_only_paths {
        if fs::canonicalize(path).map_err(|_| Error::RuntimeBlocked)? != *path {
            return Err(Error::RuntimeBlocked);
        }
        paths.push(path.clone());
    }
    paths.sort_by(|a, b| {
        a.components()
            .count()
            .cmp(&b.components().count())
            .then(a.cmp(b))
    });
    let mut selected: Vec<PathBuf> = Vec::new();
    for path in paths {
        if !selected.iter().any(|parent| path.starts_with(parent)) {
            selected.push(path);
        }
    }
    Ok((selected, aliases))
}
#[derive(Default)]
struct Resources {
    namespace: bool,
    link: bool,
    table: bool,
    identity: Option<(u64, u64)>,
}
struct RuntimeChild {
    process: Child,
    pidfd: OwnedFd,
}
impl Drop for RuntimeChild {
    fn drop(&mut self) {
        let _ = sys::kill_pidfd(&self.pidfd);
        let _ = self.process.wait();
    }
}
mod executor;
pub use executor::LinuxRuntime;

#[cfg(test)]
mod tests;
