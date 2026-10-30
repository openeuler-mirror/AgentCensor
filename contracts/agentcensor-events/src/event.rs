use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::correlation::CorrelationContext;
use crate::decision::{PolicyContext, SecurityCategory, SecurityDecision};
use crate::subject::EventSubject;
use crate::target::EventTarget;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    CensorGuard,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SecurityEvent {
    pub category: SecurityCategory,
    pub operation: String,
    pub decision: SecurityDecision,
    pub target: EventTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PolicyContext>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExternalEvent {
    pub schema_version: u16,
    pub source: EventSource,
    pub source_instance_id: String,
    pub source_sequence: u64,
    pub occurred_at_ns: u64,
    pub subject: EventSubject,
    pub event: SecurityEvent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation: Option<CorrelationContext>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, String>,
}
