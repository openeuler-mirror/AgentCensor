//! Collector capability and guarantee contracts.

use model_core::capability::{CapabilityDescriptor, CapabilitySet};
use model_core::ids::CollectorName;

/// Capabilities and identity advertised by a collector implementation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectorDescriptor {
    pub name: CollectorName,
    pub capabilities: Vec<CapabilityDescriptor>,
}

impl CollectorDescriptor {
    pub fn capability_set(&self) -> CapabilitySet {
        CapabilitySet::new(
            self.capabilities
                .iter()
                .map(|descriptor| descriptor.capability.clone()),
        )
    }
}

/// Explanation for a capability that could not be bound to a collector.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityBindingFailure {
    pub collector: CollectorName,
    pub detail: String,
}
