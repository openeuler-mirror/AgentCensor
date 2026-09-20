//! Storage facade implementation for SQLite.

use model_core::diagnostics::CaptureDiagnostic;
use model_core::event::DomainEvent;
use model_core::ids::TraceId;
use model_core::payload::PayloadSegment;
use model_core::process::{ProcessMembership, ProcessRecord};
use model_core::trace::{TraceLifecycleState, TraceRecord};
use semantic_action_contract::{SemanticAction, SemanticActionLink, SemanticContent};
use storage_core::{BackfillJob, BackfillJobState, CallSpanRecord, StorageBackend, StorageError};

use crate::SqliteStorage;

fn storage_error(stage: &'static str, error: rusqlite::Error) -> StorageError {
    StorageError::new(stage, error.to_string())
}

impl StorageBackend for SqliteStorage {
    fn next_trace_id_seed(&self) -> Result<u64, StorageError> {
        SqliteStorage::next_trace_id_seed(self)
            .map_err(|error| storage_error("trace_id_seed", error))
    }

    fn next_event_id_seed(&self) -> Result<u64, StorageError> {
        SqliteStorage::next_event_id_seed(self)
            .map_err(|error| storage_error("event_id_seed", error))
    }

    fn reserve_process_id_block(&mut self, count: u64) -> Result<(u64, u64), StorageError> {
        SqliteStorage::reserve_process_id_block(self, count)
            .map_err(|error| storage_error("reserve_process_id_block", error))
    }

    fn upsert_process_record(&mut self, record: ProcessRecord) -> Result<(), StorageError> {
        SqliteStorage::upsert_process_record(self, &record)
            .map_err(|error| storage_error("upsert_process_record", error))
    }

    fn list_process_records(&self) -> Result<Vec<ProcessRecord>, StorageError> {
        SqliteStorage::list_process_records(self)
            .map_err(|error| storage_error("list_process_records", error))
    }

    fn create_trace(&mut self, trace: TraceRecord) -> Result<(), StorageError> {
        SqliteStorage::create_trace(self, &trace)
            .map_err(|error| storage_error("create_trace", error))
    }

    fn read_trace(&self, trace_id: TraceId) -> Result<Option<TraceRecord>, StorageError> {
        SqliteStorage::read_trace(self, trace_id)
            .map_err(|error| storage_error("read_trace", error))
    }

    fn update_trace_lifecycle(
        &mut self,
        trace_id: TraceId,
        lifecycle_state: TraceLifecycleState,
    ) -> Result<(), StorageError> {
        SqliteStorage::update_trace_lifecycle(self, trace_id, lifecycle_state)
            .map_err(|error| storage_error("update_trace_lifecycle", error))
    }

    fn upsert_membership(&mut self, membership: ProcessMembership) -> Result<(), StorageError> {
        SqliteStorage::upsert_membership(self, &membership)
            .map_err(|error| storage_error("upsert_membership", error))
    }

    fn append_event(&mut self, event: DomainEvent) -> Result<(), StorageError> {
        SqliteStorage::append_event(self, &event)
            .map_err(|error| storage_error("append_event", error))
    }

    fn append_events_batch(&mut self, events: &[DomainEvent]) -> Result<(), StorageError> {
        SqliteStorage::append_events_batch(self, events)
            .map_err(|error| storage_error("append_events_batch", error))
    }

    fn append_payload(&mut self, segment: PayloadSegment) -> Result<(), StorageError> {
        SqliteStorage::append_payload(self, &segment)
            .map_err(|error| storage_error("append_payload", error))
    }

    fn append_payloads_batch(&mut self, segments: &[PayloadSegment]) -> Result<(), StorageError> {
        SqliteStorage::append_payloads_batch(self, segments)
            .map_err(|error| storage_error("append_payloads_batch", error))
    }

    fn apply_ingest_batch(
        &mut self,
        sequence: u64,
        ancillary: Vec<storage_core::AncillaryRow>,
        events: Vec<DomainEvent>,
        payloads: Vec<PayloadSegment>,
    ) -> Result<(), StorageError> {
        SqliteStorage::apply_ingest_batch(self, sequence, ancillary, events, payloads)
            .map_err(|error| storage_error("apply_ingest_batch", error))
    }

