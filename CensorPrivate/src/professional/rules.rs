//! Bundled patterns are data; execution, boundaries and validators are independent.
use super::{model::NerModel, AnalyzeOptions, Prediction};
use crate::{
    error::{Error, Result},
    references::TOKEN_RE,
};
use fancy_regex::Regex;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
#[serde(untagged)]
enum Group {
    Index(usize),
    Name(String),
}
#[derive(Deserialize)]
struct Pattern {
    #[serde(rename = "name")]
    _name: String,
    entity_type: String,
    expression: String,
    score: f64,
    group: Group,
    evidence: Vec<String>,
}
#[derive(Deserialize)]
struct RuleSpec {
    pattern: Pattern,
    context: Option<String>,
    context_window: usize,
    validator: Option<String>,
    failure_score: Option<f64>,
    failure_entity_type: Option<String>,
}
#[derive(Deserialize)]
struct Catalog {
    patterns: Vec<Pattern>,
    rules: Vec<RuleSpec>,
    fields: BTreeMap<String, String>,
    addresses: Vec<String>,
    plate: String,
    passport: String,
    number: String,
}
struct Rule {
    pattern: Pattern,
    expression: Regex,
    source: &'static str,
    context: Option<Regex>,
    window: usize,
    validator: Option<String>,
    failure_score: Option<f64>,
    failure_entity_type: Option<String>,
}
pub(super) struct Rules {
    rules: Vec<Rule>,
    fields: Vec<(String, Regex)>,
    addresses: Vec<Regex>,
    plate: Regex,
    passport: Regex,
    email: Regex,
    number: Regex,
}
fn compile(expression: &str, insensitive: bool) -> Result<Regex> {
    let unicode = regex::Regex::new(r"\\u([0-9a-fA-F]{4})").map_err(|_| Error::PolicyInvalid)?;
    let ascii = expression.contains("(?a:");
    let expression = unicode
        .replace_all(expression, r"\u{$1}")
        .replace("(?a:", "(?-i:")
        .replace(r"\Z", r"\z");
    let expression = if ascii {
        expression.replace("[A-Z", "[A-Za-z")
    } else {
        expression
    };
    fancy_regex::RegexBuilder::new(&format!(
        "{}{}",
        if insensitive { "(?i)" } else { "" },
        expression
    ))
    .backtrack_limit(8_000_000)
    .build()
    .map_err(|e| {
        eprintln!("invalid bundled detector pattern: {e}");
        Error::PolicyInvalid
    })
}
fn union(a: &mut Vec<String>, b: &[String]) {
    a.extend_from_slice(b);
    a.sort();
    a.dedup();
}
fn merge(old: &mut Prediction, new: Prediction) {
    // Validation evidence is independent of the score winner and merge order.
    let severity = |state: &str| match state {
        "failed" => 2,
        "passed" => 1,
        _ => 0,
    };
    if severity(&new.validation) > severity(&old.validation) {
        old.validation = new.validation.clone();
    }
    old.score = old.score.max(new.score);
    union(&mut old.sources, &new.sources);
    union(&mut old.evidence, &new.evidence);
    if old.unit.is_none() {
        old.unit = new.unit;
    }
}
fn key(p: &Prediction) -> (String, usize, usize) {
    (p.entity_type.clone(), p.start, p.end)
}
fn dedup(items: Vec<Prediction>) -> Vec<Prediction> {
    let mut map = BTreeMap::new();
    for item in items {
        map.entry(key(&item))
            .and_modify(|old| merge(old, item.clone()))
            .or_insert(item);
    }
    map.into_values().collect()
}
fn trim_address(text: &str, mut start: usize, mut end: usize) -> (usize, usize) {
    while start < end && matches!(text.as_bytes()[start], b' ' | 9) {
        start += 1;
    }
    while end > start {
        let c = text[..end].chars().next_back().unwrap();
        if c == ' ' || c == char::from(9) || c == ',' || c == '，' {
            end -= c.len_utf8();
        } else {
            break;
        }
    }
    (start, end)
}
fn china_id(value: &str) -> bool {
    let groups: Vec<_> = value.split([' ', '\t']).filter(|s| !s.is_empty()).collect();
    if value.contains([' ', '\t'])
        && (value.starts_with([' ', '\t'])
            || value.ends_with([' ', '\t'])
            || groups.len() != 3
            || groups.iter().map(|s| s.len()).ne([6, 8, 4]))
    {
        return false;
    }
    let value = value.replace([' ', '\t'], "");
    let bytes = value.as_bytes();
    if bytes.len() != 18 || bytes[0] == b'0' || !bytes[..17].iter().all(u8::is_ascii_digit) {
        return false;
    }
    let date = |range: std::ops::Range<usize>| value.get(range)?.parse::<u32>().ok();
    if date(6..10)
        // chrono accepts year zero; Python datetime.date (the oracle) does not.
        .filter(|year| *year > 0)
        .and_then(|y| chrono::NaiveDate::from_ymd_opt(y as i32, date(10..12)?, date(12..14)?))
        .is_none()
    {
        return false;
    }
    let weights = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2];
    let sum: usize = bytes[..17]
        .iter()
        .zip(weights)
        .map(|(d, w)| usize::from(d - b'0') * w)
        .sum();
    bytes[17].to_ascii_uppercase() == b"10X98765432"[sum % 11]
}
fn credit_card(value: &str) -> bool {
    let digits: Vec<_> = value
        .bytes()
        .filter(|b| !matches!(b, b' ' | b'-'))
        .collect();
    if !(13..=19).contains(&digits.len())
        || !digits.iter().all(u8::is_ascii_digit)
        || digits.iter().all(|d| *d == digits[0])
    {
        return false;
    }
    digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| {
            let n = u32::from(d - b'0') * if i % 2 == 1 { 2 } else { 1 };
            if n > 9 {
                n - 9
            } else {
                n
            }
        })
        .sum::<u32>()
        % 10
        == 0
}
impl Rules {
    pub(super) fn new() -> Result<Self> {
        let data: Catalog =
            serde_json::from_str(include_str!("catalog.json")).map_err(|_| Error::PolicyInvalid)?;
        let email = compile(
            &data
                .patterns
                .iter()
                .find(|p| p._name == "email")
                .ok_or(Error::PolicyInvalid)?
                .expression,
            true,
        )?;
        let mut rules = Vec::new();
        for pattern in data.patterns {
            rules.push(Rule {
                expression: compile(&pattern.expression, true)?,
                pattern,
                source: "regex",
                context: None,
                window: 32,
                validator: None,
                failure_score: None,
                failure_entity_type: None,
            });
        }
        for spec in data.rules {
            rules.push(Rule {
                expression: compile(&spec.pattern.expression, true)?,
                pattern: spec.pattern,
                source: "rule",
                context: spec
                    .context
                    .as_ref()
                    .map(|p| compile(p, true))
                    .transpose()?,
                window: spec.context_window,
                validator: spec.validator,
                failure_score: spec.failure_score,
                failure_entity_type: spec.failure_entity_type,
            });
        }
        Ok(Self {
            rules,
            fields: data
                .fields
                .into_iter()
                .map(|(k, p)| Ok((k, compile(&p, true)?)))
                .collect::<Result<_>>()?,
            addresses: data
                .addresses
                .iter()
                .map(|p| compile(p, false))
                .collect::<Result<_>>()?,
            plate: compile(&data.plate, true)?,
            passport: compile(&format!("\\A(?:{})\\z", data.passport), true)?,
            email,
            number: compile(&data.number, false)?,
        })
    }
    fn process(
        &self,
        text: &str,
        mut p: Prediction,
        rule: Option<&Rule>,
    ) -> Result<Option<Prediction>> {
        let original = (p.start, p.end);
        if p.entity_type == "ADDRESS" {
            (p.start, p.end) = trim_address(text, p.start, p.end);
        }
        if ["ADDRESS", "LOCATION"].contains(&p.entity_type.as_str()) {
            'search: for pattern in &self.addresses {
                for capture in pattern.captures_iter(text) {
                    let capture = capture.map_err(|_| Error::DetectionFailed)?;
                    let m = capture.name("value").ok_or(Error::DetectionFailed)?;
                    if m.start() <= p.start && p.start < p.end && p.end <= m.end() {
                        p.start = m.start();
                        p.end = m.end();
                        break 'search;
                    }
                }
            }
        }
        if p.start >= p.end || text.get(p.start..p.end).is_none() {
            return Ok(None);
        }
        if p.entity_type == "PERSON" {
            for matched in self.email.find_iter(text) {
                let matched = matched.map_err(|_| Error::DetectionFailed)?;
                if matched.start() <= p.start && p.end <= matched.end() {
                    return Ok(None);
                }
            }
        }
        if let Some(rule) = rule {
            if let Some(context) = &rule.context {
                let offsets: Vec<_> = text
                    .char_indices()
                    .map(|(i, _)| i)
                    .chain(std::iter::once(text.len()))
                    .collect();
                let s = offsets
                    .binary_search(&p.start)
                    .map_err(|_| Error::DetectionFailed)?;
                let e = offsets
                    .binary_search(&p.end)
                    .map_err(|_| Error::DetectionFailed)?;
                let ranges = [
                    (s.saturating_sub(rule.window), s),
                    (e, (e + rule.window).min(offsets.len() - 1)),
                ];
                let mut found = false;
                for (a, b) in ranges {
                    let upper = offsets[(b + 1).min(offsets.len() - 1)];
                    for m in context.find_iter(&text[..upper]) {
                        let m = m.map_err(|_| Error::DetectionFailed)?;
                        if m.start() >= offsets[a] && m.end() <= offsets[b] {
                            found = true;
                            break;
                        }
                    }
                }
                if !found {
                    return Ok(None);
                }
                p.evidence.push("context".into());
            }
        }
        let value = &text[p.start..p.end];
        let validator = rule.and_then(|r| r.validator.as_deref());
        let valid = match validator.unwrap_or(&p.entity_type) {
            "valid_china_id" | "CHINA_ID" => Some(china_id(value)),
            "valid_credit_card" | "CREDIT_CARD" => Some(credit_card(value)),
            "IP_ADDRESS" => Some(value.parse::<std::net::IpAddr>().is_ok()),
            "valid_passport" => Some(
                self.passport
                    .is_match(value)
                    .map_err(|_| Error::DetectionFailed)?,
            ),
            "valid_license_plate" | "LICENSE_PLATE" => {
                let compact = value.replace([' ', '·'], "");
                let full = self
                    .plate
                    .find(value)
                    .map_err(|_| Error::DetectionFailed)?
                    .is_some_and(|m| m.start() == 0 && m.end() == value.len());
                Some(
                    full && (compact.chars().count() == 8
                        || compact
                            .chars()
                            .skip(2)
                            .filter(|c| c.is_alphabetic())
                            .count()
                            <= 2),
                )
            }
            _ => None,
        };
        if p.entity_type == "CHINA_ID" && valid == Some(true) {
            p.score = p.score.max(0.99);
        }
        if valid == Some(false) {
            let fallback = if validator.is_some() {
                rule.and_then(|r| r.failure_score)
            } else if p.entity_type == "CHINA_ID" {
                Some(0.6)
            } else {
                None
            };
            let Some(score) = fallback else {
                return Ok(None);
            };
            p.score = score;
            if let Some(kind) = rule.and_then(|r| r.failure_entity_type.as_ref()) {
                p.entity_type = kind.clone();
            }
        }
        if p.entity_type == "NUMBER" {
            p.evidence.push("numeric_baseline".into());
        }
        p.validation = match valid {
            Some(true) => "passed",
            Some(false) => "failed",
            None => "not_checked",
        }
        .into();
        if valid == Some(true) {
            p.evidence.push("validation".into());
        }
        if original != (p.start, p.end) && p.evidence.iter().any(|s| s == "ner") {
            p.evidence.push("address_structure".into());
        }
        Ok(Some(p))
    }
    fn fragment(
        &self,
        text: &str,
        structural: bool,
        ner: Option<&mut NerModel>,
    ) -> Result<Vec<Prediction>> {
        let mut output = Vec::new();
        for rule in &self.rules {
            for capture in rule.expression.captures_iter(text) {
                let capture = capture.map_err(|_| Error::DetectionFailed)?;
                let matched = match &rule.pattern.group {
                    Group::Index(i) => capture.get(*i),
                    Group::Name(n) => capture.name(n),
                };
                let Some(m) = matched else { continue };
                if m.start() == m.end() {
                    continue;
                }
                let (mut start, mut end) = (m.start(), m.end());
                if rule.source == "rule"
                    && matches!(&rule.pattern.group,Group::Name(n) if n=="value")
                    && end - start >= 2
                    && matches!(text.as_bytes()[start], b'"' | 39)
                    && text.as_bytes()[start] == text.as_bytes()[end - 1]
                {
                    start += 1;
                    end -= 1;
                }
                let p = Prediction {
                    entity_type: rule.pattern.entity_type.clone(),
                    start,
                    end,
                    score: rule.pattern.score,
                    sources: vec![rule.source.into()],
                    validation: "not_checked".into(),
                    evidence: if rule.pattern.evidence.is_empty() {
                        vec!["format".into()]
                    } else {
                        rule.pattern.evidence.clone()
                    },
                    unit: None,
                };
                if let Some(p) = self.process(text, p, Some(rule))? {
                    output.push(p);
                }
                if output.len() > 65536 {
                    return Err(Error::ResourceLimit);
                }
            }
        }
        if !structural {
            for e in ner.ok_or(Error::DetectionFailed)?.predict(text)? {
                let kind = match e.label.as_str() {
                    "PER" => "PERSON",
                    "ORG" => "ORGANIZATION",
                    "LOC" => "LOCATION",
                    "TIME" => "DATE_TIME",
                    _ => return Err(Error::DetectionFailed),
                };
                let p = Prediction {
                    entity_type: kind.into(),
                    start: e.start,
                    end: e.end,
                    score: e.score,
                    sources: vec!["ner".into()],
                    validation: "not_checked".into(),
                    evidence: vec!["ner".into()],
                    unit: None,
                };
                if let Some(p) = self.process(text, p, None)? {
                    if p.score >= 0.5 {
                        output.push(p);
                    }
                }
            }
        }
        let mut output = dedup(output);
        let specific: Vec<_> = output
            .iter()
            .filter(|p| p.entity_type != "NUMBER")
            .map(|p| (p.start, p.end))
            .collect();
        let ids: Vec<_> = output
            .iter()
            .filter(|p| p.entity_type == "CHINA_ID" && p.validation == "passed")
            .map(|p| (p.start, p.end))
            .collect();
        let cards: Vec<_> = output
            .iter()
            .filter(|p| p.entity_type == "CREDIT_CARD" && p.evidence.iter().any(|e| e == "field"))
            .map(|p| (p.start, p.end))
            .collect();
        output.retain(|p| {
            let contained = |ranges: &[(usize, usize)]| {
                ranges.iter().any(|(s, e)| *s <= p.start && p.end <= *e)
            };
            let field = p.evidence.iter().any(|e| e == "field");
            match p.entity_type.as_str() {
                "NUMBER" => !contained(&specific),
                "ID_NUMBER" => !contained(&ids),
                "CREDIT_CARD" if !field => !contained(&ids),
                "CHINA_ID" if !field => !contained(&cards),
                _ => true,
            }
        });
        Ok(output)
    }
    pub(super) fn analyze(
        &self,
        text: &str,
        options: &AnalyzeOptions,
        mut ner: Option<&mut NerModel>,
    ) -> Result<Vec<Prediction>> {
        let references: Vec<_> = TOKEN_RE
            .find_iter(text)
            .map(|m| (m.start(), m.end()))
            .collect();
        let mut scan = zeroize::Zeroizing::new(text.to_owned());
        for (s, e) in references.iter().rev() {
            scan.replace_range(*s..*e, &"#".repeat(e - s));
        }
        let mut results = Vec::new();
        let mut lower = 0;
        for (upper, next) in references
            .iter()
            .copied()
            .chain(std::iter::once((text.len(), text.len())))
        {
            if !text[lower..upper].trim().is_empty() {
                for mut p in
                    self.fragment(&text[lower..upper], options.structural, ner.as_deref_mut())?
                {
                    p.start += lower;
                    p.end += lower;
                    results.push(p);
                }
            }
            lower = next;
        }
        if !options.field.is_empty() && !options.structural && !scan.trim().is_empty() {
            for (kind, pattern) in &self.fields {
                if pattern
                    .find(&options.field)
                    .map_err(|_| Error::DetectionFailed)?
                    .is_some_and(|m| m.start() == 0 && m.end() == options.field.len())
                {
                    let spec = Rule {
                        pattern: Pattern {
                            _name: "field".into(),
                            entity_type: kind.clone(),
                            expression: String::new(),
                            score: 0.85,
                            group: Group::Index(0),
                            evidence: vec![],
                        },
                        expression: compile("", false)?,
                        source: "rule",
                        context: None,
                        window: 32,
                        validator: match kind.as_str() {
                            "CHINA_ID" => Some("valid_china_id".into()),
                            "CREDIT_CARD" => Some("valid_credit_card".into()),
                            _ => None,
                        },
                        failure_score: Some(0.6),
                        failure_entity_type: if kind == "CREDIT_CARD" {
                            Some("ID_NUMBER".into())
                        } else {
                            None
                        },
                    };
                    let p = Prediction {
                        entity_type: kind.clone(),
                        start: 0,
                        end: scan.len(),
                        score: if ["SECRET", "PASSWORD"].contains(&kind.as_str()) {
                            0.95
                        } else {
                            0.85
                        },
                        sources: vec!["rule".into()],
                        validation: "not_checked".into(),
                        evidence: vec!["field".into()],
                        unit: None,
                    };
                    if let Some(p) = self.process(&scan, p, Some(&spec))? {
                        results.push(p);
                    }
                }
            }
        }
        results.retain(|p| match p.entity_type.as_str() {
            "MONEY" if !options.mask_money => false,
            "DATE_TIME" if !options.mask_dates => false,
            "NUMBER" => options.mask_all_numbers,
            _ => p.score >= 0.6,
        });
        if results.len() > 65536 {
            return Err(Error::ResourceLimit);
        }
        let mut seen: BTreeSet<_> = results.iter().map(key).collect();
        let mut templates: BTreeMap<(String, String), Prediction> = BTreeMap::new();
        for p in &results {
            let value = &scan[p.start..p.end];
            let numeric = p.entity_type == "NUMBER"
                || value.chars().any(|c| c.is_numeric())
                    && value
                        .chars()
                        .all(|c| c.is_numeric() || " +-.,eE".contains(c));
            if numeric || value.trim().is_empty() {
                continue;
            }
            templates
                .entry((p.entity_type.clone(), value.into()))
                .and_modify(|old| merge(old, p.clone()))
                .or_insert_with(|| p.clone());
        }
        for ((_, value), p) in templates {
            let mut offset = 0;
            while let Some(relative) = scan[offset..].find(&value) {
                let start = offset + relative;
                let end = start + value.len();
                if seen.insert((p.entity_type.clone(), start, end)) {
                    let mut p = p.clone();
                    p.start = start;
                    p.end = end;
                    results.push(p);
                    if results.len() > 65536 {
                        return Err(Error::ResourceLimit);
                    }
                }
                offset = start
                    + scan[start..]
                        .chars()
                        .next()
                        .ok_or(Error::DetectionFailed)?
                        .len_utf8();
            }
        }
        let mut output = Vec::new();
        for p in results {
            let mut ranges = vec![(p.start, p.end)];
            for (lower, upper) in &references {
                ranges = ranges
                    .into_iter()
                    .flat_map(|(s, e)| [(s, e.min(*lower)), (s.max(*upper), e)])
                    .filter(|(s, e)| s < e)
                    .collect();
            }
            for (start, end) in ranges {
                if text[start..end].trim().is_empty() {
                    continue;
                }
                let mut item = p.clone();
                item.start = start;
                item.end = end;
                item.entity_type = match p.entity_type.as_str() {
                    "EMAIL_ADDRESS" => "EMAIL",
                    "PHONE_NUMBER" => "PHONE",
                    "CHINA_ID" | "ID_NUMBER" => "ID",
                    "CREDIT_CARD" => "ACCOUNT",
                    "PASSWORD" => "SECRET",
                    "ORGANIZATION" => "ORG",
                    "DATE_TIME" => "DATE",
                    other => other,
                }
                .into();
                if item.entity_type == "MONEY" {
                    let fragment = &text[start..end];
                    match self
                        .number
                        .find(fragment)
                        .map_err(|_| Error::DetectionFailed)?
                    {
                        Some(number)
                            if !fragment[number.end()..]
                                .trim_start()
                                .starts_with(['万', '亿', 'k', 'K', 'm', 'M']) =>
                        {
                            let currencies = [
                                ("人民币", "CNY"),
                                ("CNY", "CNY"),
                                ("RMB", "CNY"),
                                ("USD", "USD"),
                                ("EUR", "EUR"),
                                ("GBP", "GBP"),
                                ("JPY", "JPY"),
                                ("HKD", "HKD"),
                                ("AUD", "AUD"),
                                ("CAD", "CAD"),
                                ("CHF", "CHF"),
                                ("美元", "USD"),
                                ("欧元", "EUR"),
                                ("英镑", "GBP"),
                                ("日元", "JPY"),
                                ("港币", "HKD"),
                                ("￥", "CNY"),
                                ("¥", "CNY"),
                                ("元", "CNY"),
                                ("$", "USD"),
                                ("€", "EUR"),
                                ("£", "GBP"),
                            ];
                            item.unit = currencies
                                .iter()
                                .find(|(token, _)| {
                                    fragment.to_lowercase().contains(&token.to_lowercase())
                                })
                                .map(|(_, unit)| (*unit).into());
                            item.start = start + number.start();
                            item.end = start + number.end();
                        }
                        _ => item.entity_type = "CONFIDENTIAL".into(),
                    }
                }
                output.push(item);
            }
        }
        let mut output = dedup(output);
        output.sort_by(|a, b| {
            (a.start, a.end, &a.entity_type).cmp(&(b.start, b.end, &b.entity_type))
        });
        Ok(output)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn structural_reference_requires_no_python_or_model() {
        let rules = Rules::new().unwrap();
        let mut checked = 0;
        for line in include_str!("../../tests/fixtures/native-detector-reference.jsonl").lines() {
            let mut row: serde_json::Value = serde_json::from_str(line).unwrap();
            if row["request"]["structural"] != true {
                continue;
            }
            let text = row["request"]["text"].as_str().unwrap().to_owned();
            row["request"].as_object_mut().unwrap().remove("text");
            let options: AnalyzeOptions = serde_json::from_value(row["request"].clone()).unwrap();
            let actual = rules.analyze(&text, &options, None).unwrap();
            let mut actual = serde_json::to_value(actual).unwrap();
            let expected = &mut row["results"];
            for values in [&mut actual, expected] {
                for item in values.as_array_mut().unwrap() {
                    for key in ["sources", "evidence"] {
                        item[key]
                            .as_array_mut()
                            .unwrap()
                            .sort_by_key(|v| v.as_str().unwrap().to_owned());
                    }
                }
            }
            assert_eq!(actual, row["results"], "structural oracle row {checked}");
            checked += 1;
        }
        assert_eq!(checked, 25);
    }
    #[test]
    fn bundled_rules_compile() {
        assert!(Rules::new().is_ok());
    }
    #[test]
    fn validation_checks() {
        assert!(china_id("11010519491231002X"));
        assert!(china_id("110105 19491231 002X"));
        assert!(china_id("110105\t19491231\t002X"));
        assert!(!china_id("1101 0519491231002X"));
        assert!(!china_id(" 110105 19491231 002X"));
        assert!(!china_id("110105194912310021"));
        assert!(!china_id("11010519990230002X"));
        assert!(!china_id("11010500000101001X")); // Valid checksum, invalid year.
        assert!(credit_card("4111 1111 1111 1111"));
        assert!(!credit_card("0000000000000000"));
    }

    fn structural(text: &str) -> Vec<Prediction> {
        static RULES: std::sync::LazyLock<Rules> =
            std::sync::LazyLock::new(|| Rules::new().unwrap());
        RULES
            .analyze(
                text,
                &AnalyzeOptions {
                    structural: true,
                    ..Default::default()
                },
                None,
            )
            .unwrap()
    }

    #[test]
    fn typed_cards_ids_and_passports_preserve_complete_values() {
        for (text, kind, value, validation, score) in [
            (
                "还款卡号4111111111111112；普通数量42",
                "ACCOUNT",
                "4111111111111112",
                "failed",
                0.6,
            ),
            (
                "转账到张三的卡 4111111111111112",
                "ACCOUNT",
                "4111111111111112",
                "failed",
                0.6,
            ),
            (
                "4111111111111112 张三的银行卡",
                "ACCOUNT",
                "4111111111111112",
                "failed",
                0.6,
            ),
            (
                "4111 1111 1111 1111",
                "ACCOUNT",
                "4111 1111 1111 1111",
                "passed",
                0.8,
            ),
            (
                "🛡️身份证号110105 19491231 002X",
                "ID",
                "110105 19491231 002X",
                "passed",
                0.99,
            ),
            (
                "身份证：110105 19491231 0021",
                "ID",
                "110105 19491231 0021",
                "failed",
                0.6,
            ),
            ("护照E12345678", "ID", "E12345678", "passed", 0.85),
            ("护照发你了 EA1234567", "ID", "EA1234567", "passed", 0.85),
            (
                "passport number: EB1234567",
                "ID",
                "EB1234567",
                "passed",
                0.85,
            ),
        ] {
            let results = structural(text);
            let found: Vec<_> = results.iter().filter(|p| p.entity_type == kind).collect();
            assert_eq!(found.len(), 1, "{text}");
            let p = found[0];
            assert_eq!(
                (&text[p.start..p.end], p.validation.as_str(), p.score),
                (value, validation, score),
                "{text}"
            );
        }
        for text in [
            "库存4111111111111112",
            "信用卡不是4111111111111112",
            "银行卡\n4111111111111112",
            "护照E123456789",
            "护照EI1234567",
            "订单E12345678",
        ] {
            assert!(
                structural(text)
                    .iter()
                    .all(|p| !["ID", "ACCOUNT"].contains(&p.entity_type.as_str())),
                "{text}"
            );
        }
        // A generic ID field must not create a truncated second candidate.
        let text = "证件号码：110105 19491231 002X";
        let results = structural(text);
        assert_eq!(results.len(), 1);
        assert_eq!(
            &text[results[0].start..results[0].end],
            "110105 19491231 002X"
        );
        assert_eq!(results[0].validation, "passed");
    }

    #[test]
    fn plate_context_protects_typos_without_validating_them() {
        for value in [
            "京A12345",
            "粤B12C34",
            "沪AD12345",
            "粤B12345F",
            "浙A·12345",
            "苏A d12345",
        ] {
            let results = structural(value);
            let p = results
                .iter()
                .find(|p| p.entity_type == "LICENSE_PLATE")
                .unwrap();
            assert_eq!(
                (&value[p.start..p.end], p.validation.as_str(), p.score),
                (value, "passed", 0.9)
            );
        }
        for (text, value) in [
            ("🛡️车牌号码：\"京A12I45\"，电话13800138000", "京A12I45"),
            ("名下车辆京AABCDE", "京AABCDE"),
            ("京A12I45这个车剐蹭了", "京A12I45"),
        ] {
            let results = structural(text);
            let found: Vec<_> = results
                .iter()
                .filter(|p| p.entity_type == "LICENSE_PLATE")
                .collect();
            assert_eq!(found.len(), 1);
            let p = found[0];
            assert_eq!(
                (&text[p.start..p.end], p.validation.as_str(), p.score),
                (value, "failed", 0.6)
            );
            assert!(p.evidence.iter().any(|e| e == "plate_context"));
        }
        for text in [
            "京AABCDE",
            "产品编号京AABCDE",
            "车牌待登记；产品编号京AABCDE",
            "车牌号京AABCDE123",
            "京A12345警",
            "车牌不是京AABCDE",
            "京AABCDE；这辆车暂未登记",
        ] {
            assert!(
                structural(text)
                    .iter()
                    .all(|p| p.entity_type != "LICENSE_PLATE"),
                "{text}"
            );
        }
    }

    #[test]
    fn addresses_stop_before_following_contact_fields() {
        for (prefix, address, suffix) in [
            (
                "收货地址：",
                "北京市海淀区中关村大街27号2号楼3单元301室",
                "，联系人：张三，电话：13800138000。",
            ),
            (
                "配送地址：",
                "广东省广州市荔湾区人民路68号27栋1442室",
                "，联系人张三（13800138000）",
            ),
            (
                "地址：",
                "杭州市西湖区文三路90号1单元301室",
                "电话：13800138000",
            ),
            ("外卖送到", "海淀南京东路73号", " 联系张三13800138000"),
            ("地址", "南山中关村117号", " 联系张三13800138000"),
            ("我住在", "天河西溪路187号", " 联系张三13800138000"),
            ("寄到", "东城文昌巷8号2栋301室", " 联系张三13800138000"),
            (
                "address: \"",
                "123 Example Street, London",
                "\" phone: 13800138000",
            ),
        ] {
            let text = format!("🛡️{prefix}{address}{suffix}");
            let results = structural(&text);
            let addresses: Vec<_> = results
                .iter()
                .filter(|p| p.entity_type == "ADDRESS")
                .map(|p| &text[p.start..p.end])
                .collect();
            assert_eq!(addresses, [address], "{text}");
        }
        for text in [
            "我在会议室3号开会",
            "现在任务42个",
            "地址待登记；计划42号任务",
        ] {
            assert!(
                structural(text).iter().all(|p| p.entity_type != "ADDRESS"),
                "{text}"
            );
        }
    }

    fn candidate(kind: &str, start: usize, end: usize) -> Prediction {
        Prediction {
            entity_type: kind.into(),
            start,
            end,
            score: 0.9,
            sources: vec!["ner".into()],
            validation: "not_checked".into(),
            evidence: vec!["ner".into()],
            unit: None,
        }
    }

    #[test]
    fn ner_context_rejects_mailbox_names_and_completes_addresses() {
        let rules = Rules::new().unwrap();
        let text = "张三的邮箱 alice@example.com";
        assert!(rules
            .process(text, candidate("PERSON", 0, "张三".len()), None)
            .unwrap()
            .is_some());
        let start = text.find("alice").unwrap();
        assert!(rules
            .process(text, candidate("PERSON", start, start + 5), None)
            .unwrap()
            .is_none());
        let address = "广东省广州市荔湾区人民路68号27栋1442室";
        let text = format!("配送地址：{address}，联系人张三");
        let start = text.find("荔湾区").unwrap();
        let p = rules
            .process(
                &text,
                candidate("LOCATION", start, start + "荔湾区".len()),
                None,
            )
            .unwrap()
            .unwrap();
        assert_eq!(&text[p.start..p.end], address);
        assert!(p.evidence.iter().any(|e| e == "address_structure"));
    }

    #[test]
    fn validation_merge_is_independent_of_score_and_order() {
        let mut items = [
            candidate("ID", 0, 3),
            candidate("ID", 0, 3),
            candidate("ID", 0, 3),
        ];
        items[1].score = 0.8;
        items[1].validation = "passed".into();
        items[2].score = 0.6;
        items[2].validation = "failed".into();
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let mut p = items[order[0]].clone();
            merge(&mut p, items[order[1]].clone());
            merge(&mut p, items[order[2]].clone());
            assert_eq!((p.score, p.validation.as_str()), (0.9, "failed"));
        }
    }

    #[test]
    fn explicit_card_evidence_resolves_numbers_that_pass_both_checks() {
        // Synthetic value satisfies both the national-ID checksum and Luhn.
        let value = "110105194912310150";
        assert!(china_id(value) && credit_card(value));
        for (text, kind) in [
            (value.to_owned(), "ID"),
            (format!("证件号码：{value}"), "ID"),
            (format!("银行卡：{value}"), "ACCOUNT"),
        ] {
            let results = structural(&text);
            assert_eq!(results.len(), 1, "{text}");
            assert_eq!(
                (
                    results[0].entity_type.as_str(),
                    results[0].validation.as_str()
                ),
                (kind, "passed")
            );
        }
    }
}
