use crate::error::{ErrorCode, Result, CensorFsError};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct NamespaceConfig {
    pub workspace: PathBuf,
    pub uid: u32,
    pub gid: u32,
}

impl NamespaceConfig {
    pub fn validate(&self) -> Result<()> {
        if self.workspace != Path::new("/workspace") {
            return Err(CensorFsError::bad_request(
                "v1 only permits the /workspace mount target",
            ));
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
pub fn create_private_mount_namespace(config: &NamespaceConfig) -> Result<()> {
    use std::ffi::CString;
    config.validate()?;
    if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let root = CString::new("/").unwrap();
    if unsafe {
        libc::mount(
            std::ptr::null(),
            root.as_ptr(),
            std::ptr::null(),
            (libc::MS_PRIVATE | libc::MS_REC) as libc::c_ulong,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    std::fs::create_dir_all(&config.workspace)?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn mount_fuse_fd(
    fuse_fd: std::os::fd::RawFd,
    config: &NamespaceConfig,
    read_only: bool,
) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    config.validate()?;
    if fuse_fd < 0 {
        return Err(CensorFsError::bad_request("invalid FUSE file descriptor"));
    }
    let source = CString::new("censorfs").unwrap();
    let fs_type = CString::new("fuse.censorfs").unwrap();
    let target = CString::new(config.workspace.as_os_str().as_bytes())
        .map_err(|_| CensorFsError::bad_request("workspace path contains NUL"))?;
    let options = CString::new(format!(
        "fd={fuse_fd},rootmode=40000,user_id={},group_id={},default_permissions",
        config.uid, config.gid,
    ))
    .unwrap();
    let flags = libc::MS_NOSUID | libc::MS_NODEV | if read_only { libc::MS_RDONLY } else { 0 };
    if unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            fs_type.as_ptr(),
            flags as libc::c_ulong,
            options.as_ptr() as *const libc::c_void,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn ensure_workspace_is_mounted(config: &NamespaceConfig) -> Result<()> {
    config.validate()?;
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")?;
    let expected = config.workspace.to_string_lossy();
    let mounted = mountinfo.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let _mount_id = fields.next();
        let _parent_id = fields.next();
        let _major_minor = fields.next();
        let _root = fields.next();
        fields.next() == Some(expected.as_ref())
    });
    if !mounted {
        return Err(CensorFsError::new(
            ErrorCode::StateChanged,
            "/workspace has no mount in the new namespace",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn drop_to_agent(uid: u32, gid: u32) -> Result<()> {
    if unsafe { libc::prctl(libc::PR_SET_KEEPCAPS, 0, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    for capability in 0..=63 {
        let result = unsafe { libc::prctl(libc::PR_CAPBSET_DROP, capability, 0, 0, 0) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if !matches!(error.raw_os_error(), Some(libc::EINVAL) | Some(libc::EPERM)) {
                return Err(error.into());
            }
        }
    }
    if unsafe { libc::setgroups(0, std::ptr::null()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if unsafe { libc::setgid(gid) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if unsafe { libc::setuid(uid) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    #[repr(C)]
    struct CapabilityHeader {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CapabilityData {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;
    let header = CapabilityHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let data = [CapabilityData {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; 2];
    if unsafe { libc::syscall(libc::SYS_capset, &header, data.as_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    const PR_CAP_AMBIENT: libc::c_int = 47;
    const PR_CAP_AMBIENT_CLEAR_ALL: libc::c_ulong = 4;
    if unsafe { libc::prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn create_private_mount_namespace(_config: &NamespaceConfig) -> Result<()> {
    Err(CensorFsError::new(
        ErrorCode::Unsupported,
        "mount namespaces require Linux",
    ))
}

#[cfg(not(target_os = "linux"))]
pub fn drop_to_agent(_uid: u32, _gid: u32) -> Result<()> {
    Err(CensorFsError::new(
        ErrorCode::Unsupported,
        "credential switching requires Linux",
    ))
}

#[cfg(not(target_os = "linux"))]
pub fn ensure_workspace_is_mounted(_config: &NamespaceConfig) -> Result<()> {
    Err(CensorFsError::new(
        ErrorCode::Unsupported,
        "mount verification requires Linux",
    ))
}
