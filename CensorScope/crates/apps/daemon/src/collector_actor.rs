//! Dedicated owner of the eBPF collector and its control priority queue.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use collector_binding::{TraceBindingHandle, TraceBindingRequest};
use collector_instance::{
    CollectorError, CollectorInstance, CollectorPollBatch, CollectorRawBatch,
};
use collector_stats::CollectorStats;
use config_core::daemon::EbpfCollectorConfig;
use ebpf_collector::EbpfCollector;
use model_core::ids::TraceId;
use model_core::process::ProcessRecord;
use serde::{Deserialize, Serialize};
use storage_spool::{RecordKind, SpoolConfig, SpoolWriter};

/// Versioned collector payload stored in one spool record.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CollectorBatchEnvelope {
    pub version: u16,
    pub batch: CollectorPollBatch,
}

/// Durable sink for collector batches.
pub struct SpoolBatchSink {
    writer: SpoolWriter,
}

impl SpoolBatchSink {
    pub fn open(path: impl AsRef<std::path::Path>, config: SpoolConfig) -> Result<Self, String> {
        SpoolWriter::open(path, config)
            .map(|writer| Self { writer })
            .map_err(|error| error.to_string())
    }
}

impl CollectorBatchSink for SpoolBatchSink {
    fn append(&mut self, batch: CollectorPollBatch) -> Result<(), String> {
        let envelope = CollectorBatchEnvelope { version: 1, batch };
        let payload = serde_json::to_vec(&envelope).map_err(|error| error.to_string())?;
        self.writer
            .append(RecordKind::Event, &payload)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn flush(&mut self) -> Result<(), String> {
        self.writer.sync().map_err(|error| error.to_string())
    }

    fn append_raw(&mut self, batch: CollectorRawBatch) -> Result<(), String> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&(batch.events.len() as u32).to_le_bytes());
        for event in batch.events {
            let len = u32::try_from(event.len()).map_err(|_| "raw event too large".to_string())?;
            payload.extend_from_slice(&len.to_le_bytes());
            payload.extend_from_slice(&event);
        }
        let diagnostics = if batch.diagnostics.is_empty() {
            Vec::new()
        } else {
            serde_json::to_vec(&batch.diagnostics).map_err(|error| error.to_string())?
        };
        payload.extend_from_slice(&(diagnostics.len() as u32).to_le_bytes());
        payload.extend_from_slice(&diagnostics);
        self.writer
            .append(RecordKind::RawEvent, &payload)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

/// Receives collected batches after the actor has drained the kernel transport.
/// The implementation is replaced by the durable spool sink during daemon
/// assembly; the actor does not depend on a particular persistence backend.
pub trait CollectorBatchSink: Send + 'static {
    fn append(&mut self, batch: CollectorPollBatch) -> Result<(), String>;

    fn append_raw(&mut self, batch: CollectorRawBatch) -> Result<(), String> {
        let _ = batch;
        Err("raw spool is unsupported by this sink".to_string())
    }

    fn flush(&mut self) -> Result<(), String> {
        Ok(())
    }
}

enum Command {
    Bind(
        TraceBindingRequest,
        Sender<Result<TraceBindingHandle, CollectorError>>,
    ),
    Seed(
        TraceId,
        Vec<ProcessRecord>,
        Sender<Result<(), CollectorError>>,
    ),
    Unbind(TraceId, Sender<Result<(), CollectorError>>),
    Stats(Sender<CollectorStats>),
    Stop(Sender<Result<(), String>>),
}

impl Command {
    fn priority(&self) -> u8 {
        match self {
            Self::Stop(_) => 0,
            Self::Unbind(_, _) => 1,
            Self::Bind(_, _) => 2,
            Self::Seed(_, _, _) => 3,
            Self::Stats(_) => 4,
        }
    }
}

/// Client endpoint for the collector actor.
pub struct CollectorActorHandle {
    commands: Sender<Command>,
    health: Arc<Mutex<Option<String>>>,
    join: Option<JoinHandle<()>>,
}

impl CollectorActorHandle {
    pub fn spawn(
        config: EbpfCollectorConfig,
        sink: impl CollectorBatchSink,
    ) -> Result<Self, String> {
        let (commands, command_rx) = mpsc::channel();
        let health = Arc::new(Mutex::new(None));
        let actor_health = Arc::clone(&health);
        let (started_tx, started_rx) = mpsc::channel();
        let join = thread::Builder::new()
            .name("censorscope-collector".to_string())
            .spawn(move || run_actor(config, sink, command_rx, actor_health, started_tx))
            .map_err(|error| format!("spawn collector actor: {error}"))?;
        started_rx
            .recv()
            .map_err(|_| "collector actor stopped during startup".to_string())??;
        Ok(Self {
            commands,
            health,
            join: Some(join),
        })
    }

    pub fn bind_trace(
        &self,
        request: TraceBindingRequest,
    ) -> Result<TraceBindingHandle, CollectorError> {
        let (reply, result) = reply_channel();
        self.commands
            .send(Command::Bind(request, reply))
            .map_err(|_| actor_closed())?;
        result.recv().map_err(|_| actor_closed())?
    }

    pub fn seed_trace_memberships(
        &self,
        trace_id: TraceId,
        records: Vec<ProcessRecord>,
    ) -> Result<(), CollectorError> {
        let (reply, result) = reply_channel();
        self.commands
            .send(Command::Seed(trace_id, records, reply))
            .map_err(|_| actor_closed())?;
        result.recv().map_err(|_| actor_closed())?
    }

