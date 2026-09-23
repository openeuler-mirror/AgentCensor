//! Persistence required by process tracking and lifecycle collection.

use std::time::SystemTime;

use model_core::diagnostics::CaptureDiagnostic;
use model_core::event::DomainEvent;
use model_core::ids::TraceId;
use model_core::payload::PayloadSegment;
use model_core::process::{ProcessMembership, ProcessRecord, SessionIdentity};
use model_core::trace::{TraceLifecycleState, TraceRecord};
use semantic_action_contract::{SemanticAction, SemanticActionLink, SemanticContent};

use crate::{BackfillJob, CallSpanRecord, StorageError};

/// One ancillary row produced while ingesting an event or a payload segment.
///
/// Rows are queued in the order they must be applied: an action precedes the
/// links that reference it, and a trace exists before its lifecycle moves.
/// They are collected per item rather than written one at a time because every
/// row carries its own commit cost once it reaches storage.
#[derive(Clone, Debug)]
pub enum AncillaryRow {
    ProcessRecord(ProcessRecord),
    Session {
        session_id: SessionIdentity,
        trace_id: TraceId,
        observed_at: SystemTime,
    },
    TraceRecord(TraceRecord),
    TraceLifecycle {
        trace_id: TraceId,
        state: TraceLifecycleState,
    },
    Membership(ProcessMembership),
    SemanticAction(SemanticAction),
    SemanticLink(SemanticActionLink),
    SemanticContent(SemanticContent),
    Diagnostic(CaptureDiagnostic),
}

