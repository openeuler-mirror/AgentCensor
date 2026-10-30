use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EventSubject {
    pub pid: u32,
    pub tgid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comm: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}
