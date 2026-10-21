use censorprivacy::{
    compute::{compute, Operation},
    detection::{DetectionEngine, Detector, DetectorSpec, Span},
    error::Result,
    policy::PrivacyPolicy,
    references::Reference,
    vault::{RunCapability, RunVault},
};
use serde_json::json;
use std::{collections::BTreeSet, sync::Arc, time::Duration};
fn labels(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|s| (*s).into()).collect()
}

fn vault() -> (RunVault, RunCapability) {
    let vault = RunVault::new(Duration::from_secs(3600)).unwrap();
    let cap = vault.begin_run("trusted-local", 1).unwrap();
    (vault, cap)
}
#[test]
fn private_bonus_workflow_keeps_operands_and_results_out_of_model_context() {
    let (vault, cap) = vault();
    let engine = DetectionEngine::new(PrivacyPolicy::default()).unwrap();
    let original = "姓名：张三 奖金10000元；姓名：李四 奖金20000元";
    let protected = vault
        .protect_text(&cap, 1, original, engine.detect(original, false).unwrap())
        .unwrap();
    for secret in ["张三", "李四", "10000", "20000"] {
        assert!(!protected.contains(secret));
    }
    let refs: Vec<_> = regex::Regex::new(r"<CP_MONEY_[0-9a-f]{32}>")
        .unwrap()
        .find_iter(&protected)
        .map(|m| Reference::parse(m.as_str()).unwrap())
        .collect();
    let result = compute(&vault, &cap, 1, Operation::Aggregate, &refs, &json!({})).unwrap();
    let tool_context = serde_json::to_string(&result).unwrap();
    for value in ["10000", "20000", "30000", "15000"] {
        assert!(!tool_context.contains(value));
    }
    let reply = format!(
        "合计 {} 元，平均 {} 元",
        result.results["sum"], result.results["mean"]
    );
    assert_eq!(
        vault.restore_text(&cap, 1, &reply).unwrap(),
        "合计 30000 元，平均 15000 元"
    );
}

#[test]
fn gateway_keeps_required_custom_detectors_after_policy_reload() {
    use censorprivacy::gate::{GateOptions, PrivacyGate};
    struct Local;
    impl Detector for Local {
        fn detect(&self, text: &str) -> Result<Vec<Span>> {
            Ok(text
                .find("LocalName")
                .map(|start| Span {
                    entity_type: "PERSON".into(),
                    start,
                    end: start + 9,
                    labels: labels(&["confidential"]),
                    metadata: Default::default(),
                })
                .into_iter()
                .collect())
        }
    }
    let mut gate = PrivacyGate::with_detectors(
        PrivacyPolicy::default(),
        GateOptions::default(),
        vec![DetectorSpec {
            detector: Arc::new(Local),
            required: true,
        }],
    )
    .unwrap();
    assert!(!gate
        .protect_text("LocalName", "")
        .unwrap()
        .contains("LocalName"));
    gate.reload(PrivacyPolicy::from_json(r#"{"epoch":2}"#).unwrap())
        .unwrap();
    assert!(!gate
        .protect_text("LocalName", "")
        .unwrap()
        .contains("LocalName"));
}
