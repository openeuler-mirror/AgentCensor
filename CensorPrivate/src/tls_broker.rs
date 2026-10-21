//! Authenticated decision plane; never releases an incomplete HTTP message.
use crate::{
    error::{Error, Result},
    gate::PrivacyGate,
    json,
    protocols::Api,
    tls_privacy::LengthPreservingTransformer,
};
use regex::Regex;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    net::Ipv4Addr,
    sync::LazyLock,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;
pub const REPLACE: u8 = 2;
pub const BLOCK: u8 = 3;
pub const DEFER: u8 = 4;
pub const MAX_MESSAGE: usize = 256 * 1024 + 32772;
static REQUEST_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^POST /[A-Za-z0-9/_-]+ HTTP/1\.1$").unwrap());
static RESPONSE_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^HTTP/1\.1 2[0-9]{2} [ -~]*$").unwrap());
static PUBLIC_AGENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:curl/[0-9.]+|python-[a-z-]+/[0-9.]+|cp-fixture|tls-fixture|CensorPrivacy/[0-9.]+)$",
    )
    .unwrap()
});
pub fn decode_frame(raw: &[u8], direction: u8, limit: usize) -> Result<(u64, &[u8])> {
    if raw.len() < 20
        || &raw[..4] != b"CPT1"
        || raw[4] != direction
        || !matches!(direction, 1 | 2)
        || raw[5..8] != [0, 0, 0]
    {
        return Err(Error::TlsBlocked);
    }
    let stream = u64::from_be_bytes(raw[8..16].try_into().map_err(|_| Error::TlsBlocked)?);
    let length =
        u32::from_be_bytes(raw[16..20].try_into().map_err(|_| Error::TlsBlocked)?) as usize;
    if stream == 0 || length == 0 || length > limit || raw.len() - 20 != length {
        return Err(Error::TlsBlocked);
    }
    Ok((stream, &raw[20..]))
}
pub fn encode_reply(action: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(12 + payload.len());
    bytes.extend_from_slice(b"CPR1");
    bytes.extend_from_slice(&[action, 0, 0, 0]);
    bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}
