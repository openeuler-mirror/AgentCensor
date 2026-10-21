use censorprivacy::{
    compute::{compute, Operation},
    error::Error,
    references::Reference,
    vault::{Clock, Registration, RunCapability, RunVault},
};
use serde_json::json;
use std::{
    collections::BTreeSet,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
fn labels(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|s| (*s).into()).collect()
}
fn registration(kind: &str) -> Registration {
    Registration::new(kind, labels(&["confidential"]))
}
fn vault() -> (RunVault, RunCapability) {
    let vault = RunVault::new(Duration::from_secs(3600)).unwrap();
    let cap = vault.begin_run("trusted-local", 1).unwrap();
    (vault, cap)
}
fn register(vault: &RunVault, cap: &RunCapability, value: &str, kind: &str) -> Reference {
    vault
        .register_value(cap, 1, value, registration(kind))
        .unwrap()
}

#[test]
fn secrets_and_secret_label_descendants_never_restore_or_compute() {
    let (vault, cap) = vault();
    let secret = register(&vault, &cap, "sk-production-credential", "SECRET");
    assert_eq!(
        vault
            .restore_text(&cap, 1, &format!("{secret} sk-production-credential"))
            .unwrap(),
        "[REDACTED_SECRET] [REDACTED_SECRET]"
    );
    assert!(matches!(
        compute(
            &vault,
            &cap,
            1,
            Operation::Sum,
            &[secret.clone()],
            &json!({})
        ),
        Err(Error::VaultDenied)
    ));
    let mut derived = registration("NUMBER");
    derived.depends_on.push(secret);
    let reference = vault.register_value(&cap, 1, "42", derived).unwrap();
    assert_eq!(
        vault.restore_text(&cap, 1, reference.as_str()).unwrap(),
        "[REDACTED_SECRET]"
    );
    assert!(matches!(
        compute(&vault, &cap, 1, Operation::Sum, &[reference], &json!({})),
        Err(Error::VaultDenied)
    ));
}

#[test]
fn exact_arithmetic_never_silently_rounds_or_uses_binary_float() {
    for (op, a, b, expected) in [
        (Operation::Add, "0.1", "0.2", "0.3"),
        (
            Operation::Subtract,
            "10000000000000000000000000000000000000000000000000",
            "1",
            "9999999999999999999999999999999999999999999999999",
        ),
        (Operation::Multiply, "0.00001", "0.00002", "0.0000000002"),
        (Operation::Divide, "1", "8", "0.125"),
        (Operation::Divide, "1", "40", "0.025"),
        (Operation::Divide, "-12", "0.5", "-24"),
    ] {
        let (vault, cap) = vault();
        let refs = [
            register(&vault, &cap, a, "NUMBER"),
            register(&vault, &cap, b, "NUMBER"),
        ];
        let result = compute(&vault, &cap, 1, op, &refs, &json!({}))
            .unwrap()
            .result
            .unwrap();
        assert_eq!(
            vault.restore_text(&cap, 1, result.as_str()).unwrap(),
            expected
        );
    }
    let (vault, cap) = vault();
    for (a, b) in [("1", "3"), ("1", "0"), ("1e128", "0.1"), ("1e-128", "10")] {
        let refs = [
            register(&vault, &cap, a, "NUMBER"),
            register(&vault, &cap, b, "NUMBER"),
        ];
        assert!(matches!(
            compute(&vault, &cap, 1, Operation::Divide, &refs, &json!({})),
            Err(Error::ComputeInvalid)
        ));
    }
    for invalid in [
        "NaN",
        "Infinity",
        "1e129",
        "1e-129",
        "12,34",
        "0xFF",
        "123456789012345678901234567890123456789012345678901",
    ] {
        let reference = register(&vault, &cap, invalid, "NUMBER");
        assert!(matches!(
            compute(&vault, &cap, 1, Operation::Sum, &[reference], &json!({})),
            Err(Error::ComputeInvalid)
        ));
    }
}

