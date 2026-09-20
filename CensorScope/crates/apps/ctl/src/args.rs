//! Command-line input shapes for the control application.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand};
use config_core::capture_profile::CaptureProfile;
use config_core::daemon::{DEFAULT_OPERATOR_CONFIG_PATH, OperatorConfig};
use control_contract::command::ProcessRef;
use control_contract::selector::TraceSelector;
use model_core::ids::{ProfileName, RequestId, TraceId, TraceName};

use crate::process_ref::process_ref;

/// Fully parsed ctl invocation, including control socket and request identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CtlInvocation {
    pub socket_path: Option<PathBuf>,
    pub request_id: RequestId,
    pub command: CtlCommand,
    /// Emit machine-readable JSON for the control reply.
    pub json: bool,
}

/// Commands implemented by the CensorScope control client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CtlCommand {
    Init {
        config_path: PathBuf,
        force: bool,
        patch_path: Option<PathBuf>,
    },
    TrackAdd {
        root: ProcessRef,
        display_name: TraceName,
        profile_name: ProfileName,
        tags: BTreeSet<String>,
        trace_id: Option<TraceId>,
    },
    TrackRemove {
        selector: TraceSelector,
    },
    OperationStatus {
        operation_id: RequestId,
    },
    ListTraces {
        selector: Option<TraceSelector>,
    },
    Doctor,
    CallStart {
        trace_id: TraceId,
        session_id: Option<String>,
        call_id: String,
        host_pid: u32,
        started_at: u64,
    },
    CallEnd {
        trace_id: TraceId,
        session_id: Option<String>,
        call_id: String,
        host_pid: u32,
        ended_at: u64,
        status: String,
    },
    Export {
        database: PathBuf,
        trace_id: TraceId,
        session_id: String,
        call_id: Option<String>,
        out_path: PathBuf,
        full: bool,
        no_internal: bool,
        page_size: Option<usize>,
        after_event: Option<u64>,
    },
}

/// Parse CLI arguments and load configuration required by remote commands.
pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<CtlInvocation, String> {
    let cli = CtlCli::try_parse_from(std::iter::once("censorscopectl".to_string()).chain(args))
        .unwrap_or_else(|error| error.exit());
    cli.into_invocation()
}

#[derive(Clone, Debug, Parser)]
#[command(
    name = "censorscopectl",
    about = "Control a running CensorScope daemon"
)]
struct CtlCli {
    #[arg(long = "config", global = true, value_name = "PATH")]
    config_path: Option<PathBuf>,

    #[arg(long = "socket-path", global = true, value_name = "PATH")]
    socket_path: Option<PathBuf>,

    #[arg(long = "request-id", global = true, value_name = "ID")]
    request_id: Option<u64>,

    #[arg(long = "json", global = true, help = "Print the control reply as JSON")]
    json: bool,

    #[command(subcommand)]
    command: CtlCommandArgs,
}

impl CtlCli {
    fn into_invocation(self) -> Result<CtlInvocation, String> {
        let explicit_config = self.config_path.is_some();
        let config_path = self.config_path.unwrap_or_else(default_config_path);
        let local_config_command = self.command.is_local_config_command();
        let operator_config = if matches!(&self.command, CtlCommandArgs::Init(_)) {
            None
        } else {
            Some(load_operator_config(&config_path)?)
        };
        let socket_path = if local_config_command {
            None
        } else {
            Some(
                self.socket_path
                    .or_else(|| {
                        operator_config
                            .as_ref()
                            .map(|config| config.socket_path.clone())
                    })
                    .ok_or_else(|| {
                        "missing --socket-path and operator config was not loaded".to_string()
                    })?,
            )
        };
        let request_id = match self.request_id {
            Some(raw) => RequestId::new(raw),
            None => generated_request_id()?,
        };
        Ok(CtlInvocation {
            socket_path,
            request_id,
            command: self.command.into_command(
                operator_config.as_ref(),
                config_path,
                explicit_config,
            )?,
            json: self.json,
        })
    }
}

