//! Authenticated transport: only gate-approved immutable bytes reach a fixed upstream.
use crate::{
    error::{Error, Result},
    gate::PrivacyGate,
    json,
    policy::PrivacyPolicy,
    protocols::Api,
    streaming::SseTransformer,
};
use axum::{
    body::{to_bytes, Body, Bytes},
    extract::{Request, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use flate2::bufread::GzDecoder;
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use std::{
    io::Read,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

pub struct GatewayConfig {
    upstream: url::Url,
    access_token: Zeroizing<String>,
    upstream_key: Option<Zeroizing<String>>,
    pub max_decoded_bytes: usize,
    pub max_decompression_ratio: usize,
    pub upstream_timeout: Duration,
    pub upstream_ca_pem: Option<Vec<u8>>,
}
impl GatewayConfig {
    pub fn new(
        upstream: &str,
        access_token: String,
        upstream_key: Option<String>,
        allow_http_test_upstream: bool,
    ) -> Result<Self> {
        let upstream = url::Url::parse(upstream).map_err(|_| Error::ProtocolBlocked)?;
        if upstream.host_str().is_none()
            || !upstream.username().is_empty()
            || upstream.password().is_some()
            || upstream.query().is_some()
            || upstream.fragment().is_some()
            || !matches!(upstream.scheme(), "http" | "https")
        {
            return Err(Error::ProtocolBlocked);
        }
        if upstream.scheme() == "http"
            && (!allow_http_test_upstream
                || !matches!(
                    upstream.host_str(),
                    Some("127.0.0.1" | "[::1]" | "localhost")
                ))
        {
            return Err(Error::ProtocolBlocked);
        }
        if !(24..=512).contains(&access_token.len())
            || !access_token.bytes().all(|byte| (33..=126).contains(&byte))
        {
            return Err(Error::ProtocolBlocked);
        }
        Ok(Self {
            upstream,
            access_token: Zeroizing::new(access_token),
            upstream_key: upstream_key.map(Zeroizing::new),
            max_decoded_bytes: 32 * 1024 * 1024,
            max_decompression_ratio: 20,
            upstream_timeout: Duration::from_secs(60),
            upstream_ca_pem: None,
        })
    }
}

#[derive(Clone)]
struct ServiceState {
    gate: Arc<Mutex<PrivacyGate>>,
    config: Arc<GatewayConfig>,
    client: reqwest::Client,
}
pub struct GatewayService {
    state: ServiceState,
}
impl GatewayService {
    pub fn new(gate: PrivacyGate, config: GatewayConfig) -> Result<Self> {
        if config.max_decoded_bytes == 0
            || config.max_decoded_bytes > 32 * 1024 * 1024
            || config.max_decompression_ratio == 0
        {
            return Err(Error::ResourceLimit);
        }
        gate.register_transport_secret(&config.access_token)?;
        if let Some(key) = &config.upstream_key {
            gate.register_transport_secret(key)?;
        }
        let mut client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(config.upstream_timeout)
            .min_tls_version(reqwest::tls::Version::TLS_1_2);
        if let Some(pem) = &config.upstream_ca_pem {
            client = client.add_root_certificate(
                reqwest::Certificate::from_pem(pem).map_err(|_| Error::ProtocolBlocked)?,
            );
        }
        Ok(Self {
            state: ServiceState {
                gate: Arc::new(Mutex::new(gate)),
                config: Arc::new(config),
                client: client.build().map_err(|_| Error::ServiceUnavailable)?,
            },
        })
    }
    pub fn router(&self) -> Router {
        Router::new()
            .fallback(handle)
            .with_state(self.state.clone())
    }
    pub fn reload(&self, policy: PrivacyPolicy) -> Result<()> {
        let mut gate = self
            .state
            .gate
            .lock()
            .map_err(|_| Error::ServiceUnavailable)?;
        if let Err(error) = gate.reload(policy).and_then(|()| {
            gate.register_transport_secret(&self.state.config.access_token)?;
            if let Some(key) = &self.state.config.upstream_key {
                gate.register_transport_secret(key)?;
            }
            Ok(())
        }) {
            gate.disable();
            return Err(error);
        }
        Ok(())
    }
    pub fn disable(&self) {
        if let Ok(mut gate) = self.state.gate.lock() {
            gate.disable();
        }
    }
    pub fn close(&self) -> Result<()> {
        self.state
            .gate
            .lock()
            .map_err(|_| Error::ServiceUnavailable)?
            .close()
    }
}

struct Failure {
    code: &'static str,
    status: StatusCode,
}
impl Failure {
    fn bad(code: &'static str) -> Self {
        Self {
            code,
            status: StatusCode::BAD_REQUEST,
        }
    }
}
impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self::bad(error.code())
    }
}
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (
            self.status,
            [(header::CACHE_CONTROL, "no-store")],
            Json(json!({"error":{"code":self.code}})),
        )
            .into_response()
    }
}
type HttpResult<T> = std::result::Result<T, Failure>;
fn lock(state: &ServiceState) -> HttpResult<MutexGuard<'_, PrivacyGate>> {
    state.gate.lock().map_err(|_| Failure {
        code: "service_unavailable",
        status: StatusCode::SERVICE_UNAVAILABLE,
    })
}
fn header_value<'a>(headers: &'a HeaderMap, name: &str, default: &'a str) -> HttpResult<&'a str> {
    if headers.get_all(name).iter().count() > 1 {
        return Err(Failure::bad("duplicate_header"));
    }
    headers.get(name).map_or(Ok(default), |value| {
        value.to_str().map_err(|_| Failure::bad("invalid_header"))
    })
}
fn authenticate(request: &Request, config: &GatewayConfig) -> HttpResult<()> {
    let authorization = header_value(request.headers(), "authorization", "")?;
    let key = header_value(request.headers(), "x-api-key", "")?;
    let supplied = authorization.strip_prefix("Bearer ").unwrap_or(key);
    if supplied.is_empty() || !bool::from(supplied.as_bytes().ct_eq(config.access_token.as_bytes()))
    {
        return Err(Failure {
            code: "unauthorized",
            status: StatusCode::UNAUTHORIZED,
        });
    }
    if request.uri().query().is_some_and(|query| !query.is_empty()) {
        return Err(Failure::bad("query_not_supported"));
    }
    if request.headers().contains_key("upgrade") || request.method() == Method::CONNECT {
        return Err(Failure::bad("tunnel_not_supported"));
    }
    Ok(())
}
async fn body(request: Request, state: &ServiceState) -> HttpResult<Value> {
    let content_type = header_value(request.headers(), "content-type", "")?
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    if content_type != "application/json" {
        return Err(Failure::bad("content_type_not_supported"));
    }
    let encoding =
        header_value(request.headers(), "content-encoding", "identity")?.to_ascii_lowercase();
    let wire_limit = lock(state)?.max_body_bytes();
    let raw = Zeroizing::new(
        to_bytes(request.into_body(), wire_limit)
            .await
            .map_err(|_| Failure {
                code: "request_too_large",
                status: StatusCode::PAYLOAD_TOO_LARGE,
            })?
            .to_vec(),
    );
    let decoded = match encoding.as_str() {
        "identity" => raw,
        "gzip" => {
            let mut decoder = GzDecoder::new(raw.as_slice());
            let mut decoded = Zeroizing::new(Vec::new());
            decoder
                .by_ref()
                .take(state.config.max_decoded_bytes as u64 + 1)
                .read_to_end(&mut decoded)
                .map_err(|_| Failure::bad("compression_invalid"))?;
            if decoded.len() > state.config.max_decoded_bytes
                || !decoder.get_ref().is_empty()
                || decoded.len()
                    > raw
                        .len()
                        .max(1)
                        .saturating_mul(state.config.max_decompression_ratio)
            {
                return Err(Failure::bad("compression_limit"));
            }
            decoded
        }
        _ => return Err(Failure::bad("compression_not_supported")),
    };
    if decoded.len() > state.config.max_decoded_bytes {
        return Err(Failure::bad("request_too_large"));
    }
    let value = json::from_slice(&decoded)?;
    if !value.is_object() {
        return Err(Failure::bad("invalid_request"));
    }
    Ok(value)
}
fn upstream_error(error: reqwest::Error) -> Failure {
    if error.is_timeout() {
        Failure {
            code: "upstream_timeout",
            status: StatusCode::GATEWAY_TIMEOUT,
        }
    } else {
        Failure {
            code: "upstream_unavailable",
            status: StatusCode::BAD_GATEWAY,
        }
    }
}
async fn bounded_response(response: reqwest::Response, limit: usize) -> HttpResult<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(Failure::bad("response_too_large"));
    }
    let mut stream = response.bytes_stream();
    let mut output = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(upstream_error)?;
        if output.len() + chunk.len() > limit {
            return Err(Failure::bad("response_too_large"));
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

async fn handle(State(state): State<ServiceState>, request: Request) -> Response {
    match dispatch(state, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}
async fn dispatch(state: ServiceState, request: Request) -> HttpResult<Response> {
    authenticate(&request, &state.config)?;
    let path = request.uri().path().to_owned();
    match (request.method().as_str(), path.as_str()) {
        ("GET", "/ready") => {
            let gate = lock(&state)?;
            gate.validate()?;
            return Ok(Json(json!({"status":"ready","version":env!("CARGO_PKG_VERSION"),"run_id":gate.run_id()})).into_response());
        }
        ("GET", "/v1/models") => {
            let gate = lock(&state)?;
            gate.validate()?;
            return Ok(Json(json!({"object":"list","data":gate.model_ids().iter().map(|id| json!({"id":id,"object":"model"})).collect::<Vec<_>>()})).into_response());
        }
        ("POST", "/v1/private_compute") => {
            let payload = body(request, &state).await?;
            let gate = lock(&state)?;
            return Ok(Json(gate.compute(&payload)?).into_response());
        }
        (
            "POST",
            "/v1/chat/completions" | "/v1/messages" | "/v1/responses" | "/v1/messages/count_tokens",
        ) => (),
        _ => return Err(Failure::bad("route_not_supported")),
    }
    let api = match path.as_str() {
        "/v1/chat/completions" => Api::Chat,
        "/v1/responses" => Api::Responses,
        _ => Api::Messages,
    };
    if api == Api::Messages
        && header_value(request.headers(), "anthropic-version", "2023-06-01")? != "2023-06-01"
    {
        return Err(Failure::bad("version_not_supported"));
    }
    let payload = body(request, &state).await?;
    let target = state
        .config
        .upstream
        .as_str()
        .trim_end_matches('/')
        .to_owned()
        + &path;
    let approved = {
        let mut gate = lock(&state)?;
        let permit = gate.protect(api, &payload, &target)?;
        gate.consume(permit, &target, "POST")?
    };
    let (target, protected) = approved.into_parts();
    let mut upstream = state
        .client
        .post(&target)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT_ENCODING, "identity");
    if api == Api::Messages {
        upstream = upstream.header("anthropic-version", "2023-06-01");
        if let Some(key) = &state.config.upstream_key {
            upstream = upstream.header("x-api-key", key.as_str());
        }
    } else if let Some(key) = &state.config.upstream_key {
        upstream = upstream.bearer_auth(key.as_str());
    }
    let upstream = upstream
        .body(protected)
        .send()
        .await
        .map_err(upstream_error)?;
    if !upstream.status().is_success() {
        return Err(Failure {
            code: "upstream_status",
            status: StatusCode::BAD_GATEWAY,
        });
    }
    if !header_value(upstream.headers(), "content-encoding", "identity")?
        .eq_ignore_ascii_case("identity")
    {
        return Err(Failure::bad("response_compression_not_supported"));
    }
    let content_type = header_value(upstream.headers(), "content-type", "")?
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    if path == "/v1/messages/count_tokens" {
        if upstream.status() != StatusCode::OK {
            return Err(Failure::bad("count_response_invalid"));
        }
        let raw = Zeroizing::new(bounded_response(upstream, 65_536).await?);
        let value = json::from_slice(&raw)?;
        if value.as_object().is_none_or(|object| object.len() != 1)
            || value
                .get("input_tokens")
                .and_then(Value::as_u64)
                .is_none_or(|n| n >= (1u64 << 53))
        {
            return Err(Failure::bad("count_response_invalid"));
        }
        return Ok(Json(value).into_response());
    }
    if content_type == "text/event-stream" {
        let processor = SseTransformer::new(api, &*lock(&state)?)?;
        let limit = state.config.max_decoded_bytes;
        let output = stream::unfold(
            (upstream.bytes_stream(), processor, state, 0usize, false),
            move |(mut source, mut processor, state, mut seen, finished)| async move {
                if finished {
                    return None;
                }
                let output = match source.next().await {
                    Some(Ok(chunk)) => {
                        seen += chunk.len();
                        if seen > limit {
                            Err(Error::ResourceLimit)
                        } else {
                            match state.gate.lock() {
                                Ok(mut gate) => processor.feed(&chunk, &mut gate),
                                Err(_) => Err(Error::ServiceUnavailable),
                            }
                        }
                    }
                    Some(Err(_)) => Err(Error::ServiceUnavailable),
                    None => {
                        let output = match state.gate.lock() {
                            Ok(gate) => processor.finish(&gate),
                            Err(_) => Err(Error::ServiceUnavailable),
                        };
                        return match output {
                            Ok(output) if output.is_empty() => None,
                            other => Some((
                                other
                                    .map(Bytes::from)
                                    .map_err(|_| std::io::Error::other("stream_blocked")),
                                (source, processor, state, seen, true),
                            )),
                        };
                    }
                };
                let failed = output.is_err();
                Some((
                    output
                        .map(Bytes::from)
                        .map_err(|_| std::io::Error::other("stream_blocked")),
                    (source, processor, state, seen, failed),
                ))
            },
        );
        return Ok((
            [
                (header::CONTENT_TYPE, "text/event-stream"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            Body::from_stream(output),
        )
            .into_response());
    }
    if content_type != "application/json" {
        return Err(Failure::bad("response_type_not_supported"));
    }
    let raw = Zeroizing::new(bounded_response(upstream, state.config.max_decoded_bytes).await?);
    let decoded = json::from_slice(&raw)?;
    let restored = lock(&state)?.restore(api, &decoded)?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(restored)).into_response())
}
