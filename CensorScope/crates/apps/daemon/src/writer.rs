//! Dedicated SQLite writer thread.
//!
//! All SQLite writes happen on a single writer thread that owns the only
//! read-write connection. The daemon's event loop pushes ordered work over a
//! bounded FIFO (per-event ancillary rows, batched events/payloads) plus a
//! fast control lane that can either await a commit or enqueue it for later
//! acknowledgement.
//!
//! Attribution backfill jobs are enqueued durably together with the span close
//! (`call_end_with_jobs`) and execute later on this thread once every bulk
//! item enqueued before the close has been committed (FIFO gate), re-running
//! the shared matcher against the committed call spans.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use model_core::event::DomainEvent;
use model_core::ids::TraceId;
use model_core::payload::PayloadSegment;
use model_core::process::SessionIdentity;
use model_core::trace::TraceRecord;
use storage_core::{
    AncillaryRow, BackfillJob, BackfillJobKind, BackfillJobState, StorageBackend, StorageError,
};
use storage_factory::{StorageConfig, open_storage_backend};

use crate::attribution::unique_call_window_match;

/// Ancillary rows coalesced before one storage transaction.
///
/// Ancillary rows arrive one event at a time, and committing each event's rows
/// separately costs a transaction per event. Consecutive rows are accumulated
/// and applied together; the bound keeps a burst from growing the buffer
/// without limit.
const ANCILLARY_BATCH_ROWS: usize = 512;
/// Estimated memory one queued row costs before it is committed.
///
/// Row shapes differ by an order of magnitude, so the budget counts rows at a
/// representative size and counts payload bytes exactly: plaintext is what makes
/// a backlog expensive, and it is the one size known up front.
const EVENT_ROW_BYTES: usize = 1_024;
const PAYLOAD_SEGMENT_OVERHEAD_BYTES: usize = 256;
const PERSIST_ROW_BYTES: usize = 512;
/// Default memory budget for queued rows.
const DEFAULT_QUEUE_BUDGET_BYTES: usize = 256 * 1024 * 1024;
/// How long a producer waits for room before giving up on one item.
///
/// Long enough for a commit in progress to release room, short enough that a
/// stalled writer cannot hold the event loop: past this the item is dropped and
/// counted, which keeps the daemon responsive while the kernel transport -- sized
/// by the operator -- absorbs the load.
const QUEUE_PRESSURE_WAIT: Duration = Duration::from_millis(25);
/// Sleep between room checks while waiting.
const QUEUE_PRESSURE_POLL: Duration = Duration::from_millis(1);

/// Run queued backfill jobs every this many popped bulk items so a continuous firehose never starves them.
const JOB_RUN_ITEMS: u64 = 4096;
const JOB_RUN_MAX: usize = 8;
/// Attribution SELECT row caps mirror the pre-writer limits.
const WINDOW_LIMIT: u64 = 100_000;
const SWEEP_LIMIT: u64 = 200_000;
/// Upper bound a control caller waits for its ack before declaring the writer
/// unresponsive (a stalled disk commit can exceed 10s).
const CONTROL_REPLY_TIMEOUT: Duration = Duration::from_secs(30);
const CONTROL_QUEUE_CAP: usize = 1_024;

/// One ordered, idempotent persistence operation submitted by the daemon.
///
/// Ordering among writes of one event (a semantic action before its links) is
/// preserved by FIFO execution. The type is the storage-level row so a whole
/// item can be handed to the backend as one batch.
pub use storage_core::AncillaryRow as PersistWrite;

/// Ordered stream items produced by the event loop. Ordering among bulk items
/// equals push order (single producer); `EndOfDrain` marks a completed drain
/// cycle and commits whatever the writer has accumulated.
#[derive(Debug)]
pub enum BulkItem {
    /// Ancillary rows of one event/segment (process/session/semantic/...).
    Writes(Vec<PersistWrite>),
    Events(Vec<DomainEvent>),
    Payloads(Vec<PayloadSegment>),
    EndOfDrain,
}

impl BulkItem {
    /// Estimated memory this item holds until the writer commits it.
    fn cost_bytes(&self) -> usize {
        match self {
            BulkItem::Writes(rows) => rows.len() * PERSIST_ROW_BYTES,
            BulkItem::Events(events) => events.len() * EVENT_ROW_BYTES,
            BulkItem::Payloads(segments) => segments
                .iter()
                .map(|segment| {
                    PAYLOAD_SEGMENT_OVERHEAD_BYTES + segment.bytes.as_ref().map_or(0, Vec::len)
                })
                .sum(),
            BulkItem::EndOfDrain => 0,
        }
    }
}

/// A call-start persistence request.
#[derive(Clone, Debug)]
pub struct CallStartWrite {
    pub trace_id: TraceId,
    pub session_id: Option<String>,
    pub call_id: String,
    pub host_pid: u32,
    pub started_at: SystemTime,
}

/// A call-end persistence request: closes the span row and enqueues its
/// attribution jobs in one transaction. Each job carries the FIFO gate (the
/// number of bulk items enqueued before this close) that must be committed
/// before the job may run.
#[derive(Clone, Debug)]
pub struct CallCloseWrite {
    pub trace_id: TraceId,
    pub session_id: Option<String>,
    pub call_id: String,
    pub host_pid: u32,
    pub ended_at: SystemTime,
    pub status: String,
    pub jobs: Vec<(BackfillJob, u64)>,
}

/// One normalized spool record and its durable sequence.
#[derive(Clone, Debug)]
pub struct IngestBatchWrite {
    pub sequence: u64,
    pub ancillary: Vec<PersistWrite>,
    pub events: Vec<DomainEvent>,
    pub payloads: Vec<PayloadSegment>,
}

/// Values returned by acknowledged control operations.
#[derive(Debug)]
pub enum ControlValue {
    Unit,
    IdBlock { start: u64, end: u64 },
    TraceRecord(Box<Option<TraceRecord>>),
}

pub type ControlReply = Sender<Result<ControlValue, String>>;

enum ControlItem {
    CallStart(CallStartWrite, ControlReply),
    CallClose(CallCloseWrite, ControlReply),
    PersistWrites(Vec<PersistWrite>, ControlReply),
    IngestBatch(IngestBatchWrite, ControlReply),
    ReserveIdBlock {
        count: u64,
        reply: ControlReply,
    },
    ReadTrace {
        trace_id: TraceId,
        reply: ControlReply,
    },
    Stop(ControlReply),
}

