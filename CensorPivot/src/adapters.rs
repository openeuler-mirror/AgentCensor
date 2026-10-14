use crate::config::Config;
use crate::engine::{CensorFs, CensorScope, ToolRunner, ToolSession};
use crate::error::{PivotError, Result};
use crate::model::{
    BatchRequest, CallResult, DoctorReport, FsResources, RunnerReply, RunnerRequest, ToolCall,
    TransactionRecord,
};
use crate::process::{Output, Process};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct CommandAdapters {
    config: Config,
}

impl CommandAdapters {
    pub const fn new(config: Config) -> Self {
        Self { config }
    }

    fn command_json(&self, component: &'static str, command: &mut Command) -> Result<Value> {
        // Allow JSON escaping of the FS control protocol's bounded 2 MiB frame.
        let output = Process::spawn(command, 16 * 1024 * 1024, false)?
            .collect(Duration::from_millis(self.config.component_timeout_ms))?;
        if output.timed_out || output.truncated {
            return Err(PivotError::component(
                component,
                "command timed out or reply exceeded limit",
            ));
        }
        decode_reply(component, output)
    }

    fn censorfs(&self, args: &[String]) -> Result<Value> {
        self.command_json(
            "censorfs",
            Command::new(&self.config.censorfs.command)
                .arg("--socket")
                .arg(&self.config.censorfs.socket)
                .arg("--json")
                .args(args),
        )
    }

    fn scope_request(&self, args: &[String]) -> Result<Value> {
        self.command_json(
            "censorscope",
            Command::new(&self.config.censorscope.command)
                .arg("--config")
                .arg(&self.config.censorscope.operator_config)
                .arg("--socket-path")
                .arg(&self.config.censorscope.socket)
                .arg("--json")
                .args(args),
        )
    }

    fn scope_command(&self, args: &[String]) -> Result<()> {
        let reply = self.scope_request(args)?;
        if reply.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err(PivotError::component(
                "censorscope",
                reply
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("control command failed"),
            ))
        }
    }

    pub fn doctor_report(&self) -> DoctorReport {
        let mut checks = BTreeMap::new();
        if let Some(path) = &self.config.supervisor_config {
            checks.insert(
                "components.configuration".into(),
                crate::daemon::DaemonConfig::load(path)
                    .and_then(|supervisor| self.config.validate_supervisor(&supervisor))
                    .map(|()| "ok".to_string())
                    .unwrap_or_else(|error| error.to_string()),
            );
        }
        check_file(
            &mut checks,
            "censorfs.mounter_command",
            Path::new(&self.config.censorfs.mounter_command[0]),
        );
        checks.insert(
            "censorfs.fuse".into(),
            if Path::new("/dev/fuse").exists() {
                "ok"
            } else {
                "missing /dev/fuse"
            }
            .into(),
        );
        checks.insert(
            "censorfs.cgroup_v2".into(),
            if self
                .config
                .censorfs
                .cgroup_root
                .join("cgroup.controllers")
                .is_file()
            {
                "ok"
            } else {
                "missing cgroup v2 root"
            }
            .into(),
        );
        check_file(
            &mut checks,
            "censorfs.command",
            &self.config.censorfs.command,
        );
        check_file(
            &mut checks,
            "censorguard.control",
            &self.config.censorguard.control,
        );
        check_file(
            &mut checks,
            "censorscope.command",
            &self.config.censorscope.command,
        );
        check_socket(&mut checks, "censorfs.socket", &self.config.censorfs.socket);
        match crate::guard::load_config(&self.config.censorguard.config) {
            Ok(guard) => check_socket(
                &mut checks,
                "censorguard.launch_socket",
                &guard.launch_socket,
            ),
            Err(error) => {
                checks.insert("censorguard.config".into(), error.to_string());
            }
        }
        checks.insert(
            "censorguard.rpc".into(),
            crate::guard::status(
                &self.config.censorguard.control,
                &self.config.censorguard.config,
                Duration::from_millis(self.config.component_timeout_ms),
            )
            .and_then(|status| {
                crate::guard::require_ready(
                    &status,
                    &self.config.censorguard.config,
                    &self.config.censorguard.allowed_groups,
                )
            })
            .map(|()| "ok".into())
            .unwrap_or_else(|error| error.to_string()),
        );
        check_socket(
            &mut checks,
            "censorscope.socket",
            &self.config.censorscope.socket,
        );
        checks.insert(
            "censorfs.rpc".into(),
            match self.censorfs(&["info".into()]) {
                Ok(_) => "ok".into(),
                Err(error) => error.to_string(),
            },
        );
        checks.insert(
            "censorscope.rpc".into(),
            match self.scope_request(&["doctor".into()]) {
                Ok(reply)
                    if reply.get("storage_ready").and_then(Value::as_bool) == Some(true)
                        && reply
                            .get("available_collectors")
                            .and_then(Value::as_array)
                            .is_some_and(|items| !items.is_empty()) =>
                {
                    "ok".into()
                }
                Ok(_) => "daemon not ready".into(),
                Err(error) => error.to_string(),
            },
        );
        let ready = checks.values().all(|value| value == "ok")
            || (!self.config.censorscope.required
                && checks
                    .iter()
                    .filter(|(key, _)| !key.starts_with("censorscope."))
                    .all(|(_, value)| value == "ok"));
        DoctorReport { ready, checks }
    }
}

