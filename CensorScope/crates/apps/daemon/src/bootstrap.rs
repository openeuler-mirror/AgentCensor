//! Assembly boundary for the control server, trace runtime, collector, and storage.

use std::io;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use config_core::capture_profile::CaptureProfile;
use config_core::daemon::{EbpfCollectorConfig, WriterConfig};
use control_contract::reply::ControlError;
use storage_factory::StorageConfig;
use uds_control_server::{PeerCredentials, UdsControlConnection, UdsControlServer};

use crate::service_host::DaemonServiceHost;

/// Fully assembled daemon service before the Unix socket loop starts.
pub struct LocalDaemonServer {
    control_tx: SyncSender<WorkerCommand>,
    worker: Option<JoinHandle<()>>,
}

enum WorkerCommand {
    Request {
        request: Vec<u8>,
        peer: Option<PeerCredentials>,
        reply: SyncSender<Vec<u8>>,
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
    ) -> Result<Self, ControlError> {
        let host = DaemonServiceHost::build(
            storage,
            writer,
            profile,
            ebpf,
            active_trace_max,
            session_env_name,
        )?;
        let (control_tx, control_rx) = mpsc::sync_channel(1_024);
        let worker = thread::Builder::new()
            .name("censorscope-control-worker".to_string())
            .spawn(move || run_worker(host, control_rx))
            .map_err(|error| ControlError::new("control_worker", error.to_string()))?;
        Ok(Self {
            control_tx,
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
