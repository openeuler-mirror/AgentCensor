//! Complete-message privacy gate. This module performs no network operations.
use crate::{
    compute::{self, ComputeResult, Operation},
    detection::{DetectionEngine, DetectorSpec, Span},
    error::{Error, Result},
    json,
    policy::PrivacyPolicy,
    protocols::{self, Api, RequestProtector},
    references::{random_hex, Reference, TOKEN_PATTERN, TOKEN_RE},
    vault::{Clock, MonotonicClock, Registration, RunCapability, RunVault},
};
use regex::Regex;
use serde_json::{Number, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

static MONEY_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(salary|bonus|amount|price|balance|money|total|average|mean|工资|薪|金额|奖金|报价)",
    )
    .expect("constant field grammar")
});
static CALL_ID_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^/(?:messages/\d+/(?:id|tool_call_id|tool_calls/\d+/id|content/\d+/(?:id|tool_use_id))|input/\d+/(?:id|call_id))$").expect("constant call grammar")
});

pub struct GateOptions {
    pub sensitive_paths: Vec<String>,
    pub public_identifiers: Vec<String>,
    pub max_body_bytes: usize,
}
impl Default for GateOptions {
    fn default() -> Self {
        Self {
            sensitive_paths: Vec::new(),
            public_identifiers: vec!["configured-model".into()],
            max_body_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Opaque, non-Clone, single-use authority over immutable serialized bytes.
/// Neither a client header nor deserializing JSON can construct this value.
pub struct ReleasePermit {
    identifier: String,
    run_id: String,
    epoch: u64,
    method: String,
    target: String,
    body: Vec<u8>,
    digest: [u8; 32],
    expires_at: Duration,
}
impl ReleasePermit {
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

/// Constructed only after consume has revalidated the entire permit.
pub struct ApprovedRequest {
    target: String,
    body: Vec<u8>,
}
impl ApprovedRequest {
    pub fn target(&self) -> &str {
        &self.target
    }
    pub fn body(&self) -> &[u8] {
        &self.body
    }
    pub fn into_parts(self) -> (String, Vec<u8>) {
        (self.target, self.body)
    }
}

pub struct PrivacyGate {
    policy: PrivacyPolicy,
    detector: DetectionEngine,
    vault: RunVault,
    cap: RunCapability,
    options: GateOptions,
    permits: HashMap<String, Duration>,
    observed_ids: BTreeSet<String>,
    disabled: bool,
    clock: Arc<dyn Clock>,
}
impl PrivacyGate {
    pub fn new(policy: PrivacyPolicy, options: GateOptions) -> Result<Self> {
        Self::with_clock(policy, options, Arc::new(MonotonicClock(Instant::now())))
    }
    pub fn with_detectors(
        policy: PrivacyPolicy,
        options: GateOptions,
        specs: Vec<DetectorSpec>,
    ) -> Result<Self> {
        let mut gate = Self::new(policy, options)?;
        gate.detector = gate.detector.with_detectors(specs);
        Ok(gate)
    }
    pub fn with_clock(
        policy: PrivacyPolicy,
        options: GateOptions,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        if options.max_body_bytes == 0 || options.max_body_bytes > 32 * 1024 * 1024 {
            return Err(Error::ResourceLimit);
        }
        let detector = DetectionEngine::new(policy.clone())?;
        let vault = RunVault::with_clock(policy.ttl(), clock.clone())?;
        let cap = vault.begin_run("local", policy.epoch())?;
        Ok(Self {
            policy,
            detector,
            vault,
            cap,
            options,
            permits: HashMap::new(),
            observed_ids: BTreeSet::new(),
            disabled: false,
            clock,
        })
    }
    pub fn run_id(&self) -> &str {
        self.cap.run_id()
    }
    pub fn epoch(&self) -> u64 {
        self.policy.epoch()
    }
    pub fn model_ids(&self) -> &[String] {
        &self.options.public_identifiers
    }
    pub fn max_body_bytes(&self) -> usize {
        self.options.max_body_bytes
    }
    pub fn validate(&self) -> Result<()> {
        if self.disabled {
            return Err(Error::ServiceUnavailable);
        }
        self.vault.validate_run(&self.cap, self.epoch())
    }
    fn source_sensitive(&self, path: &str) -> bool {
        self.options.sensitive_paths.iter().any(|prefix| {
            path == prefix || path.starts_with(&format!("{}/", prefix.trim_end_matches('/')))
        })
    }
    pub(crate) fn spans(&self, text: &str, path: &str, source_check: bool) -> Result<Vec<Span>> {
        let mut spans = self.detector.detect(text, false)?;
        if source_check && self.source_sensitive(path) && !text.is_empty() {
            // Whole-field protection must not downgrade a detected credential
            // to an ordinary display-restorable CONFIDENTIAL value. Retain the
            // individual secret literals for raw provider-output redaction too.
            for span in spans
                .iter()
                .filter(|span| span.entity_type == "SECRET" || span.labels.contains("secret"))
            {
                self.vault.register_value(
                    &self.cap,
                    self.epoch(),
                    &text[span.start..span.end],
                    Registration::new("SECRET", span.labels.clone()),
                )?;
            }
            spans.push(Span {
                entity_type: "CONFIDENTIAL".into(),
                start: 0,
                end: text.len(),
                labels: self.policy.labels().clone(),
                metadata: Default::default(),
            });
        }
        spans.extend(self.vault.known_spans(&self.cap, self.epoch(), text)?);
        Ok(spans)
    }
    pub fn protect_text(&self, text: &str, path: &str) -> Result<String> {
        self.validate()?;
        self.vault
            .protect_text(&self.cap, self.epoch(), text, self.spans(text, path, true)?)
    }
    pub(crate) fn transport_register(&self, text: &str, span: &Span) -> Result<Reference> {
        self.validate()?;
        let mut registration = Registration::new(&span.entity_type, span.labels.clone());
        registration.metadata.unit = span.metadata.get("unit").cloned();
        registration.metadata.entity_id = span.metadata.get("entity_id").cloned();
        registration.metadata.source = span.metadata.get("source").cloned();
        self.vault.register_value(
            &self.cap,
            self.epoch(),
            &text[span.start..span.end],
            registration,
        )
    }
    pub(crate) fn transport_entry(
        &self,
        reference: &Reference,
    ) -> Result<Option<crate::vault::VaultEntry>> {
        self.validate()?;
        self.vault
            .transport_entry(&self.cap, self.epoch(), reference)
    }
    pub fn register_transport_secret(&self, value: &str) -> Result<()> {
        self.validate()?;
        self.vault.register_value(
            &self.cap,
            self.epoch(),
            value,
            Registration::new("SECRET", BTreeSet::from(["confidential".into()])),
        )?;
        Ok(())
    }
    pub(crate) fn restore_token(&self, text: &str) -> Result<String> {
        self.validate()?;
        self.vault.restore_text(&self.cap, self.epoch(), text)
    }
    pub(crate) fn secret_literals(&self) -> Result<Vec<zeroize::Zeroizing<String>>> {
        self.validate()?;
        self.vault.secret_literals(&self.cap, self.epoch())
    }
    pub fn redact_known_secrets(&self, text: &str) -> Result<String> {
        self.validate()?;
        self.vault
            .redact_known_secrets(&self.cap, self.epoch(), text)
    }
    pub fn protect(&mut self, api: Api, body: &Value, target: &str) -> Result<ReleasePermit> {
        self.validate()?;
        let transformed = protocols::protect_request(api, body, self)?;
        let body = serde_json::to_vec(&transformed).map_err(|_| Error::InvalidJson)?;
        if body.len() > self.options.max_body_bytes {
            return Err(Error::ResourceLimit);
        }
        let final_body = json::from_slice(&body)?;
        protocols::protect_request(api, &final_body, &FinalCheck(self))?;
        let now = self.clock.now();
        self.permits.retain(|_, expiry| now < *expiry);
        if self.permits.len() >= 1024 {
            return Err(Error::ResourceLimit);
        }
        let identifier = random_hex::<16>()?;
        let expires_at = now
            .checked_add(Duration::from_secs(30))
            .ok_or(Error::ReleaseDenied)?;
        self.permits.insert(identifier.clone(), expires_at);
        Ok(ReleasePermit {
            identifier,
            run_id: self.run_id().into(),
            epoch: self.epoch(),
            method: "POST".into(),
            target: target.into(),
            digest: Sha256::digest(&body).into(),
            body,
            expires_at,
        })
    }
    pub fn consume(
        &mut self,
        permit: ReleasePermit,
        target: &str,
        method: &str,
    ) -> Result<ApprovedRequest> {
        self.validate()?;
        let existing = self.permits.remove(&permit.identifier);
        if existing != Some(permit.expires_at)
            || permit.run_id != self.run_id()
            || permit.epoch != self.epoch()
            || self.clock.now() >= permit.expires_at
            || method != permit.method
            || target != permit.target
            || permit.digest != <[u8; 32]>::from(Sha256::digest(&permit.body))
        {
            return Err(Error::ReleaseDenied);
        }
        Ok(ApprovedRequest {
            target: permit.target,
            body: permit.body,
        })
    }
    pub fn compute(&self, payload: &Value) -> Result<ComputeResult> {
        self.validate()?;
        let object = payload.as_object().ok_or(Error::ComputeInvalid)?;
        if object
            .keys()
            .any(|key| !["operation", "refs", "options", "run_id"].contains(&key.as_str()))
            || object
                .get("run_id")
                .is_some_and(|value| value != self.run_id())
        {
            return Err(Error::ComputeInvalid);
        }
        let operation: Operation = serde_json::from_value(
            payload
                .get("operation")
                .cloned()
                .ok_or(Error::ComputeInvalid)?,
        )
        .map_err(|_| Error::ComputeInvalid)?;
        let refs = payload
            .get("refs")
            .and_then(Value::as_array)
            .ok_or(Error::ComputeInvalid)?;
        if refs.is_empty() || refs.len() > 256 {
            return Err(Error::ComputeInvalid);
        }
        let refs = refs
            .iter()
            .map(|value| Reference::parse(value.as_str().ok_or(Error::InvalidReference)?))
            .collect::<Result<Vec<_>>>()?;
        compute::compute(
            &self.vault,
            &self.cap,
            self.epoch(),
            operation,
            &refs,
            payload.get("options").unwrap_or(&serde_json::json!({})),
        )
    }
    pub fn restore(&mut self, api: Api, body: &Value) -> Result<Value> {
        self.validate()?;
        let result = crate::response::restore_response(api, body, self)?;
        self.remember_response_ids(&result)?;
        Ok(result)
    }
    pub fn restore_display(&self, text: &str) -> Result<String> {
        use crate::response::ResponseRestorer;
        self.validate()?;
        let mut result = self.display_group(&[text.into()])?;
        self.redact_known_secrets(&result.pop().ok_or(Error::ProtocolBlocked)?)
    }
    fn validate_refs(&self, refs: &[Value]) -> Result<()> {
        if refs.is_empty() || refs.len() > 256 {
            return Err(Error::InvalidReference);
        }
        let refs = refs
            .iter()
            .map(|reference| Reference::parse(reference.as_str().ok_or(Error::InvalidReference)?))
            .collect::<Result<Vec<_>>>()?;
        self.vault.resolve_compute(&self.cap, self.epoch(), &refs)?;
        Ok(())
    }
    pub(crate) fn remember_response_ids(&mut self, value: &Value) -> Result<()> {
        fn remember(ids: &mut BTreeSet<String>, value: &Value) -> Result<()> {
            if let Some(id) = value.as_str().filter(|id| {
                !id.is_empty()
                    && id.len() <= 128
                    && id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
            }) {
                if ids.len() >= 4096 && !ids.contains(id) {
                    return Err(Error::ResourceLimit);
                }
                ids.insert(id.into());
            }
            Ok(())
        }
        fn item_ids(ids: &mut BTreeSet<String>, item: &Value) -> Result<()> {
            if matches!(
                item.get("type").and_then(Value::as_str),
                Some("tool_use" | "function_call" | "custom_tool_call" | "message")
            ) {
                for key in ["id", "call_id"] {
                    if let Some(value) = item.get(key) {
                        remember(ids, value)?;
                    }
                }
            }
            Ok(())
        }
        item_ids(&mut self.observed_ids, value)?;
        for key in ["content", "output"] {
            if let Some(items) = value.get(key).and_then(Value::as_array) {
                for item in items {
                    item_ids(&mut self.observed_ids, item)?;
                }
            }
        }
        for key in ["item", "content_block"] {
            if let Some(item) = value.get(key) {
                item_ids(&mut self.observed_ids, item)?;
            }
        }
        if let Some(response) = value.get("response").filter(|value| value.is_object()) {
            self.remember_response_ids(response)?;
        }
        if let Some(choices) = value.get("choices").and_then(Value::as_array) {
            for choice in choices {
                for key in ["message", "delta"] {
                    if let Some(calls) = choice
                        .get(key)
                        .and_then(|message| message.get("tool_calls"))
                        .and_then(Value::as_array)
                    {
                        for call in calls {
                            if let Some(id) = call.get("id") {
                                remember(&mut self.observed_ids, id)?;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
    pub fn disable(&mut self) {
        self.disabled = true;
        self.permits.clear();
    }
    pub fn close(&mut self) -> Result<()> {
        self.disable();
        self.vault.end_run(&self.cap)
    }
    pub fn reload(&mut self, policy: PrivacyPolicy) -> Result<()> {
        if policy.epoch() <= self.epoch() {
            return Err(Error::PolicyInvalid);
        }
        let detector = self.detector.with_policy(policy.clone())?;
        detector.detect("local readiness probe", false)?;
        self.vault
            .advance_epoch(&self.cap, self.epoch(), policy.epoch(), policy.ttl())?;
        self.policy = policy;
        self.detector = detector;
        self.permits.clear();
        self.observed_ids.clear();
        self.disabled = false;
        Ok(())
    }
}

impl crate::response::ResponseRestorer for PrivacyGate {
    fn display_group(&self, values: &[String]) -> Result<Vec<String>> {
        self.validate()?;
        let mut renderer = crate::display::DisplayRestorer::new();
        let mut output = Vec::new();
        let mut boundary_pending = false;
        for (index, value) in values.iter().enumerate() {
            output.push(renderer.feed(value, |reference| self.restore_token(reference))?);
            if index + 1 < values.len() && renderer.has_pending() {
                boundary_pending = true;
            }
        }
        // Do not move restored bytes across structured part boundaries, where
        // the provider may attach annotations or offsets to a particular part.
        if boundary_pending {
            return Ok(values.to_vec());
        }
        if let Some(last) = output.last_mut() {
            last.push_str(&renderer.finish()?);
        }
        Ok(output)
    }
    fn tool_arguments(&self, name: &str, arguments: &Value) -> Result<Value> {
        self.validate()?;
        let encoded = serde_json::to_string(arguments).map_err(|_| Error::ProtocolBlocked)?;
        if !TOKEN_RE.is_match(&encoded) {
            return Ok(arguments.clone());
        }
        if matches!(name, "private_compute" | "privcalc") {
            let object = arguments.as_object().ok_or(Error::ProtocolBlocked)?;
            if object
                .keys()
                .any(|key| !["operation", "refs", "options"].contains(&key.as_str()))
            {
                return Err(Error::ProtocolBlocked);
            }
            self.validate_refs(
                arguments
                    .get("refs")
                    .and_then(Value::as_array)
                    .ok_or(Error::ProtocolBlocked)?,
            )?;
            return Ok(arguments.clone());
        }
        if matches!(name, "shell" | "terminal" | "bash") && arguments.is_object() {
            let command = arguments
                .get("command")
                .and_then(Value::as_str)
                .ok_or(Error::ProtocolBlocked)?;
            let without_refs = TOKEN_RE.replace_all(command, "REF");
            if without_refs.chars().any(|c| "\n\r;&|$`<>".contains(c)) {
                return Err(Error::ProtocolBlocked);
            }
            let argv = shlex::split(command).ok_or(Error::ProtocolBlocked)?;
            if argv.len() < 3
                || argv[0] != "privcalc"
                || !["sum", "mean", "min", "max", "aggregate", "date_diff"]
                    .contains(&argv[1].as_str())
            {
                return Err(Error::ProtocolBlocked);
            }
            let refs: Vec<_> = argv[2..].iter().cloned().map(Value::String).collect();
            self.validate_refs(&refs)?;
            let mut approved = arguments.clone();
            let quoted: Vec<_> = argv[2..]
                .iter()
                .map(|reference| format!("'{reference}'"))
                .collect();
            approved["command"] = format!("privcalc {} {}", argv[1], quoted.join(" ")).into();
            return Ok(approved);
        }
        Err(Error::ProtocolBlocked)
    }
    fn redact_json(&self, value: &Value) -> Result<Value> {
        Ok(match value {
            Value::String(text) => self.redact_known_secrets(text)?.into(),
            Value::Array(values) => values
                .iter()
                .map(|value| self.redact_json(value))
                .collect::<Result<Vec<_>>>()?
                .into(),
            Value::Object(object) => {
                let mut output = serde_json::Map::new();
                for (key, value) in object {
                    let safe_key = self.redact_known_secrets(key)?;
                    if output.contains_key(&safe_key) {
                        return Err(Error::ProtocolBlocked);
                    }
                    let safe_value = if key == "arguments" {
                        if let Some(text) = value.as_str() {
                            match json::from_slice(text.as_bytes()) {
                                Ok(inner) => serde_json::to_string(&self.redact_json(&inner)?)
                                    .map_err(|_| Error::ProtocolBlocked)?
                                    .into(),
                                Err(_) => self.redact_known_secrets(text)?.into(),
                            }
                        } else {
                            self.redact_json(value)?
                        }
                    } else {
                        self.redact_json(value)?
                    };
                    output.insert(safe_key, safe_value);
                }
                output.into()
            }
            _ => value.clone(),
        })
    }
}

impl RequestProtector for PrivacyGate {
    fn text(&self, text: &str, path: &str) -> Result<String> {
        self.protect_text(text, path)
    }
    fn structural(&self, text: &str, path: &str) -> Result<()> {
        let mut spans = self.spans(text, path, true)?;
        let leaf = path.rsplit('/').next().unwrap_or("");
        let schema = path.split('/').any(|part| {
            matches!(
                part,
                "input_schema" | "parameters" | "schema" | "json_schema"
            )
        });
        let exempt = (leaf == "pattern"
            && (text == TOKEN_PATTERN || text == format!("^{TOKEN_PATTERN}$")))
            || ([
                "minItems",
                "maxItems",
                "minLength",
                "maxLength",
                "minProperties",
                "maxProperties",
            ]
            .contains(&leaf)
                && schema
                && text.parse::<u64>().is_ok_and(|n| n <= 1_000_000))
            || (path == "/model"
                && self
                    .options
                    .public_identifiers
                    .iter()
                    .any(|model| model == text))
            || (self.observed_ids.contains(text) && CALL_ID_PATH.is_match(path));
        if exempt {
            spans.retain(|span| {
                !matches!(
                    span.entity_type.as_str(),
                    "NUMBER" | "MONEY" | "DATE" | "DURATION"
                )
            });
        }
        if !spans.is_empty() {
            return Err(Error::SensitiveStructure);
        }
        Ok(())
    }
    fn number(&self, value: &Number, path: &str) -> Result<()> {
        if self.policy.mask_all_numbers()
            || (self.policy.mask_money() && MONEY_FIELD.is_match(path))
            || !self.spans(&value.to_string(), path, true)?.is_empty()
            || self.source_sensitive(path)
        {
            return Err(Error::PrivateNumberRequiresCompute);
        }
        Ok(())
    }
}
struct FinalCheck<'a>(&'a PrivacyGate);
impl RequestProtector for FinalCheck<'_> {
    fn text(&self, text: &str, path: &str) -> Result<String> {
        if !self.0.spans(text, path, false)?.is_empty() {
            return Err(Error::DetectionFailed);
        }
        Ok(text.into())
    }
    fn structural(&self, text: &str, path: &str) -> Result<()> {
        self.0.structural(text, path)
    }
    fn number(&self, number: &Number, path: &str) -> Result<()> {
        self.0.number(number, path)
    }
}