fn check_file(checks: &mut BTreeMap<String, String>, name: &str, path: &Path) {
    let status = fs::metadata(path)
        .map(|metadata| {
            if metadata.is_file() {
                "ok"
            } else {
                "not a file"
            }
        })
        .unwrap_or("missing");
    checks.insert(name.into(), status.into());
}

fn check_socket(checks: &mut BTreeMap<String, String>, name: &str, path: &Path) {
    use std::os::unix::fs::FileTypeExt;
    let status = fs::metadata(path)
        .map(|metadata| {
            if metadata.file_type().is_socket() {
                "ok"
            } else {
                "not a socket"
            }
        })
        .unwrap_or("missing");
    checks.insert(name.into(), status.into());
}

fn string_at(value: &Value, path: &[&str]) -> Result<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key).ok_or_else(|| {
            PivotError::component("censorfs", format!("reply missing {}", path.join(".")))
        })?;
    }
    current.as_str().map(str::to_owned).ok_or_else(|| {
        PivotError::component("censorfs", format!("{} is not a string", path.join(".")))
    })
}

fn u32_at(value: &Value, path: &[&str]) -> Result<u32> {
    let mut current = value;
    for key in path {
        current = current.get(*key).ok_or_else(|| {
            PivotError::component("censorfs", format!("reply missing {}", path.join(".")))
        })?;
    }
    current
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(|| {
            PivotError::component("censorfs", format!("{} is not a u32", path.join(".")))
        })
}

impl CensorFs for CommandAdapters {
    fn open(&self, record: &TransactionRecord) -> Result<FsResources> {
        let args = vec![
            "--request-id".into(),
            record.open_request_id.to_string(),
            "variant-open".into(),
            "--branch".into(),
            record.summary.branch.clone(),
            "--expected-generation".into(),
            record.summary.expected_generation.clone(),
            "--expected-head-seq".into(),
            record.summary.expected_head_seq.to_string(),
            "--run".into(),
            record.transaction_id.to_string(),
            "--variant".into(),
            "batch".into(),
        ];
        let reply = self.censorfs(&args)?;
        Ok(FsResources {
            open_attempted: true,
            ticket_id: Some(string_at(&reply, &["result", "ticket", "ticket_id"])?),
            view_id: Some(string_at(&reply, &["result", "view", "view_id"])?),
            candidate_id: None,
            owner_uid: Some(u32_at(&reply, &["result", "view", "owner_uid"])?),
            owner_gid: Some(u32_at(&reply, &["result", "view", "owner_gid"])?),
        })
    }

    fn prepare(&self, record: &TransactionRecord) -> Result<String> {
        let ticket = required(&record.fs.ticket_id, "ticket_id")?;
        let view = required(&record.fs.view_id, "view_id")?;
        let args = vec![
            "--request-id".into(),
            record.prepare_request_id.to_string(),
            "variant-prepare".into(),
            "--ticket".into(),
            ticket.into(),
            "--view".into(),
            view.into(),
            "--run".into(),
            record.transaction_id.to_string(),
            "--variant".into(),
            "batch".into(),
            "--timeout-ms".into(),
            self.config.censorfs.prepare_timeout_ms.to_string(),
            "--max-diff-file-bytes".into(),
            self.config.censorfs.max_diff_file_bytes.to_string(),
        ];
        let reply = self.censorfs(&args)?;
        string_at(&reply, &["result", "candidate", "candidate_id"])
    }