/// Writer-thread tuning values (defaults mirror censorscoped.conf `[writer]`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriterParams {
    /// Commit a partial accumulator once this many events accumulate.
    pub batch_items: usize,
    /// Idle window before a partial accumulator is committed.
    pub idle: Duration,
    /// Periodic PASSIVE checkpoint cadence; zero disables the periodic one.
    pub checkpoint_interval: Duration,
    /// Bulk-channel capacity in items; zero keeps the channel unbounded.
    pub queue_cap_items: usize,
    /// Memory budget for the queued rows, in estimated bytes; zero disables the
    /// budget. Reaching it makes the producer wait for the writer instead of
    /// discarding rows, so the kernel transport -- not this queue -- is where a
    /// capture under sustained overload loses data. Production default comes
    /// from `[writer] queue_budget_bytes`.
    pub queue_budget_bytes: usize,
}

impl Default for WriterParams {
    fn default() -> Self {
        Self {
            batch_items: 2_048,
            idle: Duration::from_millis(8),
            checkpoint_interval: Duration::from_secs(60),
            queue_cap_items: 65_536,
            queue_budget_bytes: DEFAULT_QUEUE_BUDGET_BYTES,
        }
    }
}

enum BulkSender {
    Unbounded(Sender<BulkItem>),
    Bounded(SyncSender<BulkItem>),
}

impl BulkSender {
    fn try_send(&self, item: BulkItem) -> Result<(), ()> {
        match self {
            BulkSender::Unbounded(sender) => sender.send(item).map_err(|_| ()),
            BulkSender::Bounded(sender) => sender.try_send(item).map_err(|_| ()),
        }
    }
}

/// Counters the writer thread publishes for the event loop's loss ledger.
///
/// They are plain atomics rather than messages: the event loop reads them on its
/// own cadence and reports the growth, so a burst of failures costs the writer
/// thread nothing beyond an increment.
#[derive(Clone, Default)]
struct WriterCounters {
    /// Items enqueued so far; channel depth = pushed - popped.
    pushed: Arc<AtomicU64>,
    /// Estimated bytes of the rows still queued.
    queued_bytes: Arc<AtomicUsize>,
    /// Estimated bytes of the items refused by the queue.
    dropped_bytes: Arc<AtomicU64>,
    /// Ancillary rows that failed to commit.
    ancillary_failures: Arc<AtomicU64>,
    /// Event rows dropped after a batch failed twice.
    event_rows_dropped: Arc<AtomicU64>,
    /// Payload segments dropped after a batch failed twice.
    payload_rows_dropped: Arc<AtomicU64>,
    /// Attribution jobs that exhausted their attempt budget.
    backfill_gave_up: Arc<AtomicU64>,
}

/// Writer-thread endpoint owned by the daemon event loop.
pub struct WriterHandle {
    bulk: BulkSender,
    control: SyncSender<ControlItem>,
    counters: WriterCounters,
    /// Bulk items dropped because room never became available.
    bulk_dropped: Arc<AtomicU64>,
    /// Budget for `queued_bytes`; zero disables it.
    queue_budget_bytes: usize,
    join: Option<JoinHandle<()>>,
}

