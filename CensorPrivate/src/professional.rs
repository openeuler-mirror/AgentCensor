//! Native privacy detection using local ONNX resources.
mod model;
mod rules;

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
}
impl Default for ProfessionalConfig {
    fn default() -> Self {
        let prefix = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent()?.parent().map(PathBuf::from));
        Self {
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
    ner: model::NerModel,
    failed: bool,
    // Only hashes and offsets/metadata, never source text or vault references.
    cache: VecDeque<([u8; 32], Vec<Prediction>)>,
    cached_spans: usize,
}
pub struct ProfessionalDetector {
    native: Mutex<Native>,
}
impl ProfessionalDetector {
    pub fn start(config: ProfessionalConfig) -> Result<Self> {
        let rules = rules::Rules::new()?;
        let mut ner = model::NerModel::load(&config)?;
        ner.predict("张三在北京工作。")?;
        Ok(Self {
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
        let result = rules.analyze(text, options, Some(ner));
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
        let result = native.ner.predict(text);
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
        for item in self.analyze(text, &options)? {
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
