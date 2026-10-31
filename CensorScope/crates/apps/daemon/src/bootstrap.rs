//! Assembly boundary for the control server, trace runtime, collector, and storage.

use std::io;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use config_core::capture_profile::CaptureProfile;
use config_core::daemon::{EbpfCollectorConfig, ExternalEventsConfig, WriterConfig};
use control_contract::reply::ControlError;
use storage_factory::StorageConfig;
use uds_control_server::{PeerCredentials, UdsControlConnection, UdsControlServer};

use crate::service_host::DaemonServiceHost;

/// Fully assembled daemon service before the Unix socket loop starts.
pub struct LocalDaemonServer {
    control_tx: SyncSender<WorkerCommand>,
    external_events: Option<external_events_adapter::ExternalEventServer>,
    worker: Option<JoinHandle<()>>,
}

enum WorkerCommand {
    Request {
        request: Vec<u8>,
        peer: Option<PeerCredentials>,
        reply: SyncSender<Vec<u8>>,
    },
    ExternalEvent {
        event: agentcensor_events::ExternalEvent,
        reply: SyncSender<Result<(), ControlError>>,
    },
    Shutdown {
        reply: SyncSender<Result<(), ControlError>>,
    },
}

impl LocalDaemonServer {
    // Operator config knobs are plumbed into the daemon as separate parameters.
    /// Build a daemon instance with an empty in-memory trace registry.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        storage: &StorageConfig,
        profile: CaptureProfile,
        ebpf: EbpfCollectorConfig,
        writer: WriterConfig,
        active_trace_max: u32,
        session_env_name: String,
        external_events: ExternalEventsConfig,
    ) -> Result<Self, ControlError> {
        let host = DaemonServiceHost::build(
            storage,
            writer,
            profile,
            ebpf,
            active_trace_max,
            session_env_name,
        )?;
        let queue_capacity = usize::try_from(external_events.queue_capacity).unwrap_or(8_192);
        let (control_tx, control_rx) = mpsc::sync_channel(queue_capacity.max(1_024));
        let worker = thread::Builder::new()
            .name("censorscope-control-worker".to_string())
            .spawn(move || run_worker(host, control_rx))
            .map_err(|error| ControlError::new("control_worker", error.to_string()))?;
        let external_server = if external_events.enabled {
            let event_tx = control_tx.clone();
            let handler = std::sync::Arc::new(move |event| {
                let (reply_tx, reply_rx) = mpsc::sync_channel(1);
                event_tx
                    .send(WorkerCommand::ExternalEvent {
                        event,
                        reply: reply_tx,
                    })
                    .map_err(|_| "Scope event worker stopped".to_string())?;
                reply_rx
                    .recv()
                    .map_err(|_| "Scope event worker stopped before reply".to_string())?
                    .map_err(|error| format!("{}: {}", error.code, error.message))
            });
            Some(
                external_events_adapter::ExternalEventServer::start(
                    &external_events.socket_path,
                    external_events.socket_permissions.mode,
                    handler,
                )
                .map_err(|error| ControlError::new("external_events", error.to_string()))?,
            )
        } else {
            None
        };
        Ok(Self {
            control_tx,
            external_events: external_server,
            worker: Some(worker),
        })
    }

    /// Decode and execute one complete control request.
    pub fn handle_request(&mut self, request: &[u8]) -> Vec<u8> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        if self
            .control_tx
            .send(WorkerCommand::Request {
                request: request.to_vec(),
                peer: None,
                reply: reply_tx,
            })
            .is_err()
        {
            return Vec::new();
        }
        reply_rx.recv().unwrap_or_default()
    }

    /// Unbind active collectors and checkpoint persistent storage.
    pub fn shutdown(&mut self) -> Result<(), ControlError> {
        if let Some(mut external_events) = self.external_events.take() {
            external_events.stop();
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.control_tx
            .send(WorkerCommand::Shutdown { reply: reply_tx })
            .map_err(|error| ControlError::new("control_worker", error.to_string()))?;
        let result = reply_rx
            .recv()
            .map_err(|error| ControlError::new("control_worker", error.to_string()))?;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        result
    }

    pub(crate) fn progress_control_connection(
        &mut self,
        connection: &mut UdsControlConnection,
    ) -> io::Result<bool> {
        connection.try_progress_with_dispatch(|request, peer| self.dispatch(request, Some(peer)))
    }

    fn dispatch(
        &self,
        request: Vec<u8>,
        peer: Option<PeerCredentials>,
    ) -> io::Result<Receiver<Vec<u8>>> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.control_tx
            .try_send(WorkerCommand::Request {
                request,
                peer,
                reply: reply_tx,
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    io::Error::new(io::ErrorKind::WouldBlock, "control worker queue is full")
                }
                mpsc::TrySendError::Disconnected(_) => {
                    io::Error::new(io::ErrorKind::BrokenPipe, "control worker stopped")
                }
            })?;
        Ok(reply_rx)
    }
}

fn run_worker(host: DaemonServiceHost, control_rx: mpsc::Receiver<WorkerCommand>) {
    let mut server = UdsControlServer::new(host);
    loop {
        match control_rx.recv_timeout(Duration::from_millis(10)) {
            Ok(WorkerCommand::Request {
                request,
                peer,
                reply,
            }) => {
                let response = match peer {
                    Some(peer) => server.handle_bytes_for_peer(&request, peer),
                    None => server.handle_bytes(&request),
                };
                let _ = reply.send(response);
            }
            Ok(WorkerCommand::ExternalEvent { event, reply }) => {
                let result = server.service_mut().ingest_external_event(event);
                let _ = reply.send(result);
            }
            Ok(WorkerCommand::Shutdown { reply }) => {
                let result = server.service_mut().shutdown();
                let _ = reply.send(result);
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if let Err(error) = server.service_mut().drain_live_events() {
            tracing::warn!(code = %error.code, message = %error.message, "live event drain failed");
        }
    }
}
