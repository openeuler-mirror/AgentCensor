//! Active trace ownership, attach bootstrap, and lifecycle event handling.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant, SystemTime};

use collector_binding::TraceBindingRequest;
use collector_instance::{CollectorInstance, CollectorPollBatch};
use config_core::capture_profile::CaptureProfile;
use config_core::daemon::EbpfCollectorConfig;
use config_core::trace_snapshot::CaptureProfileSnapshot;
use control_contract::command::{ControlCommand, OperationStatusCommand, TrackAddCommand};
use control_contract::reply::{
    ControlError, ControlReply, DoctorReply, OperationState, TraceListItem, TrackAddReply,
};
use control_contract::selector::TraceSelector;
use ebpf_collector::EbpfCollector;
use ebpf_collector::procfs::{
    ProcfsIdentityReader, ProcfsTreeSnapshotter, read_container_identity, read_process_session,
    resolve_namespaced_pid,
};
use ingest_runtime::fd_lineage::FdLineage;
use ingest_runtime::retention::PayloadRetention;
use model_core::diagnostics::CaptureDiagnostic;
use model_core::ids::{EventId, RequestId, TraceId};
use model_core::process::{
    ExitObservationSource, ExitStatus, ProcessIdentity, ProcessObservation, SessionIdentity,
};
use model_core::trace::TraceLifecycleState;
use process_identity::{ProcessIdentityError, ProcessIdentityManager, ProcessIdentityReader};
use process_tree_snapshot_contract::snapshot::ProcessTreeSnapshotter;
use storage_core::{BackfillJob, BackfillJobKind, BackfillJobState, CallSpanRecord};
use storage_factory::{StorageConfig, open_storage_backend};
use trace_runtime::TraceRuntime;
use trace_runtime::commands::{RootRemovalRequest, TrackTraceRequest};
use uds_control_server::{ControlService, PeerCredentials};

use crate::attribution::unique_call_window_match;
use crate::collector_actor::{CollectorActorHandle, CollectorBatchEnvelope, SpoolBatchSink};
use crate::peer_identity::PeerIdentity;
use crate::writer::{
    BulkItem, CallCloseWrite, CallStartWrite, IngestBatchWrite, PersistWrite, WriterHandle,
    WriterParams,
};
use storage_spool::{RecordKind, SpoolConfig, SpoolConsumer};

const PROCESS_ID_BLOCK_SIZE: u64 = 65_536;

/// Batches one drain step applies before returning to the control plane.
const DRAIN_BATCH_LIMIT: usize = 8;
const MAX_BUFFERED_SPOOL_RECORDS: usize = 32;

/// Rows one queued bulk item carries at most.
///
/// A dropped item is lost whole, so this bounds the worst case loss of one
/// writer-queue overflow to this many rows.
const BULK_ITEM_MAX_ROWS: usize = 512;
const MAX_TRACK_OPERATIONS: usize = 1_024;

#[derive(Default)]
struct IngestCapture {
    ancillary: Vec<PersistWrite>,
    events: Vec<model_core::event::DomainEvent>,
    payloads: Vec<model_core::payload::PayloadSegment>,
}

struct PendingIngest {
    reply: Option<Receiver<Result<crate::writer::ControlValue, String>>>,
    retry: bool,
}

struct BufferedSpoolRecord {
    next: storage_spool::SpoolPosition,
    record: storage_spool::SpoolRecord,
    pending: Option<PendingIngest>,
    done: bool,
}

struct TrackAddOperation {
    command: TrackAddCommand,
    peer: Option<PeerIdentity>,
    state: OperationState,
    trace_id: Option<TraceId>,
    lifecycle_state: Option<model_core::trace::TraceLifecycleState>,
    error: Option<String>,
}

/// Where the session identity of one collected item came from.
///
/// The candidates are kept separate rather than collapsed into the winning
/// value so diagnostics can tell a session the kernel already knew from one
/// recovered out of the process environment, and both from one inherited from
/// the matching call span.
struct SessionResolution {
    kernel: Option<SessionIdentity>,
    procfs: Option<SessionIdentity>,
    from_call: Option<SessionIdentity>,
}

impl SessionResolution {
    /// The session recorded against the item, in order of authority.
    fn effective(&self) -> Option<SessionIdentity> {
        self.kernel
            .clone()
            .or_else(|| self.procfs.clone())
            .or_else(|| self.from_call.clone())
    }

    const fn source(&self) -> &'static str {
        if self.kernel.is_some() {
            "ebpf"
        } else if self.procfs.is_some() {
            "procfs"
        } else if self.from_call.is_some() {
            "call"
        } else {
            "none"
        }
    }
}

/// Owns all active traces for one daemon process and coordinates collection and storage.
///
/// The trace registry is deliberately not restored from SQLite after restart.
pub struct DaemonServiceHost {
    profile: CaptureProfile,
    runtime: TraceRuntime,
    collector: CollectorActorHandle,
    decoder: EbpfCollector,
    spool_consumer: Option<SpoolConsumer>,
    writer: WriterHandle,
    processes: ProcessIdentityManager,
    roots: BTreeMap<TraceId, u32>,
    active_trace_max: usize,
    next_event_id: u64,
    ebpf_enabled: bool,
    payload_retention: PayloadRetention,
    fd_lineage: FdLineage,
    /// Pipe/Unix-socket channel ids created by the trace root (the dsh host
    /// process). Their traffic with direct children (workers) is orchestration
    /// noise filtered at ingest (P2 self-noise).
    root_channels: BTreeMap<TraceId, BTreeSet<u64>>,
    collector_drop_totals: BTreeMap<String, u64>,
    collector_health_reported: bool,
    llm_exchange: semantic_action_runtime::LlmExchangeRuntime,
    mcp_payload_action_heads:
        BTreeMap<(TraceId, String), (String, semantic_action_contract::SemanticActionKind)>,
    /// Plaintext retained per trace and stream for protocol parsing. Raw chunks
    /// are always persisted independently; this state only feeds protocol
    /// parsers, is consumed as messages are recognized and is capped by
    /// `STREAM_PLAINTEXT_MAX_BYTES`.
    payload_reassembly: BTreeMap<(TraceId, String), StreamPlaintext>,
    payload_call_buffers: BTreeMap<(TraceId, String, u64), Vec<u8>>,
    command_heads: BTreeMap<(TraceId, u64), String>,
    session_env_name: String,
    call_spans: Vec<CallSpanRecord>,
    pending_events: Vec<model_core::event::DomainEvent>,
    pending_payloads: Vec<model_core::payload::PayloadSegment>,
    ingest_capture: Option<IngestCapture>,
    buffered_spool: VecDeque<BufferedSpoolRecord>,
    track_operations: BTreeMap<RequestId, TrackAddOperation>,
    /// Where captured data was lost, by stage.
    loss_ledger: LossLedger,
    /// Rows accepted for storage, reported alongside the ledger so its numbers
    /// come with a denominator.
    ingested: LossCounts,
    /// Lifecycle state last queued per trace, so a state that has already been
    /// handed to storage is not rewritten for every following event.
    trace_lifecycle_written: BTreeMap<TraceId, model_core::trace::TraceLifecycleState>,
    /// Host coordinates per process lifetime, keyed by host PID and the kernel
    /// generation stamped on its events. Reading them costs several `/proc`
    /// reads whose result cannot change while the process lives, so one lookup
    /// per generation replaces one per item.
    process_identities: BTreeMap<(u32, u64), ProcessObservation>,
    /// Session lookups per process lifetime, keyed like the identity cache and
    /// including the "no session variable" result. The environment the kernel
    /// exposes for a process is fixed when that process is executed, so a
    /// negative result is as reusable as a positive one.
    session_lookups: BTreeMap<(u32, u64), Option<SessionIdentity>>,
}

