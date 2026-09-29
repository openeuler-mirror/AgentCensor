use std::collections::{BTreeMap, HashSet, VecDeque};

use model_core::payload::{PayloadContentState, PayloadDirection, PayloadSegment};
use model_core::process::SessionIdentity;
use semantic_action_contract::{
    SemanticAction, SemanticActionCompleteness, SemanticActionKind, SemanticActionStatus,
    SemanticEvidence, SemanticEvidenceKind,
};

/// Parse a bounded HTTP/1.1 plaintext segment. This parser only projects
/// headers that are actually present; it never reconstructs body text from
/// metadata or from an incomplete segment.
pub fn project_http1_payload(
    segment: &PayloadSegment,
    session: Option<SessionIdentity>,
) -> Option<SemanticAction> {
    let bytes = segment.bytes.as_deref()?;
    if let Some(action) = project_http2_payload(segment, bytes, session.clone()) {
        return Some(action);
    }
    let separator = bytes.windows(4).position(|window| window == b"\r\n\r\n")?;
    let header_end = separator + 4;
    let header_text = std::str::from_utf8(&bytes[..separator]).ok()?;
    let mut lines = header_text.split("\r\n");
    let start_line = lines.next()?;
    let (kind, title, mut attributes) = if start_line.starts_with("HTTP/") {
        let mut parts = start_line.splitn(3, ' ');
        let protocol = parts.next()?.to_string();
        let status = parts.next()?.parse::<u16>().ok()?;
        let reason = parts.next().unwrap_or_default().to_string();
        let mut attributes = BTreeMap::new();
        attributes.insert("http.protocol".to_string(), protocol);
        attributes.insert("http.status_code".to_string(), status.to_string());
        if !reason.is_empty() {
            attributes.insert("http.reason".to_string(), reason.clone());
        }
        (
            SemanticActionKind::HttpMessage,
            format!("HTTP {status}"),
            attributes,
        )
    } else {
        let mut parts = start_line.splitn(3, ' ');
        let method = parts.next()?.to_string();
        let path = parts.next()?.to_string();
        let protocol = parts.next()?.to_string();
        if !protocol.starts_with("HTTP/") || method.is_empty() || path.is_empty() {
            return None;
        }
        let mut attributes = BTreeMap::new();
        attributes.insert("http.protocol".to_string(), protocol);
        attributes.insert("http.method".to_string(), method.clone());
        attributes.insert("http.path".to_string(), path.clone());
        (
            SemanticActionKind::HttpMessage,
            format!("{method} {path}"),
            attributes,
        )
    };
    for line in lines {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        attributes.insert(
            format!("http.header.{}", key.trim().to_ascii_lowercase()),
            value.trim().to_string(),
        );
    }
    let declared_length = attributes
        .get("http.header.content-length")
        .and_then(|value| value.parse::<usize>().ok());
    let chunked = attributes
        .get("http.header.transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"));
    let body_available = bytes.len().saturating_sub(header_end);
    let chunked_complete = !chunked || chunked_body_complete(&bytes[header_end..]);
    let body_complete = declared_length.is_none_or(|length| body_available >= length)
        && chunked_complete
        && !matches!(
            segment.content_state,
            PayloadContentState::Truncated | PayloadContentState::Loss
        );
    let completeness = if body_complete {
        SemanticActionCompleteness::Complete
    } else {
        SemanticActionCompleteness::Partial
    };
    attributes.insert("http.headers".to_string(), header_text.to_string());
    attributes.insert("http.body_bytes".to_string(), body_available.to_string());
    attributes.insert(
        "http.content_state".to_string(),
        if body_complete { "complete" } else { "partial" }.to_string(),
    );
    attributes.insert(
        "censorscope.action.kind".to_string(),
        kind.as_str().to_string(),
    );
    let direction = match segment.direction {
        PayloadDirection::Outbound => "outbound",
        PayloadDirection::Inbound => "inbound",
        _ => "unknown",
    };
    attributes.insert("http.direction".to_string(), direction.to_string());
    Some(SemanticAction {
        action_id: format!(
            "payload:{}:{}:{}",
            segment.trace_id.get(),
            segment.stream_key.as_deref().unwrap_or("unknown"),
            segment.sequence
        ),
        trace_id: segment.trace_id,
        kind,
        title,
        start_time: segment.observed_at,
        end_time: segment.completed.then_some(segment.observed_at),
        process: segment.process,
        status: if matches!(segment.content_state, PayloadContentState::Loss) {
            SemanticActionStatus::Unknown
        } else {
            SemanticActionStatus::Success
        },
        completeness,
        confidence_millis: (completeness == SemanticActionCompleteness::Complete).then_some(900),
        attributes,
        evidence: vec![SemanticEvidence {
            kind: SemanticEvidenceKind::PayloadSegment,
            id: segment.sequence,
            role: "http.plaintext".to_string(),
        }],
        session_id: session,
    })
}