    fn publish(&self, record: &TransactionRecord) -> Result<()> {
        let candidate = required(&record.fs.candidate_id, "candidate_id")?;
        let args = vec![
            "--request-id".into(),
            record.finish_request_id.to_string(),
            "variant-publish".into(),
            "--candidate".into(),
            candidate.into(),
            "--expected-generation".into(),
            record.summary.expected_generation.clone(),
            "--expected-head-seq".into(),
            record.summary.expected_head_seq.to_string(),
            "--decision-id".into(),
            record.transaction_id.to_string(),
            "--run".into(),
            record.transaction_id.to_string(),
            "--variant".into(),
            "batch".into(),
        ];
        self.censorfs(&args).map(|_| ())
    }

    fn abort(&self, record: &TransactionRecord) -> Result<()> {
        let Some(ticket) = record.fs.ticket_id.as_ref() else {
            return Ok(());
        };
        let mut args = vec![
            "--request-id".into(),
            record.finish_request_id.to_string(),
            "variant-abort".into(),
            "--ticket".into(),
            ticket.clone(),
        ];
        if let Some(view) = &record.fs.view_id {
            args.extend(["--view".into(), view.clone()]);
        }
        args.extend([
            "--run".into(),
            record.transaction_id.to_string(),
            "--variant".into(),
            "batch".into(),
        ]);
        self.censorfs(&args).map(|_| ())
    }
}

fn required<'a>(value: &'a Option<String>, name: &str) -> Result<&'a str> {
    value
        .as_deref()
        .ok_or_else(|| PivotError::Protocol(format!("transaction is missing {name}")))
}

impl CensorScope for CommandAdapters {
    fn required(&self) -> bool {
        self.config.censorscope.required
    }

    fn start_batch(&self, request: &BatchRequest, host_pid: u32) -> Result<Option<u64>> {
        let mut args = vec![
            "track-add".into(),
            "--root-pid".into(),
            host_pid.to_string(),
            "--name".into(),
            format!("censorpivot-{}", request.session_id),
            "--tag".into(),
            "censorpivot".into(),
            "--tag".into(),
            format!("session:{}", request.session_id),
        ];
        if let Some(trace_id) = request.trace_id {
            args.extend(["--trace-id".into(), trace_id.to_string()]);
        }
        let reply = self.scope_request(&args)?;
        if reply.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(PivotError::component(
                "censorscope",
                reply
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("track-add failed"),
            ));
        }
        let trace_id = reply
            .get("trace_id")
            .and_then(Value::as_u64)
            .filter(|trace_id| *trace_id > 0)
            .ok_or_else(|| {
                PivotError::component("censorscope", "track-add returned no trace_id")
            })?;
        Ok(Some(trace_id))
    }

    fn call_start(
        &self,
        request: &BatchRequest,
        call: &ToolCall,
        host_pid: u32,
        trace_id: Option<u64>,
    ) -> Result<()> {
        let Some(trace_id) = trace_id else {
            return if self.required() {
                Err(PivotError::component("censorscope", "trace_id is required"))
            } else {
                Ok(())
            };
        };
        self.scope_command(&[
            "call-start".into(),
            "--trace-id".into(),
            trace_id.to_string(),
            "--session-id".into(),
            request.session_id.clone(),
            "--call-id".into(),
            call.id.clone(),
            "--pid".into(),
            host_pid.to_string(),
        ])
    }

    fn call_end(
        &self,
        request: &BatchRequest,
        call: &ToolCall,
        host_pid: u32,
        trace_id: Option<u64>,
        status: &str,
    ) -> Result<()> {
        let Some(trace_id) = trace_id else {
            return Ok(());
        };
        self.scope_command(&[
            "call-end".into(),
            "--trace-id".into(),
            trace_id.to_string(),
            "--session-id".into(),
            request.session_id.clone(),
            "--call-id".into(),
            call.id.clone(),
            "--pid".into(),
            host_pid.to_string(),
            "--status".into(),
            status.into(),
        ])
    }

    fn finish_batch(
        &self,
        _request: &BatchRequest,
        _host_pid: u32,
        trace_id: Option<u64>,
    ) -> Result<()> {
        let Some(trace_id) = trace_id else {
            return Ok(());
        };
        self.scope_command(&[
            "track-remove".into(),
            "--trace-id".into(),
            trace_id.to_string(),
        ])
    }
}