impl DaemonServiceHost {
    /// Assemble an empty runtime while retaining persisted process ID allocation state.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        storage_config: &StorageConfig,
        writer_config: config_core::daemon::WriterConfig,
        profile: CaptureProfile,
        ebpf_config: EbpfCollectorConfig,
        active_trace_max: u32,
        session_env_name: String,
    ) -> Result<Self, ControlError> {
        // Boot-time persistence runs on a temporary connection before the
        // writer thread owns the only read-write connection.
        let mut storage = open_storage_backend(storage_config).map_err(storage_error)?;
        let next_trace_id = storage.next_trace_id_seed().map_err(storage_error)?;
        let next_event_id = storage.next_event_id_seed().map_err(storage_error)?;
        let records = storage.list_process_records().map_err(storage_error)?;
        let (block_start, block_end) = storage
            .reserve_process_id_block(PROCESS_ID_BLOCK_SIZE)
            .map_err(storage_error)?;
        drop(storage);
        let processes =
            ProcessIdentityManager::with_reserved_block(block_start, block_end, records)
                .map_err(|error| control_error("process_identity", error))?;
        let descriptor = EbpfCollector::new(ebpf_config.clone()).descriptor().clone();
        let spool_path = storage_config.path().with_extension("spool");
        // Spool data is scoped to one daemon lifetime; stale records are not
        // replayed after restart.
        if spool_path.exists() {
            fs::remove_dir_all(&spool_path)
                .map_err(|error| control_error("spool_reset", error.to_string()))?;
        }
        let checkpoint_path = spool_path.join("database.checkpoint");
        let sink = SpoolBatchSink::open(
            &spool_path,
            SpoolConfig {
                max_bytes: writer_config.spool_max_bytes,
                ..SpoolConfig::default()
            },
        )
        .map_err(|error| control_error("spool_open", error))?;
        let collector = CollectorActorHandle::spawn(ebpf_config.clone(), sink)
            .map_err(|error| control_error("collector_actor", error))?;
        let decoder = EbpfCollector::new_decoder(
            ebpf_config.clone(),
            profile
                .capabilities
                .iter()
                .map(|request| request.capability),
        );
        let spool_consumer = SpoolConsumer::open(&spool_path, &checkpoint_path, DRAIN_BATCH_LIMIT)
            .map_err(|error| control_error("spool_consumer", error.to_string()))?;
        let writer = WriterHandle::spawn(
            storage_config,
            WriterParams {
                batch_items: usize::try_from(writer_config.batch_items).unwrap_or(512),
                idle: std::time::Duration::from_millis(writer_config.idle_timeout_ms),
                checkpoint_interval: std::time::Duration::from_secs(
                    writer_config.checkpoint_interval_secs,
                ),
                queue_cap_items: usize::try_from(writer_config.queue_cap_items).unwrap_or(0),
                queue_budget_bytes: usize::try_from(writer_config.queue_budget_bytes)
                    .unwrap_or(usize::MAX),
            },
        )
        .map_err(|error| control_error("writer_spawn", error))?;
        Ok(Self {
            profile,
            runtime: TraceRuntime::new(vec![descriptor], next_trace_id),
            collector,
            decoder,
            spool_consumer: Some(spool_consumer),
            writer,
            processes,
            roots: BTreeMap::new(),
            active_trace_max: usize::try_from(active_trace_max)
                .map_err(|error| ControlError::new("active_trace_max", error.to_string()))?,
            next_event_id,
            ebpf_enabled: ebpf_config.enabled,
            payload_retention: PayloadRetention::new(),
            fd_lineage: FdLineage::new(),
            root_channels: BTreeMap::new(),
            collector_drop_totals: BTreeMap::new(),
            collector_health_reported: false,
            llm_exchange: semantic_action_runtime::LlmExchangeRuntime::default(),
            mcp_payload_action_heads: BTreeMap::new(),
            payload_reassembly: BTreeMap::new(),
            payload_call_buffers: BTreeMap::new(),
            command_heads: BTreeMap::new(),
            session_env_name,
            call_spans: Vec::new(),
            pending_events: Vec::new(),
            pending_payloads: Vec::new(),
            ingest_capture: None,
            buffered_spool: VecDeque::new(),
            track_operations: BTreeMap::new(),
            loss_ledger: LossLedger::default(),
            ingested: LossCounts::default(),
            trace_lifecycle_written: BTreeMap::new(),
            process_identities: BTreeMap::new(),
            session_lookups: BTreeMap::new(),
        })
    }

    /// Hand one item's ancillary rows to the writer. Returns whether they were
    /// enqueued; a full queue drops them and the caller decides whether the work
    /// has to be redone.
    fn push_writes(&mut self, writes: Vec<PersistWrite>) -> bool {
        if writes.is_empty() {
            return true;
        }
        if let Some(capture) = self.ingest_capture.as_mut() {
            capture.ancillary.extend(writes);
            return true;
        }
        self.writer.push_bulk(BulkItem::Writes(writes))
    }

    fn queue_event(&mut self, event: model_core::event::DomainEvent) {
        if let Some(capture) = self.ingest_capture.as_mut() {
            capture.events.push(event);
        } else {
            self.pending_events.push(event);
        }
    }

    fn queue_payload(&mut self, payload: model_core::payload::PayloadSegment) {
        if let Some(capture) = self.ingest_capture.as_mut() {
            capture.payloads.push(payload);
        } else {
            self.pending_payloads.push(payload);
        }
    }

    fn assign_call_id(
        &self,
        trace_id: TraceId,
        host_pid: Option<u32>,
        session_id: &Option<SessionIdentity>,
        observed_at: SystemTime,
    ) -> Option<String> {
        // Fallback for events the caller left without a call id: match the
        // span's explicit parameters and interval. A child shell has a
        // different PID from the span-opening host, so PID is only a
        // disambiguator; the shared matcher keeps live ingest and queued
        // backfill from drifting apart.
        unique_call_window_match(
            &self.call_spans,
            trace_id,
            host_pid,
            session_id,
            observed_at,
        )
    }

    fn session_for_call(&self, trace_id: TraceId, call_id: &str) -> Option<SessionIdentity> {
        self.call_spans
            .iter()
            .find(|span| span.trace_id == trace_id && span.call_id == call_id)
            .and_then(|span| span.session_id.clone())
            .map(SessionIdentity::new)
    }

    /// Drain durable collector records without coupling the control socket to
    /// the collector transport.
    pub fn drain_live_events(&mut self) -> Result<(), ControlError> {
        self.progress_buffered_ingest()?;
        if !self
            .runtime
            .list_trace_records()
            .iter()
            .any(|trace| trace.lifecycle_state.is_active_or_draining())
        {
            return Ok(());
        }
        self.fill_spool_buffer()?;
        self.dispatch_buffered_spool()?;
        self.progress_buffered_ingest()?;
        self.record_collector_losses()?;
        self.absorb_writer_losses();
        self.report_loss_ledger(false);
        Ok(())
    }

    fn fill_spool_buffer(&mut self) -> Result<(), ControlError> {
        if self.buffered_spool.len() >= MAX_BUFFERED_SPOOL_RECORDS {
            return Ok(());
        }
        let mut consumer = self
            .spool_consumer
            .take()
            .ok_or_else(|| ControlError::new("spool_consumer", "consumer unavailable"))?;
        while self.buffered_spool.len() < MAX_BUFFERED_SPOOL_RECORDS {
            let record = match consumer.read_next() {
                Ok(record) => record,
                Err(error) => {
                    self.spool_consumer = Some(consumer);
                    return Err(ControlError::new("spool_read", error.to_string()));
                }
            };
            let Some(record) = record else {
                break;
            };
            self.buffered_spool.push_back(BufferedSpoolRecord {
                next: record.next,
                record,
                pending: None,
                done: false,
            });
        }
        self.spool_consumer = Some(consumer);
        Ok(())
    }

    fn dispatch_buffered_spool(&mut self) -> Result<(), ControlError> {
        let mut index = 0;
        while index < self.buffered_spool.len() {
            let mut buffered = self
                .buffered_spool
                .remove(index)
                .expect("buffered spool index exists");
            if !buffered.done && buffered.pending.is_none() {
                match self.apply_spool_record(&buffered.record) {
                    Ok(batch) => match self.writer.ingest_batch_async(batch) {
                        Ok(reply) => {
                            buffered.pending = Some(PendingIngest {
                                reply: Some(reply),
                                retry: false,
                            });
                        }
                        Err(error) => {
                            self.buffered_spool.insert(index, buffered);
                            return Err(control_error("ingest_queue", error));
                        }
                    },
                    Err(error) if error.code == "trace_not_ready" => {}
                    Err(error) => {
                        self.buffered_spool.insert(index, buffered);
                        return Err(error);
                    }
                }
            }
            self.buffered_spool.insert(index, buffered);
            index += 1;
        }
        Ok(())
    }

    fn progress_buffered_ingest(&mut self) -> Result<(), ControlError> {
        let mut index = 0;
        while index < self.buffered_spool.len() {
            let mut buffered = self
                .buffered_spool
                .remove(index)
                .expect("buffered spool index exists");
            if let Some(mut pending) = buffered.pending.take() {
                if pending.retry {
                    let batch = match self.apply_spool_record(&buffered.record) {
                        Ok(batch) => batch,
                        Err(error) => {
                            buffered.pending = Some(pending);
                            self.buffered_spool.insert(index, buffered);
                            return Err(error);
                        }
                    };
                    match self.writer.ingest_batch_async(batch) {
                        Ok(reply) => {
                            pending.reply = Some(reply);
                            pending.retry = false;
                        }
                        Err(error) => {
                            buffered.pending = Some(pending);
                            self.buffered_spool.insert(index, buffered);
                            return Err(control_error("ingest_queue", error));
                        }
                    }
                } else {
                    let reply = pending.reply.take().expect("in-flight ingest reply exists");
                    match reply.try_recv() {
                        Ok(Ok(crate::writer::ControlValue::Unit)) => buffered.done = true,
                        Ok(Ok(_)) => {
                            self.buffered_spool.insert(index, buffered);
                            return Err(ControlError::new(
                                "ingest_commit",
                                "unexpected writer reply",
                            ));
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(error = %error, "ingest commit failed; retrying spool record");
                            pending.retry = true;
                            buffered.pending = Some(pending);
                        }
                        Err(TryRecvError::Empty) => {
                            pending.reply = Some(reply);
                            buffered.pending = Some(pending);
                        }
                        Err(TryRecvError::Disconnected) => {
                            self.buffered_spool.insert(index, buffered);
                            return Err(ControlError::new(
                                "ingest_commit",
                                "writer reply channel closed",
                            ));
                        }
                    }
                }
            }
            self.buffered_spool.insert(index, buffered);
            index += 1;
        }

        let mut consumer = self
            .spool_consumer
            .take()
            .ok_or_else(|| ControlError::new("spool_consumer", "consumer unavailable"))?;
        while self
            .buffered_spool
            .front()
            .is_some_and(|record| record.done)
        {
            let record = self
                .buffered_spool
                .pop_front()
                .expect("completed spool record exists");
            if let Err(error) = consumer.checkpoint(record.next) {
                self.buffered_spool.push_front(record);
                self.spool_consumer = Some(consumer);
                return Err(ControlError::new("spool_checkpoint", error.to_string()));
            }
        }
        self.spool_consumer = Some(consumer);
        Ok(())
    }

    fn apply_spool_record(
        &mut self,
        record: &storage_spool::SpoolRecord,
    ) -> Result<IngestBatchWrite, ControlError> {
        if record.kind == RecordKind::RawEvent {
            let (raws, diagnostics) = Self::decode_raw_spool(&record.payload)?;
            let mut batch = self
                .decoder
                .decode_raw_events(raws)
                .map_err(|error| ControlError::new("spool_decode", error.message))?;
            batch.observations.splice(0..0, diagnostics);
            self.ingest_capture = Some(IngestCapture::default());
            let result = self.apply_collector_batch(record.sequence, batch);
            if result.is_err() {
                self.ingest_capture = None;
            }
            return result;
        }
        if record.kind != RecordKind::Event {
            return Err(ControlError::new(
                "spool_record",
                format!("unsupported record kind {:?}", record.kind),
            ));
        }
        let envelope: CollectorBatchEnvelope = serde_json::from_slice(&record.payload)
            .map_err(|error| ControlError::new("spool_decode", error.to_string()))?;
        if envelope.version != 1 {
            return Err(ControlError::new(
                "spool_version",
                format!("unsupported collector batch version {}", envelope.version),
            ));
        }
        self.ingest_capture = Some(IngestCapture::default());
        let result = self.apply_collector_batch(record.sequence, envelope.batch);
        if result.is_err() {
            self.ingest_capture = None;
        }
        result
    }

    fn decode_raw_spool(
        payload: &[u8],
    ) -> Result<(Vec<Vec<u8>>, Vec<collector_event::RawCollectorEvent>), ControlError> {
        let mut cursor = 0usize;
        let read_u32 = |payload: &[u8], cursor: &mut usize| -> Result<u32, ControlError> {
            let end = cursor
                .checked_add(4)
                .ok_or_else(|| ControlError::new("spool_decode", "length overflow"))?;
            let bytes = payload
                .get(*cursor..end)
                .ok_or_else(|| ControlError::new("spool_decode", "truncated raw record"))?;
            *cursor = end;
            Ok(u32::from_le_bytes(bytes.try_into().expect("u32 width")))
        };
        let count = usize::try_from(read_u32(payload, &mut cursor)?)
            .map_err(|_| ControlError::new("spool_decode", "event count overflow"))?;
        let mut events = Vec::with_capacity(count);
        for _ in 0..count {
            let len = usize::try_from(read_u32(payload, &mut cursor)?)
                .map_err(|_| ControlError::new("spool_decode", "event length overflow"))?;
            let end = cursor
                .checked_add(len)
                .ok_or_else(|| ControlError::new("spool_decode", "event length overflow"))?;
            events.push(
                payload
                    .get(cursor..end)
                    .ok_or_else(|| ControlError::new("spool_decode", "truncated raw event"))?
                    .to_vec(),
            );
            cursor = end;
        }
        let diagnostics_len = usize::try_from(read_u32(payload, &mut cursor)?)
            .map_err(|_| ControlError::new("spool_decode", "diagnostic length overflow"))?;
        let end = cursor
            .checked_add(diagnostics_len)
            .ok_or_else(|| ControlError::new("spool_decode", "diagnostic length overflow"))?;
        let diagnostics = if diagnostics_len == 0 {
            Vec::new()
        } else {
            serde_json::from_slice(
                payload
                    .get(cursor..end)
                    .ok_or_else(|| ControlError::new("spool_decode", "truncated diagnostics"))?,
            )
            .map_err(|error| ControlError::new("spool_decode", error.to_string()))?
        };
        Ok((events, diagnostics))
    }

    fn apply_collector_batch(
        &mut self,
        sequence: u64,
        batch: CollectorPollBatch,
    ) -> Result<IngestBatchWrite, ControlError> {
        let mut trace_ids = std::collections::BTreeSet::new();
        trace_ids.extend(
            batch
                .observations
                .iter()
                .filter_map(|event| event.envelope.trace_id),
        );
        trace_ids.extend(
            batch
                .payload_segments
                .iter()
                .filter_map(|segment| segment.envelope.trace_id),
        );
        if let Some(trace_id) = trace_ids
            .iter()
            .find(|trace_id| self.runtime.get_trace(**trace_id).is_none())
        {
            return Err(ControlError::new(
                "trace_not_ready",
                format!("trace {} has not been attached", trace_id.get()),
            ));
        }
        for event in batch.observations {
            self.apply_event(event)?;
        }
        for raw_segment in batch.payload_segments {
            self.apply_payload_segment(raw_segment)?;
        }
        let capture = self
            .ingest_capture
            .take()
            .ok_or_else(|| ControlError::new("spool_capture", "capture state missing"))?;
        Ok(IngestBatchWrite {
            sequence,
            ancillary: capture.ancillary,
            events: capture.events,
            payloads: capture.payloads,
        })
    }

    /// Mirror the writer's loss counters into the ledger.
    ///
    /// The counters are monotonic totals, so the ledger holds the numbers from
    /// the point they are read; a cycle that cannot publish them yet does not
    /// lose them.
    fn absorb_writer_losses(&mut self) {
        self.loss_ledger.set(
            "data.queue_overflow",
            self.writer.dropped_bulk_total(),
            self.writer.dropped_bulk_bytes_total(),
        );
        self.loss_ledger.set(
            "data.ancillary_row_failure",
            self.writer.ancillary_failures_total(),
            0,
        );
        let (events, payloads) = self.writer.dropped_row_totals();
        self.loss_ledger.set("data.event_row_failure", events, 0);
        self.loss_ledger
            .set("data.payload_row_failure", payloads, 0);
        self.loss_ledger.set(
            "attribution.backfill_gave_up",
            self.writer.backfill_gave_up_total(),
            0,
        );
    }

    /// Publish what each stage lost since the last report.
    ///
    /// Every stage is written as its own diagnostic so the database answers
    /// "which stage lost the most" with one grouped query, and logged so a run
    /// can be read without opening the database. A cycle with no active trace
    /// leaves the growth outstanding rather than dropping it.
    fn report_loss_ledger(&mut self, force: bool) {
        if !force && !self.loss_ledger.due(LOSS_LEDGER_INTERVAL) {
            return;
        }
        // Volume is reported on the same cadence so the loss numbers always come
        // with a denominator, and so a quiet capture still shows a heartbeat.
        tracing::info!(
            items_total = self.ingested.count,
            payload_bytes_total = self.ingested.bytes,
            "ingest volume"
        );
        let pending = self.loss_ledger.pending();
        if pending.is_empty() {
            self.loss_ledger.note_report(&pending);
            return;
        }
        let Some(trace_id) = self
            .runtime
            .list_trace_records()
            .into_iter()
            .find(|trace| trace.lifecycle_state.is_active_or_draining())
            .map(|trace| trace.trace_id)
        else {
            // Nothing owns the loss yet; keep it for the next interval.
            self.loss_ledger.note_report(&[]);
            return;
        };
        let observed_at = SystemTime::now();
        let mut writes = Vec::with_capacity(pending.len());
        for (stage, delta, total) in &pending {
            writes.push(PersistWrite::Diagnostic(CaptureDiagnostic::loss(
                trace_id,
                observed_at,
                model_core::ids::CollectorName::new("loss-ledger"),
                stage.clone(),
                delta.count,
                delta.bytes,
            )));
            tracing::info!(
                stage = %stage,
                lost = delta.count,
                lost_bytes = delta.bytes,
                total_lost = total.count,
                "loss ledger"
            );
        }
        if self.push_writes(writes) {
            self.loss_ledger.note_report(&pending);
        }
    }

    /// Hand the accumulated rows to the writer in bounded batches.
    ///
    /// A queued item is lost whole when the writer queue overflows, so the rows
    /// one item carries bound the worst case loss of a single overflow: without
    /// the bound a whole drain cycle's worth of plaintext travels as one item.
    fn flush_pending_storage(&mut self) {
        for batch in into_bounded_batches(std::mem::take(&mut self.pending_events)) {
            self.writer.push_bulk(BulkItem::Events(batch));
        }
        for batch in into_bounded_batches(std::mem::take(&mut self.pending_payloads)) {
            self.writer.push_bulk(BulkItem::Payloads(batch));
        }
        self.writer.push_bulk(BulkItem::EndOfDrain);
    }

    fn record_collector_losses(&mut self) -> Result<(), ControlError> {
        if !self.collector_health_reported
            && let Some(message) = self.collector.health()
        {
            self.collector_health_reported = true;
            self.loss_ledger.add("data.collector_unavailable", 1, 0);
            tracing::error!(message = %message, "collector actor unavailable");
        }
        let stats = self.collector.stats().map_err(collector_control_error)?;
        let losses = drop_counter_deltas(&mut self.collector_drop_totals, &stats.dropped);
        if losses.is_empty() {
            return Ok(());
        }
        let mut writes = Vec::new();
        let traces = self
            .runtime
            .list_trace_records()
            .into_iter()
            .filter(|trace| trace.lifecycle_state.is_active_or_draining())
            .map(|trace| (trace.trace_id, trace.root_process_identity))
            .collect::<Vec<_>>();
        for (reason, dropped) in losses {
            // The collector names the observation class it lost, which is what
            // separates missing plaintext from a missing process tree edge.
            let class = reason.strip_prefix("loss_").unwrap_or(&reason).to_string();
            self.loss_ledger
                .add(&format!("data.kernel.{class}"), dropped, 0);
            for (trace_id, process) in &traces {
                writes.push(PersistWrite::Diagnostic(CaptureDiagnostic::loss(
                    *trace_id,
                    stats.last_heartbeat_at,
                    stats.collector_name.clone(),
                    format!("collector_{reason}"),
                    dropped,
                    0,
                )));
                let event = model_core::event::DomainEvent::new(
                    model_core::event::EventEnvelope {
                        event_id: self.take_event_id()?,
                        trace_id: *trace_id,
                        observed_at: stats.last_heartbeat_at,
                        process: *process,
                        collector: stats.collector_name.clone(),
                        kind: model_core::event::EventKind::Loss,
                        flags: model_core::event::EventFlags::LOSS
                            .union(model_core::event::EventFlags::PARTIAL)
                            .union(model_core::event::EventFlags::CAPTURE_GAP),
                        session_id: None,
                        call_id: None,
                    },
                    model_core::event::EventPayload::Loss(model_core::event::LossPayload {
                        reason: format!("collector_{reason}"),
                        dropped,
                        bytes: 0,
                    }),
                );
                self.queue_event(event);
            }
        }
        self.push_writes(writes);
        Ok(())
    }

    fn apply_payload_segment(
        &mut self,
        mut raw: collector_event::RawPayloadSegment,
    ) -> Result<(), ControlError> {
        let trace_id = raw
            .envelope
            .trace_id
            .ok_or_else(|| ControlError::new("payload", "segment has no trace ID"))?;
        if self.runtime.get_trace(trace_id).is_none() {
            self.loss_ledger.add("data.unknown_trace", 1, 0);
            return Ok(());
        }
        let host_pid = raw.envelope.process.host.as_ref().map(|host| host.pid);
        let session_resolution = self.resolve_item_session(trace_id, host_pid, &mut raw.envelope);
        let observed = raw.envelope.process.clone();
        let observation = self.resolve_observation(observed);
        let (identity, mut record) = self.resolve_process(observation)?;
        let session = session_resolution.effective();
        record.session_id = session.clone();
        let mut writes = vec![PersistWrite::ProcessRecord(record.clone())];
        self.ingested.count = self.ingested.count.saturating_add(1);
        let segment = self
            .payload_retention
            .accept(ingest_runtime::normalize_payload(
                raw,
                ingest_runtime::IngestMatch {
                    trace_id,
                    process: identity,
                    parent: None,
                    session: session.clone(),
                },
            ));
        if let Some(session) = &session {
            writes.push(PersistWrite::Session {
                session_id: session.clone(),
                trace_id,
                observed_at: segment.observed_at,
            });
        }
        // Plaintext arrives in TLS records of at most a few KiB while the protocol
        // parsers need whole messages, so every stream keeps the bytes no parser has
        // consumed yet and hands them over as one view. Two properties keep that
        // cheap: the view is built only when the new bytes could complete a message,
        // and the bytes a parser has already seen are dropped, so the retained
        // window stays near one unfinished message instead of growing with the
        // connection's whole history.
        match segment.bytes.as_ref() {
            // Plaintext the kernel could not read is captured loss, and it is
            // reported here rather than only as a per-trace total at close.
            None => {
                self.loss_ledger
                    .add("data.payload_bytes_unavailable", 1, segment.original_size)
            }
            Some(bytes) => {
                self.ingested.bytes = self.ingested.bytes.saturating_add(bytes.len() as u64)
            }
        }
        if matches!(
            segment.content_state,
            model_core::payload::PayloadContentState::Loss
        ) {
            writes.push(PersistWrite::Diagnostic(CaptureDiagnostic::loss(
                trace_id,
                segment.observed_at,
                model_core::ids::CollectorName::new("payload"),
                "payload_loss".to_string(),
                1,
                segment.original_size.saturating_sub(segment.captured_size),
            )));
        }
        let mut parse_segment = segment.clone();
        let mut reassembly_report: Option<u64> = None;
        let (http_actions, sse_actions, llm_actions, mcp_actions, parser_diagnostics) = match (
            parse_segment.stream_key.clone(),
            parse_segment.bytes.clone(),
        ) {
            (Some(stream), Some(bytes)) => {
                let is_h2 = parse_segment.protocol_hint.as_deref().is_some_and(|hint| {
                    hint.eq_ignore_ascii_case("http2") || hint.eq_ignore_ascii_case("h2")
                });
                let call_key = (
                    trace_id,
                    stream.clone(),
                    parse_segment.operation_id.unwrap_or(parse_segment.sequence),
                );
                let call_complete = parse_segment.completed
                    && matches!(
                        parse_segment.content_state,
                        model_core::payload::PayloadContentState::Complete
                    );
                {
                    let call = self
                        .payload_call_buffers
                        .entry(call_key.clone())
                        .or_default();
                    let offset = usize::try_from(parse_segment.offset.unwrap_or(call.len() as u64))
                        .unwrap_or(call.len());
                    if call.len() < offset {
                        call.resize(offset, 0);
                    }
                    let end = offset.saturating_add(bytes.len());
                    if call.len() < end {
                        call.resize(end, 0);
                    }
                    call[offset..end].copy_from_slice(&bytes);
                }
                let mut completes = completes_message_hint(&bytes);
                let mut finished_call = None;
                if parse_segment.completed {
                    let call = self
                        .payload_call_buffers
                        .remove(&call_key)
                        .unwrap_or_default();
                    completes |= completes_message_hint(&call);
                    finished_call = Some(call);
                }
                let state = self
                    .payload_reassembly
                    .entry((trace_id, stream.clone()))
                    .or_default();
                let h2_output = finished_call.as_deref().filter(|_| is_h2).map(|call| {
                    let assembler = match parse_segment.direction {
                        model_core::payload::PayloadDirection::Inbound => &mut state.http2_inbound,
                        _ => &mut state.http2_outbound,
                    };
                    assembler.ingest(call, parse_segment.content_state)
                });
                if !is_h2 && let Some(call) = finished_call {
                    state.tail.extend_from_slice(&call);
                }
                if state.tail.len() * 2 < STREAM_PLAINTEXT_MAX_BYTES {
                    state.cap_reported = false;
                }

                let mut parsed = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
                if let Some(h2_output) = h2_output {
                    let http = h2_output
                        .messages
                        .iter()
                        .map(|message| {
                            semantic_action_runtime::project_http2_message(
                                &parse_segment,
                                message,
                                session.clone(),
                            )
                        })
                        .collect::<Vec<_>>();
                    let (llm, mut diagnostics) =
                        semantic_action_runtime::project_llm_http2_messages_with_diagnostics(
                            &parse_segment,
                            session.clone(),
                            h2_output.messages,
                        );
                    diagnostics.extend(h2_output.diagnostics.into_iter().map(|message| {
                        CaptureDiagnostic {
                            trace_id,
                            observed_at: parse_segment.observed_at,
                            collector: model_core::ids::CollectorName::new("http2-assembler"),
                            kind: model_core::diagnostics::DiagnosticKind::CaptureGap,
                            severity: model_core::diagnostics::DiagnosticSeverity::Warning,
                            dedupe_key: Some(format!(
                                "http2:{}:{}:{}:{}",
                                trace_id.get(),
                                stream,
                                parse_segment.sequence,
                                message
                            )),
                            message,
                            dropped: 0,
                            dropped_bytes: 0,
                        }
                    }));
                    parsed = (http, Vec::new(), llm, Vec::new(), diagnostics);
                }
                // Re-scanning the accumulated stream costs time proportional to its
                // length, so it is worth doing only when the bytes just added can end a
                // message: a record that cannot complete one leaves the parsers with
                // nothing new to find.
                if !is_h2
                    && (completes || parse_segment.completed || tail_ends_message(&state.tail))
                {
                    state.view.clear();
                    state.view.extend_from_slice(&state.tail);
                    // A call still in flight is not part of the retained tail yet.
                    if let Some(call) = self.payload_call_buffers.get(&call_key) {
                        state.view.extend_from_slice(call);
                    }
                    parse_segment.captured_size = state.view.len() as u64;
                    parse_segment.original_size =
                        parse_segment.original_size.max(parse_segment.captured_size);
                    parse_segment.content_state = if matches!(
                        segment.content_state,
                        model_core::payload::PayloadContentState::Loss
                    ) {
                        model_core::payload::PayloadContentState::Loss
                    } else if parse_segment.completed && call_complete {
                        model_core::payload::PayloadContentState::Complete
                    } else {
                        model_core::payload::PayloadContentState::Truncated
                    };
                    // The parsers read the view off the segment, so it is materialized
                    // here rather than for every record: this runs only when the bytes
                    // just added could complete a message.
                    parse_segment.bytes = Some(state.view.clone());
                    let http = semantic_action_runtime::project_http1_payload(
                        &parse_segment,
                        session.clone(),
                    );
                    let sse = semantic_action_runtime::project_sse_payload(
                        &parse_segment,
                        session.clone(),
                    );
                    let (llm, parser_diagnostics) =
                        semantic_action_runtime::project_llm_http_message_with_diagnostics(
                            &parse_segment,
                            session.clone(),
                        );
                    let mcp = semantic_action_runtime::project_mcp_payload(
                        &parse_segment,
                        session.clone(),
                    );
                    // A message the parsers recognized is consumed: keeping it would
                    // re-derive the same action for every later record of the stream.
                    let consumed = if http.is_some() {
                        semantic_action_runtime::extract_http1_message(&parse_segment)
                            .map(|message| message.message_len)
                            .or_else(|| {
                                semantic_action_runtime::extract_http2_message(&parse_segment)
                                    .map(|message| message.message_len)
                            })
                            .or_else(|| {
                                let messages =
                                    semantic_action_runtime::extract_http2_messages(&parse_segment);
                                (!messages.is_empty()
                                    && messages.iter().all(|message| message.complete))
                                .then_some(state.view.len())
                            })
                            .or_else(|| header_block_end(&state.view))
                    } else if !llm.is_empty() || !mcp.is_empty() {
                        // These parsers read the window as one document, so everything
                        // they saw was consumed -- except under a framing protocol,
                        // where only the first frame is read and the later frames must
                        // stay available.
                        let framed = parse_segment
                            .protocol_hint
                            .as_deref()
                            .is_some_and(|hint| hint.eq_ignore_ascii_case("websocket"));
                        if framed {
                            semantic_action_runtime::extract_websocket_message(&parse_segment)
                                .map(|message| message.message_len)
                        } else {
                            semantic_action_runtime::extract_http1_message(&parse_segment)
                                .map(|message| message.message_len)
                                .or(Some(state.view.len()))
                        }
                    } else if !sse.is_empty() {
                        last_event_boundary(&state.view)
                    } else {
                        None
                    };
                    if let Some(consumed) = consumed {
                        consume_window(
                            &mut state.tail,
                            self.payload_call_buffers.get_mut(&call_key),
                            consumed,
                        );
                    }
                    parsed = (
                        http.into_iter().collect(),
                        sse,
                        llm,
                        mcp,
                        parser_diagnostics,
                    );
                }

                // Bytes past the cap are dropped from the parse window only: the
                // captured records are persisted independently, so nothing stored is
                // shortened here, and the loss of derived actions is reported.
                if state.tail.len() > STREAM_PLAINTEXT_MAX_BYTES {
                    let dropped = state.tail.len() - STREAM_PLAINTEXT_MAX_BYTES;
                    state.tail.drain(..dropped);
                    if !state.cap_reported {
                        state.cap_reported = true;
                        // Only derived protocol actions are lost here: the
                        // captured records stay stored in full.
                        self.loss_ledger
                            .add("derived.parse_window_truncated", 1, dropped as u64);
                        reassembly_report = Some(dropped as u64);
                    }
                }
                parsed
            }
            _ => (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        };
        writes.extend(parser_diagnostics.into_iter().map(PersistWrite::Diagnostic));
        if let Some(dropped_bytes) = reassembly_report {
            writes.push(PersistWrite::Diagnostic(CaptureDiagnostic::loss(
                trace_id,
                segment.observed_at,
                model_core::ids::CollectorName::new("payload-reassembly"),
                "reassembly_truncated".to_string(),
                1,
                dropped_bytes,
            )));
        }
        let stream_key = segment
            .stream_key
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let segment_reference = format!("payload:{}:{}", stream_key, segment.sequence);
        self.queue_payload(segment);
        let mut actions = Vec::new();
        actions.extend(http_actions);
        actions.extend(sse_actions);
        actions.extend(llm_actions);
        actions.extend(mcp_actions);
        let mut links = Vec::new();
        let mut sse_stream_id = None;
        for action in &actions {
            match action.kind {
                semantic_action_contract::SemanticActionKind::SseStream => {
                    sse_stream_id = Some(action.action_id.clone());
                }
                semantic_action_contract::SemanticActionKind::SseEvent => {
                    if let Some(source) = &sse_stream_id {
                        links.push(semantic_action_contract::SemanticActionLink {
                            trace_id: action.trace_id,
                            source_action_id: source.clone(),
                            target_action_id: action.action_id.clone(),
                            role: semantic_action_contract::SemanticActionLinkRole::SseStreamEvent,
                            confidence:
                                semantic_action_contract::SemanticActionLinkConfidence::Observed,
                            valid: true,
                        });
                    }
                }
                semantic_action_contract::SemanticActionKind::McpRequest
                | semantic_action_contract::SemanticActionKind::McpToolCall => {
                    self.mcp_payload_action_heads.insert(
                        (action.trace_id, stream_key.clone()),
                        (action.action_id.clone(), action.kind),
                    );
                }
                semantic_action_contract::SemanticActionKind::McpResponse => {
                    if let Some((source, source_kind)) = self
                        .mcp_payload_action_heads
                        .get(&(action.trace_id, stream_key.clone()))
                    {
                        let role = if *source_kind
                            == semantic_action_contract::SemanticActionKind::McpToolCall
                        {
                            semantic_action_contract::SemanticActionLinkRole::McpToolCallResponse
                        } else {
                            semantic_action_contract::SemanticActionLinkRole::McpResponseStdin
                        };
                        links.push(semantic_action_contract::SemanticActionLink {
                            trace_id: action.trace_id,
                            source_action_id: source.clone(),
                            target_action_id: action.action_id.clone(),
                            role,
                            confidence:
                                semantic_action_contract::SemanticActionLinkConfidence::Derived,
                            valid: true,
                        });
                    }
                }
                _ => {}
            }
        }
        let llm_actions = actions
            .iter()
            .filter(|action| {
                matches!(
                    action.kind,
                    semantic_action_contract::SemanticActionKind::LlmRequest
                        | semantic_action_contract::SemanticActionKind::LlmResponse
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        let llm_output = self.llm_exchange.observe(llm_actions);
        writes.extend(
            llm_output
                .diagnostics
                .iter()
                .cloned()
                .map(PersistWrite::Diagnostic),
        );
        actions.retain(|action| {
            !matches!(
                action.kind,
                semantic_action_contract::SemanticActionKind::LlmRequest
                    | semantic_action_contract::SemanticActionKind::LlmResponse
            )
        });
        actions.extend(llm_output.actions);
        links.extend(llm_output.links);
        let sse_stream_action_id = actions
            .iter()
            .find(|action| action.kind == semantic_action_contract::SemanticActionKind::SseStream)
            .map(|action| action.action_id.clone());
        for action in &actions {
            let role = match action.kind {
                semantic_action_contract::SemanticActionKind::LlmRequest => {
                    semantic_action_contract::SemanticActionLinkRole::LlmRequestHttpMessage
                }
                semantic_action_contract::SemanticActionKind::LlmResponse => {
                    semantic_action_contract::SemanticActionLinkRole::LlmResponseHttpMessage
                }
                _ => continue,
            };
            let http_stream_id = action.attributes.get("http.stream_id");
            let http_action_id = actions
                .iter()
                .find(|candidate| {
                    candidate.kind == semantic_action_contract::SemanticActionKind::HttpMessage
                        && match http_stream_id {
                            Some(stream_id) => {
                                candidate.attributes.get("http.stream_id") == Some(stream_id)
                            }
                            None => !candidate.attributes.contains_key("http.stream_id"),
                        }
                })
                .map(|candidate| candidate.action_id.clone());
            if let Some(target_action_id) = &http_action_id {
                links.push(semantic_action_contract::SemanticActionLink {
                    trace_id: action.trace_id,
                    source_action_id: action.action_id.clone(),
                    target_action_id: target_action_id.clone(),
                    role,
                    confidence: semantic_action_contract::SemanticActionLinkConfidence::Observed,
                    valid: true,
                });
            }
            if action.kind == semantic_action_contract::SemanticActionKind::LlmResponse
                && let Some(target_action_id) = &sse_stream_action_id
            {
                links.push(semantic_action_contract::SemanticActionLink {
                    trace_id: action.trace_id,
                    source_action_id: action.action_id.clone(),
                    target_action_id: target_action_id.clone(),
                    role: semantic_action_contract::SemanticActionLinkRole::LlmResponseSseStream,
                    confidence: semantic_action_contract::SemanticActionLinkConfidence::Observed,
                    valid: true,
                });
            }
        }
        for action in &actions {
            if action.kind == semantic_action_contract::SemanticActionKind::LlmCall
                && let Some(command_id) = self.command_heads.get(&(trace_id, action.process.get()))
            {
                links.push(semantic_action_contract::SemanticActionLink {
                    trace_id,
                    source_action_id: command_id.clone(),
                    target_action_id: action.action_id.clone(),
                    role: semantic_action_contract::SemanticActionLinkRole::CommandContainsLlmCall,
                    confidence: semantic_action_contract::SemanticActionLinkConfidence::Derived,
                    valid: true,
                });
            }
        }
        for action in actions {
            let content_kind = match action.kind {
                semantic_action_contract::SemanticActionKind::HttpMessage => {
                    Some(semantic_action_contract::SemanticContentKind::HttpSummary)
                }
                semantic_action_contract::SemanticActionKind::SseStream
                | semantic_action_contract::SemanticActionKind::SseEvent => {
                    Some(semantic_action_contract::SemanticContentKind::SseSummary)
                }
                semantic_action_contract::SemanticActionKind::LlmCall
                | semantic_action_contract::SemanticActionKind::LlmRequest
                | semantic_action_contract::SemanticActionKind::LlmResponse => {
                    Some(semantic_action_contract::SemanticContentKind::LlmJson)
                }
                semantic_action_contract::SemanticActionKind::McpToolCall
                | semantic_action_contract::SemanticActionKind::McpRequest
                | semantic_action_contract::SemanticActionKind::McpResponse
                | semantic_action_contract::SemanticActionKind::McpStdin
                | semantic_action_contract::SemanticActionKind::McpStdout => {
                    Some(semantic_action_contract::SemanticContentKind::McpJson)
                }
                semantic_action_contract::SemanticActionKind::FileRead
                | semantic_action_contract::SemanticActionKind::FileWrite
                | semantic_action_contract::SemanticActionKind::FileModify => {
                    Some(semantic_action_contract::SemanticContentKind::FilePathSet)
                }
                _ => None,
            };
            let canonical_json = content_kind.and_then(|_kind| {
                if matches!(
                    action.completeness,
                    semantic_action_contract::SemanticActionCompleteness::Unknown
                ) {
                    return None;
                }
                // Keep structured summaries bounded (raw bodies persist in
                // payload_segments); omit oversized JSON rather than emit an
                // invalid summary.
                serde_json::to_string(&action.attributes)
                    .ok()
                    .filter(|summary| summary.len() <= 16 * 1024)
            });
            let content = content_kind.map(|kind| semantic_action_contract::SemanticContent {
                trace_id: action.trace_id,
                action_id: action.action_id.clone(),
                content_id: format!("{}:content", action.action_id),
                kind,
                state: match action.completeness {
                    semantic_action_contract::SemanticActionCompleteness::Complete => {
                        semantic_action_contract::SemanticContentState::Complete
                    }
                    semantic_action_contract::SemanticActionCompleteness::Partial
                    | semantic_action_contract::SemanticActionCompleteness::Inferred => {
                        semantic_action_contract::SemanticContentState::Partial
                    }
                    semantic_action_contract::SemanticActionCompleteness::Unknown => {
                        semantic_action_contract::SemanticContentState::MetadataOnly
                    }
                },
                payload_reference: Some(segment_reference.clone()),
                canonical_json,
            });
            writes.push(PersistWrite::SemanticAction(action));
            if let Some(content) = content {
                writes.push(PersistWrite::SemanticContent(content));
            }
        }
        for link in links {
            writes.push(PersistWrite::SemanticLink(link));
        }
        self.push_writes(writes);
        Ok(())
    }

    /// Stop active collector bindings and flush the writer thread.
    pub fn shutdown(&mut self) -> Result<(), ControlError> {
        let trace_ids = self.roots.keys().copied().collect::<Vec<_>>();
        let mut writes = Vec::new();
        for trace_id in trace_ids {
            self.collector
                .unbind_trace(trace_id)
                .map_err(collector_control_error)?;
            // A daemon shutdown ends the observation window. Mark any
            // non-terminal trace failed before checkpointing so consumers do
            // not mistake an abruptly closed stream for a complete trace.
            let lifecycle = self
                .runtime
                .get_trace(trace_id)
                .map(|entry| entry.trace.lifecycle_state);
            writes.extend(
                self.llm_exchange
                    .finalize_trace(trace_id)
                    .into_iter()
                    .map(PersistWrite::SemanticAction),
            );
            if lifecycle.is_some_and(|state| !state.is_terminal()) {
                let now = SystemTime::now();
                self.runtime
                    .fail_trace(trace_id, now)
                    .map_err(|error| control_error("trace_shutdown", format!("{error:?}")))?;
                writes.push(PersistWrite::TraceLifecycle {
                    trace_id,
                    state: model_core::trace::TraceLifecycleState::Failed,
                });
            }
            let retention = self.payload_retention.stats(trace_id);
            if retention.metadata_only_segments > 0 {
                writes.push(PersistWrite::Diagnostic(CaptureDiagnostic {
                    trace_id,
                    observed_at: SystemTime::now(),
                    collector: model_core::ids::CollectorName::new("payload-retention"),
                    kind: model_core::diagnostics::DiagnosticKind::Truncated,
                    severity: model_core::diagnostics::DiagnosticSeverity::Warning,
                    message: "payload_metadata_only_aggregate".to_string(),
                    dedupe_key: None,
                    dropped: retention.metadata_only_segments,
                    dropped_bytes: retention.metadata_only_bytes,
                }));
            }
        }
        self.push_writes(writes);
        // Publish the final numbers before the writer stops, so a run's losses
        // are recorded even if the last interval has not elapsed.
        self.record_collector_losses()?;
        self.absorb_writer_losses();
        self.report_loss_ledger(true);
        self.flush_pending_storage();
        let writer_result = self
            .writer
            .stop_and_checkpoint()
            .map_err(|error| control_error("writer_shutdown", error));
        let actor_result = self
            .collector
            .shutdown()
            .map_err(|error| control_error("collector_shutdown", error));
        writer_result.and(actor_result)
    }

    fn execute_track_add(
        &mut self,
        command: TrackAddCommand,
    ) -> Result<TrackAddReply, ControlError> {
        let operation_id = command.request_id;
        let active_count = self
            .runtime
            .list_trace_records()
            .into_iter()
            .filter(|trace| trace.lifecycle_state.is_active_or_draining())
            .count();
        if active_count >= self.active_trace_max {
            return Err(ControlError::new(
                "active_trace_limit",
                "active trace limit reached",
            ));
        }
        let root_observation =
            resolve_namespaced_pid(command.root.namespace_pid, &command.root.pid_namespace)
                .map_err(|error| ControlError::new("resolve_root", error))?;
        let root_host_pid = root_observation
            .host
            .as_ref()
            .map(|host| host.pid)
            .ok_or_else(|| ControlError::new("resolve_root", "root has no host PID"))?;
        let snapshot = ProcfsTreeSnapshotter
            .snapshot(&root_observation)
            .map_err(|error| ControlError::new("snapshot_process_tree", error))?;
        let (root_identity, mut root_record) = self.resolve_process(root_observation.clone())?;
        root_record.session_id = root_observation
            .host
            .as_ref()
            .and_then(|host| self.read_process_session(host.pid));
        // Continue a persisted trace id when requested so a restarted root
        // process keeps the same observation window; otherwise allocate fresh.
        // A trace whose root process has exited (or whose root pid is no
        // longer alive) may be re-rooted by a new root even while the daemon
        // still holds the entry - this is what lets a dsh web restart reuse
        // the same trace id while the daemon keeps running.
        let (trace_id, continued) = match command.trace_id {
            Some(trace_id) => {
                if let Some(entry) = self.runtime.get_trace(trace_id) {
                    if !trace_reroot_allowed(
                        entry.trace.lifecycle_state.is_active_or_draining(),
                        self.root_is_alive(trace_id),
                    ) {
                        return Err(ControlError::new(
                            "trace_id",
                            "trace is already active in this daemon",
                        ));
                    }
                    self.collector
                        .unbind_trace(trace_id)
                        .map_err(collector_control_error)?;
                    self.runtime.forget_trace(trace_id);
                }
                let persisted = self
                    .writer
                    .read_trace(trace_id)
                    .map_err(|error| control_error("read_trace", error))?
                    .ok_or_else(|| ControlError::new("trace_id", "trace not found in storage"))?;
                (trace_id, Some(persisted))
            }
            None => (self.runtime.reserve_trace_id(), None),
        };
        let display_name = continued
            .as_ref()
            .map(|trace| trace.display_name.clone())
            .unwrap_or(command.display_name);
        let tags = continued
            .as_ref()
            .map(|trace| trace.tags.clone())
            .unwrap_or(command.tags);
        let created_at = continued
            .as_ref()
            .map(|trace| trace.timings.created_at)
            .unwrap_or_else(SystemTime::now);
        let profile_snapshot =
            CaptureProfileSnapshot::from_profile(&self.profile, SystemTime::now());
        let sensor_plan = self
            .runtime
            .negotiate(&profile_snapshot)
            .map_err(|error| control_error("negotiate", error))?;
        let container = read_container_identity(root_host_pid);
        self.runtime
            .create_starting_trace(
                trace_id,
                TrackTraceRequest {
                    root_identity,
                    root_pid_namespace: root_observation
                        .namespace
                        .as_ref()
                        .map(|value| value.pid_namespace.clone()),
                    root_container_id: container.as_ref().map(|value| value.container_id.clone()),
                    root_working_directory: snapshot
                        .root_working_directory()
                        .map(ToOwned::to_owned),
                    display_name,
                    profile_snapshot: profile_snapshot.clone(),
                    tags,
                    created_at,
                },
                sensor_plan,
            )
            .map_err(|error| control_error("create_trace", error))?;

        if self.ebpf_enabled {
            self.collector
                .bind_trace(TraceBindingRequest {
                    trace_id,
                    root_identity,
                    root_observation: root_observation.clone(),
                    root_namespace_pid: command.root.namespace_pid,
                    profile_snapshot,
                    requested_capabilities: self.profile.capabilities.clone(),
                })
                .map_err(collector_control_error)?;
        } else {
            self.runtime
                .mark_degraded(trace_id)
                .map_err(|error| control_error("mark_degraded", error))?;
        }

        let mut records = vec![root_record];
        for process in snapshot.processes {
            let (identity, mut record) = self.resolve_process(process.identity.clone())?;
            record.session_id = process
                .identity
                .host
                .as_ref()
                .and_then(|host| self.read_process_session(host.pid));
            if identity != root_identity {
                let parent_observation = process.parent.ok_or_else(|| {
                    ControlError::new("snapshot_process_tree", "descendant has no parent")
                })?;
                let (parent, mut parent_record) =
                    self.resolve_process(parent_observation.clone())?;
                parent_record.session_id = parent_observation
                    .host
                    .as_ref()
                    .and_then(|host| self.read_process_session(host.pid));
                records.push(parent_record);
                self.runtime
                    .insert_membership(
                        trace_id,
                        model_core::process::ProcessMembership::inherited(
                            trace_id,
                            identity,
                            parent,
                            snapshot.captured_at,
                        ),
                    )
                    .map_err(|error| control_error("insert_membership", error))?;
            }
            records.push(record);
        }
        records.sort_by_key(|record| record.identity);
        records.dedup_by_key(|record| record.identity);
        if self.ebpf_enabled {
            self.collector
                .seed_trace_memberships(trace_id, records.clone())
                .map_err(collector_control_error)?;
        }
        self.decoder
            .seed_decode_memberships(trace_id, records.clone());
        self.runtime
            .activate_trace(trace_id, SystemTime::now())
            .map_err(|error| control_error("activate_trace", error))?;
        self.persist_trace_state(trace_id, records)?;
        self.roots.insert(trace_id, root_host_pid);
        Ok(TrackAddReply {
            trace_id: Some(trace_id),
            lifecycle_state: self
                .runtime
                .get_trace(trace_id)
                .expect("created trace")
                .trace
                .lifecycle_state,
            operation_id,
            operation_state: OperationState::Active,
            error: None,
        })
    }

    fn persist_trace_state(
        &mut self,
        trace_id: TraceId,
        records: Vec<model_core::process::ProcessRecord>,
    ) -> Result<(), ControlError> {
        let mut writes = records
            .into_iter()
            .map(PersistWrite::ProcessRecord)
            .collect::<Vec<_>>();
        let entry = self
            .runtime
            .get_trace(trace_id)
            .ok_or_else(|| ControlError::new("persist_trace", "trace is missing"))?;
        writes.push(PersistWrite::TraceRecord(entry.trace.clone()));
        for membership in entry.memberships.memberships().cloned().collect::<Vec<_>>() {
            writes.push(PersistWrite::Membership(membership));
        }
        self.writer
            .persist(writes)
            .map_err(|error| control_error("persist_trace_state", error))
    }

    fn resolve_process(
        &mut self,
        observation: ProcessObservation,
    ) -> Result<(ProcessIdentity, model_core::process::ProcessRecord), ControlError> {
        let resolution = match self.processes.resolve_or_create(observation.clone()) {
            Ok(value) => value,
            Err(ProcessIdentityError::IdBlockExhausted) => {
                let (start, end) = self
                    .writer
                    .reserve_id_block(PROCESS_ID_BLOCK_SIZE)
                    .map_err(|error| control_error("reserve_process_id_block", error))?;
                self.processes
                    .install_reserved_block(start, end)
                    .map_err(|error| control_error("process_identity", error))?;
                self.processes
                    .resolve_or_create(observation)
                    .map_err(|error| control_error("process_identity", error))?
            }
            Err(error) => return Err(control_error("process_identity", error)),
        };
        let record = self
            .processes
            .record(resolution.identity)
            .cloned()
            .ok_or_else(|| ControlError::new("process_identity", "resolved record is missing"))?;
        Ok((resolution.identity, record))
    }

    /// Resolve the session identity and call id of one collected item.
    ///
    /// The envelope is updated in place so that the session recorded against
    /// the item is the same one used to attribute it. This runs before process
    /// identity resolution because the call-span matcher keys on the session
    /// already carried by the envelope.
    ///
    /// The kernel session cache is authoritative when it has an entry: the
    /// collector copies it into the envelope from a per-process cache, so a hit
    /// means the session was already resolved and the process environment does
    /// not have to be read again. Reading it costs a full `/proc/<pid>/environ`
    /// read per environment variable name and is the most expensive per-item
    /// syscall on this path, so it is consulted at most once per item and only
    /// when the kernel cache had nothing.
    fn resolve_item_session(
        &mut self,
        trace_id: TraceId,
        host_pid: Option<u32>,
        envelope: &mut collector_event::RawEventEnvelope,
    ) -> SessionResolution {
        let kernel = envelope.session_id.clone();
        let procfs = match (&kernel, host_pid) {
            (None, Some(pid)) => {
                self.session_for_process(pid, process_generation(&envelope.process))
            }
            _ => None,
        };
        if envelope.session_id.is_none() {
            envelope.session_id = procfs.clone();
        }
        if envelope.call_id.is_none() {
            envelope.call_id = self.assign_call_id(
                trace_id,
                host_pid,
                &envelope.session_id,
                envelope.observed_at,
            );
        }
        let from_call = match (envelope.session_id.as_ref(), envelope.call_id.as_deref()) {
            (None, Some(call_id)) => self.session_for_call(trace_id, call_id),
            _ => None,
        };
        if envelope.session_id.is_none() {
            envelope.session_id = from_call.clone();
        }
        let resolution = SessionResolution {
            kernel,
            procfs,
            from_call,
        };
        // Runs once per collected item: keep at `trace!` so DEBUG has no
        // per-item log-write cost (a DEBUG line here stalled the control plane
        // under capture load).
        tracing::trace!(
            host_pid = ?host_pid,
            kernel_session = ?resolution.kernel,
            proc_session = ?resolution.procfs,
            source = resolution.source(),
            "session resolution"
        );
        resolution
    }

    fn read_process_session(&self, pid: u32) -> Option<SessionIdentity> {
        read_process_session(pid, "CENSORSCOPE_SESSION_ID")
            .or_else(|| read_process_session(pid, &self.session_env_name))
    }

    /// Session lookup for one process, reusing the result for its lifetime.
    fn session_for_process(
        &mut self,
        host_pid: u32,
        generation: Option<u64>,
    ) -> Option<SessionIdentity> {
        if let Some(generation) = generation
            && let Some(cached) = self.session_lookups.get(&(host_pid, generation))
        {
            return cached.clone();
        }
        let session = self.read_process_session(host_pid);
        if let Some(generation) = generation {
            self.evict_process_caches_if_full();
            self.session_lookups
                .insert((host_pid, generation), session.clone());
        }
        session
    }

    /// Host coordinates of one observation, reusing the result of an earlier
    /// lookup for the same process lifetime.
    ///
    /// The observation the event carried is returned unchanged when no
    /// generation is available to pin a process lifetime, and when `/proc` is
    /// unreadable: a failed lookup is cheap, while caching its unenriched result
    /// would pin an observation that cannot distinguish a reused PID for the
    /// rest of that lifetime.
    fn resolve_observation(&mut self, observed: ProcessObservation) -> ProcessObservation {
        let Some(pid) = observed.host.as_ref().map(|host| host.pid) else {
            return observed;
        };
        let Some(generation) = process_generation(&observed) else {
            return ProcfsIdentityReader.read_identity(pid).unwrap_or(observed);
        };
        if let Some(cached) = self.process_identities.get(&(pid, generation)) {
            return cached.clone();
        }
        match ProcfsIdentityReader.read_identity(pid) {
            Ok(resolved) => {
                self.evict_process_caches_if_full();
                self.process_identities
                    .insert((pid, generation), resolved.clone());
                resolved
            }
            Err(_) => observed,
        }
    }

    /// Drop every cached lookup once the maps reach their bound.
    ///
    /// The entries are pure caches, so eviction only costs a `/proc` re-read.
    /// Bounding them keeps a long-running daemon from accumulating entries for
    /// processes whose exit was never observed; both maps share one key space
    /// and one lifetime, so they are cleared together.
    fn evict_process_caches_if_full(&mut self) {
        if self.process_identities.len() < PROCESS_CACHE_MAX_ENTRIES {
            return;
        }
        self.process_identities.clear();
        self.session_lookups.clear();
    }

    /// Forget the cached lookups of a process that has exited.
    fn forget_process_caches(&mut self, host_pid: Option<u32>) {
        let Some(pid) = host_pid else {
            return;
        };
        self.process_identities
            .retain(|(cached_pid, _), _| *cached_pid != pid);
        self.session_lookups
            .retain(|(cached_pid, _), _| *cached_pid != pid);
    }

    /// Whether the effective capture profile requests IPC audit events
    /// (collection level L3). Pipe/socketpair probes stay attached at every
    /// level as channel infrastructure; below L3 their observations are
    /// consumed internally (fd lineage / orchestration-noise folding) and the
    /// audit output is suppressed in `apply_event`.
    fn ipc_audit_enabled(&self) -> bool {
        self.profile.capabilities.iter().any(|request| {
            matches!(
                request.capability,
                model_core::capability::Capability::IpcPipeFifo
                    | model_core::capability::Capability::IpcUnixSocket
            )
        })
    }

    fn apply_event(
        &mut self,
        mut raw: collector_event::RawCollectorEvent,
    ) -> Result<(), ControlError> {
        let trace_id = raw
            .envelope
            .trace_id
            .ok_or_else(|| ControlError::new("process_event", "event has no trace ID"))?;
        if self.runtime.get_trace(trace_id).is_none() {
            // Captured but not owned: the trace was removed or never tracked here.
            self.loss_ledger.add("data.unknown_trace", 1, 0);
            return Ok(());
        }
        let host_pid = raw.envelope.process.host.as_ref().map(|host| host.pid);
        let session_resolution = self.resolve_item_session(trace_id, host_pid, &mut raw.envelope);
        let operation = match &raw.payload {
            collector_event::RawObservationPayload::Process { operation, .. } => {
                Some(operation.as_str())
            }
            _ => None,
        };
        let observed = raw.envelope.process.clone();
        let observation = self.resolve_observation(observed);
        let (identity, mut record) = self.resolve_process(observation)?;
        let session = session_resolution.effective();
        record.session_id = session.clone();
        self.ingested.count = self.ingested.count.saturating_add(1);
        let mut writes = vec![PersistWrite::ProcessRecord(record)];
        if let Some(session) = &session {
            writes.push(PersistWrite::Session {
                session_id: session.clone(),
                trace_id,
                observed_at: raw.envelope.observed_at,
            });
        }
        let parent = match &raw.payload {
            collector_event::RawObservationPayload::Process {
                parent: Some(parent),
                ..
            } => {
                let (parent, parent_record) = self.resolve_process(parent.clone())?;
                writes.push(PersistWrite::ProcessRecord(parent_record));
                Some(parent)
            }
            _ => None,
        };
        self.update_fd_lineage(trace_id, identity, parent, &raw.payload);
        self.track_root_channel(trace_id, identity, &raw.payload);
        if self.drop_orchestration_noise(trace_id, identity, &raw.payload) {
            return Ok(());
        }
        // IPC probes stay attached at every level as channel infrastructure;
        // below L3 their observations are consumed internally (fd lineage and
        // orchestration-noise folding above) and the IPC audit event itself is
        // suppressed so no Ipc row is persisted, exported, or shown in the UI.
        if matches!(
            &raw.payload,
            collector_event::RawObservationPayload::Ipc { .. }
        ) && !self.ipc_audit_enabled()
        {
            return Ok(());
        }
        if operation == Some("fork")
            && self
                .runtime
                .find_membership_in_trace(trace_id, &identity)
                .is_none()
        {
            let parent =
                parent.ok_or_else(|| ControlError::new("process_event", "fork has no parent"))?;
            self.runtime
                .inherit_process(trace_id, &parent, identity, raw.envelope.observed_at)
                .map_err(|error| control_error("inherit_process", error))?;
        }
        let event = ingest_runtime::normalize(
            raw.clone(),
            ingest_runtime::IngestMatch {
                trace_id,
                process: identity,
                parent,
                session: session.clone(),
            },
            self.take_event_id()?,
        );
        let semantic_batch = semantic_action_runtime::project_event(&event);
        self.queue_event(event);
        let actions = semantic_batch.actions;
        let mut links = semantic_batch.links;
        for action in &actions {
            if action.kind == semantic_action_contract::SemanticActionKind::CommandInvocation {
                self.command_heads
                    .insert((trace_id, identity.get()), action.action_id.clone());
            }
        }
        if let Some(command_id) = self.command_heads.get(&(trace_id, identity.get())).cloned() {
            if actions.iter().any(|action| {
                matches!(
                    action.kind,
                    semantic_action_contract::SemanticActionKind::FileRead
                        | semantic_action_contract::SemanticActionKind::FileWrite
                        | semantic_action_contract::SemanticActionKind::FileModify
                        | semantic_action_contract::SemanticActionKind::FileTtyIo
                )
            }) {
                for action in &actions {
                    if matches!(
                        action.kind,
                        semantic_action_contract::SemanticActionKind::FileRead
                            | semantic_action_contract::SemanticActionKind::FileWrite
                            | semantic_action_contract::SemanticActionKind::FileModify
                            | semantic_action_contract::SemanticActionKind::FileTtyIo
                    ) {
                        links.push(semantic_action_contract::SemanticActionLink {
                            trace_id,
                            source_action_id: command_id.clone(),
                            target_action_id: action.action_id.clone(),
                            role: semantic_action_contract::SemanticActionLinkRole::CommandContainsFileAccess,
                            confidence: semantic_action_contract::SemanticActionLinkConfidence::Derived,
                            valid: true,
                        });
                    }
                }
            }
        }
        for action in actions {
            writes.push(PersistWrite::SemanticAction(action));
        }
        for link in links {
            writes.push(PersistWrite::SemanticLink(link));
        }
        if operation == Some("exit") {
            let code = match &raw.payload {
                collector_event::RawObservationPayload::Process { metadata, .. } => metadata
                    .get("exit_code")
                    .and_then(|value| value.parse().ok()),
                _ => None,
            };
            self.runtime
                .mark_process_exited(
                    trace_id,
                    &identity,
                    ExitStatus {
                        code,
                        observed_at: raw.envelope.observed_at,
                        source: Some(ExitObservationSource::Event),
                    },
                )
                .map_err(|error| control_error("mark_process_exited", error))?;
            self.processes.mark_exited(identity);
            self.command_heads.remove(&(trace_id, identity.get()));
            // An exit is the last event of a process lifetime: releasing its
            // cached lookups keeps the caches proportional to the processes
            // that are actually live.
            self.forget_process_caches(host_pid);
        }
        if let Some(membership) = self.runtime.find_membership_in_trace(trace_id, &identity) {
            writes.push(PersistWrite::Membership(membership));
        }
        let lifecycle = self
            .runtime
            .get_trace(trace_id)
            .expect("trace checked")
            .trace
            .lifecycle_state;
        // Every event carries the trace's lifecycle state but few events change
        // it, and the state is readable from the trace row at any time: only a
        // transition needs its own write.
        let lifecycle_changed = self.trace_lifecycle_written.get(&trace_id) != Some(&lifecycle);
        if lifecycle_changed {
            writes.push(PersistWrite::TraceLifecycle {
                trace_id,
                state: lifecycle,
            });
        }
        if self.push_writes(writes) && lifecycle_changed {
            self.trace_lifecycle_written.insert(trace_id, lifecycle);
        }
        Ok(())
    }

    fn update_fd_lineage(
        &mut self,
        trace_id: TraceId,
        process: ProcessIdentity,
        parent: Option<ProcessIdentity>,
        payload: &collector_event::RawObservationPayload,
    ) {
        match payload {
            collector_event::RawObservationPayload::Process { operation, .. }
                if operation == "fork" =>
            {
                if let Some(parent) = parent {
                    self.fd_lineage.fork(trace_id, parent, process);
                }
            }
            collector_event::RawObservationPayload::File {
                operation,
                fd: Some(fd),
                metadata,
                ..
            } if operation == "close" && syscall_succeeded(metadata) => {
                self.fd_lineage.close(trace_id, process, *fd);
            }
            collector_event::RawObservationPayload::File {
                operation,
                fd: Some(first_fd),
                size: Some(last_fd),
                metadata,
                ..
            } if operation == "close_range" && syscall_succeeded(metadata) => {
                let last_fd = i32::try_from(*last_fd).unwrap_or(i32::MAX);
                self.fd_lineage
                    .close_range(trace_id, process, *first_fd, last_fd);
            }
            collector_event::RawObservationPayload::File {
                operation,
                fd: Some(fd),
                metadata,
                ..
            } if matches!(operation.as_str(), "dup" | "dup2" | "dup3" | "fcntl")
                && syscall_succeeded(metadata) =>
            {
                if let Some(target) = metadata
                    .get("target_fd")
                    .and_then(|value| value.parse::<i32>().ok())
                {
                    let _ = self.fd_lineage.dup(trace_id, process, *fd, target);
                }
            }
            collector_event::RawObservationPayload::Ipc {
                operation,
                metadata,
                ..
            } => {
                let first = metadata.get("fd_a").and_then(|value| value.parse().ok());
                let second = metadata.get("fd_b").and_then(|value| value.parse().ok());
                if let (Some(first), Some(second)) = (first, second) {
                    if operation == "pipe" {
                        self.fd_lineage.pipe_pair(trace_id, process, first, second);
                    } else if operation == "socketpair" {
                        self.fd_lineage
                            .socket_pair(trace_id, process, first, second);
                    }
                }
            }
            _ => {}
        }
    }

    fn root_identity(&self, trace_id: TraceId) -> Option<ProcessIdentity> {
        self.runtime
            .get_trace(trace_id)
            .map(|entry| entry.trace.root_process_identity)
    }

    /// Whether the current root host pid of a trace is still alive in this
    /// daemon's pid namespace. A dead root makes the trace re-rootable even
    /// when the daemon has not yet observed the exit lifecycle event.
    fn root_is_alive(&self, trace_id: TraceId) -> bool {
        self.roots
            .get(&trace_id)
            .is_some_and(|pid| std::path::Path::new(&format!("/proc/{pid}")).exists())
    }

    /// A direct child of the trace root (the dsh host process). Tool processes
    /// are grandchildren (children of the worker), so they never match.
    fn is_root_direct_child(&self, trace_id: TraceId, identity: ProcessIdentity) -> bool {
        let Some(root) = self.root_identity(trace_id) else {
            return false;
        };
        if identity == root {
            return false;
        }
        self.runtime
            .find_membership_in_trace(trace_id, &identity)
            .and_then(|membership| membership.inherited_from)
            .is_some_and(|parent| parent == root)
    }

    /// P2 self-noise: remember pipe/Unix-socket channels created by the trace
    /// root so later traffic on those channels (including worker-side
    /// inherited/dup'd fds) can be recognized as orchestration.
    fn track_root_channel(
        &mut self,
        trace_id: TraceId,
        identity: ProcessIdentity,
        payload: &collector_event::RawObservationPayload,
    ) {
        let collector_event::RawObservationPayload::Ipc {
            operation,
            metadata,
            ..
        } = payload
        else {
            return;
        };
        if operation != "pipe" && operation != "socketpair" {
            return;
        }
        if self.root_identity(trace_id) != Some(identity) {
            return;
        }
        let first = metadata
            .get("fd_a")
            .and_then(|value| value.parse::<i32>().ok());
        let second = metadata
            .get("fd_b")
            .and_then(|value| value.parse::<i32>().ok());
        let channels = self
            .root_channels
            .entry(trace_id)
            .or_insert_with(BTreeSet::new);
        for fd in [first, second].into_iter().flatten() {
            if let Some(channel) = self.fd_lineage.channel_of(trace_id, identity, fd) {
                channels.insert(channel);
            }
        }
    }

    /// P2 self-noise decision: drop events that are pure orchestration traffic
    /// between the trace root (dsh host) and its direct workers - the worker's
    /// console/std streams and read/write on root-created pipe/socketpair
    /// channels, on both ends. Real tool activity (grandchildren) is kept.
    fn drop_orchestration_noise(
        &self,
        trace_id: TraceId,
        identity: ProcessIdentity,
        payload: &collector_event::RawObservationPayload,
    ) -> bool {
        if !self.is_root_direct_child(trace_id, identity)
            && self.root_identity(trace_id) != Some(identity)
        {
            return false;
        }
        let is_root = self.root_identity(trace_id) == Some(identity);
        match payload {
            // Direct-child console streams go to the host: orchestration.
            collector_event::RawObservationPayload::Stdio { .. } => !is_root,
            collector_event::RawObservationPayload::File {
                operation,
                fd: Some(fd),
                ..
            } if is_rw_operation(operation) => {
                self.channel_is_noise(trace_id, identity, *fd, is_root)
            }
            collector_event::RawObservationPayload::Net {
                operation,
                metadata,
                ..
            } if is_rw_operation(operation) => metadata
                .get("fd")
                .and_then(|value| value.parse::<i32>().ok())
                .is_some_and(|fd| self.channel_is_noise(trace_id, identity, fd, is_root)),
            _ => false,
        }
    }

    fn channel_is_noise(
        &self,
        trace_id: TraceId,
        identity: ProcessIdentity,
        fd: i32,
        is_root: bool,
    ) -> bool {
        if !self.fd_lineage.is_channel_fd(trace_id, identity, fd) {
            return false;
        }
        if is_root {
            // Root-created pipe/socketpair endpoints are orchestration by
            // definition (tools are spawned by workers, not by the root).
            return true;
        }
        self.fd_lineage
            .channel_of(trace_id, identity, fd)
            .is_some_and(|channel| {
                self.root_channels
                    .get(&trace_id)
                    .is_some_and(|channels| channels.contains(&channel))
            })
    }

    fn take_event_id(&mut self) -> Result<EventId, ControlError> {
        let id = EventId::new(self.next_event_id);
        self.next_event_id = self
            .next_event_id
            .checked_add(1)
            .ok_or_else(|| ControlError::new("event_id", "event ID exhausted"))?;
        Ok(id)
    }

    fn matching_trace_ids(&self, selector: Option<&TraceSelector>) -> Vec<TraceId> {
        self.runtime
            .list_trace_records()
            .into_iter()
            .filter(|trace| {
                selector.is_none_or(|selector| {
                    selector.matches(trace, self.roots.get(&trace.trace_id).copied())
                })
            })
            .map(|trace| trace.trace_id)
            .collect()
    }

    fn list_items(&self, selector: Option<&TraceSelector>) -> Vec<TraceListItem> {
        self.matching_trace_ids(selector)
            .into_iter()
            .filter_map(|trace_id| {
                let trace = &self.runtime.get_trace(trace_id)?.trace;
                Some(TraceListItem {
                    trace_id,
                    display_name: trace.display_name.clone(),
                    root_pid: *self.roots.get(&trace_id)?,
                    root_pid_namespace: trace.root_pid_namespace.clone(),
                    root_container_id: trace.root_container_id.clone(),
                    lifecycle_state: trace.lifecycle_state,
                    health: trace.health,
                    tags: trace.tags.clone(),
                    created_at: trace.timings.created_at,
                })
            })
            .collect()
    }

    fn remove(&mut self, selector: &TraceSelector) -> Result<TraceId, ControlError> {
        let matches = self.matching_trace_ids(Some(selector));
        let [trace_id] = matches.as_slice() else {
            return Err(ControlError::new(
                "trace_selector",
                if matches.is_empty() {
                    "no active trace matched"
                } else {
                    "selector matched multiple active traces"
                },
            ));
        };
        let trace_id = *trace_id;
        self.runtime
            .track_remove_root(RootRemovalRequest {
                trace_id,
                removed_at: SystemTime::now(),
            })
            .map_err(|error| control_error("track_remove", error))?;
        self.collector
            .unbind_trace(trace_id)
            .map_err(collector_control_error)?;
        let mut writes = Vec::new();
        if let Some(entry) = self.runtime.get_trace(trace_id) {
            writes.push(PersistWrite::TraceLifecycle {
                trace_id,
                state: entry.trace.lifecycle_state,
            });
        }
        // The trace is no longer owned by this daemon: stop tracking the state
        // already handed to storage so a later re-root starts clean.
        self.trace_lifecycle_written.remove(&trace_id);
        let retention = self.payload_retention.stats(trace_id);
        if retention.metadata_only_segments > 0 {
            writes.push(PersistWrite::Diagnostic(CaptureDiagnostic {
                trace_id,
                observed_at: SystemTime::now(),
                collector: model_core::ids::CollectorName::new("payload-retention"),
                kind: model_core::diagnostics::DiagnosticKind::Truncated,
                severity: model_core::diagnostics::DiagnosticSeverity::Warning,
                message: "payload_metadata_only_aggregate".to_string(),
                dedupe_key: None,
                dropped: retention.metadata_only_segments,
                dropped_bytes: retention.metadata_only_bytes,
            }));
        }
        self.writer
            .persist_async(writes)
            .map_err(|error| control_error("trace_remove_queue", error))?;
        self.payload_reassembly.retain(|(id, _), _| *id != trace_id);
        self.payload_call_buffers
            .retain(|(id, _, _), _| *id != trace_id);
        let finalized = self
            .llm_exchange
            .finalize_trace(trace_id)
            .into_iter()
            .map(PersistWrite::SemanticAction)
            .collect::<Vec<_>>();
        if !finalized.is_empty() {
            self.writer
                .persist_async(finalized)
                .map_err(|error| control_error("trace_remove_finalize", error))?;
        }
        self.mcp_payload_action_heads
            .retain(|(id, _), _| *id != trace_id);
        self.runtime.forget_trace(trace_id);
        self.roots.remove(&trace_id);
        Ok(trace_id)
    }
}

fn is_rw_operation(operation: &str) -> bool {
    matches!(
        operation,
        "read"
            | "write"
            | "readv"
            | "writev"
            | "pread64"
            | "pwrite64"
            | "send"
            | "sendto"
            | "sendmsg"
            | "recv"
            | "recvfrom"
            | "recvmsg"
    )
}

fn syscall_succeeded(metadata: &BTreeMap<String, String>) -> bool {
    metadata
        .get("result")
        .and_then(|value| value.parse::<i64>().ok())
        .is_some_and(|result| result >= 0)
}

fn drop_counter_deltas(
    totals: &mut BTreeMap<String, u64>,
    counters: &[collector_stats::DropCounter],
) -> Vec<(String, u64)> {
    counters
        .iter()
        .filter_map(|counter| {
            let previous = totals
                .insert(counter.reason.clone(), counter.count)
                .unwrap_or(0);
            let delta = if counter.count >= previous {
                counter.count - previous
            } else {
                counter.count
            };
            (delta > 0).then(|| (counter.reason.clone(), delta))
        })
        .collect()
}

#[cfg(test)]
mod loss_tests {
    use super::*;

    #[test]
    fn collector_drop_deltas_are_incremental_and_handle_counter_reset() {
        let mut totals = BTreeMap::new();
        let counters = |count| {
            vec![collector_stats::DropCounter {
                reason: "ring_buffer_loss".to_string(),
                count,
            }]
        };
        assert_eq!(
            drop_counter_deltas(&mut totals, &counters(3)),
            vec![("ring_buffer_loss".to_string(), 3)]
        );
        assert!(drop_counter_deltas(&mut totals, &counters(3)).is_empty());
        assert_eq!(
            drop_counter_deltas(&mut totals, &counters(5)),
            vec![("ring_buffer_loss".to_string(), 2)]
        );
        assert_eq!(
            drop_counter_deltas(&mut totals, &counters(1)),
            vec![("ring_buffer_loss".to_string(), 1)]
        );
    }
}

impl ControlService for DaemonServiceHost {
    fn handle(&mut self, command: ControlCommand) -> Result<ControlReply, ControlError> {
        self.handle_command(None, command)
    }

    fn handle_from_peer(
        &mut self,
        credentials: PeerCredentials,
        command: ControlCommand,
    ) -> Result<ControlReply, ControlError> {
        let peer = PeerIdentity::resolve(credentials)?;
        if let ControlCommand::TrackAdd(command) = &command {
            peer.authorize_process_ref(&command.root)?;
        }
        self.handle_command(Some(peer), command)
    }
}

impl DaemonServiceHost {
    /// Execute one `track-add` request to a terminal state.
    ///
    /// This runs inline on the control worker thread so the reply to the
    /// request itself carries the outcome: `active` with the created (or
    /// continued) trace id, or a structured error. Nothing is deferred to a
    /// later drain tick, so no caller has to poll `operation-status` to learn
    /// whether the trace started. The trade-off is that this request occupies
    /// the control worker for the whole operation (procfs snapshot, identity
    /// resolution and the writer's control barrier) instead of returning
    /// immediately; spanning a dsh boot does not send control traffic that
    /// would need to jump ahead of it.
    fn run_track_add_operation(
        &mut self,
        operation_id: RequestId,
    ) -> Result<TrackAddReply, ControlError> {
        let Some(mut operation) = self.track_operations.remove(&operation_id) else {
            return Err(ControlError::new(
                "operation_not_found",
                "operation does not exist",
            ));
        };
        operation.state = OperationState::Starting;
        let peer = operation.peer.clone();
        let before = self
            .runtime
            .list_trace_records()
            .iter()
            .map(|trace| trace.trace_id)
            .collect::<BTreeSet<_>>();
        let result = self.execute_track_add(operation.command.clone());
        let outcome = match result {
            Ok(reply) => {
                let trace_id = reply.trace_id.expect("successful track-add has a trace id");
                let owner_result = match peer {
                    Some(peer) => self
                        .runtime
                        .bind_trace_owner(trace_id, peer.principal.trace_owner()),
                    None => Ok(()),
                };
                match owner_result {
                    Ok(()) => {
                        operation.state = OperationState::Active;
                        operation.trace_id = Some(trace_id);
                        operation.lifecycle_state = Some(reply.lifecycle_state);
                        Ok(reply)
                    }
                    Err(error) => {
                        self.cleanup_failed_track_add(&before);
                        let reason = format!("{error:?}");
                        operation.state = OperationState::Failed;
                        operation.error = Some(format!("trace_owner: {reason}"));
                        operation.lifecycle_state = Some(TraceLifecycleState::Failed);
                        Err(ControlError::new("trace_owner", reason))
                    }
                }
            }
            Err(error) => {
                self.cleanup_failed_track_add(&before);
                operation.state = OperationState::Failed;
                operation.error = Some(format!("{}: {}", error.code, error.message));
                operation.lifecycle_state = Some(TraceLifecycleState::Failed);
                Err(error)
            }
        };
        self.track_operations.insert(operation_id, operation);
        outcome
    }

    fn cleanup_failed_track_add(&mut self, before: &BTreeSet<TraceId>) {
        let created = self
            .runtime
            .list_trace_records()
            .iter()
            .map(|trace| trace.trace_id)
            .filter(|trace_id| !before.contains(trace_id))
            .collect::<Vec<_>>();
        let mut writes = Vec::new();
        for trace_id in created {
            let _ = self.collector.unbind_trace(trace_id);
            if self.runtime.get_trace(trace_id).is_some() {
                let _ = self.runtime.fail_trace(trace_id, SystemTime::now());
                writes.push(PersistWrite::TraceLifecycle {
                    trace_id,
                    state: model_core::trace::TraceLifecycleState::Failed,
                });
            }
            self.runtime.forget_trace(trace_id);
            self.roots.remove(&trace_id);
            self.trace_lifecycle_written.remove(&trace_id);
            self.payload_reassembly.retain(|(id, _), _| *id != trace_id);
            self.payload_call_buffers
                .retain(|(id, _, _), _| *id != trace_id);
        }
        if !writes.is_empty() {
            let _ = self.writer.persist(writes);
        }
    }

    fn operation_status(
        &self,
        command: OperationStatusCommand,
        peer: Option<&PeerIdentity>,
    ) -> Result<ControlReply, ControlError> {
        let operation = self
            .track_operations
            .get(&command.operation_id)
            .ok_or_else(|| ControlError::new("operation_not_found", "operation does not exist"))?;
        if let (Some(peer), Some(owner)) = (peer, operation.peer.as_ref())
            && !peer.principal.matches(&owner.principal)
        {
            return Err(ControlError::new(
                "peer_authorization",
                "peer is not authorized for this operation",
            ));
        }
        Ok(ControlReply::OperationStatus(
            control_contract::reply::OperationStatusReply {
                operation_id: command.operation_id,
                trace_id: operation.trace_id,
                lifecycle_state: operation.lifecycle_state,
                operation_state: operation.state,
                error: operation.error.clone(),
            },
        ))
    }

    fn handle_command(
        &mut self,
        peer: Option<PeerIdentity>,
        command: ControlCommand,
    ) -> Result<ControlReply, ControlError> {
        match command {
            ControlCommand::TrackAdd(command) => {
                let operation_id = command.request_id;
                if self.track_operations.contains_key(&operation_id) {
                    return Err(ControlError::new(
                        "operation_id",
                        "operation id is already in use",
                    ));
                }
                // Every operation reaches a terminal state before its request
                // returns, so this map is diagnostic history for
                // `operation-status` rather than a pending queue: evict the
                // oldest entries instead of refusing new work once it is full.
                while self.track_operations.len() >= MAX_TRACK_OPERATIONS {
                    let Some(oldest) = self.track_operations.keys().next().copied() else {
                        break;
                    };
                    self.track_operations.remove(&oldest);
                }
                self.track_operations.insert(
                    operation_id,
                    TrackAddOperation {
                        command,
                        peer,
                        state: OperationState::Preparing,
                        trace_id: None,
                        lifecycle_state: Some(TraceLifecycleState::Starting),
                        error: None,
                    },
                );
                // Inline execution: the reply below is the final state, so a
                // failure reaches the caller as a control error with the
                // daemon's own reason instead of a `preparing` reply that has
                // to be polled afterwards.
                Ok(ControlReply::TrackAdded(
                    self.run_track_add_operation(operation_id)?,
                ))
            }
            ControlCommand::OperationStatus(command) => {
                self.operation_status(command, peer.as_ref())
            }
            ControlCommand::TrackRemove(command) => {
                let matches = self.matching_trace_ids(Some(&command.selector));
                if let Some(peer) = &peer {
                    for trace_id in &matches {
                        if let Some(owner) = self
                            .runtime
                            .get_trace(*trace_id)
                            .and_then(|entry| entry.owner.as_ref())
                        {
                            peer.authorize_trace_owner(*trace_id, owner)?;
                        }
                    }
                }
                self.remove(&command.selector)?;
                Ok(ControlReply::TrackRemoved)
            }
            ControlCommand::ListTraces(command) => {
                let mut items = self.list_items(command.selector.as_ref());
                if let Some(peer) = &peer {
                    items.retain(|item| {
                        self.runtime
                            .get_trace(item.trace_id)
                            .and_then(|entry| entry.owner.as_ref())
                            .is_none_or(|owner| {
                                peer.authorize_trace_owner(item.trace_id, owner).is_ok()
                            })
                    });
                }
                Ok(ControlReply::TraceList(items))
            }
            ControlCommand::Doctor(_) => Ok(ControlReply::Doctor(DoctorReply {
                available_collectors: if self.ebpf_enabled {
                    vec!["procfs".to_string(), "ebpf".to_string()]
                } else {
                    vec!["procfs".to_string()]
                },
                storage_ready: true,
            })),
            ControlCommand::CallStart(command) => {
                let started_at =
                    std::time::UNIX_EPOCH + std::time::Duration::from_nanos(command.started_at);
                // SQLite keys spans by (trace_id, call_id): dedupe the
                // in-memory matcher on the same identity so a retried
                // call-start cannot create ambiguous duplicate time-window
                // candidates. Only span metadata is deduped; every eBPF
                // observation remains independently stored.
                if let Some(span) = self.call_spans.iter_mut().find(|span| {
                    span.trace_id == command.trace_id && span.call_id == command.call_id
                }) {
                    span.session_id = command.session_id.clone();
                    span.host_pid = command.host_pid;
                    span.started_at = started_at;
                    span.ended_at = None;
                } else {
                    self.call_spans.push(CallSpanRecord {
                        trace_id: command.trace_id,
                        session_id: command.session_id.clone(),
                        call_id: command.call_id.clone(),
                        host_pid: command.host_pid,
                        started_at,
                        ended_at: None,
                        status: None,
                    });
                }
                self.writer
                    .call_start_async(CallStartWrite {
                        trace_id: command.trace_id,
                        session_id: command.session_id.clone(),
                        call_id: command.call_id.clone(),
                        host_pid: command.host_pid,
                        started_at,
                    })
                    .map_err(|error| control_error("call_start_queue", error))?;
                Ok(ControlReply::CallStarted)
            }
            ControlCommand::CallEnd(command) => {
                if !matches!(
                    command.status.as_str(),
                    "success" | "error" | "cancelled" | "timeout"
                ) {
                    return Err(ControlError::new(
                        "status",
                        "status must be success, error, cancelled, or timeout",
                    ));
                }
                let ended_at =
                    std::time::UNIX_EPOCH + std::time::Duration::from_nanos(command.ended_at);
                // Close the in-memory span first so ingest attribution is
                // settled for events drained after this point; jobs scope by
                // the registered span session, not this command's value.
                let (span_started, span_session) = match self.call_spans.iter_mut().find(|span| {
                    span.trace_id == command.trace_id && span.call_id == command.call_id
                }) {
                    Some(span) => {
                        span.ended_at = Some(ended_at);
                        (Some(span.started_at), span.session_id.clone())
                    }
                    None => (None, None),
                };
                let job_session = span_session.or_else(|| command.session_id.clone());
                let gate = self.writer.bulk_pushed_count();
                let now = SystemTime::now();
                let mut jobs = Vec::new();
                if let Some(span_started) = span_started {
                    if span_started <= ended_at {
                        // Events ingested before their queued control
                        // call-start landed were stored with call_id NULL; the
                        // window job re-runs the matcher once all events
                        // enqueued before this close are committed.
                        jobs.push((
                            build_backfill_job(
                                command.trace_id,
                                job_session.clone(),
                                Some(command.call_id.clone()),
                                BackfillJobKind::Window,
                                Some(span_started),
                                Some(ended_at),
                                span_started,
                                ended_at,
                            ),
                            gate,
                        ));
                        // A stale close means the span stayed open while
                        // sibling windows were evaluated (their backfills
                        // ambiguous); sweep the whole session once it lands.
                        let arrived_late = ended_at
                            .checked_add(Duration::from_secs(2))
                            .is_none_or(|bound| bound < now);
                        if arrived_late {
                            jobs.push((
                                build_backfill_job(
                                    command.trace_id,
                                    job_session,
                                    Some(command.call_id.clone()),
                                    BackfillJobKind::Sweep,
                                    Some(span_started),
                                    Some(ended_at),
                                    span_started,
                                    now,
                                ),
                                gate,
                            ));
                        }
                    }
                }
                // Close the stored span and enqueue its backfill jobs together;
                // the writer commits the transaction asynchronously.
                self.writer
                    .call_close_async(CallCloseWrite {
                        trace_id: command.trace_id,
                        session_id: command.session_id.clone(),
                        call_id: command.call_id.clone(),
                        host_pid: command.host_pid,
                        ended_at,
                        status: command.status.clone(),
                        jobs,
                    })
                    .map_err(|error| control_error("call_end_queue", error))?;
                Ok(ControlReply::CallEnded)
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_backfill_job(
    trace_id: TraceId,
    session_id: Option<String>,
    call_id: Option<String>,
    kind: BackfillJobKind,
    span_started_at: Option<SystemTime>,
    span_ended_at: Option<SystemTime>,
    scope_from: SystemTime,
    scope_to: SystemTime,
) -> BackfillJob {
    let now = SystemTime::now();
    BackfillJob {
        queue_id: 0,
        trace_id,
        session_id,
        call_id,
        kind,
        span_started_at,
        span_ended_at,
        scope_from,
        scope_to,
        state: BackfillJobState::Pending,
        attempts: 0,
        last_error: None,
        enqueued_at: now,
        updated_at: now,
    }
}

fn storage_error(error: storage_core::StorageError) -> ControlError {
    ControlError::new(error.stage, error.message)
}

fn collector_control_error(error: collector_instance::CollectorError) -> ControlError {
    ControlError::new(error.stage, error.message)
}

/// Bound on the per-process lookup caches, in entries.
const PROCESS_CACHE_MAX_ENTRIES: usize = 65_536;

/// Split accumulated rows into the batches one queued item may carry.
///
/// A queued item is lost whole when the writer queue overflows, so the bound
/// decides the worst case loss of a single overflow. The tail keeps the
/// original allocation, so splitting costs no copy of the rows themselves.
fn into_bounded_batches<T>(mut rows: Vec<T>) -> Vec<Vec<T>> {
    if rows.is_empty() {
        return Vec::new();
    }
    let mut batches = Vec::new();
    while rows.len() > BULK_ITEM_MAX_ROWS {
        batches.push(rows.drain(..BULK_ITEM_MAX_ROWS).collect());
    }
    batches.push(rows);
    batches
}

/// Upper bound on the plaintext one stream retains for protocol parsing.
///
/// The retained window feeds derived protocol actions only: captured records are
/// persisted independently, so bytes dropped here shorten no stored payload. The
/// bound exists because a stream whose messages never complete -- an open SSE
/// response, a protocol the parsers do not understand -- would otherwise retain
/// every byte captured on that connection for as long as it lives.
const STREAM_PLAINTEXT_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Plaintext retained per stream for the protocol parsers.
#[derive(Default)]
struct StreamPlaintext {
    /// Bytes no parser has consumed yet.
    tail: Vec<u8>,
    /// View handed to the parsers, reused so a record costs no allocation.
    view: Vec<u8>,
    /// HTTP/2 HPACK tables are independent for each connection direction.
    http2_inbound: semantic_action_runtime::Http2ConnectionAssembler,
    http2_outbound: semantic_action_runtime::Http2ConnectionAssembler,
    /// Whether the cap has already been reported for the current episode.
    cap_reported: bool,
}

/// Whether these bytes could complete a message for one of the parsers.
///
/// Cheap check over the newly arrived bytes only; it decides whether the
/// retained stream is worth re-scanning at all.
fn completes_message_hint(bytes: &[u8]) -> bool {
    bytes.windows(2).any(|window| window == b"\n\n")
        || bytes.windows(4).any(|window| window == b"\r\n\r\n")
        || bytes.contains(&b'}')
}

/// Whether the retained bytes end like a finished structured message.
fn tail_ends_message(tail: &[u8]) -> bool {
    matches!(
        tail.iter().rev().find(|byte| !byte.is_ascii_whitespace()),
        Some(b'}') | Some(b']')
    )
}

/// Drop `consumed` bytes from the front of the retained window.
///
/// A recognized message can end inside a call that is still in flight, in which
/// case the consumed prefix reaches past the retained tail into that call's
/// buffer; leaving those bytes behind would let the same message be recognized
/// again for every later record of the stream.
fn consume_window(tail: &mut Vec<u8>, call: Option<&mut Vec<u8>>, consumed: usize) {
    if consumed <= tail.len() {
        tail.drain(..consumed);
        return;
    }
    let mut rest = consumed - tail.len();
    tail.clear();
    if let Some(call) = call {
        rest = rest.min(call.len());
        call.drain(..rest);
    }
}

/// Offset just past the first HTTP header block in `bytes`.
fn header_block_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

/// Offset just past the last complete event in `bytes`.
///
/// Event boundaries are blank lines, so a prefix ending there holds only events
/// the parsers have already seen.
fn last_event_boundary(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(2)
        .rposition(|window| window == b"\n\n")
        .map(|position| position + 2)
}

/// How often the loss ledger is published while a capture runs.
const LOSS_LEDGER_INTERVAL: Duration = Duration::from_secs(15);

/// Lost records of one stage, and the bytes they held where that is known.
#[derive(Clone, Copy, Default, Debug, Eq, PartialEq)]
struct LossCounts {
    count: u64,
    bytes: u64,
}

/// Where captured data was lost, by stage.
///
/// Stage names are `<category>.<stage>`, and the category is what makes the
/// ledger readable as a priority list: `data` never reached storage, `derived`
/// only lost a protocol action whose plaintext is still stored, and
/// `attribution` only lost a call association.
///
/// A stage either accumulates (`add`) or mirrors a counter that already totals
/// everything lost so far (`set`); using both for one stage would double count,
/// so each stage picks one.
#[derive(Default)]
struct LossLedger {
    totals: BTreeMap<String, LossCounts>,
    reported: BTreeMap<String, LossCounts>,
    last_report: Option<Instant>,
}

impl LossLedger {
    /// Accumulate loss that is observed one record at a time.
    fn add(&mut self, stage: &str, count: u64, bytes: u64) {
        if count == 0 && bytes == 0 {
            return;
        }
        let entry = self.totals.entry(stage.to_string()).or_default();
        entry.count = entry.count.saturating_add(count);
        entry.bytes = entry.bytes.saturating_add(bytes);
    }

    /// Mirror a counter that already totals everything lost so far.
    fn set(&mut self, stage: &str, count: u64, bytes: u64) {
        let entry = self.totals.entry(stage.to_string()).or_default();
        entry.count = count;
        entry.bytes = bytes;
    }

    /// Growth per stage since the last report, with the cumulative total.
    fn pending(&self) -> Vec<(String, LossCounts, LossCounts)> {
        self.totals
            .iter()
            .filter_map(|(stage, totals)| {
                let reported = self.reported.get(stage).copied().unwrap_or_default();
                (totals.count > reported.count).then(|| {
                    (
                        stage.clone(),
                        LossCounts {
                            count: totals.count - reported.count,
                            bytes: totals.bytes.saturating_sub(reported.bytes),
                        },
                        *totals,
                    )
                })
            })
            .collect()
    }

    fn due(&self, interval: Duration) -> bool {
        self.last_report
            .is_none_or(|last| last.elapsed() >= interval)
    }

    /// Mark what has been published, so the next report only carries new loss.
    fn note_report(&mut self, published: &[(String, LossCounts, LossCounts)]) {
        for (stage, _, totals) in published {
            self.reported.insert(stage.clone(), *totals);
        }
        self.last_report = Some(Instant::now());
    }
}

/// Kernel-supplied generation that pins one process lifetime.
///
/// The kernel stamps collected events with the process start time, which
/// changes when a PID is reused. An observation that carries no generation
/// cannot be told apart from a later process reusing the same PID, so it is
/// never cached.
fn process_generation(observation: &ProcessObservation) -> Option<u64> {
    let host = observation.host.as_ref()?;
    let generation = host.start_boottime_ns.unwrap_or(host.start_time_ticks);
    (generation != 0).then_some(generation)
}

/// A persisted trace id may be re-rooted by a new root process unless the
/// daemon still holds the trace active *and* its root process is alive (that
/// would be a duplicate live owner). Dead or exited roots - including the
/// window before the exit lifecycle event is ingested - may rebind the same
/// trace id, which is what lets a dsh web restart reuse the id while the
/// daemon keeps running.
fn trace_reroot_allowed(active_or_draining: bool, root_alive: bool) -> bool {
    !(active_or_draining && root_alive)
}

fn control_error(stage: &'static str, error: impl std::fmt::Debug) -> ControlError {
    ControlError::new(stage, format!("{error:?}"))
}

#[cfg(test)]
mod test_harness {
    use super::*;
    use config_core::daemon::{EbpfEnabledMode, MemlockRlimit, TlsScanDebounce, WriterConfig};

    pub(super) fn test_host() -> (DaemonServiceHost, std::path::PathBuf) {
        test_host_with_profile(config_core::capture_profile::CaptureProfile::for_level(
            config_core::capture_profile::CaptureLevel::L3,
        ))
    }

    pub(super) fn test_host_with_profile(
        profile: config_core::capture_profile::CaptureProfile,
    ) -> (DaemonServiceHost, std::path::PathBuf) {
        let path = temp_db_path();
        let config = StorageConfig::sqlite(&path, 5_000);
        let host = DaemonServiceHost::build(
            &config,
            WriterConfig::default(),
            profile,
            config_core::daemon::EbpfCollectorConfig {
                enabled_mode: EbpfEnabledMode::False,
                enabled: false,
                memlock_rlimit: MemlockRlimit::Inherit,
                tracked_process_max_entries: 4096,
                pending_operation_max_entries: 4096,
                event_ring_buffer_max_bytes: 1024 * 1024,
                tls_dynamic_loading: false,
                tls_scan_debounce: TlsScanDebounce::None,
                tls_scan_interval_ms: 1000,
            },
            4,
            "DSH_CENSORSCOPE_SESSION_ID".to_string(),
        )
        .expect("build host");
        (host, path)
    }

    pub(super) fn temp_db_path() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "censorscope-host-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// Remove a test database together with its WAL sidecars.
    pub(super) fn remove_db(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }
}

#[cfg(test)]
mod writer_pipeline_tests {
    use super::test_harness::{remove_db, test_host, test_host_with_profile};
    use super::*;
    use control_contract::command::{CallEndCommand, CallStartCommand};
    use model_core::ids::RequestId;

    #[test]
    fn ipc_audit_enabled_follows_collection_level() {
        use config_core::capture_profile::CaptureLevel;
        for (level, enabled) in [
            (CaptureLevel::L1, false),
            (CaptureLevel::L2, false),
            (CaptureLevel::L3, true),
        ] {
            let (host, db) = test_host_with_profile(
                config_core::capture_profile::CaptureProfile::for_level(level),
            );
            assert_eq!(
                host.ipc_audit_enabled(),
                enabled,
                "level {level} ipc audit flag"
            );
            drop(host);
            remove_db(&db);
        }
    }

    #[test]
    fn trace_reroot_blocks_only_live_duplicate_owners() {
        assert!(!trace_reroot_allowed(true, true)); // active + alive root
        assert!(trace_reroot_allowed(true, false)); // active, root exited: dsh web restart rebind
        assert!(trace_reroot_allowed(false, true)); // terminal entry, pid reused
        assert!(trace_reroot_allowed(false, false));
    }

    #[test]
    fn call_pipeline_closes_span_and_executes_window_job_through_writer() {
        let (mut host, db) = test_host();
        let trace = TraceId::new(21);
        let reply = host
            .handle_command(
                None,
                ControlCommand::CallStart(CallStartCommand {
                    request_id: RequestId::new(1),
                    trace_id: trace,
                    session_id: Some("s1".to_string()),
                    call_id: "c1".to_string(),
                    host_pid: 9,
                    started_at: 5_000_000_000,
                }),
            )
            .expect("call start");
        assert!(matches!(reply, ControlReply::CallStarted));
        let reply = host
            .handle_command(
                None,
                ControlCommand::CallEnd(CallEndCommand {
                    request_id: RequestId::new(2),
                    trace_id: trace,
                    session_id: Some("s1".to_string()),
                    call_id: "c1".to_string(),
                    host_pid: 9,
                    ended_at: 20_000_000_000,
                    status: "success".to_string(),
                }),
            )
            .expect("call end");
        assert!(matches!(reply, ControlReply::CallEnded));
        // Give the writer an idle tick to commit the close and execute its job.
        std::thread::sleep(std::time::Duration::from_millis(60));
        host.shutdown().expect("host shutdown");
        drop(host);

        let mut storage = open_storage_backend(&StorageConfig::sqlite(&db, 5_000)).expect("reopen");
        let spans = storage.list_call_spans(trace).expect("list spans");
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0].ended_at,
            Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(20))
        );
        assert!(
            storage
                .list_pending_backfill_job_ids()
                .expect("pending ids")
                .is_empty()
        );
        assert!(storage.claim_backfill_jobs(10).expect("claim").is_empty());
        remove_db(&db);
    }

    /// Control-plane reference to this test process, in its own PID namespace.
    fn self_process_ref() -> control_contract::command::ProcessRef {
        use model_core::process::NamespaceIdentity;
        let namespace = std::fs::read_link("/proc/self/ns/pid").expect("pid namespace link");
        let status = std::fs::read_to_string("/proc/self/status").expect("process status");
        let namespace_pid = status
            .lines()
            .find_map(|line| {
                line.strip_prefix("NSpid:")
                    .and_then(|value| value.split_whitespace().last())
                    .and_then(|value| value.parse::<u32>().ok())
            })
            .expect("NSpid field");
        control_contract::command::ProcessRef::new(
            namespace_pid,
            NamespaceIdentity::new(namespace.display().to_string()),
        )
    }

    fn track_add_command(
        request_id: RequestId,
        trace_id: Option<TraceId>,
    ) -> control_contract::command::TrackAddCommand {
        use model_core::ids::{ProfileName, TraceName};
        control_contract::command::TrackAddCommand {
            request_id,
            root: self_process_ref(),
            display_name: TraceName::new("sync-track-add"),
            profile_name: ProfileName::new("test"),
            tags: BTreeSet::new(),
            trace_id,
        }
    }

    /// One `track-add` request must answer with the terminal state: the trace
    /// becomes active (or the request fails) inside the call, so callers never
    /// poll `operation-status` to learn whether observation started.
    #[test]
    fn track_add_reply_carries_the_terminal_state() {
        let (mut host, db) = test_host();
        let available_collectors = host
            .runtime
            .list_trace_records()
            .into_iter()
            .filter(|trace| trace.lifecycle_state.is_active_or_draining())
            .count();
        assert_eq!(available_collectors, 0, "no trace before track-add");

        let reply = host
            .handle_command(
                None,
                ControlCommand::TrackAdd(track_add_command(RequestId::new(1), None)),
            )
            .expect("track-add succeeds inline");
        let ControlReply::TrackAdded(reply) = reply else {
            panic!("track-add must answer with a track reply");
        };
        assert_eq!(reply.operation_state, OperationState::Active);
        assert!(reply.lifecycle_state.is_active_or_draining());
        let trace_id = reply.trace_id.expect("active track-add carries a trace id");
        assert!(reply.error.is_none());
        // Nothing is left pending for a later drain tick: the drain that used
        // to advance the operation can no longer change any state.
        host.drain_live_events().expect("live drain");
        let status = host
            .handle_command(
                None,
                ControlCommand::OperationStatus(OperationStatusCommand {
                    request_id: RequestId::new(2),
                    operation_id: RequestId::new(1),
                }),
            )
            .expect("operation status");
        let ControlReply::OperationStatus(status) = status else {
            panic!("operation status reply");
        };
        assert_eq!(status.operation_state, OperationState::Active);
        assert_eq!(status.trace_id, Some(trace_id));

        // A rejected track-add is an error reply that carries the daemon's own
        // reason, not a `preparing` reply whose failure has to be discovered
        // from a later status read. Continuing a trace whose root is still
        // alive is the reachable rejection for a plugin restart.
        let error = host
            .handle_command(
                None,
                ControlCommand::TrackAdd(track_add_command(RequestId::new(3), Some(trace_id))),
            )
            .expect_err("continuing a live trace is rejected");
        assert_eq!(error.code, "trace_id");
        assert!(
            error.message.contains("already active"),
            "reason is carried in the error: {}",
            error.message
        );
        let status = host
            .handle_command(
                None,
                ControlCommand::OperationStatus(OperationStatusCommand {
                    request_id: RequestId::new(4),
                    operation_id: RequestId::new(3),
                }),
            )
            .expect("failed operation status");
        let ControlReply::OperationStatus(status) = status else {
            panic!("operation status reply");
        };
        assert_eq!(status.operation_state, OperationState::Failed);
        assert!(
            status
                .error
                .as_deref()
                .is_some_and(|error| error.contains("already active")),
            "failed operation records its reason: {:?}",
            status.error
        );

        host.shutdown().expect("host shutdown");
        drop(host);
        remove_db(&db);
    }
}

/// Opt-in measurement of the userspace ingest path.
///
/// The eBPF collector needs CAP_BPF and a writable tracefs, so this harness
/// drives the ingest entry points with synthetic observations instead. It
/// attributes the daemon's per-item cost (identity resolution, session lookup,
/// semantic projection, queue hand-off) without depending on what the kernel
/// happens to produce, which is what makes an ingest change measurable on a
/// host that cannot attach probes.
///
/// The synthetic items carry no kernel session, so every item has to consult
/// the process environment: that is the path this benchmark is expected to
/// move. Items are observed for the test process itself, whose `/proc` entries
/// are guaranteed to exist and be readable.
///
/// Run with:
///
/// ```text
/// cargo test --release -p daemon -- ingest_ --ignored --nocapture
/// ```
#[cfg(test)]
mod ingest_bench {
    use super::test_harness::{remove_db, test_host};
    use super::*;
    use collector_event::{RawEventEnvelope, RawObservationPayload, RawPayloadSegment};
    use model_core::ids::CollectorName;
    use model_core::payload::{PayloadContentState, PayloadDirection, PayloadSourceBoundary};
    use std::collections::BTreeMap;
    use std::time::Instant;

    /// Items per measured run: large enough to swamp start-up noise, small
    /// enough to keep the run in the seconds range.
    const ITEMS: usize = 20_000;
    /// Payload items are heavier per item (a full plaintext chunk plus protocol
    /// projection), so a shorter run keeps the pre-built batch and the
    /// per-stream reassembly buffers in a sane memory range.
    const PAYLOAD_ITEMS: usize = 10_000;
    /// Hand-off cadence. Mirrors a drain cycle so the accumulator is bounded
    /// during the run instead of growing with the item count.
    const FLUSH_EVERY: usize = 512;
    const PAYLOAD_BYTES: usize = 4096;

    struct BenchHost {
        host: DaemonServiceHost,
        db: std::path::PathBuf,
        trace_id: TraceId,
        observation: ProcessObservation,
    }

    impl BenchHost {
        fn new() -> Self {
            let (mut host, db) = test_host();
            let observation = ProcfsIdentityReader
                .read_identity(std::process::id())
                .expect("identity of the test process");
            let (root_identity, _) = host
                .resolve_process(observation.clone())
                .expect("root identity");
            let trace_id = host.runtime.reserve_trace_id();
            let profile_snapshot =
                CaptureProfileSnapshot::from_profile(&host.profile, SystemTime::now());
            let sensor_plan = host
                .runtime
                .negotiate(&profile_snapshot)
                .expect("sensor plan");
            host.runtime
                .create_starting_trace(
                    trace_id,
                    TrackTraceRequest {
                        root_identity,
                        root_pid_namespace: observation
                            .namespace
                            .as_ref()
                            .map(|value| value.pid_namespace.clone()),
                        root_container_id: None,
                        root_working_directory: None,
                        display_name: model_core::ids::TraceName::new("ingest-bench"),
                        profile_snapshot,
                        tags: BTreeSet::new(),
                        created_at: SystemTime::now(),
                    },
                    sensor_plan,
                )
                .expect("create trace");
            Self {
                host,
                db,
                trace_id,
                observation,
            }
        }

        fn envelope(&self) -> RawEventEnvelope {
            RawEventEnvelope {
                trace_id: Some(self.trace_id),
                observed_at: SystemTime::now(),
                process: self.observation.clone(),
                collector: CollectorName::new("ebpf"),
                session_id: None,
                call_id: None,
            }
        }

        fn file_event(&self) -> collector_event::RawCollectorEvent {
            collector_event::RawCollectorEvent {
                envelope: self.envelope(),
                payload: RawObservationPayload::File {
                    operation: "read".to_string(),
                    path: Some("/tmp/ingest-bench".to_string()),
                    fd: Some(3),
                    size: Some(PAYLOAD_BYTES as u64),
                    metadata: BTreeMap::new(),
                },
            }
        }

        fn payload_segment(&self, sequence: u64) -> RawPayloadSegment {
            RawPayloadSegment {
                envelope: self.envelope(),
                source: PayloadSourceBoundary::Uprobe,
                content_state: PayloadContentState::Complete,
                direction: PayloadDirection::Outbound,
                // A fresh stream per item keeps plaintext reassembly buffers
                // from accumulating over the run.
                stream_key: Some(format!("bench:{sequence}")),
                sequence,
                operation_id: Some(sequence),
                offset: Some(0),
                completed: true,
                original_size: PAYLOAD_BYTES as u64,
                captured_size: PAYLOAD_BYTES as u64,
                library: Some("openssl".to_string()),
                symbol: Some("SSL_write".to_string()),
                protocol_hint: Some("tls".to_string()),
                loss_reason: None,
                bytes: Some(vec![b'a'; PAYLOAD_BYTES]),
            }
        }

        fn finish(self) {
            let mut host = self.host;
            host.shutdown().expect("host shutdown");
            drop(host);
            remove_db(&self.db);
        }
    }

    fn report(label: &str, items: usize, elapsed: std::time::Duration) {
        let per_item_us = elapsed.as_secs_f64() * 1_000_000.0 / items as f64;
        let per_second = items as f64 / elapsed.as_secs_f64();
        println!(
            "{label}: {items} items in {:.3}s -> {per_item_us:.2} us/item, {per_second:.0} items/s",
            elapsed.as_secs_f64()
        );
    }

    #[test]
    #[ignore = "opt-in ingest benchmark; run with --ignored --nocapture"]
    fn ingest_event_throughput() {
        let mut bench = BenchHost::new();
        // Items are built before the timer starts so the measurement covers
        // ingest work only: the real caller hands over already-decoded items.
        let items: Vec<_> = (0..ITEMS).map(|_| bench.file_event()).collect();
        let started = Instant::now();
        for (index, event) in items.into_iter().enumerate() {
            bench.host.apply_event(event).expect("apply event");
            if index % FLUSH_EVERY == 0 {
                bench.host.flush_pending_storage();
            }
        }
        let elapsed = started.elapsed();
        bench.host.flush_pending_storage();
        report("ingest event", ITEMS, elapsed);
        bench.finish();
    }

    #[test]
    #[ignore = "opt-in ingest benchmark; run with --ignored --nocapture"]
    fn ingest_payload_throughput() {
        let mut bench = BenchHost::new();
        let items: Vec<_> = (0..PAYLOAD_ITEMS)
            .map(|sequence| bench.payload_segment(sequence as u64))
            .collect();
        let started = Instant::now();
        for (index, segment) in items.into_iter().enumerate() {
            bench
                .host
                .apply_payload_segment(segment)
                .expect("apply payload");
            if index % FLUSH_EVERY == 0 {
                bench.host.flush_pending_storage();
            }
        }
        let elapsed = started.elapsed();
        bench.host.flush_pending_storage();
        report("ingest payload segment", PAYLOAD_ITEMS, elapsed);
        bench.finish();
    }
}

/// Batching of the rows one writer-queue item carries.
///
/// The bound is what keeps a single queue overflow from discarding a whole
/// drain cycle's worth of captured plaintext.
#[cfg(test)]
mod bulk_batch_tests {
    use super::*;

    #[test]
    fn rows_split_at_the_item_bound() {
        let batches = into_bounded_batches((0..BULK_ITEM_MAX_ROWS as u32).collect::<Vec<u32>>());
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), BULK_ITEM_MAX_ROWS);
    }

    #[test]
    fn one_row_past_the_bound_becomes_a_second_item() {
        let batches =
            into_bounded_batches((0..BULK_ITEM_MAX_ROWS as u32 + 1).collect::<Vec<u32>>());
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].len(), BULK_ITEM_MAX_ROWS);
        assert_eq!(batches[1].len(), 1);
        // The split preserves order, so per-event row ordering survives it.
        assert_eq!(batches[0][0], 0);
        assert_eq!(batches[1][0], BULK_ITEM_MAX_ROWS as u32);
    }

    #[test]
    fn a_large_drain_splits_into_full_leading_batches() {
        let rows: Vec<u32> = (0..BULK_ITEM_MAX_ROWS * 2 + 1).map(|v| v as u32).collect();
        let batches = into_bounded_batches(rows);
        assert_eq!(batches.len(), 3);
        assert_eq!(
            batches.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![BULK_ITEM_MAX_ROWS, BULK_ITEM_MAX_ROWS, 1]
        );
    }

    #[test]
    fn no_rows_queue_no_item() {
        let batches = into_bounded_batches(Vec::<u32>::new());
        assert!(batches.is_empty());
    }
}

