//! Active trace ownership, attach bootstrap, and lifecycle event handling.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime};

use collector_binding::TraceBindingRequest;
use collector_instance::CollectorInstance;
use config_core::capture_profile::CaptureProfile;
use config_core::daemon::EbpfCollectorConfig;
use config_core::trace_snapshot::CaptureProfileSnapshot;
use control_contract::command::{ControlCommand, TrackAddCommand};
use control_contract::reply::{
    ControlError, ControlReply, DoctorReply, TraceListItem, TrackAddReply,
};
use control_contract::selector::TraceSelector;
use ebpf_collector::EbpfCollector;
use ebpf_collector::procfs::{
    ProcfsIdentityReader, ProcfsTreeSnapshotter, read_container_identity, read_process_session,
    resolve_namespaced_pid,
};
use ingest_runtime::fd_lineage::FdLineage;
use ingest_runtime::retention::{PayloadRetention, PayloadRetentionConfig};
use model_core::diagnostics::CaptureDiagnostic;
use model_core::ids::{EventId, TraceId};
use model_core::process::{
    ExitObservationSource, ExitStatus, ProcessIdentity, ProcessObservation, SessionIdentity,
};
use process_identity::{ProcessIdentityError, ProcessIdentityManager, ProcessIdentityReader};
use process_tree_snapshot_contract::snapshot::ProcessTreeSnapshotter;
use storage_core::{
    BackfillJob, BackfillJobKind, BackfillJobState, CallSpanRecord,
};
use storage_factory::{StorageConfig, open_storage_backend};
use trace_runtime::TraceRuntime;
use trace_runtime::commands::{RootRemovalRequest, TrackTraceRequest};
use uds_control_server::{ControlService, PeerCredentials};

use crate::attribution::unique_call_window_match;
use crate::peer_identity::PeerIdentity;
use crate::writer::{
    BulkItem, CallCloseWrite, CallStartWrite, PersistWrite, WriterHandle, WriterParams,
};

const PROCESS_ID_BLOCK_SIZE: u64 = 65_536;

/// Owns all active traces for one daemon process and coordinates collection and storage.
///
/// The trace registry is deliberately not restored from SQLite after restart.
pub struct DaemonServiceHost {
    profile: CaptureProfile,
    runtime: TraceRuntime,
    collector: EbpfCollector,
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
    payload_action_heads:
        BTreeMap<(TraceId, String), (String, semantic_action_contract::SemanticActionKind)>,
    /// Best-effort plaintext reassembly keyed by trace and TLS stream. Raw
    /// chunks are always persisted independently; this buffer only feeds
    /// protocol parsers and is bounded by the configured retention policy.
    payload_reassembly: BTreeMap<(TraceId, String), Vec<u8>>,
    payload_call_buffers: BTreeMap<(TraceId, String, u64), Vec<u8>>,
    command_heads: BTreeMap<(TraceId, u64), String>,
    session_env_name: String,
    call_spans: Vec<CallSpanRecord>,
    pending_events: Vec<model_core::event::DomainEvent>,
    pending_payloads: Vec<model_core::payload::PayloadSegment>,
}