#[test]
fn dimensions_dates_and_result_expression_identity_are_preserved() {
    let (vault, cap) = vault();
    let a = register(&vault, &cap, "10", "MONEY");
    let b = register(&vault, &cap, "20", "MONEY");
    let first = compute(
        &vault,
        &cap,
        1,
        Operation::Min,
        &[a.clone(), b.clone()],
        &json!({}),
    )
    .unwrap()
    .result
    .unwrap();
    assert_ne!(first, a);
    let repeat = compute(
        &vault,
        &cap,
        1,
        Operation::Min,
        &[a.clone(), b.clone()],
        &json!({}),
    )
    .unwrap()
    .result
    .unwrap();
    assert_eq!(repeat, first);
    let other_expression = compute(
        &vault,
        &cap,
        1,
        Operation::Max,
        &[a.clone(), a.clone()],
        &json!({}),
    )
    .unwrap()
    .result
    .unwrap();
    assert_ne!(other_expression, first);
    assert!(compute(
        &vault,
        &cap,
        1,
        Operation::Multiply,
        &[a.clone(), b.clone()],
        &json!({})
    )
    .is_err());
    let mut usd = registration("MONEY");
    usd.metadata.unit = Some("USD".into());
    let c = vault.register_value(&cap, 1, "10", usd).unwrap();
    assert_ne!(a, c);
    assert!(compute(&vault, &cap, 1, Operation::Sum, &[a, c], &json!({})).is_err());
    let refs = [
        register(&vault, &cap, "2024年2月28日", "DATE"),
        register(&vault, &cap, "2024/3/1", "DATE"),
    ];
    let result = compute(
        &vault,
        &cap,
        1,
        Operation::DateDiff,
        &refs,
        &json!({"unit":"days"}),
    )
    .unwrap();
    assert_eq!(result.entity_type, "DURATION");
    assert_eq!(result.unit.as_deref(), Some("days"));
    assert_eq!(
        vault
            .restore_text(&cap, 1, result.result.unwrap().as_str())
            .unwrap(),
        "2"
    );
}

#[derive(Default)]
struct ManualClock(AtomicU64);
impl Clock for ManualClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::SeqCst))
    }
}
impl ManualClock {
    fn set(&self, millis: u64) {
        self.0.store(millis, Ordering::SeqCst);
    }
}

#[test]
fn expiry_and_epoch_remove_authority_without_declassifying_known_values() {
    let clock = Arc::new(ManualClock::default());
    let vault = RunVault::with_clock(Duration::from_secs(60), clock.clone()).unwrap();
    let cap = vault.begin_run("owner", 1).unwrap();
    let mut parent = registration("NUMBER");
    parent.ttl = Some(Duration::from_secs(1));
    let reference = vault.register_value(&cap, 1, "531", parent).unwrap();
    let result = compute(
        &vault,
        &cap,
        1,
        Operation::Sum,
        &[reference.clone()],
        &json!({}),
    )
    .unwrap()
    .result
    .unwrap();
    clock.set(1000);
    assert!(vault.restore_text(&cap, 1, reference.as_str()).is_err());
    assert!(vault.restore_text(&cap, 1, result.as_str()).is_err());
    assert_eq!(vault.known_spans(&cap, 1, "value 531.").unwrap().len(), 1);
    assert!(vault
        .known_spans(&cap, 1, "python531 531.5 _531")
        .unwrap()
        .is_empty());
    vault
        .advance_epoch(&cap, 1, 2, Duration::from_secs(60))
        .unwrap();
    assert!(vault.restore_text(&cap, 2, reference.as_str()).is_err());
    assert_eq!(vault.known_spans(&cap, 2, "531").unwrap().len(), 1);
    assert!(vault
        .protect_text(&cap, 2, reference.as_str(), vec![])
        .is_err());
    let fresh = vault.protect_text(&cap, 2, "531", vec![]).unwrap();
    assert_ne!(fresh, reference.as_str());
    assert_eq!(vault.restore_text(&cap, 2, &fresh).unwrap(), "531");
    clock.set(60_000);
    assert!(vault.validate_run(&cap, 2).is_err());
    assert_eq!(vault.cleanup_expired().unwrap(), 1);
}