/// Persistence boundary used by the daemon for process and trace state.
pub trait StorageBackend {
    /// Returns an unused trace ID seed based on persisted traces.
    fn next_trace_id_seed(&self) -> Result<u64, StorageError>;
    /// Returns an unused event ID seed based on persisted lifecycle events.
    fn next_event_id_seed(&self) -> Result<u64, StorageError>;
    /// Atomically reserves a half-open range of logical process IDs.
    fn reserve_process_id_block(&mut self, count: u64) -> Result<(u64, u64), StorageError>;
    /// Inserts or enriches the canonical coordinates for a process lifetime.
    fn upsert_process_record(&mut self, record: ProcessRecord) -> Result<(), StorageError>;
    /// Loads process identities needed to avoid reusing persisted logical IDs.
    fn list_process_records(&self) -> Result<Vec<ProcessRecord>, StorageError>;
    /// Persists metadata for a newly activated trace.
    fn create_trace(&mut self, trace: TraceRecord) -> Result<(), StorageError>;
    /// Loads one persisted trace record, if present. Used to continue an
    /// existing trace across daemon restarts via `track-add --trace-id`.
    fn read_trace(&self, trace_id: TraceId) -> Result<Option<TraceRecord>, StorageError>;
    /// Persists the latest lifecycle state of a trace.
    fn update_trace_lifecycle(
        &mut self,
        trace_id: TraceId,
        lifecycle_state: TraceLifecycleState,
    ) -> Result<(), StorageError>;
    /// Inserts or updates one process membership in a trace tree.
    fn upsert_membership(&mut self, membership: ProcessMembership) -> Result<(), StorageError>;
    /// Appends one normalized process lifecycle event.
    fn append_event(&mut self, event: DomainEvent) -> Result<(), StorageError>;
    fn append_events_batch(&mut self, events: &[DomainEvent]) -> Result<(), StorageError> {
        for event in events {
            self.append_event(event.clone())?;
        }
        Ok(())
    }
    /// Appends one retained payload segment without exposing internal codecs.
    fn append_payload(&mut self, segment: PayloadSegment) -> Result<(), StorageError>;
    fn append_payloads_batch(&mut self, segments: &[PayloadSegment]) -> Result<(), StorageError> {
        for segment in segments {
            self.append_payload(segment.clone())?;
        }
        Ok(())
    }
    /// Commits one decoded spool batch atomically. The consumer may advance
    /// its spool checkpoint only after this method succeeds.
    fn apply_ingest_batch(
        &mut self,
        sequence: u64,
        ancillary: Vec<AncillaryRow>,
        events: Vec<DomainEvent>,
        payloads: Vec<PayloadSegment>,
    ) -> Result<(), StorageError>;
    /// Records session directory metadata observed during event ingest.
    fn upsert_session(
        &mut self,
        session_id: &SessionIdentity,
        trace_id: TraceId,
        observed_at: SystemTime,
    ) -> Result<(), StorageError>;
    fn call_start(
        &mut self,
        trace_id: TraceId,
        session_id: Option<&str>,
        call_id: &str,
        host_pid: u32,
        started_at: SystemTime,
    ) -> Result<(), StorageError>;
    fn call_end(
        &mut self,
        trace_id: TraceId,
        session_id: Option<&str>,
        call_id: &str,
        host_pid: u32,
        ended_at: SystemTime,
        status: &str,
    ) -> Result<(), StorageError>;
    /// Closes one call span and enqueues its attribution backfill jobs in one
    /// transaction: an acknowledged close always carries its resumable jobs.
    /// Returns the assigned queue ids in job order.
    // Mirrors the `call_end` signature plus the jobs argument.
    #[allow(clippy::too_many_arguments)]
    fn call_end_with_jobs(
        &mut self,
        trace_id: TraceId,
        session_id: Option<&str>,
        call_id: &str,
        host_pid: u32,
        ended_at: SystemTime,
        status: &str,
        jobs: &[crate::BackfillJob],
    ) -> Result<Vec<i64>, StorageError>;
    /// Fetches stored events inside one call window that still carry no
    /// `call_id`.  Ingestion can attribute an event before a queued control
    /// call-start reaches the daemon (single-threaded stalls); this lets the
    /// daemon re-evaluate those rows once the span is known.  Returns
    /// `(event_id, host_pid, observed_at_ns)` — host_pid is `None` when no
    /// process record resolved.  Events with a NULL session match any window
    /// (mirror of the ingest-time matcher).
    fn unassigned_events_in_window(
        &self,
        trace_id: TraceId,
        session_id: Option<&str>,
        started_at_ns: i64,
        ended_at_ns: i64,
        limit: u64,
    ) -> Result<Vec<(i64, Option<u32>, i64)>, StorageError>;
    /// Backfills `call_id` onto previously stored events, skipping rows that
    /// were attributed in the meantime (UPDATE ... WHERE call_id IS NULL).
    fn assign_event_call_ids(&mut self, assignments: &[(i64, String)]) -> Result<(), StorageError>;
    /// Loads the currently committed call spans of one trace in start order.
    /// The writer thread uses this as the span set when it re-runs unique-window
    /// attribution for a backfill job, so jobs need no in-memory registry
    /// snapshot and stay resumable across daemon restarts.
    fn list_call_spans(&self, trace_id: TraceId) -> Result<Vec<CallSpanRecord>, StorageError>;
    /// Persists one attribution backfill job as `pending`. The daemon writes it
    /// in the same writer work item that closes the span so an acknowledged
    /// close is never left without its job.
    fn enqueue_backfill_job(&mut self, job: &BackfillJob) -> Result<i64, StorageError>;
    /// Claims up to `limit` pending jobs oldest-first, flipping each to
    /// `running` (a crash leaves a resumable row behind).
    fn claim_backfill_jobs(&mut self, limit: u64) -> Result<Vec<BackfillJob>, StorageError>;
    /// Lists the queue ids of pending jobs oldest-first without claiming them
    /// (startup scheduling for the paced executor).
    fn list_pending_backfill_job_ids(&self) -> Result<Vec<i64>, StorageError>;
    /// Claims one specific pending job (used once its bulk gate is satisfied),
    /// flipping it to `running`. None when it is no longer pending.
    fn claim_backfill_job_by_id(
        &mut self,
        queue_id: i64,
    ) -> Result<Option<BackfillJob>, StorageError>;
    /// Finishes one claimed job: no error -> `done`; error -> attempts + 1 and
    /// either requeued as `pending` or terminal `done` past the attempt budget.
    /// Returns the resulting state.
    fn finish_backfill_job(
        &mut self,
        queue_id: i64,
        error: Option<&str>,
    ) -> Result<crate::BackfillJobState, StorageError>;
    /// Startup recovery: resets any `running` rows left by a crash to
    /// `pending`. Returns the number of rows reset.
    fn reset_stale_backfill_jobs(&mut self) -> Result<usize, StorageError>;
    /// Persists an explicit collector or retention diagnostic.
    fn append_diagnostic(&mut self, diagnostic: CaptureDiagnostic) -> Result<(), StorageError>;
    /// Upserts one action and its evidence before any links reference it.
    fn upsert_semantic_action(&mut self, action: SemanticAction) -> Result<(), StorageError>;
    /// Upserts one trace-scoped lineage link after both actions exist.
    fn upsert_semantic_link(&mut self, link: SemanticActionLink) -> Result<(), StorageError>;
    /// Stores bounded structured content or a payload reference separately
    /// from raw bytes and action attributes.
    fn upsert_semantic_content(&mut self, content: SemanticContent) -> Result<(), StorageError>;
    /// Flushes backend state needed for orderly daemon shutdown.
    fn checkpoint(&mut self) -> Result<(), StorageError>;
    /// Applies one ordered batch of ancillary rows in a single transaction.
    ///
    /// A backend that can wrap the whole batch reports per-row failures and
    /// keeps the rows that succeeded: losing one malformed row must not discard
    /// the rest of the batch. The default applies rows one at a time.
    fn apply_ancillary_batch(
        &mut self,
        rows: Vec<AncillaryRow>,
    ) -> Result<Vec<String>, StorageError> {
        let mut failures = Vec::new();
        for row in rows {
            if let Err(error) = self.apply_ancillary_row(row) {
                failures.push(error.to_string());
            }
        }
        Ok(failures)
    }
    /// Applies a control-plane batch atomically.
    fn apply_control_batch(&mut self, rows: Vec<AncillaryRow>) -> Result<(), StorageError>;
    /// Applies one ancillary row.
    fn apply_ancillary_row(&mut self, row: AncillaryRow) -> Result<(), StorageError> {
        match row {
            AncillaryRow::ProcessRecord(record) => self.upsert_process_record(record),
            AncillaryRow::Session {
                session_id,
                trace_id,
                observed_at,
            } => self.upsert_session(&session_id, trace_id, observed_at),
            AncillaryRow::TraceRecord(trace) => self.create_trace(trace),
            AncillaryRow::TraceLifecycle { trace_id, state } => {
                self.update_trace_lifecycle(trace_id, state)
            }
            AncillaryRow::Membership(membership) => self.upsert_membership(membership),
            AncillaryRow::SemanticAction(action) => self.upsert_semantic_action(action),
            AncillaryRow::SemanticLink(link) => self.upsert_semantic_link(link),
            AncillaryRow::SemanticContent(content) => self.upsert_semantic_content(content),
            AncillaryRow::Diagnostic(diagnostic) => self.append_diagnostic(diagnostic),
        }
    }
    /// Checkpoint and truncate the WAL file (needs a reader-free moment).
    fn checkpoint_truncate(&mut self) -> Result<(), StorageError>;
}
