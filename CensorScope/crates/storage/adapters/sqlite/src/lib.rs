//! SQLite persistence for CensorScope process tracking.

pub mod backend;
pub mod config;
pub mod schema;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use model_core::diagnostics::{CaptureDiagnostic, DiagnosticKind, DiagnosticSeverity};
use model_core::event::DomainEvent;
use model_core::process::{
    HostProcessCoordinates, NamespaceIdentity, NamespaceProcessCoordinates, ProcessMembership,
    ProcessRecord, ProcessResolutionState,
};
use model_core::trace::{TraceLifecycleState, TraceRecord};
use rusqlite::{Connection, OptionalExtension, Row, params};
use semantic_action_contract::{SemanticAction, SemanticActionLink, SemanticContent};
use storage_core::{BackfillJob, BackfillJobKind, BackfillJobState, CallSpanRecord};

pub use config::{SQLITE_DEFAULT_BUSY_TIMEOUT_MS, SqliteStorageConfig};

// Reserve ID ranges at startup so a restart cannot reuse identifiers once
// old rows have been removed.
const TRACE_ID_RESERVATION: u64 = 1_000_000;
const EVENT_ID_RESERVATION: u64 = 1_000_000;

/// Requeue attempts before one attribution backfill job is marked terminal
/// `done` with `last_error`.
const BACKFILL_JOB_MAX_ATTEMPTS: u32 = 3;

/// SQLite implementation of the process tracking storage boundary.
#[derive(Clone)]
pub struct SqliteStorage {
    connection: Rc<RefCell<Connection>>,
}

impl SqliteStorage {
    /// Start or replace a Harness tool call span.
    pub fn call_start(
        &mut self,
        trace_id: model_core::ids::TraceId,
        session_id: Option<&str>,
        call_id: &str,
        host_pid: u32,
        started_at: SystemTime,
    ) -> Result<(), rusqlite::Error> {
        if call_id.len() > 128 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        self.connection.borrow_mut().execute(
            "INSERT OR REPLACE INTO call_spans(trace_id,session_id,call_id,host_pid,started_at,ended_at,status) VALUES (?1,?2,?3,?4,?5,NULL,NULL)",
            params![trace_id.get(), session_id, call_id, host_pid, encode_time(started_at)],
        )?;
        Ok(())
    }

    /// Close a Harness tool call span with a terminal status.
    pub fn call_end(
        &mut self,
        trace_id: model_core::ids::TraceId,
        session_id: Option<&str>,
        call_id: &str,
        host_pid: u32,
        ended_at: SystemTime,
        status: &str,
    ) -> Result<(), rusqlite::Error> {
        if call_id.len() > 128 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        self.connection.borrow_mut().execute(
            "UPDATE call_spans SET ended_at=?1,status=?2,session_id=COALESCE(session_id,?3) WHERE trace_id=?4 AND call_id=?5 AND host_pid=?6",
            params![encode_time(ended_at), status, session_id, trace_id.get(), call_id, host_pid],
        )?;
        Ok(())
    }

