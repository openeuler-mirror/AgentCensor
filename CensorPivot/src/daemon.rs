//! Minimal lifecycle manager for the three AgentCensor data-plane daemons.

use crate::model::DoctorReport;
use crate::process::Process;
use crate::{PivotError, Result};
use fs2::FileExt;
use nix::errno::Errno;
use nix::sys::prctl;
use nix::sys::signal::{SigSet, SigmaskHow, Signal, killpg};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{Pid, getppid};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const DEFAULT_CONFIG: &str = "/etc/agentcensor/censord.json";

pub const fn default_config_path() -> &'static str {
    DEFAULT_CONFIG
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    pub state_dir: PathBuf,
    pub startup_timeout_ms: u64,
    pub shutdown_timeout_ms: u64,
    pub poll_interval_ms: u64,
    pub command_output_bytes: usize,
    pub censorfs: CensorFsDaemonConfig,
    pub censorguard: CensorGuardDaemonConfig,
    pub censorscope: CensorScopeDaemonConfig,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            state_dir: "/var/lib/agentcensor/censord".into(),
            startup_timeout_ms: 30_000,
            shutdown_timeout_ms: 10_000,
            poll_interval_ms: 50,
            command_output_bytes: 256 * 1024,
            censorfs: CensorFsDaemonConfig::default(),
            censorguard: CensorGuardDaemonConfig::default(),
            censorscope: CensorScopeDaemonConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CensorFsDaemonConfig {
    pub daemon: PathBuf,
    pub control: PathBuf,
    pub storage_root: PathBuf,
    pub import_root: PathBuf,
    pub socket: PathBuf,
    pub branch: String,
}