impl ToolRunner for CommandAdapters {
    fn start(&self, request: &BatchRequest, fs: &FsResources) -> Result<Box<dyn ToolSession>> {
        if !self
            .config
            .censorguard
            .allowed_groups
            .contains(&request.guard_group)
        {
            return Err(PivotError::Invalid(
                "Guard group is not permitted by Pivot configuration".into(),
            ));
        }
        let view_id = required(&fs.view_id, "view_id")?;
        let uid = fs
            .owner_uid
            .filter(|uid| *uid != 0)
            .ok_or_else(|| PivotError::Invalid("runner View owner must be non-root".into()))?;
        let gid = fs
            .owner_gid
            .filter(|gid| *gid != 0)
            .ok_or_else(|| PivotError::Invalid("runner View group must be non-root".into()))?;
        let guard = crate::guard::load_config(&self.config.censorguard.config)?;
        let status = crate::guard::status(
            &self.config.censorguard.control,
            &self.config.censorguard.config,
            Duration::from_millis(self.config.component_timeout_ms),
        )?;
        crate::guard::require_ready(
            &status,
            &self.config.censorguard.config,
            std::slice::from_ref(&request.guard_group),
        )?;
        let guard_boot = status.daemon_boot_id;
        let executable = std::env::current_exe()?;
        let identity = uuid::Uuid::new_v4();
        let mut command = Command::new(&self.config.censorfs.mounter_command[0]);
        command
            .args(&self.config.censorfs.mounter_command[1..])
            .arg("--socket")
            .arg(&self.config.censorfs.socket)
            .arg("--view-id")
            .arg(view_id)
            .arg("--uid")
            .arg(uid.to_string())
            .arg("--gid")
            .arg(gid.to_string())
            .arg("--cgroup-root")
            .arg(&self.config.censorfs.cgroup_root)
            .arg("--cgroup-state-dir")
            .arg(&self.config.censorfs.cgroup_state_dir)
            .arg("--cgroup-scope")
            .arg(format!("runner-{identity}"))
            .arg("--cgroup-cleanup-timeout-ms")
            .arg("5000")
            .arg("--")
            .arg(executable)
            .arg("__batch-runner")
            .arg("--guard-socket")
            .arg(&guard.launch_socket)
            .arg("--guard-domain")
            .arg(&request.guard_scope)
            .arg("--guard-group")
            .arg(&request.guard_group)
            .arg("--guard-timeout-ms")
            .arg(self.config.runner_timeout_ms.to_string())
            .arg("--max-output-bytes")
            .arg(self.config.max_output_bytes.to_string())
            .arg("--max-frame-bytes")
            .arg(self.config.max_frame_bytes.to_string())
            .env_clear()
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )
            .env("HOME", "/workspace")
            .envs(&self.config.runner_environment);
        // Lossy UTF-8 and JSON escaping can expand each captured output byte sixfold.
        let limit = self.config.max_output_bytes * 12 + 65536;
        let timeout = Duration::from_millis(self.config.runner_timeout_ms);
        let mut process = Process::spawn(&mut command, limit, true)?;
        let host_pid = match serde_json::from_slice(&process.line(timeout)?)? {
            RunnerReply::Ready { host_pid } if host_pid > 0 && i32::try_from(host_pid).is_ok() => {
                host_pid
            }
            _ => {
                return Err(PivotError::Protocol(
                    "runner did not send a valid Ready PID".into(),
                ));
            }
        };
        let status = crate::guard::status(
            &self.config.censorguard.control,
            &self.config.censorguard.config,
            Duration::from_millis(self.config.component_timeout_ms),
        )?;
        crate::guard::require_ready(
            &status,
            &self.config.censorguard.config,
            std::slice::from_ref(&request.guard_group),
        )?;
        crate::guard::require_runner(
            &status,
            &request.guard_scope,
            &request.guard_group,
            host_pid,
            &guard_boot,
        )?;
        Ok(Box::new(CommandSession {
            guard: self.config.censorguard.clone(),
            guard_timeout: Duration::from_millis(self.config.component_timeout_ms),
            guard_boot,
            guard_domain: request.guard_scope.clone(),
            guard_group: request.guard_group.clone(),
            process: Some(process),
            host_pid,
            timeout,
            environment: self.config.runner_environment.clone(),
            session_id: request.session_id.clone(),
        }))
    }
}

