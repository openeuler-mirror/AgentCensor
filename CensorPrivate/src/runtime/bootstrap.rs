//! Single-threaded child bootstrap. Call before constructing any async runtime.
use super::{identity, RuntimeSpec};
use crate::{
    error::{Error, Result},
    json,
};
use censorprivacy_runtime_sys as sys;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Read,
    os::fd::AsRawFd,
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Command,
};
use zeroize::{Zeroize, Zeroizing};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Configuration {
    pub command: Vec<String>,
    pub workspace: PathBuf,
    pub uid: u32,
    pub gid: u32,
    pub environment: BTreeMap<String, String>,
    pub read_only_paths: Vec<PathBuf>,
    pub system_aliases: BTreeMap<PathBuf, PathBuf>,
    pub host_mount: (u64, u64),
    pub host_ipc: (u64, u64),
    pub host_net: (u64, u64),
    pub workspace_identity: (u64, u64),
    pub read_only_identities: BTreeMap<PathBuf, (u64, u64)>,
}
impl Drop for Configuration {
    fn drop(&mut self) {
        for value in self.environment.values_mut() {
            value.zeroize();
        }
        for value in &mut self.command {
            value.zeroize();
        }
    }
}
fn blocked<T>(result: std::io::Result<T>) -> Result<T> {
    result.map_err(|_| Error::RuntimeBlocked)
}
fn snapshot(path: &Path) -> Result<Vec<u8>> {
    let file = blocked(File::open(path))?;
    if !blocked(file.metadata())?.is_file() {
        return Err(Error::RuntimeBlocked);
    }
    let mut data = Vec::new();
    blocked(file.take(1_048_577).read_to_end(&mut data))?;
    if data.len() > 1_048_576 {
        return Err(Error::ResourceLimit);
    }
    Ok(data)
}
fn ca_snapshots(
    path: &Path,
    snapshots: &mut Vec<(PathBuf, Vec<u8>)>,
    bytes: &mut usize,
    count: &mut usize,
) -> Result<()> {
    for entry in blocked(fs::read_dir(path))? {
        let entry = blocked(entry)?;
        if blocked(entry.file_type())?.is_dir() {
            ca_snapshots(&entry.path(), snapshots, bytes, count)?;
        } else {
            let data = snapshot(&entry.path())?;
            *bytes += data.len();
            *count += 1;
            if *count > 4096
                || *bytes > 16 * 1024 * 1024
                || data.windows(11).any(|window| window == b"PRIVATE KEY")
            {
                return Err(Error::RuntimeBlocked);
            }
            snapshots.push((entry.path(), data));
        }
    }
    Ok(())
}
fn target(root: &Path, path: &Path) -> Result<PathBuf> {
    if path
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(Error::RuntimeBlocked);
    }
    Ok(root.join(path.strip_prefix("/").map_err(|_| Error::RuntimeBlocked)?))
}
fn directory(path: &Path, mode: u32) -> Result<()> {
    blocked(fs::create_dir_all(path))?;
    blocked(fs::set_permissions(path, fs::Permissions::from_mode(mode)))
}