impl DaemonServiceHost {
    /// Assemble an empty runtime while retaining persisted process ID allocation state.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        storage_config: &StorageConfig,
        writer_config: config_core::daemon::WriterConfig,
        profile: CaptureProfile,
        ebpf_config: EbpfCollectorConfig,
        payload_max_trace_bytes: u64,
        payload_max_segment_bytes: u64,
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
        let collector = EbpfCollector::new(ebpf_config.clone());
        let descriptor = collector.descriptor().clone();
        let writer = WriterHandle::spawn(
            storage_config,
            WriterParams {
                batch_items: usize::try_from(writer_config.batch_items).unwrap_or(512),
                idle: std::time::Duration::from_millis(writer_config.idle_timeout_ms),
                checkpoint_interval: std::time::Duration::from_secs(
                    writer_config.checkpoint_interval_secs,
                ),
                queue_cap_items: usize::try_from(writer_config.queue_cap_items).unwrap_or(0),
            },
        )
        .map_err(|error| control_error("writer_spawn", error))?;
        Ok(Self {
            profile,
            runtime: TraceRuntime::new(vec![descriptor], next_trace_id),
            collector,
            writer,
            processes,
            roots: BTreeMap::new(),
            active_trace_max: usize::try_from(active_trace_max)
                .map_err(|error| ControlError::new("active_trace_max", error.to_string()))?,
            next_event_id,
            ebpf_enabled: ebpf_config.enabled,
            payload_retention: PayloadRetention::new(PayloadRetentionConfig {
                max_trace_bytes: if payload_max_trace_bytes == 0 {
                    u64::MAX
                } else {
                    payload_max_trace_bytes
                },
                max_segment_bytes: payload_max_segment_bytes,
            }),
            fd_lineage: FdLineage::new(),
            root_channels: BTreeMap::new(),
            collector_drop_totals: BTreeMap::new(),
            payload_action_heads: BTreeMap::new(),
            payload_reassembly: BTreeMap::new(),
            payload_call_buffers: BTreeMap::new(),
            command_heads: BTreeMap::new(),
            session_env_name,
            call_spans: Vec::new(),
            pending_events: Vec::new(),
            pending_payloads: Vec::new(),
        })
    }

    fn push_writes(&self, writes: Vec<PersistWrite>) {
        if !writes.is_empty() {
            self.writer.push_bulk(BulkItem::Writes(writes));
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
        unique_call_window_match(&self.call_spans, trace_id, host_pid, session_id, observed_at)
    }

    fn session_for_call(&self, trace_id: TraceId, call_id: &str) -> Option<SessionIdentity> {
        self.call_spans
            .iter()
            .find(|span| span.trace_id == trace_id && span.call_id == call_id)
            .and_then(|span| span.session_id.clone())
            .map(SessionIdentity::new)
    }

    /// Drain and apply all lifecycle events currently available from the collector.
    pub fn drain_live_events(&mut self) -> Result<(), ControlError> {
        let batch = self
            .collector
            .poll_batch()
            .map_err(collector_control_error)?;
        for event in batch.observations {
            self.apply_event(event)?;
        }
        for raw_segment in batch.payload_segments {
            self.apply_payload_segment(raw_segment)?;
        }
        self.record_collector_losses()?;
        self.flush_pending_storage();
        self.report_dropped_bulk();
        Ok(())
    }

    /// When a bounded bulk queue dropped items because the writer was behind,
    /// surface the loss through the existing diagnostics channel (best-effort:
    /// the diagnostic itself may be dropped again under the same pressure).
    fn report_dropped_bulk(&self) {
        let dropped = self.writer.take_dropped_bulk();
        if dropped == 0 {
            return;
        }
        let Some(trace_id) = self
            .runtime
            .list_trace_records()
            .into_iter()
            .find(|trace| trace.lifecycle_state.is_active_or_draining())
            .map(|trace| trace.trace_id)
        else {
            return;
        };
        self.push_writes(vec![PersistWrite::Diagnostic(CaptureDiagnostic::loss(
            trace_id,
            SystemTime::now(),
            model_core::ids::CollectorName::new("daemon"),
            "queue_overflow_drop".to_string(),
            dropped,
            0,
        ))]);
    }

    fn flush_pending_storage(&mut self) {
        if !self.pending_events.is_empty() {
            let events = std::mem::take(&mut self.pending_events);
            self.writer.push_bulk(BulkItem::Events(events));
        }
        if !self.pending_payloads.is_empty() {
            let payloads = std::mem::take(&mut self.pending_payloads);
            self.writer.push_bulk(BulkItem::Payloads(payloads));
        }
        self.writer.push_bulk(BulkItem::EndOfDrain);
    }

    fn record_collector_losses(&mut self) -> Result<(), ControlError> {
        let stats = self.collector.stats();
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
                self.pending_events.push(event);
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
            return Ok(());
        }
        let host_pid = raw.envelope.process.host.as_ref().map(|host| host.pid);
        let event_session = raw
            .envelope
            .session_id
            .clone()
            .or_else(|| host_pid.and_then(|pid| self.read_process_session(pid)));
        if raw.envelope.call_id.is_none() {
            raw.envelope.call_id =
                self.assign_call_id(trace_id, host_pid, &event_session, raw.envelope.observed_at);
        }
        if raw.envelope.session_id.is_none() {
            raw.envelope.session_id = event_session;
        }
        if raw.envelope.session_id.is_none() {
            if let Some(call_id) = raw.envelope.call_id.as_deref() {
                raw.envelope.session_id = self.session_for_call(trace_id, call_id);
            }
        }
        let observed = raw.envelope.process.clone();
        let observed_host_pid = observed.host.as_ref().map(|host| host.pid);
        let observation = observed
            .host
            .as_ref()
            .and_then(|host| ProcfsIdentityReader.read_identity(host.pid).ok())
            .unwrap_or(observed);
        let (identity, mut record) = self.resolve_process(observation)?;
        let session = self.resolve_session(observed_host_pid, raw.envelope.session_id.clone());
        record.session_id = session.clone();
        let mut writes = vec![PersistWrite::ProcessRecord(record.clone())];
        let retained = self
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
                observed_at: retained.segment.observed_at,
            });
        }
        if let Some(loss) = retained.loss {
            writes.push(PersistWrite::Diagnostic(CaptureDiagnostic {
                trace_id,
                observed_at: retained.segment.observed_at,
                collector: model_core::ids::CollectorName::new("payload-retention"),
                kind: model_core::diagnostics::DiagnosticKind::Truncated,
                severity: model_core::diagnostics::DiagnosticSeverity::Warning,
                message: loss.reason.clone(),
                dropped: 1,
                dropped_bytes: loss.original_size.saturating_sub(loss.captured_size),
            }));
            let loss_event = model_core::event::DomainEvent::new(
                model_core::event::EventEnvelope {
                    event_id: self.take_event_id()?,
                    trace_id,
                    observed_at: retained.segment.observed_at,
                    process: identity,
                    collector: model_core::ids::CollectorName::new("payload-retention"),
                    kind: model_core::event::EventKind::Loss,
                    flags: model_core::event::EventFlags::LOSS
                        .union(model_core::event::EventFlags::TRUNCATED),
                    session_id: session.clone(),
                    call_id: retained.segment.call_id.clone(),
                },
                model_core::event::EventPayload::Loss(model_core::event::LossPayload {
                    reason: loss.reason,
                    dropped: 1,
                    bytes: loss.original_size.saturating_sub(loss.captured_size),
                }),
            );
            self.pending_events.push(loss_event);
        }
        let mut parse_segment = retained.segment.clone();
        let mut reassembly_key = None;
        if let (Some(stream), Some(bytes)) = (&parse_segment.stream_key, &parse_segment.bytes) {
            let key = (trace_id, stream.clone());
            reassembly_key = Some(key.clone());
            let call_key = (
                trace_id,
                stream.clone(),
                parse_segment.operation_id.unwrap_or(parse_segment.sequence),
            );
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
            call[offset..end].copy_from_slice(bytes);
            let assembled = self.payload_reassembly.entry(key.clone()).or_default();
            let mut parser_bytes = assembled.clone();
            let call_complete = parse_segment.completed
                && matches!(
                    parse_segment.content_state,
                    model_core::payload::PayloadContentState::Complete
                );
            if parse_segment.completed {
                assembled.extend_from_slice(call);
                self.payload_call_buffers.remove(&call_key);
                parser_bytes = assembled.clone();
            } else {
                parser_bytes.extend_from_slice(call);
            }
            parse_segment.bytes = Some(parser_bytes.clone());
            parse_segment.captured_size = parser_bytes.len() as u64;
            parse_segment.original_size =
                parse_segment.original_size.max(parse_segment.captured_size);
            if parse_segment.completed {
                parse_segment.content_state = model_core::payload::PayloadContentState::Complete;
                if !call_complete {
                    parse_segment.content_state =
                        model_core::payload::PayloadContentState::Truncated;
                }
            } else {
                parse_segment.content_state = model_core::payload::PayloadContentState::Truncated;
            }
        }
        let http_action =
            semantic_action_runtime::project_http1_payload(&parse_segment, session.clone());
        let sse_actions =
            semantic_action_runtime::project_sse_payload(&parse_segment, session.clone());
        let llm_actions =
            semantic_action_runtime::project_llm_payload(&parse_segment, session.clone());
        let mcp_actions =
            semantic_action_runtime::project_mcp_payload(&parse_segment, session.clone());
        if http_action.as_ref().is_some_and(|action| {
            action.completeness == semantic_action_contract::SemanticActionCompleteness::Complete
        }) {
            if let Some(key) = reassembly_key {
                self.payload_reassembly.remove(&key);
            }
        }
        let stream_key = retained
            .segment
            .stream_key
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let segment_reference = format!("payload:{}:{}", stream_key, retained.segment.sequence);
        self.pending_payloads.push(retained.segment);
        let mut actions = Vec::new();
        if let Some(action) = http_action {
            actions.push(action);
        }
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
                semantic_action_contract::SemanticActionKind::LlmRequest
                | semantic_action_contract::SemanticActionKind::McpRequest
                | semantic_action_contract::SemanticActionKind::McpToolCall => {
                    self.payload_action_heads.insert(
                        (action.trace_id, stream_key.clone()),
                        (action.action_id.clone(), action.kind),
                    );
                }
                semantic_action_contract::SemanticActionKind::LlmResponse
                | semantic_action_contract::SemanticActionKind::McpResponse => {
                    if let Some((source, source_kind)) = self
                        .payload_action_heads
                        .get(&(action.trace_id, stream_key.clone()))
                    {
                        let role = if action.kind
                            == semantic_action_contract::SemanticActionKind::LlmResponse
                        {
                            semantic_action_contract::SemanticActionLinkRole::LlmRequestLlmResponse
                        } else if *source_kind
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
                    dropped: retention.metadata_only_segments,
                    dropped_bytes: retention.metadata_only_bytes,
                }));
            }
        }
        self.push_writes(writes);
        self.flush_pending_storage();
        self.writer
            .stop_and_checkpoint()
            .map_err(|error| control_error("writer_shutdown", error))
    }

    fn track_add(&mut self, command: TrackAddCommand) -> Result<TrackAddReply, ControlError> {
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
                .bind_trace(&TraceBindingRequest {
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
        self.runtime
            .activate_trace(trace_id, SystemTime::now())
            .map_err(|error| control_error("activate_trace", error))?;
        self.persist_trace_state(trace_id, records)?;
        self.roots.insert(trace_id, root_host_pid);
        Ok(TrackAddReply {
            trace_id,
            lifecycle_state: self
                .runtime
                .get_trace(trace_id)
                .expect("created trace")
                .trace
                .lifecycle_state,
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

    fn resolve_session(
        &self,
        host_pid: Option<u32>,
        kernel_session: Option<SessionIdentity>,
    ) -> Option<SessionIdentity> {
        let proc_session = host_pid.and_then(|pid| self.read_process_session(pid));
        let session = kernel_session.clone().or(proc_session.clone());
        let source = if kernel_session.is_some() {
            "ebpf"
        } else if proc_session.is_some() {
            "procfs"
        } else {
            "none"
        };
        // resolve_session runs once per collected event: keep at `trace!` so
        // DEBUG has no per-event log-write cost (a DEBUG line here stalled
        // the control plane under capture load).
        tracing::trace!(
            host_pid = ?host_pid,
            kernel_session = ?kernel_session,
            proc_session = ?proc_session,
            source,
            "session resolution"
        );
        session
    }

    fn read_process_session(&self, pid: u32) -> Option<SessionIdentity> {
        read_process_session(pid, "CENSORSCOPE_SESSION_ID")
            .or_else(|| read_process_session(pid, &self.session_env_name))
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
            return Ok(());
        }
        let host_pid = raw.envelope.process.host.as_ref().map(|host| host.pid);
        let event_session = raw
            .envelope
            .session_id
            .clone()
            .or_else(|| host_pid.and_then(|pid| self.read_process_session(pid)));
        if raw.envelope.call_id.is_none() {
            raw.envelope.call_id =
                self.assign_call_id(trace_id, host_pid, &event_session, raw.envelope.observed_at);
        }
        if raw.envelope.session_id.is_none() {
            raw.envelope.session_id = event_session;
        }
        if raw.envelope.session_id.is_none() {
            if let Some(call_id) = raw.envelope.call_id.as_deref() {
                raw.envelope.session_id = self.session_for_call(trace_id, call_id);
            }
        }
        let operation = match &raw.payload {
            collector_event::RawObservationPayload::Process { operation, .. } => {
                Some(operation.as_str())
            }
            _ => None,
        };
        let observed = raw.envelope.process.clone();
        let observed_host_pid = observed.host.as_ref().map(|host| host.pid);
        let observation = observed
            .host
            .as_ref()
            .and_then(|host| ProcfsIdentityReader.read_identity(host.pid).ok())
            .unwrap_or(observed);
        let (identity, mut record) = self.resolve_process(observation)?;
        let session = self.resolve_session(observed_host_pid, raw.envelope.session_id.clone());
        record.session_id = session.clone();
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
        if matches!(&raw.payload, collector_event::RawObservationPayload::Ipc { .. })
            && !self.ipc_audit_enabled()
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
        self.pending_events.push(event);
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
        }
        if let Some(membership) = self.runtime.find_membership_in_trace(trace_id, &identity) {
            writes.push(PersistWrite::Membership(membership));
        }
        let trace = &self
            .runtime
            .get_trace(trace_id)
            .expect("trace checked")
            .trace;
        writes.push(PersistWrite::TraceLifecycle {
            trace_id,
            state: trace.lifecycle_state,
        });
        self.push_writes(writes);
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
        let collector_event::RawObservationPayload::Ipc { operation, metadata, .. } = payload
        else {
            return;
        };
        if operation != "pipe" && operation != "socketpair" {
            return;
        }
        if self.root_identity(trace_id) != Some(identity) {
            return;
        }
        let first = metadata.get("fd_a").and_then(|value| value.parse::<i32>().ok());
        let second = metadata.get("fd_b").and_then(|value| value.parse::<i32>().ok());
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
        let retention = self.payload_retention.stats(trace_id);
        if retention.metadata_only_segments > 0 {
            writes.push(PersistWrite::Diagnostic(CaptureDiagnostic {
                trace_id,
                observed_at: SystemTime::now(),
                collector: model_core::ids::CollectorName::new("payload-retention"),
                kind: model_core::diagnostics::DiagnosticKind::Truncated,
                severity: model_core::diagnostics::DiagnosticSeverity::Warning,
                message: "payload_metadata_only_aggregate".to_string(),
                dropped: retention.metadata_only_segments,
                dropped_bytes: retention.metadata_only_bytes,
            }));
        }
        self.writer
            .persist(writes)
            .map_err(|error| control_error("trace_remove_persist", error))?;
        self.payload_reassembly.retain(|(id, _), _| *id != trace_id);
        self.payload_call_buffers
            .retain(|(id, _, _), _| *id != trace_id);
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
    fn handle_command(
        &mut self,
        peer: Option<PeerIdentity>,
        command: ControlCommand,
    ) -> Result<ControlReply, ControlError> {
        match command {
            ControlCommand::TrackAdd(command) => {
                let reply = self.track_add(command)?;
                if let Some(peer) = peer {
                    self.runtime
                        .bind_trace_owner(reply.trace_id, peer.principal.trace_owner())
                        .map_err(|error| control_error("trace_owner", error))?;
                }
                Ok(ControlReply::TrackAdded(reply))
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
                let started_at = std::time::UNIX_EPOCH
                    + std::time::Duration::from_nanos(command.started_at);
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
                    .call_start(CallStartWrite {
                        trace_id: command.trace_id,
                        session_id: command.session_id.clone(),
                        call_id: command.call_id.clone(),
                        host_pid: command.host_pid,
                        started_at,
                    })
                    .map_err(|error| control_error("call_start", error))?;
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
                let ended_at = std::time::UNIX_EPOCH
                    + std::time::Duration::from_nanos(command.ended_at);
                // Close the in-memory span first so ingest attribution is
                // settled for events drained after this point; jobs scope by
                // the registered span session, not this command's value.
                let (span_started, span_session) = match self
                    .call_spans
                    .iter_mut()
                    .find(|span| {
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
                // Close the stored span and persist the jobs in one writer
                // transaction so the reply means the close is durable;
                // backfill execution is asynchronous on the writer thread.
                self.writer
                    .call_close(CallCloseWrite {
                        trace_id: command.trace_id,
                        session_id: command.session_id.clone(),
                        call_id: command.call_id.clone(),
                        host_pid: command.host_pid,
                        ended_at,
                        status: command.status.clone(),
                        jobs,
                    })
                    .map_err(|error| control_error("call_end", error))?;
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
mod writer_pipeline_tests {
    use super::*;
    use config_core::daemon::{EbpfEnabledMode, MemlockRlimit, TlsScanDebounce, WriterConfig};
    use control_contract::command::{CallEndCommand, CallStartCommand};
    use model_core::ids::RequestId;
    use storage_factory::StorageConfig;

    fn test_host() -> (DaemonServiceHost, std::path::PathBuf) {
        test_host_with_profile(config_core::capture_profile::CaptureProfile::for_level(
            config_core::capture_profile::CaptureLevel::L3,
        ))
    }

    fn test_host_with_profile(
        profile: config_core::capture_profile::CaptureProfile,
    ) -> (DaemonServiceHost, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "censorscope-host-pipeline-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
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
            64 * 1024 * 1024,
            1024 * 1024,
            4,
            "DSH_CENSORSCOPE_SESSION_ID".to_string(),
        )
        .expect("build host");
        (host, path)
    }

    #[test]
    fn ipc_audit_enabled_follows_collection_level() {
        use config_core::capture_profile::CaptureLevel;
        for (level, enabled) in [
            (CaptureLevel::L1, false),
            (CaptureLevel::L2, false),
            (CaptureLevel::L3, true),
        ] {            let (host, db) = test_host_with_profile(config_core::capture_profile::CaptureProfile::for_level(level));
            assert_eq!(
                host.ipc_audit_enabled(),
                enabled,
                "level {level} ipc audit flag"
            );
            drop(host);
            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_file(format!("{}-wal", db.display()));
            let _ = std::fs::remove_file(format!("{}-shm", db.display()));
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
        // The call-end reply means the close and its window job are durable;
        // give the writer an idle tick to execute the job before shutdown.
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
        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(format!("{}-wal", db.display()));
        let _ = std::fs::remove_file(format!("{}-shm", db.display()));
    }
}
