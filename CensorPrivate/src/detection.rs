use crate::{
    error::{Error, Result},
    policy::{validate_entity_type, PrivacyPolicy},
    references::TOKEN_RE,
};
use fancy_regex::Regex;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Offsets are UTF-8 byte boundaries, validated against the source before use.
/// Span text is deliberately not copied or exposed through Debug.
#[derive(Clone)]
pub struct Span {
    pub entity_type: String,
    pub start: usize,
    pub end: usize,
    pub labels: BTreeSet<String>,
    pub metadata: BTreeMap<String, String>,
}

pub trait Detector: Send + Sync {
    fn detect(&self, text: &str) -> Result<Vec<Span>>;
}

#[derive(Clone)]
pub struct DetectorSpec {
    pub detector: Arc<dyn Detector>,
    pub required: bool,
}

/// Output of an explicitly supplied, already loaded local analyzer. Offsets are
/// Unicode scalar positions, never wire bytes.
pub struct AnalyzerMatch {
    pub entity_type: String,
    pub character_range: std::ops::Range<usize>,
}
pub trait LocalAnalyzer: Send + Sync {
    fn analyze(
        &self,
        text: &str,
        language: &str,
        score_threshold: f32,
    ) -> Result<Vec<AnalyzerMatch>>;
}
/// Adapter for a caller-supplied local analyzer. No model is constructed,
/// downloaded or allowed to bypass required rules here.
pub struct AnalyzerDetector {
    analyzer: Arc<dyn LocalAnalyzer>,
    language: String,
    threshold: f32,
    labels: BTreeSet<String>,
}
impl AnalyzerDetector {
    pub fn new(
        analyzer: Arc<dyn LocalAnalyzer>,
        language: String,
        threshold: f32,
        labels: BTreeSet<String>,
    ) -> Result<Self> {
        if language.is_empty()
            || language.len() > 32
            || !language
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
            || !threshold.is_finite()
            || !(0.0..=1.0).contains(&threshold)
            || labels.is_empty()
            || labels.iter().any(String::is_empty)
        {
            return Err(Error::PolicyInvalid);
        }
        Ok(Self {
            analyzer,
            language,
            threshold,
            labels,
        })
    }
}
impl Detector for AnalyzerDetector {
    fn detect(&self, text: &str) -> Result<Vec<Span>> {
        let predictions = self
            .analyzer
            .analyze(text, &self.language, self.threshold)?;
        if predictions.len() > 65_536 {
            return Err(Error::ResourceLimit);
        }
        let offsets: Vec<usize> = text
            .char_indices()
            .map(|(offset, _)| offset)
            .chain(std::iter::once(text.len()))
            .collect();
        let mut spans = Vec::with_capacity(predictions.len());
        for prediction in predictions {
            let entity_type = match prediction.entity_type.as_str() {
                "EMAIL_ADDRESS" => "EMAIL",
                "PHONE_NUMBER" => "PHONE",
                "DATE_TIME" => "DATE",
                "ORGANIZATION" => "ORG",
                "CREDIT_CARD" => "ACCOUNT",
                "US_SSN" => "ID",
                other => other,
            };
            spans.push(Span {
                entity_type: entity_type.into(),
                start: *offsets
                    .get(prediction.character_range.start)
                    .ok_or(Error::DetectionFailed)?,
                end: *offsets
                    .get(prediction.character_range.end)
                    .ok_or(Error::DetectionFailed)?,
                labels: self.labels.clone(),
                metadata: Default::default(),
            });
        }
        validate_spans(text, &spans)?;
        Ok(spans)
    }
}

pub fn validate_spans(text: &str, spans: &[Span]) -> Result<()> {
    for span in spans {
        if validate_entity_type(&span.entity_type).is_err()
            || span.start >= span.end
            || text.get(span.start..span.end).is_none()
            || span.labels.is_empty()
            || span.labels.iter().any(String::is_empty)
        {
            return Err(Error::DetectionFailed);
        }
    }
    Ok(())
}

fn priority(kind: &str) -> u8 {
    match kind {
        "SECRET" => 100,
        "CONFIDENTIAL" => 90,
        "ID" | "ACCOUNT" => 80,
        "DATE" => 70,
        "MONEY" => 60,
        "PHONE" | "EMAIL" => 50,
        "NUMBER" => 10,
        "LITERAL" => 0,
        _ => 40,
    }
}