/// Lifecycle writes are queued on transitions rather than on every event.
#[cfg(test)]
mod lifecycle_write_tests {
    use super::test_harness::{remove_db, test_host};
    use super::*;
    use collector_event::{RawCollectorEvent, RawEventEnvelope, RawObservationPayload};
    use model_core::ids::{CollectorName, TraceId};
    use std::collections::BTreeMap;

    fn event(trace_id: TraceId, pid: u32) -> RawCollectorEvent {
        RawCollectorEvent {
            envelope: RawEventEnvelope {
                trace_id: Some(trace_id),
                observed_at: SystemTime::now(),
                process: ProcessObservation::host(process_identity::HostProcessCoordinates::new(
                    pid, 4242,
                )),
                collector: CollectorName::new("test"),
                session_id: Some(SessionIdentity::new("s1")),
                call_id: Some("call-1".to_string()),
            },
            payload: RawObservationPayload::File {
                operation: "read".to_string(),
                path: Some("/tmp/lifecycle".to_string()),
                fd: Some(3),
                size: Some(1),
                metadata: BTreeMap::new(),
            },
        }
    }

    #[test]
    fn repeated_events_do_not_rewrite_the_same_state() {
        let (mut host, db) = test_host();
        let trace_id = host.runtime.reserve_trace_id();
        let observation = ProcfsIdentityReader
            .read_identity(std::process::id())
            .expect("identity of the test process");
        let (root_identity, _) = host.resolve_process(observation).expect("root identity");
        let profile_snapshot =
            CaptureProfileSnapshot::from_profile(&host.profile, SystemTime::now());
        let sensor_plan = host
            .runtime
            .negotiate(&profile_snapshot)
            .expect("sensor plan");
        host.runtime
            .create_starting_trace(
                trace_id,
                TrackTraceRequest {
                    root_identity,
                    root_pid_namespace: None,
                    root_container_id: None,
                    root_working_directory: None,
                    display_name: model_core::ids::TraceName::new("lifecycle-test"),
                    profile_snapshot,
                    tags: BTreeSet::new(),
                    created_at: SystemTime::now(),
                },
                sensor_plan,
            )
            .expect("create trace");

        let state = host
            .runtime
            .get_trace(trace_id)
            .expect("trace")
            .trace
            .lifecycle_state;
        host.apply_event(event(trace_id, std::process::id()))
            .expect("first event");
        assert_eq!(host.trace_lifecycle_written.get(&trace_id), Some(&state));
        let after_first = host.trace_lifecycle_written.clone();
        host.apply_event(event(trace_id, std::process::id()))
            .expect("second event");
        assert_eq!(
            host.trace_lifecycle_written, after_first,
            "an unchanged state must not be queued again"
        );
        drop(host);
        remove_db(&db);
    }
}

