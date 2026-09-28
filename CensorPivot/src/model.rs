use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BatchRequest {
    pub request_id: String,
    pub session_id: String,
    #[serde(default)]
    pub trace_id: Option<u64>,
    pub branch: String,
    pub expected_generation: String,
    pub expected_head_seq: u64,
    pub guard_scope: String,
    #[serde(default)]
    pub guard_group: String,
    pub calls: Vec<ToolCall>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub id: String,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

const fn default_timeout_ms() -> u64 {
    60_000
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Commit,
    Abort,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionState {
    Received,
    Preparing,
    Prepared,
    CommitDecided,
    AbortDecided,
    Committed,
    Aborted,
}

impl TransactionState {
    pub const fn terminal(self) -> bool {
        matches!(self, Self::Committed | Self::Aborted)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct FsResources {
    #[serde(default)]
    pub open_attempted: bool,
    pub ticket_id: Option<String>,
    pub view_id: Option<String>,
    pub candidate_id: Option<String>,
    pub owner_uid: Option<u32>,
    pub owner_gid: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RequestSummary {
    pub session_id: String,
    pub trace_id: Option<u64>,
    pub branch: String,
    pub expected_generation: String,
    pub expected_head_seq: u64,
    pub guard_scope: String,
    pub guard_group: String,
    pub call_ids: Vec<String>,
}

impl From<&BatchRequest> for RequestSummary {
    fn from(request: &BatchRequest) -> Self {
        Self {
            session_id: request.session_id.clone(),
            trace_id: request.trace_id,
            branch: request.branch.clone(),
            expected_generation: request.expected_generation.clone(),
            expected_head_seq: request.expected_head_seq,
            guard_scope: request.guard_scope.clone(),
            guard_group: request.guard_group.clone(),
            call_ids: request.calls.iter().map(|call| call.id.clone()).collect(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CallResult {
    pub call_id: String,
    pub exit_code: i32,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub stdout: String,
    pub stderr: String,
    pub output_truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RunnerRequest {
    pub session_id: String,
    pub call: ToolCall,
    pub environment: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "data")]
pub enum RunnerReply {
    Ready { host_pid: u32 },
    Result(CallResult),
    Error(String),
}

impl CallResult {
    pub const fn succeeded(&self) -> bool {
        self.exit_code == 0 && !self.timed_out
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TransactionRecord {
    pub schema_version: u32,
    pub transaction_id: Uuid,
    pub request_id: String,
    pub request_fingerprint: String,
    pub actor_uid: u32,
    pub state: TransactionState,
    pub decision: Option<Decision>,
    pub summary: RequestSummary,
    pub fs: FsResources,
    pub open_request_id: Uuid,
    pub prepare_request_id: Uuid,
    pub finish_request_id: Uuid,
    pub results: Vec<CallResult>,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub last_error: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "op")]
pub enum ClientRequest {
    Execute { batch: BatchRequest },
    Status { transaction_id: Uuid },
    Recover,
    Doctor,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DoctorReport {
    pub ready: bool,
    pub checks: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "data")]
pub enum ClientReply {
    Transaction(Box<TransactionRecord>),
    Transactions(Vec<TransactionRecord>),
    Doctor(DoctorReport),
    Error { code: String, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn example_batch_matches_the_wire_schema() -> Result<(), Box<dyn std::error::Error>> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/batch.json");
        let batch: BatchRequest = serde_json::from_slice(&std::fs::read(path)?)?;
        assert_eq!(batch.calls.len(), 2);
        assert_eq!(batch.calls[0].id, "create-report");
        Ok(())
    }

    #[test]
    fn atomic_demo_matches_the_wire_schema() -> Result<(), Box<dyn std::error::Error>> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/atomic-code-change.json");
        let batch: BatchRequest = serde_json::from_slice(&std::fs::read(path)?)?;
        assert_eq!(batch.calls.len(), 2);
        assert_eq!(batch.calls[0].id, "generate-artifact");
        assert_eq!(batch.calls[1].id, "verify-artifact");
        Ok(())
    }
}