/// Merge the entire union; selecting just the highest priority span can leak
/// the unselected tail of another detector's partially overlapping match.
pub fn merge_spans(text: &str, mut spans: Vec<Span>) -> Result<Vec<Span>> {
    validate_spans(text, &spans)?;
    spans.sort_by_key(|span| (span.start, span.end));
    let mut merged: Vec<Span> = Vec::new();
    for span in spans {
        if merged
            .last()
            .is_none_or(|previous| previous.end <= span.start)
        {
            merged.push(span);
            continue;
        }
        let previous = merged.pop().ok_or(Error::DetectionFailed)?;
        let start = previous.start;
        let end = previous.end.max(span.end);
        let labels = previous.labels.union(&span.labels).cloned().collect();
        let winner = if priority(&previous.entity_type) >= priority(&span.entity_type) {
            previous
        } else {
            span
        };
        let metadata = if winner.start == start && winner.end == end {
            winner.metadata
        } else {
            BTreeMap::new()
        };
        merged.push(Span {
            entity_type: winner.entity_type,
            start,
            end,
            labels,
            metadata,
        });
    }
    Ok(merged)
}

struct Rule {
    kind: String,
    expression: Regex,
    group: usize,
    metadata: BTreeMap<String, String>,
}

pub struct RuleDetector {
    policy: PrivacyPolicy,
    rules: Vec<Rule>,
}

impl RuleDetector {
    pub fn new(policy: PrivacyPolicy) -> Result<Self> {
        let mut detector = Self {
            policy,
            rules: Vec::new(),
        };
        let builtins = [
            (
                "EMAIL",
                r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}",
                0,
            ),
            ("PHONE", r"(?<!\d)(?:\+?86[- ]?)?1[3-9]\d{9}(?!\d)", 0),
            ("ID", r"(?<!\d)\d{17}[\dXx](?!\d)", 0),
            (
                "SECRET",
                r"(?i)(?<![A-Za-z0-9])(?:sk|pk|api|token|key|secret|bearer)[-_][A-Za-z0-9_-]{8,}",
                0,
            ),
            (
                "SECRET",
                r"(?s)-----BEGIN (?:[A-Z ]+ )?PRIVATE KEY-----.*?-----END (?:[A-Z ]+ )?PRIVATE KEY-----",
                0,
            ),
            (
                "SECRET",
                r#"(?i)(?:[A-Z0-9_]*(?:password|passwd|api[_-]?key|secret|token|credential)|密码)\s*[:=：]\s*["']?([^\s"',;}]+)"#,
                1,
            ),
            (
                "SECRET",
                r#"(?i)authorization\s*[:=：]\s*["']?Bearer\s+([^\s"',;}]+)"#,
                1,
            ),
            (
                "PERSON",
                r"(?:姓名|联系人|患者姓名|员工姓名|客户姓名)\s*[:：=]\s*([\u{3400}-\u{9fff}·]{2,12})",
                1,
            ),
        ];
        for (kind, expression, group) in builtins {
            detector.add(kind, expression, group, None)?;
        }
        let number = r"[+-]?(?:\d{1,3}(?:,\d{3})+|\d+)(?:\.\d+)?";
        if detector.policy.mask_money() {
            for (expression, unit) in [
                (
                    format!(r"(?i)(?:人民币|CNY|RMB|￥|¥)\s*({number})"),
                    Some("CNY"),
                ),
                (format!(r"(?i)(?:USD|\$)\s*({number})"), Some("USD")),
                (format!(r"({number})\s*(?:元|人民币)"), Some("CNY")),
                (
                    format!(
                        r"(?i)(?:工资|薪资|薪酬|奖金|报价|价格|金额|余额|salary|bonus)\s*[:：=]?\s*({number})"
                    ),
                    None,
                ),
            ] {
                detector.add("MONEY", &expression, 1, unit)?;
            }
        }
        if detector.policy.mask_dates() {
            detector.add("DATE", r"(?<!\d)\d{4}(?:[-/]\d{1,2}[-/]\d{1,2}|年\d{1,2}月\d{1,2}日?)(?:[T ]\d{1,2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:Z|[+-]\d{2}:\d{2})?)?(?!\d)", 0, None)?;
            detector.add("DATE", r"(?<!\d)\d{1,2}月\d{1,2}日(?!\d)", 0, None)?;
        }
        if detector.policy.mask_all_numbers() {
            detector.add("NUMBER", r"(?<![A-Za-z0-9_.])[+-]?(?:\d{1,3}(?:,\d{3})+|\d+)(?:\.\d+)?(?:[eE][+-]?\d+)?(?!\d|\.\d)", 0, None)?;
        }
        for (kind, expressions) in detector.policy.patterns().clone() {
            for expression in expressions {
                detector.add_with_budget(&kind, &expression, 0, None, 100_000)?;
            }
        }
        Ok(detector)
    }

    fn add(
        &mut self,
        kind: &str,
        expression: &str,
        group: usize,
        unit: Option<&str>,
    ) -> Result<()> {
        // Fixed built-in lookarounds perform bounded work per input byte; their
        // budget must admit a complete 256 KiB TLS message. User expressions
        // retain a separate, smaller adversarial-backtracking budget.
        self.add_with_budget(kind, expression, group, unit, 8_000_000)
    }
    fn add_with_budget(
        &mut self,
        kind: &str,
        expression: &str,
        group: usize,
        unit: Option<&str>,
        budget: usize,
    ) -> Result<()> {
        // Fancy-regex's backtracking limit is mandatory: an exhausted rule
        // aborts required detection instead of silently returning no matches.
        let expression = fancy_regex::RegexBuilder::new(expression)
            .backtrack_limit(budget)
            .build()
            .map_err(|_| Error::PolicyInvalid)?;
        let metadata = unit
            .map(|unit| BTreeMap::from([("unit".into(), unit.into())]))
            .unwrap_or_default();
        self.rules.push(Rule {
            kind: kind.into(),
            expression,
            group,
            metadata,
        });
        Ok(())
    }
}

