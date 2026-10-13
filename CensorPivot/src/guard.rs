//! Guard 0.5 configuration and private runner registration.
//! Register the actual runner PID before emitting Ready or accepting any tools.
use crate::{PivotError, Result};
use censorguard_common::config::DaemonConfig;
use censorguard_common::protocol::{
    RpcMethod, RpcParams, RpcRequest, RpcResponse, VERSION, read_json_line, write_json_line,
};
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

pub fn load_config(path: &Path) -> Result<DaemonConfig> {
    censorguard_common::config::load(path).map_err(|error| {
        PivotError::component("censorguard", format!("{}: {error}", path.display()))
    })
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name != "__base__"
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

pub fn register_runner(socket: &Path, domain: &str, group: &str, timeout: Duration) -> Result<()> {
    if !valid_name(domain)
        || !valid_name(group)
        || timeout.is_zero()
        || timeout > Duration::from_secs(300)
    {
        return Err(PivotError::Invalid(
            "invalid Guard domain, group, or registration timeout".into(),
        ));
    }
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let request = RpcRequest {
        version: VERSION,
        request_id: uuid::Uuid::new_v4().to_string(),
        method: RpcMethod::RegisterSelf,
        params: RpcParams {
            scope: Some(domain.into()),
            group: Some(group.into()),
            ..RpcParams::default()
        },
    };
    write_json_line(&mut stream, &request)
        .map_err(|error| PivotError::component("censorguard", error))?;
    let reply: RpcResponse = read_json_line(&mut BufReader::new(stream))
        .map_err(|error| PivotError::component("censorguard", error))?;
    if reply.version != VERSION || reply.request_id != request.request_id {
        return Err(PivotError::Protocol(
            "Guard registration reply version or request ID mismatch".into(),
        ));
    }
    if !reply.ok {
        return Err(PivotError::component(
            "censorguard",
            reply
                .error
                .map_or_else(|| "registration rejected".into(), |error| error.message),
        ));
    }
    let registered = reply
        .result
        .and_then(|result| result.response)
        .filter(|response| response.ok)
        .and_then(|response| response.domain)
        .ok_or_else(|| PivotError::Protocol("Guard registration missing domain receipt".into()))?;
    if registered.name != domain || !registered.policies.iter().any(|policy| policy == group) {
        return Err(PivotError::Protocol(
            "Guard registered a different domain or policy group".into(),
        ));
    }
    Ok(())
}

/// Query the public CLI explicitly in JSON mode, with a bounded deadline.
pub fn status(
    control: &Path,
    config: &Path,
    timeout: Duration,
) -> Result<censorguard_common::protocol::Response> {
    let output = crate::process::Process::spawn(
        std::process::Command::new(control)
            .arg("--config")
            .arg(config)
            .args(["status", "--json"]),
        16 * 1024 * 1024,
        false,
    )?
    .collect(timeout)?;
    if output.timed_out || output.truncated || !output.status.success() {
        return Err(PivotError::component(
            "censorguard",
            format!(
                "status failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    let response = serde_json::from_slice(&output.stdout)?;
    Ok(response)
}

pub fn require_ready(
    response: &censorguard_common::protocol::Response,
    config: &Path,
    groups: &[String],
) -> Result<()> {
    let canonical = config.canonicalize()?;
    if !response.ok
        || !response.daemon_ready
        || response.hooks_healthy != Some(true)
        || response.daemon_boot_id.is_empty()
        || response.config_path.as_deref() != canonical.to_str()
        || !groups.iter().all(|group| {
            response
                .groups
                .iter()
                .any(|published| published.name == *group)
        })
    {
        return Err(PivotError::component(
            "censorguard",
            "daemon is not ready, configuration differs, hooks are unhealthy, or a requested policy group is unpublished",
        ));
    }
    Ok(())
}

pub fn require_runner(
    response: &censorguard_common::protocol::Response,
    domain: &str,
    group: &str,
    pid: u32,
    boot: &str,
) -> Result<()> {
    if response.daemon_boot_id != boot
        || !response
            .domains
            .iter()
            .any(|info| info.name == domain && info.policies.iter().any(|policy| policy == group))
        || !response.trees.iter().any(|tree| {
            tree.domain == domain && tree.nodes.iter().any(|node| node.root && node.pid == pid)
        })
    {
        return Err(PivotError::component(
            "censorguard",
            "runner registration was lost or daemon restarted; aborting the batch",
        ));
    }
    Ok(())
}