/// Per-process lookup caching at the ingest boundary.
///
/// The caches exist so that `/proc` reads that cannot change within a process
/// lifetime happen once per lifetime instead of once per item. These tests
/// drive the caches with sentinel entries: a value that could not have come
/// from `/proc` proves which source the lookup actually used, and therefore
/// whether the cache was consulted at all.
#[cfg(test)]
mod ingest_cache_tests {
    use super::test_harness::{remove_db, test_host};
    use super::*;
    use process_identity::HostProcessCoordinates;

    fn observation(pid: u32, generation: u64) -> ProcessObservation {
        ProcessObservation::host(
            HostProcessCoordinates::new(pid, generation).with_start_boottime_ns(generation),
        )
    }

    fn sentinel(pid: u32) -> ProcessObservation {
        ProcessObservation::host(HostProcessCoordinates::new(pid, 0).with_task_id(pid))
    }

    #[test]
    fn identity_lookup_is_reused_within_one_generation() {
        let (mut host, db) = test_host();
        let observed = observation(4242, 11);
        host.process_identities.insert((4242, 11), sentinel(4242));
        assert_eq!(host.resolve_observation(observed), sentinel(4242));
        drop(host);
        remove_db(&db);
    }

    #[test]
    fn identity_lookup_misses_when_the_generation_changes() {
        let (mut host, db) = test_host();
        let pid = std::process::id();
        // A reused PID arrives with a new kernel generation, so the entry left
        // by the previous process must not be served. The observed process is
        // this test process, whose `/proc` entries are readable.
        host.process_identities.insert((pid, 11), sentinel(pid));
        let resolved = host.resolve_observation(observation(pid, 12));
        assert_ne!(resolved, sentinel(pid));
        assert!(
            host.process_identities.contains_key(&(pid, 12)),
            "the new generation has to be looked up and cached on its own"
        );
        drop(host);
        remove_db(&db);
    }