struct CommandSession {
    guard: crate::config::CensorGuardConfig,
    guard_timeout: Duration,
    guard_boot: String,
    guard_domain: String,
    guard_group: String,
    process: Option<Process>,
    host_pid: u32,
    timeout: Duration,
    environment: BTreeMap<String, String>,
    session_id: String,
}

impl CommandSession {
    fn check_guard(&self, require_runner: bool) -> Result<()> {
        let status =
            crate::guard::status(&self.guard.control, &self.guard.config, self.guard_timeout)?;
        crate::guard::require_ready(
            &status,
            &self.guard.config,
            std::slice::from_ref(&self.guard_group),
        )?;
        if status.daemon_boot_id != self.guard_boot {
            return Err(PivotError::component(
                "censorguard",
                "daemon restarted during batch; aborting",
            ));
        }
        if require_runner {
            crate::guard::require_runner(
                &status,
                &self.guard_domain,
                &self.guard_group,
                self.host_pid,
                &self.guard_boot,
            )?;
        }
        Ok(())
    }
}

impl ToolSession for CommandSession {
    fn host_pid(&self) -> u32 {
        self.host_pid
    }

    fn run(&mut self, call: &ToolCall) -> Result<CallResult> {
        self.check_guard(true)?;
        let process = self
            .process
            .as_mut()
            .ok_or_else(|| PivotError::Protocol("runner already closed".into()))?;
        let mut frame = serde_json::to_vec(&RunnerRequest {
            session_id: self.session_id.clone(),
            call: call.clone(),
            environment: self.environment.clone(),
        })?;
        frame.push(b'\n');
        process.send(&frame, self.timeout)?;
        let reply = serde_json::from_slice(
            &process.line(Duration::from_millis(call.timeout_ms) + self.timeout)?,
        )?;
        self.check_guard(true)?;
        match reply {
            RunnerReply::Result(result) if result.call_id == call.id => Ok(result),
            RunnerReply::Error(message) => Err(PivotError::component("runner", message)),
            _ => Err(PivotError::Protocol(
                "runner reply does not match current call".into(),
            )),
        }
    }

