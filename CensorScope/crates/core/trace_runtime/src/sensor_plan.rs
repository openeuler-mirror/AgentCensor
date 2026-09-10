//! Immutable trace-binding plan derived from contracts and snapshots.

use std::collections::BTreeMap;

use collector_capability::CollectorDescriptor;
use config_core::trace_snapshot::CaptureProfileSnapshot;
use model_core::capability::{Capability, RequestMode};
use model_core::ids::CollectorName;

/// Capabilities assigned to one collector for a trace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectorPlan {
    pub collector_name: CollectorName,
    pub capabilities: Vec<Capability>,
}

/// Immutable collector binding plan produced during trace creation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SensorPlan {
    pub profile_name: model_core::ids::ProfileName,
    pub collectors: Vec<CollectorPlan>,
}

/// Required capability that no available collector can provide.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NegotiationFailure {
    pub capability: Capability,
    pub detail: String,
}

impl SensorPlan {
    /// Assign each required profile capability to an available collector.
    pub fn negotiate(
        profile: &CaptureProfileSnapshot,
        collectors: &[CollectorDescriptor],
    ) -> Result<Self, Vec<NegotiationFailure>> {
        let mut bindings: BTreeMap<CollectorName, Vec<Capability>> = BTreeMap::new();
        let mut missing_required = Vec::new();

        for request in &profile.capability_requests {
            let maybe_collector = collectors
                .iter()
                .find(|collector| collector.capability_set().contains(&request.capability));

            match maybe_collector {
                Some(collector) => bindings
                    .entry(collector.name.clone())
                    .or_default()
                    .push(request.capability.clone()),
                None if request.mode == RequestMode::Required => {
                    missing_required.push(NegotiationFailure {
                        capability: request.capability,
                        detail: "no collector declared support".to_string(),
                    })
                }
                None => {}
            }
        }

        if !missing_required.is_empty() {
            return Err(missing_required);
        }

        let collectors = bindings
            .into_iter()
            .map(|(collector_name, capabilities)| CollectorPlan {
                collector_name,
                capabilities,
            })
            .collect();

        Ok(Self {
            profile_name: profile.profile_name.clone(),
            collectors,
        })
    }
}