impl WriterHandle {
    /// Open storage on a new thread and start the writer loop. Waits for the
    /// thread's startup (schema/resume) before returning.
    pub fn spawn(config: &StorageConfig, params: WriterParams) -> Result<Self, String> {
        let (bulk_tx, bulk_rx) = if params.queue_cap_items > 0 {
            let (tx, rx) = mpsc::sync_channel(params.queue_cap_items);
            (BulkSender::Bounded(tx), rx)
        } else {
            let (tx, rx) = mpsc::channel();
            (BulkSender::Unbounded(tx), rx)
        };
        let (control_tx, control_rx) = mpsc::sync_channel(CONTROL_QUEUE_CAP);
        let counters = WriterCounters::default();
        let bulk_dropped = Arc::new(AtomicU64::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let writer_counters = counters.clone();
        let config = config.clone();
        let join = thread::Builder::new()
            .name("censorscope-writer".to_string())
            .spawn(move || {
                let mut writer =
                    match Writer::open(config, params, writer_counters, bulk_rx, control_rx) {
                        Ok(writer) => writer,
                        Err(error) => {
                            let _ = started_tx.send(Err(error));
                            return;
                        }
                    };
                if let Err(error) = writer.resume_pending_jobs() {
                    let _ = started_tx.send(Err(error));
                    return;
                }
                let _ = started_tx.send(Ok(()));
                writer.run();
            })
            .map_err(|error| format!("spawn writer: {error}"))?;
        started_rx
            .recv_timeout(CONTROL_REPLY_TIMEOUT)
            .map_err(|error| format!("writer startup timeout: {error}"))?
            .map_err(|error| format!("writer startup failed: {error}"))?;
        Ok(Self {
            bulk: bulk_tx,
            control: control_tx,
            counters,
            bulk_dropped,
            queue_budget_bytes: params.queue_budget_bytes,
            join: Some(join),
        })
    }

    /// Enqueue one ordered bulk item, waiting for room while the writer is
    /// behind.
    ///
    /// The queued rows carry a memory budget: reaching it makes this producer
    /// wait instead of discarding rows, so a capture that outruns storage loses
    /// data in the kernel transport, where the loss is counted per observation
    /// class, rather than in whole queued batches here. The wait is bounded --
    /// an item that finds no room within `QUEUE_PRESSURE_WAIT` is dropped and
    /// counted -- so a stalled writer cannot pin the event loop forever.
    /// Control operations use their own lane and are never behind this budget.
    pub fn push_bulk(&self, item: BulkItem) -> bool {
        let cost = item.cost_bytes();
        if !self.reserve_queue_bytes(cost) {
            self.record_dropped_item(cost);
            return false;
        }
        if self.bulk.try_send(item).is_ok() {
            self.counters.pushed.fetch_add(1, Ordering::Relaxed);
            true
        } else {
            self.counters
                .queued_bytes
                .fetch_sub(cost, Ordering::Relaxed);
            self.record_dropped_item(cost);
            false
        }
    }

    fn record_dropped_item(&self, cost: usize) {
        self.bulk_dropped.fetch_add(1, Ordering::Relaxed);
        self.counters
            .dropped_bytes
            .fetch_add(cost as u64, Ordering::Relaxed);
    }

    /// Claim `cost` bytes of the queue budget, waiting briefly for room.
    fn reserve_queue_bytes(&self, cost: usize) -> bool {
        if self.queue_budget_bytes == 0 {
            return true;
        }
        let mut waited = Duration::ZERO;
        loop {
            let queued = self.counters.queued_bytes.load(Ordering::Relaxed);
            if queued.saturating_add(cost) <= self.queue_budget_bytes {
                // Another producer may have claimed the room in between; the
                // compare-exchange keeps the budget honest without a lock.
                if self
                    .counters
                    .queued_bytes
                    .compare_exchange(queued, queued + cost, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
                {
                    return true;
                }
                continue;
            }
            if waited >= QUEUE_PRESSURE_WAIT {
                return false;
            }
            thread::sleep(QUEUE_PRESSURE_POLL);
            waited += QUEUE_PRESSURE_POLL;
        }
    }

    /// Total bulk items dropped on a full queue since the writer started.
    ///
    /// Monotonic: callers report the delta against their own last reported
    /// total, so a caller that cannot record the loss yet does not consume it.
    pub fn dropped_bulk_total(&self) -> u64 {
        self.bulk_dropped.load(Ordering::Relaxed)
    }

    /// Number of bulk items enqueued so far (FIFO gate source).
    pub fn bulk_pushed_count(&self) -> u64 {
        self.counters.pushed.load(Ordering::Relaxed)
    }

    /// Estimated bytes of the items dropped on a full queue.
    pub fn dropped_bulk_bytes_total(&self) -> u64 {
        self.counters.dropped_bytes.load(Ordering::Relaxed)
    }

    /// Ancillary rows that failed to commit.
    pub fn ancillary_failures_total(&self) -> u64 {
        self.counters.ancillary_failures.load(Ordering::Relaxed)
    }

    /// Event and payload rows dropped after their batch failed twice.
    pub fn dropped_row_totals(&self) -> (u64, u64) {
        (
            self.counters.event_rows_dropped.load(Ordering::Relaxed),
            self.counters.payload_rows_dropped.load(Ordering::Relaxed),
        )
    }

    /// Attribution jobs that exhausted their attempt budget.
    pub fn backfill_gave_up_total(&self) -> u64 {
        self.counters.backfill_gave_up.load(Ordering::Relaxed)
    }

    #[allow(dead_code)]
    pub fn call_start(&self, write: CallStartWrite) -> Result<(), String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.control
            .send(ControlItem::CallStart(write, reply_tx))
            .map_err(|_| "writer channel closed".to_string())?;
        wait_reply(reply_rx).map(|_| ())
    }

    pub fn call_start_async(&self, write: CallStartWrite) -> Result<(), String> {
        let (reply_tx, _reply_rx) = mpsc::channel();
        self.control
            .try_send(ControlItem::CallStart(write, reply_tx))
            .map_err(|error| control_queue_error(error.to_string()))
    }

    #[allow(dead_code)]
    pub fn call_close(&self, write: CallCloseWrite) -> Result<(), String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.control
            .send(ControlItem::CallClose(write, reply_tx))
            .map_err(|_| "writer channel closed".to_string())?;
        wait_reply(reply_rx).map(|_| ())
    }

    pub fn call_close_async(&self, write: CallCloseWrite) -> Result<(), String> {
        let (reply_tx, _reply_rx) = mpsc::channel();
        self.control
            .try_send(ControlItem::CallClose(write, reply_tx))
            .map_err(|error| control_queue_error(error.to_string()))
    }

    pub fn persist(&self, writes: Vec<PersistWrite>) -> Result<(), String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.control
            .send(ControlItem::PersistWrites(writes, reply_tx))
            .map_err(|_| "writer channel closed".to_string())?;
        wait_reply(reply_rx).map(|_| ())
    }

    pub fn persist_async(&self, writes: Vec<PersistWrite>) -> Result<(), String> {
        let (reply_tx, _reply_rx) = mpsc::channel();
        self.control
            .try_send(ControlItem::PersistWrites(writes, reply_tx))
            .map_err(|error| control_queue_error(error.to_string()))
    }

    pub fn ingest_batch_async(
        &self,
        batch: IngestBatchWrite,
    ) -> Result<Receiver<Result<ControlValue, String>>, String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.control
            .send(ControlItem::IngestBatch(batch, reply_tx))
            .map_err(|_| "writer channel closed".to_string())?;
        Ok(reply_rx)
    }

    pub fn reserve_id_block(&self, count: u64) -> Result<(u64, u64), String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.control
            .send(ControlItem::ReserveIdBlock {
                count,
                reply: reply_tx,
            })
            .map_err(|_| "writer channel closed".to_string())?;
        match wait_reply(reply_rx)? {
            ControlValue::IdBlock { start, end } => Ok((start, end)),
            _ => Err("unexpected reserve reply".to_string()),
        }
    }

    pub fn read_trace(&self, trace_id: TraceId) -> Result<Option<TraceRecord>, String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.control
            .send(ControlItem::ReadTrace {
                trace_id,
                reply: reply_tx,
            })
            .map_err(|_| "writer channel closed".to_string())?;
        match wait_reply(reply_rx)? {
            ControlValue::TraceRecord(record) => Ok(*record),
            _ => Err("unexpected read_trace reply".to_string()),
        }
    }

    /// Flush all queued work, checkpoint, and join the writer thread.
    pub fn stop_and_checkpoint(&mut self) -> Result<(), String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.control
            .send(ControlItem::Stop(reply_tx))
            .map_err(|_| "writer channel closed".to_string())?;
        let result = reply_rx
            .recv()
            .map_err(|_| "writer stopped before ack".to_string())?
            .map(|_| ());
        if let Some(join) = self.join.take()
            && join.join().is_err()
        {
            tracing::warn!("writer thread panicked");
        }
        result
    }
}

fn wait_reply(rx: Receiver<Result<ControlValue, String>>) -> Result<ControlValue, String> {
    rx.recv_timeout(CONTROL_REPLY_TIMEOUT)
        .map_err(|error| format!("writer unresponsive: {error}"))?
}

fn control_queue_error(message: String) -> String {
    format!("writer control queue unavailable: {message}")
}

