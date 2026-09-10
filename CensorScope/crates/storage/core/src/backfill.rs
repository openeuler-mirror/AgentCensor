//! Durable call-span and attribution-backfill job records.
//!
//! The daemon closes a call span and enqueues a window/sweep attribution job in
//! the same writer work item. Jobs are persisted here so a crash after the
//! control reply can never lose a pending closure: the writer thread claims
//! `pending` jobs, executes them against committed events, and marks them done,
//! while `running` rows left by a crash are reset to `pending` at daemon
//! startup. Attribution re-evaluates against the *currently committed* spans of
//! the trace (see [`CallSpanRecord`]) so jobs never carry an in-memory registry
//! snapshot and survive daemon restarts.

use std::time::SystemTime;

use model_core::ids::TraceId;

/// Stored call-span row as of job execution time. The writer thread reads the
/// committed spans of a trace and re-runs the same unique-window matching
/// ingest would have used live.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallSpanRecord {
    pub trace_id: TraceId,
    pub session_id: Option<String>,
    pub call_id: String,
    pub host_pid: u32,
    pub started_at: SystemTime,
    pub ended_at: Option<SystemTime>,
    pub status: Option<String>,
}

/// What a queued attribution job re-evaluates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackfillJobKind {
    /// One just-closed call window `[span_started_at, span_ended_at]`.
    Window,
    /// A session range re-swept after a late span close
    /// `[scope_from, scope_to]`.
    Sweep,
}

impl BackfillJobKind {
    pub fn as_storage_str(self) -> &'static str {
        match self {
            BackfillJobKind::Window => "window",
            BackfillJobKind::Sweep => "sweep",
        }
    }

    pub fn from_storage_str(value: &str) -> Option<Self> {
        match value {
            "window" => Some(BackfillJobKind::Window),
            "sweep" => Some(BackfillJobKind::Sweep),
            _ => None,
        }
    }
}

/// Claim lifecycle of one durable attribution job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackfillJobState {
    /// Inserted with the span close; ready to be claimed.
    Pending,
    /// Claimed by the writer thread and being executed. A `running` row left
    /// by a crash is reset to `pending` on daemon startup.
    Running,
    /// Executed successfully, or failed past the attempt budget.
    Done,
}

impl BackfillJobState {
    pub fn as_storage_str(self) -> &'static str {
        match self {
            BackfillJobState::Pending => "pending",
            BackfillJobState::Running => "running",
            BackfillJobState::Done => "done",
        }
    }

    pub fn from_storage_str(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(BackfillJobState::Pending),
            "running" => Some(BackfillJobState::Running),
            "done" => Some(BackfillJobState::Done),
            _ => None,
        }
    }
}

/// One durable attribution backfill job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackfillJob {
    /// Zero when enqueued; assigned by storage.
    pub queue_id: i64,
    pub trace_id: TraceId,
    pub session_id: Option<String>,
    /// The closing call for `Window` jobs; informational for `Sweep` jobs.
    pub call_id: Option<String>,
    pub kind: BackfillJobKind,
    /// Set for `Window` jobs (the span the close carried).
    pub span_started_at: Option<SystemTime>,
    pub span_ended_at: Option<SystemTime>,
    /// Attribution SELECT window for both kinds.
    pub scope_from: SystemTime,
    pub scope_to: SystemTime,
    pub state: BackfillJobState,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub enqueued_at: SystemTime,
    pub updated_at: SystemTime,
}
