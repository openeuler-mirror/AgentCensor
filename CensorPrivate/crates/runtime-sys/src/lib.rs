//! Small Linux FFI boundary. The privacy core forbids unsafe code.
#![cfg(target_os = "linux")]
#![deny(unsafe_op_in_unsafe_fn)]

use std::{
    ffi::CString,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, fs::FileTypeExt, process::CommandExt},
    },
    path::Path,
    process::{Child, Command},
    time::{Duration, Instant},
};

fn checked(result: libc::c_int) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn path_c(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}
pub fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no pointers and has no preconditions.
    unsafe { libc::geteuid() }
}
fn duplicate(fd: libc::c_int) -> io::Result<File> {
    // SAFETY: fcntl validates fd; a successful F_DUPFD_CLOEXEC returns a new,
    // uniquely owned descriptor. No ownership is taken from the source.
    let new = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if new < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: new is the successful, unique descriptor just obtained above.
    Ok(unsafe { File::from_raw_fd(new) })
}
const SEALS: libc::c_int =
    libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
pub fn sealed_configuration(bytes: &[u8]) -> io::Result<File> {
    if bytes.len() > 1_048_576 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let name = CString::new("censorprivacy-run").expect("constant memfd name");
    // SAFETY: name is a valid C string and flags are the documented Linux ABI.
    let fd =
        unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: memfd_create returned this new descriptor exclusively to us.
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(bytes)?;
    file.seek(SeekFrom::Start(0))?;
    // SAFETY: file owns fd throughout both fcntl calls. No mutable mapping exists.
    checked(unsafe { libc::fcntl(fd, libc::F_ADD_SEALS, SEALS) })?;
    // SAFETY: F_GET_SEALS has no pointer argument; fd remains open.
    let applied = unsafe { libc::fcntl(fd, libc::F_GET_SEALS) };
    if applied < 0 || applied & SEALS != SEALS {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(file)
}
pub fn read_sealed_configuration(fd: i32) -> io::Result<Vec<u8>> {
    if fd <= 2 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut file = duplicate(fd)?;
    // SAFETY: the duplicated file remains alive and F_GET_SEALS validates type.
    let applied = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    if applied < 0 || applied & SEALS != SEALS {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(1_048_577).read_to_end(&mut bytes)?;
    if bytes.len() > 1_048_576 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(bytes)
}
pub fn readiness_pipe() -> io::Result<(File, File)> {
    let mut descriptors = [-1; 2];
    // SAFETY: descriptors points to two initialized writable c_int slots.
    checked(unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) })?;
    // SAFETY: successful pipe2 returned two distinct, uniquely owned fds.
    Ok(unsafe {
        (
            File::from_raw_fd(descriptors[0]),
            File::from_raw_fd(descriptors[1]),
        )
    })
}
pub fn spawn_with_configuration(
    command: &mut Command,
    config: &File,
    ready: &File,
) -> io::Result<Child> {
    let inherited = [config.as_raw_fd(), ready.as_raw_fd()];
    // SAFETY: after fork this closure performs only async-signal-safe libc calls
    // and stack operations. Both borrowed files live until spawn returns after
    // exec. All other fds, including Rust's exec-error pipe, stay CLOEXEC.
    unsafe {
        command.pre_exec(move || {
            checked(libc::setsid())?;
            if libc::syscall(
                libc::SYS_close_range,
                3u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) < 0
            {
                return Err(io::Error::last_os_error());
            }
            for descriptor in inherited {
                checked(libc::fcntl(descriptor, libc::F_SETFD, 0))?;
            }
            Ok(())
        });
    }
    command.spawn()
}
pub fn close_on_exec_nonstdio() -> io::Result<()> {
    // SAFETY: the CLOEXEC flag changes inheritance only, and closes no live Rust
    // handles. The kernel validates the range and rejects unsupported kernels.
    let result = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
pub fn wait_ready(mut reader: File, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(io::ErrorKind::InvalidInput)?;
    loop {
        let millis = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll receives a valid mutable array of exactly one element;
        // reader owns its fd until the call returns.
        let result = unsafe { libc::poll(&mut descriptor, 1, millis) };
        if result < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if result <= 0 {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let mut response = [0u8; 64];
        if reader.read(&mut response)? == 6 && &response[..6] == b"READY\n" {
            return Ok(());
        }
        return Err(io::ErrorKind::PermissionDenied.into());
    }
}
pub fn acknowledge_ready(fd: i32) -> io::Result<()> {
    if fd <= 2 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut file = duplicate(fd)?;
    if !file.metadata()?.file_type().is_fifo() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    file.write_all(b"READY\n")
}
pub fn pidfd_open(pid: u32) -> io::Result<OwnedFd> {
    // SAFETY: pidfd_open has scalar arguments only. The kernel checks the pid.
    let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0u32) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful pidfd_open returns a new descriptor owned by us.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor as i32) })
}
pub fn kill_pidfd(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: fd is owned and alive; a null siginfo requests kernel-generated
    // SIGKILL. This targets the opened process identity, never a reused pid.
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            libc::SIGKILL,
            std::ptr::null::<libc::siginfo_t>(),
            0u32,
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
/// pidfds become readable on exit, including for children reaped by PID 1.
pub fn pidfd_exited(fd: &OwnedFd) -> io::Result<bool> {
    let mut descriptor = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll receives a valid one-element mutable stack array, while fd
    // remains owned. Timeout zero observes identity without blocking or reaping.
    let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    if descriptor.revents & libc::POLLNVAL != 0 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(descriptor.revents & (libc::POLLIN | libc::POLLHUP) != 0)
}
pub fn check_stdio(workspace: &Path) -> io::Result<()> {
    for descriptor in 0..=2 {
        let file = match duplicate(descriptor) {
            Ok(file) => file,
            Err(error) if error.raw_os_error() == Some(libc::EBADF) => continue,
            Err(error) => return Err(error),
        };
        let kind = file.metadata()?.file_type();
        let destination = std::fs::read_link(format!("/proc/self/fd/{descriptor}"))?;
        let allowed = if kind.is_socket() {
            let mut family: libc::c_int = 0;
            let mut length = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
            // SAFETY: family and length point to initialized writable stack
            // values, and file owns the descriptor for this synchronous call.
            checked(unsafe {
                libc::getsockopt(
                    file.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_DOMAIN,
                    (&mut family as *mut libc::c_int).cast(),
                    &mut length,
                )
            })?;
            length as usize == std::mem::size_of::<libc::c_int>() && family == libc::AF_UNIX
        } else if kind.is_file() {
            // SAFETY: F_GETFL has no pointer argument and file is alive.
            let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
            if flags < 0 {
                return Err(io::Error::last_os_error());
            }
            (flags & libc::O_ACCMODE == libc::O_RDONLY)
                || (!destination.as_os_str().as_bytes().ends_with(b" (deleted)")
                    && destination
                        .canonicalize()?
                        .starts_with(workspace.canonicalize()?))
        } else if kind.is_char_device() {
            // SAFETY: isatty validates the scalar fd and mutates no Rust values.
            (unsafe { libc::isatty(file.as_raw_fd()) == 1 })
                || destination == Path::new("/dev/null")
        } else if kind.is_fifo() {
            destination
                .to_str()
                .and_then(|value| value.strip_prefix("pipe:["))
                .and_then(|value| value.strip_suffix(']'))
                .is_some_and(|value| value.parse::<u64>().is_ok())
        } else {
            false
        };
        if !allowed {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
    }
    Ok(())
}

#[repr(C)]
struct MountAttr {
    attr_set: u64,
    attr_clr: u64,
    propagation: u64,
    userns_fd: u64,
}
pub fn set_mount_attributes(path: &Path, recursive: bool, readonly: bool) -> io::Result<()> {
    let path = path_c(path)?;
    let attributes = MountAttr {
        attr_set: if readonly { 7 } else { 6 },
        attr_clr: if readonly { 0 } else { 1 },
        propagation: 0,
        userns_fd: 0,
    };
    // SAFETY: path and attributes outlive the call with their exact C layouts;
    // flags and struct size follow mount_setattr's x86_64/aarch64 Linux ABI.
    let result = unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            libc::AT_FDCWD,
            path.as_ptr(),
            if recursive { libc::AT_RECURSIVE } else { 0 },
            &attributes,
            std::mem::size_of::<MountAttr>(),
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn mount(
    source: Option<&Path>,
    destination: &Path,
    filesystem: Option<&str>,
    flags: libc::c_ulong,
    options: Option<&str>,
) -> io::Result<()> {
    let source = source.map(path_c).transpose()?;
    let destination = path_c(destination)?;
    let filesystem = filesystem
        .map(CString::new)
        .transpose()
        .map_err(|_| io::ErrorKind::InvalidInput)?;
    let options = options
        .map(CString::new)
        .transpose()
        .map_err(|_| io::ErrorKind::InvalidInput)?;
    // SAFETY: every optional C string is NUL-terminated and remains alive for
    // the synchronous syscall. Null pointers represent absent ABI parameters.
    checked(unsafe {
        libc::mount(
            source
                .as_ref()
                .map_or(std::ptr::null(), |value| value.as_ptr()),
            destination.as_ptr(),
            filesystem
                .as_ref()
                .map_or(std::ptr::null(), |value| value.as_ptr()),
            flags,
            options
                .as_ref()
                .map_or(std::ptr::null(), |value| value.as_ptr().cast()),
        )
    })
}
pub fn private_tmpfs(destination: &Path, options: &str) -> io::Result<()> {
    // mount(2) receives VFS options in flags, unlike mount(8)'s -o parser.
    let mut flags = 0;
    let mut filesystem_options = Vec::new();
    for option in options.split(',') {
        match option {
            "rw" | "dev" => (),
            "nosuid" => flags |= libc::MS_NOSUID,
            "nodev" => flags |= libc::MS_NODEV,
            "noexec" => flags |= libc::MS_NOEXEC,
            value if value.starts_with("mode=") || value.starts_with("size=") => {
                filesystem_options.push(value)
            }
            _ => return Err(io::ErrorKind::InvalidInput.into()),
        }
    }
    mount(
        Some(Path::new("tmpfs")),
        destination,
        Some("tmpfs"),
        flags,
        Some(&filesystem_options.join(",")),
    )
}
pub fn bind_descriptor(source: &File, destination: &Path) -> io::Result<()> {
    mount(
        Some(&Path::new("/proc/self/fd").join(source.as_raw_fd().to_string())),
        destination,
        None,
        libc::MS_BIND,
        None,
    )
}
pub fn private_proc(destination: &Path) -> io::Result<()> {
    mount(
        Some(Path::new("proc")),
        destination,
        Some("proc"),
        libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
        None,
    )
}
pub fn chroot(path: &Path, workspace: &Path) -> io::Result<()> {
    let path = path_c(path)?;
    let workspace = path_c(workspace)?;
    // SAFETY: both C strings remain valid across synchronous path syscalls.
    checked(unsafe { libc::chroot(path.as_ptr()) })?;
    // SAFETY: workspace remains a valid NUL-terminated string after chroot.
    checked(unsafe { libc::chdir(workspace.as_ptr()) })
}
pub fn create_device(path: &Path, major: u32, minor: u32) -> io::Result<()> {
    let path = path_c(path)?;
    // SAFETY: path is valid for the call and makedev constructs the Linux dev_t.
    checked(unsafe {
        libc::mknod(
            path.as_ptr(),
            libc::S_IFCHR | 0o666,
            libc::makedev(major, minor),
        )
    })
}
pub fn drop_privileges(uid: u32, gid: u32) -> io::Result<()> {
    if uid == 0 || gid == 0 || uid >= (1 << 31) || gid >= (1 << 31) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let last_cap: u32 = std::fs::read_to_string("/proc/sys/kernel/cap_last_cap")?
        .trim()
        .parse()
        .map_err(|_| io::ErrorKind::InvalidData)?;
    if last_cap > 63 {
        return Err(io::ErrorKind::Unsupported.into());
    }
    for cap in 0..=last_cap {
        // SAFETY: PR_CAPBSET_DROP takes scalar capability arguments only.
        checked(unsafe { libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0) })?;
    }
    // SAFETY: count zero ignores the null groups pointer. Scalar uid/gid were
    // validated non-root above; the OS enforces all privilege transitions.
    checked(unsafe { libc::setgroups(0, std::ptr::null()) })?;
    // SAFETY: setgid and setuid have scalar arguments and no memory preconditions.
    checked(unsafe { libc::setgid(gid) })?;
    // SAFETY: setuid is applied only after supplementary groups and gid drop.
    checked(unsafe { libc::setuid(uid) })?;
    #[repr(C)]
    struct Header {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Data {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    let header = Header {
        version: 0x2008_0522,
        pid: 0,
    };
    let data = [Data {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; 2];
    // SAFETY: capset's version-3 ABI takes exactly these header/data layouts.
    let result = unsafe { libc::syscall(libc::SYS_capset, &header, data.as_ptr()) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: prctl arguments are scalar documented NO_NEW_PRIVS/PDEATHSIG ABI.
    checked(unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) })?;
    // SAFETY: setuid cleared PDEATHSIG, so restore it before executing the agent.
    checked(unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) })
}

#[repr(C)]
struct ArgCmp {
    arg: u32,
    op: i32,
    datum_a: u64,
    datum_b: u64,
}
pub fn install_seccomp() -> io::Result<()> {
    type Init = unsafe extern "C" fn(u32) -> *mut libc::c_void;
    type Resolve = unsafe extern "C" fn(*const libc::c_char) -> i32;
    type Rule = unsafe extern "C" fn(*mut libc::c_void, u32, i32, u32, *const ArgCmp) -> i32;
    type Load = unsafe extern "C" fn(*mut libc::c_void) -> i32;
    type Release = unsafe extern "C" fn(*mut libc::c_void);
    // SAFETY: only the trusted system libseccomp ABI is loaded. The bootstrap
    // has a cleared environment. Library and symbol handles live until release.
    let library = unsafe { libloading::Library::new("libseccomp.so.2") }
        .map_err(|_| io::ErrorKind::Unsupported)?;
    // SAFETY: these signatures exactly match libseccomp's documented C API.
    let (init, resolve, rule, load, release) = unsafe {
        (
            library.get::<Init>(b"seccomp_init\0"),
            library.get::<Resolve>(b"seccomp_syscall_resolve_name\0"),
            library.get::<Rule>(b"seccomp_rule_add_array\0"),
            library.get::<Load>(b"seccomp_load\0"),
            library.get::<Release>(b"seccomp_release\0"),
        )
    };
    let init = init.map_err(|_| io::ErrorKind::Unsupported)?;
    let resolve = resolve.map_err(|_| io::ErrorKind::Unsupported)?;
    let rule = rule.map_err(|_| io::ErrorKind::Unsupported)?;
    let load = load.map_err(|_| io::ErrorKind::Unsupported)?;
    let release = release.map_err(|_| io::ErrorKind::Unsupported)?;
    // SAFETY: SCMP_ACT_ALLOW is a valid default action.
    let context = unsafe { init(0x7fff_0000) };
    if context.is_null() {
        return Err(io::ErrorKind::Other.into());
    }
    let result = (|| {
        let deny =
            |name: &str, comparisons: &[ArgCmp], error: i32, required: bool| -> io::Result<()> {
                let name = CString::new(name).map_err(|_| io::ErrorKind::InvalidInput)?;
                // SAFETY: name is a valid C string and resolve has the checked ABI.
                let number = unsafe { resolve(name.as_ptr()) };
                if number < 0 {
                    return if required {
                        Err(io::ErrorKind::Unsupported.into())
                    } else {
                        Ok(())
                    };
                }
                // SAFETY: context is live and comparisons contains count elements of
                // the exact scmp_arg_cmp layout; the C API copies them synchronously.
                let result = unsafe {
                    rule(
                        context,
                        0x0005_0000 | error as u32,
                        number,
                        comparisons.len() as u32,
                        if comparisons.is_empty() {
                            std::ptr::null()
                        } else {
                            comparisons.as_ptr()
                        },
                    )
                };
                if result != 0 {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Ok(())
            };
        for family in [libc::AF_UNIX, libc::AF_NETLINK, libc::AF_PACKET] {
            deny(
                "socket",
                &[ArgCmp {
                    arg: 0,
                    op: 4,
                    datum_a: family as u64,
                    datum_b: 0,
                }],
                libc::EPERM,
                true,
            )?;
        }
        for name in ["setns", "unshare", "ptrace"] {
            deny(name, &[], libc::EPERM, true)?;
        }
        for name in [
            "mount",
            "mount_setattr",
            "umount",
            "umount2",
            "pivot_root",
            "open_tree",
            "move_mount",
            "fsopen",
            "fsconfig",
            "fsmount",
            "fspick",
            "bpf",
            "process_vm_readv",
            "process_vm_writev",
            "pidfd_getfd",
            "open_by_handle_at",
            "mknod",
            "mknodat",
            "kexec_load",
            "reboot",
        ] {
            deny(name, &[], libc::EPERM, false)?;
        }
        deny("clone3", &[], libc::ENOSYS, false)?;
        for flag in [
            0x0002_0000,
            0x0200_0000,
            0x0400_0000,
            0x0800_0000,
            0x1000_0000,
            0x2000_0000,
            0x4000_0000,
            0x80,
        ] {
            deny(
                "clone",
                &[ArgCmp {
                    arg: 0,
                    op: 7,
                    datum_a: flag,
                    datum_b: flag,
                }],
                libc::EPERM,
                true,
            )?;
        }
        // SAFETY: context is completely built and still owned by this function.
        if unsafe { load(context) } != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    })();
    // SAFETY: this is the sole release of the live context, before library drop.
    unsafe { release(context) };
    result
}

/// Start a child in a new session with only standard descriptors inherited.
pub fn spawn_session(command: &mut Command) -> io::Result<Child> {
    // SAFETY: this closure uses only async-signal-safe syscalls after fork.
    unsafe {
        command.pre_exec(|| {
            checked(libc::setsid())?;
            if libc::syscall(
                libc::SYS_close_range,
                3u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn()
}
/// Observe exit without reaping; its PID/session cannot be reused before wait.
pub fn exited_unreaped(pidfd: &OwnedFd) -> io::Result<bool> {
    // SAFETY: siginfo_t is an output-only C struct initialized by waitid;
    // pidfd is borrowed and remains open for the synchronous call.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: correct Linux P_PIDFD ABI; WNOWAIT keeps process identity pinned.
    checked(unsafe {
        libc::waitid(
            libc::P_PIDFD,
            pidfd.as_raw_fd() as u32,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    })?;
    // SAFETY: waitid initialized the siginfo union, and si_pid is its child field.
    Ok(unsafe { info.si_pid() != 0 })
}
/// Caller must retain its unreaped session leader until after this operation.
pub fn kill_session(child: &Child) -> io::Result<()> {
    let id: i32 = child
        .id()
        .try_into()
        .map_err(|_| io::ErrorKind::InvalidInput)?;
    // SAFETY: scalar negative PID targets the session's process group; the
    // caller retains its unreaped leader, preventing group-ID reuse.
    let result = unsafe { libc::kill(-id, libc::SIGKILL) };
    if result < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn immutable_seals_are_verified_before_configuration_reads() {
        let mut file = sealed_configuration(b"private configuration").unwrap();
        assert!(file.write_all(b"changed").is_err());
        assert_eq!(
            read_sealed_configuration(file.as_raw_fd()).unwrap(),
            b"private configuration"
        );
        assert!(read_sealed_configuration(0).is_err());
    }
    #[test]
    fn readiness_requires_the_exact_private_pipe_acknowledgement() {
        let (reader, writer) = readiness_pipe().unwrap();
        let thread = std::thread::spawn(move || {
            acknowledge_ready(writer.as_raw_fd()).unwrap();
        });
        wait_ready(reader, Duration::from_secs(1)).unwrap();
        thread.join().unwrap();
        let (reader, mut writer) = readiness_pipe().unwrap();
        writer.write_all(b"WRONG\n").unwrap();
        assert!(wait_ready(reader, Duration::from_secs(1)).is_err());
    }
    #[test]
    fn pidfd_signalling_targets_the_opened_child() {
        let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let descriptor = pidfd_open(child.id()).unwrap();
        assert!(!pidfd_exited(&descriptor).unwrap());
        let killed = kill_pidfd(&descriptor);
        if killed.is_err() {
            let _ = child.kill();
        }
        let status = child.wait().unwrap();
        assert!(pidfd_exited(&descriptor).unwrap());
        assert!(killed.is_ok());
        assert!(!status.success());
    }
}