    #[test]
    fn a_failed_lookup_is_not_cached() {
        let (mut host, db) = test_host();
        // Nothing can be read for a PID that does not exist. Caching the
        // unenriched fallback would pin an observation that cannot distinguish a
        // reused PID for the rest of that process lifetime.
        let missing = u32::MAX - 1;
        let observed = observation(missing, 11);
        assert_eq!(host.resolve_observation(observed.clone()), observed);
        assert!(host.process_identities.is_empty());
        drop(host);
        remove_db(&db);
    }

    #[test]
    fn observations_without_a_generation_are_never_cached() {
        let (mut host, db) = test_host();
        let pid = std::process::id();
        let anonymous = ProcessObservation::host(HostProcessCoordinates::new(pid, 0));
        assert_eq!(process_generation(&anonymous), None);
        let _ = host.resolve_observation(anonymous);
        assert!(
            host.process_identities.is_empty(),
            "an observation that cannot pin a process lifetime must not be cached"
        );
        drop(host);
        remove_db(&db);
    }

    #[test]
    fn session_lookup_reuses_a_recorded_result_and_records_a_missing_one() {
        let (mut host, db) = test_host();
        let recorded = SessionIdentity::new("sentinel");
        host.session_lookups
            .insert((4242, 11), Some(recorded.clone()));
        assert_eq!(host.session_for_process(4242, Some(11)), Some(recorded));

        // A process that carries no session variable is the common case, and
        // its negative result is what keeps the environment from being re-read
        // for every one of its items.
        let pid = std::process::id();
        let generation = 7;
        let _ = host.session_for_process(pid, Some(generation));
        assert!(host.session_lookups.contains_key(&(pid, generation)));
        drop(host);
        remove_db(&db);
    }

