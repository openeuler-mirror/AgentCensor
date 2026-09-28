//! Command-line parsing for daemon initialization and process supervision.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use config_core::capture_profile::CaptureLevel;
use config_core::daemon::DEFAULT_OPERATOR_CONFIG_PATH;

/// Fully resolved command accepted by the `censorscoped` executable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CensorscopedCommand {
    Init {
        config_path: PathBuf,
        force: bool,
        patch_path: Option<PathBuf>,
    },
    Run {
        config_path: PathBuf,
        level: CaptureLevel,
    },
    Start {
        config_path: PathBuf,
        level: CaptureLevel,
    },
    Stop {
        config_path: PathBuf,
    },
    Restart {
        config_path: PathBuf,
        level: CaptureLevel,
    },
    Status {
        config_path: PathBuf,
    },
}

/// Parses command-line arguments and applies the default operator-config path.
///
/// `--level` selects the collection level for the daemon run (`run`/`start`/
/// `restart`) and defaults to L1 when absent. Levels are per-run state only:
/// they are never persisted to the operator file and never switch mid-run.
/// `init`/`stop`/`status` do not accept `--level`.
pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<CensorscopedCommand, String> {
    let cli =
        CensorscopedCli::try_parse_from(std::iter::once("censorscoped".to_string()).chain(args))
            .unwrap_or_else(|error| error.exit());
    let explicit = cli.config_path.is_some();
    let config = cli
        .config_path
        .unwrap_or_else(|| DEFAULT_OPERATOR_CONFIG_PATH.into());
    let default_level = || CaptureLevel::L1;
    Ok(match cli.command {
        Command::Init(args) => {
            reject_level_for_non_run(&cli.level)?;
            CensorscopedCommand::Init {
                config_path: match (args.output, explicit) {
                    (Some(_), true) => {
                        return Err(
                            "init accepts either --output or --config, not both".to_string()
                        );
                    }
                    (Some(path), false) => path,
                    (None, _) => config,
                },
                force: args.force,
                patch_path: args.patch,
            }
        }
        Command::Run => {
            let level = parse_level(cli.level.as_deref(), default_level())?;
            CensorscopedCommand::Run {
                config_path: config,
                level,
            }
        }
        Command::Start => {
            let level = parse_level(cli.level.as_deref(), default_level())?;
            CensorscopedCommand::Start {
                config_path: config,
                level,
            }
        }
        Command::Stop => {
            reject_level_for_non_run(&cli.level)?;
            CensorscopedCommand::Stop {
                config_path: config,
            }
        }
        Command::Restart => {
            let level = parse_level(cli.level.as_deref(), default_level())?;
            CensorscopedCommand::Restart {
                config_path: config,
                level,
            }
        }
        Command::Status => {
            reject_level_for_non_run(&cli.level)?;
            CensorscopedCommand::Status {
                config_path: config,
            }
        }
    })
}

fn parse_level(raw: Option<&str>, default: CaptureLevel) -> Result<CaptureLevel, String> {
    match raw {
        None => Ok(default),
        Some(raw) => raw.parse().map_err(|error| error),
    }
}

/// `--level` selects the collection level of a daemon *run*; `init`, `stop`,
/// and `status` never run the collector, so a level flag there is an error.
fn reject_level_for_non_run(level: &Option<String>) -> Result<(), String> {
    if let Some(raw) = level {
        return Err(format!(
            "--level is only valid with run, start, or restart (got {raw:?})"
        ));
    }
    Ok(())
}

#[derive(Parser)]
#[command(
    name = "censorscoped",
    about = "Run and supervise the CensorScope daemon"
)]
struct CensorscopedCli {
    #[arg(long = "config", global = true, value_name = "PATH")]
    config_path: Option<PathBuf>,
    #[arg(long = "level", global = true, value_name = "LEVEL")]
    level: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init(InitArgs),
    Run,
    Start,
    Stop,
    Restart,
    Status,
}

#[derive(Args)]
struct InitArgs {
    #[arg(long, value_name = "PATH")]
    output: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    patch: Option<PathBuf>,
    #[arg(long)]
    force: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_defaults_to_level_l1() {
        let command = parse_args(["run".to_string()]).unwrap();
        assert_eq!(
            command,
            CensorscopedCommand::Run {
                config_path: PathBuf::from(DEFAULT_OPERATOR_CONFIG_PATH),
                level: CaptureLevel::L1,
            }
        );
    }

    #[test]
    fn explicit_level_is_preserved_for_run_start_restart() {
        for sub in ["run", "start", "restart"] {
            let command =
                parse_args([sub.to_string(), "--level".to_string(), "L3".to_string()]).unwrap();
            let level = match command {
                CensorscopedCommand::Run { level, .. }
                | CensorscopedCommand::Start { level, .. }
                | CensorscopedCommand::Restart { level, .. } => level,
                _ => panic!("expected {sub} command"),
            };
            assert_eq!(level, CaptureLevel::L3);
        }
    }

    #[test]
    fn level_rejected_for_non_run_commands() {
        for sub in ["init", "stop", "status"] {
            let result = parse_args([sub.to_string(), "--level".to_string(), "L1".to_string()]);
            assert!(result.is_err(), "{sub} should reject --level");
        }
    }

    #[test]
    fn unknown_level_is_rejected() {
        let result = parse_args(["run".to_string(), "--level".to_string(), "L4".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn init_uses_default_config_path_without_socket_path() {
        let command = parse_args(["init".to_string()]).unwrap();
        assert!(matches!(
            command,
            CensorscopedCommand::Init { config_path, force: false, patch_path: None }
                if config_path == PathBuf::from(DEFAULT_OPERATOR_CONFIG_PATH)
        ));
    }
}