    /// Fetch events in one call window that still lack a `call_id`.
    pub fn unassigned_events_in_window(
        &self,
        trace_id: model_core::ids::TraceId,
        session_id: Option<&str>,
        started_at_ns: i64,
        ended_at_ns: i64,
        limit: u64,
    ) -> Result<Vec<(i64, Option<u32>, i64)>, rusqlite::Error> {
        let connection = self.connection.borrow();
        let mut statement = connection.prepare(
            "SELECT e.event_id, p.host_pid, e.observed_at
               FROM events e
               LEFT JOIN processes p ON p.process_id = e.process_id
              WHERE e.trace_id = ?1
                AND e.call_id IS NULL
                AND e.observed_at BETWEEN ?2 AND ?3
                AND (?4 IS NULL OR e.session_id IS NULL OR e.session_id = ?4)
              ORDER BY e.observed_at, e.event_id
              LIMIT ?5",
        )?;
        let rows = statement.query_map(
            params![
                trace_id.get(),
                started_at_ns,
                ended_at_ns,
                session_id,
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        rows.collect()
    }

    /// Backfill `call_id` onto stored events, never overwriting an assignment
    /// that arrived between the read and this write.
    pub fn assign_event_call_ids(
        &mut self,
        assignments: &[(i64, String)],
    ) -> Result<(), rusqlite::Error> {
        if assignments.is_empty() {
            return Ok(());
        }
        let mut connection = self.connection.borrow_mut();
        let transaction = connection.transaction()?;
        {
            let mut statement =
                transaction.prepare("UPDATE events SET call_id=?1 WHERE event_id=?2 AND call_id IS NULL")?;
            for (event_id, call_id) in assignments {
                statement.execute(params![call_id, event_id])?;
            }
        }
        transaction.commit()
    }

    /// Close one call span and enqueue its attribution backfill jobs in one
    /// transaction, so an acknowledged close is never left without its
    /// resumable job.
    // mirrors call_end's arguments plus the jobs list (intentional arity)
    #[allow(clippy::too_many_arguments)]
    pub fn call_end_with_jobs(
        &mut self,
        trace_id: model_core::ids::TraceId,
        session_id: Option<&str>,
        call_id: &str,
        host_pid: u32,
        ended_at: SystemTime,
        status: &str,
        jobs: &[BackfillJob],
    ) -> Result<Vec<i64>, rusqlite::Error> {
        if call_id.len() > 128 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        for job in jobs {
            if job.state != BackfillJobState::Pending {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        let mut connection = self.connection.borrow_mut();
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE call_spans SET ended_at=?1,status=?2,session_id=COALESCE(session_id,?3) WHERE trace_id=?4 AND call_id=?5 AND host_pid=?6",
            params![encode_time(ended_at), status, session_id, trace_id.get(), call_id, host_pid],
        )?;
        let mut ids = Vec::with_capacity(jobs.len());
        for job in jobs {
            transaction.execute(
                "INSERT INTO call_backfill_queue (
                    trace_id, session_id, call_id, kind, span_started_ns, span_ended_ns,
                    scope_from_ns, scope_to_ns, state, attempts, last_error,
                    enqueued_at_ns, updated_at_ns
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    job.trace_id.get(),
                    job.session_id,
                    job.call_id,
                    job.kind.as_storage_str(),
                    job.span_started_at.map(encode_time),
                    job.span_ended_at.map(encode_time),
                    encode_time(job.scope_from),
                    encode_time(job.scope_to),
                    BackfillJobState::Pending.as_storage_str(),
                    job.attempts,
                    job.last_error,
                    encode_time(job.enqueued_at),
                    encode_time(job.updated_at),
                ],
            )?;
            let id: i64 = transaction.query_row("SELECT last_insert_rowid()", [], |row| {
                row.get(0)
            })?;
            ids.push(id);
        }
        transaction.commit()?;
        Ok(ids)
    }

    /// Read the committed call spans of one trace in start order; the
    /// attribution matcher reads them from here, so queued jobs need no
    /// in-memory registry snapshot.
    pub fn list_call_spans(
        &self,
        trace_id: model_core::ids::TraceId,
    ) -> Result<Vec<CallSpanRecord>, rusqlite::Error> {
        let connection = self.connection.borrow();
        let mut statement = connection.prepare(
            "SELECT trace_id, session_id, call_id, host_pid, started_at, ended_at, status
             FROM call_spans WHERE trace_id = ?1
             ORDER BY started_at, call_id",
        )?;
        let rows = statement.query_map([trace_id.get()], |row| {
            Ok(CallSpanRecord {
                trace_id: model_core::ids::TraceId::new(row.get(0)?),
                session_id: row.get(1)?,
                call_id: row.get(2)?,
                host_pid: row.get(3)?,
                started_at: decode_time(row.get(4)?),
                ended_at: row.get::<_, Option<i64>>(5)?.map(decode_time),
                status: row.get(6)?,
            })
        })?;
        rows.collect()
    }

    /// Persist one attribution backfill job as `pending`, returning the
    /// assigned queue id.
    pub fn enqueue_backfill_job(
        &mut self,
        job: &BackfillJob,
    ) -> Result<i64, rusqlite::Error> {
        if job.state != BackfillJobState::Pending {
            return Err(rusqlite::Error::InvalidQuery);
        }
        self.connection.borrow_mut().execute(
            "INSERT INTO call_backfill_queue (
                trace_id, session_id, call_id, kind, span_started_ns, span_ended_ns,
                scope_from_ns, scope_to_ns, state, attempts, last_error,
                enqueued_at_ns, updated_at_ns
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                job.trace_id.get(),
                job.session_id,
                job.call_id,
                job.kind.as_storage_str(),
                job.span_started_at.map(encode_time),
                job.span_ended_at.map(encode_time),
                encode_time(job.scope_from),
                encode_time(job.scope_to),
                BackfillJobState::Pending.as_storage_str(),
                job.attempts,
                job.last_error,
                encode_time(job.enqueued_at),
                encode_time(job.updated_at),
            ],
        )?;
        Ok(self.connection.borrow().last_insert_rowid())
    }

    /// Claim one specific pending job (used once its bulk gate is satisfied),
    /// flipping it to `running`. Returns None when it is no longer pending.
    pub fn claim_backfill_job_by_id(
        &mut self,
        queue_id: i64,
    ) -> Result<Option<BackfillJob>, rusqlite::Error> {
        let mut connection = self.connection.borrow_mut();
        let transaction = connection.transaction()?;
        let job = {
            let mut statement = transaction.prepare(
                "SELECT queue_id, trace_id, session_id, call_id, kind,
                        span_started_ns, span_ended_ns, scope_from_ns, scope_to_ns,
                        state, attempts, last_error, enqueued_at_ns, updated_at_ns
                 FROM call_backfill_queue
                 WHERE queue_id = ?1 AND state = 'pending'
                 ORDER BY queue_id",
            )?;
            let mut rows = statement.query_map([queue_id], backfill_job_from_row)?;
            rows.next().transpose()?
        };
        let Some(job) = job else {
            return Ok(None);
        };
        transaction.execute(
            "UPDATE call_backfill_queue
                SET state = 'running', updated_at_ns = ?1
              WHERE queue_id = ?2 AND state = 'pending'",
            params![encode_time(SystemTime::now()), queue_id],
        )?;
        transaction.commit()?;
        Ok(Some(BackfillJob {
            state: BackfillJobState::Running,
            updated_at: SystemTime::now(),
            ..job
        }))
    }

    /// List the queue ids of all pending jobs oldest-first without claiming
    /// them; startup uses this to schedule crash-left jobs for the writer's
    /// paced executor, which claims each one by id later.
    pub fn list_pending_backfill_job_ids(&self) -> Result<Vec<i64>, rusqlite::Error> {
        let connection = self.connection.borrow();
        let mut statement = connection.prepare(
            "SELECT queue_id FROM call_backfill_queue
             WHERE state = 'pending'
             ORDER BY queue_id",
        )?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect()
    }

    /// Claim up to `limit` pending jobs oldest-first, flipping each to
    /// `running` inside one transaction so a crash leaves a resumable row.
    pub fn claim_backfill_jobs(
        &mut self,
        limit: u64,
    ) -> Result<Vec<BackfillJob>, rusqlite::Error> {
        let mut connection = self.connection.borrow_mut();
        let transaction = connection.transaction()?;
        let claimed_at = SystemTime::now();
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let jobs = {
            let mut statement = transaction.prepare(
                "SELECT queue_id, trace_id, session_id, call_id, kind,
                        span_started_ns, span_ended_ns, scope_from_ns, scope_to_ns,
                        state, attempts, last_error, enqueued_at_ns, updated_at_ns
                 FROM call_backfill_queue
                 WHERE state = 'pending'
                 ORDER BY queue_id
                 LIMIT ?1",
            )?;
            let rows = statement.query_map([limit], backfill_job_from_row)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for job in &jobs {
            transaction.execute(
                "UPDATE call_backfill_queue
                    SET state = 'running', updated_at_ns = ?1
                  WHERE queue_id = ?2 AND state = 'pending'",
                params![encode_time(claimed_at), job.queue_id],
            )?;
        }
        transaction.commit()?;
        Ok(jobs
            .into_iter()
            .map(|mut job| {
                job.state = BackfillJobState::Running;
                job.updated_at = claimed_at;
                job
            })
            .collect())
    }

    /// Finish one claimed job: success marks it `done`; failure increments
    /// attempts, requeueing as `pending` until the budget is exhausted and
    /// the job becomes terminal `done`. Returns the resulting state.
    pub fn finish_backfill_job(
        &mut self,
        queue_id: i64,
        error: Option<&str>,
    ) -> Result<BackfillJobState, rusqlite::Error> {
        let mut connection = self.connection.borrow_mut();
        let transaction = connection.transaction()?;
        let attempts: i64 = transaction
            .query_row(
                "SELECT attempts FROM call_backfill_queue
                  WHERE queue_id = ?1 AND state = 'running'",
                [queue_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(rusqlite::Error::InvalidQuery)?;
        let next_attempts = u32::try_from(attempts)
            .unwrap_or(u32::MAX)
            .saturating_add(1);
        let next_state = if error.is_none() || next_attempts >= BACKFILL_JOB_MAX_ATTEMPTS {
            BackfillJobState::Done
        } else {
            BackfillJobState::Pending
        };
        transaction.execute(
            "UPDATE call_backfill_queue
                SET state = ?1, attempts = ?2, last_error = ?3, updated_at_ns = ?4
              WHERE queue_id = ?5",
            params![
                next_state.as_storage_str(),
                next_attempts,
                error,
                encode_time(SystemTime::now()),
                queue_id
            ],
        )?;
        transaction.commit()?;
        Ok(next_state)
    }

    /// Startup recovery: reset any `running` rows left by a crash to `pending`
    /// so they are claimed and executed again. Returns the number reset.
    pub fn reset_stale_backfill_jobs(&mut self) -> Result<usize, rusqlite::Error> {
        self.connection
            .borrow_mut()
            .execute(
                "UPDATE call_backfill_queue
                    SET state = 'pending', updated_at_ns = ?1
                  WHERE state = 'running'",
                [encode_time(SystemTime::now())],
            )
    }

    /// Open or create a CensorScope SQLite database with WAL journaling enabled.
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        Self::open_with_busy_timeout(path, Duration::from_millis(SQLITE_DEFAULT_BUSY_TIMEOUT_MS))
    }

    /// Opens or creates a CensorScope database with a caller-selected lock wait timeout.
    pub fn open_with_busy_timeout(
        path: &Path,
        busy_timeout: Duration,
    ) -> Result<Self, rusqlite::Error> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(busy_timeout)?;
        let mode = connection.query_row("PRAGMA journal_mode = WAL", [], |row| {
            row.get::<_, String>(0)
        })?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        // Default `synchronous=FULL` fsyncs the WAL on every commit from
        // inside the single-threaded daemon loop; a stalled fsync once froze
        // the control plane. `NORMAL` fsyncs only at checkpoints, keeps crash
        // safety, and only risks the latest commits on power loss.
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        // 256 MB caps the -wal file (well above steady-state live frames) so
        // a successful checkpoint can actually truncate it.
        connection.pragma_update(None, "journal_size_limit", 256 * 1024 * 1024)?;
        // 4096 pages (~16 MB) auto-checkpoint: keeps live frames small and
        // readers fast without stalling the single writer (a huge WAL once
        // made readers time out); shrinking the -wal file on disk is left to
        // the daemon's quiet-period TRUNCATE checkpoint.
        connection.pragma_update(None, "wal_autocheckpoint", 4096)?;
        schema::initialize(&connection)?;
        Ok(Self {
            connection: Rc::new(RefCell::new(connection)),
        })
    }

    /// Open an in-memory database for tests and ephemeral daemon instances.
    pub fn open_in_memory() -> Result<Self, rusqlite::Error> {
        let connection = Connection::open_in_memory()?;
        schema::initialize(&connection)?;
        Ok(Self {
            connection: Rc::new(RefCell::new(connection)),
        })
    }

    /// Reserve the next trace identifier from persistent storage.
    pub fn next_trace_id_seed(&self) -> Result<u64, rusqlite::Error> {
        reserve_id_block(
            &self.connection,
            "trace_id_sequence",
            "next_trace_id",
            TRACE_ID_RESERVATION,
            "traces",
            "trace_id",
        )
    }

    /// Reserves the next event identifier from persistent storage.
    pub fn next_event_id_seed(&self) -> Result<u64, rusqlite::Error> {
        reserve_id_block(
            &self.connection,
            "event_id_sequence",
            "next_event_id",
            EVENT_ID_RESERVATION,
            "events",
            "event_id",
        )
    }

    /// Reserve a non-overlapping range of logical process identifiers.
    pub fn reserve_process_id_block(&mut self, count: u64) -> Result<(u64, u64), rusqlite::Error> {
        if count == 0 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let mut connection = self.connection.borrow_mut();
        let transaction = connection.transaction()?;
        let start = transaction.query_row(
            "SELECT next_process_id FROM process_id_sequence WHERE singleton = 1",
            [],
            |row| row.get::<_, u64>(0),
        )?;
        let end = start
            .checked_add(count)
            .ok_or(rusqlite::Error::InvalidQuery)?;
        transaction.execute(
            "UPDATE process_id_sequence SET next_process_id = ?1 WHERE singleton = 1",
            [end],
        )?;
        transaction.commit()?;
        Ok((start, end))
    }

    /// Insert or enrich a process record and its namespace aliases.
    pub fn upsert_process_record(&mut self, record: &ProcessRecord) -> Result<(), rusqlite::Error> {
        let mut connection = self.connection.borrow_mut();
        let transaction = connection.transaction()?;
        let host = record.host.as_ref();
        transaction.execute(
            "INSERT INTO processes (
                process_id, host_pid, host_task_id, host_start_ticks,
                host_start_boottime_ns, resolution_state, session_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(process_id) DO UPDATE SET
                host_pid = excluded.host_pid,
                host_task_id = excluded.host_task_id,
                host_start_ticks = excluded.host_start_ticks,
                host_start_boottime_ns = excluded.host_start_boottime_ns,
                resolution_state = excluded.resolution_state,
                session_id = excluded.session_id",
            params![
                record.identity.get(),
                host.map(|value| value.pid),
                host.and_then(|value| value.task_id),
                host.map(|value| value.start_time_ticks),
                host.and_then(|value| value.start_boottime_ns),
                resolution_state_name(record.resolution_state),
                record.session_id.as_ref().map(|value| value.as_str()),
            ],
        )?;
        for namespace in &record.namespaces {
            transaction.execute(
                "INSERT OR IGNORE INTO process_namespace_aliases (
                    process_id, pid_namespace, namespace_pid, namespace_start_ticks
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    record.identity.get(),
                    namespace.pid_namespace.as_str(),
                    namespace.pid,
                    namespace.start_time_ticks,
                ],
            )?;
        }
        transaction.commit()
    }

    /// Load all known process records used to preserve logical identities.
    pub fn list_process_records(&self) -> Result<Vec<ProcessRecord>, rusqlite::Error> {
        let connection = self.connection.borrow();
        let mut statement = connection.prepare(
            "SELECT process_id, host_pid, host_task_id, host_start_ticks,
                    host_start_boottime_ns, resolution_state, session_id
             FROM processes ORDER BY process_id",
        )?;
        let rows = statement.query_map([], process_record_from_row)?;
        let mut records = rows.collect::<Result<Vec<_>, _>>()?;
        for record in &mut records {
            let mut aliases = connection.prepare(
                "SELECT pid_namespace, namespace_pid, namespace_start_ticks
                 FROM process_namespace_aliases WHERE process_id = ?1",
            )?;
            record.namespaces = aliases
                .query_map([record.identity.get()], |row| {
                    Ok(NamespaceProcessCoordinates::new(
                        NamespaceIdentity::new(row.get::<_, String>(0)?),
                        row.get(1)?,
                        row.get(2)?,
                    ))
                })?
                .collect::<Result<_, _>>()?;
        }
        Ok(records)
    }

    /// Persist trace metadata at the moment it becomes active.
    pub fn create_trace(&mut self, trace: &TraceRecord) -> Result<(), rusqlite::Error> {
        self.connection.borrow_mut().execute(
            "INSERT OR REPLACE INTO traces (
                trace_id, root_process_id, root_pid_namespace, root_container_id,
                root_working_directory, display_name, profile_name, tags, lifecycle_state,
                health, created_at, started_at, completed_at, exited_at, failed_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                trace.trace_id.get(),
                trace.root_process_identity.get(),
                trace
                    .root_pid_namespace
                    .as_ref()
                    .map(|value| value.as_str()),
                trace.root_container_id,
                trace.root_working_directory,
                trace.display_name.to_string(),
                trace.profile_name.to_string(),
                encode_set(&trace.tags),
                trace.lifecycle_state.as_storage_str(),
                match trace.health {
                    model_core::trace::TraceHealth::Clean => "clean",
                    model_core::trace::TraceHealth::Degraded => "degraded",
                },
                encode_time(trace.timings.created_at),
                trace.timings.started_at.map(encode_time),
                trace.timings.completed_at.map(encode_time),
                trace.timings.exited_at.map(encode_time),
                trace.timings.failed_at.map(encode_time),
            ],
        )?;
        Ok(())
    }

