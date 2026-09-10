//! Assembly boundary for the control server, trace runtime, collector, and storage.

use std::io;

use config_core::capture_profile::CaptureProfile;
use config_core::daemon::{EbpfCollectorConfig, WriterConfig};
use control_contract::reply::ControlError;
use storage_factory::StorageConfig;
use uds_control_server::{UdsControlConnection, UdsControlServer};

use crate::service_host::DaemonServiceHost;

/// Fully assembled daemon service before the Unix socket loop starts.
pub struct LocalDaemonServer {
    server: UdsControlServer<DaemonServiceHost>,
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
        payload_max_trace_bytes: u64,
        payload_max_segment_bytes: u64,
        active_trace_max: u32,
        session_env_name: String,
    ) -> Result<Self, ControlError> {
        Ok(Self {
            server: UdsControlServer::new(DaemonServiceHost::build(
                storage,
                writer,
                profile,
                ebpf,
                payload_max_trace_bytes,
                payload_max_segment_bytes,
                active_trace_max,
                session_env_name,
            )?),
        })
    }

    /// Decode and execute one complete control request.
    pub fn handle_request(&mut self, request: &[u8]) -> Vec<u8> {
        self.server.handle_bytes(request)
    }

    /// Drain pending lifecycle observations into runtime state and SQLite.
    pub fn drain_live_events(&mut self) -> Result<(), ControlError> {
        self.server.service_mut().drain_live_events()
    }

    /// Unbind active collectors and checkpoint persistent storage.
    pub fn shutdown(&mut self) -> Result<(), ControlError> {
        self.server.service_mut().shutdown()
    }

    pub(crate) fn progress_control_connection(
        &mut self,
        connection: &mut UdsControlConnection,
    ) -> io::Result<bool> {
        connection.try_progress(&mut self.server)
    }
}
