use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyRoot {
    /// Global baseline rules. Every rule is `file|exec|net ACTION TARGET`.
    pub rules: Vec<String>,
    /// Named policy groups bindable to domains.
    pub groups: BTreeMap<String, GroupPolicy>,
    pub domains: Vec<DomainRef>,
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