    /// Persist a lifecycle transition for an active trace.
    pub fn update_trace_lifecycle(
        &mut self,
        trace_id: model_core::ids::TraceId,
        lifecycle: TraceLifecycleState,
    ) -> Result<(), rusqlite::Error> {
        self.connection.borrow_mut().execute(
            "UPDATE traces SET lifecycle_state = ?2 WHERE trace_id = ?1",
            params![trace_id.get(), lifecycle.as_storage_str()],
        )?;
        Ok(())
    }

    /// Load one persisted trace record, when present. Used to continue an
    /// existing trace across daemon restarts via `track-add --trace-id`.
    pub fn read_trace(
        &self,
        trace_id: model_core::ids::TraceId,
    ) -> Result<Option<TraceRecord>, rusqlite::Error> {
        let connection = self.connection.borrow();
        let mut statement = connection.prepare(
            "SELECT trace_id, root_process_id, root_pid_namespace, root_container_id,
                    root_working_directory, display_name, profile_name, tags,
                    lifecycle_state, health, created_at, started_at, completed_at,
                    exited_at, failed_at
             FROM traces WHERE trace_id = ?1",
        )?;
        let mut rows = statement.query_map([trace_id.get()], |row| {
            Ok(TraceRecord {
                trace_id: model_core::ids::TraceId::new(row.get(0)?),
                root_process_identity: model_core::process::ProcessIdentity::new(row.get(1)?),
                root_pid_namespace: row.get::<_, Option<String>>(2)?.map(NamespaceIdentity::new),
                root_container_id: row.get(3)?,
                root_working_directory: row.get(4)?,
                display_name: model_core::ids::TraceName::new(row.get::<_, String>(5)?),
                profile_name: model_core::ids::ProfileName::new(row.get::<_, String>(6)?),
                tags: decode_set(&row.get::<_, String>(7)?),
                lifecycle_state: parse_lifecycle(&row.get::<_, String>(8)?)?,
                health: parse_health(&row.get::<_, String>(9)?)?,
                timings: model_core::trace::TraceTiming {
                    created_at: decode_time(row.get(10)?),
                    started_at: row.get::<_, Option<i64>>(11)?.map(decode_time),
                    completed_at: row.get::<_, Option<i64>>(12)?.map(decode_time),
                    exited_at: row.get::<_, Option<i64>>(13)?.map(decode_time),
                    failed_at: row.get::<_, Option<i64>>(14)?.map(decode_time),
                },
            })
        })?;
        rows.next().transpose()
    }

    /// Upsert session directory metadata observed during event ingest.
    pub fn upsert_session(
        &self,
        session_id: &model_core::process::SessionIdentity,
        trace_id: model_core::ids::TraceId,
        observed_at: SystemTime,
    ) -> Result<(), rusqlite::Error> {
        self.connection.borrow_mut().execute(
            "INSERT INTO sessions (session_id, display_name, first_seen, last_seen, last_trace_id)
             VALUES (?1, NULL, ?2, ?2, ?3)
             ON CONFLICT(session_id) DO UPDATE SET
                last_seen = excluded.last_seen,
                last_trace_id = excluded.last_trace_id",
            params![
                session_id.as_str(),
                encode_time(observed_at),
                trace_id.get()
            ],
        )?;
        Ok(())
    }

