use crate::error::{Error, Result};
use fancy_regex::Regex;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

fn default_epoch() -> u64 {
    1
}
fn yes() -> bool {
    true
}
fn default_ttl() -> f64 {
    3600.0
}
fn default_labels() -> BTreeSet<String> {
    BTreeSet::from(["confidential".into()])
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyConfig {
    #[serde(default = "default_epoch")]
    epoch: u64,
    #[serde(default)]
    dictionaries: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    patterns: BTreeMap<String, Vec<String>>,
    #[serde(default = "yes")]
    mask_money: bool,
    #[serde(default = "yes")]
    mask_dates: bool,
    #[serde(default)]
    mask_all_numbers: bool,
    #[serde(default = "default_labels")]
    default_labels: BTreeSet<String>,
    #[serde(default = "default_ttl")]
    ttl_seconds: f64,
}

/// Validated immutable snapshot. No public field permits mutation after validation.
#[derive(Clone)]
pub struct PrivacyPolicy {
    config: PolicyConfig,
    ttl: Duration,
}

impl Default for PrivacyPolicy {
    fn default() -> Self {
        Self::from_json("{}").expect("constant policy")
    }
}

impl PrivacyPolicy {
    pub fn from_json(raw: &str) -> Result<Self> {
        let value = crate::json::from_slice(raw.as_bytes()).map_err(|_| Error::PolicyInvalid)?;
        Self::validate(serde_json::from_value(value).map_err(|_| Error::PolicyInvalid)?)
    }
    pub fn from_yaml(raw: &str) -> Result<Self> {
        Self::validate(serde_yaml::from_str(raw).map_err(|_| Error::PolicyInvalid)?)
    }
    fn validate(mut config: PolicyConfig) -> Result<Self> {
        let ttl =
            Duration::try_from_secs_f64(config.ttl_seconds).map_err(|_| Error::PolicyInvalid)?;
        if config.epoch == 0
            || ttl.is_zero()
            || config.default_labels.is_empty()
            || config.default_labels.iter().any(String::is_empty)
        {
            return Err(Error::PolicyInvalid);
        }
        for (name, values) in config
            .dictionaries
            .iter_mut()
            .chain(config.patterns.iter_mut())
        {
            validate_entity_type(name)?;
            if values.iter().any(String::is_empty) {
                return Err(Error::PolicyInvalid);
            }
            let mut seen = BTreeSet::new();
            values.retain(|value| seen.insert(value.clone()));
        }
        for patterns in config.patterns.values() {
            for pattern in patterns {
                let regex = Regex::new(pattern).map_err(|_| Error::PolicyInvalid)?;
                if regex.is_match("").map_err(|_| Error::PolicyInvalid)? {
                    return Err(Error::PolicyInvalid);
                }
            }
        }
        Ok(Self { config, ttl })
    }
    pub fn epoch(&self) -> u64 {
        self.config.epoch
    }
    pub fn ttl(&self) -> Duration {
        self.ttl
    }
    pub fn labels(&self) -> &BTreeSet<String> {
        &self.config.default_labels
    }
    pub fn mask_money(&self) -> bool {
        self.config.mask_money
    }
    pub fn mask_dates(&self) -> bool {
        self.config.mask_dates
    }
    pub fn mask_all_numbers(&self) -> bool {
        self.config.mask_all_numbers
    }
    pub(crate) fn dictionaries(&self) -> &BTreeMap<String, Vec<String>> {
        &self.config.dictionaries
    }
    pub(crate) fn patterns(&self) -> &BTreeMap<String, Vec<String>> {
        &self.config.patterns
    }
}

pub fn validate_entity_type(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 40
        || !value.as_bytes()[0].is_ascii_uppercase()
        || !value
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
    {
        return Err(Error::PolicyInvalid);
    }
    Ok(())
}
