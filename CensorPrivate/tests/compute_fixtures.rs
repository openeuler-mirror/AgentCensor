use censorprivacy::{
    compute::{compute, Operation},
    error::Result,
    vault::{Registration, RunVault, ValueMetadata},
};
use serde_json::{json, Value};
use std::{collections::BTreeSet, time::Duration};

// Compare exact decimal spellings without binary floating point or rounding.
fn canonical(value: &str) -> (String, i32) {
    let (mantissa, exponent) = value.split_once(['e', 'E']).unwrap_or((value, "0"));
    let mut exponent: i32 = exponent.parse().unwrap();
    exponent -= mantissa
        .split_once('.')
        .map_or(0, |(_, part)| part.len() as i32);
    let negative = mantissa.starts_with('-');
    let digits = mantissa.trim_start_matches(['-', '+']).replace('.', "");
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return ("0".into(), 0);
    }
    let significant = digits.trim_end_matches('0');
    exponent += (digits.len() - significant.len()) as i32;
    (
        format!("{}{significant}", if negative { "-" } else { "" }),
        exponent,
    )
}

fn evaluate(input: &Value) -> Result<Value> {
    let vault = RunVault::new(Duration::from_secs(3600))?;
    let capability = vault.begin_run("fixture-test", 1)?;
    let mut refs = Vec::new();
    for value in input["values"].as_array().unwrap() {
        let mut registration = Registration::new(
            input["entity_type"].as_str().unwrap_or("NUMBER"),
            BTreeSet::from(["confidential".into()]),
        );
        registration.metadata = ValueMetadata {
            unit: input["unit"].as_str().map(str::to_owned),
            ..Default::default()
        };
        refs.push(vault.register_value(&capability, 1, value.as_str().unwrap(), registration)?);
    }
    let operation: Operation = serde_json::from_value(input["operation"].clone()).unwrap();
    let result = compute(&vault, &capability, 1, operation, &refs, &input["options"])?;
    let mut output = serde_json::Map::new();
    for (name, reference) in result.results {
        output.insert(
            name,
            vault
                .restore_text(&capability, 1, reference.as_str())?
                .into(),
        );
    }
    Ok(output.into())
}

#[test]
fn arithmetic_matches_one_hundred_twenty_seven_frozen_reference_cases() {
    let cases = include_str!("fixtures/compute.jsonl");
    assert_eq!(cases.lines().count(), 127);
    for (index, line) in cases.lines().enumerate() {
        let case: Value = serde_json::from_str(line).unwrap();
        let actual =
            evaluate(&case["input"]).unwrap_or_else(|error| json!({"error": error.code()}));
        let expected = &case["expected"];
        if expected.get("error").is_some() || actual.get("error").is_some() {
            assert_eq!(&actual, expected, "fixture {index}");
            continue;
        }
        let actual = actual.as_object().unwrap();
        let expected = expected.as_object().unwrap();
        assert_eq!(
            actual.keys().collect::<Vec<_>>(),
            expected.keys().collect::<Vec<_>>()
        );
        for (name, value) in expected {
            assert_eq!(
                canonical(actual[name].as_str().unwrap()),
                canonical(value.as_str().unwrap()),
                "fixture {index}, {name}"
            );
        }
    }
}
