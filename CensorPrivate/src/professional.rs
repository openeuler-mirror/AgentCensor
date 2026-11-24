//! Native privacy detection using local ONNX resources.
mod batch;
mod config;
mod model;
mod rules;
pub use config::{configured_ner_batch_size, configured_ner_device, NerDevice};

use crate::{
    detection::{validate_spans, Span},
    error::{Error, Result},
    policy::PrivacyPolicy,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::Mutex,
};
pub const MODEL_ID: &str = "shibing624/bert4ner-base-chinese";

pub struct ProfessionalConfig {
    pub model_directory: Option<PathBuf>,
    pub runtime_library: Option<PathBuf>,
    /// ONNX intra-op threads; inter-op remains one to avoid oversubscription.
    pub ner_threads: usize,
    pub ner_device: NerDevice,
    /// Maximum windows per inference, per device; incomplete batches run immediately.
    pub ner_batch_size: usize,
    pub window_timeout_seconds: u64,
    pub categories: crate::categories::Categories,
}
impl Default for ProfessionalConfig {
    fn default() -> Self {
        let prefix = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent()?.parent().map(PathBuf::from));
        Self {
            ner_threads: 1,
            ner_device: NerDevice::Cpu,
            ner_batch_size: 1,
            window_timeout_seconds: 16,
            categories: None,
            model_directory: std::env::var_os("CP_DETECTOR_MODEL_DIR")
                .map(PathBuf::from)
                .or_else(|| {
                    prefix
                        .as_ref()
                        .map(|p| p.join("share/censorprivacy/models/bert4ner-base-chinese"))
                        .filter(|p| p.is_dir())
                })
                .or_else(|| {
                    Some(
                        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                            .join("models/bert4ner-base-chinese"),
                    )
                }),
            runtime_library: std::env::var_os("CP_DETECTOR_ORT_LIBRARY")
                .or_else(|| std::env::var_os("ORT_DYLIB_PATH"))
                .map(PathBuf::from)
                .or_else(|| prefix.map(|p| p.join("lib/censorprivacy/libonnxruntime.so"))),
        }
    }
}

/// CLI overrides the environment. Invalid configuration must never silently
/// select a different inference setup.
pub fn configured_ner_threads(explicit: Option<u16>) -> Result<usize> {
    let threads = match explicit {
        Some(value) => usize::from(value),
        None => match std::env::var("CP_NER_THREADS") {
            Ok(value) => value.parse().map_err(|_| Error::DetectionFailed)?,
            Err(std::env::VarError::NotPresent) => 1,
            Err(_) => return Err(Error::DetectionFailed),
        },
    };
    if !(1..=256).contains(&threads) {
        return Err(Error::DetectionFailed);
    }
    Ok(threads)
}

