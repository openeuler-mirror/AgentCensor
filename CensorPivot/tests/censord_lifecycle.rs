use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn daemon_script(
    root: &Path,
    name: &str,
    lifetime_seconds: u32,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = root.join(format!("{name}.sh"));
    let body = format!(
        "#!/bin/sh\n\
         echo $$ > {root}/{name}.pid\n\
         sleep {lifetime_seconds} &\n\
         echo $! > {root}/{name}.child.pid\n\
         wait\n",
        root = root.display()
    );
    fs::write(&path, body)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    Ok(path)
}

fn assert_process_gone(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let pid = fs::read_to_string(path)?.trim().parse::<u32>()?;
    let proc_path = PathBuf::from(format!("/proc/{pid}"));
    let started = Instant::now();
    while proc_path.exists() && started.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !proc_path.exists(),
        "process {pid} from {} survived",
        path.display()
    );
    Ok(())
}

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
}

fn fixture(scope_lifetime: u32) -> Result<Fixture, Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().to_path_buf();
    let fs_daemon = daemon_script(&root, "censorfs", 300)?;
    let guard_daemon = daemon_script(&root, "censorguard", 300)?;
    let scope_daemon = daemon_script(&root, "censorscope", scope_lifetime)?;
    let storage = root.join("storage/superblock");
    fs::create_dir_all(&storage)?;
    fs::write(storage.join("slot.a"), b"fixture")?;
    let scope_config = root.join("scope.conf");
    let guard_policy = root.join("guard.yaml");
    let bpf_object = root.join("guard.bpf.o");
    fs::write(&scope_config, b"fixture")?;
    fs::write(&guard_policy, b"rules: []\n")?;
    fs::write(&bpf_object, b"fixture")?;

    let config_path = root.join("censord.json");
    let config: Value = json!({
        "state_dir": root.join("state"),
        "startup_timeout_ms": 2000,
        "shutdown_timeout_ms": 500,
        "poll_interval_ms": 10,
        "command_output_bytes": 4096,
        "censorfs": {
            "daemon": fs_daemon,
            "control": "/bin/true",
            "storage_root": root.join("storage"),
            "import_root": root.join("import"),
            "socket": root.join("fs.sock"),
            "branch": "main"
        },
        "censorguard": {
            "daemon": guard_daemon,
            "control": "/bin/true",
            "policy": guard_policy,
            "bpf_object": bpf_object,
            "state_dir": root.join("guard-state"),
            "control_socket": root.join("guard-ctl.sock"),
            "event_socket": root.join("guard-events.sock"),
            "launch_socket": root.join("guard-launch.sock"),
            "dsh_socket": root.join("guard-dsh.sock"),
            "ui_socket": root.join("guard-ui.sock"),
            "extra_args": []
        },
        "censorscope": {
            "daemon": scope_daemon,
            "control": "/bin/true",
            "config": scope_config,
            "socket": root.join("scope.sock"),
            "level": "L3",
            "extra_args": []
        }
    });
    fs::write(&config_path, serde_json::to_vec_pretty(&config)?)?;
    Ok(Fixture {
        _temporary: temporary,
        root,
        config: config_path,
    })
}

fn spawn(fixture: &Fixture) -> Result<std::process::Child, Box<dyn std::error::Error>> {
    Ok(Command::new(env!("CARGO_BIN_EXE_censord"))
        .args(["run", "--config"])
        .arg(&fixture.config)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?)
}

fn assert_all_processes_gone(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    for name in [
        "censorfs.pid",
        "censorfs.child.pid",
        "censorguard.pid",
        "censorguard.child.pid",
        "censorscope.pid",
        "censorscope.child.pid",
    ] {
        assert_process_gone(&root.join(name))?;
    }
    Ok(())
}

fn wait_for_pid_files(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        if ["censorfs", "censorguard", "censorscope"]
            .iter()
            .all(|name| root.join(format!("{name}.child.pid")).is_file())
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err("component PID files were not created".into())
}

#[test]
fn startup_component_exit_stops_all_components_and_their_children()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture(1)?;

    let daemon = spawn(&fixture)?;
    let output = daemon.wait_with_output()?;
    assert!(
        !output.status.success(),
        "censord should report the component failure: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("censorscope"),
        "unexpected error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_all_processes_gone(&fixture.root)
}

#[test]
fn sigterm_during_startup_stops_every_component_process() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = fixture(300)?;
    let daemon = spawn(&fixture)?;
    wait_for_pid_files(&fixture.root)?;
    std::thread::sleep(Duration::from_millis(50));

    kill(Pid::from_raw(i32::try_from(daemon.id())?), Signal::SIGTERM)?;
    let output = daemon.wait_with_output()?;
    assert!(
        output.status.success(),
        "censord signal shutdown failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_all_processes_gone(&fixture.root)
}
