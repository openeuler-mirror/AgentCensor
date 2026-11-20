use censorprivacy::{
    error::Error,
    references::Reference,
    vault::{Registration, RunCapability, RunVault, ValueMetadata},
};
use std::{collections::BTreeSet, sync::Arc, time::Duration};
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
fn references_are_opaque_and_bound_to_run_and_vault() {
    let (vault, cap) = vault();
    let reference = register(&vault, &cap, "张三", "PERSON");
    assert_eq!(reference.as_str().len(), "<CP_PERSON_>".len() + 8);
    assert_eq!(
        vault.restore_text(&cap, 1, reference.as_str()).unwrap(),
        "张三"
    );
    let other = vault.begin_run("different-owner", 1).unwrap();
    assert!(matches!(
        vault.restore_text(&other, 1, reference.as_str()),
        Err(Error::VaultDenied)
    ));
    let unrelated = RunVault::new(Duration::from_secs(3600)).unwrap();
    assert!(matches!(
        unrelated.restore_text(&cap, 1, reference.as_str()),
        Err(Error::VaultDenied)
    ));
    let fake = "<CP_PERSON_00000000>";
    let protected = vault.protect_text(&cap, 1, fake, vec![]).unwrap();
    assert_ne!(protected, fake);
    assert_eq!(vault.restore_text(&cap, 1, &protected).unwrap(), fake); // no recursive expansion
    assert!(matches!(
        vault.restore_text(&cap, 1, fake),
        Err(Error::VaultDenied)
    ));
}

#[test]
fn short_references_round_trip_many_distinct_values() {
    let (vault, cap) = vault();
    let mut issued = BTreeSet::new();
    for index in 0..256 {
        let value = format!("客户{index}");
        let reference = register(&vault, &cap, &value, "CUSTOMER");
        let suffix = reference
            .as_str()
            .strip_prefix("<CP_CUSTOMER_")
            .unwrap()
            .strip_suffix('>')
            .unwrap();
        assert_eq!(suffix.len(), 8);
        assert!(suffix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
        assert!(issued.insert(reference.clone()));
        assert_eq!(Reference::parse(reference.as_str()).unwrap(), reference);
        assert_eq!(register(&vault, &cap, &value, "CUSTOMER"), reference);
        assert_eq!(
            vault.restore_text(&cap, 1, reference.as_str()).unwrap(),
            value
        );
    }
}

#[test]
fn concurrent_registration_deduplicates_without_losing_secret_labels() {
    let (vault, cap) = vault();
    let vault = Arc::new(vault);
    let refs = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..16)
            .map(|i| {
                let vault = vault.clone();
                let cap = cap.clone();
                scope.spawn(move || {
                    let mut registration = registration("NUMBER");
                    if i == 0 {
                        registration.labels.insert("secret".into());
                    }
                    vault.register_value(&cap, 1, "7", registration).unwrap()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(refs.iter().all(|reference| reference == &refs[0]));
    assert_eq!(
        vault.restore_text(&cap, 1, refs[0].as_str()).unwrap(),
        "[REDACTED_SECRET]"
    );
    vault.end_run(&cap).unwrap();
    assert!(vault.restore_text(&cap, 1, refs[0].as_str()).is_err());
}

#[test]
fn metadata_entity_identity_distinguishes_equal_values() {
    let (vault, cap) = vault();
    let mut spec = registration("PERSON");
    spec.metadata = ValueMetadata {
        entity_id: Some("contact:alice".into()),
        ..Default::default()
    };
    let a = vault.register_value(&cap, 1, "Alex", spec).unwrap();
    let mut spec = registration("PERSON");
    spec.metadata.entity_id = Some("contact:bob".into());
    let b = vault.register_value(&cap, 1, "Alex", spec).unwrap();
    assert_ne!(a, b);
}

#[test]
fn overlapping_known_literals_do_not_leak_the_unmatched_tail() {
    let (vault, cap) = vault();
    register(&vault, &cap, "abcab", "SECRET");
    assert_eq!(
        vault.redact_known_secrets(&cap, 1, "abcabcab").unwrap(),
        "[REDACTED_SECRET]"
    );
    let protected = vault.protect_text(&cap, 1, "abcabcab", vec![]).unwrap();
    assert!(!protected.ends_with("cab"));
    assert_eq!(
        vault.restore_text(&cap, 1, &protected).unwrap(),
        "[REDACTED_SECRET]"
    );
}
