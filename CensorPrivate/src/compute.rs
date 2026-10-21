//! Exact, bounded decimal computation. No floating point and no raw result API.
use crate::{
    error::{Error, Result},
    references::Reference,
    vault::{Registration, RunCapability, RunVault, ValueMetadata, VaultEntry},
};
use chrono::NaiveDate;
use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{Signed, Zero};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::LazyLock,
};
use zeroize::Zeroizing;

static NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[+-]?(?:[0-9]{1,3}(?:,[0-9]{3})+|[0-9]+)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]{1,3})?$")
        .expect("constant numeric grammar")
});
static DATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([0-9]{4})-([0-9]{1,2})-([0-9]{1,2})$").expect("constant date grammar")
});

struct Decimal {
    coefficient: BigInt,
    scale: i32,
}

impl Decimal {
    fn parse(value: &str) -> Result<Self> {
        let value = value.trim();
        if value.len() > 160 || !NUMBER.is_match(value) {
            return Err(Error::ComputeInvalid);
        }
        let normalized = Zeroizing::new(value.replace(',', ""));
        let (mantissa, exponent) = normalized
            .split_once(['e', 'E'])
            .unwrap_or((&normalized, "0"));
        let exponent: i32 = exponent.parse().map_err(|_| Error::ComputeInvalid)?;
        let scale = mantissa
            .split_once('.')
            .map_or(0, |(_, fraction)| fraction.len() as i32)
            - exponent;
        let digits = Zeroizing::new(mantissa.replace('.', ""));
        let coefficient: BigInt = digits.parse().map_err(|_| Error::ComputeInvalid)?;
        let result = Self { coefficient, scale };
        result.check(50)?;
        Ok(result)
    }
    fn digits(&self) -> usize {
        // Decimal's significant digit count includes trailing zeroes.
        let digits = Zeroizing::new(self.coefficient.abs().to_str_radix(10));
        digits.len()
    }
    fn check(&self, precision: usize) -> Result<()> {
        let digits = self.digits();
        let adjusted = digits as i32 - self.scale - 1;
        if digits > precision || adjusted.unsigned_abs() > 128 {
            return Err(Error::ComputeInvalid);
        }
        Ok(())
    }
    fn pow10(exponent: u32) -> BigInt {
        BigInt::from(10).pow(exponent)
    }
    fn aligned(&self, scale: i32) -> BigInt {
        &self.coefficient * Self::pow10((scale - self.scale) as u32)
    }
    fn add(&self, other: &Self, subtract: bool) -> Self {
        let scale = self.scale.max(other.scale);
        let rhs = other.aligned(scale);
        Self {
            coefficient: if subtract {
                self.aligned(scale) - rhs
            } else {
                self.aligned(scale) + rhs
            },
            scale,
        }
    }
    fn compare(&self, other: &Self) -> std::cmp::Ordering {
        let scale = self.scale.max(other.scale);
        self.aligned(scale).cmp(&other.aligned(scale))
    }
    fn multiply(&self, other: &Self) -> Self {
        Self {
            coefficient: &self.coefficient * &other.coefficient,
            scale: self.scale + other.scale,
        }
    }
    fn divide(&self, other: &Self) -> Result<Self> {
        if other.coefficient.is_zero() {
            return Err(Error::ComputeInvalid);
        }
        let divisor = self.coefficient.gcd(&other.coefficient);
        let mut numerator = &self.coefficient / &divisor;
        let mut denominator = &other.coefficient / divisor;
        if denominator.is_negative() {
            numerator = -numerator;
            denominator = -denominator;
        }
        let mut twos = 0u32;
        let mut fives = 0u32;
        while denominator.is_even() {
            denominator /= 2;
            twos += 1;
        }
        while (&denominator % 5u8).is_zero() {
            denominator /= 5;
            fives += 1;
        }
        // A reduced denominator containing any other prime never terminates.
        if denominator != BigInt::from(1) {
            return Err(Error::ComputeInvalid);
        }
        let places = twos.max(fives);
        let coefficient =
            numerator * BigInt::from(2).pow(places - twos) * BigInt::from(5).pow(places - fives);
        let mut result = Self {
            coefficient,
            scale: self.scale - other.scale + places as i32,
        };
        // Reduce only division results; finite exact arithmetic never rounds.
        while !result.coefficient.is_zero() && (&result.coefficient % 10u8).is_zero() {
            result.coefficient /= 10;
            result.scale -= 1;
        }
        result.check(512)?;
        Ok(result)
    }
    fn fixed(&self) -> Zeroizing<String> {
        let mut digits = Zeroizing::new(self.coefficient.abs().to_str_radix(10));
        if self.scale <= 0 {
            if !self.coefficient.is_zero() {
                digits.extend(std::iter::repeat_n('0', (-self.scale) as usize));
            }
        } else {
            let scale = self.scale as usize;
            if digits.len() <= scale {
                let mut prefix = Zeroizing::new(String::from("0."));
                prefix.extend(std::iter::repeat_n('0', scale - digits.len()));
                prefix.push_str(&digits);
                digits = prefix;
            } else {
                let at = digits.len() - scale;
                digits.insert(at, '.');
            }
        }
        if self.coefficient.is_negative() {
            digits.insert(0, '-');
        }
        digits
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Sum,
    Mean,
    Min,
    Max,
    Add,
    Subtract,
    Multiply,
    Divide,
    DateDiff,
    Aggregate,
    SumAndMean,
}
impl Operation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sum => "sum",
            Self::Mean => "mean",
            Self::Min => "min",
            Self::Max => "max",
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Multiply => "multiply",
            Self::Divide => "divide",
            Self::DateDiff => "date_diff",
            Self::Aggregate => "aggregate",
            Self::SumAndMean => "sum_and_mean",
        }
    }
}

