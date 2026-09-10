use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "censorfsd", about = "CensorFS core daemon")]
struct Arguments {
    #[arg(long)]
    storage_root: PathBuf,
    #[arg(long)]
    socket: Option<PathBuf>,
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = Arguments::parse();
    let socket = arguments
        .socket
        .unwrap_or_else(|| censorfs_core::control::default_socket(&arguments.storage_root));
    let fs = censorfs_core::CensorFs::open(arguments.storage_root)?;
    censorfs_core::control::ControlServer::new(socket, fs).run()?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    let _ = Arguments::parse();
    eprintln!("censorfsd requires Linux");
    std::process::exit(2);
}
