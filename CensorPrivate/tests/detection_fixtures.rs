use censorprivacy::{detection::DetectionEngine, policy::PrivacyPolicy};
use serde_json::{json, Value};

#[test]
fn detection_matches_eighty_frozen_reference_cases() {
    let cases = include_str!("fixtures/detect.jsonl");
    assert_eq!(cases.lines().count(), 80);
    for (index, line) in cases.lines().enumerate() {
        let case: Value = serde_json::from_str(line).unwrap();
        let input = &case["input"];
        let policy = PrivacyPolicy::from_json(&input["policy"].to_string()).unwrap();
        let engine = DetectionEngine::new(policy).unwrap();
        let text = input["text"].as_str().unwrap();
        let actual: Vec<_> = engine
            .detect(text, false)
            .unwrap()
            .iter()
            .map(|span| {
                json!({
                    "entity_type": span.entity_type,
                    "start": span.start,
                    "end": span.end,
                    "text": &text[span.start..span.end],
                    "labels": span.labels,
                    "metadata": span.metadata,
                })
            })
            .collect();
        assert_eq!(json!(actual), case["expected"], "fixture {index}");
    }
}