    /// Persist one process-to-trace membership and its exit state.
    pub fn upsert_membership(
        &mut self,
        membership: &ProcessMembership,
    ) -> Result<(), rusqlite::Error> {
        self.connection.borrow_mut().execute(
            "INSERT OR REPLACE INTO memberships (
                trace_id, process_id, inherited_from_process_id, observed_at,
                capture_enabled, propagation_enabled, membership_state, exit_code,
                exit_observed_at, exit_observation_source
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                membership.trace_id.get(),
                membership.identity.get(),
                membership.inherited_from.map(|value| value.get()),
                membership.observed_at.map(encode_time),
                i64::from(membership.capture_enabled),
                i64::from(membership.propagation_enabled),
                membership_state_name(membership.state),
                membership.exit_status.as_ref().and_then(|value| value.code),
                membership
                    .exit_status
                    .as_ref()
                    .map(|value| encode_time(value.observed_at)),
                membership
                    .exit_status
                    .as_ref()
                    .and_then(|value| value.source)
                    .map(exit_source_name),
            ],
        )?;
        Ok(())
    }

    /// Append one normalized process lifecycle event.
    pub fn append_event(&mut self, event: &DomainEvent) -> Result<(), rusqlite::Error> {
        if event
            .envelope
            .call_id
            .as_ref()
            .is_some_and(|id| id.len() > 128)
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let mut parent = None;
        let mut executable = None;
        let mut argv = None;
        let mut argv_flags = 0;
        let mut path = None;
        let mut fd = None;
        let mut size = None;
        let mut endpoint = None;
        let mut direction = 0;
        let mut result = None;
        let mut channel = None;
        let mut stream = None;
        let mut protocol = None;
        let mut resource = None;
        let mut loss_reason = None;
        let mut dropped = None;
        let mut dropped_bytes = None;
        let (operation, metadata) = match &event.payload {
            model_core::event::EventPayload::Process(payload) => (payload.operation.clone(), {
                parent = payload.parent.map(|value| value.get());
                executable = payload.executable.clone();
                argv = payload.argv.as_ref().map(encode_argv);
                argv_flags = payload.argv.as_ref().map_or(0, |value| value.flags);
                encode_map(&payload.metadata)
            }),
            model_core::event::EventPayload::File(payload) => (payload.operation.clone(), {
                path = payload.path.clone();
                fd = payload.fd;
                size = payload.size;
                encode_map(&payload.metadata)
            }),
            model_core::event::EventPayload::Net(payload) => (payload.operation.clone(), {
                endpoint = payload.endpoint.clone();
                direction = payload.direction as i64;
                size = payload.length;
                result = payload.result;
                encode_map(&payload.metadata)
            }),
            model_core::event::EventPayload::Ipc(payload) => (payload.operation.clone(), {
                channel = payload.channel.clone();
                direction = payload.direction as i64;
                size = payload.length;
                encode_map(&payload.metadata)
            }),
            model_core::event::EventPayload::Stdio(payload) => ("stdio".to_string(), {
                stream = Some(payload.stream.clone());
                direction = payload.direction as i64;
                size = payload.length;
                encode_map(&payload.metadata)
            }),
            model_core::event::EventPayload::Application(payload) => ("application".to_string(), {
                protocol = Some(payload.protocol.clone());
                encode_map(&payload.metadata)
            }),
            model_core::event::EventPayload::Resource(payload) => ("resource".to_string(), {
                resource = Some(payload.resource.clone());
                encode_map(&payload.metadata)
            }),
            model_core::event::EventPayload::Control(payload) => {
                (payload.operation.clone(), encode_map(&payload.metadata))
            }
            model_core::event::EventPayload::Loss(payload) => ("loss".to_string(), {
                loss_reason = Some(payload.reason.clone());
                dropped = Some(payload.dropped);
                dropped_bytes = Some(payload.bytes);
                String::new()
            }),
        };
        self.connection.borrow_mut().execute(
            "INSERT OR REPLACE INTO events (
                event_id, trace_id, observed_at, process_id, collector, kind, flags,
                operation, parent_process_id, executable, argv, argv_flags, path, fd,
                size_bytes, endpoint, direction, result, channel, stream, protocol,
                resource, loss_reason, dropped, dropped_bytes, session_id, call_id, metadata
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                       ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28)",
            params![
                event.envelope.event_id.get(),
                event.envelope.trace_id.get(),
                encode_time(event.envelope.observed_at),
                event.envelope.process.get(),
                event.envelope.collector.to_string(),
                event.envelope.kind as i64,
                event.envelope.flags.bits(),
                operation,
                parent,
                executable,
                argv,
                argv_flags,
                path,
                fd,
                size,
                endpoint,
                direction,
                result,
                channel,
                stream,
                protocol,
                resource,
                loss_reason,
                dropped,
                dropped_bytes,
                event
                    .envelope
                    .session_id
                    .as_ref()
                    .map(|value| value.as_str()),
                event.envelope.call_id.as_deref(),
                metadata,
            ],
        )?;
        Ok(())
    }

    pub fn append_events_batch(&mut self, events: &[DomainEvent]) -> Result<(), rusqlite::Error> {
        self.connection
            .borrow_mut()
            .execute_batch("BEGIN IMMEDIATE")?;
        for event in events {
            if let Err(error) = self.append_event(event) {
                let _ = self.connection.borrow_mut().execute_batch("ROLLBACK");
                return Err(error);
            }
        }
        self.connection.borrow_mut().execute_batch("COMMIT")
    }

    /// Append one retained payload segment. The bytes column is nullable so
    /// metadata-only segments remain queryable without fabricated content.
    pub fn append_payload(
        &mut self,
        segment: &model_core::payload::PayloadSegment,
    ) -> Result<(), rusqlite::Error> {
        self.connection.borrow_mut().execute(
            "INSERT INTO payload_segments (
                trace_id, process_id, session_id, observed_at, source, content_state, direction,
                stream_key, sequence, operation_id, offset_bytes, completed,
                original_size, captured_size, library, symbol, protocol_hint,
                loss_reason, bytes, call_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            params![
                segment.trace_id.get(),
                segment.process.get(),
                segment.session_id.as_ref().map(|value| value.as_str()),
                encode_time(segment.observed_at),
                segment.source as i64,
                segment.content_state as i64,
                segment.direction as i64,
                segment.stream_key,
                segment.sequence,
                segment.operation_id,
                segment.offset,
                i64::from(segment.completed),
                segment.original_size,
                segment.captured_size,
                segment.library,
                segment.symbol,
                segment.protocol_hint,
                segment.loss_reason,
                segment.bytes,
                segment.call_id,
            ],
        )?;
        Ok(())
    }

    pub fn append_payloads_batch(
        &mut self,
        segments: &[model_core::payload::PayloadSegment],
    ) -> Result<(), rusqlite::Error> {
        self.connection
            .borrow_mut()
            .execute_batch("BEGIN IMMEDIATE")?;
        for segment in segments {
            if let Err(error) = self.append_payload(segment) {
                let _ = self.connection.borrow_mut().execute_batch("ROLLBACK");
                return Err(error);
            }
        }
        self.connection.borrow_mut().execute_batch("COMMIT")
    }

    /// Persist one explicit capture diagnostic. Diagnostics remain queryable
    /// without decoding event or payload blobs.
    pub fn append_diagnostic(
        &mut self,
        diagnostic: &CaptureDiagnostic,
    ) -> Result<(), rusqlite::Error> {
        self.connection.borrow_mut().execute(
            "INSERT INTO diagnostics (
                trace_id, observed_at, collector, kind, severity, message,
                dropped, dropped_bytes
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                diagnostic.trace_id.get(),
                encode_time(diagnostic.observed_at),
                diagnostic.collector.to_string(),
                diagnostic_kind_code(diagnostic.kind),
                diagnostic_severity_code(diagnostic.severity),
                diagnostic.message,
                diagnostic.dropped,
                diagnostic.dropped_bytes,
            ],
        )?;
        Ok(())
    }

    /// Upsert an action, attributes, and evidence in one transaction. The
    /// public columns are plain SQL values so read-only consumers need no
    /// internal codec knowledge.
    pub fn upsert_semantic_action(
        &mut self,
        action: &SemanticAction,
    ) -> Result<(), rusqlite::Error> {
        let mut connection = self.connection.borrow_mut();
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO semantic_actions (
                trace_id, action_id, kind, kind_name, title, start_time, end_time,
                process_id, status, completeness, confidence_millis, session_id,
                schema_version
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1)
             ON CONFLICT(trace_id, action_id) DO UPDATE SET
                kind = excluded.kind, kind_name = excluded.kind_name,
                title = excluded.title, start_time = excluded.start_time,
                end_time = excluded.end_time, process_id = excluded.process_id,
                status = excluded.status, completeness = excluded.completeness,
                confidence_millis = excluded.confidence_millis,
                session_id = excluded.session_id,
                schema_version = 1",
            params![
                action.trace_id.get(),
                action.action_id,
                action.kind as i64,
                action.kind.as_str(),
                action.title,
                encode_time(action.start_time),
                action.end_time.map(encode_time),
                action.process.get(),
                action.status as i64,
                action.completeness as i64,
                action.confidence_millis.map(i64::from),
                action.session_id.as_ref().map(|value| value.as_str()),
            ],
        )?;
        transaction.execute(
            "DELETE FROM semantic_action_attributes WHERE trace_id = ?1 AND action_id = ?2",
            params![action.trace_id.get(), action.action_id],
        )?;
        for (key, value) in &action.attributes {
            transaction.execute(
                "INSERT INTO semantic_action_attributes
                    (trace_id, action_id, attr_key, attr_value)
                 VALUES (?1, ?2, ?3, ?4)",
                params![action.trace_id.get(), action.action_id, key, value],
            )?;
        }
        transaction.execute(
            "DELETE FROM semantic_action_evidence WHERE trace_id = ?1 AND action_id = ?2",
            params![action.trace_id.get(), action.action_id],
        )?;
        for evidence in &action.evidence {
            transaction.execute(
                "INSERT INTO semantic_action_evidence
                    (trace_id, action_id, evidence_kind, evidence_id, evidence_role)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    action.trace_id.get(),
                    action.action_id,
                    evidence.kind as i64,
                    evidence.id,
                    evidence.role,
                ],
            )?;
        }
        transaction.commit()
    }

    /// Upsert a lineage link only when both endpoints exist in the same trace.
    pub fn upsert_semantic_link(
        &mut self,
        link: &SemanticActionLink,
    ) -> Result<(), rusqlite::Error> {
        let connection = self.connection.borrow();
        let exists = |action_id: &str| {
            connection.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM semantic_actions
                    WHERE trace_id = ?1 AND action_id = ?2
                )",
                params![link.trace_id.get(), action_id],
                |row| row.get::<_, i64>(0),
            )
        };
        if exists(&link.source_action_id)? == 0 || exists(&link.target_action_id)? == 0 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        connection.execute(
            "INSERT INTO semantic_action_links (
                trace_id, source_action_id, target_action_id, role, role_name,
                confidence, valid, schema_version
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)
             ON CONFLICT(trace_id, source_action_id, target_action_id, role)
             DO UPDATE SET role_name = excluded.role_name,
                confidence = excluded.confidence, valid = excluded.valid,
                schema_version = 1",
            params![
                link.trace_id.get(),
                link.source_action_id,
                link.target_action_id,
                link.role as i64,
                link.role.as_str(),
                link.confidence as i64,
                i64::from(link.valid),
            ],
        )?;
        Ok(())
    }

    /// Store a bounded structured-content summary or a reference to retained
    /// payload bytes. Large bodies stay in payload_segments and are never
    /// copied into action attributes or this table.
    pub fn upsert_semantic_content(
        &mut self,
        content: &SemanticContent,
    ) -> Result<(), rusqlite::Error> {
        self.connection.borrow_mut().execute(
            "INSERT INTO semantic_contents (
                trace_id, action_id, content_id, kind, kind_name, state,
                state_name, payload_reference, canonical_json, schema_version
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1)
             ON CONFLICT(trace_id, content_id) DO UPDATE SET
                action_id = excluded.action_id,
                kind = excluded.kind,
                kind_name = excluded.kind_name,
                state = excluded.state,
                state_name = excluded.state_name,
                payload_reference = excluded.payload_reference,
                canonical_json = excluded.canonical_json,
                schema_version = 1",
            params![
                content.trace_id.get(),
                content.action_id,
                content.content_id,
                content.kind as i64,
                content.kind.as_str(),
                content.state as i64,
                content.state.as_str(),
                content.payload_reference,
                content.canonical_json,
            ],
        )?;
        Ok(())
    }

    /// Flush pending WAL pages before daemon shutdown.
    pub fn checkpoint(&mut self) -> Result<(), rusqlite::Error> {
        self.connection
            .borrow_mut()
            .execute_batch("PRAGMA wal_checkpoint(PASSIVE);")
    }

    /// Checkpoint and truncate the WAL file. Requires a moment with no other
    /// readers (may block up to the busy timeout); used when the WAL has grown
    /// large so the file is actually shrunk on disk.
    pub fn checkpoint_truncate(&mut self) -> Result<(), rusqlite::Error> {
        self.connection
            .borrow_mut()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
    }
}