#[cfg(test)]
mod thread_tests {
    use super::*;
    #[test]
    fn explicit_thread_configuration_is_bounded_and_overrides_environment() {
        assert_eq!(configured_ner_threads(Some(4)).unwrap(), 4);
        assert_eq!(configured_ner_threads(Some(1)).unwrap(), 1);
        assert_eq!(configured_ner_threads(Some(256)).unwrap(), 256);
        assert!(configured_ner_threads(Some(0)).is_err());
        assert!(configured_ner_threads(Some(257)).is_err());
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnalyzeOptions {
    pub field: String,
    pub structural: bool,
    pub mask_money: bool,
    pub mask_dates: bool,
    pub mask_all_numbers: bool,
}
impl Default for AnalyzeOptions {
    fn default() -> Self {
        Self {
            field: String::new(),
            structural: false,
            mask_money: true,
            mask_dates: true,
            mask_all_numbers: false,
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Prediction {
    pub entity_type: String,
    pub start: usize,
    pub end: usize,
    pub score: f64,
    pub sources: Vec<String>,
    pub validation: String,
    pub evidence: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}
#[derive(Clone, Serialize)]
pub struct NerEntity {
    pub label: String,
    pub start: usize,
    pub end: usize,
    pub score: f64,
}
struct Native {
    rules: rules::Rules,
    ner: Option<model::NerModel>,
    failed: bool,
    // Only hashes and offsets/metadata, never source text or vault references.
    cache: VecDeque<([u8; 32], Vec<Prediction>)>,
    cached_spans: usize,
}
pub struct ProfessionalDetector {
    native: Mutex<Native>,
    categories: crate::categories::Categories,
}
impl ProfessionalDetector {
    pub fn start(config: ProfessionalConfig) -> Result<Self> {
        if !(1..=3600).contains(&config.window_timeout_seconds)
            || !(1..=256).contains(&config.ner_threads)
            || !(1..=256).contains(&config.ner_batch_size)
        {
            return Err(Error::NerConfigInvalid);
        }
        config.ner_device.validate()?;
        let rules = rules::Rules::new_selected(&config.categories)?;
        let ner = if crate::categories::uses_ner(&config.categories) {
            let mut model = model::NerModel::load(&config)?;
            model.warmup()?;
            Some(model)
        } else {
            None
        };
        Ok(Self {
            categories: config.categories,
            native: Mutex::new(Native {
                rules,
                ner,
                failed: false,
                cache: VecDeque::new(),
                cached_spans: 0,
            }),
        })
    }
    /// Original UTF-8 byte offsets, not Python character positions.
    pub fn analyze(&self, text: &str, options: &AnalyzeOptions) -> Result<Vec<Prediction>> {
        let mut options = options.clone();
        if self.categories.is_some() {
            options.mask_money = crate::categories::allows(&self.categories, "MONEY");
            options.mask_dates = crate::categories::allows(&self.categories, "DATE");
            options.mask_all_numbers = crate::categories::allows(&self.categories, "NUMBER");
        }
        self.analyze_selected(text, &options, &self.categories)
    }
    fn analyze_selected(
        &self,
        text: &str,
        options: &AnalyzeOptions,
        categories: &crate::categories::Categories,
    ) -> Result<Vec<Prediction>> {
        if text.len() > 1024 * 1024 || options.field.len() > 8192 {
            return Err(Error::ResourceLimit);
        }
        let mut native = self.native.lock().map_err(|_| Error::DetectionFailed)?;
        if native.failed {
            return Err(Error::DetectionFailed);
        }
        let mut digest = Sha256::new();
        digest.update((text.len() as u64).to_le_bytes());
        digest.update(text.as_bytes());
        digest.update(options.field.as_bytes());
        // Policy reload can narrow categories while reusing the loaded model.
        digest.update(serde_json::to_vec(categories).map_err(|_| Error::DetectionFailed)?);
        digest.update([
            u8::from(options.structural),
            u8::from(options.mask_money),
            u8::from(options.mask_dates),
            u8::from(options.mask_all_numbers),
        ]);
        let key: [u8; 32] = digest.finalize().into();
        if let Some((_, cached)) = native.cache.iter().find(|(old, _)| *old == key) {
            return Ok(cached.clone());
        }
        let Native { rules, ner, .. } = &mut *native;
        let result = rules.analyze_selected(text, options, ner.as_mut(), categories);
        match &result {
            Ok(spans) if spans.len() <= 4096 => {
                // Agent tool schemas have many small fields. Keep the system
                // prompt cached across those fields and the final check;
                // cached entity metadata retains the same bounded span budget.
                while native.cache.len() >= 512 || native.cached_spans + spans.len() > 4096 {
                    let (_, removed) = native.cache.pop_front().ok_or(Error::DetectionFailed)?;
                    native.cached_spans -= removed.len();
                }
                native.cached_spans += spans.len();
                native.cache.push_back((key, spans.clone()));
            }
            Err(_) => {
                native.failed = true;
                native.cache.clear();
                native.cached_spans = 0;
            }
            _ => {}
        }
        result
    }
    /// Offline differential evaluation uses the exact production NER path.
    pub fn ner_entities(&self, text: &str) -> Result<Vec<NerEntity>> {
        if text.len() > 1024 * 1024 {
            return Err(Error::ResourceLimit);
        }
        let mut native = self.native.lock().map_err(|_| Error::DetectionFailed)?;
        if native.failed {
            return Err(Error::DetectionFailed);
        }
        let result = native
            .ner
            .as_mut()
            .ok_or(Error::DetectionFailed)?
            .predict(text);
        if result.is_err() {
            native.failed = true;
        }
        result
    }
    pub(crate) fn detect(
        &self,
        text: &str,
        field: &str,
        structural: bool,
        policy: &PrivacyPolicy,
    ) -> Result<Vec<Span>> {
        let options = AnalyzeOptions {
            field: field.into(),
            structural,
            mask_money: policy.mask_money(),
            mask_dates: policy.mask_dates(),
            mask_all_numbers: policy.mask_all_numbers(),
        };
        let mut spans = Vec::new();
        let categories = crate::categories::intersect(&self.categories, policy.categories());
        for item in self.analyze_selected(text, &options, &categories)? {
            if !item.score.is_finite() || !(0.0..=1.0).contains(&item.score) {
                return Err(Error::DetectionFailed);
            }
            let mut metadata = BTreeMap::from([
                ("source".into(), item.sources.join(",")),
                ("score".into(), item.score.to_string()),
                ("validation".into(), item.validation),
                ("evidence".into(), item.evidence.join(",")),
            ]);
            if let Some(unit) = item.unit {
                metadata.insert("unit".into(), unit);
            }
            spans.push(Span {
                entity_type: item.entity_type,
                start: item.start,
                end: item.end,
                labels: policy.labels().clone(),
                metadata,
            });
        }
        validate_spans(text, &spans)?;
        Ok(spans)
    }
}
