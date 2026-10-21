//! Exact wire ranges for complete JSON messages. The carrier owns withholding.
use crate::{
    detection::{merge_spans, Span},
    display::DisplayRestorer,
    error::{Error, Result},
    gate::PrivacyGate,
    json,
    protocols::{self, Api, RequestProtector},
    references::{random_hex, Reference, TOKEN_RE},
    response::{self, ResponseRestorer},
};
use serde_json::{Number, Value};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};
use zeroize::Zeroizing;

struct Node {
    path: String,
    value: Zeroizing<String>,
    bytes: Vec<Range<usize>>,
    key: bool,
}
impl Node {
    fn bounds(&self, range: Range<usize>) -> Result<Range<usize>> {
        if range.start >= range.end
            || range.end > self.bytes.len()
            || !self.value.is_char_boundary(range.start)
            || !self.value.is_char_boundary(range.end)
        {
            return Err(Error::TlsBlocked);
        }
        Ok(self.bytes[range.start].start..self.bytes[range.end - 1].end)
    }
}
struct Lexer<'a> {
    raw: &'a [u8],
    index: usize,
    nodes: Vec<Node>,
}
impl<'a> Lexer<'a> {
    fn nodes(raw: &'a [u8], path: &str) -> Result<Vec<Node>> {
        json::from_slice(raw)?;
        let mut lexer = Self {
            raw,
            index: 0,
            nodes: Vec::new(),
        };
        lexer.value(path, 0)?;
        lexer.space();
        if lexer.index != raw.len() {
            return Err(Error::TlsBlocked);
        }
        Ok(lexer.nodes)
    }
    fn space(&mut self) {
        while self
            .raw
            .get(self.index)
            .is_some_and(|b| b" \r\n\t".contains(b))
        {
            self.index += 1;
        }
    }
    fn byte(&self) -> Result<u8> {
        self.raw.get(self.index).copied().ok_or(Error::TlsBlocked)
    }
    fn string(&mut self, path: String, key: bool) -> Result<String> {
        if self.byte()? != b'"' || self.nodes.len() >= 16_384 {
            return Err(Error::TlsBlocked);
        }
        self.index += 1;
        let mut value = Zeroizing::new(String::new());
        let mut bytes = Vec::new();
        while self.byte()? != b'"' {
            let start = self.index;
            let text = if self.byte()? == b'\\' {
                let width = if self.raw.get(start + 1) == Some(&b'u') {
                    let digits = self
                        .raw
                        .get(start + 2..start + 6)
                        .ok_or(Error::TlsBlocked)?;
                    let high = u16::from_str_radix(
                        std::str::from_utf8(digits).map_err(|_| Error::TlsBlocked)?,
                        16,
                    )
                    .map_err(|_| Error::TlsBlocked)?;
                    if (0xd800..=0xdbff).contains(&high) {
                        12
                    } else {
                        6
                    }
                } else {
                    2
                };
                self.index += width;
                let mut quoted = Vec::with_capacity(width + 2);
                quoted.push(b'"');
                quoted.extend_from_slice(self.raw.get(start..self.index).ok_or(Error::TlsBlocked)?);
                quoted.push(b'"');
                json::from_slice(&quoted)?
                    .as_str()
                    .ok_or(Error::TlsBlocked)?
                    .to_owned()
            } else {
                let first = self.byte()?;
                let width = if first < 128 {
                    1
                } else if first < 224 {
                    2
                } else if first < 240 {
                    3
                } else {
                    4
                };
                self.index += width;
                std::str::from_utf8(self.raw.get(start..self.index).ok_or(Error::TlsBlocked)?)
                    .map_err(|_| Error::TlsBlocked)?
                    .to_owned()
            };
            if text.chars().count() != 1 {
                return Err(Error::TlsBlocked);
            }
            bytes.extend(std::iter::repeat(start..self.index).take(text.len()));
            value.push_str(&text);
        }
        self.index += 1;
        let decoded = value.to_string();
        self.nodes.push(Node {
            path,
            value,
            bytes,
            key,
        });
        Ok(decoded)
    }
    fn value(&mut self, path: &str, depth: usize) -> Result<()> {
        if depth > 64 {
            return Err(Error::TlsBlocked);
        }
        self.space();
        match self.byte()? {
            b'"' => {
                self.string(path.into(), false)?;
            }
            b'{' => {
                self.index += 1;
                self.space();
                while self.byte()? != b'}' {
                    let key = self.string(protocols::pointer(path, "<key>")?, true)?;
                    self.space();
                    if self.byte()? != b':' {
                        return Err(Error::TlsBlocked);
                    }
                    self.index += 1;
                    self.value(&protocols::pointer(path, &key)?, depth + 1)?;
                    self.space();
                    if self.byte()? == b'}' {
                        break;
                    }
                    if self.byte()? != b',' {
                        return Err(Error::TlsBlocked);
                    }
                    self.index += 1;
                    self.space();
                }
                self.index += 1;
            }
            b'[' => {
                self.index += 1;
                self.space();
                let mut index = 0;
                while self.byte()? != b']' {
                    self.value(&protocols::pointer(path, &index.to_string())?, depth + 1)?;
                    index += 1;
                    self.space();
                    if self.byte()? == b']' {
                        break;
                    }
                    if self.byte()? != b',' {
                        return Err(Error::TlsBlocked);
                    }
                    self.index += 1;
                }
                self.index += 1;
            }
            _ => {
                while self
                    .raw
                    .get(self.index)
                    .is_some_and(|b| !b" ,]}\r\n\t".contains(b))
                {
                    self.index += 1;
                }
            }
        }
        Ok(())
    }
}
fn inner_nodes(node: &Node) -> Result<Vec<Node>> {
    let mut nodes = Lexer::nodes(node.value.as_bytes(), &node.path)?;
    for inner in &mut nodes {
        for range in &mut inner.bytes {
            *range = node.bytes.get(range.start).ok_or(Error::TlsBlocked)?.start
                ..node.bytes.get(range.end - 1).ok_or(Error::TlsBlocked)?.end;
        }
    }
    Ok(nodes)
}
struct Alias {
    reference: Reference,
    original: Zeroizing<Vec<u8>>,
    reversible: bool,
}
pub struct LengthPreservingTransformer {
    run_id: String,
    epoch: u64,
    aliases: BTreeMap<String, Alias>,
    index: BTreeMap<(Reference, [u8; 32]), String>,
    ordinary: Vec<Zeroizing<String>>,
    ordinary_bytes: usize,
    max_aliases: usize,
}
impl LengthPreservingTransformer {
    pub fn new(gate: &PrivacyGate) -> Result<Self> {
        gate.validate()?;
        Ok(Self {
            run_id: gate.run_id().into(),
            epoch: gate.epoch(),
            aliases: BTreeMap::new(),
            index: BTreeMap::new(),
            ordinary: Vec::new(),
            ordinary_bytes: 0,
            max_aliases: 4096,
        })
    }
    fn guard(&self, gate: &PrivacyGate) -> Result<()> {
        gate.validate()?;
        if gate.run_id() != self.run_id || gate.epoch() != self.epoch {
            return Err(Error::TlsBlocked);
        }
        Ok(())
    }
    fn parse(&self, gate: &PrivacyGate, payload: &[u8]) -> Result<(Value, Vec<Node>)> {
        self.guard(gate)?;
        if payload.is_empty() || payload.len() > gate.max_body_bytes().min(256 * 1024) {
            return Err(Error::ResourceLimit);
        }
        let body = json::from_slice(payload)?;
        if !body.is_object() {
            return Err(Error::TlsBlocked);
        }
        Ok((body, Lexer::nodes(payload, "")?))
    }
    fn reserve(&mut self, nodes: &[Node]) -> Result<()> {
        for node in nodes {
            if self.ordinary_bytes + node.value.len() > 32 * 1024 * 1024
                || self.ordinary.len() >= 262_144
            {
                return Err(Error::ResourceLimit);
            }
            self.ordinary_bytes += node.value.len();
            self.ordinary.push(node.value.clone());
        }
        Ok(())
    }
    fn matches(&self, text: &str) -> Result<Vec<(Range<usize>, String)>> {
        let mut found = Vec::new();
        for alias in self.aliases.keys() {
            for (start, _) in text.match_indices(alias) {
                let end = start + alias.len();
                let bytes = text.as_bytes();
                let word = |b: &u8| b.is_ascii_alphanumeric() || *b == b'_';
                if start > 0 && word(&bytes[start - 1]) || end < bytes.len() && word(&bytes[end]) {
                    continue;
                }
                if found.len() >= 16_384 {
                    return Err(Error::ResourceLimit);
                }
                found.push((start..end, alias.clone()));
            }
        }
        found.sort_by_key(|(range, _)| range.start);
        if found.windows(2).any(|pair| pair[0].0.end > pair[1].0.start) {
            return Err(Error::TlsBlocked);
        }
        Ok(found)
    }
    fn entry(&self, gate: &PrivacyGate, alias: &Alias) -> Result<crate::vault::VaultEntry> {
        gate.transport_entry(&alias.reference)?
            .ok_or(Error::VaultDenied)
    }
    fn reversible(&self, gate: &PrivacyGate, alias: &Alias) -> Result<bool> {
        let entry = self.entry(gate, alias)?;
        Ok(alias.reversible
            && !entry.is_secret()
            && !gate
                .secret_literals()?
                .iter()
                .any(|secret| entry.value.contains(secret.as_str())))
    }
    pub fn resolve_alias(&self, gate: &PrivacyGate, alias: &str) -> Result<Reference> {
        self.guard(gate)?;
        let record = self.aliases.get(alias).ok_or(Error::VaultDenied)?;
        if !self.reversible(gate, record)? {
            return Err(Error::VaultDenied);
        }
        Ok(record.reference.clone())
    }
    fn new_alias(
        &mut self,
        gate: &PrivacyGate,
        reference: Reference,
        original: &[u8],
        reversible: bool,
    ) -> Result<String> {
        let identity = (reference.clone(), Sha256::digest(original).into());
        if let Some(old) = self.index.get(&identity) {
            self.entry(gate, &self.aliases[old])?;
            return Ok(old.clone());
        }
        if self.aliases.len() >= self.max_aliases || original.len() < 3 {
            return Err(Error::ResourceLimit);
        }
        let prefix = if original.len() >= 12 {
            "~P"
        } else if original.len() >= 6 {
            "~"
        } else {
            ""
        };
        for _ in 0..128 {
            let mut alias = String::from(prefix);
            while alias.len() < original.len() {
                let mut bytes = [0u8; 64];
                getrandom::getrandom(&mut bytes).map_err(|_| Error::ServiceUnavailable)?;
                for byte in bytes {
                    if byte < 234 && alias.len() < original.len() {
                        alias.push((b'A' + byte % 26) as char);
                    }
                }
            }
            if self.aliases.contains_key(&alias)
                || self
                    .aliases
                    .keys()
                    .any(|old| alias.contains(old) || old.contains(&alias))
                || self.ordinary.iter().any(|text| text.contains(&alias))
                || !gate.spans(&alias, "", false)?.is_empty()
            {
                continue;
            }
            self.aliases.insert(
                alias.clone(),
                Alias {
                    reference,
                    original: Zeroizing::new(original.into()),
                    reversible,
                },
            );
            self.index.insert(identity, alias.clone());
            return Ok(alias);
        }
        Err(Error::TlsBlocked)
    }
    pub fn protect_json(
        &mut self,
        gate: &PrivacyGate,
        api: Api,
        payload: &[u8],
    ) -> Result<Vec<u8>> {
        let (body, mut nodes) = self.parse(gate, payload)?;
        let observer = Observer {
            gate,
            allowed: RefCell::new(BTreeMap::new()),
            residual: None,
        };
        protocols::protect_request(api, &body, &observer)?;
        let allowed = observer.allowed.into_inner();
        let mut extras = Vec::new();
        for node in &nodes {
            if !node.key
                && node.path.ends_with("/arguments")
                && !allowed.contains_key(&node.path)
                && allowed
                    .keys()
                    .any(|path| path.starts_with(&(node.path.clone() + "/")))
            {
                extras.extend(inner_nodes(node)?);
            }
        }
        nodes.extend(extras);
        self.reserve(&nodes)?;
        let mut replacements = Vec::new();
        for node in &nodes {
            if node.key || allowed.get(&node.path).map(String::as_str) != Some(node.value.as_str())
            {
                continue;
            }
            let owned = self.matches(&node.value)?;
            for (_, alias) in &owned {
                self.entry(gate, &self.aliases[alias])?;
            }
            let mut spans = gate.spans(&node.value, &node.path, true)?;
            for token in TOKEN_RE.find_iter(&node.value) {
                if gate.protect_text(token.as_str(), "")? != token.as_str() {
                    spans.push(Span {
                        entity_type: "LITERAL".into(),
                        start: token.start(),
                        end: token.end(),
                        labels: BTreeSet::from(["literal".into()]),
                        metadata: Default::default(),
                    });
                }
            }
            for span in merge_spans(&node.value, spans)? {
                if owned
                    .iter()
                    .any(|(range, _)| span.start < range.end && range.start < span.end)
                {
                    return Err(Error::TlsBlocked);
                }
                let wire = node.bounds(span.start..span.end)?;
                let original = &payload[wire.clone()];
                let reference = gate.transport_register(&node.value, &span)?;
                let replacement = if original.len() < 3 {
                    vec![b'*'; original.len()]
                } else {
                    self.new_alias(
                        gate,
                        reference,
                        original,
                        span.entity_type != "SECRET" && !span.labels.contains("secret"),
                    )?
                    .into_bytes()
                };
                replacements.push((wire, replacement));
            }
        }
        let output = replace(payload, replacements)?;
        protocols::protect_request(
            api,
            &json::from_slice(&output)?,
            &Observer {
                gate,
                allowed: RefCell::new(BTreeMap::new()),
                residual: Some(self),
            },
        )?;
        self.guard(gate)?;
        Ok(output)
    }
    pub fn restore_json(
        &mut self,
        gate: &mut PrivacyGate,
        api: Api,
        payload: &[u8],
    ) -> Result<Vec<u8>> {
        let (mut body, nodes) = self.parse(gate, payload)?;
        self.reserve(&nodes)?;
        let mut secret_nodes = Vec::new();
        for node in &nodes {
            if !node.key && node.path.ends_with("/arguments") {
                secret_nodes.extend(inner_nodes(node)?);
            }
        }
        let mut ranges = Vec::new();
        for node in nodes.iter().chain(&secret_nodes) {
            for span in gate
                .spans(&node.value, "", false)?
                .iter()
                .filter(|span| span.entity_type == "SECRET" || span.labels.contains("secret"))
            {
                ranges.push(node.bounds(span.start..span.end)?);
            }
        }
        ranges.sort_by_key(|range| range.start);
        let mut merged: Vec<Range<usize>> = Vec::new();
        for range in ranges {
            if let Some(last) = merged.last_mut().filter(|last| range.start <= last.end) {
                last.end = last.end.max(range.end);
            } else {
                merged.push(range);
            }
        }
        let mut replacements: Vec<_> = merged
            .iter()
            .map(|range| (range.clone(), vec![b'*'; range.len()]))
            .collect();
        let mut candidates = BTreeMap::new();
        for node in &nodes {
            if node.key {
                continue;
            }
            let mut swaps = Vec::new();
            for (range, alias) in self.matches(&node.value)? {
                let record = &self.aliases[&alias];
                self.entry(gate, record)?;
                let wire = node.bounds(range.clone())?;
                if !self.reversible(gate, record)?
                    || record.original.len() != wire.len()
                    || merged
                        .iter()
                        .any(|other| wire.start < other.end && other.start < wire.end)
                {
                    continue;
                }
                let value = self.entry(gate, record)?.value;
                add_candidate(
                    payload,
                    &node.path,
                    range,
                    wire,
                    record.original.to_vec(),
                    value,
                    &mut candidates,
                    &mut swaps,
                )?;
            }
            for token in TOKEN_RE.find_iter(&node.value) {
                let reference = Reference::parse(token.as_str())?;
                let Some(entry) = gate.transport_entry(&reference)? else {
                    continue;
                };
                let wire = node.bounds(token.range())?;
                if merged
                    .iter()
                    .any(|other| wire.start < other.end && other.start < wire.end)
                {
                    continue;
                }
                let value = if entry.is_secret() {
                    Zeroizing::new("[REDACTED_SECRET]".into())
                } else {
                    entry.value
                };
                if gate
                    .secret_literals()?
                    .iter()
                    .any(|secret| value.contains(secret.as_str()))
                {
                    continue;
                }
                let encoded = serde_json::to_vec(value.as_str()).map_err(|_| Error::TlsBlocked)?;
                if encoded.len() - 2 > wire.len() {
                    continue;
                }
                let mut padded = encoded[1..encoded.len() - 1].to_vec();
                padded.resize(wire.len(), b' ');
                add_candidate(
                    payload,
                    &node.path,
                    token.range(),
                    wire,
                    padded,
                    value,
                    &mut candidates,
                    &mut swaps,
                )?;
            }
            let mut text = node.value.to_string();
            swaps.sort_by_key(|(range, _)| range.start);
            if swaps.windows(2).any(|pair| pair[0].0.end > pair[1].0.start) {
                return Err(Error::TlsBlocked);
            }
            for (range, marker) in swaps.into_iter().rev() {
                text.replace_range(range, &marker);
            }
            *body.pointer_mut(&node.path).ok_or(Error::TlsBlocked)? = Value::String(text);
        }
        let restorer = CandidateRestorer {
            candidates: &candidates,
        };
        let safe = response::restore_response(api, &body, &restorer)?;
        for (marker, candidate) in &candidates {
            if safe
                .pointer(&candidate.path)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.contains(marker))
            {
                replacements.push((candidate.wire.clone(), candidate.original.to_vec()));
            }
        }
        let output = replace(payload, replacements)?;
        self.guard(gate)?;
        gate.remember_response_ids(&json::from_slice(&output)?)?;
        Ok(output)
    }
}
struct Observer<'a> {
    gate: &'a PrivacyGate,
    allowed: RefCell<BTreeMap<String, String>>,
    residual: Option<&'a LengthPreservingTransformer>,
}
impl RequestProtector for Observer<'_> {
    fn text(&self, text: &str, path: &str) -> Result<String> {
        if let Some(transformer) = self.residual {
            let ranges = transformer.matches(text)?;
            for (_, alias) in &ranges {
                transformer.entry(self.gate, &transformer.aliases[alias])?;
            }
            if self.gate.spans(text, path, false)?.iter().any(|span| {
                !ranges
                    .iter()
                    .any(|(range, _)| range.start <= span.start && span.end <= range.end)
            }) {
                return Err(Error::TlsBlocked);
            }
        } else {
            self.allowed.borrow_mut().insert(path.into(), text.into());
        }
        Ok(text.into())
    }
    fn structural(&self, text: &str, path: &str) -> Result<()> {
        self.gate.structural(text, path)
    }
    fn number(&self, _: &Number, _: &str) -> Result<()> {
        Err(Error::PrivateNumberRequiresCompute)
    }
}
struct Candidate {
    path: String,
    wire: Range<usize>,
    original: Zeroizing<Vec<u8>>,
    value: Zeroizing<String>,
}
#[allow(clippy::too_many_arguments)]
fn add_candidate(
    payload: &[u8],
    path: &str,
    range: Range<usize>,
    wire: Range<usize>,
    original: Vec<u8>,
    value: Zeroizing<String>,
    candidates: &mut BTreeMap<String, Candidate>,
    swaps: &mut Vec<(Range<usize>, String)>,
) -> Result<()> {
    if candidates.len() >= 16_384 {
        return Err(Error::ResourceLimit);
    }
    let marker = loop {
        let marker = format!("<CP_TLS_{}>", random_hex::<16>()?);
        if !payload
            .windows(marker.len())
            .any(|window| window == marker.as_bytes())
            && !candidates.contains_key(&marker)
        {
            break marker;
        }
    };
    candidates.insert(
        marker.clone(),
        Candidate {
            path: path.into(),
            wire,
            original: Zeroizing::new(original),
            value,
        },
    );
    swaps.push((range, marker));
    Ok(())
}
struct CandidateRestorer<'a> {
    candidates: &'a BTreeMap<String, Candidate>,
}
impl ResponseRestorer for CandidateRestorer<'_> {
    fn display_group(&self, values: &[String]) -> Result<Vec<String>> {
        let mut renderer = DisplayRestorer::new();
        let mut output = Vec::new();
        let mut pending = false;
        for (index, value) in values.iter().enumerate() {
            output.push(renderer.feed(value, |marker| {
                Ok(self.candidates.get(marker).map_or_else(
                    || marker.to_owned(),
                    |candidate| candidate.value.to_string(),
                ))
            })?);
            if index + 1 < values.len() && renderer.has_pending() {
                pending = true;
            }
        }
        if pending {
            return Ok(values.to_vec());
        }
        if let Some(last) = output.last_mut() {
            last.push_str(&renderer.finish()?);
        }
        Ok(output)
    }
    fn tool_arguments(&self, _name: &str, arguments: &Value) -> Result<Value> {
        // Carrier never restores executable parameters. Compute aliases are
        // resolved only at the authenticated local compute endpoint.
        Ok(arguments.clone())
    }
    fn redact_json(&self, value: &Value) -> Result<Value> {
        Ok(value.clone())
    }
}
fn replace(payload: &[u8], mut swaps: Vec<(Range<usize>, Vec<u8>)>) -> Result<Vec<u8>> {
    swaps.sort_by_key(|(range, _)| range.start);
    let mut previous = 0;
    let mut result = payload.to_vec();
    for (range, value) in swaps {
        if range.start < previous
            || range.start >= range.end
            || range.end > payload.len()
            || range.len() != value.len()
        {
            return Err(Error::TlsBlocked);
        }
        result[range.clone()].copy_from_slice(&value);
        previous = range.end;
    }
    json::from_slice(&result)?;
    Ok(result)
}