fn backfill_job_from_row(row: &Row<'_>) -> Result<BackfillJob, rusqlite::Error> {
    let kind_raw: String = row.get(4)?;
    let state_raw: String = row.get(9)?;
    Ok(BackfillJob {
        queue_id: row.get(0)?,
        trace_id: model_core::ids::TraceId::new(row.get(1)?),
        session_id: row.get(2)?,
        call_id: row.get(3)?,
        kind: BackfillJobKind::from_storage_str(&kind_raw).ok_or(rusqlite::Error::InvalidQuery)?,
        span_started_at: row.get::<_, Option<i64>>(5)?.map(decode_time),
        span_ended_at: row.get::<_, Option<i64>>(6)?.map(decode_time),
        scope_from: decode_time(row.get(7)?),
        scope_to: decode_time(row.get(8)?),
        state: BackfillJobState::from_storage_str(&state_raw)
            .ok_or(rusqlite::Error::InvalidQuery)?,
        attempts: row.get(10)?,
        last_error: row.get(11)?,
        enqueued_at: decode_time(row.get(12)?),
        updated_at: decode_time(row.get(13)?),
    })
}

fn process_record_from_row(row: &Row<'_>) -> Result<ProcessRecord, rusqlite::Error> {
    let host_pid = row.get::<_, Option<u32>>(1)?;
    Ok(ProcessRecord {
        identity: model_core::process::ProcessIdentity::new(row.get(0)?),
        host: host_pid.map(|pid| HostProcessCoordinates {
            pid,
            task_id: row.get(2).expect("host task id column"),
            start_time_ticks: row
                .get::<_, Option<u64>>(3)
                .expect("host start ticks column")
                .unwrap_or(0),
            start_boottime_ns: row.get(4).expect("host boot time column"),
        }),
        namespaces: BTreeSet::new(),
        resolution_state: match row.get::<_, String>(5)?.as_str() {
            "provisional" => ProcessResolutionState::Provisional,
            "resolved" => ProcessResolutionState::Resolved,
            "conflicted" => ProcessResolutionState::Conflicted,
            _ => return Err(rusqlite::Error::InvalidQuery),
        },
        session_id: row
            .get::<_, Option<String>>(6)?
            .map(model_core::process::SessionIdentity::new),
    })
}

fn resolution_state_name(value: ProcessResolutionState) -> &'static str {
    match value {
        ProcessResolutionState::Provisional => "provisional",
        ProcessResolutionState::Resolved => "resolved",
        ProcessResolutionState::Conflicted => "conflicted",
    }
}

fn membership_state_name(value: model_core::process::MembershipState) -> &'static str {
    match value {
        model_core::process::MembershipState::Starting => "starting",
        model_core::process::MembershipState::Active => "active",
        model_core::process::MembershipState::Exited => "exited",
        model_core::process::MembershipState::IdentityStale => "identity_stale",
    }
}

fn exit_source_name(value: model_core::process::ExitObservationSource) -> &'static str {
    match value {
        model_core::process::ExitObservationSource::Event => "event",
        model_core::process::ExitObservationSource::Reconciled => "reconciled",
    }
}

fn encode_time(value: SystemTime) -> i64 {
    i64::try_from(
        value
            .duration_since(UNIX_EPOCH)
            .expect("timestamp before unix epoch")
            .as_nanos(),
    )
    .expect("timestamp exceeds sqlite integer")
}

fn decode_time(value: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_nanos(u64::try_from(value).expect("timestamp underflow"))
}

fn parse_lifecycle(raw: &str) -> Result<TraceLifecycleState, rusqlite::Error> {
    TraceLifecycleState::from_storage_str(raw).ok_or(rusqlite::Error::InvalidQuery)
}

