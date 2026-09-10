//! Executable entry point for daemon initialization, supervision, and serving.

#[path = "censorscoped/args.rs"]
mod args;
#[path = "censorscoped/entry.rs"]
mod entry;
#[path = "censorscoped/logging.rs"]
mod logging;
#[path = "censorscoped/process.rs"]
mod process;
#[path = "censorscoped/signals.rs"]
mod signals;

fn main() {
    if let Err(error) = logging::install() {
        tracing::error!(error = %error, "failed to install daemon tracing subscriber");
        std::process::exit(1);
    }
    if let Err(error) = entry::run_from_env() {
        tracing::error!(error = %error, "censorscoped command failed");
        std::process::exit(1);
    }
}
