//! Canonical detection categories shared by policy rules and native inference.
use crate::{
    error::{Error, Result},
    policy::validate_entity_type,
};
use std::collections::BTreeSet;

pub const BUILTIN_CATEGORIES: &[&str] = &[
    "PERSON",
    "ORG",
    "LOCATION",
    "ADDRESS",
    "EMAIL",
    "PHONE",
    "ID",
    "ACCOUNT",
    "SECRET",
    "MONEY",
    "DATE",
    "NUMBER",
    "IP_ADDRESS",
    "MAC_ADDRESS",
    "LICENSE_PLATE",
    "MEDICAL_INFO",
    "CONFIDENTIAL",
];

pub fn canonical(kind: &str) -> &str {
    match kind {
        "PER" => "PERSON",
        "ORGANIZATION" => "ORG",
        "LOC" => "LOCATION",
        "EMAIL_ADDRESS" => "EMAIL",
        "PHONE_NUMBER" => "PHONE",
        "CHINA_ID" | "ID_NUMBER" | "US_SSN" => "ID",
        "CREDIT_CARD" => "ACCOUNT",
        "PASSWORD" => "SECRET",
        "DATE_TIME" | "TIME" => "DATE",
        other => other,
    }
}

pub type Categories = Option<BTreeSet<String>>;

pub fn normalize(items: &[String]) -> Result<BTreeSet<String>> {
    items
        .iter()
        .map(|item| {
            validate_entity_type(item).map_err(|_| Error::ConfigInvalid)?;
            Ok(canonical(item).to_owned())
        })
        .collect()
}

pub fn allows(categories: &Categories, kind: &str) -> bool {
    categories
        .as_ref()
        .is_none_or(|items| items.contains(canonical(kind)))
}

pub fn uses_ner(categories: &Categories) -> bool {
    ["PERSON", "ORG", "LOCATION", "DATE"]
        .iter()
        .any(|kind| allows(categories, kind))
}

pub fn intersect(left: &Categories, right: &Categories) -> Categories {
    match (left, right) {
        (Some(a), Some(b)) => Some(a.intersection(b).cloned().collect()),
        (Some(a), None) | (None, Some(a)) => Some(a.clone()),
        (None, None) => None,
    }
}