    #[test]
    fn kernel_session_short_circuits_the_environment_lookup() {
        let (mut host, db) = test_host();
        let mut envelope = collector_event::RawEventEnvelope {
            trace_id: Some(TraceId::new(1)),
            observed_at: SystemTime::now(),
            process: observation(4242, 11),
            collector: model_core::ids::CollectorName::new("test"),
            session_id: Some(SessionIdentity::new("from-kernel")),
            call_id: Some("call-1".to_string()),
        };
        host.session_lookups
            .insert((4242, 11), Some(SessionIdentity::new("from-procfs")));
        let resolution = host.resolve_item_session(TraceId::new(1), Some(4242), &mut envelope);
        assert_eq!(
            resolution.effective(),
            Some(SessionIdentity::new("from-kernel"))
        );
        assert_eq!(resolution.source(), "ebpf");
        drop(host);
        remove_db(&db);
    }

    #[test]
    fn environment_lookup_still_runs_without_a_kernel_session() {
        let (mut host, db) = test_host();
        // A PID that cannot exist leaves nothing to recover, so this asserts the
        // fallback is still attempted rather than skipped.
        let mut envelope = collector_event::RawEventEnvelope {
            trace_id: Some(TraceId::new(1)),
            observed_at: SystemTime::now(),
            process: observation(u32::MAX, 11),
            collector: model_core::ids::CollectorName::new("test"),
            session_id: None,
            call_id: Some("call-1".to_string()),
        };
        let resolution = host.resolve_item_session(TraceId::new(1), Some(u32::MAX), &mut envelope);
        assert_eq!(resolution.effective(), None);
        assert_eq!(resolution.source(), "none");
        drop(host);
        remove_db(&db);
    }