#[derive(Clone, Debug, Subcommand)]
enum CtlCommandArgs {
    #[command(about = "Initialize the default operator config")]
    Init(InitArgs),
    #[command(about = "Attach a trace to an existing root process")]
    TrackAdd(TrackAddArgs),
    #[command(about = "Remove a trace by selector")]
    TrackRemove(SelectorArgs),
    #[command(name = "operation-status", about = "Read a track-add operation")]
    OperationStatus(OperationStatusArgs),
    #[command(name = "trace-list", about = "List traces")]
    ListTraces(SelectorArgs),
    #[command(about = "Check daemon control-plane readiness")]
    Doctor,
    #[command(name = "call-start", about = "Start a tool call span")]
    CallStart(CallSpanStartArgs),
    #[command(name = "call-end", about = "End a tool call span")]
    CallEnd(CallSpanEndArgs),
    #[command(about = "Export a trace/session snapshot")]
    Export(ExportArgs),
}

impl CtlCommandArgs {
    fn into_command(
        self,
        config: Option<&OperatorConfig>,
        config_path: PathBuf,
        explicit_config: bool,
    ) -> Result<CtlCommand, String> {
        match self {
            Self::Init(args) => Ok(CtlCommand::Init {
                config_path: init_config_path(args.output_path, config_path, explicit_config)?,
                force: args.force,
                patch_path: args.patch_path,
            }),
            Self::TrackAdd(args) => {
                let root_pid = args.root_pid;
                Ok(CtlCommand::TrackAdd {
                    root: process_ref(root_pid)?,
                    display_name: trace_name(args.name, root_pid)?,
                    profile_name: configured_profile(config)?.name.clone(),
                    tags: args.tags.into_iter().collect(),
                    trace_id: args.trace_id,
                })
            }
            Self::TrackRemove(args) => Ok(CtlCommand::TrackRemove {
                selector: required_selector(args)?,
            }),
            Self::OperationStatus(args) => Ok(CtlCommand::OperationStatus {
                operation_id: args.operation_id,
            }),
            Self::ListTraces(args) => Ok(CtlCommand::ListTraces {
                selector: optional_selector(args)?,
            }),
            Self::Doctor => Ok(CtlCommand::Doctor),
            Self::CallStart(a) => Ok(CtlCommand::CallStart {
                trace_id: a.trace_id,
                session_id: a.session_id,
                call_id: a.call_id,
                host_pid: a.host_pid,
                started_at: a.started_at.unwrap_or_else(now_secs),
            }),
            Self::CallEnd(a) => Ok(CtlCommand::CallEnd {
                trace_id: a.trace_id,
                session_id: a.session_id,
                call_id: a.call_id,
                host_pid: a.host_pid,
                ended_at: a.ended_at.unwrap_or_else(now_secs),
                status: a.status,
            }),
            Self::Export(a) => Ok(CtlCommand::Export {
                database: config
                    .ok_or_else(|| "missing operator config".to_string())?
                    .storage
                    .path()
                    .to_path_buf(),
                trace_id: a.trace_id,
                session_id: a.session_id,
                call_id: a.call_id,
                out_path: a.out_path,
                full: a.full,
                no_internal: a.no_internal,
                page_size: a.page_size,
                after_event: a.after_event,
            }),
        }
    }

    fn is_local_config_command(&self) -> bool {
        matches!(self, Self::Init(_) | Self::Export(_))
    }
}

