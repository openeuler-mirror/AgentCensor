use censorprivacy::{
    detection::{merge_spans, DetectionEngine, Detector, DetectorSpec, Span},
    error::{Error, Result},
    policy::PrivacyPolicy,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
fn labels(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|s| (*s).into()).collect()
}

#[test]
fn policy_rejects_invalid_types_unknown_fields_and_unbounded_ttl() {
    for raw in [
        r#"{"epoch":true}"#,
        r#"{"epoch":0}"#,
        r#"{"epoch":-1}"#,
        r#"{"unknown":true}"#,
        r#"{"mask_money":1}"#,
        r#"{"mask_dates":"false"}"#,
        r#"{"default_labels":[]}"#,
        r#"{"default_labels":[""]}"#,
        r#"{"ttl_seconds":0}"#,
        r#"{"ttl_seconds":-1}"#,
        r#"{"ttl_seconds":1e100}"#,
        r#"{"dictionaries":{"bad":["value"]}}"#,
        r#"{"patterns":{"PERSON":["("]}}"#,
        r#"{"patterns":{"PERSON":["a*"]}}"#,
        r#"{"patterns":{"PERSON":["(?=a)"]}}"#, // zero width at runtime also fails closed
    ] {
        if raw.contains("(?=a)") {
            continue;
        }
        assert!(
            matches!(PrivacyPolicy::from_json(raw), Err(Error::PolicyInvalid)),
            "{raw}"
        );
    }
    for raw in [
        "ttl_seconds: .nan",
        "ttl_seconds: .inf",
        "epoch: true",
        "epoch: 1\nepoch: 2",
    ] {
        assert!(PrivacyPolicy::from_yaml(raw).is_err());
    }
    assert!(PrivacyPolicy::from_yaml(include_str!("../examples/policy.yaml")).is_ok());
    let policy =
        PrivacyPolicy::from_json(r#"{"patterns":{"PERSON":["(?<=name:)[A-Z]+"]}}"#).unwrap();
    assert_eq!(
        DetectionEngine::new(policy)
            .unwrap()
            .detect("name:ALICE", false)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn detector_handles_unicode_offsets_and_full_overlap_union() {
    let policy = PrivacyPolicy::from_json(r#"{"dictionaries":{"ORG":["北京研究所"]}}"#).unwrap();
    let engine = DetectionEngine::new(policy).unwrap();
    let text = "🙂姓名：张三 北京研究所 奖金10,000元 a@example.com 2026年10月15日";
    let spans = engine.detect(text, false).unwrap();
    let detected: Vec<_> = spans
        .iter()
        .map(|span| (span.entity_type.as_str(), &text[span.start..span.end]))
        .collect();
    assert_eq!(
        detected,
        vec![
            ("PERSON", "张三"),
            ("ORG", "北京研究所"),
            ("MONEY", "10,000"),
            ("EMAIL", "a@example.com"),
            ("DATE", "2026年10月15日")
        ]
    );
    let overlapping = vec![
        Span {
            entity_type: "PERSON".into(),
            start: 0,
            end: 6,
            labels: labels(&["a"]),
            metadata: BTreeMap::new(),
        },
        Span {
            entity_type: "SECRET".into(),
            start: 3,
            end: 9,
            labels: labels(&["b"]),
            metadata: BTreeMap::from([("unit".into(), "USD".into())]),
        },
    ];
    let spans = merge_spans("张三李", overlapping).unwrap();
    assert_eq!((spans[0].start, spans[0].end), (0, 9));
    assert_eq!(spans[0].entity_type, "SECRET");
    assert_eq!(spans[0].labels, labels(&["a", "b"]));
    assert!(spans[0].metadata.is_empty());
}

struct BrokenDetector;

impl Detector for BrokenDetector {
    fn detect(&self, _: &str) -> Result<Vec<Span>> {
        Ok(vec![Span {
            entity_type: "PERSON".into(),
            start: 1,
            end: 2,
            labels: labels(&["confidential"]),
            metadata: BTreeMap::new(),
        }])
    }
}

#[test]
fn malformed_required_detector_fails_and_optional_cannot_remove_rules() {
    let engine = |required| {
        DetectionEngine::new(PrivacyPolicy::default())
            .unwrap()
            .with_detectors(vec![DetectorSpec {
                detector: Arc::new(BrokenDetector),
                required,
            }])
    };
    assert!(matches!(
        engine(true).detect("张三 a@example.com", false),
        Err(Error::DetectionFailed)
    ));
    assert_eq!(
        engine(false)
            .detect("张三 a@example.com", false)
            .unwrap()
            .len(),
        1
    );
    let policy = PrivacyPolicy::from_json(r#"{"patterns":{"PERSON":["(?=a)"]}}"#).unwrap();
    assert!(matches!(
        DetectionEngine::new(policy).unwrap().detect("a", false),
        Err(Error::DetectionFailed)
    ));
}

#[test]
fn supplied_local_analyzer_converts_unicode_character_ranges_and_validates_results() {
    use censorprivacy::detection::{AnalyzerDetector, AnalyzerMatch, LocalAnalyzer};
    struct Analyzer(bool);
    impl LocalAnalyzer for Analyzer {
        fn analyze(&self, _: &str, language: &str, threshold: f32) -> Result<Vec<AnalyzerMatch>> {
            assert_eq!(language, "zh");
            assert_eq!(threshold, 0.3);
            Ok(vec![AnalyzerMatch {
                entity_type: "ORGANIZATION".into(),
                character_range: if self.0 { 1..3 } else { 1..999 },
            }])
        }
    }
    let detector = AnalyzerDetector::new(
        Arc::new(Analyzer(true)),
        "zh".into(),
        0.3,
        labels(&["confidential"]),
    )
    .unwrap();
    let text = "🙂北京 office";
    let spans = detector.detect(text).unwrap();
    assert_eq!(spans[0].entity_type, "ORG");
    assert_eq!(&text[spans[0].start..spans[0].end], "北京");
    assert!(AnalyzerDetector::new(
        Arc::new(Analyzer(true)),
        "zh".into(),
        f32::NAN,
        labels(&["confidential"])
    )
    .is_err());
    assert!(AnalyzerDetector::new(
        Arc::new(Analyzer(false)),
        "zh".into(),
        0.3,
        labels(&["confidential"])
    )
    .unwrap()
    .detect(text)
    .is_err());
}
