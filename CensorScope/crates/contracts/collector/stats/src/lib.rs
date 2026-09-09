//! Collector health and drop-counter contracts.

use std::time::SystemTime;

use model_core::ids::CollectorName;

/// Number of collector events discarded for one reason.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropCounter {
    pub reason: String,
    pub count: u64,
}

/// Runtime health snapshot reported by one collector instance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectorStats {
    pub collector_name: CollectorName,
    pub active_bindings: usize,
    pub last_heartbeat_at: SystemTime,
    pub dropped: Vec<DropCounter>,
}
