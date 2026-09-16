use censorpivot::model::{BatchRequest, ClientReply, ClientRequest, ToolCall, TransactionState};
use censorpivot::server;
use censorpivot::{PivotError, Result};
use clap::Parser;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

const MAX_REPLY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "censorpivot-demo",
    about = "Run the atomic artifact generation demo against a live AgentCensor stack"
)]
struct Arguments {
    #[arg(long, default_value = "/run/censorpivot/control.sock")]
    pivot_socket: PathBuf,
    #[arg(long, default_value = "/run/censorfs/control.sock")]
    censorfs_socket: PathBuf,
    #[arg(long, default_value = "censorfs")]
    censorfs_command: PathBuf,
    #[arg(long, default_value = "main")]
    branch: String,
    #[arg(long, default_value = "censorguard-dsh-default")]
    guard_group: String,
}

fn main() {
    if let Err(error) = run(Arguments::parse()) {
        eprintln!("censorpivot-demo: {error}");
        std::process::exit(1);
    }
}

fn run(arguments: Arguments) -> Result<()> {
    require_full_stack(&arguments.pivot_socket)?;
    let before = branch_head(
        &arguments.censorfs_command,
        &arguments.censorfs_socket,
        &arguments.branch,
    )?;
    let demo_id = Uuid::new_v4();
    let batch = demo_batch(
        demo_id,
        arguments.branch.clone(),
        before.generation.clone(),
        before.sequence,
        arguments.guard_group,
    );
    let reply = server::request(
        &arguments.pivot_socket,
        &ClientRequest::Execute { batch },
        MAX_REPLY_BYTES,
    )?;
    println!("{}", serde_json::to_string_pretty(&reply)?);
    let record = match reply {
        ClientReply::Transaction(record) => record,
        ClientReply::Error { code, message } => {
            return Err(PivotError::component(
                "demo",
                format!("Pivot returned {code}: {message}"),
            ));
        }
        ClientReply::Transactions(_) | ClientReply::Doctor(_) => {
            return Err(PivotError::Protocol(
                "Pivot returned an unexpected reply to execute".into(),
            ));
        }
    };
    if record.state != TransactionState::Committed {
        return Err(PivotError::component(
            "demo",
            format!(
                "expected committed transaction, got {:?}: {}",
                record.state,
                record.last_error.as_deref().unwrap_or("no error reported")
            ),
        ));
    }
    let trace_id = record.summary.trace_id.ok_or_else(|| {
        PivotError::component(
            "demo",
            "transaction committed without a CensorScope trace; check Scope access or set it required",
        )
    })?;
    let after = branch_head(
        &arguments.censorfs_command,
        &arguments.censorfs_socket,
        &arguments.branch,
    )?;
    if after.generation == before.generation || after.sequence <= before.sequence {
        return Err(PivotError::component(
            "demo",
            "transaction committed but the CensorFS branch head did not advance",
        ));
    }
    println!(
        "demo committed: branch={} head_seq={}->{} trace_id={}",
        arguments.branch, before.sequence, after.sequence, trace_id
    );
    Ok(())
}

fn require_full_stack(socket: &Path) -> Result<()> {
    let reply = server::request(socket, &ClientRequest::Doctor, MAX_REPLY_BYTES)?;
    let report = match reply {
        ClientReply::Doctor(report) => report,
        ClientReply::Error { code, message } => {
            return Err(PivotError::component(
                "doctor",
                format!("Pivot returned {code}: {message}"),
            ));
        }
        ClientReply::Transaction(_) | ClientReply::Transactions(_) => {
            return Err(PivotError::Protocol(
                "Pivot returned an unexpected reply to doctor".into(),
            ));
        }
    };
    let failed = report
        .checks
        .iter()
        .filter(|(_, status)| status.as_str() != "ok")
        .map(|(name, status)| format!("{name}={status}"))
        .collect::<Vec<_>>();
    if failed.is_empty() {
        Ok(())
    } else {
        Err(PivotError::component(
            "doctor",
            format!("full stack is not ready: {}", failed.join(", ")),
        ))
    }
}

struct BranchHead {
    generation: String,
    sequence: u64,
}

fn branch_head(command: &Path, socket: &Path, branch: &str) -> Result<BranchHead> {
    let output = Command::new(command)
        .arg("--socket")
        .arg(socket)
        .arg("--json")
        .arg("head")
        .arg(branch)
        .output()
        .map_err(|error| PivotError::component("censorfs", error))?;
    if !output.status.success() {
        return Err(PivotError::component(
            "censorfs",
            String::from_utf8_lossy(&output.stderr).trim(),
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        PivotError::component("censorfs", format!("invalid head JSON: {error}"))
    })?;
    let generation = value
        .get("generation_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| PivotError::component("censorfs", "head is missing generation_id"))?;
    let sequence = value
        .get("head_seq")
        .and_then(Value::as_u64)
        .ok_or_else(|| PivotError::component("censorfs", "head is missing head_seq"))?;
    Ok(BranchHead {
        generation: generation.to_owned(),
        sequence,
    })
}

fn demo_batch(
    demo_id: Uuid,
    branch: String,
    expected_generation: String,
    expected_head_seq: u64,
    guard_group: String,
) -> BatchRequest {
    BatchRequest {
        request_id: format!("atomic-code-change-{demo_id}"),
        session_id: format!("pivot-demo-{demo_id}"),
        trace_id: None,
        branch,
        expected_generation,
        expected_head_seq,
        guard_scope: format!("pivot-demo-{demo_id}"),
        guard_group,
        calls: vec![
            ToolCall {
                id: "generate-artifact".into(),
                program: "/bin/sh".into(),
                args: vec![
                    "-c".into(),
                    "mkdir -p demo && printf 'pivot demo transaction\\n' > demo/pivot-result.txt"
                        .into(),
                ],
                cwd: ".".into(),
                env: BTreeMap::new(),
                timeout_ms: 5_000,
            },
            ToolCall {
                id: "verify-artifact".into(),
                program: "/bin/sh".into(),
                args: vec![
                    "-c".into(),
                    "test \"$(cat demo/pivot-result.txt)\" = 'pivot demo transaction'".into(),
                ],
                cwd: ".".into(),
                env: BTreeMap::new(),
                timeout_ms: 5_000,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_has_exactly_one_atomic_two_call_scenario() {
        let batch = demo_batch(
            Uuid::nil(),
            "main".into(),
            "generation".into(),
            7,
            "group".into(),
        );
        assert_eq!(batch.calls.len(), 2);
        assert_eq!(batch.calls[0].id, "generate-artifact");
        assert_eq!(batch.calls[1].id, "verify-artifact");
        assert!(batch.trace_id.is_none());
    }
}