/// Return one complete HTTP/1.x message body and its framing metadata.
///
/// This intentionally handles only Content-Length and chunked framing. A
/// close-delimited body is not considered complete while the connection is
/// live because the payload window does not encode a reliable message end.
pub fn extract_http1_message(segment: &PayloadSegment) -> Option<Http1Message> {
    let bytes = segment.bytes.as_deref()?;
    let separator = bytes.windows(4).position(|window| window == b"\r\n\r\n")?;
    let header_end = separator.checked_add(4)?;
    let header_text = std::str::from_utf8(&bytes[..separator]).ok()?;
    let mut lines = header_text.split("\r\n");
    let start_line = lines.next()?;
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let body = &bytes[header_end..];
    let (body, message_len, complete) = if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
    {
        let (decoded, encoded_len) = decode_complete_chunked_body(body)?;
        (decoded, header_end + encoded_len, true)
    } else if let Some(length) = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
    {
        if body.len() < length {
            (body.to_vec(), bytes.len(), false)
        } else {
            (body[..length].to_vec(), header_end + length, true)
        }
    } else {
        (body.to_vec(), bytes.len(), false)
    };
    let is_response = start_line.starts_with("HTTP/");
    let status = if is_response {
        start_line
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse::<u16>().ok())
    } else {
        None
    };
    Some(Http1Message {
        method: (!is_response)
            .then(|| start_line.split_whitespace().next().map(str::to_string))
            .flatten(),
        path: (!is_response)
            .then(|| start_line.split_whitespace().nth(1).map(str::to_string))
            .flatten(),
        status,
        headers,
        body,
        message_len,
        complete,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Http1Message {
    pub method: Option<String>,
    pub path: Option<String>,
    pub status: Option<u16>,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub message_len: usize,
    pub complete: bool,
}

/// Extract one HTTP/2 stream from an already framed segment.
///
/// Use [`extract_http2_messages`] when a capture window may contain multiple
/// interleaved streams.
pub fn extract_http2_message(segment: &PayloadSegment) -> Option<Http2Message> {
    let mut messages = extract_http2_messages(segment);
    (messages.len() == 1).then(|| messages.pop().unwrap())
}

/// Extract independent HTTP/2 messages from all streams present in a capture
/// window. Header blocks may span HEADERS and CONTINUATION frames; trailers
/// are decoded into the same stream's trailer map.
pub fn extract_http2_messages(segment: &PayloadSegment) -> Vec<Http2Message> {
    let mut decoder = HpackDecoder::default();
    extract_http2_messages_with_decoder(segment, &mut decoder)
}

/// Extract HTTP/2 messages while retaining HPACK dynamic-table state across
/// payload segments belonging to one connection and direction.
pub fn extract_http2_messages_with_decoder(
    segment: &PayloadSegment,
    decoder: &mut HpackDecoder,
) -> Vec<Http2Message> {
    let Some(bytes) = segment.bytes.as_deref() else {
        return Vec::new();
    };
    const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
    let mut cursor = if bytes.starts_with(PREFACE) {
        PREFACE.len()
    } else {
        0
    };
    #[derive(Default)]
    struct State {
        body: Vec<u8>,
        headers: BTreeMap<String, String>,
        trailers: BTreeMap<String, String>,
        header_block: Vec<u8>,
        collecting_headers: bool,
        saw_headers: bool,
        end_stream: bool,
        first_offset: usize,
        last_offset: usize,
    }
    let mut streams = BTreeMap::<u32, State>::new();
    while cursor + 9 <= bytes.len() {
        let header = &bytes[cursor..cursor + 9];
        let length =
            (usize::from(header[0]) << 16) | (usize::from(header[1]) << 8) | usize::from(header[2]);
        let Some(end) = cursor
            .checked_add(9)
            .and_then(|value| value.checked_add(length))
        else {
            break;
        };
        if end > bytes.len() {
            break;
        }
        let frame_type = header[3];
        let flags = header[4];
        let id = (u32::from(header[5] & 0x7f) << 24)
            | (u32::from(header[6]) << 16)
            | (u32::from(header[7]) << 8)
            | u32::from(header[8]);
        if id != 0 {
            let state = streams.entry(id).or_insert_with(|| State {
                first_offset: cursor,
                ..State::default()
            });
            state.last_offset = end;
            let mut payload_start = cursor + 9;
            let mut payload_end = end;
            if matches!(frame_type, 0x0 | 0x1) && flags & 0x8 != 0 {
                let Some(padding) = bytes.get(payload_start).copied().map(usize::from) else {
                    break;
                };
                payload_start += 1;
                if padding > payload_end.saturating_sub(payload_start) {
                    break;
                }
                payload_end -= padding;
            }
            let payload = &bytes[payload_start..payload_end];
            match frame_type {
                0x0 => state.body.extend_from_slice(payload),
                0x1 | 0x9 => {
                    if frame_type == 0x1 {
                        state.collecting_headers = true;
                        state.saw_headers = true;
                        state.header_block.clear();
                    }
                    if state.collecting_headers {
                        state.header_block.extend_from_slice(payload);
                    }
                    if flags & 0x4 != 0 {
                        let decoded = decoder.decode(&state.header_block);
                        if state.body.is_empty() && state.headers.is_empty() {
                            if let Some(decoded) = decoded {
                                state.headers = decoded;
                            }
                        } else if let Some(decoded) = decoded {
                            state.trailers = decoded;
                        }
                        state.collecting_headers = false;
                    }
                }
                _ => {}
            }
            if frame_type == 0x0 && flags & 0x1 != 0 {
                state.end_stream = true;
            }
            if frame_type == 0x1 && flags & 0x1 != 0 {
                state.end_stream = true;
            }
        }
        cursor = end;
    }
    streams
        .into_iter()
        .filter_map(|(stream_id, state)| {
            if state.body.is_empty() && !state.saw_headers {
                return None;
            }
            Some(Http2Message {
                stream_id,
                body: state.body,
                headers: state.headers,
                trailers: state.trailers,
                message_len: state.last_offset.saturating_sub(state.first_offset),
                complete: cursor == bytes.len()
                    && state.end_stream
                    && !matches!(
                        segment.content_state,
                        PayloadContentState::Truncated | PayloadContentState::Loss
                    ),
            })
        })
        .collect()
}

pub struct HpackDecoder {
    dynamic: VecDeque<(String, String)>,
    dynamic_size: usize,
    max_dynamic_size: usize,
}

impl Default for HpackDecoder {
    fn default() -> Self {
        Self {
            dynamic: VecDeque::new(),
            dynamic_size: 0,
            max_dynamic_size: 4096,
        }
    }
}

impl HpackDecoder {
    fn decode(&mut self, block: &[u8]) -> Option<BTreeMap<String, String>> {
        let (headers, base_len, staged, dynamic_size, max_dynamic_size) = {
            let mut state = HpackDecodeState::new(self);
            let mut cursor = 0;
            let mut headers = BTreeMap::new();
            let mut saw_header = false;
            while cursor < block.len() {
                let first = block[cursor];
                if first & 0x80 != 0 {
                    let (index, used) = decode_hpack_int(&block[cursor..], 7)?;
                    let (name, value) = state.lookup(index)?;
                    headers.insert(name, value);
                    cursor += used;
                    saw_header = true;
                    continue;
                }
                if first & 0x20 != 0 {
                    if saw_header {
                        return None;
                    }
                    let (size, used) = decode_hpack_int(&block[cursor..], 5)?;
                    state.resize(size)?;
                    cursor += used;
                    continue;
                }
                let incremental = first & 0x40 != 0;
                let prefix = if incremental { 6 } else { 4 };
                let (name_index, used) = decode_hpack_int(&block[cursor..], prefix)?;
                cursor += used;
                let name = if name_index == 0 {
                    let (raw, used) = decode_hpack_string(&block[cursor..])?;
                    cursor += used;
                    raw
                } else {
                    state.lookup(name_index)?.0
                };
                let (value, used) = decode_hpack_string(&block[cursor..])?;
                cursor += used;
                if incremental {
                    state.insert(name.clone(), value.clone());
                }
                headers.insert(name, value);
                saw_header = true;
            }
            let (base_len, staged, dynamic_size, max_dynamic_size) = state.into_parts();
            (headers, base_len, staged, dynamic_size, max_dynamic_size)
        };
        self.dynamic.truncate(base_len);
        for entry in staged.into_iter().rev() {
            self.dynamic.push_front(entry);
        }
        self.dynamic_size = dynamic_size;
        self.max_dynamic_size = max_dynamic_size;
        Some(headers)
    }
}

const HPACK_MAX_DYNAMIC_TABLE_SIZE: usize = 64 * 1024;

struct HpackDecodeState<'a> {
    base: &'a VecDeque<(String, String)>,
    base_len: usize,
    staged: VecDeque<(String, String)>,
    dynamic_size: usize,
    max_dynamic_size: usize,
}

impl<'a> HpackDecodeState<'a> {
    fn new(decoder: &'a HpackDecoder) -> Self {
        Self {
            base: &decoder.dynamic,
            base_len: decoder.dynamic.len(),
            staged: VecDeque::new(),
            dynamic_size: decoder.dynamic_size,
            max_dynamic_size: decoder.max_dynamic_size,
        }
    }

    fn lookup(&self, index: usize) -> Option<(String, String)> {
        if index == 0 {
            return None;
        }
        if let Some((name, value)) = hpack_static(index) {
            return Some((name.to_string(), value.to_string()));
        }
        let dynamic_index = index.checked_sub(62)?;
        if let Some(value) = self.staged.get(dynamic_index) {
            return Some(value.clone());
        }
        let base_index = dynamic_index.checked_sub(self.staged.len())?;
        (base_index < self.base_len)
            .then(|| self.base.get(base_index).cloned())
            .flatten()
    }

    fn resize(&mut self, size: usize) -> Option<()> {
        if size > HPACK_MAX_DYNAMIC_TABLE_SIZE {
            return None;
        }
        self.max_dynamic_size = size;
        self.evict_to_limit();
        Some(())
    }

    fn insert(&mut self, name: String, value: String) {
        let size = hpack_entry_size(&name, &value);
        if size > self.max_dynamic_size {
            self.staged.clear();
            self.base_len = 0;
            self.dynamic_size = 0;
            return;
        }
        self.staged.push_front((name, value));
        self.dynamic_size = self.dynamic_size.saturating_add(size);
        self.evict_to_limit();
    }

    fn evict_to_limit(&mut self) {
        while self.dynamic_size > self.max_dynamic_size {
            let evicted_size = if self.base_len > 0 {
                self.base_len -= 1;
                self.base
                    .get(self.base_len)
                    .map(|(name, value)| hpack_entry_size(name, value))
            } else {
                self.staged
                    .pop_back()
                    .map(|(name, value)| hpack_entry_size(&name, &value))
            };
            let Some(evicted_size) = evicted_size else {
                self.dynamic_size = 0;
                break;
            };
            self.dynamic_size = self.dynamic_size.saturating_sub(evicted_size);
        }
    }

    fn into_parts(self) -> (usize, VecDeque<(String, String)>, usize, usize) {
        (
            self.base_len,
            self.staged,
            self.dynamic_size,
            self.max_dynamic_size,
        )
    }
}

fn hpack_entry_size(name: &str, value: &str) -> usize {
    32usize
        .saturating_add(name.len())
        .saturating_add(value.len())
}

fn hpack_static(index: usize) -> Option<(&'static str, &'static str)> {
    const STATIC: &[(&str, &str)] = &[
        (":authority", ""),
        (":method", "GET"),
        (":method", "POST"),
        (":path", "/"),
        (":path", "/index.html"),
        (":scheme", "http"),
        (":scheme", "https"),
        (":status", "200"),
        (":status", "204"),
        (":status", "206"),
        (":status", "304"),
        (":status", "400"),
        (":status", "404"),
        (":status", "500"),
        ("accept-charset", ""),
        ("accept-encoding", "gzip, deflate"),
        ("accept-language", ""),
        ("accept-ranges", ""),
        ("accept", ""),
        ("access-control-allow-origin", ""),
        ("age", ""),
        ("allow", ""),
        ("authorization", ""),
        ("cache-control", ""),
        ("content-disposition", ""),
        ("content-encoding", ""),
        ("content-language", ""),
        ("content-length", ""),
        ("content-location", ""),
        ("content-range", ""),
        ("content-type", ""),
        ("cookie", ""),
        ("date", ""),
        ("etag", ""),
        ("expect", ""),
        ("expires", ""),
        ("from", ""),
        ("host", ""),
        ("if-match", ""),
        ("if-modified-since", ""),
        ("if-none-match", ""),
        ("if-range", ""),
        ("if-unmodified-since", ""),
        ("last-modified", ""),
        ("link", ""),
        ("location", ""),
        ("max-forwards", ""),
        ("proxy-authenticate", ""),
        ("proxy-authorization", ""),
        ("range", ""),
        ("referer", ""),
        ("refresh", ""),
        ("retry-after", ""),
        ("server", ""),
        ("set-cookie", ""),
        ("strict-transport-security", ""),
        ("transfer-encoding", ""),
        ("user-agent", ""),
        ("vary", ""),
        ("via", ""),
        ("www-authenticate", ""),
    ];
    STATIC.get(index.checked_sub(1)?).copied()
}

fn decode_hpack_int(bytes: &[u8], prefix: u8) -> Option<(usize, usize)> {
    let mask = (1u8 << prefix) - 1;
    let mut value = usize::from(*bytes.first()? & mask);
    if value < usize::from(mask) {
        return Some((value, 1));
    }
    let mut shift = 0;
    for (index, byte) in bytes.iter().copied().enumerate().skip(1) {
        value = value.checked_add(usize::from(byte & 0x7f).checked_shl(shift)?)?;
        if byte & 0x80 == 0 {
            return Some((value, index + 1));
        }
        shift += 7;
        if shift > usize::BITS - 7 {
            return None;
        }
    }
    None
}

fn decode_hpack_string(bytes: &[u8]) -> Option<(String, usize)> {
    let (length, used) = decode_hpack_int(bytes, 7)?;
    const MAX_HEADER_STRING_BYTES: usize = 64 * 1024;
    if length > MAX_HEADER_STRING_BYTES {
        return None;
    }
    let end = used.checked_add(length)?;
    let raw = bytes.get(used..end)?;
    let decoded = if bytes.first()? & 0x80 != 0 {
        let mut decoded = Vec::with_capacity(length.saturating_mul(2).min(MAX_HEADER_STRING_BYTES));
        httlib_huffman::decode(raw, &mut decoded, httlib_huffman::DecoderSpeed::FiveBits).ok()?;
        if decoded.len() > MAX_HEADER_STRING_BYTES {
            return None;
        }
        decoded
    } else {
        raw.to_vec()
    };
    Some((std::str::from_utf8(&decoded).ok()?.to_string(), end))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Http2Message {
    pub stream_id: u32,
    pub body: Vec<u8>,
    pub headers: BTreeMap<String, String>,
    pub trailers: BTreeMap<String, String>,
    pub message_len: usize,
    pub complete: bool,
}

pub fn project_http2_message(
    segment: &PayloadSegment,
    message: &Http2Message,
    session: Option<SessionIdentity>,
) -> SemanticAction {
    let mut attributes = BTreeMap::new();
    attributes.insert(
        "censorscope.action.kind".to_string(),
        "http.message".to_string(),
    );
    attributes.insert("http.protocol".to_string(), "h2".to_string());
    attributes.insert("http.stream_id".to_string(), message.stream_id.to_string());
    attributes.insert(
        "http.body_bytes".to_string(),
        message.body.len().to_string(),
    );
    for (key, value) in &message.headers {
        match key.as_str() {
            ":method" => attributes.insert("http.method".to_string(), value.clone()),
            ":path" => attributes.insert("http.path".to_string(), value.clone()),
            ":status" => attributes.insert("http.status_code".to_string(), value.clone()),
            _ => attributes.insert(format!("http.header.{key}"), value.clone()),
        };
    }
    for (key, value) in &message.trailers {
        attributes.insert(format!("http.trailer.{key}"), value.clone());
    }
    SemanticAction {
        action_id: format!(
            "payload:{}:{}:h2:{}:{}",
            segment.trace_id.get(),
            segment.stream_key.as_deref().unwrap_or("unknown"),
            message.stream_id,
            segment.sequence
        ),
        trace_id: segment.trace_id,
        kind: SemanticActionKind::HttpMessage,
        title: attributes
            .get("http.path")
            .cloned()
            .unwrap_or_else(|| format!("HTTP/2 stream {}", message.stream_id)),
        start_time: segment.observed_at,
        end_time: message.complete.then_some(segment.observed_at),
        process: segment.process,
        status: if message.complete {
            SemanticActionStatus::Success
        } else {
            SemanticActionStatus::Unknown
        },
        completeness: if message.complete {
            SemanticActionCompleteness::Complete
        } else {
            SemanticActionCompleteness::Partial
        },
        confidence_millis: message.complete.then_some(900),
        attributes,
        evidence: vec![SemanticEvidence {
            kind: SemanticEvidenceKind::PayloadSegment,
            id: segment.sequence,
            role: "http2.stream".to_string(),
        }],
        session_id: session,
    }
}

const HTTP2_MAX_STREAMS: usize = 1024;
const HTTP2_MAX_STREAM_BODY: usize = 4 * 1024 * 1024;
const HTTP2_MAX_HEADER_BLOCK: usize = 64 * 1024;
const HTTP2_RECENTLY_CLOSED_STREAMS: usize = 4096;

#[derive(Default)]
pub struct Http2ConnectionAssembler {
    frame_tail: Vec<u8>,
    streams: BTreeMap<u32, Http2AssemblyStream>,
    pending_continuation: Option<u32>,
    decoder: HpackDecoder,
    recently_closed: HashSet<u32>,
    closed_order: VecDeque<u32>,
}

#[derive(Default)]
struct Http2AssemblyStream {
    body: Vec<u8>,
    headers: BTreeMap<String, String>,
    trailers: BTreeMap<String, String>,
    header_block: Vec<u8>,
    header_is_trailer: bool,
    end_stream_after_headers: bool,
    tainted: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Http2AssemblerOutput {
    pub messages: Vec<Http2Message>,
    pub diagnostics: Vec<String>,
}

impl Http2ConnectionAssembler {
    pub fn ingest(
        &mut self,
        bytes: &[u8],
        content_state: PayloadContentState,
    ) -> Http2AssemblerOutput {
        let mut output = Http2AssemblerOutput::default();
        let input_tainted = matches!(
            content_state,
            PayloadContentState::Loss | PayloadContentState::Truncated
        );
        if input_tainted {
            for stream in self.streams.values_mut() {
                stream.tainted = true;
            }
            output.diagnostics.push("http2_payload_loss".to_string());
        }
        self.frame_tail.extend_from_slice(bytes);
        let mut cursor = 0usize;
        const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
        if self.frame_tail.starts_with(PREFACE) {
            cursor = PREFACE.len();
        }
        while cursor + 9 <= self.frame_tail.len() {
            let header = &self.frame_tail[cursor..cursor + 9];
            let length = (usize::from(header[0]) << 16)
                | (usize::from(header[1]) << 8)
                | usize::from(header[2]);
            let Some(end) = cursor
                .checked_add(9)
                .and_then(|value| value.checked_add(length))
            else {
                output
                    .diagnostics
                    .push("http2_frame_length_overflow".to_string());
                break;
            };
            if end > self.frame_tail.len() {
                break;
            }
            let frame_type = header[3];
            let flags = header[4];
            let stream_id = (u32::from(header[5] & 0x7f) << 24)
                | (u32::from(header[6]) << 16)
                | (u32::from(header[7]) << 8)
                | u32::from(header[8]);
            if let Some(expected) = self.pending_continuation
                && (frame_type != 0x9 || stream_id != expected)
            {
                output
                    .diagnostics
                    .push("http2_continuation_interrupted".to_string());
                if let Some(stream) = self.streams.get_mut(&expected) {
                    stream.tainted = true;
                    stream.header_block.clear();
                }
                self.pending_continuation = None;
            }
            if stream_id != 0 {
                if self.recently_closed.contains(&stream_id) {
                    output
                        .diagnostics
                        .push("http2_frame_after_end_stream".to_string());
                    cursor = end;
                    continue;
                }
                if !self.streams.contains_key(&stream_id) && self.streams.len() >= HTTP2_MAX_STREAMS
                {
                    output.diagnostics.push("http2_stream_limit".to_string());
                    cursor = end;
                    continue;
                }
                let state = self.streams.entry(stream_id).or_default();
                state.tainted |= input_tainted;
                let payload = &self.frame_tail[cursor + 9..end];
                match frame_type {
                    0x0 => {
                        if state.headers.is_empty() && state.header_block.is_empty() {
                            state.tainted = true;
                            output
                                .diagnostics
                                .push("http2_data_before_headers".to_string());
                        }
                        let Some(data) = http2_payload_without_padding(payload, flags) else {
                            state.tainted = true;
                            output
                                .diagnostics
                                .push("http2_invalid_data_padding".to_string());
                            cursor = end;
                            continue;
                        };
                        if state.body.len().saturating_add(data.len()) > HTTP2_MAX_STREAM_BODY {
                            state.tainted = true;
                            output
                                .diagnostics
                                .push("http2_stream_body_limit".to_string());
                        } else {
                            state.body.extend_from_slice(data);
                        }
                        if flags & 0x1 != 0 {
                            self.finish_stream(stream_id, &mut output);
                        }
                    }
                    0x1 => {
                        let Some(block) = http2_headers_fragment(payload, flags) else {
                            state.tainted = true;
                            output
                                .diagnostics
                                .push("http2_invalid_headers_frame".to_string());
                            cursor = end;
                            continue;
                        };
                        state.header_is_trailer =
                            !state.headers.is_empty() || !state.body.is_empty();
                        state.end_stream_after_headers = flags & 0x1 != 0;
                        if state.header_is_trailer && !state.end_stream_after_headers {
                            state.tainted = true;
                            output
                                .diagnostics
                                .push("http2_trailer_without_end_stream".to_string());
                        }
                        state.header_block.clear();
                        if state.header_block.len().saturating_add(block.len())
                            > HTTP2_MAX_HEADER_BLOCK
                        {
                            state.tainted = true;
                            output
                                .diagnostics
                                .push("http2_header_block_limit".to_string());
                        } else {
                            state.header_block.extend_from_slice(block);
                        }
                        if flags & 0x4 != 0 {
                            self.finish_header_block(stream_id, &mut output);
                        } else {
                            self.pending_continuation = Some(stream_id);
                        }
                    }
                    0x9 => {
                        if self.pending_continuation != Some(stream_id) {
                            state.tainted = true;
                            output
                                .diagnostics
                                .push("http2_unexpected_continuation".to_string());
                        } else if state.header_block.len().saturating_add(payload.len())
                            > HTTP2_MAX_HEADER_BLOCK
                        {
                            state.tainted = true;
                            output
                                .diagnostics
                                .push("http2_header_block_limit".to_string());
                        } else {
                            state.header_block.extend_from_slice(payload);
                            if flags & 0x4 != 0 {
                                self.pending_continuation = None;
                                self.finish_header_block(stream_id, &mut output);
                            }
                        }
                    }
                    0x3 => {
                        state.tainted = true;
                        output.diagnostics.push("http2_rst_stream".to_string());
                        self.finish_stream(stream_id, &mut output);
                    }
                    _ => {}
                }
            }
            cursor = end;
        }
        if cursor > 0 {
            self.frame_tail.drain(..cursor);
        }
        if input_tainted {
            self.frame_tail.clear();
            if let Some(stream_id) = self.pending_continuation.take()
                && let Some(stream) = self.streams.get_mut(&stream_id)
            {
                stream.tainted = true;
                stream.header_block.clear();
            }
        }
        output
    }

    fn finish_header_block(&mut self, stream_id: u32, output: &mut Http2AssemblerOutput) {
        let Some(state) = self.streams.get_mut(&stream_id) else {
            return;
        };
        match self.decoder.decode(&state.header_block) {
            Some(headers) if state.header_is_trailer => state.trailers = headers,
            Some(headers) => state.headers = headers,
            None => {
                state.tainted = true;
                output
                    .diagnostics
                    .push("http2_hpack_decode_failed".to_string());
            }
        }
        state.header_block.clear();
        if state.end_stream_after_headers {
            self.finish_stream(stream_id, output);
        }
    }

    fn finish_stream(&mut self, stream_id: u32, output: &mut Http2AssemblerOutput) {
        let Some(state) = self.streams.remove(&stream_id) else {
            return;
        };
        if !state.body.is_empty() || !state.headers.is_empty() || !state.header_block.is_empty() {
            output.messages.push(Http2Message {
                stream_id,
                body: state.body,
                headers: state.headers,
                trailers: state.trailers,
                message_len: 0,
                complete: !state.tainted && state.header_block.is_empty(),
            });
        }
        self.record_closed(stream_id);
    }

    fn record_closed(&mut self, stream_id: u32) {
        if self.recently_closed.insert(stream_id) {
            self.closed_order.push_back(stream_id);
        }
        while self.closed_order.len() > HTTP2_RECENTLY_CLOSED_STREAMS {
            if let Some(expired) = self.closed_order.pop_front() {
                self.recently_closed.remove(&expired);
            }
        }
    }
}

fn http2_payload_without_padding(payload: &[u8], flags: u8) -> Option<&[u8]> {
    if flags & 0x8 == 0 {
        return Some(payload);
    }
    let padding = usize::from(*payload.first()?);
    payload.get(1..payload.len().checked_sub(padding)?)
}

fn http2_headers_fragment(payload: &[u8], flags: u8) -> Option<&[u8]> {
    let mut start = 0usize;
    let mut end = payload.len();
    if flags & 0x8 != 0 {
        let padding = usize::from(*payload.first()?);
        start = 1;
        end = end.checked_sub(padding)?;
    }
    if flags & 0x20 != 0 {
        start = start.checked_add(5)?;
    }
    payload.get(start..end)
}

/// Extract one WebSocket text message, including continuation fragments.
/// Control frames are skipped, while an incomplete data message remains
/// buffered for the next payload segment.
pub fn extract_websocket_message(segment: &PayloadSegment) -> Option<WebSocketMessage> {
    let bytes = segment.bytes.as_deref()?;
    let mut cursor = 0usize;
    let mut message = Vec::new();
    let mut fragmented = false;
    let mut saw_data = false;
    loop {
        let frame = parse_websocket_frame(bytes, cursor)?;
        cursor = frame.next_offset;
        if frame.opcode >= 0x8 {
            continue;
        }
        if frame.opcode == 0x1 {
            if saw_data {
                return None;
            }
            saw_data = true;
            fragmented = !frame.fin;
            message.extend_from_slice(&frame.payload);
        } else if frame.opcode == 0x0 && saw_data && fragmented {
            message.extend_from_slice(&frame.payload);
            fragmented = !frame.fin;
        } else {
            return None;
        }
        if saw_data && !fragmented {
            return Some(WebSocketMessage {
                body: message,
                message_len: cursor,
                complete: matches!(segment.content_state, PayloadContentState::Complete)
                    && (segment.completed || cursor < bytes.len()),
            });
        }
        if cursor >= bytes.len() {
            return None;
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebSocketMessage {
    pub body: Vec<u8>,
    pub message_len: usize,
    pub complete: bool,
}

struct WebSocketFrame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
    next_offset: usize,
}

fn parse_websocket_frame(bytes: &[u8], offset: usize) -> Option<WebSocketFrame> {
    let first = *bytes.get(offset)?;
    let second = *bytes.get(offset + 1)?;
    let fin = first & 0x80 != 0;
    let opcode = first & 0x0f;
    let masked = second & 0x80 != 0;
    let mut length = usize::from(second & 0x7f);
    let mut cursor = offset.checked_add(2)?;
    if length == 126 {
        length = usize::from(u16::from_be_bytes([
            *bytes.get(cursor)?,
            *bytes.get(cursor + 1)?,
        ]));
        cursor = cursor.checked_add(2)?;
    } else if length == 127 {
        length = usize::try_from(u64::from_be_bytes(
            bytes.get(cursor..cursor.checked_add(8)?)?.try_into().ok()?,
        ))
        .ok()?;
        cursor = cursor.checked_add(8)?;
    }
    let mask = if masked {
        let mask = bytes.get(cursor..cursor.checked_add(4)?)?;
        cursor = cursor.checked_add(4)?;
        Some(mask)
    } else {
        None
    };
    let payload = bytes.get(cursor..cursor.checked_add(length)?)?;
    let decoded = if let Some(mask) = mask {
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % 4])
            .collect()
    } else {
        payload.to_vec()
    };
    Some(WebSocketFrame {
        fin,
        opcode,
        payload: decoded,
        next_offset: cursor + length,
    })
}

fn decode_complete_chunked_body(bytes: &[u8]) -> Option<(Vec<u8>, usize)> {
    let mut cursor = 0usize;
    let mut body = Vec::new();
    loop {
        let line_end = bytes[cursor..]
            .windows(2)
            .position(|window| window == b"\r\n")?
            .checked_add(cursor)?;
        let size_text = std::str::from_utf8(&bytes[cursor..line_end]).ok()?;
        let size_text = size_text.split(';').next()?.trim();
        let size = usize::from_str_radix(size_text, 16).ok()?;
        cursor = line_end.checked_add(2)?;
        if size == 0 {
            let trailer_end = bytes[cursor..]
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|offset| cursor + offset + 4)
                .or_else(|| {
                    bytes
                        .get(cursor..)?
                        .starts_with(b"\r\n")
                        .then_some(cursor + 2)
                })?;
            return Some((body, trailer_end));
        }
        let end = cursor.checked_add(size)?;
        body.extend_from_slice(bytes.get(cursor..end)?);
        if bytes.get(end..end.checked_add(2)?)? != b"\r\n" {
            return None;
        }
        cursor = end.checked_add(2)?;
    }
}

fn chunked_body_complete(body: &[u8]) -> bool {
    // A chunked body ends with a zero-size chunk plus an optional trailer and
    // blank line; only that terminal framing makes it complete.
    let mut cursor = 0;
    loop {
        let Some(relative_end) = body[cursor..]
            .windows(2)
            .position(|window| window == b"\r\n")
        else {
            return false;
        };
        let line_end = cursor + relative_end;
        let line = &body[cursor..line_end];
        let size_text = line
            .split(|byte| *byte == b';')
            .next()
            .and_then(|value| std::str::from_utf8(value).ok())
            .map(str::trim);
        let Ok(size) = size_text.unwrap_or_default().to_string().parse::<usize>() else {
            return false;
        };
        cursor = line_end + 2;
        if size == 0 {
            return body[cursor..].starts_with(b"\r\n")
                || body[cursor..]
                    .windows(4)
                    .any(|window| window == b"\r\n\r\n");
        }
        let Some(data_end) = cursor.checked_add(size) else {
            return false;
        };
        if data_end + 2 > body.len() || &body[data_end..data_end + 2] != b"\r\n" {
            return false;
        }
        cursor = data_end + 2;
    }
}

fn project_http2_payload(
    segment: &PayloadSegment,
    bytes: &[u8],
    session: Option<SessionIdentity>,
) -> Option<SemanticAction> {
    const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
    let mut cursor = if bytes.starts_with(PREFACE) {
        PREFACE.len()
    } else {
        0
    };
    let mut frames = 0_u64;
    let mut stream_id = None;
    let mut headers = 0_u64;
    let mut data_bytes = 0_u64;
    while cursor + 9 <= bytes.len() {
        let header = &bytes[cursor..cursor + 9];
        let length =
            (usize::from(header[0]) << 16) | (usize::from(header[1]) << 8) | usize::from(header[2]);
        let end = cursor.checked_add(9)?.checked_add(length)?;
        if end > bytes.len() {
            break;
        }
        let frame_type = header[3];
        let id = (u32::from(header[5] & 0x7f) << 24)
            | (u32::from(header[6]) << 16)
            | (u32::from(header[7]) << 8)
            | u32::from(header[8]);
        if id != 0 && stream_id.is_none() {
            stream_id = Some(id);
        }
        match frame_type {
            0x0 => data_bytes = data_bytes.saturating_add(length as u64),
            0x1 | 0x9 => headers = headers.saturating_add(1),
            _ => {}
        }
        frames += 1;
        cursor = end;
    }
    if frames == 0 || (headers == 0 && data_bytes == 0) {
        return None;
    }
    let complete = cursor == bytes.len()
        && !matches!(
            segment.content_state,
            PayloadContentState::Truncated | PayloadContentState::Loss
        );
    let completeness = if complete {
        SemanticActionCompleteness::Complete
    } else {
        SemanticActionCompleteness::Partial
    };
    let mut attributes = BTreeMap::new();
    attributes.insert(
        "censorscope.action.kind".to_string(),
        "http.message".to_string(),
    );
    attributes.insert("http.protocol".to_string(), "h2".to_string());
    attributes.insert("http.frame_count".to_string(), frames.to_string());
    attributes.insert("http.headers_frames".to_string(), headers.to_string());
    attributes.insert("http.data_bytes".to_string(), data_bytes.to_string());
    if let Some(id) = stream_id {
        attributes.insert("http.stream_id".to_string(), id.to_string());
    }
    Some(SemanticAction {
        action_id: format!(
            "payload:{}:{}:{}",
            segment.trace_id.get(),
            segment.stream_key.as_deref().unwrap_or("unknown"),
            segment.sequence
        ),
        trace_id: segment.trace_id,
        kind: SemanticActionKind::HttpMessage,
        title: "HTTP/2 message".to_string(),
        start_time: segment.observed_at,
        end_time: segment.completed.then_some(segment.observed_at),
        process: segment.process,
        status: if matches!(segment.content_state, PayloadContentState::Loss) {
            SemanticActionStatus::Unknown
        } else {
            SemanticActionStatus::Success
        },
        completeness,
        confidence_millis: (completeness == SemanticActionCompleteness::Complete).then_some(850),
        attributes,
        evidence: vec![SemanticEvidence {
            kind: SemanticEvidenceKind::PayloadSegment,
            id: segment.sequence,
            role: "http2.frame".to_string(),
        }],
        session_id: session,
    })
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use model_core::ids::TraceId;
    use model_core::process::ProcessIdentity;

    use super::*;

    fn segment(bytes: &[u8], state: PayloadContentState) -> PayloadSegment {
        PayloadSegment {
            trace_id: TraceId::new(4),
            process: ProcessIdentity::new(9),
            session_id: None,
            call_id: None,
            observed_at: SystemTime::UNIX_EPOCH,
            source: model_core::payload::PayloadSourceBoundary::Uprobe,
            content_state: state,
            direction: PayloadDirection::Outbound,
            stream_key: Some("s".into()),
            sequence: 3,
            operation_id: None,
            offset: None,
            completed: true,
            original_size: bytes.len() as u64,
            captured_size: bytes.len() as u64,
            library: Some("openssl".into()),
            symbol: Some("SSL_write".into()),
            protocol_hint: Some("tls".into()),
            loss_reason: None,
            bytes: Some(bytes.to_vec()),
        }
    }

    fn h2_frame(frame_type: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
        let length = payload.len();
        let mut bytes = vec![
            ((length >> 16) & 0xff) as u8,
            ((length >> 8) & 0xff) as u8,
            (length & 0xff) as u8,
            frame_type,
            flags,
            ((stream >> 24) & 0x7f) as u8,
            (stream >> 16) as u8,
            (stream >> 8) as u8,
            stream as u8,
        ];
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn parses_http_request_headers_without_copying_body() {
        let action = project_http1_payload(
            &segment(
                b"POST /v1/chat HTTP/1.1\r\nHost: api.test\r\nContent-Length: 20\r\n\r\n{}",
                PayloadContentState::Truncated,
            ),
            None,
        )
        .unwrap();
        assert_eq!(action.kind, SemanticActionKind::HttpMessage);
        assert_eq!(action.completeness, SemanticActionCompleteness::Partial);
        assert_eq!(action.attributes["http.method"], "POST");
        assert_eq!(action.attributes["http.header.host"], "api.test");
        assert!(!action.attributes.contains_key("http.body_text"));
    }

    #[test]
    fn metadata_only_payload_does_not_fabricate_http_action() {
        let mut value = segment(b"", PayloadContentState::MetadataOnly);
        value.bytes = None;
        assert!(project_http1_payload(&value, None).is_none());
    }

    #[test]
    fn projects_http2_frames_and_stream_id() {
        let mut bytes = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n".to_vec();
        bytes.extend_from_slice(&[0, 0, 3, 1, 4, 0, 0, 0, 1, b'a', b'b', b'c']);
        let action =
            project_http1_payload(&segment(&bytes, PayloadContentState::Complete), None).unwrap();
        assert_eq!(action.attributes["http.protocol"], "h2");
        assert_eq!(action.attributes["http.stream_id"], "1");
        assert_eq!(action.attributes["http.headers_frames"], "1");
    }

    #[test]
    fn chunked_http_body_is_partial_until_terminal_chunk() {
        let partial = segment(
            b"POST /upload HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n",
            PayloadContentState::Complete,
        );
        assert_eq!(
            project_http1_payload(&partial, None).unwrap().completeness,
            SemanticActionCompleteness::Partial
        );
        let complete = segment(
            b"POST /upload HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n",
            PayloadContentState::Complete,
        );
        assert_eq!(
            project_http1_payload(&complete, None).unwrap().completeness,
            SemanticActionCompleteness::Complete
        );
    }

    #[test]
    fn websocket_text_message_reassembles_continuations_and_masking() {
        let mut bytes = vec![0x01, 0x82, 1, 2, 3, 4];
        bytes.extend_from_slice(&[b'h' ^ 1, b'i' ^ 2]);
        bytes.extend_from_slice(&[0x80, 0x82, 4, 3, 2, 1]);
        bytes.extend_from_slice(&[b'!' ^ 4, b'?' ^ 3]);
        let mut value = segment(&bytes, PayloadContentState::Complete);
        value.protocol_hint = Some("websocket".to_string());
        let message = extract_websocket_message(&value).expect("fragmented text message");
        assert_eq!(message.body, b"hi!?".to_vec());
        assert_eq!(message.message_len, bytes.len());
        assert!(message.complete);
    }

    #[test]
    fn websocket_control_frame_is_skipped_before_text_message() {
        let mut bytes = vec![0x89, 0x00, 0x81, 0x02];
        bytes.extend_from_slice(b"ok");
        let mut value = segment(&bytes, PayloadContentState::Complete);
        value.protocol_hint = Some("websocket".to_string());
        let message = extract_websocket_message(&value).expect("text after ping");
        assert_eq!(message.body, b"ok".to_vec());
    }

    #[test]
    fn http2_padded_data_excludes_padding_bytes() {
        let body = br#"{"choices":[]}"#;
        let padding = 2u8;
        let length = body.len() + 1 + usize::from(padding);
        let mut bytes = vec![
            ((length >> 16) & 0xff) as u8,
            ((length >> 8) & 0xff) as u8,
            (length & 0xff) as u8,
            0,
            0x9,
            0,
            0,
            0,
            1,
            padding,
        ];
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(&[0, 0]);
        let mut value = segment(&bytes, PayloadContentState::Complete);
        value.protocol_hint = Some("http2".to_string());
        let message = extract_http2_message(&value).expect("padded DATA");
        assert_eq!(message.body, body);
        assert!(message.complete);
    }

    #[test]
    fn http2_decodes_static_hpack_headers_and_trailers() {
        let mut bytes = Vec::new();
        let headers = [0x83, 0x87, 0x84];
        bytes.extend_from_slice(&[0, 0, headers.len() as u8, 1, 4, 0, 0, 0, 1]);
        bytes.extend_from_slice(&headers);
        let body = br#"{"choices":[]}"#;
        bytes.extend_from_slice(&[
            ((body.len() >> 16) & 0xff) as u8,
            ((body.len() >> 8) & 0xff) as u8,
            (body.len() & 0xff) as u8,
            0,
            1,
            0,
            0,
            0,
            1,
        ]);
        bytes.extend_from_slice(body);
        let mut value = segment(&bytes, PayloadContentState::Complete);
        value.protocol_hint = Some("http2".to_string());
        let message = extract_http2_message(&value).expect("h2 message");
        assert_eq!(
            message.headers.get(":method").map(String::as_str),
            Some("POST")
        );
        assert_eq!(
            message.headers.get(":scheme").map(String::as_str),
            Some("https")
        );
        assert_eq!(message.body, body);
    }

    #[test]
    fn http2_keeps_interleaved_streams_independent() {
        let mut bytes = Vec::new();
        for (stream, body) in [
            (1u32, br#"{"choices":[1]}"#.as_slice()),
            (3, br#"{"choices":[2]}"#),
        ] {
            let len = body.len();
            bytes.extend_from_slice(&[
                ((len >> 16) & 0xff) as u8,
                ((len >> 8) & 0xff) as u8,
                (len & 0xff) as u8,
                0,
                1,
                0,
                0,
                (stream >> 8) as u8,
                stream as u8,
            ]);
            bytes.extend_from_slice(body);
        }
        let value = segment(&bytes, PayloadContentState::Complete);
        let messages = extract_http2_messages(&value);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].stream_id, 1);
        assert_eq!(messages[1].stream_id, 3);
        assert_ne!(messages[0].body, messages[1].body);
    }

    #[test]
    fn http2_reassembles_continuation_and_trailer_headers() {
        fn frame(frame_type: u8, flags: u8, stream: u32, body: &[u8]) -> Vec<u8> {
            let length = body.len();
            let mut bytes = vec![
                ((length >> 16) & 0xff) as u8,
                ((length >> 8) & 0xff) as u8,
                (length & 0xff) as u8,
                frame_type,
                flags,
                0,
                0,
                0,
                stream as u8,
            ];
            bytes.extend_from_slice(body);
            bytes
        }
        let mut bytes = frame(1, 0, 1, &[0x83]);
        bytes.extend(frame(
            9,
            0x4,
            1,
            &[
                0x04,
                b"/v1/chat/completions".len() as u8,
                b'/',
                b'v',
                b'1',
                b'/',
                b'c',
                b'h',
                b'a',
                b't',
                b'/',
                b'c',
                b'o',
                b'm',
                b'p',
                b'l',
                b'e',
                b't',
                b'i',
                b'o',
                b'n',
                b's',
            ],
        ));
        let body = br#"{"choices":[]}"#;
        bytes.extend(frame(0, 0, 1, body));
        bytes.extend(frame(
            1,
            0x4,
            1,
            &[
                0x00, 0x09, b'x', b'-', b't', b'r', b'a', b'i', b'l', b'e', b'r', 0x01, b'1',
            ],
        ));
        let value = segment(&bytes, PayloadContentState::Complete);
        let messages = extract_http2_messages(&value);
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].headers.get(":method").map(String::as_str),
            Some("POST")
        );
        assert_eq!(
            messages[0].trailers.get("x-trailer").map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn hpack_dynamic_table_indexes_previous_literal() {
        let mut decoder = HpackDecoder::default();
        let first = decoder.decode(&[0x40, 0x01, b'x', 0x01, b'y']).unwrap();
        assert_eq!(first.get("x").map(String::as_str), Some("y"));
        let second = decoder.decode(&[0xbe]).unwrap();
        assert_eq!(second.get("x").map(String::as_str), Some("y"));
    }

    #[test]
    fn failed_hpack_block_does_not_mutate_dynamic_table() {
        let mut decoder = HpackDecoder::default();
        decoder
            .decode(&[0x40, 0x01, b'x', 0x01, b'y'])
            .expect("initial dynamic entry");
        assert!(
            decoder
                .decode(&[0x40, 0x01, b'a', 0x01, b'b', 0x80])
                .is_none()
        );
        let retained = decoder.decode(&[0xbe]).expect("retained dynamic entry");
        assert_eq!(retained.get("x").map(String::as_str), Some("y"));
        assert!(!retained.contains_key("a"));
    }

    #[test]
    fn hpack_decoder_state_can_cross_payload_segments() {
        let mut decoder = HpackDecoder::default();
        let first = segment(
            &[0, 0, 5, 1, 4, 0, 0, 0, 1, 0x40, 0x01, b'x', 0x01, b'y'],
            PayloadContentState::Complete,
        );
        let _ = extract_http2_messages_with_decoder(&first, &mut decoder);
        let body = br#"{"choices":[]}"#;
        let mut second_bytes = vec![
            0,
            0,
            1,
            1,
            4,
            0,
            0,
            0,
            1,
            0xbe,
            ((body.len() >> 16) & 0xff) as u8,
            ((body.len() >> 8) & 0xff) as u8,
            (body.len() & 0xff) as u8,
            0,
            1,
            0,
            0,
            0,
            1,
        ];
        second_bytes.extend_from_slice(body);
        let second = segment(&second_bytes, PayloadContentState::Complete);
        let messages = extract_http2_messages_with_decoder(&second, &mut decoder);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].headers.get("x").map(String::as_str), Some("y"));
    }

    #[test]
    fn hpack_huffman_decodes_rfc_7541_example() {
        let encoded = [
            0x8c, 0xf1, 0xe3, 0xc2, 0xe5, 0xf2, 0x3a, 0x6b, 0xa0, 0xab, 0x90, 0xf4, 0xff,
        ];
        let (decoded, used) = decode_hpack_string(&encoded).expect("RFC Huffman value");
        assert_eq!(decoded, "www.example.com");
        assert_eq!(used, encoded.len());
    }

    #[test]
    fn hpack_huffman_rejects_eos_and_invalid_padding() {
        assert!(decode_hpack_string(&[0x81, 0xff]).is_none());
        assert!(decode_hpack_string(&[0x84, 0xff, 0xff, 0xff, 0xff]).is_none());
    }

    #[test]
    fn incremental_http2_assembler_spans_segments_without_rescanning() {
        fn frame(frame_type: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
            let length = payload.len();
            let mut bytes = vec![
                ((length >> 16) & 0xff) as u8,
                ((length >> 8) & 0xff) as u8,
                (length & 0xff) as u8,
                frame_type,
                flags,
                0,
                0,
                0,
                stream as u8,
            ];
            bytes.extend_from_slice(payload);
            bytes
        }
        let mut assembler = Http2ConnectionAssembler::default();
        let first = assembler.ingest(&frame(1, 0, 1, &[0x83]), PayloadContentState::Complete);
        assert!(first.messages.is_empty());
        let mut second = frame(9, 0x4, 1, &[0x84]);
        second.extend(frame(0, 0x1, 1, br#"{"choices":[]}"#));
        let output = assembler.ingest(&second, PayloadContentState::Complete);
        assert!(output.diagnostics.is_empty());
        assert_eq!(output.messages.len(), 1);
        assert_eq!(
            output.messages[0]
                .headers
                .get(":method")
                .map(String::as_str),
            Some("POST")
        );
        assert_eq!(output.messages[0].body, br#"{"choices":[]}"#);
        assert!(output.messages[0].complete);
    }

    #[test]
    fn incremental_http2_assembler_reports_bad_continuation() {
        let mut assembler = Http2ConnectionAssembler::default();
        let headers = [0, 0, 1, 1, 0, 0, 0, 0, 1, 0x83];
        let _ = assembler.ingest(&headers, PayloadContentState::Complete);
        let data = [0, 0, 0, 0, 1, 0, 0, 0, 3];
        let output = assembler.ingest(&data, PayloadContentState::Complete);
        assert!(
            output
                .diagnostics
                .iter()
                .any(|value| value == "http2_continuation_interrupted")
        );
    }

    #[test]
    fn incremental_http2_assembler_decodes_terminal_trailers() {
        let mut assembler = Http2ConnectionAssembler::default();
        let mut bytes = h2_frame(1, 0x4, 1, &[0x88]);
        bytes.extend(h2_frame(0, 0, 1, br#"{"choices":[]}"#));
        bytes.extend(h2_frame(
            1,
            0x5,
            1,
            &[
                0x00, 0x09, b'x', b'-', b't', b'r', b'a', b'i', b'l', b'e', b'r', 0x01, b'1',
            ],
        ));
        let output = assembler.ingest(&bytes, PayloadContentState::Complete);
        assert!(output.diagnostics.is_empty());
        assert_eq!(output.messages.len(), 1);
        assert_eq!(
            output.messages[0]
                .trailers
                .get("x-trailer")
                .map(String::as_str),
            Some("1")
        );
        assert!(output.messages[0].complete);
    }

    #[test]
    fn lossy_http2_input_never_emits_a_complete_message() {
        let mut assembler = Http2ConnectionAssembler::default();
        let mut bytes = h2_frame(1, 0x4, 1, &[0x83, 0x84]);
        bytes.extend(h2_frame(0, 0x1, 1, br#"{"model":"lost"}"#));
        let output = assembler.ingest(&bytes, PayloadContentState::Loss);
        assert_eq!(output.messages.len(), 1);
        assert!(!output.messages[0].complete);
        assert!(
            output
                .diagnostics
                .iter()
                .any(|value| value == "http2_payload_loss")
        );
    }

    #[test]
    fn frames_after_end_stream_are_rejected_without_reopening_stream() {
        let mut assembler = Http2ConnectionAssembler::default();
        let mut complete = h2_frame(1, 0x4, 1, &[0x83, 0x84]);
        complete.extend(h2_frame(0, 0x1, 1, br#"{"model":"one"}"#));
        assert_eq!(
            assembler
                .ingest(&complete, PayloadContentState::Complete)
                .messages
                .len(),
            1
        );
        let output = assembler.ingest(
            &h2_frame(0, 0x1, 1, br#"{"model":"two"}"#),
            PayloadContentState::Complete,
        );
        assert!(output.messages.is_empty());
        assert!(
            output
                .diagnostics
                .iter()
                .any(|value| value == "http2_frame_after_end_stream")
        );
    }
}
