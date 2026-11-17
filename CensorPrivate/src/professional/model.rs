use super::{batch, NerDevice, NerEntity, ProfessionalConfig, MODEL_ID};
use crate::error::{Error, Result};
use ort::{
    execution_providers::{
        cann::{CANNImplementationMode, CANNPrecisionMode},
        CANNExecutionProvider, CPUExecutionProvider, CUDAExecutionProvider, ExecutionProvider,
    },
    session::{run_options::RunOptions, Session},
    value::Tensor,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
};
use tokenizers::{Encoding, Tokenizer, TruncationParams};

// ONNX Runtime termination is cooperative. A timed-out inference is rejected;
// the required detector is then latched failed by ProfessionalDetector.
struct Deadline {
    options: std::sync::Arc<RunOptions>,
    stop: Option<std::sync::mpsc::Sender<()>>,
    timer: Option<std::thread::JoinHandle<()>>,
}

impl Deadline {
    fn new() -> Result<Self> {
        let options = std::sync::Arc::new(RunOptions::new().map_err(|_| Error::DetectionFailed)?);
        let (stop, received) = std::sync::mpsc::channel();
        let shared = options.clone();
        let timer = std::thread::Builder::new()
            .name("ner-deadline".into())
            .spawn(move || {
                if received
                    .recv_timeout(std::time::Duration::from_secs(60))
                    .is_err()
                {
                    let _ = shared.terminate();
                }
            })
            .map_err(|_| Error::DetectionFailed)?;
        Ok(Self {
            options,
            stop: Some(stop),
            timer: Some(timer),
        })
    }

}
impl Drop for Deadline {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(timer) = self.timer.take() {
            let _ = timer.join();
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    model_id: String,
    precision: String,
    max_length: usize,
    stride: usize,
    labels: Vec<String>,
    sha256: BTreeMap<String, String>,
}
impl Manifest {
    fn load(directory: &std::path::Path) -> Result<Self> {
        let manifest: Self = crate::json::from_slice(
            &std::fs::read(directory.join("manifest.json")).map_err(|_| Error::DetectionFailed)?,
        )
        .and_then(|v| serde_json::from_value(v).map_err(|_| Error::DetectionFailed))?;
        let expected: BTreeSet<_> = [
            "O", "B-PER", "I-PER", "B-ORG", "I-ORG", "B-LOC", "I-LOC", "B-TIME", "I-TIME",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        if manifest.model_id != MODEL_ID
            || manifest.precision != "float32"
            || manifest.max_length != 512
            || manifest.stride != 64
            || manifest.labels.len() != 9
            || manifest.labels.iter().cloned().collect::<BTreeSet<_>>() != expected
            || manifest
                .sha256
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                != ["config.json", "model.onnx", "tokenizer.json"]
        {
            return Err(Error::DetectionFailed);
        }
        for (name, expected) in &manifest.sha256 {
            let mut file =
                std::fs::File::open(directory.join(name)).map_err(|_| Error::DetectionFailed)?;
            let mut digest = Sha256::new();
            let mut buffer = [0u8; 65536];
            loop {
                let n = file.read(&mut buffer).map_err(|_| Error::DetectionFailed)?;
                if n == 0 {
                    break;
                }
                digest.update(&buffer[..n]);
            }
            if format!("{:x}", digest.finalize()) != *expected {
                return Err(Error::DetectionFailed);
            }
        }
        let config_json: serde_json::Value = serde_json::from_slice(
            &std::fs::read(directory.join("config.json")).map_err(|_| Error::DetectionFailed)?,
        )
        .map_err(|_| Error::DetectionFailed)?;
        if config_json["model_type"] != "bert" {
            return Err(Error::DetectionFailed);
        }
        for (i, label) in manifest.labels.iter().enumerate() {
            if config_json["id2label"][i.to_string()] != *label {
                return Err(Error::DetectionFailed);
            }
        }
        Ok(manifest)
    }
}
pub(super) struct NerModel {
    tokenizer: Tokenizer,
    sessions: Vec<Session>,
    labels: Vec<String>,
    batch_size: usize,
    pad_id: u32,
}
impl NerModel {
    pub(super) fn load(config: &ProfessionalConfig) -> Result<Self> {
        if !(1..=256).contains(&config.ner_threads) {
            return Err(Error::DetectionFailed);
        }
        config.ner_device.validate()?;
        if !(1..=256).contains(&config.ner_batch_size) {
            return Err(Error::NerConfigInvalid);
        }
        let directory = config
            .model_directory
            .as_ref()
            .ok_or(Error::DetectionFailed)?;
        let manifest = Manifest::load(directory)?;
        let library = config
            .runtime_library
            .as_ref()
            .filter(|p| p.is_file())
            .ok_or(Error::DetectionFailed)?;
        ort::init_from(library.to_string_lossy())
            .with_name("censorprivacy")
            .with_telemetry(false)
            .commit()
            .map_err(|_| Error::DetectionFailed)?;
        let accelerated = !matches!(config.ner_device, NerDevice::Cpu);
        let providers: Vec<_> = match &config.ner_device {
            NerDevice::Cpu => vec![CPUExecutionProvider::default()
                .with_arena_allocator(true)
                .build()
                .error_on_failure()],
            NerDevice::Cuda(ids) => {
                if !CUDAExecutionProvider::default()
                    .is_available()
                    .unwrap_or(false)
                {
                    return Err(Error::NerDeviceUnavailable);
                }
                ids.iter()
                    .map(|id| {
                        CUDAExecutionProvider::default()
                            .with_device_id(*id)
                            .with_tf32(false)
                            .build()
                            .error_on_failure()
                    })
                    .collect()
            }
            NerDevice::Cann(ids) => {
                if !CANNExecutionProvider::default()
                    .is_available()
                    .unwrap_or(false)
                {
                    return Err(Error::NerDeviceUnavailable);
                }
                ids.iter()
                    .map(|id| {
                        CANNExecutionProvider::default()
                            .with_device_id(*id)
                            .with_precision_mode(CANNPrecisionMode::MustKeepOrigin)
                            .with_implementation_mode(CANNImplementationMode::HighPrecision)
                            .build()
                            .error_on_failure()
                    })
                    .collect()
            }
        };
        let mut sessions = Vec::with_capacity(providers.len());
        for provider in providers {
            let builder = Session::builder()
                .and_then(|b| b.with_intra_threads(config.ner_threads))
                .and_then(|b| b.with_inter_threads(1))
                .map_err(|_| Error::DetectionFailed)?;
            let session = builder
                .with_execution_providers([provider])
                .and_then(|b| b.commit_from_file(directory.join("model.onnx")))
                .map_err(|_| {
                    if accelerated {
                        Error::NerDeviceUnavailable
                    } else {
                        Error::DetectionFailed
                    }
                })?;
            if session
                .inputs
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>()
                != ["input_ids", "attention_mask", "token_type_ids"]
                || session.outputs.len() != 1
                || session.outputs[0].name != "logits"
            {
                return Err(Error::DetectionFailed);
            }
            sessions.push(session);
        }
        let mut tokenizer = Tokenizer::from_file(directory.join("tokenizer.json"))
            .map_err(|_| Error::DetectionFailed)?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: 512,
                stride: 64,
                ..Default::default()
            }))
            .map_err(|_| Error::DetectionFailed)?;
        tokenizer.with_padding(None);
        let pad_id = tokenizer
            .token_to_id("[PAD]")
            .ok_or(Error::DetectionFailed)?;
        Ok(Self {
            sessions,
            tokenizer,
            labels: manifest.labels,
            batch_size: config.ner_batch_size,
            pad_id,
        })
    }
    pub(super) fn warmup(&mut self) -> Result<()> {
        let encoded = self
            .tokenizer
            .encode("张三在北京工作。", true)
            .map_err(|_| Error::DetectionFailed)?;
        for session in &mut self.sessions {
            infer_batch(
                session,
                &[&encoded],
                "张三在北京工作。",
                &self.labels,
                self.pad_id,
            )?;
        }
        Ok(())
    }
    pub(super) fn predict(&mut self, text: &str) -> Result<Vec<NerEntity>> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let encoded = self
            .tokenizer
            .encode(text, true)
            .map_err(|_| Error::DetectionFailed)?;
        let windows: Vec<_> = std::iter::once(&encoded)
            .chain(encoded.get_overflowing())
            .collect();
        let labels = &self.labels;
        let pad_id = self.pad_id;
        let batches = batch::dispatch(
            &mut self.sessions,
            &windows,
            self.batch_size,
            |session, windows| infer_batch(session, windows, text, labels, pad_id),
        )?;
        let mut entities: Vec<_> = batches.into_iter().flatten().collect();
        entities.sort_by_key(|e| e.start);
        let mut selected: Vec<NerEntity> = Vec::new();
        for entity in entities {
            if let Some(previous) = selected.last_mut() {
                if previous.start <= entity.start && entity.start < previous.end {
                    let a = text[entity.start..entity.end].chars().count();
                    let b = text[previous.start..previous.end].chars().count();
                    if a > b || a == b && entity.score > previous.score {
                        *previous = entity;
                    }
                    continue;
                }
            }
            selected.push(entity);
        }
        Ok(selected)
    }
}

