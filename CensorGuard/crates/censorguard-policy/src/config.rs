use censorguard_common::config::DaemonConfig;
use censorguard_common::protocol::PolicySource;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyRoot {
    /// Global baseline rules. Every rule is `file|exec|net ACTION TARGET`.
    pub rules: Vec<String>,
    /// Named policy groups bindable to domains.
    pub groups: BTreeMap<String, GroupPolicy>,
    /// Publication metadata in private snapshots, separate from editable group YAML.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub group_sources: BTreeMap<String, PolicySource>,
    pub domains: Vec<DomainRef>,
    /// Runtime paths and daemon settings. They do not affect policy compilation.
    pub daemon: DaemonConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DomainRef {
    pub name: String,
    #[serde(default)]
    pub group: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GroupPolicy {
    /// Unified rule syntax: `file|exec|net allow|deny[+audit] ...`.
    pub rules: Vec<String>,
}