fn parse_health(raw: &str) -> Result<model_core::trace::TraceHealth, rusqlite::Error> {
    match raw {
        "clean" => Ok(model_core::trace::TraceHealth::Clean),
        "degraded" => Ok(model_core::trace::TraceHealth::Degraded),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn decode_set(raw: &str) -> BTreeSet<String> {
    raw.split('\n')
        .filter(|item| !item.is_empty())
        .map(unescape)
        .collect()
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('e') => out.push('='),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn diagnostic_kind_code(value: DiagnosticKind) -> i64 {
    value as i64
}

fn diagnostic_severity_code(value: DiagnosticSeverity) -> i64 {
    value as i64
}

fn encode_set(values: &BTreeSet<String>) -> String {
    values
        .iter()
        .map(|value| escape(value))
        .collect::<Vec<_>>()
        .join("\n")
}

fn encode_map(values: &BTreeMap<String, String>) -> String {
    values
        .iter()
        .map(|(key, value)| format!("{}={}", escape(key), escape(value)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn encode_argv(argv: &model_core::process::ArgvCapture) -> String {
    let encoded = argv
        .args
        .iter()
        .map(|arg| {
            arg.iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{}:{encoded}", argv.args.len())
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('=', "\\e")
}

fn reserve_id_block(
    connection: &Rc<RefCell<Connection>>,
    sequence_table: &str,
    sequence_column: &str,
    reservation: u64,
    table: &str,
    column: &str,
) -> Result<u64, rusqlite::Error> {
    let mut connection = connection.borrow_mut();
    let transaction = connection.transaction()?;
    let sequence = transaction.query_row(
        &format!("SELECT {sequence_column} FROM {sequence_table} WHERE singleton = 1"),
        [],
        |row| row.get::<_, u64>(0),
    )?;
    let persisted_next = transaction.query_row(
        &format!("SELECT COALESCE(MAX({column}), 0) + 1 FROM {table}"),
        [],
        |row| row.get::<_, u64>(0),
    )?;
    let start = sequence.max(persisted_next);
    let end = start
        .checked_add(reservation)
        .ok_or(rusqlite::Error::InvalidQuery)?;
    transaction.execute(
        &format!("UPDATE {sequence_table} SET {sequence_column} = ?1 WHERE singleton = 1"),
        [end],
    )?;
    transaction.commit()?;
    Ok(start)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::SystemTime;

    use model_core::diagnostics::{CaptureDiagnostic, DiagnosticKind, DiagnosticSeverity};
    use model_core::ids::TraceId;
    use model_core::payload::{
        PayloadContentState, PayloadDirection, PayloadSegment, PayloadSourceBoundary,
    };
    use model_core::process::{ArgvCapture, ProcessIdentity};
    use model_core::trace::TraceRecord;
    use semantic_action_contract::{
        SemanticAction, SemanticActionCompleteness, SemanticActionKind, SemanticActionLink,
        SemanticActionLinkConfidence, SemanticActionLinkRole, SemanticActionStatus,
        SemanticContent, SemanticContentKind, SemanticContentState, SemanticEvidence,
        SemanticEvidenceKind,
    };
    use storage_core::{BackfillJob, BackfillJobKind, BackfillJobState};

    #[test]
    fn new_schema_uses_unified_event_table() {
        let storage = super::SqliteStorage::open_in_memory().unwrap();
        let count = storage
            .connection
            .borrow()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'events'",
                [],
                |row| row.get::<_, u32>(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        let argv_columns: u32 = storage
            .connection
            .borrow()
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('events') WHERE name IN ('argv', 'argv_flags')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(argv_columns, 2);
        let version: i32 = storage
            .connection
            .borrow()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn id_seed_reserves_persistent_ranges_for_restart() {
        let storage = super::SqliteStorage::open_in_memory().unwrap();
        let first_trace = storage.next_trace_id_seed().unwrap();
        let second_trace = storage.next_trace_id_seed().unwrap();
        assert!(second_trace >= first_trace + super::TRACE_ID_RESERVATION);
        let first_event = storage.next_event_id_seed().unwrap();
        let second_event = storage.next_event_id_seed().unwrap();
        assert!(second_event >= first_event + super::EVENT_ID_RESERVATION);
    }

    #[test]
    fn id_reservation_survives_database_reopen() {
        let path =
            std::env::temp_dir().join(format!("censorscope-id-seed-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let first = {
            let storage = super::SqliteStorage::open(&path).unwrap();
            storage.next_event_id_seed().unwrap()
        };
        let second = {
            let storage = super::SqliteStorage::open(&path).unwrap();
            storage.next_event_id_seed().unwrap()
        };
        let _ = std::fs::remove_file(&path);
        assert!(second >= first + super::EVENT_ID_RESERVATION);
    }

    #[test]
    fn argv_encoding_preserves_empty_argument_and_count() {
        let encoded = super::encode_argv(&ArgvCapture {
            args: vec![Vec::new(), vec![0xff]],
            flags: ArgvCapture::PARTIAL,
        });
        assert_eq!(encoded, "2:,ff");
    }

    #[test]
    fn structured_content_is_separate_and_idempotent() {
        let mut storage = super::SqliteStorage::open_in_memory().unwrap();
        let content = SemanticContent {
            trace_id: TraceId::new(1),
            action_id: "a".to_string(),
            content_id: "content-1".to_string(),
            kind: SemanticContentKind::LlmJson,
            state: SemanticContentState::Partial,
            payload_reference: Some("segment:7".to_string()),
            canonical_json: Some(r#"{"model":"demo"}"#.to_string()),
        };
        storage.upsert_semantic_content(&content).unwrap();
        storage.upsert_semantic_content(&content).unwrap();
        let row: (String, String, Option<String>) = storage
            .connection
            .borrow()
            .query_row(
                "SELECT kind_name, state_name, payload_reference FROM semantic_contents_read",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "llm.json".to_string(),
                "partial".to_string(),
                Some("segment:7".to_string())
            )
        );
    }

    #[test]
    fn unified_events_preserve_file_and_network_fields() {
        use model_core::event::{
            DomainEvent, EventEnvelope, EventFlags, EventKind, EventPayload, FilePayload,
            NetPayload,
        };
        use model_core::ids::{CollectorName, EventId};

        let mut storage = super::SqliteStorage::open_in_memory().unwrap();
        let envelope = |id| EventEnvelope {
            event_id: EventId::new(id),
            trace_id: TraceId::new(1),
            observed_at: SystemTime::UNIX_EPOCH,
            process: ProcessIdentity::new(2),
            collector: CollectorName::new("test"),
            kind: EventKind::Unknown,
            session_id: None,
            call_id: None,
            flags: EventFlags::empty(),
        };
        storage
            .append_event(&DomainEvent::new(
                envelope(1),
                EventPayload::File(FilePayload {
                    operation: "write".to_string(),
                    path: Some("/tmp/x".to_string()),
                    fd: Some(4),
                    size: Some(9),
                    metadata: BTreeMap::new(),
                }),
            ))
            .unwrap();
        storage
            .append_event(&DomainEvent::new(
                envelope(2),
                EventPayload::Net(NetPayload {
                    operation: "connect".to_string(),
                    endpoint: Some("127.0.0.1:80".to_string()),
                    direction: PayloadDirection::Outbound,
                    length: Some(3),
                    result: Some(0),
                    metadata: BTreeMap::new(),
                }),
            ))
            .unwrap();
        let connection = storage.connection.borrow();
        let file: (String, i32, u64) = connection
            .query_row(
                "SELECT path, fd, size_bytes FROM events_read WHERE event_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(file, ("/tmp/x".to_string(), 4, 9));
        let net: (String, i64, i64) = connection
            .query_row(
                "SELECT endpoint, direction, result FROM events_read WHERE event_id = 2",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(net, ("127.0.0.1:80".to_string(), 2, 0));
    }

    #[test]
    fn call_spans_are_started_and_closed_with_status() {
        let mut storage = super::SqliteStorage::open_in_memory().unwrap();
        storage
            .call_start(
                TraceId::new(9),
                Some("sess"),
                "call-1",
                123,
                SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1),
            )
            .unwrap();
        storage
            .call_end(
                TraceId::new(9),
                Some("sess"),
                "call-1",
                123,
                SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(2),
                "success",
            )
            .unwrap();
        let row: (i64, i64, String) = storage
            .connection
            .borrow()
            .query_row(
                "SELECT started_at, ended_at, status FROM call_spans WHERE trace_id=9 AND call_id='call-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, (1_000_000_000, 2_000_000_000, "success".to_string()));
    }

    #[test]
    fn payload_segments_store_bytes_and_metadata_only_rows() {
        let mut storage = super::SqliteStorage::open_in_memory().unwrap();
        let base = PayloadSegment {
            trace_id: TraceId::new(1),
            process: ProcessIdentity::new(2),
            session_id: Some(model_core::process::SessionIdentity::new("sess-1")),
            call_id: None,
            observed_at: SystemTime::UNIX_EPOCH,
            source: PayloadSourceBoundary::Stdio,
            content_state: PayloadContentState::Complete,
            direction: PayloadDirection::Outbound,
            stream_key: Some("stream".into()),
            sequence: 1,
            operation_id: None,
            offset: None,
            completed: true,
            original_size: 3,
            captured_size: 3,
            library: None,
            symbol: None,
            protocol_hint: Some("http".into()),
            loss_reason: None,
            bytes: Some(b"abc".to_vec()),
        };
        storage.append_payload(&base).unwrap();
        let mut metadata = base.clone();
        metadata.sequence = 2;
        metadata.content_state = PayloadContentState::MetadataOnly;
        metadata.captured_size = 0;
        metadata.bytes = None;
        storage.append_payload(&metadata).unwrap();
        let connection = storage.connection.borrow();
        let (with_bytes, without_bytes): (u32, u32) = connection
            .query_row(
                "SELECT SUM(bytes IS NOT NULL), SUM(bytes IS NULL) FROM payload_segments",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((with_bytes, without_bytes), (1, 1));
        let stored_session: String = connection
            .query_row(
                "SELECT session_id FROM payload_segments WHERE sequence = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_session, "sess-1");
    }

    #[test]
    fn diagnostics_are_queryable_as_plain_metadata() {
        let mut storage = super::SqliteStorage::open_in_memory().unwrap();
        storage
            .append_diagnostic(&CaptureDiagnostic {
                trace_id: TraceId::new(7),
                observed_at: SystemTime::UNIX_EPOCH,
                collector: model_core::ids::CollectorName::new("test"),
                kind: DiagnosticKind::CaptureGap,
                severity: DiagnosticSeverity::Warning,
                message: "lost bytes".to_string(),
                dropped: 2,
                dropped_bytes: 9,
            })
            .unwrap();
        let row: (i64, String, i64) = storage
            .connection
            .borrow()
            .query_row(
                "SELECT kind, message, dropped_bytes FROM diagnostics WHERE trace_id = 7",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                DiagnosticKind::CaptureGap as i64,
                "lost bytes".to_string(),
                9
            )
        );
    }

    fn action(trace_id: TraceId, action_id: &str) -> SemanticAction {
        SemanticAction {
            action_id: action_id.to_string(),
            trace_id,
            kind: SemanticActionKind::HttpMessage,
            title: "HTTP".to_string(),
            start_time: SystemTime::UNIX_EPOCH,
            end_time: None,
            process: ProcessIdentity::new(3),
            status: SemanticActionStatus::Success,
            completeness: SemanticActionCompleteness::Complete,
            confidence_millis: Some(900),
            attributes: [("http.method".to_string(), "GET".to_string())]
                .into_iter()
                .collect(),
            evidence: vec![SemanticEvidence {
                kind: SemanticEvidenceKind::Event,
                id: 99,
                role: "source".to_string(),
            }],
            session_id: None,
        }
    }

    #[test]
    fn semantic_actions_upsert_and_links_are_trace_scoped() {
        let mut storage = super::SqliteStorage::open_in_memory().unwrap();
        let trace = TraceId::new(11);
        storage.upsert_semantic_action(&action(trace, "a")).unwrap();
        storage.upsert_semantic_action(&action(trace, "b")).unwrap();
        storage
            .upsert_semantic_link(&SemanticActionLink {
                trace_id: trace,
                source_action_id: "a".to_string(),
                target_action_id: "b".to_string(),
                role: SemanticActionLinkRole::LlmCallRequest,
                confidence: SemanticActionLinkConfidence::Observed,
                valid: true,
            })
            .unwrap();
        let connection = storage.connection.borrow();
        let counts: (u32, u32, u32, u32) = connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM semantic_actions WHERE trace_id = 11),
                    (SELECT COUNT(*) FROM semantic_action_attributes WHERE trace_id = 11),
                    (SELECT COUNT(*) FROM semantic_action_evidence WHERE trace_id = 11),
                    (SELECT COUNT(*) FROM semantic_action_links WHERE trace_id = 11)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(counts, (2, 2, 2, 1));
        let view_count: u32 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'view' AND name IN
                 ('semantic_actions_read', 'semantic_action_links_read',
                  'semantic_action_evidence_read')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(view_count, 3);
        drop(connection);
        assert!(
            storage
                .upsert_semantic_link(&SemanticActionLink {
                    trace_id: TraceId::new(12),
                    source_action_id: "a".to_string(),
                    target_action_id: "b".to_string(),
                    role: SemanticActionLinkRole::LlmCallRequest,
                    confidence: SemanticActionLinkConfidence::Derived,
                    valid: false,
                })
                .is_err()
        );
    }

    #[test]
    fn trace_round_trips_through_create_and_read() {
        let mut storage = super::SqliteStorage::open_in_memory().unwrap();
        let mut trace = TraceRecord::new(
            TraceId::new(21),
            ProcessIdentity::new(4),
            model_core::ids::TraceName::new("dsh-web"),
            model_core::ids::ProfileName::new("process-lifecycle"),
            SystemTime::UNIX_EPOCH,
        );
        trace.add_tag("session=demo");
        storage.create_trace(&trace).unwrap();

        let loaded = storage.read_trace(TraceId::new(21)).unwrap().unwrap();
        assert_eq!(loaded.trace_id, TraceId::new(21));
        assert_eq!(loaded.display_name.to_string(), "dsh-web");
        assert!(loaded.tags.contains("session=demo"));
        assert_eq!(loaded.timings.created_at, SystemTime::UNIX_EPOCH);

        // INSERT OR REPLACE allows continuing the same trace id with a new root.
        let mut continued = trace;
        continued.root_process_identity = ProcessIdentity::new(99);
        storage.create_trace(&continued).unwrap();
        let reloaded = storage.read_trace(TraceId::new(21)).unwrap().unwrap();
        assert_eq!(reloaded.root_process_identity, ProcessIdentity::new(99));

        assert!(storage.read_trace(TraceId::new(999)).unwrap().is_none());
    }

    #[test]
    fn session_directory_upserts_and_advances_last_seen() {
        let storage = super::SqliteStorage::open_in_memory().unwrap();
        let session = model_core::process::SessionIdentity::new("sess-1");
        storage
            .upsert_session(&session, TraceId::new(1), SystemTime::UNIX_EPOCH)
            .unwrap();
        storage
            .upsert_session(
                &session,
                TraceId::new(2),
                SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(5),
            )
            .unwrap();
        let row = storage
            .connection
            .borrow()
            .query_row(
                "SELECT last_trace_id, last_seen > first_seen FROM sessions WHERE session_id = ?1",
                [session.as_str()],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap();
        assert_eq!(row.0, 2);
        assert_eq!(row.1, 1);
    }

    #[test]
    fn window_backfill_selects_and_assigns_unassigned_events() {
        use model_core::event::{
            DomainEvent, EventEnvelope, EventFlags, EventKind, EventPayload, FilePayload,
        };
        use model_core::ids::{CollectorName, EventId};
        let mut storage = super::SqliteStorage::open_in_memory().unwrap();
        let base = std::time::UNIX_EPOCH + std::time::Duration::from_secs(10_000);
        let env = |trace: u64, id: u64, secs: u64, session: Option<&str>, call: Option<&str>| {
            EventEnvelope {
                event_id: EventId::new(id),
                trace_id: TraceId::new(trace),
                observed_at: base + std::time::Duration::from_secs(secs),
                process: ProcessIdentity::new(50),
                collector: CollectorName::new("test"),
                kind: EventKind::Unknown,
                session_id: session.map(model_core::process::SessionIdentity::new),
                call_id: call.map(str::to_string),
                flags: EventFlags::empty(),
            }
        };
        let file = |path: &str| {
            EventPayload::File(FilePayload {
                operation: "write".to_string(),
                path: Some(path.to_string()),
                fd: Some(3),
                size: Some(1),
                metadata: BTreeMap::new(),
            })
        };
        // In-window candidates: same session (id 1) and NULL session (id 2).
        storage
            .append_event(&DomainEvent::new(env(7, 1, 10, Some("sess-1"), None), file("/a")))
            .unwrap();
        storage
            .append_event(&DomainEvent::new(env(7, 2, 11, None, None), file("/b")))
            .unwrap();
        // Already attributed / outside window / other trace must not match.
        storage
            .append_event(&DomainEvent::new(env(7, 3, 12, Some("sess-1"), Some("call-x")), file("/c")))
            .unwrap();
        storage
            .append_event(&DomainEvent::new(env(7, 4, 13, Some("sess-1"), None), file("/d")))
            .unwrap();
        storage
            .append_event(&DomainEvent::new(env(8, 5, 10, Some("sess-1"), None), file("/e")))
            .unwrap();
        let ns = |secs: u64| i64::try_from((base + std::time::Duration::from_secs(secs)).duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()).unwrap();
        let candidates = storage
            .unassigned_events_in_window(TraceId::new(7), Some("sess-1"), ns(10), ns(12), 100)
            .unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].0, 1);
        assert_eq!(candidates[1].0, 2);
        storage
            .assign_event_call_ids(&[(1, "call-new".to_string()), (2, "call-new".to_string())])
            .unwrap();
        let read = |event_id: i64| {
            storage
                .connection
                .borrow()
                .query_row(
                    "SELECT call_id FROM events WHERE event_id = ?1",
                    [event_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .unwrap()
        };
        assert_eq!(read(1).as_deref(), Some("call-new"));
        assert_eq!(read(2).as_deref(), Some("call-new"));
        // Pre-assigned rows are never overwritten.
        assert_eq!(read(3).as_deref(), Some("call-x"));
        assert_eq!(read(4), None);
        assert_eq!(read(5), None);
    }

    fn backfill_job(trace: u64, kind: BackfillJobKind, from: u64, to: u64) -> BackfillJob {
        let base = std::time::UNIX_EPOCH + std::time::Duration::from_secs(10_000);
        let at = |secs: u64| base + std::time::Duration::from_secs(secs);
        BackfillJob {
            queue_id: 0,
            trace_id: TraceId::new(trace),
            session_id: Some("sess-1".to_string()),
            call_id: Some("call-1".to_string()),
            kind,
            span_started_at: Some(at(from)),
            span_ended_at: Some(at(to)),
            scope_from: at(from),
            scope_to: at(to),
            state: BackfillJobState::Pending,
            attempts: 0,
            last_error: None,
            enqueued_at: at(0),
            updated_at: at(0),
        }
    }

    #[test]
    fn backfill_queue_claims_finishes_and_requeues_within_attempt_budget() {
        let mut storage = super::SqliteStorage::open_in_memory().unwrap();
        let id = storage
            .enqueue_backfill_job(&backfill_job(3, BackfillJobKind::Window, 1, 2))
            .unwrap();
        let claimed = storage.claim_backfill_jobs(10).unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].queue_id, id);
        assert_eq!(claimed[0].state, BackfillJobState::Running);
        assert!(storage.claim_backfill_jobs(10).unwrap().is_empty());
        assert_eq!(
            storage.finish_backfill_job(id, None).unwrap(),
            BackfillJobState::Done
        );
        assert!(storage.claim_backfill_jobs(10).unwrap().is_empty());
        let second = storage
            .enqueue_backfill_job(&backfill_job(3, BackfillJobKind::Window, 3, 4))
            .unwrap();
        for attempt in 1..=3u32 {
            let claimed = storage.claim_backfill_jobs(10).unwrap();
            assert_eq!(claimed.len(), 1);
            assert_eq!(claimed[0].queue_id, second);
            let state = storage.finish_backfill_job(second, Some("boom")).unwrap();
            let expected = if attempt < 3 {
                BackfillJobState::Pending
            } else {
                BackfillJobState::Done
            };
            assert_eq!(state, expected, "attempt {attempt}");
        }
        assert!(storage.claim_backfill_jobs(10).unwrap().is_empty());
        let (attempts, last_error): (i64, Option<String>) = storage
            .connection
            .borrow()
            .query_row(
                "SELECT attempts, last_error FROM call_backfill_queue WHERE queue_id = ?1",
                [second],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((attempts, last_error.as_deref()), (3, Some("boom")));
    }

    #[test]
    fn boot_reset_returns_running_jobs_to_pending_in_queue_order() {
        let mut storage = super::SqliteStorage::open_in_memory().unwrap();
        let first = storage
            .enqueue_backfill_job(&backfill_job(4, BackfillJobKind::Window, 1, 2))
            .unwrap();
        let second = storage
            .enqueue_backfill_job(&backfill_job(4, BackfillJobKind::Sweep, 5, 9))
            .unwrap();
        assert_eq!(storage.claim_backfill_jobs(10).unwrap().len(), 2);
        // A crash left both rows `running`; startup resets them.
        assert_eq!(storage.reset_stale_backfill_jobs().unwrap(), 2);
        let reclaimed = storage.claim_backfill_jobs(10).unwrap();
        assert_eq!(
            reclaimed.iter().map(|job| job.queue_id).collect::<Vec<_>>(),
            vec![first, second]
        );
    }

    #[test]
    fn listed_call_spans_reflect_open_and_closed_rows_in_start_order() {
        let mut storage = super::SqliteStorage::open_in_memory().unwrap();
        let at = |secs: u64| SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        storage
            .call_start(TraceId::new(5), Some("s1"), "call-open", 100, at(5))
            .unwrap();
        storage
            .call_start(TraceId::new(5), Some("s1"), "call-b", 200, at(2))
            .unwrap();
        storage
            .call_end(TraceId::new(5), Some("s1"), "call-b", 200, at(3), "success")
            .unwrap();
        storage
            .call_start(TraceId::new(9), Some("s1"), "other-trace", 300, at(1))
            .unwrap();
        let spans = storage.list_call_spans(TraceId::new(5)).unwrap();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].call_id, "call-b");
        assert_eq!(spans[0].started_at, at(2));
        assert_eq!(spans[0].ended_at, Some(at(3)));
        assert_eq!(spans[0].status.as_deref(), Some("success"));
        assert_eq!(spans[1].call_id, "call-open");
        assert_eq!(spans[1].ended_at, None);
        assert_eq!(storage.list_call_spans(TraceId::new(9)).unwrap().len(), 1);
    }

    #[test]
    fn call_end_with_jobs_closes_span_and_enqueues_atomically() {
        let mut storage = super::SqliteStorage::open_in_memory().unwrap();
        let trace = TraceId::new(6);
        let at = |secs: u64| SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        storage
            .call_start(trace, Some("s1"), "call-1", 42, at(1))
            .unwrap();
        let job = backfill_job(6, BackfillJobKind::Window, 1, 2);
        let ids = storage
            .call_end_with_jobs(trace, Some("s1"), "call-1", 42, at(2), "success", &[job])
            .unwrap();
        assert_eq!(ids.len(), 1);
        assert!(ids[0] > 0);
        let (ended, status): (i64, String) = storage
            .connection
            .borrow()
            .query_row(
                "SELECT ended_at, status FROM call_spans WHERE trace_id=6 AND call_id='call-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((ended, status.as_str()), (2_000_000_000, "success"));
        let claimed = storage.claim_backfill_jobs(10).unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].kind, BackfillJobKind::Window);
        assert_eq!(
            claimed[0].scope_to,
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(10_002)
        );
        // Non-pending jobs are rejected so the transaction stays consistent.
        let mut stale = backfill_job(6, BackfillJobKind::Window, 3, 4);
        stale.state = BackfillJobState::Done;
        assert!(
            storage
                .call_end_with_jobs(trace, Some("s1"), "call-x", 7, at(3), "error", &[stale])
                .is_err()
        );
    }

    #[test]
    fn claim_backfill_job_by_id_claims_only_the_named_pending_row() {
        let mut storage = super::SqliteStorage::open_in_memory().unwrap();
        let first = storage
            .enqueue_backfill_job(&backfill_job(8, BackfillJobKind::Window, 1, 2))
            .unwrap();
        let second = storage
            .enqueue_backfill_job(&backfill_job(8, BackfillJobKind::Sweep, 5, 9))
            .unwrap();
        let claimed = storage.claim_backfill_job_by_id(second).unwrap().unwrap();
        assert_eq!(claimed.queue_id, second);
        assert_eq!(claimed.state, BackfillJobState::Running);
        assert!(storage.claim_backfill_job_by_id(second).unwrap().is_none());
        let remaining = storage.claim_backfill_job_by_id(first).unwrap().unwrap();
        assert_eq!(remaining.queue_id, first);
        assert_eq!(remaining.state, BackfillJobState::Running);
    }
}