struct Writer {
    storage: Box<dyn StorageBackend>,
    bulk_rx: Receiver<BulkItem>,
    control_rx: Receiver<ControlItem>,
    batch_items: usize,
    idle: Duration,
    checkpoint_interval: Duration,
    events: Vec<DomainEvent>,
    payloads: Vec<PayloadSegment>,
    /// Number of bulk items popped so far (all committed after a flush).
    popped: u64,
    items_since_jobs: u64,
    /// Ancillary rows waiting for the next storage transaction.
    pending_ancillary: Vec<AncillaryRow>,
    /// Durable backfill jobs waiting on their FIFO gate: queue_id -> gate.
    pending_jobs: BTreeMap<i64, u64>,
    last_checkpoint: Instant,
    /// Extra wait added to the periodic checkpoint after an expensive one.
    checkpoint_backoff: Duration,
    /// Main database path (for the -wal file size probe).
    db_path: PathBuf,
    /// Last time a real (non-EndOfDrain) bulk item or control op was handled.
    last_real_bulk: Instant,
    /// Latched when the accumulator grows past a stall threshold so we log
    /// once per episode instead of spamming.
    stall_warned: bool,
    /// Counters shared with the event loop for its loss ledger.
    counters: WriterCounters,
    depth_warned: bool,
    last_mem_log: Instant,
}

impl Writer {
    fn open(
        config: StorageConfig,
        params: WriterParams,
        counters: WriterCounters,
        bulk_rx: Receiver<BulkItem>,
        control_rx: Receiver<ControlItem>,
    ) -> Result<Self, String> {
        let storage = open_storage_backend(&config)
            .map_err(|error| format!("writer open: {}: {}", error.stage, error.message))?;
        let db_path = config.path().to_path_buf();
        Ok(Self {
            storage,
            bulk_rx,
            control_rx,
            batch_items: params.batch_items.max(1),
            idle: params.idle,
            checkpoint_interval: params.checkpoint_interval,
            events: Vec::new(),
            payloads: Vec::new(),
            popped: 0,
            items_since_jobs: 0,
            pending_ancillary: Vec::new(),
            pending_jobs: BTreeMap::new(),
            last_checkpoint: Instant::now(),
            checkpoint_backoff: Duration::ZERO,
            db_path,
            last_real_bulk: Instant::now(),
            stall_warned: false,
            counters,
            depth_warned: false,
            last_mem_log: Instant::now(),
        })
    }

    /// Startup recovery: reset `running` rows left by a crash and schedule all
    /// remaining pending jobs for the paced run-loop executor (gate 0 — at
    /// boot there is no bulk backlog, so every job is immediately eligible).
    /// Nothing heavy runs here, so daemon startup never blocks on a backlog.
    fn resume_pending_jobs(&mut self) -> Result<(), String> {
        let resumed = self
            .storage
            .reset_stale_backfill_jobs()
            .map_err(storage_stage("reset_stale_backfill_jobs"))?;
        if resumed > 0 {
            tracing::warn!(resumed, "backfill queue: reset stale running jobs");
        }
        let ids = self
            .storage
            .list_pending_backfill_job_ids()
            .map_err(storage_stage("list_pending_backfill_job_ids"))?;
        if !ids.is_empty() {
            tracing::info!(count = ids.len(), "backfill queue resume");
            for queue_id in ids {
                self.pending_jobs.insert(queue_id, 0);
            }
        }
        Ok(())
    }

    fn run(&mut self) {
        loop {
            // Fast control lane first: control replies never wait behind a deep event backlog.
            while let Ok(item) = self.control_rx.try_recv() {
                if self.handle_control(item) {
                    return;
                }
            }
            match self.bulk_rx.recv_timeout(self.idle) {
                Ok(item) => {
                    self.items_since_jobs = self.items_since_jobs.saturating_add(1);
                    self.handle_bulk(item);
                    self.observe_stall();
                    self.observe_queue_memory();
                }
                Err(RecvTimeoutError::Timeout) => {
                    // Ancillary rows commit before job execution and checkpoint,
                    // both of which read rows this writer has already accepted.
                    self.flush_ancillary();
                    self.flush_accumulator();
                    self.run_ready_jobs();
                    self.maybe_checkpoint();
                    self.observe_queue_memory();
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.flush_ancillary();
                    self.flush_accumulator();
                    tracing::info!("writer bulk channel closed; exiting");
                    return;
                }
            }
            if self.items_since_jobs >= JOB_RUN_ITEMS {
                self.items_since_jobs = 0;
                self.flush_ancillary();
                self.flush_accumulator();
                self.run_ready_jobs();
                self.maybe_checkpoint();
            }
        }
    }

