use censorprivacy::{
    categories,
    config::{AppConfig, ConfigArgs},
    detection::DetectionEngine,
    gate::{GateOptions, PrivacyGate},
    policy::PrivacyPolicy,
    professional::{AnalyzeOptions, NerDevice, ProfessionalDetector},
};
use std::{fs, os::unix::fs::symlink, path::PathBuf, time::Duration};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let mut random = [0u8; 16];
        getrandom::getrandom(&mut random).unwrap();
        let path =
            std::env::temp_dir().join(format!("cp-config-{:032x}", u128::from_be_bytes(random)));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn all_categories_and_explicit_list_override_legacy_switches() {
    let legacy =
        PrivacyPolicy::from_yaml("ttl_seconds: 86400\nmask_money: false\nmask_dates: false\n")
            .unwrap();
    let config =
        AppConfig::from_yaml("server: {ttl_seconds: 86400}\ndetection: {categories: all}").unwrap();
    let all = config.privacy_policy().unwrap();
    let legacy_applied = config.apply_policy(legacy).unwrap();
    assert!(legacy_applied.mask_all_numbers());
    assert_eq!(all.ttl(), Duration::from_secs(86400));
    assert!(all.mask_all_numbers() && all.mask_money() && all.mask_dates());
    for kind in categories::BUILTIN_CATEGORIES {
        assert!(all.allows_category(kind));
    }
    let registered = AppConfig::from_yaml(
        "detection: {categories: all}\ncustom: {dictionaries: {CUSTOMER: [ExampleCustomer]}}",
    )
    .unwrap();
    assert!(registered
        .privacy_policy()
        .unwrap()
        .allows_category("CUSTOMER"));
    let resolved = serde_json::to_value(&registered).unwrap();
    assert_eq!(resolved["detection"]["categories"], "all");
    let selected = AppConfig::from_yaml(
        "server: {ttl_seconds: 7200}\ndetection: {categories: [EMAIL_ADDRESS, PHONE]}\n",
    )
    .unwrap();
    let policy = selected.privacy_policy().unwrap();
    assert_eq!(policy.ttl(), Duration::from_secs(7200));
    assert!(!policy.mask_money() && !policy.mask_dates() && !policy.mask_all_numbers());
    let text = "姓名：张三 a@example.com 13800138000 secret=SensitivePassword 42";
    let spans = DetectionEngine::new(policy)
        .unwrap()
        .detect(text, false)
        .unwrap();
    assert_eq!(
        spans
            .iter()
            .map(|s| s.entity_type.as_str())
            .collect::<Vec<_>>(),
        ["EMAIL", "PHONE"]
    );
}

#[test]
fn omitted_null_and_empty_categories_disable_built_in_and_custom_detection() {
    for detection in [
        "",
        "detection: {}",
        "detection: {categories: null}",
        "detection: {categories: []}",
    ] {
        let config = AppConfig::from_yaml(&format!(
            "custom: {{dictionaries: {{CUSTOMER: [ExampleCustomer]}}}}\n{detection}"
        ))
        .unwrap();
        let policy = config.privacy_policy().unwrap();
        assert!(categories::BUILTIN_CATEGORIES
            .iter()
            .all(|kind| !policy.allows_category(kind)));
        assert!(!policy.allows_category("CUSTOMER"));
        let engine = DetectionEngine::new(policy).unwrap();
        assert!(engine
            .detect("ExampleCustomer a@example.com 13800138000 42", false)
            .unwrap()
            .is_empty());
        let native = config.detector(None, None, None, None, None).unwrap();
        assert!(native
            .categories
            .as_ref()
            .is_some_and(|kinds| kinds.is_empty()));
    }
}