#[derive(Clone, Debug, Args)]
struct CallSpanStartArgs {
    #[arg(long="trace-id", value_parser=parse_trace_id)]
    trace_id: TraceId,
    #[arg(long = "session-id")]
    session_id: Option<String>,
    #[arg(long = "call-id")]
    call_id: String,
    #[arg(long = "pid")]
    host_pid: u32,
    #[arg(long = "started-at")]
    started_at: Option<u64>,
}
#[derive(Clone, Debug, Args)]
struct CallSpanEndArgs {
    #[arg(long="trace-id", value_parser=parse_trace_id)]
    trace_id: TraceId,
    #[arg(long = "session-id")]
    session_id: Option<String>,
    #[arg(long = "call-id")]
    call_id: String,
    #[arg(long = "pid")]
    host_pid: u32,
    #[arg(long = "ended-at")]
    ended_at: Option<u64>,
    #[arg(long = "status", default_value = "success")]
    status: String,
}

#[derive(Clone, Debug, Args)]
struct ExportArgs {
    #[arg(long = "trace-id", alias = "trace", value_parser = parse_trace_id)]
    trace_id: TraceId,
    #[arg(long = "session-id", value_parser = parse_session_id)]
    session_id: String,
    #[arg(
        long = "call-id",
        alias = "call",
        value_name = "CALL_ID",
        help = "Restrict events/payload segments to one tool-call id"
    )]
    call_id: Option<String>,
    #[arg(long = "out-path", value_name = "PATH")]
    out_path: PathBuf,
    #[arg(long = "full")]
    full: bool,
    #[arg(
        long = "no-internal",
        help = "Drop plugin self-noise and diagnostics: censorscopectl housekeeping \
                  processes and tls.coverage application rows"
    )]
    no_internal: bool,
    #[arg(long = "page-size", value_parser = parse_page_size)]
    page_size: Option<usize>,
    #[arg(long = "after-event")]
    after_event: Option<u64>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[derive(Clone, Debug, Args)]
struct InitArgs {
    #[arg(long = "output", value_name = "PATH")]
    output_path: Option<PathBuf>,

    #[arg(long = "patch", value_name = "PATH")]
    patch_path: Option<PathBuf>,

    #[arg(long = "force")]
    force: bool,
}

#[derive(Clone, Debug, Args)]
struct TrackAddArgs {
    #[arg(long = "root-pid", value_name = "PID")]
    root_pid: u32,

    #[arg(long = "name", value_name = "NAME")]
    name: Option<String>,

    #[arg(long = "tag", value_name = "TAG")]
    tags: Vec<String>,

    #[arg(long = "trace-id", value_parser = parse_trace_id, value_name = "ID",
          help = "Continue an existing trace id instead of allocating a new one")]
    trace_id: Option<TraceId>,
}

#[derive(Clone, Debug, Args)]
struct OperationStatusArgs {
    #[arg(long = "operation-id", value_parser = parse_request_id, value_name = "ID")]
    operation_id: RequestId,
}

#[derive(Clone, Debug, Args)]
struct SelectorArgs {
    #[arg(long = "trace-id", value_parser = parse_trace_id, value_name = "ID")]
    trace_id: Option<TraceId>,

    #[arg(long = "root-pid", value_name = "PID")]
    root_pid: Option<u32>,

    #[arg(long = "name", value_name = "NAME")]
    name: Option<String>,

    #[arg(long = "tag-selector", value_name = "TAG")]
    tag_selector: Option<String>,
}

fn load_operator_config(config_path: &PathBuf) -> Result<OperatorConfig, String> {
    OperatorConfig::load(config_path)
}

fn init_config_path(
    output_path: Option<PathBuf>,
    config_path: PathBuf,
    explicit_config: bool,
) -> Result<PathBuf, String> {
    match (output_path, explicit_config) {
        (Some(_), true) => Err("init accepts either --output or --config, not both".to_string()),
        (Some(path), false) => Ok(path),
        (None, true) => Ok(config_path),
        (None, false) => Ok(default_config_path()),
    }
}

fn default_config_path() -> PathBuf {
    PathBuf::from(DEFAULT_OPERATOR_CONFIG_PATH)
}

