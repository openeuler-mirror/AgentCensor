//! CensorScope process tracking daemon.

mod attribution;
mod bootstrap;
mod ebpf_resolve;
mod peer_identity;
mod service_host;
mod socket_loop;
mod writer;

pub use bootstrap::LocalDaemonServer;
pub use ebpf_resolve::{EbpfResolution, resolve_ebpf_collector_config};
pub use socket_loop::DaemonRunError;