#[test]
fn rejects_unknown_fields_duplicates_invalid_limits_and_category_typos() {
    for yaml in [
        "unknown: true",
        "policy: {}",
        "custom: {mask_money: true}",
        "custom: {mask_dates: true}",
        "custom: {mask_all_numbers: true}",
        "custom: {ttl_seconds: 60}",
        "server: {ttl_seconds: 0}",
        "server: {ttl_seconds: .inf}",
        "server: {ttl_seconds: 1, ttl_seconds: 2}",
        "ner: {thread: 4}",
        "server: {port: 0}",
        "ner: {threads: 0}",
        "ner: {batch_size: 257}",
        "ner: {device: 'cuda:0,0'}",
        "ner: {window_timeout_seconds: 0}",
        "ner: {window_timeout_seconds: 3601}",
        "limits: {max_body_bytes: 0}",
        "limits: {max_decoded_bytes: 268435457}",
        "limits: {upstream_timeout_seconds: 86401}",
        "server: {ttl_seconds: .nan}",
        "server: {ttl_seconds: -1}",
        "server: {ttl_seconds: 315360001}",
        "custom: {file: policy.yaml}",
        "custom: {epoch: 0}",
        "custom: {default_labels: []}",
        "custom: {patterns: {CUSTOMER: ['[']}}",
        "custom: {epoch: 1, epoch: 2}",
        "detection: {categories: [EMALI]}",
        "detection: {categories: [MEDICAL_INFO]}",
        "server: {models: []}",
        "server: {token_file: ''}",
        "server: {port: 8765, port: 8775}",
        "ner: {}\nner: {}",
        "detection: {categories: EMAIL}",
        "detection: {categories: ALL}",
        "detection: {categories: [all]}",
        "detection: {categories: [email]}",
    ] {
        assert!(AppConfig::from_yaml(yaml).is_err(), "{yaml}");
    }
    assert!(ConfigArgs {
        config: Some(PathBuf::from("/missing/cp-config.yaml")),
        no_config: false
    }
    .load()
    .is_err());
    assert!(ConfigArgs {
        config: None,
        no_config: true
    }
    .load()
    .unwrap()
    .is_none());
}

#[test]
fn projected_configmap_paths_resolve_from_the_mount_directory() {
    let temp = Temp::new();
    fs::create_dir(temp.0.join("..generation")).unwrap();
    fs::write(temp.0.join("..generation/config.yaml"), "server: {ttl_seconds: 123, token_file: /run/private/token}\nner: {model_dir: models, ort_library: lib/libonnxruntime.so}\n").unwrap();
    symlink("..generation", temp.0.join("..data")).unwrap();
    symlink("..data/config.yaml", temp.0.join("config.yaml")).unwrap();
    let config = AppConfig::load(&temp.0.join("config.yaml")).unwrap();
    assert_eq!(config.ner.model_dir, Some(temp.0.join("models")));
    assert_eq!(
        config.server.token_file,
        Some(PathBuf::from("/run/private/token"))
    );
    assert_eq!(
        config.privacy_policy().unwrap().ttl(),
        Duration::from_secs(123)
    );
    fs::write(temp.0.join("large.yaml"), " ".repeat(1_048_577)).unwrap();
    assert!(AppConfig::load(&temp.0.join("large.yaml")).is_err());
    assert!(AppConfig::load(&temp.0).is_err());
}

#[test]
fn cli_detector_parameters_override_yaml_and_custom_categories_use_custom_rules() {
    let config = AppConfig::from_yaml("custom: {dictionaries: {CUSTOMER: [ExampleCustomer]}}\nner: {threads: 4, device: 'cuda:1', batch_size: 8, window_timeout_seconds: 20, model_dir: yaml-model}\ndetection: {categories: [CUSTOMER]}\n").unwrap();
    let resolved = config
        .detector(
            Some(2),
            Some(NerDevice::Cpu),
            Some(3),
            Some(PathBuf::from("cli-model")),
            None,
        )
        .unwrap();
    assert_eq!(
        (
            resolved.ner_threads,
            resolved.ner_batch_size,
            resolved.window_timeout_seconds
        ),
        (2, 3, 20)
    );
    assert_eq!(resolved.ner_device, NerDevice::Cpu);
    assert_eq!(resolved.model_directory, Some(PathBuf::from("cli-model")));
    let policy = config.privacy_policy().unwrap();
    let engine = DetectionEngine::new(policy).unwrap();
    let spans = engine
        .detect("ExampleCustomer a@example.com", false)
        .unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].entity_type, "CUSTOMER");
}

