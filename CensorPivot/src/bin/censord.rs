use censorpivot::daemon::{
    DaemonConfig, component_runner, default_config_path, doctor, initialize,
};
use censorpivot::{PivotError, Result};
use clap::{Parser, Subcommand};
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "censord",
    version,
    about = "Supervise the AgentCensor component daemons"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Initialize component state and validate component configuration.
    Init {
        #[arg(long, default_value = default_config_path())]
        config: PathBuf,
    },
    /// Run and supervise all three component daemons in the foreground.
    Run {
        #[arg(long, default_value = default_config_path())]
        config: PathBuf,
    },
    /// Check all component binaries, sockets, and control-plane round trips.
    Doctor {
        #[arg(long, default_value = default_config_path())]
        config: PathBuf,
    },
    /// Upgrade preserved pre-0.5 Guard settings, saving .pre-guard-0.5.0.bak files.
    MigrateGuard {
        #[arg(long, default_value = default_config_path())]
        config: PathBuf,
        #[arg(long, default_value = "/etc/censorpivot/config.json")]
        pivot_config: PathBuf,
    },
    #[command(name = "__component-runner", hide = true)]
    ComponentRunner {
        #[arg(long)]
        parent_pid: u32,
        program: PathBuf,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("censord: {error}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<i32> {
    match Cli::parse().command {
        Commands::Init { config } => {
            let config = DaemonConfig::load(&config)?;
            initialize(&config)?;
            println!("censord: initialization complete");
            Ok(0)
        }
        Commands::Run { config } => {
            let config = DaemonConfig::load(&config)?;
            let executable = std::env::current_exe().map_err(PivotError::Io)?;
            censorpivot::daemon::run(&config, &executable)?;
            Ok(0)
        }
        Commands::Doctor { config } => {
            let config = DaemonConfig::load(&config)?;
            let report = doctor(&config);
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(i32::from(!report.ready))
        }
        Commands::MigrateGuard {
            config,
            pivot_config,
        } => {
            let paths = censorpivot::migrate::migrate_guard(&config, &pivot_config)?;
            for path in &paths {
                println!("migrated {}", path.display());
            }
            if paths.is_empty() {
                println!("Guard configuration is already current");
            }
            Ok(0)
        }
        Commands::ComponentRunner {
            parent_pid,
            program,
            args,
        } => component_runner(parent_pid, &program, &args).map(|()| 0),
    }
}