struct Stream {
    out: Zeroizing<Vec<u8>>,
    incoming: Zeroizing<Vec<u8>>,
    awaiting: Option<Api>,
    started: Instant,
    touched: Instant,
    count: bool,
}
impl Stream {
    fn new(now: Instant) -> Self {
        Self {
            out: Zeroizing::new(Vec::new()),
            incoming: Zeroizing::new(Vec::new()),
            awaiting: None,
            started: now,
            touched: now,
            count: false,
        }
    }
}
struct Headers {
    offset: usize,
    length: usize,
    start: String,
    fields: BTreeMap<String, String>,
}
pub struct TlsBroker {
    pub transformer: LengthPreservingTransformer,
    host: Ipv4Addr,
    port: u16,
    key: Option<Zeroizing<String>>,
    epoch: u64,
    streams: BTreeMap<u64, Stream>,
    pub max_bytes: usize,
    max_streams: usize,
    max_buffered: usize,
    timeout: Duration,
    protected: u64,
    restored: u64,
    blocked: u64,
    deferred: u64,
}
impl TlsBroker {
    pub fn new(gate: &PrivacyGate, upstream: &str, key: Option<&str>) -> Result<Self> {
        let url = url::Url::parse(upstream).map_err(|_| Error::TlsBlocked)?;
        let host: Ipv4Addr = url
            .host_str()
            .ok_or(Error::TlsBlocked)?
            .parse()
            .map_err(|_| Error::TlsBlocked)?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "" | "/")
        {
            return Err(Error::TlsBlocked);
        }
        Ok(Self {
            transformer: LengthPreservingTransformer::new(gate)?,
            host,
            port: url.port().unwrap_or(443),
            key: key.map(|key| Zeroizing::new(key.to_owned())),
            epoch: gate.epoch(),
            streams: BTreeMap::new(),
            max_bytes: gate.max_body_bytes().min(MAX_MESSAGE),
            max_streams: 64,
            max_buffered: 32 * 1024 * 1024,
            timeout: Duration::from_secs(60),
            protected: 0,
            restored: 0,
            blocked: 0,
            deferred: 0,
        })
    }
    fn guard(&mut self, gate: &PrivacyGate) -> Result<()> {
        gate.validate()?;
        if gate.epoch() != self.epoch {
            self.close();
            self.transformer = LengthPreservingTransformer::new(gate)?;
            self.epoch = gate.epoch();
        }
        Ok(())
    }
    pub fn status(&mut self, gate: &PrivacyGate) -> Result<Value> {
        self.guard(gate)?;
        Ok(
            json!({"status":"ready","run_id":gate.run_id(),"policy_epoch":gate.epoch(),"transport":"tls-direct-http1-json","peer":format!("{}:{}",self.host,self.port),"limits":{"message_bytes":self.max_bytes,"streams":self.max_streams,"buffered_bytes":self.max_buffered},"counts":{"protected_requests":self.protected,"restored_responses":self.restored,"blocked":self.blocked,"deferred":self.deferred}}),
        )
    }
    pub fn close(&mut self) {
        self.streams.clear();
    }
    pub fn decide(
        &mut self,
        gate: &mut PrivacyGate,
        stream: u64,
        payload: &[u8],
        direction: u8,
    ) -> Result<(u8, Vec<u8>)> {
        self.guard(gate)?;
        let now = Instant::now();
        self.streams
            .retain(|_, state| now.duration_since(state.touched) <= self.timeout);
        let outcome = self.decision(gate, stream, payload, direction, now);
        if outcome.is_err() {
            self.streams.remove(&stream);
            self.blocked = self.blocked.saturating_add(1);
        }
        outcome
    }
    fn decision(
        &mut self,
        gate: &mut PrivacyGate,
        stream: u64,
        payload: &[u8],
        direction: u8,
        now: Instant,
    ) -> Result<(u8, Vec<u8>)> {
        if stream == 0 || payload.is_empty() || !matches!(direction, 1 | 2) {
            return Err(Error::TlsBlocked);
        }
        if !self.streams.contains_key(&stream) {
            if direction != 1 || self.streams.len() >= self.max_streams {
                return Err(Error::ResourceLimit);
            }
            self.streams.insert(stream, Stream::new(now));
        }
        let buffered: usize = self
            .streams
            .values()
            .map(|state| state.out.len() + state.incoming.len())
            .sum();
        if buffered.saturating_add(payload.len()) > self.max_buffered {
            return Err(Error::ResourceLimit);
        }
        let state = self.streams.get_mut(&stream).ok_or(Error::TlsBlocked)?;
        if now.duration_since(state.started) > self.timeout {
            return Err(Error::TlsBlocked);
        }
        if direction == 1 && (state.awaiting.is_some() || !state.incoming.is_empty())
            || direction == 2 && state.awaiting.is_none()
        {
            return Err(Error::TlsBlocked);
        }
        let buffer = if direction == 1 {
            &mut state.out
        } else {
            &mut state.incoming
        };
        if buffer.len().saturating_add(payload.len()) > self.max_bytes {
            return Err(Error::ResourceLimit);
        }
        buffer.extend_from_slice(payload);
        state.touched = now;
        let Some(headers) = headers(buffer, direction == 2)? else {
            self.deferred = self.deferred.saturating_add(1);
            return Ok((DEFER, Vec::new()));
        };
        let total = headers
            .offset
            .checked_add(headers.length)
            .ok_or(Error::ResourceLimit)?;
        if total > self.max_bytes || headers.length > 256 * 1024 {
            return Err(Error::ResourceLimit);
        }
        // Validate headers before asking the caller for more body bytes.
        let original = Zeroizing::new(buffer.to_vec());
        let safe_head = if direction == 1 {
            self.request_headers(gate, &headers, &original[..headers.offset])?
        } else {
            Vec::new()
        };
        if original.len() < total {
            self.deferred = self.deferred.saturating_add(1);
            return Ok((DEFER, Vec::new()));
        }
        if original.len() != total {
            return Err(Error::TlsBlocked);
        }
        let body = &original[headers.offset..];
        let state = self.streams.get(&stream).ok_or(Error::TlsBlocked)?;
        let (mut output, transformed, api, count) = if direction == 1 {
            let path = headers
                .start
                .split_whitespace()
                .nth(1)
                .ok_or(Error::TlsBlocked)?;
            let api = match path {
                "/v1/chat/completions" => Api::Chat,
                "/v1/responses" => Api::Responses,
                "/v1/messages" | "/v1/messages/count_tokens" => Api::Messages,
                _ => return Err(Error::TlsBlocked),
            };
            if json::from_slice(body)?.get("stream") == Some(&Value::Bool(true)) {
                return Err(Error::TlsBlocked);
            }
            (
                safe_head,
                self.transformer.protect_json(gate, api, body)?,
                Some(api),
                path.ends_with("/count_tokens"),
            )
        } else {
            let api = state.awaiting.ok_or(Error::TlsBlocked)?;
            let transformed = if state.count {
                let value = json::from_slice(body)?;
                if value.as_object().is_none_or(|object| object.len() != 1)
                    || value
                        .get("input_tokens")
                        .and_then(Value::as_u64)
                        .is_none_or(|n| n >= 1 << 53)
                {
                    return Err(Error::TlsBlocked);
                }
                body.to_vec()
            } else {
                self.transformer.restore_json(gate, api, body)?
            };
            let mut head = original[..headers.offset].to_vec();
            let mut mask = vec![false; head.len()];
            for secret in gate.secret_literals()? {
                let bytes = secret.as_bytes();
                if bytes.is_empty() {
                    continue;
                }
                for (index, window) in head.windows(bytes.len()).enumerate() {
                    if window == bytes {
                        mask[index..index + bytes.len()].fill(true);
                    }
                }
            }
            for (byte, redact) in head.iter_mut().zip(mask) {
                if redact {
                    *byte = b'*';
                }
            }
            (head, transformed, None, false)
        };
        output.extend_from_slice(&transformed);
        if output.len() != original.len() {
            return Err(Error::TlsBlocked);
        }
        gate.validate()?;
        if direction == 1 {
            let state = self.streams.get_mut(&stream).ok_or(Error::TlsBlocked)?;
            state.out.clear();
            state.awaiting = api;
            state.count = count;
            self.protected = self.protected.saturating_add(1);
        } else {
            self.streams.remove(&stream);
            self.restored = self.restored.saturating_add(1);
        }
        Ok((REPLACE, output))
    }
    fn request_headers(
        &self,
        gate: &PrivacyGate,
        headers: &Headers,
        head: &[u8],
    ) -> Result<Vec<u8>> {
        let path = headers
            .start
            .split_whitespace()
            .nth(1)
            .ok_or(Error::TlsBlocked)?;
        let authority = headers.fields.get("host").ok_or(Error::TlsBlocked)?;
        if ![
            "/v1/messages",
            "/v1/chat/completions",
            "/v1/responses",
            "/v1/messages/count_tokens",
        ]
        .contains(&path)
            || !(authority == &format!("{}:{}", self.host, self.port)
                || self.port == 443 && authority == &self.host.to_string())
        {
            return Err(Error::TlsBlocked);
        }
        for (name, value) in &headers.fields {
            if ![
                "host",
                "content-length",
                "content-type",
                "accept",
                "accept-encoding",
                "content-encoding",
                "user-agent",
                "authorization",
                "x-api-key",
                "anthropic-version",
                "connection",
            ]
            .contains(&name.as_str())
            {
                return Err(Error::TlsBlocked);
            }
            if matches!(name.as_str(), "authorization" | "x-api-key") {
                let key = self.key.as_ref().ok_or(Error::TlsBlocked)?;
                let expected = Zeroizing::new(if name == "authorization" {
                    format!("Bearer {}", key.as_str())
                } else {
                    key.to_string()
                });
                if !bool::from(value.as_bytes().ct_eq(expected.as_bytes())) {
                    return Err(Error::TlsBlocked);
                }
                continue;
            }
            if matches!(name.as_str(), "host" | "content-length") {
                continue;
            }
            if name == "anthropic-version" {
                if value != "2023-06-01" {
                    return Err(Error::TlsBlocked);
                }
                continue;
            }
            if name == "user-agent" && PUBLIC_AGENT.is_match(value) {
                continue;
            }
            let mut spans = gate.spans(value, "", false)?;
            if name == "user-agent" {
                spans.retain(|span| {
                    !matches!(
                        span.entity_type.as_str(),
                        "NUMBER" | "MONEY" | "DATE" | "DURATION"
                    )
                });
            }
            if !spans.is_empty() {
                return Err(Error::TlsBlocked);
            }
        }
        let mut output = head.to_vec();
        if headers
            .fields
            .get("accept-encoding")
            .is_some_and(|value| !value.eq_ignore_ascii_case("identity"))
        {
            let mut offset = 0;
            for line in head.split(|byte| *byte == b'\n') {
                let length = line
                    .strip_suffix(b"\r")
                    .map_or(line.len(), |line| line.len());
                if line
                    .get(..16)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"accept-encoding:"))
                {
                    let replacement = b"Accept-Encoding: identity";
                    if length < replacement.len() {
                        return Err(Error::TlsBlocked);
                    }
                    output[offset..offset + length].fill(b' ');
                    output[offset..offset + replacement.len()].copy_from_slice(replacement);
                }
                offset += line.len() + 1;
            }
        }
        Ok(output)
    }
}
fn headers(raw: &[u8], response: bool) -> Result<Option<Headers>> {
    let boundary = raw.windows(4).position(|bytes| bytes == b"\r\n\r\n");
    let Some(boundary) = boundary else {
        let expected = if response {
            b"HTTP/1.1 ".as_slice()
        } else {
            b"POST ".as_slice()
        };
        if raw.len() > 32768 || !expected.starts_with(raw) && !raw.starts_with(expected) {
            return Err(Error::TlsBlocked);
        }
        return Ok(None);
    };
    if boundary > 32768 {
        return Err(Error::ResourceLimit);
    }
    let text = std::str::from_utf8(&raw[..boundary]).map_err(|_| Error::TlsBlocked)?;
    let mut lines = text.split("\r\n");
    let start = lines.next().ok_or(Error::TlsBlocked)?;
    if !(if response {
        RESPONSE_LINE.is_match(start)
    } else {
        REQUEST_LINE.is_match(start)
    }) {
        return Err(Error::TlsBlocked);
    }
    let mut fields = BTreeMap::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(Error::TlsBlocked)?;
        if name.is_empty()
            || name.len() > 80
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(Error::TlsBlocked);
        }
        let value = value.trim_matches([' ', '\t']);
        if !value.bytes().all(|byte| (32..127).contains(&byte))
            || fields
                .insert(name.to_ascii_lowercase(), value.to_owned())
                .is_some()
        {
            return Err(Error::TlsBlocked);
        }
    }
    if ["transfer-encoding", "upgrade", "expect"]
        .iter()
        .any(|name| fields.contains_key(*name))
        || fields
            .get("content-encoding")
            .is_some_and(|value| !value.eq_ignore_ascii_case("identity"))
    {
        return Err(Error::TlsBlocked);
    }
    let length = fields.get("content-length").ok_or(Error::TlsBlocked)?;
    if length.is_empty()
        || length.len() > 8
        || !length.bytes().all(|byte| byte.is_ascii_digit())
        || length.len() > 1 && length.starts_with('0')
    {
        return Err(Error::TlsBlocked);
    }
    let length = length.parse().map_err(|_| Error::TlsBlocked)?;
    if fields.get("content-type").is_none_or(|value| {
        !value
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("application/json")
    }) {
        return Err(Error::TlsBlocked);
    }
    Ok(Some(Headers {
        offset: boundary + 4,
        length,
        start: start.into(),
        fields,
    }))
}
