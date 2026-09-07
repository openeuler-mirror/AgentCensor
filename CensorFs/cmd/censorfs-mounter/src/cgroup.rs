use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct CgroupOptions {
    pub root: PathBuf,
    pub state_dir: PathBuf,
    pub scope: String,
    pub memory_max: Option<String>,
    pub pids_max: Option<String>,
    pub cpu_max: Option<String>,
    pub cleanup_timeout: Duration,
}

#[derive(Debug)]
pub struct CgroupScope {
    path: PathBuf,
    marker: PathBuf,
    state_dir: PathBuf,
    cleanup_timeout: Duration,
}

struct StateLock(File);

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl CgroupOptions {
    pub fn validate(&self) -> Result<(), Box<dyn std::error::Error>> {
        if !self.root.is_absolute() || !self.state_dir.is_absolute() {
            return Err("cgroup root and state directory must be absolute".into());
        }
        if !valid_scope_name(&self.scope) {
            return Err("cgroup scope must match runner-[a-zA-Z0-9_.-]{1,120}".into());
        }
        validate_limit("memory.max", self.memory_max.as_deref(), false)?;
        validate_limit("pids.max", self.pids_max.as_deref(), false)?;
        validate_limit("cpu.max", self.cpu_max.as_deref(), true)?;
        if self.cleanup_timeout.is_zero() {
            return Err("cgroup cleanup timeout must be positive".into());
        }
        Ok(())
    }
}

impl CgroupScope {
    pub fn create(options: &CgroupOptions) -> Result<Self, Box<dyn std::error::Error>> {
        options.validate()?;
        fs::create_dir_all(&options.root)?;
        if !options.root.join("cgroup.controllers").is_file() {
            return Err(format!(
                "{} is not a cgroup v2 directory",
                options.root.display()
            )
            .into());
        }
        fs::create_dir_all(&options.state_dir)?;
        fs::set_permissions(&options.state_dir, fs::Permissions::from_mode(0o700))?;
        let _lock = lock_state(&options.state_dir)?;
        recover_stale_locked(&options.root, &options.state_dir, options.cleanup_timeout)?;
        enable_controllers(&options.root, options)?;

        let path = options.root.join(&options.scope);
        fs::create_dir(&path)?;
        let marker = options.state_dir.join(format!("{}.scope", options.scope));
        let result = (|| {
            write_limit(&path, "memory.max", options.memory_max.as_deref())?;
            if options.memory_max.is_some() {
                fs::write(path.join("memory.oom.group"), "1\n")?;
            }
            write_limit(&path, "pids.max", options.pids_max.as_deref())?;
            write_limit(&path, "cpu.max", options.cpu_max.as_deref())?;
            write_marker(
                &marker,
                unsafe { libc::getpid() },
                process_start_time(unsafe { libc::getpid() })?,
                &boot_id()?,
                fs::metadata(&path)?.ino(),
            )?;
            Ok::<(), Box<dyn std::error::Error>>(())
        })();
        if let Err(error) = result {
            let _ = remove_scope(&path, options.cleanup_timeout);
            let _ = fs::remove_file(&marker);
            return Err(error);
        }
        Ok(Self {
            path,
            marker,
            state_dir: options.state_dir.clone(),
            cleanup_timeout: options.cleanup_timeout,
        })
    }

    pub fn attach_current_process(&self) -> Result<(), Box<dyn std::error::Error>> {
        fs::write(self.path.join("cgroup.procs"), format!("{}\n", unsafe {
            libc::getpid()
        }))?;
        Ok(())
    }