pub fn run(config_fd: i32, ready_fd: i32) -> Result<()> {
    if sys::effective_uid() != 0
        || std::process::id() != 1
        || !matches!(std::env::consts::ARCH, "x86_64" | "aarch64")
    {
        return Err(Error::RuntimeBlocked);
    }
    blocked(sys::close_on_exec_nonstdio())?;
    let raw = Zeroizing::new(blocked(sys::read_sealed_configuration(config_fd))?);
    let config: Configuration =
        serde_json::from_value(json::from_slice(&raw)?).map_err(|_| Error::RuntimeBlocked)?;
    if identity("/proc/self/ns/mnt")? == config.host_mount
        || identity("/proc/self/ns/ipc")? == config.host_ipc
        || identity("/proc/self/ns/net")? == config.host_net
    {
        return Err(Error::RuntimeBlocked);
    }
    RuntimeSpec {
        run_id: "bootstrap".into(),
        command: config.command.clone(),
        workspace: config.workspace.clone(),
        gateway_port: 1,
        uid: config.uid,
        gid: config.gid,
        environment: config.environment.clone(),
        ipv4_subnet: None,
        guard_launcher: None,
        guard_group: "censorprivacy".into(),
        read_only_paths: config.read_only_paths.clone(),
    }
    .validate()?;
    let workspace = blocked(
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&config.workspace),
    )?;
    let metadata = blocked(workspace.metadata())?;
    if (metadata.dev(), metadata.ino()) != config.workspace_identity
        || blocked(fs::read_link(format!(
            "/proc/self/fd/{}",
            workspace.as_raw_fd()
        )))? != config.workspace
    {
        return Err(Error::RuntimeBlocked);
    }
    let mut sources = Vec::new();
    for path in &config.read_only_paths {
        if blocked(fs::canonicalize(path))? != *path {
            return Err(Error::RuntimeBlocked);
        }
        let source = blocked(
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .open(path),
        )?;
        let metadata = blocked(source.metadata())?;
        if (!metadata.is_dir() && !metadata.is_file())
            || config.read_only_identities.get(path) != Some(&(metadata.dev(), metadata.ino()))
            || blocked(fs::read_link(format!(
                "/proc/self/fd/{}",
                source.as_raw_fd()
            )))? != *path
        {
            return Err(Error::RuntimeBlocked);
        }
        sources.push((path.clone(), source, metadata.is_dir()));
    }
    let mut snapshots = Vec::new();
    for path in [
        "/etc/hosts",
        "/etc/resolv.conf",
        "/etc/passwd",
        "/etc/group",
        "/etc/nsswitch.conf",
    ] {
        if Path::new(path).exists() {
            snapshots.push((PathBuf::from(path), snapshot(Path::new(path))?));
        }
    }
    if Path::new("/etc/ssl/certs").is_dir() {
        ca_snapshots(Path::new("/etc/ssl/certs"), &mut snapshots, &mut 0, &mut 0)?;
    }
    blocked(sys::set_mount_attributes(Path::new("/"), true, true))?;
    blocked(sys::private_tmpfs(
        Path::new("/tmp"),
        "rw,nosuid,nodev,noexec,mode=1777,size=128m",
    ))?;
    let root = PathBuf::from("/tmp/censorprivacy-root");
    // /tmp was just replaced inside this verified private mount namespace.
    // No preexisting attacker-controlled root or mount targets can be present.
    blocked(fs::create_dir(&root))?;
    blocked(fs::set_permissions(
        &root,
        fs::Permissions::from_mode(0o755),
    ))?;
    for (path, source, is_directory) in sources {
        let destination = target(&root, &path)?;
        if is_directory {
            directory(&destination, 0o755)?;
        } else {
            directory(destination.parent().ok_or(Error::RuntimeBlocked)?, 0o755)?;
            blocked(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&destination),
            )?;
        }
        blocked(sys::bind_descriptor(&source, &destination))?;
        blocked(sys::set_mount_attributes(&destination, false, true))?;
    }
    for (alias, destination) in &config.system_aliases {
        let alias = target(&root, alias)?;
        if blocked(fs::symlink_metadata(&alias)).is_err() {
            blocked(std::os::unix::fs::symlink(destination, alias))?;
        }
    }
    for (path, data) in snapshots {
        let destination = target(&root, &path)?;
        if destination.exists() {
            continue;
        }
        directory(destination.parent().ok_or(Error::RuntimeBlocked)?, 0o755)?;
        blocked(fs::write(&destination, data))?;
        blocked(fs::set_permissions(
            destination,
            fs::Permissions::from_mode(0o444),
        ))?;
    }
    for path in [
        "/run",
        "/tmp",
        "/var/tmp",
        "/dev",
        "/dev/shm",
        "/proc",
        "/sys/fs/cgroup",
    ] {
        directory(&target(&root, Path::new(path))?, 0o755)?;
    }
    for path in ["/run", "/tmp", "/var/tmp", "/dev/shm"] {
        blocked(fs::set_permissions(
            target(&root, Path::new(path))?,
            fs::Permissions::from_mode(0o1777),
        ))?;
    }
    blocked(sys::private_tmpfs(
        &target(&root, Path::new("/dev"))?,
        "rw,nosuid,dev,noexec,size=4m",
    ))?;
    for (name, minor) in [("null", 3), ("zero", 5), ("random", 8), ("urandom", 9)] {
        let destination = target(&root, &Path::new("/dev").join(name))?;
        blocked(sys::create_device(&destination, 1, minor))?;
        blocked(fs::set_permissions(
            destination,
            fs::Permissions::from_mode(0o666),
        ))?;
    }
    directory(&target(&root, Path::new("/dev/shm"))?, 0o1777)?;
    for (alias, destination) in [
        ("fd", "/proc/self/fd"),
        ("stdin", "/proc/self/fd/0"),
        ("stdout", "/proc/self/fd/1"),
        ("stderr", "/proc/self/fd/2"),
    ] {
        blocked(std::os::unix::fs::symlink(
            destination,
            target(&root, &Path::new("/dev").join(alias))?,
        ))?;
    }
    let proc = target(&root, Path::new("/proc"))?;
    blocked(sys::private_proc(&proc))?;
    blocked(sys::set_mount_attributes(&proc, false, true))?;
    let workspace_target = target(&root, &config.workspace)?;
    directory(&workspace_target, 0o755)?;
    blocked(sys::bind_descriptor(&workspace, &workspace_target))?;
    blocked(sys::set_mount_attributes(&workspace_target, false, false))?;
    drop(workspace);
    blocked(sys::chroot(&root, &config.workspace))?;
    blocked(sys::drop_privileges(config.uid, config.gid))?;
    blocked(sys::install_seccomp())?;
    blocked(sys::acknowledge_ready(ready_fd))?;
    let error = Command::new(&config.command[0])
        .args(&config.command[1..])
        .env_clear()
        .envs(&config.environment)
        .exec();
    Err::<(), _>(error).map_err(|_| Error::RuntimeBlocked)
}