#[test]
fn selection_applies_before_native_overlap_and_needs_no_ner_for_rules() {
    for (selected, text, expected) in [
        ("[EMAIL]", "secret=a@example.com phone=13800138000", "EMAIL"),
        ("[NUMBER]", "13800138000", "NUMBER"),
        ("[PHONE_NUMBER]", "13800138000", "PHONE"),
    ] {
        let config = AppConfig::from_yaml(&format!("detection: {{categories: {selected}}}\nner: {{model_dir: /missing/model, ort_library: /missing/ort}}\n")).unwrap();
        let detector =
            ProfessionalDetector::start(config.detector(None, None, None, None, None).unwrap())
                .unwrap();
        let predictions = detector.analyze(text, &AnalyzeOptions::default()).unwrap();
        assert!(!predictions.is_empty(), "{selected}");
        assert!(
            predictions.iter().all(|p| p.entity_type == expected),
            "{selected}"
        );
        if expected == "EMAIL" {
            assert_eq!(
                &text[predictions[0].start..predictions[0].end],
                "a@example.com"
            );
        }
    }
    let config = AppConfig::from_yaml("detection: {categories: null}").unwrap();
    let policy = config.apply_policy(PrivacyPolicy::default()).unwrap();
    let gate = PrivacyGate::with_professional_detector(
        policy,
        GateOptions::default(),
        config.detector(None, None, None, None, None).unwrap(),
    )
    .unwrap();
    let text = "a@example.com 13800138000 secret=SensitivePassword 42";
    assert_eq!(gate.protect_text(text, "").unwrap(), text);
}

#[test]
fn all_non_ner_categories_run_without_model_or_runtime_resources() {
    let selected = categories::BUILTIN_CATEGORIES
        .iter()
        .copied()
        .filter(|kind| !["PERSON", "ORG", "LOCATION", "DATE"].contains(kind))
        .collect::<Vec<_>>()
        .join(", ");
    let config = AppConfig::from_yaml(&format!(
        "detection: {{categories: [{selected}]}}\nner: {{model_dir: /missing/model, ort_library: /missing/ort}}"
    )).unwrap();
    let detector =
        ProfessionalDetector::start(config.detector(None, None, None, None, None).unwrap())
            .unwrap();
    let text = "姓名：张三；公司：示例公司；今天是2026年11月23日；test_result: passed；诊断：感冒；邮箱a@example.com；IP 127.0.0.1。";
    let predictions = detector
        .analyze(&text.repeat(200), &AnalyzeOptions::default())
        .unwrap();
    assert!(predictions.iter().any(|p| p.entity_type == "EMAIL"));
    assert!(predictions.iter().any(|p| p.entity_type == "IP_ADDRESS"));
    assert!(predictions
        .iter()
        .all(|p| !p.sources.iter().any(|s| s == "ner")));

    // TIME is a supported alias for DATE, which also uses the BERT TIME label.
    for kind in ["PERSON", "ORG", "LOCATION", "DATE", "TIME"] {
        let config = AppConfig::from_yaml(&format!(
            "detection: {{categories: [{kind}]}}\nner: {{model_dir: /missing/model, ort_library: /missing/ort}}"
        )).unwrap();
        assert!(
            ProfessionalDetector::start(config.detector(None, None, None, None, None).unwrap())
                .is_err(),
            "{kind} must require model resources"
        );
    }
}