    pub fn unbind_trace(&self, trace_id: TraceId) -> Result<(), CollectorError> {
        let (reply, result) = reply_channel();
        self.commands
            .send(Command::Unbind(trace_id, reply))
            .map_err(|_| actor_closed())?;
        result.recv().map_err(|_| actor_closed())?
    }

    pub fn stats(&self) -> Result<CollectorStats, CollectorError> {
        let (reply, result) = mpsc::channel();
        self.commands
            .send(Command::Stats(reply))
            .map_err(|_| actor_closed())?;
        result.recv().map_err(|_| actor_closed())
    }

    pub fn health(&self) -> Option<String> {
        self.health.lock().ok().and_then(|value| value.clone())
    }

    pub fn shutdown(&mut self) -> Result<(), String> {
        let (reply, result) = mpsc::channel();
        self.commands
            .send(Command::Stop(reply))
            .map_err(|_| "collector actor channel closed".to_string())?;
        let outcome = result
            .recv()
            .map_err(|_| "collector actor stopped before shutdown".to_string())?;
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        outcome
    }
}

fn run_actor(
    config: EbpfCollectorConfig,
    mut sink: impl CollectorBatchSink,
    commands: Receiver<Command>,
    health: Arc<Mutex<Option<String>>>,
    started: Sender<Result<(), String>>,
) {
    let mut collector = EbpfCollector::new(config);
    let _ = started.send(Ok(()));
    let mut pending = VecDeque::new();
    loop {
        while let Ok(command) = commands.try_recv() {
            pending.push_back(command);
        }
        if pending.is_empty() {
            match commands.recv_timeout(Duration::from_millis(2)) {
                Ok(command) => pending.push_back(command),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    let _ = sink.flush();
                    return;
                }
            }
        }
        if let Some(index) = pending
            .iter()
            .enumerate()
            .min_by_key(|(_, command)| command.priority())
            .map(|(index, _)| index)
        {
            let command = pending.remove(index).expect("priority command exists");
            if handle_command(command, &mut collector, &mut sink) {
                return;
            }
        }
        match collector.poll_raw_batch() {
            Ok(Some(batch)) => {
                if let Err(error) = sink.append_raw(batch) {
                    set_health(&health, format!("spool sink: {error}"));
                    return;
                }
            }
            Ok(None) => {}
            Err(error) => {
                set_health(&health, format!("collector poll: {}", error.message));
                return;
            }
        }
    }
}

fn handle_command(
    command: Command,
    collector: &mut EbpfCollector,
    sink: &mut impl CollectorBatchSink,
) -> bool {
    match command {
        Command::Bind(request, reply) => {
            let _ = reply.send(collector.bind_trace(&request));
        }
        Command::Seed(trace_id, records, reply) => {
            let _ = reply.send(collector.seed_trace_memberships(trace_id, records));
        }
        Command::Unbind(trace_id, reply) => {
            let _ = reply.send(collector.unbind_trace(trace_id));
        }
        Command::Stats(reply) => {
            let _ = reply.send(collector.stats());
        }
        Command::Stop(reply) => {
            let result = sink.flush();
            let _ = reply.send(result);
            return true;
        }
    }
    false
}

fn set_health(health: &Arc<Mutex<Option<String>>>, message: String) {
    if let Ok(mut value) = health.lock() {
        *value = Some(message);
    }
}

fn reply_channel<T>() -> (
    Sender<Result<T, CollectorError>>,
    Receiver<Result<T, CollectorError>>,
) {
    mpsc::channel()
}

fn actor_closed() -> CollectorError {
    CollectorError::new("collector_actor", "collector actor is unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn control_commands_precede_data_polling() {
        let commands = [
            Command::Stats(mpsc::channel().0),
            Command::Bind(
                TraceBindingRequest {
                    trace_id: TraceId::new(1),
                    root_identity: model_core::process::ProcessIdentity::new(1),
                    root_observation: model_core::process::ProcessObservation::host(
                        model_core::process::HostProcessCoordinates::new(1, 1),
                    ),
                    root_namespace_pid: 1,
                    profile_snapshot: config_core::trace_snapshot::CaptureProfileSnapshot {
                        captured_at: std::time::SystemTime::UNIX_EPOCH,
                        profile_name: model_core::ids::ProfileName::new("test"),
                        capability_requests: Vec::new(),
                    },
                    requested_capabilities: Vec::new(),
                },
                mpsc::channel().0,
            ),
        ];
        let index = commands
            .iter()
            .enumerate()
            .min_by_key(|(_, command)| command.priority())
            .map(|(index, _)| index)
            .unwrap();
        assert_eq!(index, 1);
    }

    #[test]
    fn spool_sink_writes_versioned_batches() {
        let path =
            std::env::temp_dir().join(format!("collector-actor-spool-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        let mut sink = SpoolBatchSink::open(&path, SpoolConfig::default()).unwrap();
        sink.append(CollectorPollBatch::default()).unwrap();
        let mut reader = storage_spool::SpoolReader::open(
            &path,
            storage_spool::SpoolPosition {
                segment: 0,
                offset: 0,
            },
        )
        .unwrap();
        let records = reader.read_batch(1).unwrap();
        let envelope: CollectorBatchEnvelope = serde_json::from_slice(&records[0].payload).unwrap();
        assert_eq!(envelope.version, 1);
        assert!(envelope.batch.observations.is_empty());
    }
}