impl Default for CensorFsDaemonConfig {
    fn default() -> Self {
        Self {
            daemon: "/usr/libexec/censorfs/censorfsd".into(),
            control: "/usr/libexec/censorfs/censorfsctl".into(),
            storage_root: "/var/lib/censorfs/.censorfs".into(),
            import_root: "/var/lib/agentcensor/workspace".into(),
            socket: "/run/censorfs/control.sock".into(),
            branch: "main".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CensorGuardDaemonConfig {
    pub daemon: PathBuf,
    pub control: PathBuf,
    pub policy: PathBuf,
    pub bpf_object: PathBuf,
    pub state_dir: PathBuf,
    pub control_socket: PathBuf,
    pub event_socket: PathBuf,
    pub launch_socket: PathBuf,
    pub dsh_socket: PathBuf,
    pub ui_socket: PathBuf,
    pub extra_args: Vec<String>,
}

impl Default for CensorGuardDaemonConfig {
    fn default() -> Self {
        Self {
            daemon: "/usr/sbin/censorguardd".into(),
            control: "/usr/bin/censorguardctl".into(),
            policy: "/etc/censorguard/base.yaml".into(),
            bpf_object: "/usr/lib/censorguard/enforce.bpf.o".into(),
            state_dir: "/var/lib/censorguard".into(),
            control_socket: "/run/censorguard/ctl.sock".into(),
            event_socket: "/run/censorguard/events.sock".into(),
            launch_socket: "/run/censorguard/launch.sock".into(),
            dsh_socket: "/run/censorguard/dsh.sock".into(),
            ui_socket: "/run/censorguard/ui.sock".into(),
            extra_args: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CensorScopeDaemonConfig {
    pub daemon: PathBuf,
    pub control: PathBuf,
    pub config: PathBuf,
    pub socket: PathBuf,
    pub level: String,
    pub extra_args: Vec<String>,
}

impl Default for CensorScopeDaemonConfig {
    fn default() -> Self {
        Self {
            daemon: "/usr/sbin/censorscoped".into(),
            control: "/usr/bin/censorscopectl".into(),
            config: "/etc/censorscope/censorscoped.conf".into(),
            socket: "/run/censorscope/censorscoped.sock".into(),
            level: "L3".into(),
            extra_args: Vec::new(),
        }
    }
}

impl DaemonConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let config: Self = serde_json::from_slice(&fs::read(path)?)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.startup_timeout_ms == 0
            || self.shutdown_timeout_ms == 0
            || self.poll_interval_ms == 0
            || self.poll_interval_ms > self.startup_timeout_ms
            || self.command_output_bytes == 0
            || self.command_output_bytes > 16 * 1024 * 1024
        {
            return Err(PivotError::Invalid(
                "invalid censord timeout, polling, or output limit".into(),
            ));
        }
        if self.censorfs.branch.is_empty()
            || !matches!(self.censorscope.level.as_str(), "L1" | "L2" | "L3")
        {
            return Err(PivotError::Invalid(
                "censorfs.branch must be non-empty and censorscope.level must be L1, L2, or L3"
                    .into(),
            ));
        }
        for (name, path) in self.required_paths() {
            if !path.is_absolute() {
                return Err(PivotError::Invalid(format!(
                    "{name} must be an absolute path"
                )));
            }
        }
        Ok(())
    }

    fn required_paths(&self) -> [(&'static str, &Path); 20] {
        [
            ("state_dir", &self.state_dir),
            ("censorfs.daemon", &self.censorfs.daemon),
            ("censorfs.control", &self.censorfs.control),
            ("censorfs.storage_root", &self.censorfs.storage_root),
            ("censorfs.import_root", &self.censorfs.import_root),
            ("censorfs.socket", &self.censorfs.socket),
            ("censorguard.daemon", &self.censorguard.daemon),
            ("censorguard.control", &self.censorguard.control),
            ("censorguard.policy", &self.censorguard.policy),
            ("censorguard.bpf_object", &self.censorguard.bpf_object),
            ("censorguard.state_dir", &self.censorguard.state_dir),
            (
                "censorguard.control_socket",
                &self.censorguard.control_socket,
            ),
            ("censorguard.event_socket", &self.censorguard.event_socket),
            ("censorguard.launch_socket", &self.censorguard.launch_socket),
            ("censorguard.dsh_socket", &self.censorguard.dsh_socket),
            ("censorguard.ui_socket", &self.censorguard.ui_socket),
            ("censorscope.daemon", &self.censorscope.daemon),
            ("censorscope.control", &self.censorscope.control),
            ("censorscope.config", &self.censorscope.config),
            ("censorscope.socket", &self.censorscope.socket),
        ]
    }
}

#[derive(Debug)]
struct CommandSpec {
    program: PathBuf,
    args: Vec<OsString>,
}

impl CommandSpec {
    fn new(program: &Path, args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        Self {
            program: program.to_path_buf(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command
    }
}

#[derive(Debug)]
struct ComponentSpec {
    name: &'static str,
    run: CommandSpec,
}

pub fn initialize(config: &DaemonConfig) -> Result<()> {
    config.validate()?;
    fs::create_dir_all(&config.state_dir)?;
    fs::create_dir_all(&config.censorguard.state_dir)?;
    fs::create_dir_all(&config.censorfs.import_root)?;
    for socket in runtime_sockets(config) {
        let parent = socket.parent().ok_or_else(|| {
            PivotError::Invalid(format!("socket {} has no parent", socket.display()))
        })?;
        fs::create_dir_all(parent)?;
    }

    initialize_censorfs(config)?;
    run_checked("censorguard config", guard_check_command(config), config)?;
    run_checked("censorscope init", scope_init_command(config), config)?;
    Ok(())
}

fn initialize_censorfs(config: &DaemonConfig) -> Result<()> {
    let superblock = config.censorfs.storage_root.join("superblock");
    if superblock.join("slot.a").is_file() || superblock.join("slot.b").is_file() {
        return Ok(());
    }
    if config.censorfs.storage_root.exists()
        && config.censorfs.storage_root.read_dir()?.next().is_some()
    {
        return Err(PivotError::Invalid(format!(
            "CensorFS storage {} is non-empty but has no valid superblock marker",
            config.censorfs.storage_root.display()
        )));
    }
    let command = CommandSpec::new(
        &config.censorfs.control,
        [
            OsString::from("init"),
            OsString::from("--storage-root"),
            config.censorfs.storage_root.as_os_str().to_owned(),
            OsString::from("--import-root"),
            config.censorfs.import_root.as_os_str().to_owned(),
            OsString::from("--branch"),
            OsString::from(&config.censorfs.branch),
        ],
    );
    run_checked("censorfs init", command, config)
}

pub fn doctor(config: &DaemonConfig) -> DoctorReport {
    let mut checks = BTreeMap::new();
    for (name, path) in [
        ("censorfs.daemon", &config.censorfs.daemon),
        ("censorfs.control", &config.censorfs.control),
        ("censorguard.daemon", &config.censorguard.daemon),
        ("censorguard.control", &config.censorguard.control),
        ("censorscope.daemon", &config.censorscope.daemon),
        ("censorscope.control", &config.censorscope.control),
        ("censorguard.policy", &config.censorguard.policy),
        ("censorguard.bpf_object", &config.censorguard.bpf_object),
        ("censorscope.config", &config.censorscope.config),
    ] {
        checks.insert(name.into(), file_check(name, path));
    }
    checks.insert(
        "censorfs.superblock".into(),
        if config
            .censorfs
            .storage_root
            .join("superblock/slot.a")
            .is_file()
            || config
                .censorfs
                .storage_root
                .join("superblock/slot.b")
                .is_file()
        {
            "ok".into()
        } else {
            "missing CensorFS superblock".into()
        },
    );
    for (name, socket) in [
        ("censorfs.socket", &config.censorfs.socket),
        (
            "censorguard.control_socket",
            &config.censorguard.control_socket,
        ),
        (
            "censorguard.launch_socket",
            &config.censorguard.launch_socket,
        ),
        ("censorscope.socket", &config.censorscope.socket),
    ] {
        checks.insert(name.into(), socket_check(socket));
    }
    for (name, command) in doctor_commands(config) {
        let result = run_checked(name, command, config)
            .map(|()| "ok".into())
            .unwrap_or_else(|error| error.to_string());
        checks.insert(format!("{name}.rpc"), result);
    }
    DoctorReport {
        ready: checks.values().all(|value| value == "ok"),
        checks,
    }
}

pub fn run(config: &DaemonConfig, current_exe: &Path) -> Result<()> {
    config.validate()?;
    fs::create_dir_all(&config.state_dir)?;
    let lock = daemon_lock(config)?;
    prctl::set_child_subreaper(true).map_err(std::io::Error::from)?;

    let mut children = ManagedChildren::default();
    let parent_pid = std::process::id();
    for component in component_specs(config) {
        children.spawn(component, current_exe, parent_pid)?;
    }
    let shutdown = install_signal_waiter()?;
    match wait_until_ready(config, &mut children, &shutdown) {
        Ok(true) => {}
        Ok(false) => {
            children.shutdown(Duration::from_millis(config.shutdown_timeout_ms))?;
            drop(lock);
            return Ok(());
        }
        Err(error) => {
            children.shutdown(Duration::from_millis(config.shutdown_timeout_ms))?;
            drop(lock);
            return Err(error);
        }
    }
    println!("[READY] CensorFS, CensorGuard, and CensorScope are healthy");

    let result = monitor(config, &mut children, &shutdown);
    children.shutdown(Duration::from_millis(config.shutdown_timeout_ms))?;
    drop(lock);
    result
}

pub fn component_runner(parent_pid: u32, program: &Path, args: &[OsString]) -> Result<()> {
    prctl::set_pdeathsig(Signal::SIGKILL).map_err(std::io::Error::from)?;
    if getppid().as_raw() != i32::try_from(parent_pid).unwrap_or(-1) {
        return Err(PivotError::Component {
            component: "censord",
            message: "supervisor exited before component exec".into(),
        });
    }
    let error = Command::new(program).args(args).exec();
    Err(error.into())
}

fn daemon_lock(config: &DaemonConfig) -> Result<File> {
    let path = config.state_dir.join("censord.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)?;
    file.try_lock_exclusive().map_err(|error| {
        PivotError::Conflict(format!("another censord owns {}: {error}", path.display()))
    })?;
    Ok(file)
}

fn wait_until_ready(
    config: &DaemonConfig,
    children: &mut ManagedChildren,
    shutdown: &AtomicBool,
) -> Result<bool> {
    let started = Instant::now();
    let timeout = Duration::from_millis(config.startup_timeout_ms);
    let poll = Duration::from_millis(config.poll_interval_ms);
    let mut last_report = None;
    while started.elapsed() < timeout {
        if shutdown.load(Ordering::SeqCst) {
            return Ok(false);
        }
        if let Some((name, status)) = children.exited()? {
            return Err(component_exit(name, status));
        }
        let report = doctor(config);
        if report.ready {
            return Ok(true);
        }
        last_report = Some(report);
        std::thread::sleep(poll);
    }
    Err(PivotError::Component {
        component: "censord",
        message: format!(
            "components did not become healthy within {} ms: {:?}",
            config.startup_timeout_ms,
            last_report.map(|report| report.checks)
        ),
    })
}

fn monitor(
    config: &DaemonConfig,
    children: &mut ManagedChildren,
    shutdown: &AtomicBool,
) -> Result<()> {
    let poll = Duration::from_millis(config.poll_interval_ms);
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return Ok(());
        }
        if let Some((name, status)) = children.exited()? {
            return Err(component_exit(name, status));
        }
        std::thread::sleep(poll);
    }
}

fn component_exit(name: &'static str, status: ExitStatus) -> PivotError {
    PivotError::Component {
        component: name,
        message: format!("daemon exited unexpectedly with {status}"),
    }
}

fn install_signal_waiter() -> Result<Arc<AtomicBool>> {
    let mut signals = SigSet::empty();
    signals.add(Signal::SIGINT);
    signals.add(Signal::SIGTERM);
    signals
        .thread_swap_mask(SigmaskHow::SIG_BLOCK)
        .map_err(std::io::Error::from)?;
    let requested = Arc::new(AtomicBool::new(false));
    let worker_requested = Arc::clone(&requested);
    std::thread::Builder::new()
        .name("censord-signals".into())
        .spawn(move || {
            if signals.wait().is_ok() {
                worker_requested.store(true, Ordering::SeqCst);
            }
        })?;
    Ok(requested)
}

fn component_specs(config: &DaemonConfig) -> [ComponentSpec; 3] {
    [
        ComponentSpec {
            name: "censorfs",
            run: CommandSpec::new(
                &config.censorfs.daemon,
                [
                    OsString::from("--storage-root"),
                    config.censorfs.storage_root.as_os_str().to_owned(),
                    OsString::from("--socket"),
                    config.censorfs.socket.as_os_str().to_owned(),
                ],
            ),
        },
        ComponentSpec {
            name: "censorguard",
            run: guard_run_command(config),
        },
        ComponentSpec {
            name: "censorscope",
            run: scope_run_command(config),
        },
    ]
}

fn guard_check_command(config: &DaemonConfig) -> CommandSpec {
    CommandSpec::new(
        &config.censorguard.daemon,
        [
            OsString::from("--config"),
            config.censorguard.policy.as_os_str().to_owned(),
            OsString::from("--bpf-object"),
            config.censorguard.bpf_object.as_os_str().to_owned(),
            OsString::from("--check-config"),
        ],
    )
}

fn guard_run_command(config: &DaemonConfig) -> CommandSpec {
    let guard = &config.censorguard;
    let mut args = vec![
        OsString::from("--config"),
        guard.policy.as_os_str().to_owned(),
        OsString::from("--bpf-object"),
        guard.bpf_object.as_os_str().to_owned(),
        OsString::from("--ctl-sock"),
        guard.control_socket.as_os_str().to_owned(),
        OsString::from("--event-sock"),
        guard.event_socket.as_os_str().to_owned(),
        OsString::from("--launch-sock"),
        guard.launch_socket.as_os_str().to_owned(),
        OsString::from("--dsh-sock"),
        guard.dsh_socket.as_os_str().to_owned(),
        OsString::from("--ui-sock"),
        guard.ui_socket.as_os_str().to_owned(),
        OsString::from("--state-dir"),
        guard.state_dir.as_os_str().to_owned(),
    ];
    args.extend(guard.extra_args.iter().map(OsString::from));
    CommandSpec::new(&guard.daemon, args)
}

fn scope_init_command(config: &DaemonConfig) -> CommandSpec {
    CommandSpec::new(
        &config.censorscope.daemon,
        [
            OsString::from("--config"),
            config.censorscope.config.as_os_str().to_owned(),
            OsString::from("init"),
        ],
    )
}

fn scope_run_command(config: &DaemonConfig) -> CommandSpec {
    let scope = &config.censorscope;
    let mut args = vec![
        OsString::from("--config"),
        scope.config.as_os_str().to_owned(),
        OsString::from("--level"),
        OsString::from(&scope.level),
    ];
    args.extend(scope.extra_args.iter().map(OsString::from));
    args.push(OsString::from("run"));
    CommandSpec::new(&scope.daemon, args)
}

fn doctor_commands(config: &DaemonConfig) -> [(&'static str, CommandSpec); 3] {
    [
        (
            "censorfs",
            CommandSpec::new(
                &config.censorfs.control,
                [
                    OsString::from("--socket"),
                    config.censorfs.socket.as_os_str().to_owned(),
                    OsString::from("info"),
                ],
            ),
        ),
        (
            "censorguard",
            CommandSpec::new(
                &config.censorguard.control,
                [
                    OsString::from("--socket"),
                    config.censorguard.control_socket.as_os_str().to_owned(),
                    OsString::from("doctor"),
                ],
            ),
        ),
        (
            "censorscope",
            CommandSpec::new(
                &config.censorscope.control,
                [
                    OsString::from("--config"),
                    config.censorscope.config.as_os_str().to_owned(),
                    OsString::from("--socket-path"),
                    config.censorscope.socket.as_os_str().to_owned(),
                    OsString::from("--json"),
                    OsString::from("doctor"),
                ],
            ),
        ),
    ]
}

fn run_checked(name: &'static str, spec: CommandSpec, config: &DaemonConfig) -> Result<()> {
    let output = Process::spawn(&mut spec.command(), config.command_output_bytes, false)
        .map_err(|error| PivotError::Component {
            component: name,
            message: format!(
                "cannot start {}: {error}; {}",
                spec.program.display(),
                component_install_hint(name)
            ),
        })?
        .collect(Duration::from_millis(config.startup_timeout_ms))?;
    if output.timed_out || output.truncated || !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(PivotError::Component {
            component: name,
            message: format!(
                "command failed (status={}, timed_out={}, truncated={}): {}",
                output.status,
                output.timed_out,
                output.truncated,
                detail.trim()
            ),
        });
    }
    Ok(())
}

fn runtime_sockets(config: &DaemonConfig) -> [&Path; 7] {
    [
        &config.censorfs.socket,
        &config.censorguard.control_socket,
        &config.censorguard.event_socket,
        &config.censorguard.launch_socket,
        &config.censorguard.dsh_socket,
        &config.censorguard.ui_socket,
        &config.censorscope.socket,
    ]
}

fn file_check(name: &str, path: &Path) -> String {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => "ok".into(),
        Ok(_) => "not a file".into(),
        Err(error) => format!(
            "missing {}: {error}; {}",
            path.display(),
            component_install_hint(name)
        ),
    }
}

fn component_install_hint(name: &str) -> &'static str {
    if name.starts_with("censorfs") {
        "install CensorFS with scripts/install-agentcensor.sh fs"
    } else if name.starts_with("censorguard") {
        "install CensorGuard with scripts/install-agentcensor.sh guard"
    } else if name.starts_with("censorscope") {
        "install CensorScope with scripts/install-agentcensor.sh scope"
    } else {
        "verify the configured component path"
    }
}

fn socket_check(path: &Path) -> String {
    use std::os::unix::fs::FileTypeExt;
    match fs::metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => "ok".into(),
        Ok(_) => "not a socket".into(),
        Err(error) => error.to_string(),
    }
}

#[derive(Default)]
struct ManagedChildren {
    children: Vec<ManagedChild>,
}

impl ManagedChildren {
    fn spawn(
        &mut self,
        component: ComponentSpec,
        current_exe: &Path,
        parent_pid: u32,
    ) -> Result<()> {
        let mut command = Command::new(current_exe);
        command
            .arg("__component-runner")
            .arg("--parent-pid")
            .arg(parent_pid.to_string())
            .arg("--")
            .arg(&component.run.program)
            .args(&component.run.args)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .process_group(0);
        let child = command.spawn()?;
        let pid = i32::try_from(child.id())
            .map_err(|_| PivotError::Invalid("component PID exceeds i32".into()))?;
        self.children.push(ManagedChild {
            name: component.name,
            child,
            process_group: Pid::from_raw(pid),
            status: None,
        });
        Ok(())
    }

    fn exited(&mut self) -> Result<Option<(&'static str, ExitStatus)>> {
        for child in &mut self.children {
            if child.status.is_none() {
                child.status = child.child.try_wait()?;
            }
            if let Some(status) = child.status {
                return Ok(Some((child.name, status)));
            }
        }
        Ok(None)
    }

    fn shutdown(&mut self, timeout: Duration) -> Result<()> {
        for child in self.children.iter().rev() {
            child.signal(Signal::SIGTERM);
        }
        let started = Instant::now();
        while started.elapsed() < timeout {
            let mut running = false;
            for child in &mut self.children {
                if child.status.is_none() {
                    match child.child.try_wait() {
                        Ok(status) => child.status = status,
                        Err(error) => eprintln!("censord: wait {} failed: {error}", child.name),
                    }
                }
                running |= child.status.is_none();
            }
            if !running {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // Kill each owned group even if its leader exited; descendants may still be members.
        for child in self.children.iter().rev() {
            child.signal(Signal::SIGKILL);
        }
        let mut wait_errors = Vec::new();
        for child in &mut self.children {
            if child.status.is_none() {
                match child.child.wait() {
                    Ok(status) => child.status = Some(status),
                    Err(error) => wait_errors.push(format!("reap {} failed: {error}", child.name)),
                }
            }
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            reap_adopted();
            if self.children.iter().all(ManagedChild::group_is_gone) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let remaining: Vec<_> = self
            .children
            .iter()
            .filter(|child| !child.group_is_gone())
            .map(|child| child.name)
            .collect();
        if !wait_errors.is_empty() || !remaining.is_empty() {
            return Err(PivotError::Component {
                component: "censord",
                message: format!(
                    "component shutdown incomplete: wait_errors={wait_errors:?}, remaining_groups={remaining:?}"
                ),
            });
        }
        Ok(())
    }
}

impl Drop for ManagedChildren {
    fn drop(&mut self) {
        if self.children.iter().any(|child| child.status.is_none()) {
            let _ = self.shutdown(Duration::from_secs(1));
        }
    }
}

struct ManagedChild {
    name: &'static str,
    child: Child,
    process_group: Pid,
    status: Option<ExitStatus>,
}

impl ManagedChild {
    fn signal(&self, signal: Signal) {
        if let Err(error) = killpg(self.process_group, signal)
            && error != Errno::ESRCH
        {
            eprintln!("censord: signal {} failed: {error}", self.name);
        }
    }

    fn group_is_gone(&self) -> bool {
        matches!(killpg(self.process_group, None), Err(Errno::ESRCH))
    }
}

fn reap_adopted() {
    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) | Err(Errno::ECHILD) => return,
            Ok(_) => {}
            Err(error) => {
                eprintln!("censord: reap adopted child failed: {error}");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_is_valid() -> Result<()> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("censord.example.json");
        DaemonConfig::load(&path).map(|_| ())
    }

    #[test]
    fn commands_use_foreground_component_entrypoints() {
        let config = DaemonConfig::default();
        let specs = component_specs(&config);
        assert_eq!(specs.len(), 3);
        assert!(specs[0].run.args.iter().any(|arg| arg == "--storage-root"));
        assert!(!specs[1].run.args.iter().any(|arg| arg == "launch"));
        assert!(specs[2].run.args.iter().any(|arg| arg == "run"));
        assert!(!specs[2].run.args.iter().any(|arg| arg == "start"));
    }

    #[test]
    fn invalid_scope_level_is_rejected() {
        let mut config = DaemonConfig::default();
        config.censorscope.level = "L4".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn missing_component_command_reports_install_hint() {
        let config = DaemonConfig::default();
        let spec = CommandSpec::new(
            Path::new("/definitely-missing-agentcensor-test-binary"),
            std::iter::empty::<OsString>(),
        );
        let result = run_checked("censorfs init", spec, &config);
        assert!(result.is_err());
        let message = result
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(message.contains("/definitely-missing-agentcensor-test-binary"));
        assert!(message.contains("install-agentcensor.sh fs"));
    }

    #[test]
    fn existing_censorfs_superblock_makes_init_idempotent() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let mut config = DaemonConfig::default();
        config.censorfs.storage_root = temporary.path().join("storage");
        config.censorfs.control = temporary.path().join("must-not-run");
        fs::create_dir_all(config.censorfs.storage_root.join("superblock"))?;
        fs::write(
            config.censorfs.storage_root.join("superblock/slot.a"),
            b"fixture",
        )?;

        initialize_censorfs(&config)
    }
}