#[test]
fn postal_addresses_and_ip_addresses_have_independent_category_switches() {
    let text = "address: \"http://127.0.0.1:3080\"; 收货地址：北京市海淀区中关村大街27号；test_result: passed";
    for (selected, postal_masked, ip_masked) in [
        ("ADDRESS", true, false),
        ("IP_ADDRESS", false, true),
        ("ADDRESS, IP_ADDRESS", true, true),
        ("", false, false),
    ] {
        let config =
            AppConfig::from_yaml(&format!("detection: {{categories: [{selected}]}}")).unwrap();
        let gate = PrivacyGate::with_professional_detector(
            config.privacy_policy().unwrap(),
            GateOptions::default(),
            config.detector(None, None, None, None, None).unwrap(),
        )
        .unwrap();
        let protected = gate.protect_text(text, "").unwrap();
        assert_eq!(
            !protected.contains("北京市海淀区中关村大街27号"),
            postal_masked,
            "{selected}: {protected}"
        );
        assert_eq!(
            !protected.contains("127.0.0.1"),
            ip_masked,
            "{selected}: {protected}"
        );
        assert!(protected.contains(":3080") && protected.contains("test_result: passed"));
    }
}

#[test]
fn address_exclusions_accept_long_plain_text_and_keep_the_detector_usable() {
    let config = AppConfig::from_yaml("detection: {categories: [ADDRESS]}").unwrap();
    let detector =
        ProfessionalDetector::start(config.detector(None, None, None, None, None).unwrap())
            .unwrap();
    assert!(detector
        .analyze(&"a".repeat(10_000), &AnalyzeOptions::default())
        .unwrap()
        .is_empty());
    let postal = "北京市海淀区中关村大街27号";
    for separator in ["；", ";", "，", ",", "。", " "] {
        let text = format!("GUI: http://127.0.0.1:3080{separator}收货地址：{postal}");
        let predictions = detector.analyze(&text, &AnalyzeOptions::default()).unwrap();
        assert!(
            predictions
                .iter()
                .any(|p| p.entity_type == "ADDRESS" && &text[p.start..p.end] == postal),
            "{text}"
        );
    }
}

