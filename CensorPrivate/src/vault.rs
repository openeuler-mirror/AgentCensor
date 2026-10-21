use crate::{
    detection::{merge_spans, Span},
    error::{Error, Result},
    policy::validate_entity_type,
    references::{random_hex, Reference, TOKEN_RE},
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

fn literal_matches<'a>(text: &'a str, value: &'a str) -> impl Iterator<Item = (usize, usize)> + 'a {
    let mut cursor = 0;
    std::iter::from_fn(move || {
        let start = cursor + text.get(cursor..)?.find(value)?;
        let end = start + value.len();
        cursor = start + text[start..].chars().next()?.len_utf8();
        Some((start, end))
    })
}

pub trait Clock: Send + Sync {
    fn now(&self) -> Duration;
}

pub(crate) struct MonotonicClock(pub Instant);
impl Clock for MonotonicClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

/// Only begin_run creates this capability. A client-provided run ID is not one.
/// No Debug, Serialize or Deserialize implementation can disclose or forge it.
#[derive(Clone)]
pub struct RunCapability {
    vault_id: String,
    run_id: String,
    principal_id: String,
}
impl RunCapability {
    pub fn run_id(&self) -> &str {
        &self.run_id
    }
}

#[derive(Clone, Default)]
pub struct ValueMetadata {
    pub unit: Option<String>,
    pub entity_id: Option<String>,
    pub source: Option<String>,
}

impl ValueMetadata {
    pub(crate) fn from_span(span: &Span) -> Self {
        Self {
            unit: span.metadata.get("unit").cloned(),
            entity_id: span.metadata.get("entity_id").cloned(),
            source: span.metadata.get("source").cloned(),
        }
    }
    fn span_metadata(&self) -> BTreeMap<String, String> {
        [
            ("unit", &self.unit),
            ("entity_id", &self.entity_id),
            ("source", &self.source),
        ]
        .into_iter()
        .filter_map(|(key, value)| value.as_ref().map(|value| (key.into(), value.clone())))
        .collect()
    }
}

pub struct Registration {
    pub entity_type: String,
    pub labels: BTreeSet<String>,
    pub metadata: ValueMetadata,
    pub depends_on: Vec<Reference>,
    pub ttl: Option<Duration>,
}

impl Registration {
    pub fn new(entity_type: &str, labels: BTreeSet<String>) -> Self {
        Self {
            entity_type: entity_type.into(),
            labels,
            metadata: ValueMetadata::default(),
            depends_on: Vec::new(),
            ttl: None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct VaultEntry {
    pub value: Zeroizing<String>,
    pub entity_type: String,
    pub labels: BTreeSet<String>,
    epoch: u64,
    expires_at: Duration,
    pub metadata: ValueMetadata,
    depends_on: Vec<Reference>,
}

impl VaultEntry {
    pub(crate) fn is_secret(&self) -> bool {
        self.entity_type == "SECRET" || self.labels.contains("secret")
    }
}

struct Run {
    principal_id: String,
    epoch: u64,
    expires_at: Duration,
    entries: HashMap<Reference, VaultEntry>,
    // The value index contains hashes, not extra unprotected copies of plaintext.
    index: HashMap<[u8; 32], Reference>,
    value_bytes: usize,
}

pub struct RunVault {
    identity: String,
    ttl: Duration,
    clock: Arc<dyn Clock>,
    runs: Mutex<HashMap<String, Run>>,
}

impl RunVault {
    pub fn new(ttl: Duration) -> Result<Self> {
        Self::with_clock(ttl, Arc::new(MonotonicClock(Instant::now())))
    }
    pub fn with_clock(ttl: Duration, clock: Arc<dyn Clock>) -> Result<Self> {
        if ttl.is_zero() {
            return Err(Error::VaultDenied);
        }
        Ok(Self {
            identity: random_hex::<32>()?,
            ttl,
            clock,
            runs: Mutex::new(HashMap::new()),
        })
    }
    fn lock(&self) -> Result<MutexGuard<'_, HashMap<String, Run>>> {
        self.runs.lock().map_err(|_| Error::ServiceUnavailable)
    }
    fn run<'a>(
        &self,
        runs: &'a HashMap<String, Run>,
        cap: &RunCapability,
        epoch: u64,
    ) -> Result<&'a Run> {
        if cap.vault_id != self.identity {
            return Err(Error::VaultDenied);
        }
        let run = runs.get(&cap.run_id).ok_or(Error::VaultDenied)?;
        if run.principal_id != cap.principal_id
            || epoch != run.epoch
            || self.clock.now() >= run.expires_at
        {
            return Err(Error::VaultDenied);
        }
        Ok(run)
    }
    fn run_mut<'a>(
        &self,
        runs: &'a mut HashMap<String, Run>,
        cap: &RunCapability,
        epoch: u64,
    ) -> Result<&'a mut Run> {
        self.run(runs, cap, epoch)?;
        runs.get_mut(&cap.run_id).ok_or(Error::VaultDenied)
    }
    fn entry<'a>(&self, run: &'a Run, reference: &Reference) -> Result<&'a VaultEntry> {
        let entry = run.entries.get(reference).ok_or(Error::VaultDenied)?;
        if entry.epoch != run.epoch || self.clock.now() >= entry.expires_at {
            return Err(Error::VaultDenied);
        }
        Ok(entry)
    }

    pub fn begin_run(&self, principal: &str, epoch: u64) -> Result<RunCapability> {
        if principal.is_empty() || epoch == 0 {
            return Err(Error::VaultDenied);
        }
        let expires_at = self
            .clock
            .now()
            .checked_add(self.ttl)
            .ok_or(Error::VaultDenied)?;
        let mut runs = self.lock()?;
        if runs.len() >= 1024 {
            return Err(Error::ResourceLimit);
        }
        let mut run_id = random_hex::<32>()?;
        while runs.contains_key(&run_id) {
            run_id = random_hex::<32>()?;
        }
        runs.insert(
            run_id.clone(),
            Run {
                principal_id: principal.into(),
                epoch,
                expires_at,
                entries: HashMap::new(),
                index: HashMap::new(),
                value_bytes: 0,
            },
        );
        Ok(RunCapability {
            vault_id: self.identity.clone(),
            run_id,
            principal_id: principal.into(),
        })
    }
    pub fn validate_run(&self, cap: &RunCapability, epoch: u64) -> Result<()> {
        let runs = self.lock()?;
        self.run(&runs, cap, epoch).map(|_| ())
    }
    pub fn advance_epoch(
        &self,
        cap: &RunCapability,
        old_epoch: u64,
        new_epoch: u64,
        ttl: Duration,
    ) -> Result<()> {
        if new_epoch <= old_epoch || ttl.is_zero() {
            return Err(Error::VaultDenied);
        }
        let expiry = self
            .clock
            .now()
            .checked_add(ttl)
            .ok_or(Error::VaultDenied)?;
        let mut runs = self.lock()?;
        let run = self.run_mut(&mut runs, cap, old_epoch)?;
        run.epoch = new_epoch;
        run.expires_at = run.expires_at.min(expiry);
        Ok(())
    }
    pub fn end_run(&self, cap: &RunCapability) -> Result<()> {
        let mut runs = self.lock()?;
        if cap.vault_id != self.identity
            || runs
                .get(&cap.run_id)
                .is_none_or(|run| run.principal_id != cap.principal_id)
        {
            return Err(Error::VaultDenied);
        }
        runs.remove(&cap.run_id);
        Ok(())
    }
    pub fn cleanup_expired(&self) -> Result<usize> {
        let mut runs = self.lock()?;
        let before = runs.len();
        let now = self.clock.now();
        runs.retain(|_, run| now < run.expires_at);
        Ok(before - runs.len())
    }

    pub fn register_value(
        &self,
        cap: &RunCapability,
        epoch: u64,
        value: &str,
        registration: Registration,
    ) -> Result<Reference> {
        let mut runs = self.lock()?;
        let run = self.run_mut(&mut runs, cap, epoch)?;
        self.register(run, value, registration)
    }
    fn register(
        &self,
        run: &mut Run,
        value: &str,
        mut registration: Registration,
    ) -> Result<Reference> {
        validate_entity_type(&registration.entity_type).map_err(|_| Error::VaultDenied)?;
        if value.is_empty()
            || registration.labels.iter().any(String::is_empty)
            || (registration.entity_type != "LITERAL" && registration.labels.is_empty())
            || registration.depends_on.len() > 256
        {
            return Err(Error::VaultDenied);
        }
        if registration.entity_type == "SECRET" {
            registration.labels.insert("secret".into());
        }
        let mut expires_at = run.expires_at;
        if let Some(ttl) = registration.ttl {
            if ttl.is_zero() {
                return Err(Error::VaultDenied);
            }
            expires_at = expires_at.min(
                self.clock
                    .now()
                    .checked_add(ttl)
                    .ok_or(Error::VaultDenied)?,
            );
        }
        for dependency in &registration.depends_on {
            let parent = self.entry(run, dependency)?;
            registration.labels.extend(parent.labels.iter().cloned());
            expires_at = expires_at.min(parent.expires_at);
        }
        let identity = serde_json::to_vec(&(
            run.epoch,
            &registration.entity_type,
            value,
            &registration.metadata.unit,
            &registration.metadata.entity_id,
        ))
        .map(Zeroizing::new)
        .map_err(|_| Error::VaultDenied)?;
        let index: [u8; 32] = Sha256::digest(&*identity).into();
        if let Some(reference) = run.index.get(&index) {
            let entry = run.entries.get_mut(reference).ok_or(Error::VaultDenied)?;
            if self.clock.now() < entry.expires_at {
                entry.labels.extend(registration.labels);
                entry.expires_at = entry.expires_at.min(expires_at);
                for dependency in registration.depends_on {
                    if !entry.depends_on.contains(&dependency) {
                        entry.depends_on.push(dependency);
                    }
                }
                return Ok(reference.clone());
            }
        }
        if run.entries.len() >= 65_536
            || run
                .value_bytes
                .checked_add(value.len())
                .is_none_or(|size| size > 32 * 1024 * 1024)
        {
            return Err(Error::ResourceLimit);
        }
        let mut reference = Reference::issue(&registration.entity_type)?;
        while run.entries.contains_key(&reference) {
            reference = Reference::issue(&registration.entity_type)?;
        }
        let entry = VaultEntry {
            value: Zeroizing::new(value.into()),
            entity_type: registration.entity_type,
            labels: registration.labels,
            epoch: run.epoch,
            expires_at,
            metadata: registration.metadata,
            depends_on: registration.depends_on,
        };
        run.value_bytes += value.len();
        run.entries.insert(reference.clone(), entry);
        run.index.insert(index, reference.clone());
        Ok(reference)
    }

    /// Numeric inputs remain inside the trusted crate. No raw-value HTTP/MCP API.
    pub(crate) fn transport_entry(
        &self,
        cap: &RunCapability,
        epoch: u64,
        reference: &Reference,
    ) -> Result<Option<VaultEntry>> {
        let runs = self.lock()?;
        let run = self.run(&runs, cap, epoch)?;
        if !run.entries.contains_key(reference) {
            return Ok(None);
        }
        Ok(Some(self.entry(run, reference)?.clone()))
    }

    pub(crate) fn resolve_compute(
        &self,
        cap: &RunCapability,
        epoch: u64,
        refs: &[Reference],
    ) -> Result<Vec<VaultEntry>> {
        let runs = self.lock()?;
        let run = self.run(&runs, cap, epoch)?;
        refs.iter()
            .map(|reference| {
                let entry = self.entry(run, reference)?;
                if entry.is_secret() {
                    return Err(Error::VaultDenied);
                }
                Ok(entry.clone())
            })
            .collect()
    }

    fn known(&self, run: &Run, text: &str) -> Result<Vec<Span>> {
        let tokens: Vec<_> = TOKEN_RE.find_iter(text).map(|m| m.range()).collect();
        let mut spans = Vec::new();
        for entry in run.entries.values() {
            // Expiry/revocation removes authority, not sensitivity. Keep values
            // available for detection until the whole run is destroyed.
            if entry.entity_type == "LITERAL" {
                continue;
            }
            for (start, end) in literal_matches(text, entry.value.as_str()) {
                if tokens
                    .iter()
                    .any(|token| start < token.end && token.start < end)
                {
                    continue;
                }
                if matches!(entry.entity_type.as_str(), "MONEY" | "NUMBER" | "DURATION") {
                    let bytes = text.as_bytes();
                    if start > 0
                        && (bytes[start - 1].is_ascii_alphanumeric()
                            || b"_.".contains(&bytes[start - 1]))
                    {
                        continue;
                    }
                    if end < bytes.len()
                        && (bytes[end].is_ascii_digit()
                            || (bytes[end] == b'.'
                                && bytes.get(end + 1).is_some_and(u8::is_ascii_digit)))
                    {
                        continue;
                    }
                }
                if spans.len() >= 65_536 {
                    return Err(Error::ResourceLimit);
                }
                spans.push(Span {
                    entity_type: entry.entity_type.clone(),
                    start,
                    end,
                    labels: entry.labels.clone(),
                    metadata: entry.metadata.span_metadata(),
                });
            }
        }
        merge_spans(text, spans)
    }
    pub fn known_spans(&self, cap: &RunCapability, epoch: u64, text: &str) -> Result<Vec<Span>> {
        let runs = self.lock()?;
        self.known(self.run(&runs, cap, epoch)?, text)
    }

    pub fn protect_text(
        &self,
        cap: &RunCapability,
        epoch: u64,
        text: &str,
        mut spans: Vec<Span>,
    ) -> Result<String> {
        let mut runs = self.lock()?;
        let run = self.run_mut(&mut runs, cap, epoch)?;
        spans.extend(self.known(run, text)?);
        for matched in TOKEN_RE.find_iter(text) {
            let reference = Reference::parse(matched.as_str())?;
            if run.entries.contains_key(&reference) {
                self.entry(run, &reference)?;
            } else {
                spans.push(Span {
                    entity_type: "LITERAL".into(),
                    start: matched.start(),
                    end: matched.end(),
                    labels: BTreeSet::from(["literal".into()]),
                    metadata: BTreeMap::new(),
                });
            }
        }
        let spans = merge_spans(text, spans)?;
        let mut output = String::new();
        let mut offset = 0;
        for span in spans {
            output.push_str(&text[offset..span.start]);
            let registration = Registration {
                entity_type: span.entity_type.clone(),
                labels: span.labels.clone(),
                metadata: ValueMetadata::from_span(&span),
                depends_on: Vec::new(),
                ttl: None,
            };
            let reference = self.register(run, &text[span.start..span.end], registration)?;
            output.push_str(reference.as_str());
            offset = span.end;
        }
        output.push_str(&text[offset..]);
        Ok(output)
    }

    pub fn restore_text(&self, cap: &RunCapability, epoch: u64, text: &str) -> Result<String> {
        let runs = self.lock()?;
        let run = self.run(&runs, cap, epoch)?;
        let mut output = Zeroizing::new(String::new());
        let mut offset = 0;
        for matched in TOKEN_RE.find_iter(text) {
            output.push_str(&text[offset..matched.start()]);
            let entry = self.entry(run, &Reference::parse(matched.as_str())?)?;
            output.push_str(if entry.is_secret() {
                "[REDACTED_SECRET]"
            } else {
                entry.value.as_str()
            });
            offset = matched.end();
        }
        output.push_str(&text[offset..]);
        // Single-pass restoration: a shielded foreign CP token stays literal.
        self.redact(run, &output)
    }

    fn redact(&self, run: &Run, text: &str) -> Result<String> {
        let tokens: Vec<_> = TOKEN_RE.find_iter(text).map(|m| m.range()).collect();
        let mut ranges = Vec::<(usize, usize)>::new();
        for entry in run.entries.values().filter(|entry| entry.is_secret()) {
            for (start, end) in literal_matches(text, entry.value.as_str()) {
                if !tokens
                    .iter()
                    .any(|token| start < token.end && token.start < end)
                {
                    ranges.push((start, end));
                    if ranges.len() > 65_536 {
                        return Err(Error::ResourceLimit);
                    }
                }
            }
        }
        ranges.sort_unstable();
        let mut merged = Vec::<(usize, usize)>::new();
        for (start, end) in ranges {
            if let Some(previous) = merged.last_mut().filter(|previous| start < previous.1) {
                previous.1 = previous.1.max(end);
            } else {
                merged.push((start, end));
            }
        }
        let mut output = String::new();
        let mut offset = 0;
        for (start, end) in merged {
            output.push_str(&text[offset..start]);
            output.push_str("[REDACTED_SECRET]");
            offset = end;
        }
        output.push_str(&text[offset..]);
        Ok(output)
    }
    pub fn redact_known_secrets(
        &self,
        cap: &RunCapability,
        epoch: u64,
        text: &str,
    ) -> Result<String> {
        let runs = self.lock()?;
        self.redact(self.run(&runs, cap, epoch)?, text)
    }
    pub(crate) fn secret_literals(
        &self,
        cap: &RunCapability,
        epoch: u64,
    ) -> Result<Vec<Zeroizing<String>>> {
        let runs = self.lock()?;
        let run = self.run(&runs, cap, epoch)?;
        let mut literals = Vec::new();
        let mut bytes = 0usize;
        for entry in run.entries.values().filter(|entry| entry.is_secret()) {
            if literals
                .iter()
                .any(|value: &Zeroizing<String>| value.as_str() == entry.value.as_str())
            {
                continue;
            }
            bytes += entry.value.len();
            if literals.len() >= 256 || bytes > 65_536 {
                return Err(Error::ResourceLimit);
            }
            literals.push(entry.value.clone());
        }
        Ok(literals)
    }
}
