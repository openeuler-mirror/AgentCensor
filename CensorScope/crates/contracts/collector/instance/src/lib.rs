//! Lifecycle contracts for live collector instances.

use collector_binding::{TraceBindingHandle, TraceBindingRequest};
use collector_capability::CollectorDescriptor;
use collector_event::{RawCollectorEvent, RawPayloadSegment};
use collector_stats::CollectorStats;
use model_core::ids::TraceId;
use serde::{Deserialize, Serialize};

/// Error returned by a collector during setup, polling, or teardown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectorError {
    pub stage: String,
    pub message: String,
}

impl CollectorError {
    pub fn new(stage: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            stage: stage.into(),
            message: message.into(),
        }
    }
}

/// Runtime contract implemented by process lifecycle collectors.
pub trait CollectorInstance {
    /// Returns the collector identity and capabilities used during negotiation.
    fn descriptor(&self) -> &CollectorDescriptor;
    /// Starts collecting lifecycle events for one trace root.
    fn bind_trace(
        &mut self,
        request: &TraceBindingRequest,
    ) -> Result<TraceBindingHandle, CollectorError>;
    /// Stops collection and releases resources associated with a trace.
    fn unbind_trace(&mut self, trace_id: TraceId) -> Result<(), CollectorError>;
    /// Polls currently available observations without blocking.
    fn poll_events(&mut self) -> Result<Vec<RawCollectorEvent>, CollectorError> {
        Ok(self.poll_batch()?.observations)
    }
    /// Polls observations together with any batch-level collector state.
    fn poll_batch(&mut self) -> Result<CollectorPollBatch, CollectorError>;
    /// Drains raw transport records without decoding them.
    fn poll_raw_batch(&mut self) -> Result<Option<CollectorRawBatch>, CollectorError> {
        Ok(None)
    }
    /// Kernel descriptor that becomes readable when the transport holds data.
    ///
    /// Lets the daemon wake on captured records instead of polling on a timer.
    /// `None` while the collector has no transport to watch.
    fn transport_fd(&self) -> Option<std::os::fd::RawFd> {
        None
    }
    /// Drain the kernel transport buffer into userspace without decoding.
    ///
    /// Best-effort: call after expensive processing to shrink the ring-buffer
    /// starvation window. Default is a no-op.
    fn flush_transport(&mut self) -> Result<(), CollectorError> {
        Ok(())
    }
    /// Returns the latest health and drop counters for this instance.
    fn stats(&self) -> CollectorStats;
}

/// Raw records copied from a collector transport before userspace decoding.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CollectorRawBatch {
    pub events: Vec<Vec<u8>>,
    pub diagnostics: Vec<RawCollectorEvent>,
}

/// Batch of observations drained from a collector transport.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CollectorPollBatch {
    pub observations: Vec<RawCollectorEvent>,
    pub payload_segments: Vec<RawPayloadSegment>,
}