#[test]
fn generic_address_fields_require_postal_structure_and_never_mask_network_endpoints() {
    let config = AppConfig::from_yaml("detection: {categories: [ADDRESS]}").unwrap();
    let detector =
        ProfessionalDetector::start(config.detector(None, None, None, None, None).unwrap())
            .unwrap();
    for value in [
        "127.0.0.1",
        "127.0.0.1:3080",
        "[::1]:3080",
        "2001:db8::1",
        "http://127.0.0.1:3080",
        "https://street.example.com",
        "street.example.com:443",
        "北京市.example.test",
        "北京市海淀区中关村大街27号.md",
        "http://localhost:3080/北京市海淀区中关村大街27号",
        "http://localhost:3080/?address=北京市海淀区中关村大街27号",
        "/root/北京市海淀区中关村大街27号",
        "../北京市海淀区中关村大街27号",
        "src/北京市海淀区中关村大街27号",
        "src\\北京市海淀区中关村大街27号",
        "\\\\server\\share\\北京市海淀区中关村大街27号",
        "C:\\北京市海淀区中关村大街27号",
        "alice@street.example.com",
        "localhost:3080",
        "0x7fff1234",
        "网络路由器",
        "当前进程地址",
    ] {
        for field in ["address", "mailing_address", "通讯地址"] {
            let options = AnalyzeOptions {
                field: field.into(),
                ..Default::default()
            };
            assert!(
                detector.analyze(value, &options).unwrap().is_empty(),
                "{field}: {value}"
            );
            let labelled = format!("{field}: \"{value}\"");
            assert!(
                detector
                    .analyze(&labelled, &AnalyzeOptions::default())
                    .unwrap()
                    .is_empty(),
                "{labelled}"
            );
        }
    }
    for value in [
        "北京市海淀区中关村大街27号2号楼301室",
        "春和小区2栋301室",
        "123 Example Street, London",
        "12/34 Example Street, London",
        "PO Box 123, London",
    ] {
        let predictions = detector
            .analyze(
                value,
                &AnalyzeOptions {
                    field: "address".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(
            predictions
                .iter()
                .any(|p| p.entity_type == "ADDRESS" && &value[p.start..p.end] == value),
            "{value}"
        );
    }
    let postal = "北京市海淀区中关村大街27号";
    let mixed = format!(
        "收货地址：{postal}；address: \"http://localhost/{postal}\"; path: \"/tmp/{postal}\"; src/{postal}; src\\{postal}; \\\\server\\share\\{postal}"
    );
    let predictions = detector
        .analyze(&mixed, &AnalyzeOptions::default())
        .unwrap();
    let addresses: Vec<_> = predictions
        .iter()
        .filter(|p| p.entity_type == "ADDRESS")
        .collect();
    assert_eq!(addresses.len(), 1, "{mixed}");
    assert_eq!(addresses[0].start, mixed.find(postal).unwrap());
}

#[test]
fn money_selection_keeps_unsupported_amounts_protected_as_opaque_values() {
    let config = AppConfig::from_yaml("detection: {categories: [MONEY]}").unwrap();
    let gate = PrivacyGate::with_professional_detector(
        config.apply_policy(PrivacyPolicy::default()).unwrap(),
        GateOptions::default(),
        config.detector(None, None, None, None, None).unwrap(),
    )
    .unwrap();
    let protected = gate.protect_text("金额100万元 a@example.com", "").unwrap();
    assert!(!protected.contains("100万"));
    assert!(protected.contains("a@example.com"));
}

#[test]
fn selection_filters_external_overlaps_but_preserves_explicit_sensitive_paths() {
    use censorprivacy::{
        detection::{Detector, DetectorSpec, Span},
        error::Result,
    };
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::Arc,
    };
    struct External;
    impl Detector for External {
        fn detect(&self, text: &str) -> Result<Vec<Span>> {
            Ok(vec![Span {
                entity_type: "SECRET".into(),
                start: 0,
                end: text.len(),
                labels: BTreeSet::from(["confidential".into()]),
                metadata: BTreeMap::new(),
            }])
        }
    }
    let config = AppConfig::from_yaml("detection: {categories: [EMAIL]}").unwrap();
    let engine = DetectionEngine::new(config.apply_policy(PrivacyPolicy::default()).unwrap())
        .unwrap()
        .with_detectors(vec![DetectorSpec {
            detector: Arc::new(External),
            required: true,
        }]);
    let text = "public a@example.com";
    let spans = engine.detect(text, false).unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(&text[spans[0].start..spans[0].end], "a@example.com");
    let config = AppConfig::from_yaml("detection: {categories: []}").unwrap();
    let gate = PrivacyGate::with_professional_detector(
        config.apply_policy(PrivacyPolicy::default()).unwrap(),
        GateOptions {
            sensitive_paths: vec!["/private".into()],
            ..Default::default()
        },
        config.detector(None, None, None, None, None).unwrap(),
    )
    .unwrap();
    assert_ne!(
        gate.protect_text("confidential text", "/private/value")
            .unwrap(),
        "confidential text"
    );
    assert_eq!(
        gate.protect_text("public text", "/public").unwrap(),
        "public text"
    );
}

#[test]
fn all_number_detection_accepts_only_known_reference_schema_metadata() {
    use censorprivacy::{protocols::RequestProtector, references::TLS_TOOL_REFERENCE_PATTERN};
    let config = AppConfig::from_yaml("detection: {categories: [NUMBER]}").unwrap();
    let gate = PrivacyGate::with_professional_detector(
        config.apply_policy(PrivacyPolicy::default()).unwrap(),
        GateOptions::default(),
        config.detector(None, None, None, None, None).unwrap(),
    )
    .unwrap();
    let path = "/tools/0/function/parameters/properties/refs/items/pattern";
    assert!(gate.structural(TLS_TOOL_REFERENCE_PATTERN, path).is_ok());
    assert!(gate
        .structural(&format!("{TLS_TOOL_REFERENCE_PATTERN}extra"), path)
        .is_err());
    assert!(gate
        .structural(TLS_TOOL_REFERENCE_PATTERN, "/metadata/pattern")
        .is_err());
    assert!(gate.structural("[0-9]{18}", path).is_err());
}
