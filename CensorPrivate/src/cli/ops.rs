use std::{os::unix::process::CommandExt, path::PathBuf, process::Command};

const COMMANDS: &[&str] = &[
    "doctor",
    "deps",
    "install",
    "init",
    "start",
    "stop",
    "dashboard",
    "check",
    "systemd",
    "demo",
    "dsh",
    "xiaoo",
    "verify",
    "self-test",
];

fn location() -> Option<(PathBuf, PathBuf)> {
    let executable = std::env::current_exe().ok()?;
    let bin = executable.parent()?.to_path_buf();
    let installed = bin.parent()?.join("share/censorprivacy");
    if installed.join("scripts/ops/entry.sh").is_file() {
        return Some((installed, bin));
    }
    let source = bin.parent()?.parent()?;
    if source.join("Cargo.toml").is_file() && source.join("scripts/ops/entry.sh").is_file() {
        return Some((source.to_path_buf(), bin));
    }
    None
}

// Replace this process so signals and exit codes keep the shell launcher's behavior.
pub fn dispatch() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let command = args.first().and_then(|value| value.to_str());
    let help =
        command == Some("help") && args.get(1).and_then(|value| value.to_str()) == Some("managed");
    if !help && !command.is_some_and(|value| COMMANDS.contains(&value)) {
        return;
    }
    if let Some((root, bin)) = location() {
        let mut shell = Command::new("bash");
        shell.arg(root.join("scripts/ops/entry.sh"));
        if help
            || args
                .get(1)
                .is_some_and(|value| value == "--help" || value == "-h")
        {
            shell.arg("help");
        } else {
            shell.args(args);
        }
        let _ = shell
            .env("CP_REPO_ROOT", root)
            .env("CP_DEFAULT_BIN_DIR", bin)
            .exec();
    }
    eprintln!("CensorPrivacy management files unavailable; install share/censorprivacy alongside bin/censorprivacy");
    std::process::exit(2);
}