fn infer_batch(
    session: &mut Session,
    windows: &[&Encoding],
    text: &str,
    labels: &[String],
    pad_id: u32,
) -> Result<Vec<NerEntity>> {
    let width = windows
        .iter()
        .map(|window| window.len())
        .max()
        .ok_or(Error::DetectionFailed)?;
    let count = windows.len();
    let mut ids = vec![i64::from(pad_id); count * width];
    let mut mask = vec![0i64; count * width];
    let mut types = vec![0i64; count * width];
    for (index, window) in windows.iter().enumerate() {
        for (target, source) in [
            (&mut ids, window.get_ids()),
            (&mut mask, window.get_attention_mask()),
            (&mut types, window.get_type_ids()),
        ] {
            for (to, from) in target[index * width..].iter_mut().zip(source) {
                *to = i64::from(*from);
            }
        }
    }
    let tensor = |values: Vec<i64>| {
        Tensor::from_array(([count, width], values.into_boxed_slice()))
            .map_err(|_| Error::DetectionFailed)
    };
    let inputs = ort::inputs!["input_ids"=>tensor(ids)?, "attention_mask"=>tensor(mask)?, "token_type_ids"=>tensor(types)?];
    let deadline = Deadline::new()?;
    let options = deadline.options.clone();
    let output = session
        .run_with_options(inputs, &*options)
        .map_err(|_| Error::DetectionFailed)?;
    let (shape, logits) = output["logits"]
        .try_extract_tensor::<f32>()
        .map_err(|_| Error::DetectionFailed)?;
    if shape.as_ref() != [count as i64, width as i64, 9] || logits.iter().any(|v| !v.is_finite()) {
        return Err(Error::DetectionFailed);
    }
    let mut entities = Vec::new();
    for (window, row) in windows.iter().zip(logits.chunks_exact(width * 9)) {
        // Padding is not a token in the original encoding and must never enter BIO aggregation.
        let window_logits = &row[..window.len() * 9];
        let mut group: Option<(String, usize, usize, f32, usize)> = None;
        let finish = |group: &mut Option<(String, usize, usize, f32, usize)>,
                      entities: &mut Vec<NerEntity>| {
            if let Some((label, start, end, sum, n)) = group.take() {
                if label != "O" && start < end {
                    entities.push(NerEntity {
                        label,
                        start,
                        end,
                        score: (sum / n as f32) as f64,
                    });
                }
            }
        };
        for (i, row) in window_logits.chunks_exact(9).enumerate() {
            if window.get_special_tokens_mask()[i] != 0 {
                continue;
            }
            let (start, end) = window.get_offsets()[i];
            if text.get(start..end).is_none() {
                return Err(Error::DetectionFailed);
            }
            let mut best = 0;
            for j in 1..9 {
                if row[j] > row[best] {
                    best = j;
                }
            }
            let denominator: f32 = row.iter().map(|v| (v - row[best]).exp()).sum();
            let score = 1.0 / denominator;
            let raw = &labels[best];
            let (bi, label) = raw.split_once('-').unwrap_or(("I", raw));
            if let Some((kind, _, upper, sum, n)) = group.as_mut() {
                if kind == label && bi != "B" {
                    *upper = end;
                    *sum += score;
                    *n += 1;
                    continue;
                }
            }
            finish(&mut group, &mut entities);
            group = Some((label.into(), start, end, score, 1));
        }
        finish(&mut group, &mut entities);
    }
    Ok(entities)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Bundle(std::path::PathBuf);
    impl Drop for Bundle {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn missing_foreign_and_corrupt_bundles_are_rejected_without_runtime() {
        let bundle = Bundle(std::env::temp_dir().join(format!(
            "cp-model-validation-{}",
            crate::references::random_hex::<16>().unwrap()
        )));
        std::fs::create_dir(&bundle.0).unwrap();
        assert!(Manifest::load(&bundle.0).is_err());
        let labels = [
            "O", "B-PER", "I-PER", "B-ORG", "I-ORG", "B-LOC", "I-LOC", "B-TIME", "I-TIME",
        ];
        let id2label: BTreeMap<_, _> = labels
            .iter()
            .enumerate()
            .map(|(i, label)| (i.to_string(), *label))
            .collect();
        std::fs::write(
            bundle.0.join("config.json"),
            serde_json::to_vec(&serde_json::json!({"model_type":"bert","id2label":id2label}))
                .unwrap(),
        )
        .unwrap();
        for name in ["model.onnx", "tokenizer.json"] {
            std::fs::write(bundle.0.join(name), b"synthetic asset").unwrap();
        }
        let hashes: BTreeMap<_, _> = ["model.onnx", "tokenizer.json", "config.json"]
            .into_iter()
            .map(|name| {
                let bytes = std::fs::read(bundle.0.join(name)).unwrap();
                (name, format!("{:x}", Sha256::digest(bytes)))
            })
            .collect();
        let valid = serde_json::json!({"model_id":MODEL_ID,"precision":"float32",
            "max_length":512,"stride":64,"labels":labels,"sha256":hashes});
        let write = |value: &serde_json::Value| {
            std::fs::write(
                bundle.0.join("manifest.json"),
                serde_json::to_vec(value).unwrap(),
            )
            .unwrap();
        };
        write(&valid);
        // Validate metadata independently, without loading any runtime/model.
        assert!(Manifest::load(&bundle.0).is_ok());
        for (key, value) in [
            ("model_id", serde_json::json!("other-model")),
            ("precision", serde_json::json!("int8")),
            ("stride", serde_json::json!(0)),
            ("labels", serde_json::json!(["O"])),
        ] {
            let mut invalid = valid.clone();
            invalid[key] = value;
            write(&invalid);
            assert!(Manifest::load(&bundle.0).is_err(), "{key}");
        }
        for name in ["config.json", "model.onnx", "tokenizer.json"] {
            let mut invalid = valid.clone();
            invalid["sha256"][name] = serde_json::json!("00".repeat(32));
            write(&invalid);
            assert!(Manifest::load(&bundle.0).is_err(), "{name}");
        }
        write(&valid);
        std::fs::write(bundle.0.join("model.onnx"), b"modified asset").unwrap();
        assert!(Manifest::load(&bundle.0).is_err());
    }
}
