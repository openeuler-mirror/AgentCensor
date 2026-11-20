use crate::error::{Error, Result};
use fancy_regex::Regex;
use serde::{Deserialize, Serialize};
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

/// User-defined rules; category selection and run lifetime belong to deployment.
#[derive(Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CustomSettings {
    pub epoch: u64,
    pub dictionaries: BTreeMap<String, Vec<String>>,
    pub patterns: BTreeMap<String, Vec<String>>,
    pub default_labels: BTreeSet<String>,
}
impl Default for CustomSettings {
    fn default() -> Self {
        Self {
            epoch: default_epoch(),
            dictionaries: BTreeMap::new(),
            patterns: BTreeMap::new(),
            default_labels: default_labels(),
        }
    }
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

impl Default for PolicyConfig {
    fn default() -> Self {
        serde_json::from_str("{}").expect("constant policy configuration")
    }
}

/// Validated immutable snapshot. No public field permits mutation after validation.
#[derive(Clone)]
pub struct PrivacyPolicy {
    config: PolicyConfig,
    ttl: Duration,
    categories: crate::categories::Categories,
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
    pub fn from_custom(custom: CustomSettings) -> Result<Self> {
        Self::validate(PolicyConfig {
            epoch: custom.epoch,
            dictionaries: custom.dictionaries,
            patterns: custom.patterns,
            default_labels: custom.default_labels,
            ..PolicyConfig::default()
        })
    }
    pub fn custom_rules(&self) -> CustomSettings {
        CustomSettings {
            epoch: self.config.epoch,
            dictionaries: self.config.dictionaries.clone(),
            patterns: self.config.patterns.clone(),
            default_labels: self.config.default_labels.clone(),
        }
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
        Ok(Self {
            config,
            ttl,
            categories: None,
        })
    }
    pub fn categories(&self) -> &crate::categories::Categories {
        &self.categories
    }
    pub fn allows_category(&self, kind: &str) -> bool {
        crate::categories::allows(&self.categories, kind)
    }
    pub fn with_deployment(
        mut self,
        ttl: Option<f64>,
        categories: crate::categories::Categories,
    ) -> Result<Self> {
        if let Some(items) = &categories {
            for kind in items {
                if !crate::categories::BUILTIN_CATEGORIES.contains(&kind.as_str())
                    && !self
                        .config
                        .dictionaries
                        .keys()
                        .chain(self.config.patterns.keys())
                        .any(|custom| crate::categories::canonical(custom) == kind)
                {
                    return Err(Error::ConfigInvalid);
                }
            }
        }
        if let Some(ttl) = ttl {
            self.ttl = Duration::try_from_secs_f64(ttl).map_err(|_| Error::ConfigInvalid)?;
            if self.ttl.is_zero() {
                return Err(Error::ConfigInvalid);
            }
            self.config.ttl_seconds = ttl;
        }
        // A deployment category list is authoritative, including NUMBER.
        self.config.mask_money = crate::categories::allows(&categories, "MONEY");
        self.config.mask_dates = crate::categories::allows(&categories, "DATE");
        self.config.mask_all_numbers = crate::categories::allows(&categories, "NUMBER");
        self.categories = categories;
        Ok(self)
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