    /// Executes one control operation and replies. Returns true to exit.
    fn handle_control(&mut self, item: ControlItem) -> bool {
        let (result, reply) = match item {
            ControlItem::CallStart(write, reply) => (
                self.storage
                    .call_start(
                        write.trace_id,
                        write.session_id.as_deref(),
                        &write.call_id,
                        write.host_pid,
                        write.started_at,
                    )
                    .map(|_| ControlValue::Unit)
                    .map_err(storage_stage("call_start")),
                reply,
            ),
            ControlItem::CallClose(write, reply) => {
                (self.call_close(write).map(|_| ControlValue::Unit), reply)
            }
            ControlItem::PersistWrites(writes, reply) => (
                self.apply_writes_strict(writes).map(|_| ControlValue::Unit),
                reply,
            ),
            ControlItem::IngestBatch(batch, reply) => {
                self.flush_ancillary();
                self.flush_accumulator();
                (
                    self.storage
                        .apply_ingest_batch(
                            batch.sequence,
                            batch.ancillary,
                            batch.events,
                            batch.payloads,
                        )
                        .map_err(storage_stage("apply_ingest_batch"))
                        .map(|_| ControlValue::Unit),
                    reply,
                )
            }
            ControlItem::ReserveIdBlock { count, reply } => (
                self.storage
                    .reserve_process_id_block(count)
                    .map(|(start, end)| ControlValue::IdBlock { start, end })
                    .map_err(storage_stage("reserve_process_id_block")),
                reply,
            ),
            ControlItem::ReadTrace { trace_id, reply } => (
                self.storage
                    .read_trace(trace_id)
                    .map(|record| ControlValue::TraceRecord(Box::new(record)))
                    .map_err(storage_stage("read_trace")),
                reply,
            ),
            ControlItem::Stop(reply) => {
                // Drain whatever the event loop pushed before Stop so no
                // in-flight event batch is lost at shutdown.
                loop {
                    match self.bulk_rx.recv_timeout(self.idle) {
                        Ok(item) => self.handle_bulk(item),
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                self.flush_ancillary();
                self.flush_accumulator();
                // Final checkpoint truncates the -wal file back to the size
                // limit so a long capture does not leave a giant WAL behind.
                let checkpoint = self
                    .storage
                    .checkpoint_truncate()
                    .or_else(|_| self.storage.checkpoint())
                    .map(|_| ControlValue::Unit)
                    .map_err(storage_stage("sqlite_checkpoint"));
                let _ = reply.send(checkpoint);
                return true;
            }
        };
        let failure = result.as_ref().err().cloned();
        if reply.send(result).is_err()
            && let Some(error) = failure
        {
            tracing::warn!(error = %error, "asynchronous control write failed");
        }
        false
    }

    fn call_close(&mut self, write: CallCloseWrite) -> Result<(), String> {
        let (jobs, gates): (Vec<BackfillJob>, Vec<u64>) = write.jobs.into_iter().unzip();
        let ids = self
            .storage
            .call_end_with_jobs(
                write.trace_id,
                write.session_id.as_deref(),
                &write.call_id,
                write.host_pid,
                write.ended_at,
                &write.status,
                &jobs,
            )
            .map_err(storage_stage("call_end_with_jobs"))?;
        for (queue_id, gate) in ids.into_iter().zip(gates) {
            self.pending_jobs.insert(queue_id, gate);
        }
        Ok(())
    }

    /// Execute every write, continuing past failures (bulk lane). Returns the
    /// list of failed stage messages for reporting.
    /// Apply one item's ancillary rows as a single storage batch.
    ///
    /// Rows stay individually atomic inside the batch, so a row that fails is
    /// reported and the rest still commit.
    fn apply_writes(&mut self, writes: Vec<PersistWrite>) -> Vec<String> {
        match self.storage.apply_ancillary_batch(writes) {
            Ok(failures) => failures,
            Err(error) => vec![format!("{}: {}", error.stage, error.message)],
        }
    }

    /// Execute every write, stopping at the first failure (control lane, the
    /// reply then surfaces the error to the caller).
    fn apply_writes_strict(&mut self, writes: Vec<PersistWrite>) -> Result<(), String> {
        self.storage
            .apply_control_batch(writes)
            .map_err(storage_stage("apply_control_batch"))
    }

    fn handle_bulk(&mut self, item: BulkItem) {
        // The rows leave the queue here and are bounded by the accumulators
        // instead, so the producer's budget is released at this point.
        self.counters
            .queued_bytes
            .fetch_sub(item.cost_bytes(), Ordering::Relaxed);
        match item {
            BulkItem::Writes(writes) => {
                self.last_real_bulk = Instant::now();
                self.queue_ancillary(writes);
            }
            BulkItem::Events(events) => {
                self.last_real_bulk = Instant::now();
                self.events.extend(events);
                self.commit_event_head();
            }
            BulkItem::Payloads(payloads) => {
                self.last_real_bulk = Instant::now();
                self.payloads.extend(payloads);
                self.commit_payload_head();
            }
            BulkItem::EndOfDrain => {
                // End of a drain cycle: commit any partial tail now (bounded loss window).
                self.flush_ancillary();
                self.flush_accumulator();
            }
        }
        self.popped = self.popped.saturating_add(1);
    }

    /// Commit full event batches as soon as one accumulates so commit latency
    /// stays bounded even when drain cycles arrive faster than the idle timer.
    fn commit_event_head(&mut self) {
        while self.events.len() >= self.batch_items {
            let rest = self.events.split_off(self.batch_items);
            let head = std::mem::replace(&mut self.events, rest);
            self.commit_event_chunks(&head);
        }
    }

    fn commit_payload_head(&mut self) {
        while self.payloads.len() >= self.batch_items {
            let rest = self.payloads.split_off(self.batch_items);
            let head = std::mem::replace(&mut self.payloads, rest);
            self.commit_payload_chunks(&head);
        }
    }

    /// Unbounded-queue guard: if the writer falls behind (disk stall, huge
    /// checkpoint, slow job) the in-memory accumulator/channel grows without
    /// bound. Log once per episode past a threshold so a freeze/OOM episode is
    /// attributable in the daemon log.
    fn observe_stall(&mut self) {
        const STALL_ROWS: usize = 100_000;
        const RECOVER_ROWS: usize = 50_000;
        let queued = self.events.len().saturating_add(self.payloads.len());
        if queued > STALL_ROWS && !self.stall_warned {
            self.stall_warned = true;
            tracing::warn!(
                queued,
                "writer accumulator large; storage falling behind (unbounded queue grows)"
            );
        } else if queued < RECOVER_ROWS && self.stall_warned {
            self.stall_warned = false;
            tracing::info!(queued, "writer accumulator recovered");
        }
    }

    /// Track the real unbounded-queue risk: items sitting in the channel
    /// (pushed - popped) plus the writer's own RSS. Logs once per episode past
    /// thresholds so an OOM episode is attributable and visible early.
    fn observe_queue_memory(&mut self) {
        let depth = self
            .counters
            .pushed
            .load(Ordering::Relaxed)
            .saturating_sub(self.popped);
        const DEPTH_WARN: u64 = 200_000;
        const DEPTH_RECOVER: u64 = 100_000;
        if depth > DEPTH_WARN && !self.depth_warned {
            self.depth_warned = true;
            tracing::warn!(
                depth,
                popped = self.popped,
                "bulk queue deep; writer falling behind (bounded queue_cap_items prevents OOM)"
            );
        } else if depth < DEPTH_RECOVER && self.depth_warned {
            self.depth_warned = false;
            tracing::info!(depth, "bulk queue recovered");
        }
        // RSS sampler (2s cadence): surface memory growth before OOM.
        if self.last_mem_log.elapsed() >= Duration::from_secs(2) {
            self.last_mem_log = Instant::now();
            let rss_kb = process_rss_kb();
            if let Some(rss_kb) = rss_kb {
                let rss_mb = rss_kb / 1024;
                if rss_mb >= 700 {
                    tracing::warn!(rss_mb, depth, "writer RSS high");
                }
            }
        }
    }

    /// Accumulate ancillary rows and apply them once enough of them wait.
    fn queue_ancillary(&mut self, rows: Vec<AncillaryRow>) {
        self.pending_ancillary.extend(rows);
        if self.pending_ancillary.len() >= ANCILLARY_BATCH_ROWS {
            self.flush_ancillary();
        }
    }

    /// Apply the coalesced ancillary rows in one storage transaction.
    fn flush_ancillary(&mut self) {
        if self.pending_ancillary.is_empty() {
            return;
        }
        let rows = std::mem::take(&mut self.pending_ancillary);
        let failures = self.apply_writes(rows);
        if !failures.is_empty() {
            self.counters
                .ancillary_failures
                .fetch_add(failures.len() as u64, Ordering::Relaxed);
        }
        for failure in failures {
            tracing::warn!(error = %failure, "ancillary write failed");
        }
    }

    fn flush_accumulator(&mut self) {
        if !self.events.is_empty() {
            let events = std::mem::take(&mut self.events);
            self.commit_event_chunks(&events);
        }
        if !self.payloads.is_empty() {
            let payloads = std::mem::take(&mut self.payloads);
            self.commit_payload_chunks(&payloads);
        }
    }

    fn commit_event_chunks(&mut self, events: &[DomainEvent]) {
        for chunk in events.chunks(self.batch_items) {
            let chunk = chunk.to_vec();
            if let Err(error) = self.storage.append_events_batch(&chunk) {
                tracing::warn!(error = %error, count = chunk.len(), "event batch failed; retrying once");
                if let Err(error) = self.storage.append_events_batch(&chunk) {
                    tracing::error!(error = %error, "event batch failed twice; degrading per event");
                    let mut dropped = 0u64;
                    for event in chunk {
                        if let Err(error) = self.storage.append_event(event) {
                            dropped = dropped.saturating_add(1);
                            tracing::warn!(error = %error, "event append dropped");
                        }
                    }
                    if dropped > 0 {
                        self.counters
                            .event_rows_dropped
                            .fetch_add(dropped, Ordering::Relaxed);
                        tracing::error!(dropped, "events dropped after writer failure");
                    }
                }
            }
        }
    }

    fn commit_payload_chunks(&mut self, payloads: &[PayloadSegment]) {
        for chunk in payloads.chunks(self.batch_items) {
            let chunk = chunk.to_vec();
            if let Err(error) = self.storage.append_payloads_batch(&chunk) {
                tracing::warn!(error = %error, count = chunk.len(), "payload batch failed; retrying once");
                if let Err(error) = self.storage.append_payloads_batch(&chunk) {
                    tracing::error!(error = %error, "payload batch failed twice; degrading per segment");
                    let mut dropped = 0u64;
                    for segment in chunk {
                        if let Err(error) = self.storage.append_payload(segment) {
                            dropped = dropped.saturating_add(1);
                            tracing::warn!(error = %error, "payload append dropped");
                        }
                    }
                    if dropped > 0 {
                        self.counters
                            .payload_rows_dropped
                            .fetch_add(dropped, Ordering::Relaxed);
                        tracing::error!(dropped, "payload segments dropped after writer failure");
                    }
                }
            }
        }
    }

    /// Run jobs whose FIFO gate is satisfied (all bulk items enqueued before
    /// the enclosing call-close are committed: popped == committed after the
    /// preceding flush).
    fn run_ready_jobs(&mut self) {
        if self.pending_jobs.is_empty() {
            return;
        }
        let ready = self
            .pending_jobs
            .iter()
            .filter(|(_, gate)| **gate <= self.popped)
            .map(|(queue_id, _)| *queue_id)
            .take(JOB_RUN_MAX)
            .collect::<Vec<_>>();
        if ready.is_empty() {
            return;
        }
        for queue_id in ready {
            self.pending_jobs.remove(&queue_id);
            let job = match self.storage.claim_backfill_job_by_id(queue_id) {
                Ok(Some(job)) => job,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(queue_id, error = %error, "claim backfill job failed");
                    continue;
                }
            };
            self.execute_claimed_job(job);
        }
    }

    fn execute_claimed_job(&mut self, job: BackfillJob) {
        match execute_backfill_job(&mut self.storage, &job) {
            Ok(()) => {
                if let Err(error) = self.storage.finish_backfill_job(job.queue_id, None) {
                    tracing::warn!(queue_id = job.queue_id, error = %error, "backfill finish failed");
                }
                tracing::info!(trace_id = %job.trace_id.get(), kind = ?job.kind, "backfill job done");
            }
            Err(error) => {
                tracing::warn!(trace_id = %job.trace_id.get(), kind = ?job.kind, error = %error, "backfill job failed");
                match self.storage.finish_backfill_job(job.queue_id, Some(&error)) {
                    // Attempt budget not exhausted: requeue within this run so
                    // transient writer-side failures are retried (gate is
                    // already satisfied, all earlier bulk items committed).
                    Ok(BackfillJobState::Pending) => {
                        self.pending_jobs.insert(job.queue_id, self.popped);
                    }
                    Ok(BackfillJobState::Done) => {
                        self.counters
                            .backfill_gave_up
                            .fetch_add(1, Ordering::Relaxed);
                        tracing::error!(
                            queue_id = job.queue_id,
                            "backfill job gave up after attempts"
                        );
                    }
                    Ok(BackfillJobState::Running) => {}
                    Err(finish_error) => {
                        // Persistently failing storage: do not requeue blindly
                        // (that would spin); leave the row for boot recovery.
                        tracing::error!(queue_id = job.queue_id, error = %finish_error, "backfill requeue failed");
                    }
                }
            }
        }
    }

    /// Start truncating the WAL once it exceeds this size (bytes): a
    /// multi-GB WAL makes every checkpoint copy huge and readers slow.
    const WAL_TRUNCATE_AT: u64 = 64 * 1024 * 1024;
    /// Only checkpoint after this much true quiescence: on a slow disk a
    /// hot-path checkpoint froze the thread for minutes (D-state), so reset
    /// only in quiet gaps.
    const QUIET_BEFORE_CHECKPOINT: Duration = Duration::from_secs(2);
    /// A checkpoint slower than this makes the next one wait longer.
    ///
    /// The writer thread is the stage that feeds every queued row, so a
    /// checkpoint that stalls it also fills the queue behind it. Backing off
    /// after an expensive one keeps a slow disk from repeating the stall, while
    /// the WAL size triggers below still bound how far the file can grow.
    const CHECKPOINT_SLOW: Duration = Duration::from_millis(2_000);
    /// Longest backoff applied to the periodic checkpoint interval.
    const CHECKPOINT_BACKOFF_MAX: Duration = Duration::from_secs(30 * 60);

    fn maybe_checkpoint(&mut self) {
        if self.checkpoint_interval.is_zero() {
            return;
        }
        let idle = self.counters.pushed.load(Ordering::Relaxed) == self.popped;
        if !idle || self.last_real_bulk.elapsed() < Self::QUIET_BEFORE_CHECKPOINT {
            return;
        }
        let wal_before = self.wal_bytes();
        let interval_due =
            self.last_checkpoint.elapsed() >= self.checkpoint_interval + self.checkpoint_backoff;
        let oversized = wal_before >= Self::WAL_TRUNCATE_AT;
        if !interval_due && !oversized {
            return;
        }
        self.last_checkpoint = Instant::now();
        // PASSIVE only moves WAL content into the database and never waits on a
        // reader, so it is safe to run first; the truncating form that also
        // shrinks the file follows only when the WAL is still large.
        let started = Instant::now();
        let mut result = self.storage.checkpoint();
        let mut truncated = false;
        if result.is_ok() && self.wal_bytes() >= Self::WAL_TRUNCATE_AT {
            match self
                .storage
                .checkpoint_truncate()
                .or_else(|_| self.storage.checkpoint())
            {
                Ok(()) => truncated = true,
                Err(error) => result = Err(error),
            }
        }
        let elapsed = started.elapsed();
        if elapsed >= Self::CHECKPOINT_SLOW {
            self.checkpoint_backoff = (self.checkpoint_backoff + self.checkpoint_interval)
                .min(Self::CHECKPOINT_BACKOFF_MAX);
        } else if self.checkpoint_backoff > Duration::ZERO {
            self.checkpoint_backoff = self
                .checkpoint_backoff
                .saturating_sub(self.checkpoint_interval);
        }
        let wal_after = self.wal_bytes();
        match result {
            Ok(()) => tracing::info!(
                wal_mb_before = wal_before / (1024 * 1024),
                wal_mb_after = wal_after / (1024 * 1024),
                quiet_secs = self.last_real_bulk.elapsed().as_secs_f32(),
                took_ms = elapsed.as_millis() as u64,
                truncated,
                backoff_secs = self.checkpoint_backoff.as_secs(),
                "writer checkpoint (quiet)"
            ),
            Err(error) => tracing::warn!(
                wal_mb = wal_before / (1024 * 1024),
                error = %error,
                "writer checkpoint failed (readers may be active)"
            ),
        }
    }

    fn wal_bytes(&self) -> u64 {
        self.db_path
            .with_extension("sqlite-wal")
            .metadata()
            .map(|meta| meta.len())
            .unwrap_or(0)
    }
}

fn execute_backfill_job(
    storage: &mut Box<dyn StorageBackend>,
    job: &BackfillJob,
) -> Result<(), String> {
    let limit = match job.kind {
        BackfillJobKind::Window => WINDOW_LIMIT,
        BackfillJobKind::Sweep => SWEEP_LIMIT,
    };
    let ns = |time: SystemTime| -> i64 {
        time.duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|value| i64::try_from(value.as_nanos()).ok())
            .unwrap_or(i64::MAX)
    };
    let candidates = storage
        .unassigned_events_in_window(
            job.trace_id,
            job.session_id.as_deref(),
            ns(job.scope_from),
            ns(job.scope_to),
            limit,
        )
        .map_err(storage_stage("unassigned_events_in_window"))?;
    if candidates.is_empty() {
        return Ok(());
    }
    let candidate_count = candidates.len();
    let session = job.session_id.clone().map(SessionIdentity::new);
    let spans = storage
        .list_call_spans(job.trace_id)
        .map_err(storage_stage("list_call_spans"))?;
    let mut assignments: Vec<(i64, String)> = Vec::new();
    for (event_id, host_pid, observed_ns) in candidates {
        let observed_at =
            UNIX_EPOCH + Duration::from_nanos(u64::try_from(observed_ns).unwrap_or_default());
        if let Some(call_id) =
            unique_call_window_match(&spans, job.trace_id, host_pid, &session, observed_at)
        {
            assignments.push((event_id, call_id));
        }
    }
    if !assignments.is_empty() {
        storage
            .assign_event_call_ids(&assignments)
            .map_err(storage_stage("assign_event_call_ids"))?;
    }
    tracing::info!(
        trace_id = %job.trace_id.get(),
        candidates = candidate_count,
        assigned = assignments.len(),
        "backfill job assigned events"
    );
    Ok(())
}