    pub fn cleanup(&self) -> Result<(), Box<dyn std::error::Error>> {
        let _lock = lock_state(&self.state_dir)?;
        remove_scope(&self.path, self.cleanup_timeout)?;
        match fs::remove_file(&self.marker) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn valid_scope_name(value: &str) -> bool {
    value.starts_with("runner-")
        && value.len() <= 127
        && value.len() > "runner-".len()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn validate_limit(
    name: &str,
    value: Option<&str>,
    pair: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(value) = value else {
        return Ok(());
    };
    let parts = value.split_ascii_whitespace().collect::<Vec<_>>();
    let valid_atom = |atom: &str| atom == "max" || atom.parse::<u64>().is_ok_and(|number| number > 0);
    let valid = if pair {
        parts.len() == 2 && valid_atom(parts[0]) && parts[1].parse::<u64>().is_ok_and(|number| number > 0)
    } else {
        parts.len() == 1 && valid_atom(parts[0])
    };
    if !valid {
        return Err(format!("invalid {name} value: {value}").into());
    }
    Ok(())
}

fn lock_state(state_dir: &Path) -> Result<StateLock, Box<dyn std::error::Error>> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(state_dir.join(".lock"))?;
    file.lock_exclusive()?;
    Ok(StateLock(file))
}

fn enable_controllers(
    root: &Path,
    options: &CgroupOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let available = fs::read_to_string(root.join("cgroup.controllers"))?;
    let mut requested = Vec::new();
    if options.memory_max.is_some() {
        requested.push("memory");
    }
    if options.pids_max.is_some() {
        requested.push("pids");
    }
    if options.cpu_max.is_some() {
        requested.push("cpu");
    }
    for controller in &requested {
        if !available.split_ascii_whitespace().any(|value| value == *controller) {
            return Err(format!("cgroup v2 controller {controller} is unavailable").into());
        }
    }
    if !requested.is_empty() {
        let value = requested
            .iter()
            .map(|controller| format!("+{controller}"))
            .collect::<Vec<_>>()
            .join(" ");
        fs::write(root.join("cgroup.subtree_control"), value)?;
    }
    Ok(())
}

fn write_limit(
    path: &Path,
    name: &str,
    value: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(value) = value {
        fs::write(path.join(name), format!("{value}\n"))?;
    }
    Ok(())
}

fn write_marker(
    marker: &Path,
    pid: libc::pid_t,
    start_time: u64,
    boot_id: &str,
    cgroup_inode: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let temporary = marker.with_extension(format!("scope.tmp-{pid}"));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    writeln!(file, "pid={pid}")?;
    writeln!(file, "start_time={start_time}")?;
    writeln!(file, "boot_id={boot_id}")?;
    writeln!(file, "cgroup_inode={cgroup_inode}")?;
    file.sync_all()?;
    fs::rename(&temporary, marker)?;
    Ok(())
}

fn recover_stale_locked(
    root: &Path,
    state_dir: &Path,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !valid_scope_name(name) {
            continue;
        }
        let marker = state_dir.join(format!("{name}.scope"));
        if marker_owner_is_alive(&marker, &entry.path())? {
            continue;
        }
        remove_scope(&entry.path(), timeout)?;
        match fs::remove_file(marker) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn marker_owner_is_alive(
    marker: &Path,
    scope_path: &Path,
) -> Result<bool, Box<dyn std::error::Error>> {
    let contents = match fs::read_to_string(marker) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let mut pid = None;
    let mut expected_start_time = None;
    let mut expected_boot_id = None;
    let mut expected_inode = None;
    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("pid=") {
            pid = value.parse::<libc::pid_t>().ok();
        } else if let Some(value) = line.strip_prefix("start_time=") {
            expected_start_time = value.parse::<u64>().ok();
        } else if let Some(value) = line.strip_prefix("boot_id=") {
            expected_boot_id = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("cgroup_inode=") {
            expected_inode = value.parse::<u64>().ok();
        }
    }
    let (Some(pid), Some(expected_start_time), Some(expected_boot_id), Some(expected_inode)) =
        (pid, expected_start_time, expected_boot_id, expected_inode)
    else {
        return Err(format!("invalid cgroup scope marker {}", marker.display()).into());
    };
    if expected_boot_id != boot_id()? || expected_inode != fs::metadata(scope_path)?.ino() {
        return Err(format!("cgroup scope marker does not match {}", scope_path.display()).into());
    }
    match process_start_time(pid) {
        Ok(actual) => Ok(actual == expected_start_time),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn boot_id() -> std::io::Result<String> {
    Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_owned())
}

fn process_start_time(pid: libc::pid_t) -> std::io::Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let closing = stat
        .rfind(')')
        .ok_or_else(|| std::io::Error::new(ErrorKind::InvalidData, "invalid /proc stat"))?;
    stat[closing + 1..]
        .split_ascii_whitespace()
        .nth(19)
        .ok_or_else(|| std::io::Error::new(ErrorKind::InvalidData, "missing process start time"))?
        .parse::<u64>()
        .map_err(|error| std::io::Error::new(ErrorKind::InvalidData, error))
}

fn remove_scope(path: &Path, timeout: Duration) -> Result<(), Box<dyn std::error::Error>> {
    if !path.exists() {
        return Ok(());
    }
    match fs::write(path.join("cgroup.kill"), "1\n") {
        Ok(()) => {}
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::Unsupported) => {
            kill_members(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    let deadline = Instant::now() + timeout;
    loop {
        if !scope_is_populated(path)? {
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out draining cgroup {}", path.display()).into());
        }
        kill_members(path)?;
        std::thread::sleep(Duration::from_millis(20));
    }
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn kill_members(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let members = match fs::read_to_string(path.join("cgroup.procs")) {
        Ok(members) => members,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for member in members.lines() {
        let pid = member.parse::<libc::pid_t>()?;
        if unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.into());
            }
        }
    }
    Ok(())
}

fn scope_is_populated(path: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let events = match fs::read_to_string(path.join("cgroup.events")) {
        Ok(events) => events,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    Ok(events.lines().any(|line| line == "populated 1"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_scope_names_and_limits() {
        assert!(valid_scope_name("runner-1234.ab_cd"));
        assert!(!valid_scope_name("other-1234"));
        assert!(!valid_scope_name("runner-../escape"));
        assert!(validate_limit("memory.max", Some("max"), false).is_ok());
        assert!(validate_limit("pids.max", Some("256"), false).is_ok());
        assert!(validate_limit("cpu.max", Some("200000 100000"), true).is_ok());
        assert!(validate_limit("cpu.max", Some("200000"), true).is_err());
        assert!(validate_limit("memory.max", Some("0"), false).is_err());
    }

    #[test]
    fn reads_current_process_start_time() {
        assert!(process_start_time(unsafe { libc::getpid() }).unwrap() > 0);
    }
}