#[derive(Serialize)]
pub struct ComputeResult {
    pub operation: Operation,
    pub results: BTreeMap<String, Reference>,
    pub entity_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Reference>,
}

fn date(entry: &VaultEntry) -> Result<NaiveDate> {
    if entry.entity_type != "DATE" {
        return Err(Error::ComputeInvalid);
    }
    let value = Zeroizing::new(entry.value.trim().replace(['年', '/', '月'], "-"));
    let value = value.strip_suffix('日').unwrap_or(&value);
    let captures = DATE.captures(value).ok_or(Error::ComputeInvalid)?;
    let year: i32 = captures[1].parse().map_err(|_| Error::ComputeInvalid)?;
    if year == 0 {
        return Err(Error::ComputeInvalid);
    }
    NaiveDate::from_ymd_opt(
        year,
        captures[2].parse().map_err(|_| Error::ComputeInvalid)?,
        captures[3].parse().map_err(|_| Error::ComputeInvalid)?,
    )
    .ok_or(Error::ComputeInvalid)
}

pub fn compute(
    vault: &RunVault,
    cap: &RunCapability,
    epoch: u64,
    operation: Operation,
    refs: &[Reference],
    options: &Value,
) -> Result<ComputeResult> {
    if refs.is_empty() || refs.len() > 256 {
        return Err(Error::ComputeInvalid);
    }
    let options = options.as_object().ok_or(Error::ComputeInvalid)?;
    let entries = vault.resolve_compute(cap, epoch, refs)?;
    let labels: BTreeSet<String> = entries
        .iter()
        .flat_map(|entry| entry.labels.iter().cloned())
        .collect();
    let (results, entity_type, unit) = if operation == Operation::DateDiff {
        if entries.len() != 2
            || options.keys().any(|key| key != "unit")
            || options.get("unit").is_some_and(|value| value != "days")
        {
            return Err(Error::ComputeInvalid);
        }
        let difference = (date(&entries[1])? - date(&entries[0])?).num_days();
        (
            BTreeMap::from([(
                "date_diff".into(),
                Decimal {
                    coefficient: difference.into(),
                    scale: 0,
                },
            )]),
            "DURATION".to_owned(),
            Some("days".to_owned()),
        )
    } else {
        let operations = if matches!(operation, Operation::Aggregate | Operation::SumAndMean) {
            if options.keys().any(|key| key != "operations") {
                return Err(Error::ComputeInvalid);
            }
            if let Some(value) = options.get("operations") {
                let values = value.as_array().ok_or(Error::ComputeInvalid)?;
                if values.is_empty() || values.len() > 256 {
                    return Err(Error::ComputeInvalid);
                }
                values
                    .iter()
                    .map(|value| {
                        let op: Operation = serde_json::from_value(value.clone())
                            .map_err(|_| Error::ComputeInvalid)?;
                        if !matches!(
                            op,
                            Operation::Sum | Operation::Mean | Operation::Min | Operation::Max
                        ) {
                            return Err(Error::ComputeInvalid);
                        }
                        Ok(op)
                    })
                    .collect::<Result<Vec<_>>>()?
            } else {
                vec![Operation::Sum, Operation::Mean]
            }
        } else {
            if !options.is_empty() {
                return Err(Error::ComputeInvalid);
            }
            vec![operation]
        };
        let first = &entries[0];
        if entries.iter().any(|entry| {
            entry.entity_type != first.entity_type || entry.metadata.unit != first.metadata.unit
        }) || !matches!(
            first.entity_type.as_str(),
            "MONEY" | "NUMBER" | "DURATION" | "PERCENT"
        ) {
            return Err(Error::ComputeInvalid);
        }
        let values = entries
            .iter()
            .map(|entry| Decimal::parse(&entry.value))
            .collect::<Result<Vec<_>>>()?;
        let mut results = BTreeMap::new();
        for op in operations {
            let sum = || {
                values.iter().fold(
                    Decimal {
                        coefficient: BigInt::zero(),
                        scale: 0,
                    },
                    |sum, value| sum.add(value, false),
                )
            };
            if matches!(
                op,
                Operation::Add | Operation::Subtract | Operation::Multiply | Operation::Divide
            ) && values.len() != 2
            {
                return Err(Error::ComputeInvalid);
            }
            let result = match op {
                Operation::Sum | Operation::Add => sum(),
                Operation::Mean => sum().divide(&Decimal {
                    coefficient: values.len().into(),
                    scale: 0,
                })?,
                Operation::Min | Operation::Max => {
                    let mut winner = &values[0];
                    for value in &values[1..] {
                        let order = value.compare(winner);
                        if (op == Operation::Min && order.is_lt())
                            || (op == Operation::Max && order.is_gt())
                        {
                            winner = value;
                        }
                    }
                    Decimal {
                        coefficient: winner.coefficient.clone(),
                        scale: winner.scale,
                    }
                }
                Operation::Subtract => values[0].add(&values[1], true),
                Operation::Multiply | Operation::Divide if first.entity_type == "NUMBER" => {
                    if op == Operation::Multiply {
                        values[0].multiply(&values[1])
                    } else {
                        values[0].divide(&values[1])?
                    }
                }
                _ => return Err(Error::ComputeInvalid),
            };
            result.check(512)?;
            results.insert(op.as_str().to_owned(), result);
        }
        (
            results,
            first.entity_type.clone(),
            first.metadata.unit.clone(),
        )
    };
    let mut protected = BTreeMap::new();
    for (name, value) in results {
        let expression = serde_json::to_vec(&serde_json::json!({"operation": name, "refs": refs}))
            .map_err(|_| Error::ComputeInvalid)?;
        let digest = Sha256::digest(expression);
        let entity_id = format!("privcalc:{digest:x}");
        let registration = Registration {
            entity_type: entity_type.clone(),
            labels: labels.clone(),
            metadata: ValueMetadata {
                unit: unit.clone(),
                entity_id: Some(entity_id),
                source: Some("privcalc".into()),
            },
            depends_on: refs.to_vec(),
            ttl: None,
        };
        // Publication repeats epoch and dependency checks. A concurrent reload
        // cannot grant old computation results access under a newer policy.
        protected.insert(
            name,
            vault.register_value(cap, epoch, &value.fixed(), registration)?,
        );
    }
    let result = if protected.len() == 1 {
        protected.values().next().cloned()
    } else {
        None
    };
    Ok(ComputeResult {
        operation,
        results: protected,
        entity_type,
        unit,
        result,
    })
}