/// Resident set size of this process in KiB, read from /proc/self/status.
fn process_rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

fn storage_stage(stage: &'static str) -> impl Fn(StorageError) -> String {
    move |error: StorageError| format!("{stage}: {}", error.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_core::event::{
        DomainEvent, EventEnvelope, EventFlags, EventKind, EventPayload, FilePayload,
    };
    use model_core::ids::CollectorName;
    use model_core::process::ProcessIdentity;
    use std::path::PathBuf;

    fn event(trace: u64, id: u64, at: SystemTime) -> DomainEvent {
        DomainEvent::new(
            EventEnvelope {
                event_id: model_core::ids::EventId::new(id),
                trace_id: TraceId::new(trace),
                observed_at: at,
                process: ProcessIdentity::new(77),
                collector: CollectorName::new("test"),
                kind: EventKind::Unknown,
                session_id: Some(SessionIdentity::new("s1")),
                call_id: None,
                flags: EventFlags::empty(),
            },
            EventPayload::File(FilePayload {
                operation: "write".to_string(),
                path: Some("/tmp/x".to_string()),
                fd: Some(3),
                size: Some(1),
                metadata: Default::default(),
            }),
        )
    }

    fn job(trace: u64, from: SystemTime, to: SystemTime) -> BackfillJob {
        BackfillJob {
            queue_id: 0,
            trace_id: TraceId::new(trace),
            session_id: Some("s1".to_string()),
            call_id: Some("c1".to_string()),
            kind: BackfillJobKind::Window,
            span_started_at: Some(from),
            span_ended_at: Some(to),
            scope_from: from,
            scope_to: to,
            state: BackfillJobState::Pending,
            attempts: 0,
            last_error: None,
            enqueued_at: SystemTime::now(),
            updated_at: SystemTime::now(),
        }
    }

    fn temp_db() -> PathBuf {
        std::env::temp_dir().join(format!(
            "censorscope-writer-test-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    /// The queue budget is what decides whether a capture under overload loses
    /// rows here or in the kernel transport, so its accounting is pinned down.
    #[test]
    fn queued_rows_are_charged_against_the_budget() {
        let path = temp_db();
        let _ = std::fs::remove_file(&path);
        let config = StorageConfig::sqlite(&path, 5_000);
        let at = |secs: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);

        // A budget smaller than one payload item leaves no room at all.
        let tiny = WriterParams {
            queue_budget_bytes: 1,
            ..WriterParams::default()
        };
        let handle = WriterHandle::spawn(&config, tiny).expect("spawn writer");
        let payload = BulkItem::Payloads(vec![model_core::payload::PayloadSegment {
            trace_id: TraceId::new(1),
            process: ProcessIdentity::new(77),
            session_id: None,
            call_id: None,
            observed_at: at(1),
            source: model_core::payload::PayloadSourceBoundary::Uprobe,
            content_state: model_core::payload::PayloadContentState::Complete,
            direction: model_core::payload::PayloadDirection::Outbound,
            stream_key: Some("s".to_string()),
            sequence: 0,
            operation_id: None,
            offset: None,
            completed: true,
            original_size: 4,
            captured_size: 4,
            library: None,
            symbol: None,
            protocol_hint: None,
            loss_reason: None,
            bytes: Some(vec![1, 2, 3, 4]),
        }]);
        assert!(
            !handle.push_bulk(payload),
            "an item larger than the budget cannot be queued"
        );
        assert_eq!(handle.dropped_bulk_total(), 1);
        // An item that costs nothing is unaffected by the budget.
        assert!(handle.push_bulk(BulkItem::EndOfDrain));
        let mut handle = handle;
        handle.stop_and_checkpoint().expect("stop writer");

        // The same item fits once the budget covers it.
        let roomy = WriterParams {
            queue_budget_bytes: 1 << 20,
            ..WriterParams::default()
        };
        let handle = WriterHandle::spawn(&config, roomy).expect("spawn writer");
        assert!(handle.push_bulk(BulkItem::Events(vec![event(1, 1, at(1))])));
        assert_eq!(handle.dropped_bulk_total(), 0);
        let mut handle = handle;
        handle.stop_and_checkpoint().expect("stop writer");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    #[test]
    fn writer_persists_events_closes_spans_and_executes_backfill_jobs() {
        let path = temp_db();
        let _ = std::fs::remove_file(&path);
        let config = StorageConfig::sqlite(&path, 5_000);
        let mut handle =
            WriterHandle::spawn(&config, WriterParams::default()).expect("spawn writer");
        let at = |secs: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        let trace = TraceId::new(3);

        // Open the span, push an in-window NULL-call event, then close with a window backfill job (as CallEnd does).
        handle
            .call_start(CallStartWrite {
                trace_id: trace,
                session_id: Some("s1".to_string()),
                call_id: "c1".to_string(),
                host_pid: 9,
                started_at: at(5),
            })
            .expect("call start");
        handle.push_bulk(BulkItem::Events(vec![event(3, 1, at(10))]));
        handle.push_bulk(BulkItem::Events(vec![event(3, 2, at(11))]));
        handle.push_bulk(BulkItem::EndOfDrain);
        let gate = handle.bulk_pushed_count();
        handle
            .call_close(CallCloseWrite {
                trace_id: trace,
                session_id: Some("s1".to_string()),
                call_id: "c1".to_string(),
                host_pid: 9,
                ended_at: at(20),
                status: "success".to_string(),
                jobs: vec![(job(3, at(5), at(20)), gate)],
            })
            .expect("call close");

        // Give the writer an idle tick so the queued job executes.
        std::thread::sleep(Duration::from_millis(60));
        handle.stop_and_checkpoint().expect("stop writer");

        let storage = open_storage_backend(&config).expect("reopen storage");
        let spans = storage.list_call_spans(trace).expect("list spans");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].call_id, "c1");
        assert_eq!(spans[0].ended_at, Some(at(20)));
        let ns = |time: SystemTime| -> i64 {
            time.duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                .try_into()
                .unwrap()
        };
        let unassigned = storage
            .unassigned_events_in_window(trace, Some("s1"), ns(at(5)), ns(at(20)), 100)
            .expect("window query");
        assert!(
            unassigned.is_empty(),
            "expected backfill to assign window events"
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    #[test]
    fn writer_resumes_pending_jobs_after_a_crash() {
        let path = temp_db();
        let _ = std::fs::remove_file(&path);
        let config = StorageConfig::sqlite(&path, 5_000);
        let at = |secs: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        let trace = TraceId::new(4);
        {
            // Simulate a daemon that closed a span, enqueued a window job, then crashed before it ran.
            let mut storage = open_storage_backend(&config).expect("prepopulate");
            storage
                .call_start(trace, Some("s1"), "c1", 9, at(5))
                .unwrap();
            storage
                .append_events_batch(&[event(4, 1, at(10)), event(4, 2, at(11))])
                .unwrap();
            storage
                .call_end_with_jobs(
                    trace,
                    Some("s1"),
                    "c1",
                    9,
                    at(20),
                    "success",
                    &[job(4, at(5), at(20))],
                )
                .unwrap();
        }
        // Boot: the writer thread must reset/resume and execute the pending job.
        let mut handle =
            WriterHandle::spawn(&config, WriterParams::default()).expect("spawn writer");
        std::thread::sleep(Duration::from_millis(60));
        handle.stop_and_checkpoint().expect("stop writer");
        let storage = open_storage_backend(&config).expect("reopen");
        let ns = |time: SystemTime| -> i64 {
            time.duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                .try_into()
                .unwrap()
        };
        let unassigned = storage
            .unassigned_events_in_window(trace, Some("s1"), ns(at(5)), ns(at(20)), 100)
            .expect("window query");
        assert!(
            unassigned.is_empty(),
            "boot resume should have executed the pending job"
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }
}
