//! Evidence retained independently of resolved session and call identifiers.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributionBasis {
    #[default]
    Unknown,
    CollectorContext,
    Procfs,
    CallContext,
    TimeWindow,
    Ambiguous,
    Conflict,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AttributionEvidence {
    pub captured_session_id: Option<String>,
    pub captured_call_id: Option<String>,
    pub procfs_session_id: Option<String>,
    pub session_basis: AttributionBasis,
    pub call_basis: AttributionBasis,
    pub candidate_call_ids: Vec<String>,
}