    fn upsert_session(
        &mut self,
        session_id: &model_core::process::SessionIdentity,
        trace_id: TraceId,
        observed_at: std::time::SystemTime,
    ) -> Result<(), StorageError> {
        SqliteStorage::upsert_session(self, session_id, trace_id, observed_at)
            .map_err(|error| storage_error("upsert_session", error))
    }
    fn call_start(
        &mut self,
        trace_id: TraceId,
        session_id: Option<&str>,
        call_id: &str,
        host_pid: u32,
        started_at: std::time::SystemTime,
    ) -> Result<(), StorageError> {
        SqliteStorage::call_start(self, trace_id, session_id, call_id, host_pid, started_at)
            .map_err(|e| storage_error("call_start", e))
    }
    fn call_end(
        &mut self,
        trace_id: TraceId,
        session_id: Option<&str>,
        call_id: &str,
        host_pid: u32,
        ended_at: std::time::SystemTime,
        status: &str,
    ) -> Result<(), StorageError> {
        SqliteStorage::call_end(
            self, trace_id, session_id, call_id, host_pid, ended_at, status,
        )
        .map_err(|e| storage_error("call_end", e))
    }
    fn call_end_with_jobs(
        &mut self,
        trace_id: TraceId,
        session_id: Option<&str>,
        call_id: &str,
        host_pid: u32,
        ended_at: std::time::SystemTime,
        status: &str,
        jobs: &[BackfillJob],
    ) -> Result<Vec<i64>, StorageError> {
        SqliteStorage::call_end_with_jobs(
            self, trace_id, session_id, call_id, host_pid, ended_at, status, jobs,
        )
        .map_err(|e| storage_error("call_end_with_jobs", e))
    }
    fn unassigned_events_in_window(
        &self,
        trace_id: TraceId,
        session_id: Option<&str>,
        started_at_ns: i64,
        ended_at_ns: i64,
        limit: u64,
    ) -> Result<Vec<(i64, Option<u32>, i64)>, StorageError> {
        SqliteStorage::unassigned_events_in_window(
            self,
            trace_id,
            session_id,
            started_at_ns,
            ended_at_ns,
            limit,
        )
        .map_err(|e| storage_error("unassigned_events_in_window", e))
    }
    fn assign_event_call_ids(&mut self, assignments: &[(i64, String)]) -> Result<(), StorageError> {
        SqliteStorage::assign_event_call_ids(self, assignments)
            .map_err(|e| storage_error("assign_event_call_ids", e))
    }
    fn list_call_spans(&self, trace_id: TraceId) -> Result<Vec<CallSpanRecord>, StorageError> {
        SqliteStorage::list_call_spans(self, trace_id)
            .map_err(|e| storage_error("list_call_spans", e))
    }
    fn enqueue_backfill_job(&mut self, job: &BackfillJob) -> Result<i64, StorageError> {
        SqliteStorage::enqueue_backfill_job(self, job)
            .map_err(|e| storage_error("enqueue_backfill_job", e))
    }
    fn claim_backfill_jobs(&mut self, limit: u64) -> Result<Vec<BackfillJob>, StorageError> {
        SqliteStorage::claim_backfill_jobs(self, limit)
            .map_err(|e| storage_error("claim_backfill_jobs", e))
    }
    fn list_pending_backfill_job_ids(&self) -> Result<Vec<i64>, StorageError> {
        SqliteStorage::list_pending_backfill_job_ids(self)
            .map_err(|e| storage_error("list_pending_backfill_job_ids", e))
    }
    fn claim_backfill_job_by_id(
        &mut self,
        queue_id: i64,
    ) -> Result<Option<BackfillJob>, StorageError> {
        SqliteStorage::claim_backfill_job_by_id(self, queue_id)
            .map_err(|e| storage_error("claim_backfill_job_by_id", e))
    }
    fn finish_backfill_job(
        &mut self,
        queue_id: i64,
        error: Option<&str>,
    ) -> Result<BackfillJobState, StorageError> {
        SqliteStorage::finish_backfill_job(self, queue_id, error)
            .map_err(|e| storage_error("finish_backfill_job", e))
    }
    fn reset_stale_backfill_jobs(&mut self) -> Result<usize, StorageError> {
        SqliteStorage::reset_stale_backfill_jobs(self)
            .map_err(|e| storage_error("reset_stale_backfill_jobs", e))
    }

    fn append_diagnostic(&mut self, diagnostic: CaptureDiagnostic) -> Result<(), StorageError> {
        SqliteStorage::append_diagnostic(self, &diagnostic)
            .map_err(|error| storage_error("append_diagnostic", error))
    }

    fn upsert_semantic_action(&mut self, action: SemanticAction) -> Result<(), StorageError> {
        SqliteStorage::upsert_semantic_action(self, &action)
            .map_err(|error| storage_error("upsert_semantic_action", error))
    }

    fn upsert_semantic_link(&mut self, link: SemanticActionLink) -> Result<(), StorageError> {
        SqliteStorage::upsert_semantic_link(self, &link)
            .map_err(|error| storage_error("upsert_semantic_link", error))
    }

    fn upsert_semantic_content(&mut self, content: SemanticContent) -> Result<(), StorageError> {
        SqliteStorage::upsert_semantic_content(self, &content)
            .map_err(|error| storage_error("upsert_semantic_content", error))
    }

    fn checkpoint(&mut self) -> Result<(), StorageError> {
        SqliteStorage::checkpoint(self).map_err(|error| storage_error("sqlite_checkpoint", error))
    }
    fn checkpoint_truncate(&mut self) -> Result<(), StorageError> {
        SqliteStorage::checkpoint_truncate(self)
            .map_err(|error| storage_error("sqlite_checkpoint_truncate", error))
    }

    /// Applies the batch in one transaction instead of one per row, which is
    /// what makes an event's ancillary rows cost a single commit.
    fn apply_ancillary_batch(
        &mut self,
        rows: Vec<storage_core::AncillaryRow>,
    ) -> Result<Vec<String>, StorageError> {
        SqliteStorage::apply_ancillary_batch(self, rows)
            .map_err(|error| storage_error("apply_ancillary_batch", error))
    }

    fn apply_control_batch(
        &mut self,
        rows: Vec<storage_core::AncillaryRow>,
    ) -> Result<(), StorageError> {
        SqliteStorage::apply_control_batch(self, rows)
            .map_err(|error| storage_error("apply_control_batch", error))
    }
}