    #[test]
    fn exit_releases_the_cached_lookups_of_that_process() {
        let (mut host, db) = test_host();
        host.process_identities.insert((4242, 11), sentinel(4242));
        host.process_identities.insert((4243, 11), sentinel(4243));
        host.session_lookups
            .insert((4242, 11), Some(SessionIdentity::new("s")));
        host.forget_process_caches(Some(4242));
        assert!(!host.process_identities.contains_key(&(4242, 11)));
        assert!(host.session_lookups.is_empty());
        assert!(
            host.process_identities.contains_key(&(4243, 11)),
            "only the exited process is released"
        );
        drop(host);
        remove_db(&db);
    }

    #[test]
    fn reaching_the_bound_drops_every_cached_lookup() {
        let (mut host, db) = test_host();
        for pid in 0..PROCESS_CACHE_MAX_ENTRIES as u32 {
            host.process_identities.insert((pid, 1), sentinel(pid));
        }
        host.session_lookups
            .insert((1, 1), Some(SessionIdentity::new("s")));
        host.evict_process_caches_if_full();
        assert!(host.process_identities.is_empty());
        assert!(
            host.session_lookups.is_empty(),
            "both caches share one key space and one lifetime"
        );
        drop(host);
        remove_db(&db);
    }

    #[test]
    fn generation_prefers_the_kernel_stamp_over_procfs_ticks() {
        let stamped = ProcessObservation::host(
            HostProcessCoordinates::new(4242, 999).with_start_boottime_ns(11),
        );
        assert_eq!(process_generation(&stamped), Some(11));
        let ticks_only = ProcessObservation::host(HostProcessCoordinates::new(4242, 999));
        assert_eq!(process_generation(&ticks_only), Some(999));
        let unknown = ProcessObservation::host(HostProcessCoordinates::new(4242, 0));
        assert_eq!(process_generation(&unknown), None);
    }
}

/// Reporting contract of the loss ledger.
///
/// The ledger is what a test run is read from, so its bookkeeping is pinned:
/// growth is reported once per stage, cumulative totals stay monotonic, and a
/// cycle that cannot publish yet keeps the growth for the next one.
#[cfg(test)]
mod loss_ledger_tests {
    use super::*;

    #[test]
    fn only_growth_is_reported_and_totals_stay_cumulative() {
        let mut ledger = LossLedger::default();
        ledger.add("data.unknown_trace", 3, 0);
        let first = ledger.pending();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0, "data.unknown_trace");
        assert_eq!(first[0].1, LossCounts { count: 3, bytes: 0 });
        assert_eq!(first[0].2, LossCounts { count: 3, bytes: 0 });

        ledger.note_report(&first);
        assert!(ledger.pending().is_empty(), "nothing new to report yet");

        ledger.add("data.unknown_trace", 2, 0);
        let second = ledger.pending();
        assert_eq!(second[0].1, LossCounts { count: 2, bytes: 0 });
        assert_eq!(
            second[0].2,
            LossCounts { count: 5, bytes: 0 },
            "the total keeps the whole run"
        );
    }

    #[test]
    fn a_mirrored_counter_reports_the_same_growth() {
        let mut ledger = LossLedger::default();
        // A monotonic writer counter: the ledger mirrors it rather than adding.
        ledger.set("data.queue_overflow", 4, 4096);
        let first = ledger.pending();
        assert_eq!(
            first[0].1,
            LossCounts {
                count: 4,
                bytes: 4096
            }
        );
        ledger.note_report(&first);

        ledger.set("data.queue_overflow", 4, 4096);
        assert!(
            ledger.pending().is_empty(),
            "an unchanged counter is not new loss"
        );
        ledger.set("data.queue_overflow", 6, 6144);
        let third = ledger.pending();
        assert_eq!(
            third[0].1,
            LossCounts {
                count: 2,
                bytes: 2048
            }
        );
    }

    #[test]
    fn stages_are_reported_independently() {
        let mut ledger = LossLedger::default();
        ledger.add("data.payload_bytes_unavailable", 5, 20480);
        ledger.add("derived.parse_window_truncated", 1, 4096);
        let pending = ledger.pending();
        let stages: Vec<&str> = pending.iter().map(|(stage, _, _)| stage.as_str()).collect();
        assert_eq!(
            stages,
            vec![
                "data.payload_bytes_unavailable",
                "derived.parse_window_truncated"
            ],
            "one entry per stage, so the database can group by stage"
        );
    }

    #[test]
    fn growth_is_kept_when_there_is_no_trace_to_record_it_on() {
        let (mut host, db) = super::test_harness::test_host();
        host.loss_ledger.add("data.unknown_trace", 7, 0);
        host.report_loss_ledger(true);
        assert_eq!(
            host.loss_ledger.pending().len(),
            1,
            "a stage that could not be published stays pending"
        );
        drop(host);
        super::test_harness::remove_db(&db);
    }

    #[test]
    fn the_report_interval_gates_publication() {
        let mut ledger = LossLedger::default();
        assert!(ledger.due(LOSS_LEDGER_INTERVAL), "first report is due");
        ledger.note_report(&[]);
        assert!(!ledger.due(LOSS_LEDGER_INTERVAL));
        assert!(ledger.due(Duration::ZERO), "an elapsed interval is due");
    }
}

/// Plaintext retention for the protocol parsers.
///
/// Two properties matter for a capture that runs for hours: a stream whose
/// messages never complete must not retain unbounded plaintext, and a message
/// that has already been recognized must not be recognized again for every
/// later record of the same stream.
#[cfg(test)]
mod plaintext_window_tests {
    use super::test_harness::{remove_db, test_host};
    use super::*;
    use collector_event::{RawEventEnvelope, RawPayloadSegment};
    use model_core::ids::CollectorName;
    use model_core::payload::{PayloadContentState, PayloadDirection, PayloadSourceBoundary};

    struct Fixture {
        host: DaemonServiceHost,
        db: std::path::PathBuf,
        trace_id: TraceId,
        observation: ProcessObservation,
    }

    impl Fixture {
        fn new() -> Self {
            let (mut host, db) = test_host();
            let observation = ProcfsIdentityReader
                .read_identity(std::process::id())
                .expect("identity of the test process");
            let (root, _) = host.resolve_process(observation.clone()).expect("root");
            let trace_id = host.runtime.reserve_trace_id();
            let snapshot = CaptureProfileSnapshot::from_profile(&host.profile, SystemTime::now());
            let plan = host.runtime.negotiate(&snapshot).expect("sensor plan");
            host.runtime
                .create_starting_trace(
                    trace_id,
                    TrackTraceRequest {
                        root_identity: root,
                        root_pid_namespace: None,
                        root_container_id: None,
                        root_working_directory: None,
                        display_name: model_core::ids::TraceName::new("plaintext-window"),
                        profile_snapshot: snapshot,
                        tags: BTreeSet::new(),
                        created_at: SystemTime::now(),
                    },
                    plan,
                )
                .expect("create trace");
            Self {
                host,
                db,
                trace_id,
                observation,
            }
        }

        /// Feed one record as its own completed TLS call.
        fn feed(&mut self, stream: &str, sequence: u64, chunk_offset: u64, bytes: &[u8]) {
            self.feed_call_record(stream, sequence, sequence, chunk_offset, bytes, true);
        }

        /// Feed one record of a TLS call identified by `operation_id`.
        fn feed_call_record(
            &mut self,
            stream: &str,
            operation_id: u64,
            sequence: u64,
            chunk_offset: u64,
            bytes: &[u8],
            completed: bool,
        ) {
            let segment = RawPayloadSegment {
                envelope: RawEventEnvelope {
                    trace_id: Some(self.trace_id),
                    observed_at: SystemTime::now(),
                    process: self.observation.clone(),
                    collector: CollectorName::new("ebpf"),
                    session_id: Some(SessionIdentity::new("sess")),
                    call_id: Some("call".to_string()),
                },
                source: PayloadSourceBoundary::Uprobe,
                content_state: PayloadContentState::Complete,
                direction: PayloadDirection::Outbound,
                stream_key: Some(stream.to_string()),
                sequence,
                operation_id: Some(operation_id),
                offset: Some(chunk_offset),
                completed,
                original_size: bytes.len() as u64,
                captured_size: bytes.len() as u64,
                library: Some("openssl".to_string()),
                symbol: Some("SSL_read".to_string()),
                protocol_hint: Some("http".to_string()),
                loss_reason: None,
                bytes: Some(bytes.to_vec()),
            };
            self.host
                .apply_payload_segment(segment)
                .expect("apply payload segment");
        }

        fn retained(&self, stream: &str) -> usize {
            self.tail(stream).len()
        }

        fn tail(&self, stream: &str) -> Vec<u8> {
            self.host
                .payload_reassembly
                .get(&(self.trace_id, stream.to_string()))
                .map(|state| state.tail.clone())
                .unwrap_or_default()
        }

        fn finish(self) -> std::path::PathBuf {
            let mut host = self.host;
            host.shutdown().expect("shutdown");
            drop(host);
            self.db
        }
    }

    #[test]
    fn a_stream_that_never_completes_is_capped() {
        let mut fixture = Fixture::new();
        let stream = "tls:1:1:1";
        // Bytes that can complete nothing: no blank line, no brace.
        let chunk = vec![b'x'; 64 * 1024];
        for sequence in 0..96 {
            fixture.feed(stream, sequence, 0, &chunk);
        }
        assert_eq!(
            fixture.retained(stream),
            STREAM_PLAINTEXT_MAX_BYTES,
            "retained plaintext must stop at the cap"
        );
        let db = fixture.finish();
        remove_db(&db);
    }

    #[test]
    fn a_recognized_message_is_not_re_derived_for_every_record() {
        let mut fixture = Fixture::new();
        let stream = "tls:2:2:1";
        let headers = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n";
        fixture.feed(stream, 0, 0, headers);
        // An open SSE response: the message never completes while the stream
        // stays open, so its header block is the only complete message.
        for sequence in 1..24 {
            fixture.feed(stream, sequence, 0, b"data: {\"delta\":\"tok\"}\n\n");
        }
        let tail = fixture.tail(stream);
        let db = fixture.finish();
        // The response headers were recognized once and consumed, so no later
        // record of this stream can derive the same response again.
        assert!(
            header_block_end(&tail).is_none(),
            "the recognized header block must be released, tail={:?}",
            String::from_utf8_lossy(&tail)
        );
        assert!(
            tail.len() < 64,
            "only an unfinished event may remain, tail={}",
            tail.len()
        );
        remove_db(&db);
    }

    #[test]
    fn a_message_ending_inside_an_open_call_is_consumed_too() {
        let mut fixture = Fixture::new();
        let stream = "tls:3:3:1";
        // One TLS call split into records, as the kernel emits them: the response
        // headers end in the middle of the call, before it is marked complete.
        let records: [&[u8]; 4] = [
            b"HTTP/1.1 200 OK\r\n",
            b"Content-Type: text/event-stream\r\n",
            b"\r\n",
            b"data: {\"delta\":\"tok\"}\n\n",
        ];
        let mut offset = 0u64;
        for (index, record) in records.iter().enumerate() {
            fixture.feed_call_record(
                stream,
                7,
                index as u64,
                offset,
                record,
                index == records.len() - 1,
            );
            offset += record.len() as u64;
        }
        let tail = fixture.tail(stream);
        let db = fixture.finish();
        assert!(
            header_block_end(&tail).is_none(),
            "headers that ended inside the open call must be released, tail={:?}",
            String::from_utf8_lossy(&tail)
        );
        remove_db(&db);
    }

    /// Opt-in measurement of the per-record cost of retaining plaintext.
    ///
    /// Two stream shapes matter: one the parsers can never recognize (their
    /// retained window grows to the cap) and one that recognizes a message per
    /// record (the window is consumed as it goes). Run with:
    ///
    /// ```text
    /// cargo test --release -p daemon -- per_record_plaintext_cost --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "opt-in measurement; run with --ignored --nocapture"]
    fn per_record_plaintext_cost() {
        // A stream the parsers cannot recognize at all: nothing completes.
        let mut fixture = Fixture::new();
        for round in 1..=24u64 {
            let started = std::time::Instant::now();
            for chunk in 0..64u64 {
                fixture.feed_call_record(
                    "tls:a:1",
                    round,
                    round * 64 + chunk,
                    chunk * 4096,
                    &[b'x'; 4096],
                    chunk == 63,
                );
            }
            let elapsed = started.elapsed();
            fixture.host.flush_pending_storage();
            println!(
                "unparseable round={round} retained={} us_per_chunk={:.2}",
                fixture.retained("tls:a:1"),
                elapsed.as_secs_f64() * 1e6 / 64.0
            );
        }
        let db = fixture.finish();
        remove_db(&db);

        // One SSE response followed by many records that carry single events.
        let mut fixture = Fixture::new();
        fixture.feed(
            "tls:b:1",
            0,
            0,
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n",
        );
        for round in 1..=24u64 {
            let started = std::time::Instant::now();
            for chunk in 0..64u64 {
                fixture.feed(
                    "tls:b:1",
                    round * 64 + chunk,
                    0,
                    b"data: {\"delta\":\"token\"}\n\n",
                );
            }
            let elapsed = started.elapsed();
            fixture.host.flush_pending_storage();
            println!(
                "sse round={round} retained={} us_per_chunk={:.2}",
                fixture.retained("tls:b:1"),
                elapsed.as_secs_f64() * 1e6 / 64.0
            );
        }
        let db = fixture.finish();
        remove_db(&db);
    }
}

#[cfg(test)]
mod llm_runtime_tests {
    use super::test_harness::{remove_db, test_host};
    use super::*;
    use collector_event::{RawEventEnvelope, RawPayloadSegment};
    use model_core::ids::CollectorName;
    use model_core::payload::{PayloadContentState, PayloadDirection, PayloadSourceBoundary};
    use rusqlite::Connection;

    fn h2_frame(frame_type: u8, flags: u8, stream: u32, body: &[u8]) -> Vec<u8> {
        let length = body.len();
        let mut bytes = vec![
            ((length >> 16) & 0xff) as u8,
            ((length >> 8) & 0xff) as u8,
            (length & 0xff) as u8,
            frame_type,
            flags,
            ((stream >> 24) & 0x7f) as u8,
            (stream >> 16) as u8,
            (stream >> 8) as u8,
            stream as u8,
        ];
        bytes.extend_from_slice(body);
        bytes
    }

    struct Fixture {
        host: DaemonServiceHost,
        db: std::path::PathBuf,
        trace_id: TraceId,
        observation: ProcessObservation,
    }

    impl Fixture {
        fn new() -> Self {
            let (mut host, db) = test_host();
            let observation = ProcfsIdentityReader
                .read_identity(std::process::id())
                .expect("identity of the test process");
            let (root, _) = host.resolve_process(observation.clone()).expect("root");
            let trace_id = host.runtime.reserve_trace_id();
            let snapshot = CaptureProfileSnapshot::from_profile(&host.profile, SystemTime::now());
            let plan = host.runtime.negotiate(&snapshot).expect("sensor plan");
            host.runtime
                .create_starting_trace(
                    trace_id,
                    TrackTraceRequest {
                        root_identity: root,
                        root_pid_namespace: None,
                        root_container_id: None,
                        root_working_directory: None,
                        display_name: model_core::ids::TraceName::new("llm-runtime"),
                        profile_snapshot: snapshot,
                        tags: BTreeSet::new(),
                        created_at: SystemTime::now(),
                    },
                    plan,
                )
                .expect("create trace");
            Self {
                host,
                db,
                trace_id,
                observation,
            }
        }

        fn feed(&mut self, direction: PayloadDirection, sequence: u64, bytes: &[u8]) {
            let direction_code = u8::from(direction == PayloadDirection::Inbound);
            self.host
                .apply_payload_segment(RawPayloadSegment {
                    envelope: RawEventEnvelope {
                        trace_id: Some(self.trace_id),
                        observed_at: SystemTime::now(),
                        process: self.observation.clone(),
                        collector: CollectorName::new("ebpf"),
                        session_id: Some(SessionIdentity::new("llm-session")),
                        call_id: Some("llm-call".to_string()),
                    },
                    source: PayloadSourceBoundary::Uprobe,
                    content_state: PayloadContentState::Complete,
                    direction,
                    stream_key: Some(format!("tls:123:456:{direction_code}")),
                    sequence,
                    operation_id: Some(sequence),
                    offset: Some(0),
                    completed: true,
                    original_size: bytes.len() as u64,
                    captured_size: bytes.len() as u64,
                    library: Some("openssl".to_string()),
                    symbol: Some(
                        match direction {
                            PayloadDirection::Outbound => "SSL_write",
                            _ => "SSL_read",
                        }
                        .to_string(),
                    ),
                    protocol_hint: Some("http".to_string()),
                    loss_reason: None,
                    bytes: Some(bytes.to_vec()),
                })
                .expect("apply payload");
        }

