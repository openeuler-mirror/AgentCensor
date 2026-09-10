#[cfg(target_os = "linux")]
mod cgroup;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "censorfs-mounter",
    about = "Privileged CensorFS mount-namespace helper"
)]
struct Arguments {
    #[arg(long, default_value = "/run/censorfs/control.sock")]
    socket: std::path::PathBuf,
    #[arg(long)]
    view_id: String,
    #[arg(long)]
    uid: u32,
    #[arg(long)]
    gid: u32,
    #[arg(long)]
    read_only: bool,
    #[arg(long)]
    cgroup_root: Option<std::path::PathBuf>,
    #[arg(long)]
    cgroup_state_dir: Option<std::path::PathBuf>,
    #[arg(long)]
    cgroup_scope: Option<String>,
    #[arg(long)]
    cgroup_memory_max: Option<String>,
    #[arg(long)]
    cgroup_pids_max: Option<String>,
    #[arg(long)]
    cgroup_cpu_max: Option<String>,
    #[arg(long, default_value_t = 5000)]
    cgroup_cleanup_timeout_ms: u64,
    #[arg(last = true, required = true)]
    command: Vec<String>,
}

#[cfg(target_os = "linux")]
fn run_worker(arguments: &Arguments) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    let config = censorfs_core::namespace::NamespaceConfig {
        workspace: "/workspace".into(),
        uid: arguments.uid,
        gid: arguments.gid,
    };
    censorfs_core::namespace::create_private_mount_namespace(&config)?;
    let fuse = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fuse")?;
    censorfs_core::namespace::mount_fuse_fd(fuse.as_raw_fd(), &config, arguments.read_only)?;
    let view_id = censorfs_core::ViewId::parse(&arguments.view_id)?;
    censorfs_core::control::attach_fuse(
        &arguments.socket,
        &censorfs_core::control::AttachFuseRequest {
            view_id,
            owner_uid: arguments.uid,
            owner_gid: arguments.gid,
            read_only: arguments.read_only,
        },
        fuse.as_raw_fd(),
    )?;
    censorfs_core::namespace::ensure_workspace_is_mounted(&config)?;
    drop(fuse);
    censorfs_core::namespace::drop_to_agent(arguments.uid, arguments.gid)?;
    std::env::set_current_dir(&config.workspace)?;
    let error = std::process::Command::new(&arguments.command[0])
        .args(&arguments.command[1..])
        .exec();
    Err(error.into())
}

#[cfg(target_os = "linux")]
fn cgroup_options(
    arguments: &Arguments,
) -> Result<Option<cgroup::CgroupOptions>, Box<dyn std::error::Error>> {
    let supplied = [
        arguments.cgroup_root.is_some(),
        arguments.cgroup_state_dir.is_some(),
        arguments.cgroup_scope.is_some(),
    ];
    if supplied.iter().any(|value| *value) && !supplied.iter().all(|value| *value) {
        return Err("cgroup root, state directory, and scope must be supplied together".into());
    }
    let (Some(root), Some(state_dir), Some(scope)) = (
        arguments.cgroup_root.clone(),
        arguments.cgroup_state_dir.clone(),
        arguments.cgroup_scope.clone(),
    ) else {
        if arguments.cgroup_memory_max.is_some()
            || arguments.cgroup_pids_max.is_some()
            || arguments.cgroup_cpu_max.is_some()
        {
            return Err("cgroup limits require a cgroup scope".into());
        }
        return Ok(None);
    };
    Ok(Some(cgroup::CgroupOptions {
        root,
        state_dir,
        scope,
        memory_max: arguments.cgroup_memory_max.clone(),
        pids_max: arguments.cgroup_pids_max.clone(),
        cpu_max: arguments.cgroup_cpu_max.clone(),
        cleanup_timeout: std::time::Duration::from_millis(
            arguments.cgroup_cleanup_timeout_ms,
        ),
    }))
}

#[cfg(target_os = "linux")]
fn block_supervisor_signals() -> Result<libc::sigset_t, std::io::Error> {
    let mut signals = unsafe { std::mem::zeroed::<libc::sigset_t>() };
    if unsafe { libc::sigemptyset(&mut signals) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGCHLD] {
        if unsafe { libc::sigaddset(&mut signals, signal) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    if unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &signals, std::ptr::null_mut()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(signals)
}

#[cfg(target_os = "linux")]
fn unblock_supervisor_signals(signals: &libc::sigset_t) -> Result<(), std::io::Error> {
    if unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, signals, std::ptr::null_mut()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn child_exit_code(status: libc::c_int) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    }
}

#[cfg(target_os = "linux")]
fn supervise(
    arguments: &Arguments,
    options: &cgroup::CgroupOptions,
) -> Result<i32, Box<dyn std::error::Error>> {
    let original_parent = unsafe { libc::getppid() };
    let signals = block_supervisor_signals()?;
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if unsafe { libc::getppid() } != original_parent {
        unsafe {
            libc::kill(libc::getpid(), libc::SIGTERM);
        }
    }
    let scope = cgroup::CgroupScope::create(options)?;
    let worker = unsafe { libc::fork() };
    if worker < 0 {
        let error = std::io::Error::last_os_error();
        scope.cleanup()?;
        return Err(error.into());
    }
    if worker == 0 {
        let result: Result<(), Box<dyn std::error::Error>> = (|| {
            unblock_supervisor_signals(&signals)?;
            scope.attach_current_process()?;
            run_worker(arguments)
        })();
        if let Err(error) = result {
            eprintln!("censorfs-mounter worker failed: {error}");
        }
        unsafe { libc::_exit(1) }
    }

    unsafe {
        libc::close(libc::STDIN_FILENO);
        libc::close(libc::STDOUT_FILENO);
    }
    loop {
        let signal = unsafe { libc::sigwaitinfo(&signals, std::ptr::null_mut()) };
        if signal < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            scope.cleanup()?;
            return Err(error.into());
        }
        if signal == libc::SIGCHLD {
            let mut status = 0;
            let waited = unsafe { libc::waitpid(worker, &mut status, libc::WNOHANG) };
            if waited == worker {
                scope.cleanup()?;
                return Ok(child_exit_code(status));
            }
            if waited < 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ECHILD) {
                    scope.cleanup()?;
                    return Err(error.into());
                }
            }
            continue;
        }
        scope.cleanup()?;
        let mut status = 0;
        loop {
            let waited = unsafe { libc::waitpid(worker, &mut status, 0) };
            if waited == worker || (waited < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD)) {
                break;
            }
            if waited < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        return Ok(128 + signal);
    }
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = Arguments::parse();
    if let Some(options) = cgroup_options(&arguments)? {
        let exit_code = supervise(&arguments, &options)?;
        std::process::exit(exit_code);
    }
    run_worker(&arguments)
}

#[cfg(not(target_os = "linux"))]
fn main() {
    let _ = Arguments::parse();
    eprintln!("censorfs-mounter requires Linux");
    std::process::exit(2);
}