impl Detector for RuleDetector {
    fn detect(&self, text: &str) -> Result<Vec<Span>> {
        let tokens: Vec<_> = TOKEN_RE.find_iter(text).map(|m| m.range()).collect();
        let mut spans = Vec::new();
        let mut add = |kind: &str,
                       start: usize,
                       end: usize,
                       metadata: &BTreeMap<String, String>|
         -> Result<()> {
            if start == end {
                return Err(Error::DetectionFailed);
            }
            if tokens
                .iter()
                .any(|token| start < token.end && token.start < end)
            {
                return Ok(());
            }
            if spans.len() >= 65_536 {
                return Err(Error::ResourceLimit);
            }
            spans.push(Span {
                entity_type: kind.into(),
                start,
                end,
                labels: self.policy.labels().clone(),
                metadata: metadata.clone(),
            });
            Ok(())
        };
        for rule in &self.rules {
            for captures in rule.expression.captures_iter(text) {
                let captures = captures.map_err(|_| Error::DetectionFailed)?;
                let matched = captures.get(rule.group).ok_or(Error::DetectionFailed)?;
                add(&rule.kind, matched.start(), matched.end(), &rule.metadata)?;
            }
        }
        for (kind, literals) in self.policy.dictionaries() {
            for literal in literals {
                for (start, _) in text.match_indices(literal) {
                    add(kind, start, start + literal.len(), &BTreeMap::new())?;
                }
            }
        }
        merge_spans(text, spans)
    }
}

pub struct DetectionEngine {
    policy: PrivacyPolicy,
    rules: RuleDetector,
    specs: Vec<DetectorSpec>,
}

impl DetectionEngine {
    pub fn new(policy: PrivacyPolicy) -> Result<Self> {
        Ok(Self {
            rules: RuleDetector::new(policy.clone())?,
            policy,
            specs: Vec::new(),
        })
    }
    pub fn with_detectors(mut self, specs: Vec<DetectorSpec>) -> Self {
        self.specs = specs;
        self
    }
    pub(crate) fn with_policy(&self, policy: PrivacyPolicy) -> Result<Self> {
        Ok(Self::new(policy)?.with_detectors(self.specs.clone()))
    }
    pub fn detect(&self, text: &str, source_sensitive: bool) -> Result<Vec<Span>> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let mut spans = self.rules.detect(text)?;
        for spec in &self.specs {
            let detected = (|| {
                let detected = spec.detector.detect(text)?;
                validate_spans(text, &detected)?;
                Ok::<_, Error>(detected)
            })();
            match detected {
                Ok(detected) => spans.extend(detected),
                _ if spec.required => return Err(Error::DetectionFailed),
                _ => (),
            }
        }
        if source_sensitive {
            spans.push(Span {
                entity_type: "CONFIDENTIAL".into(),
                start: 0,
                end: text.len(),
                labels: self.policy.labels().clone(),
                metadata: BTreeMap::new(),
            });
        }
        merge_spans(text, spans)
    }
}