    fn finish(&mut self) -> Result<()> {
        self.check_guard(true)?;
        let Some(process) = self.process.take() else {
            return Ok(());
        };
        let output = process.collect(self.timeout)?;
        if output.status.success() && !output.timed_out {
            self.check_guard(false)
        } else {
            Err(PivotError::component(
                "runner",
                format!(
                    "runner/cleanup failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ))
        }
    }
}

pub fn run_local_call(request: &RunnerRequest, max_output_bytes: usize) -> Result<CallResult> {
    run_local_call_in(request, max_output_bytes, Path::new("/workspace"))
}

fn run_local_call_in(
    request: &RunnerRequest,
    max_output_bytes: usize,
    workspace: &Path,
) -> Result<CallResult> {
    let workspace = workspace.canonicalize()?;
    let relative = if request.call.cwd.is_empty() {
        Path::new(".")
    } else {
        Path::new(&request.call.cwd)
    };
    let target = workspace.join(relative).canonicalize()?;
    if !target.starts_with(&workspace) {
        return Err(PivotError::Invalid("tool cwd escapes /workspace".into()));
    }
    let mut command = Command::new(&request.call.program);
    command
        .args(&request.call.args)
        .current_dir(target)
        .env_clear()
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("HOME", "/workspace")
        .envs(&request.environment)
        .envs(&request.call.env)
        .env("CENSORSCOPE_SESSION_ID", &request.session_id)
        .env("DSH_CENSORSCOPE_CALL_ID", &request.call.id);
    let started = Instant::now();
    let output = Process::spawn(&mut command, max_output_bytes, false)?
        .collect(Duration::from_millis(request.call.timeout_ms))?;
    Ok(CallResult {
        call_id: request.call.id.clone(),
        exit_code: output.status.code().unwrap_or(128),
        timed_out: output.timed_out,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        output_truncated: output.truncated,
    })
}

fn decode_reply(component: &'static str, output: Output) -> Result<Value> {
    let reply: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        PivotError::component(
            component,
            format!(
                "invalid JSON: {error}; {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        )
    })?;
    if !output.status.success() || reply.get("ok").and_then(Value::as_bool) == Some(false) {
        let message = reply
            .get("error")
            .or_else(|| reply.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("command failed");
        return Err(PivotError::component(
            component,
            format!(
                "{}: {message}",
                reply.get("code").and_then(Value::as_str).unwrap_or("Error")
            ),
        ));
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ToolCall;

    #[test]
    fn structured_fs_error_from_stdout_survives_nonzero_exit() -> Result<()> {
        let adapter = CommandAdapters::new(Config::default());
        let error = adapter.command_json("censorfs", Command::new("/bin/sh").args([
            "-c", "printf '%s' '{\"ok\":false,\"code\":\"HeadChanged\",\"error\":\"current head moved\"}'; exit 1",
        ])).err().ok_or_else(|| PivotError::Protocol("expected failure".into()))?;
        assert!(
            error
                .to_string()
                .contains("HeadChanged: current head moved")
        );
        Ok(())
    }

    #[test]
    fn two_call_artifact_example_runs_with_real_tools() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let batch: BatchRequest =
            serde_json::from_str(include_str!("../examples/atomic-code-change.json"))?;
        for call in batch.calls {
            let result = run_local_call_in(
                &RunnerRequest {
                    session_id: batch.session_id.clone(),
                    call,
                    environment: BTreeMap::new(),
                },
                4096,
                workspace.path(),
            )?;
            assert!(result.succeeded(), "{}", result.stderr);
        }
        assert_eq!(
            fs::read_to_string(workspace.path().join("demo/pivot-result.txt"))?,
            "pivot demo transaction\n"
        );
        Ok(())
    }

    #[test]
    fn unsafe_group_and_root_view_fail_before_spawning() -> Result<()> {
        let adapter = CommandAdapters::new(Config::default());
        let mut batch: BatchRequest = serde_json::from_str(include_str!("../examples/batch.json"))?;
        batch.guard_group.clear();
        assert!(adapter.start(&batch, &FsResources::default()).is_err());
        batch.guard_group = "censorguard-dsh-default".into();
        let fs = FsResources {
            view_id: Some("view".into()),
            owner_uid: Some(0),
            owner_gid: Some(0),
            ..FsResources::default()
        };
        assert!(adapter.start(&batch, &fs).is_err());
        Ok(())
    }

    #[test]
    fn local_runner_applies_cwd_environment_and_output_limit() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        fs::create_dir(workspace.path().join("sub"))?;
        let request = RunnerRequest {
            session_id: "session-1".into(),
            call: ToolCall {
                id: "call-1".into(),
                program: "/bin/sh".into(),
                args: vec![
                    "-c".into(),
                    "printf '%s:%s:0123456789' \"$VALUE\" \"$DSH_CENSORSCOPE_CALL_ID\"".into(),
                ],
                cwd: "sub".into(),
                env: BTreeMap::from([("VALUE".into(), "pivot".into())]),
                timeout_ms: 1_000,
            },
            environment: BTreeMap::new(),
        };
        let result = run_local_call_in(&request, 20, workspace.path())?;
        assert!(result.succeeded());
        assert_eq!(result.stdout, "pivot:call-1:0123456");
        assert!(result.output_truncated);
        Ok(())
    }
    #[test]
    fn real_command_adapter_correlates_guard_registration_and_stops_on_restart() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        let config_path = root.join("guard.yaml");
        let launch_socket = root.join("launch.sock");
        fs::write(
            &config_path,
            format!(
                "daemon:\n  launch_socket: {}\nrules: []\n",
                launch_socket.display()
            ),
        )?;
        let status_path = root.join("status.json");
        let args_path = root.join("args.json");
        let calls_path = root.join("calls.jsonl");
        let control = root.join("ctl.py");
        fs::write(
            &control,
            format!(
                r#"#!/usr/bin/python3
import json, sys
from pathlib import Path
assert sys.argv[1:] == ['--config', {config_path:?}, 'status', '--json'], sys.argv
print(Path({status_path:?}).read_text())
"#
            ),
        )?;
        fs::set_permissions(&control, fs::Permissions::from_mode(0o755))?;
        let mounter = root.join("mounter.py");
        fs::write(
            &mounter,
            format!(
                r#"#!/usr/bin/python3
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
Path({args_path:?}).write_text(json.dumps(args))
assert args[args.index('--guard-socket') + 1] == {launch_socket:?}
assert args[args.index('--guard-domain') + 1] == 'pivot-fixture'
assert args[args.index('--guard-group') + 1] == 'censorguard-dsh-default'
assert 'censorguard-exec' not in ' '.join(args)
path = Path({status_path:?})
status = json.loads(path.read_text())
status['domains'] = [{{'name':'pivot-fixture','id':1,'slot':1,'group':'censorguard-dsh-default','policies':['censorguard-dsh-default'],'roots':1,'version':7}}]
status['trees'] = [{{'domain':'pivot-fixture','domain_id':1,'nodes':[{{'pid':os.getpid(),'ppid':os.getppid(),'comm':'runner','root':True}}]}}]
path.write_text(json.dumps(status))
print(json.dumps({{'kind':'ready','data':{{'host_pid':os.getpid()}}}}), flush=True)
for line in sys.stdin:
  request = json.loads(line)
  with Path({calls_path:?}).open('a') as stream: stream.write(line)
  print(json.dumps({{'kind':'result','data':{{'call_id':request['call']['id'],'exit_code':0,'timed_out':False,'duration_ms':0,'stdout':'ok','stderr':'','output_truncated':False}}}}), flush=True)
"#
            ),
        )?;
        fs::set_permissions(&mounter, fs::Permissions::from_mode(0o755))?;
        let initial = serde_json::json!({"ok":true,"daemon_ready":true,"hooks_healthy":true,"daemon_boot_id":"first", "config_path":config_path,
            "groups":[{"name":"censorguard-dsh-default","version":7,"definition":{"rules":[]}}]});
        fs::write(&status_path, serde_json::to_vec(&initial)?)?;
        let mut config = Config::default();
        config.censorguard.config = config_path;
        config.censorguard.control = control;
        config.censorfs.mounter_command = vec![mounter.to_string_lossy().into_owned()];
        let adapter = CommandAdapters::new(config);
        let mut batch: BatchRequest = serde_json::from_str(include_str!("../examples/batch.json"))?;
        batch.guard_scope = "pivot-fixture".into();
        batch.guard_group = "censorguard-dsh-default".into();
        let view = FsResources {
            view_id: Some("fixture-view".into()),
            owner_uid: Some(1000),
            owner_gid: Some(1000),
            ..FsResources::default()
        };
        let mut session = adapter.start(&batch, &view)?;
        let result = session.run(&batch.calls[0])?;
        assert!(result.succeeded());
        let mut status: Value = serde_json::from_slice(&fs::read(&status_path)?)?;
        status["daemon_boot_id"] = serde_json::json!("second");
        fs::write(&status_path, serde_json::to_vec(&status)?)?;
        assert!(session.run(&batch.calls[0]).is_err());
        assert!(session.finish().is_err());
        drop(session);
        assert_eq!(fs::read_to_string(&calls_path)?.lines().count(), 1);
        let args: Vec<String> = serde_json::from_slice(&fs::read(args_path)?)?;
        assert!(args.contains(&"__batch-runner".into()));
        assert!(args.contains(&"--cgroup-scope".into()));
        // An unhealthy daemon must be rejected before the mounter starts again.
        fs::remove_file(&calls_path)?;
        status["daemon_ready"] = serde_json::json!(false);
        fs::write(&status_path, serde_json::to_vec(&status)?)?;
        assert!(adapter.start(&batch, &view).is_err());
        assert!(!calls_path.exists());
        Ok(())
    }
}