        fn feed_h2(&mut self, direction: PayloadDirection, sequence: u64, bytes: &[u8]) {
            let direction_code = u8::from(direction == PayloadDirection::Inbound);
            self.host
                .apply_payload_segment(RawPayloadSegment {
                    envelope: RawEventEnvelope {
                        trace_id: Some(self.trace_id),
                        observed_at: SystemTime::now(),
                        process: self.observation.clone(),
                        collector: CollectorName::new("ebpf"),
                        session_id: Some(SessionIdentity::new("h2-session")),
                        call_id: Some("h2-call".to_string()),
                    },
                    source: PayloadSourceBoundary::Uprobe,
                    content_state: PayloadContentState::Complete,
                    direction,
                    stream_key: Some(format!("tls:123:789:{direction_code}")),
                    sequence,
                    operation_id: Some(sequence),
                    offset: Some(0),
                    completed: true,
                    original_size: bytes.len() as u64,
                    captured_size: bytes.len() as u64,
                    library: Some("boringssl".to_string()),
                    symbol: Some("SSL_write".to_string()),
                    protocol_hint: Some("http2".to_string()),
                    loss_reason: None,
                    bytes: Some(bytes.to_vec()),
                })
                .expect("apply h2 payload");
        }

        fn feed_loss(&mut self, direction: PayloadDirection, sequence: u64, bytes: &[u8]) {
            let direction_code = u8::from(direction == PayloadDirection::Inbound);
            self.host
                .apply_payload_segment(RawPayloadSegment {
                    envelope: RawEventEnvelope {
                        trace_id: Some(self.trace_id),
                        observed_at: SystemTime::now(),
                        process: self.observation.clone(),
                        collector: CollectorName::new("ebpf"),
                        session_id: Some(SessionIdentity::new("llm-session")),
                        call_id: Some("llm-loss".to_string()),
                    },
                    source: PayloadSourceBoundary::Uprobe,
                    content_state: PayloadContentState::Loss,
                    direction,
                    stream_key: Some(format!("tls:123:456:{direction_code}")),
                    sequence,
                    operation_id: Some(sequence),
                    offset: Some(0),
                    completed: true,
                    original_size: (bytes.len() + 32) as u64,
                    captured_size: bytes.len() as u64,
                    library: Some("openssl".to_string()),
                    symbol: Some("SSL_write".to_string()),
                    protocol_hint: Some("http".to_string()),
                    loss_reason: Some("collector_loss".to_string()),
                    bytes: Some(bytes.to_vec()),
                })
                .expect("apply lost payload");
        }

        fn finish(mut self) -> (std::path::PathBuf, TraceId) {
            self.host.shutdown().expect("shutdown");
            drop(self.host);
            (self.db, self.trace_id)
        }
    }

    #[test]
    fn http_exchange_persists_request_response_call_and_links() {
        let mut fixture = Fixture::new();
        fixture.feed(
            PayloadDirection::Outbound,
            1,
            b"POST /v1/chat/completions HTTP/1.1\r\nContent-Length: 30\r\n\r\n{\"model\":\"test\",\"messages\":[]}",
        );
        fixture.feed(
            PayloadDirection::Inbound,
            2,
            b"HTTP/1.1 200 OK\r\nContent-Length: 25\r\n\r\n{\"choices\":[],\"usage\":{}}",
        );
        let (db, trace_id) = fixture.finish();
        let connection = Connection::open(&db).expect("open database");

        for kind in ["llm.request", "llm.response", "llm.call"] {
            let count: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM semantic_actions WHERE trace_id = ?1 AND kind_name = ?2",
                    (trace_id.get(), kind),
                    |row| row.get(0),
                )
                .expect("count semantic actions");
            assert_eq!(count, 1, "one {kind}");
        }
        let (status, completeness, closed): (i64, i64, bool) = connection
            .query_row(
                "SELECT status, completeness, end_time IS NOT NULL
                 FROM semantic_actions WHERE trace_id = ?1 AND kind_name = 'llm.call'",
                [trace_id.get()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("load llm call");
        assert_eq!(
            status,
            semantic_action_contract::SemanticActionStatus::Success as i64
        );
        assert_eq!(
            completeness,
            semantic_action_contract::SemanticActionCompleteness::Complete as i64
        );
        assert!(closed);

        let provider: String = connection
            .query_row(
                "SELECT attr_value FROM semantic_action_attributes a
                 JOIN semantic_actions s USING (trace_id, action_id)
                 WHERE a.trace_id = ?1 AND s.kind_name = 'llm.request'
                   AND a.attr_key = 'llm.provider_id'",
                [trace_id.get()],
                |row| row.get(0),
            )
            .expect("request provider");
        assert_eq!(provider, "openai");

        let mut statement = connection
            .prepare(
                "SELECT role_name FROM semantic_action_links
                 WHERE trace_id = ?1 ORDER BY role_name",
            )
            .expect("prepare links");
        let roles = statement
            .query_map([trace_id.get()], |row| row.get::<_, String>(0))
            .expect("query links")
            .collect::<Result<Vec<_>, _>>()
            .expect("read links");
        for role in [
            "llm_call.request",
            "llm_call.response",
            "llm_request.http_message",
            "llm_response.http_message",
        ] {
            assert!(roles.iter().any(|value| value == role), "missing {role}");
        }
        drop(statement);
        drop(connection);
        remove_db(&db);
    }

    #[test]
    fn split_sse_exchange_persists_one_terminal_response() {
        let mut fixture = Fixture::new();
        fixture.feed(
            PayloadDirection::Outbound,
            1,
            b"POST /v1/chat/completions HTTP/1.1\r\nContent-Length: 30\r\n\r\n{\"model\":\"test\",\"messages\":[]}",
        );
        fixture.feed(
            PayloadDirection::Inbound,
            2,
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"a\"},\"finish_reason\":null}]}\n\n",
        );
        fixture.feed(
            PayloadDirection::Inbound,
            3,
            b"data: {\"choices\":[{\"delta\":{\"content\":\"b\"},\"finish_reason\":null}]}\n\n",
        );
        fixture.feed(PayloadDirection::Inbound, 4, b"data: [DONE]\n\n");
        let (db, trace_id) = fixture.finish();
        let connection = Connection::open(&db).expect("open database");

        let (responses, calls): (i64, i64) = connection
            .query_row(
                "SELECT
                    SUM(kind_name = 'llm.response'),
                    SUM(kind_name = 'llm.call')
                 FROM semantic_actions WHERE trace_id = ?1",
                [trace_id.get()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("count streaming actions");
        assert_eq!((responses, calls), (1, 1));
        let chunk_count: String = connection
            .query_row(
                "SELECT attr_value FROM semantic_action_attributes a
                 JOIN semantic_actions s USING (trace_id, action_id)
                 WHERE a.trace_id = ?1 AND s.kind_name = 'llm.response'
                   AND a.attr_key = 'llm.response.chunk_count'",
                [trace_id.get()],
                |row| row.get(0),
            )
            .expect("streaming chunk count");
        assert_eq!(chunk_count, "3");
        let evidence_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM semantic_action_evidence e
                 JOIN semantic_actions s USING (trace_id, action_id)
                 WHERE e.trace_id = ?1 AND s.kind_name = 'llm.response'",
                [trace_id.get()],
                |row| row.get(0),
            )
            .expect("streaming evidence count");
        assert_eq!(evidence_count, 3);
        let call_status: i64 = connection
            .query_row(
                "SELECT status FROM semantic_actions
                 WHERE trace_id = ?1 AND kind_name = 'llm.call'",
                [trace_id.get()],
                |row| row.get(0),
            )
            .expect("streaming call status");
        assert_eq!(
            call_status,
            semantic_action_contract::SemanticActionStatus::Success as i64
        );
        drop(connection);
        remove_db(&db);
    }

    #[test]
    fn response_without_request_persists_orphan_diagnostic() {
        let mut fixture = Fixture::new();
        fixture.feed(
            PayloadDirection::Inbound,
            1,
            b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\n\r\n{\"choices\":[]}",
        );
        let (db, trace_id) = fixture.finish();
        let connection = Connection::open(&db).expect("open database");
        let association: String = connection
            .query_row(
                "SELECT attr_value FROM semantic_action_attributes a
                 JOIN semantic_actions s USING (trace_id, action_id)
                 WHERE a.trace_id = ?1 AND s.kind_name = 'llm.response'
                   AND a.attr_key = 'llm.association_state'",
                [trace_id.get()],
                |row| row.get(0),
            )
            .expect("orphan association");
        assert_eq!(association, "orphan");
        let diagnostic_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM diagnostics
                 WHERE trace_id = ?1 AND message = 'llm_response_orphan'",
                [trace_id.get()],
                |row| row.get(0),
            )
            .expect("orphan diagnostics");
        assert_eq!(diagnostic_count, 1);
        drop(connection);
        remove_db(&db);
    }

    #[test]
    fn correlated_http_error_closes_call_as_error() {
        let mut fixture = Fixture::new();
        fixture.feed(
            PayloadDirection::Outbound,
            1,
            b"POST /v1/chat/completions HTTP/1.1\r\nContent-Length: 30\r\n\r\n{\"model\":\"test\",\"messages\":[]}",
        );
        let body = br#"{"error":{"message":"rate limited","type":"rate_limit"}}"#;
        let response = format!(
            "HTTP/1.1 429 Too Many Requests\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).expect("JSON")
        );
        fixture.feed(PayloadDirection::Inbound, 2, response.as_bytes());
        let (db, trace_id) = fixture.finish();
        let connection = Connection::open(&db).expect("open database");
        let (call_status, response_status): (i64, i64) = connection
            .query_row(
                "SELECT
                    MAX(CASE WHEN kind_name = 'llm.call' THEN status END),
                    MAX(CASE WHEN kind_name = 'llm.response' THEN status END)
                 FROM semantic_actions WHERE trace_id = ?1",
                [trace_id.get()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("error statuses");
        let error = semantic_action_contract::SemanticActionStatus::Error as i64;
        assert_eq!((call_status, response_status), (error, error));
        drop(connection);
        remove_db(&db);
    }

    #[test]
    fn payload_loss_does_not_create_complete_llm_call_and_is_persisted() {
        let mut fixture = Fixture::new();
        fixture.feed_loss(
            PayloadDirection::Outbound,
            1,
            b"POST /v1/chat/completions HTTP/1.1\r\nContent-Length: 80\r\n\r\n{\"model\":\"lost\"",
        );
        let (db, trace_id) = fixture.finish();
        let connection = Connection::open(&db).expect("open database");
        let complete_calls: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM semantic_actions
                 WHERE trace_id = ?1 AND kind_name = 'llm.call' AND completeness = ?2",
                (
                    trace_id.get(),
                    semantic_action_contract::SemanticActionCompleteness::Complete as i64,
                ),
                |row| row.get(0),
            )
            .expect("complete calls");
        assert_eq!(complete_calls, 0);
        let diagnostics: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM diagnostics
                 WHERE trace_id = ?1 AND message = 'payload_loss'",
                [trace_id.get()],
                |row| row.get(0),
            )
            .expect("payload loss diagnostic");
        assert_eq!(diagnostics, 1);
        drop(connection);
        remove_db(&db);
    }

    #[test]
    fn response_loss_finalizes_open_call_as_partial_error() {
        let mut fixture = Fixture::new();
        fixture.feed(
            PayloadDirection::Outbound,
            1,
            b"POST /v1/chat/completions HTTP/1.1\r\nContent-Length: 30\r\n\r\n{\"model\":\"test\",\"messages\":[]}",
        );
        fixture.feed_loss(
            PayloadDirection::Inbound,
            2,
            b"HTTP/1.1 200 OK\r\nContent-Length: 40\r\n\r\n{\"choices\":",
        );
        let trace_id = fixture.trace_id;
        let finalized = fixture
            .host
            .llm_exchange
            .finalize_trace(trace_id)
            .into_iter()
            .map(PersistWrite::SemanticAction)
            .collect::<Vec<_>>();
        fixture.host.push_writes(finalized);
        let (db, trace_id) = fixture.finish();
        let connection = Connection::open(&db).expect("open database");
        let (status, completeness): (i64, i64) = connection
            .query_row(
                "SELECT status, completeness FROM semantic_actions
                 WHERE trace_id = ?1 AND kind_name = 'llm.call'",
                [trace_id.get()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("partial call");
        assert_eq!(
            status,
            semantic_action_contract::SemanticActionStatus::Error as i64
        );
        assert_eq!(
            completeness,
            semantic_action_contract::SemanticActionCompleteness::Partial as i64
        );
        let diagnostics: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM diagnostics
                 WHERE trace_id = ?1 AND message = 'payload_loss'",
                [trace_id.get()],
                |row| row.get(0),
            )
            .expect("loss diagnostic");
        assert_eq!(diagnostics, 1);
        drop(connection);
        remove_db(&db);
    }

    #[test]
    fn ambiguous_provider_match_is_persisted_as_diagnostic() {
        let mut fixture = Fixture::new();
        let ambiguous =
            b"HTTP/1.1 200 OK\r\nContent-Length: 30\r\n\r\n{\"choices\":[],\"candidates\":[]}";
        fixture.feed(PayloadDirection::Inbound, 1, ambiguous);
        fixture.feed(PayloadDirection::Inbound, 1, ambiguous);
        let (db, trace_id) = fixture.finish();
        let connection = Connection::open(&db).expect("open database");
        let diagnostics: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM diagnostics
                 WHERE trace_id = ?1 AND message LIKE 'llm_provider_ambiguous:%'",
                [trace_id.get()],
                |row| row.get(0),
            )
            .expect("ambiguity diagnostic");
        assert_eq!(diagnostics, 1);
        let responses: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM semantic_actions
                 WHERE trace_id = ?1 AND kind_name = 'llm.response'",
                [trace_id.get()],
                |row| row.get(0),
            )
            .expect("ambiguous response count");
        assert_eq!(responses, 0);
        drop(connection);
        remove_db(&db);
    }

    #[test]
    fn interleaved_http2_streams_persist_independent_calls() {
        let mut request_headers = vec![0x83, 0x04, b"/v1/chat/completions".len() as u8];
        request_headers.extend_from_slice(b"/v1/chat/completions");
        let response_headers = [0x88];
        let request_body = br#"{"model":"test","messages":[]}"#;
        let response_body = br#"{"choices":[]}"#;
        let mut request_bytes = h2_frame(1, 0x4, 1, &request_headers);
        request_bytes.extend(h2_frame(1, 0x4, 3, &request_headers));
        request_bytes.extend(h2_frame(0, 0x1, 1, request_body));
        request_bytes.extend(h2_frame(0, 0x1, 3, request_body));
        let mut response_bytes = h2_frame(1, 0x4, 3, &response_headers);
        response_bytes.extend(h2_frame(1, 0x4, 1, &response_headers));
        response_bytes.extend(h2_frame(0, 0x1, 3, response_body));
        response_bytes.extend(h2_frame(0, 0x1, 1, response_body));

        let mut fixture = Fixture::new();
        fixture.feed_h2(PayloadDirection::Outbound, 1, &request_bytes);
        fixture.feed_h2(PayloadDirection::Inbound, 2, &response_bytes);
        let (db, trace_id) = fixture.finish();
        let connection = Connection::open(&db).expect("open database");
        let calls: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM semantic_actions
                 WHERE trace_id = ?1 AND kind_name = 'llm.call'",
                [trace_id.get()],
                |row| row.get(0),
            )
            .expect("h2 calls");
        assert_eq!(calls, 2);
        let stream_ids: i64 = connection
            .query_row(
                "SELECT COUNT(DISTINCT attr_value) FROM semantic_action_attributes a
                 JOIN semantic_actions s USING (trace_id, action_id)
                 WHERE s.trace_id = ?1 AND s.kind_name = 'llm.response'
                   AND a.attr_key = 'http.stream_id'",
                [trace_id.get()],
                |row| row.get(0),
            )
            .expect("h2 stream ids");
        assert_eq!(stream_ids, 2);
        let mut statement = connection
            .prepare(
                "SELECT source_stream.attr_value, target_stream.attr_value
                 FROM semantic_action_links links
                 JOIN semantic_action_attributes source_stream
                   ON source_stream.trace_id = links.trace_id
                  AND source_stream.action_id = links.source_action_id
                  AND source_stream.attr_key = 'http.stream_id'
                 JOIN semantic_action_attributes target_stream
                   ON target_stream.trace_id = links.trace_id
                  AND target_stream.action_id = links.target_action_id
                  AND target_stream.attr_key = 'http.stream_id'
                 WHERE links.trace_id = ?1
                   AND links.role_name IN (
                       'llm_request.http_message',
                       'llm_response.http_message'
                   )",
            )
            .expect("prepare h2 evidence links");
        let linked_streams = statement
            .query_map([trace_id.get()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("query h2 evidence links")
            .collect::<Result<Vec<_>, _>>()
            .expect("read h2 evidence links");
        assert_eq!(linked_streams.len(), 4);
        assert!(
            linked_streams
                .iter()
                .all(|(source, target)| source == target)
        );
        drop(statement);
        drop(connection);
        remove_db(&db);
    }

    #[test]
    fn split_http2_frames_persist_one_closed_exchange() {
        let path = b"/v1/chat/completions";
        let split = 7;
        let mut header_block = vec![0x83, 0x44, path.len() as u8];
        header_block.extend_from_slice(path);
        let request_body = br#"{"model":"split","messages":[]}"#;
        let response_body = br#"{"choices":[],"usage":{"total_tokens":2}}"#;

        let mut fixture = Fixture::new();
        fixture.feed_h2(
            PayloadDirection::Outbound,
            1,
            &h2_frame(1, 0, 1, &header_block[..split]),
        );
        fixture.feed_h2(
            PayloadDirection::Outbound,
            2,
            &h2_frame(9, 0x4, 1, &header_block[split..]),
        );
        fixture.feed_h2(
            PayloadDirection::Outbound,
            3,
            &h2_frame(0, 0, 1, &request_body[..10]),
        );
        fixture.feed_h2(
            PayloadDirection::Outbound,
            4,
            &h2_frame(0, 0x1, 1, &request_body[10..]),
        );
        fixture.feed_h2(PayloadDirection::Inbound, 5, &h2_frame(1, 0x4, 1, &[0x88]));
        fixture.feed_h2(
            PayloadDirection::Inbound,
            6,
            &h2_frame(0, 0x1, 1, response_body),
        );

        let (db, trace_id) = fixture.finish();
        let connection = Connection::open(&db).expect("open database");
        let (http, requests, responses, calls): (i64, i64, i64, i64) = connection
            .query_row(
                "SELECT
                    SUM(kind_name = 'http.message'),
                    SUM(kind_name = 'llm.request'),
                    SUM(kind_name = 'llm.response'),
                    SUM(kind_name = 'llm.call')
                 FROM semantic_actions WHERE trace_id = ?1",
                [trace_id.get()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("count split h2 exchange");
        assert_eq!((http, requests, responses, calls), (2, 1, 1, 1));
        let (status, completeness, total_tokens): (i64, i64, String) = connection
            .query_row(
                "SELECT call.status, call.completeness, usage.attr_value
                 FROM semantic_actions call
                 JOIN semantic_actions response
                   ON response.trace_id = call.trace_id
                  AND response.kind_name = 'llm.response'
                 JOIN semantic_action_attributes usage
                   ON usage.trace_id = response.trace_id
                  AND usage.action_id = response.action_id
                  AND usage.attr_key = 'llm.usage.total_tokens'
                 WHERE call.trace_id = ?1 AND call.kind_name = 'llm.call'",
                [trace_id.get()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("load split h2 call");
        assert_eq!(
            status,
            semantic_action_contract::SemanticActionStatus::Success as i64
        );
        assert_eq!(
            completeness,
            semantic_action_contract::SemanticActionCompleteness::Complete as i64
        );
        assert_eq!(total_tokens, "2");
        drop(connection);
        remove_db(&db);
    }
}