fn generated_request_id() -> Result<RequestId, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("generate request id: {error}"))?
        .as_nanos();
    let raw = u64::try_from(nanos).map_err(|error| format!("generate request id: {error}"))?;
    Ok(RequestId::new(raw))
}

fn configured_profile(config: Option<&OperatorConfig>) -> Result<&CaptureProfile, String> {
    config
        .map(|config| &config.capture_profile)
        .ok_or_else(|| "missing operator config capture profile".to_string())
}

fn trace_name(raw: Option<String>, root_pid: u32) -> Result<TraceName, String> {
    match raw {
        Some(value) if !value.is_empty() => Ok(TraceName::new(value)),
        Some(_) => Err("invalid --name: value must not be empty".to_string()),
        None => Ok(TraceName::new(format!("pid-{root_pid}"))),
    }
}

fn optional_selector(args: SelectorArgs) -> Result<Option<TraceSelector>, String> {
    let selectors = selector_candidates(args)?;
    match selectors.len() {
        0 => Ok(None),
        1 => Ok(selectors.into_iter().next()),
        _ => Err("selector flags are mutually exclusive".to_string()),
    }
}

fn required_selector(args: SelectorArgs) -> Result<TraceSelector, String> {
    optional_selector(args)?.ok_or_else(|| {
        "one selector flag is required: --trace-id, --root-pid, --name, or --tag-selector"
            .to_string()
    })
}

fn selector_candidates(args: SelectorArgs) -> Result<Vec<TraceSelector>, String> {
    let mut selectors = Vec::new();
    if let Some(raw) = args.trace_id {
        selectors.push(TraceSelector::TraceId(raw));
    }
    if let Some(raw) = args.root_pid {
        selectors.push(TraceSelector::RootPid(raw));
    }
    if let Some(raw) = args.name {
        if raw.is_empty() {
            return Err("invalid --name: value must not be empty".to_string());
        }
        selectors.push(TraceSelector::Name(TraceName::new(raw)));
    }
    if let Some(raw) = args.tag_selector {
        selectors.push(TraceSelector::Tag(raw));
    }
    Ok(selectors)
}

fn parse_trace_id(raw: &str) -> Result<TraceId, String> {
    raw.parse::<u64>()
        .map(TraceId::new)
        .map_err(|error| format!("invalid trace id: {error}"))
}

fn parse_request_id(raw: &str) -> Result<RequestId, String> {
    raw.parse::<u64>()
        .map(RequestId::new)
        .map_err(|error| format!("invalid operation id: {error}"))
}

fn parse_page_size(raw: &str) -> Result<usize, String> {
    let value = raw
        .parse::<usize>()
        .map_err(|error| format!("invalid page size: {error}"))?;
    if value == 0 {
        Err("page size must be greater than zero".to_string())
    } else {
        Ok(value)
    }
}

fn parse_session_id(raw: &str) -> Result<String, String> {
    if raw.is_empty() {
        Err("session id must not be empty".to_string())
    } else {
        Ok(raw.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_uses_default_config_path_without_socket_path() {
        let invocation = parse_args(["init".to_string()]).unwrap();

        assert_eq!(invocation.socket_path, None);
        assert!(matches!(
            invocation.command,
            CtlCommand::Init {
                ref config_path,
                force: false,
                ..
            } if config_path == &PathBuf::from(DEFAULT_OPERATOR_CONFIG_PATH)
        ));
    }

    #[test]
    fn init_can_target_output_path() {
        let invocation = parse_args([
            "init".to_string(),
            "--output".to_string(),
            "/tmp/censorscope-ctl-test.conf".to_string(),
        ])
        .unwrap();

        assert_eq!(invocation.socket_path, None);
        assert!(matches!(
            invocation.command,
            CtlCommand::Init {
                ref config_path,
                force: false,
                ..
            } if config_path == &PathBuf::from("/tmp/censorscope-ctl-test.conf")
        ));
    }
}
