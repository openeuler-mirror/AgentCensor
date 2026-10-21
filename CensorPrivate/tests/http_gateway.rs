use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use censorprivacy::{
    gate::{GateOptions, PrivacyGate},
    json,
    policy::PrivacyPolicy,
    server::{GatewayConfig, GatewayService},
};
use flate2::{write::GzEncoder, Compression};
use serde_json::{json, Value};
use std::{
    io::Write,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tower::ServiceExt;

const TOKEN: &str = "local-test-token-01234567890123456789";
const KEY: &str = "provider-test-key-01234567890123456789";
#[derive(Clone, Default)]
struct Mock {
    calls: Arc<Mutex<Vec<(String, HeaderMap, Value)>>>,
    mode: Arc<AtomicUsize>,
}
async fn provider(State(state): State<Mock>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let headers = request.headers().clone();
    let body = to_bytes(request.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let body = json::from_slice(&body).unwrap();
    state
        .calls
        .lock()
        .unwrap()
        .push((path.clone(), headers, body.clone()));
    match state.mode.load(Ordering::SeqCst) {
        1 => {
            return (
                StatusCode::TEMPORARY_REDIRECT,
                [("location", "http://127.0.0.1:1/leak")],
                "raw provider error",
            )
                .into_response()
        }
        2 => return Json(json!({"choices":[],"reasoning_content":KEY})).into_response(),
        3 => {
            return (
                [
                    ("content-type", "application/json"),
                    ("content-encoding", "gzip"),
                ],
                "compressed",
            )
                .into_response()
        }
        4 => {
            let content = body["messages"][0]["content"].as_str().unwrap();
            let midpoint = content
                .char_indices()
                .nth(content.chars().count() / 2)
                .unwrap()
                .0;
            let mut wire = String::new();
            for fragment in [&content[..midpoint], &content[midpoint..]] {
                wire += &format!(
                    "data: {}\n\n",
                    json!({"choices":[{"index":0,"delta":{"content":fragment},"finish_reason":null}]})
                );
            }
            wire += &format!(
                "data: {}\n\n",
                json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
            );
            wire += "data: [DONE]\n\n";
            return ([("content-type", "text/event-stream")], wire).into_response();
        }
        5 => {
            return Json(json!({"choices":[{"message":{"content":"x".repeat(8192)}}]}))
                .into_response()
        }
        _ => (),
    }
    if path == "/v1/messages/count_tokens" {
        return Json(json!({"input_tokens":42})).into_response();
    }
    let response = match path.as_str() {
        "/v1/chat/completions" => {
            json!({"choices":[{"message":{"role":"assistant","content":body["messages"][0]["content"]}}],"metadata":{"credential":KEY}})
        }
        "/v1/messages" => {
            json!({"content":[{"type":"text","text":body["messages"][0]["content"]}]})
        }
        "/v1/responses" => {
            json!({"output":[{"type":"message","content":[{"type":"output_text","text":body["input"]}]}]})
        }
        _ => panic!("unexpected route"),
    };
    ([("x-provider-secret", KEY)], Json(response)).into_response()
}
async fn setup(limit: usize) -> (GatewayService, Mock, tokio::task::JoinHandle<()>) {
    let state = Mock::default();
    let app = Router::new().fallback(provider).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let gate = PrivacyGate::new(PrivacyPolicy::default(), GateOptions::default()).unwrap();
    let mut config = GatewayConfig::new(
        &format!("http://{address}"),
        TOKEN.into(),
        Some(KEY.into()),
        true,
    )
    .unwrap();
    config.max_decoded_bytes = limit;
    let service = GatewayService::new(gate, config).unwrap();
    (service, state, handle)
}
fn request(path: &str, body: Value) -> Request {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}
async fn decode(response: Response) -> Value {
    json::from_slice(&to_bytes(response.into_body(), 1_048_576).await.unwrap()).unwrap()
}

#[tokio::test]
async fn real_upstream_only_sees_protected_context_and_service_credentials() {
    let (service, mock, handle) = setup(1_048_576).await;
    for path in ["/v1/chat/completions", "/v1/messages", "/v1/responses"] {
        let body = if path == "/v1/responses" {
            json!({"model":"configured-model","input":"姓名：张三 奖金100元"})
        } else {
            json!({"model":"configured-model","messages":[{"role":"user","content":"姓名：张三 奖金100元"}]})
        };
        let mut req = request(path, body);
        req.headers_mut()
            .insert("x-client-secret", "do-not-forward".parse().unwrap());
        let response = service.router().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get("x-provider-secret").is_none());
        let response = decode(response).await;
        let content = match path {
            "/v1/chat/completions" => &response["choices"][0]["message"]["content"],
            "/v1/messages" => &response["content"][0]["text"],
            _ => &response["output"][0]["content"][0]["text"],
        };
        assert_eq!(content, "姓名：张三 奖金100元");
        assert!(!response.to_string().contains(KEY));
    }
    let calls = mock.calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    let tokens = regex::Regex::new(r"<CP_[A-Z][A-Z0-9_]*_[0-9a-f]{32}>").unwrap();
    for (path, headers, body) in &*calls {
        let encoded = body.to_string();
        assert!(!encoded.contains("张三"));
        let public = tokens.replace_all(&encoded, "REF");
        assert!(!public.contains("100"));
        assert!(headers.get("x-client-secret").is_none());
        if path == "/v1/messages" {
            assert_eq!(headers["x-api-key"], KEY);
            assert_eq!(headers["anthropic-version"], "2023-06-01");
        } else {
            assert_eq!(headers["authorization"], format!("Bearer {KEY}"));
        }
    }
    handle.abort();
}

#[tokio::test]
async fn authentication_unknown_routes_bad_json_and_policy_failures_never_reach_network() {
    let (service, mock, handle) = setup(1_048_576).await;
    let unauthorized = Request::builder()
        .uri("/ready")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        service
            .router()
            .oneshot(unauthorized)
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    for req in [
        request("/unknown", json!({})),
        request("/v1/chat/completions?leak=secret", json!({"messages":[]})),
        request(
            "/v1/chat/completions",
            json!({"model":"a@example.com","messages":[]}),
        ),
        request(
            "/v1/chat/completions",
            json!({"messages":[],"metadata":{"salary":100}}),
        ),
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("x-api-key", TOKEN)
            .header("content-type", "application/json")
            .body(Body::from("{\"messages\":[],\"messages\":[]}"))
            .unwrap(),
    ] {
        assert_eq!(
            service.router().oneshot(req).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert!(mock.calls.lock().unwrap().is_empty());
    let ready = Request::builder()
        .uri("/ready")
        .header("x-api-key", TOKEN)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        decode(service.router().oneshot(ready).await.unwrap()).await["status"],
        "ready"
    );
    handle.abort();
}

#[tokio::test]
async fn bounded_gzip_and_count_tokens_share_the_same_protection() {
    let (service, mock, handle) = setup(1_048_576).await;
    let body = json!({"model":"configured-model","messages":[{"role":"user","content":"姓名：张三 奖金100元"}]}).to_string();
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(body.as_bytes()).unwrap();
    let compressed = encoder.finish().unwrap();
    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages/count_tokens")
        .header("x-api-key", TOKEN)
        .header("content-type", "application/json")
        .header("content-encoding", "gzip")
        .body(Body::from(compressed.clone()))
        .unwrap();
    let response = service.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(decode(response).await, json!({"input_tokens":42}));
    let encoded = mock.calls.lock().unwrap()[0].2.to_string();
    assert!(!encoded.contains("张三"));
    let mut trailing = compressed;
    trailing.extend_from_slice(b"trailing");
    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages/count_tokens")
        .header("x-api-key", TOKEN)
        .header("content-type", "application/json")
        .header("content-encoding", "gzip")
        .body(Body::from(trailing))
        .unwrap();
    assert_eq!(
        service.router().oneshot(req).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(mock.calls.lock().unwrap().len(), 1);
    handle.abort();
}

#[tokio::test]
async fn unsafe_provider_status_compression_opaque_data_and_oversize_have_no_raw_fallback() {
    let (service, mock, handle) = setup(4096).await;
    for mode in [1, 2, 3, 5] {
        mock.mode.store(mode, Ordering::SeqCst);
        let response = service
            .router()
            .oneshot(request(
                "/v1/chat/completions",
                json!({"messages":[{"role":"user","content":"hello"}]}),
            ))
            .await
            .unwrap();
        assert!(!response.status().is_success());
        let response = decode(response).await;
        assert!(response.get("error").is_some());
        assert!(!response.to_string().contains(KEY));
        assert!(!response.to_string().contains("raw provider error"));
    }
    assert_eq!(mock.calls.lock().unwrap().len(), 4); // Redirects are not followed.
    handle.abort();
}

#[tokio::test]
async fn real_sse_restores_before_the_client_receives_it_and_compute_stays_local() {
    let (service, mock, handle) = setup(1_048_576).await;
    mock.mode.store(4, Ordering::SeqCst);
    let response = service
        .router()
        .oneshot(request(
            "/v1/chat/completions",
            json!({"messages":[{"role":"user","content":"姓名：张三 奖金100元"}],"stream":true}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let raw = to_bytes(response.into_body(), 1_048_576).await.unwrap();
    let mut text = String::new();
    for line in std::str::from_utf8(&raw)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|line| *line != "[DONE]")
    {
        let value = json::from_slice(line.as_bytes()).unwrap();
        text.push_str(
            value["choices"][0]["delta"]["content"]
                .as_str()
                .unwrap_or(""),
        );
    }
    assert_eq!(text, "姓名：张三 奖金100元");
    let reference = {
        let calls = mock.calls.lock().unwrap();
        let text = calls[0].2["messages"][0]["content"].as_str().unwrap();
        regex::Regex::new(r"<CP_MONEY_[0-9a-f]{32}>")
            .unwrap()
            .find(text)
            .unwrap()
            .as_str()
            .to_owned()
    };
    let computed = decode(
        service
            .router()
            .oneshot(request(
                "/v1/private_compute",
                json!({"operation":"sum","refs":[reference]}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(computed["result"]
        .as_str()
        .unwrap()
        .starts_with("<CP_MONEY_"));
    assert_eq!(mock.calls.lock().unwrap().len(), 1);
    handle.abort();
}

#[test]
fn configuration_requires_tls_except_explicit_loopback_test_origins() {
    for (url, allow) in [
        ("http://example.com", true),
        ("http://127.0.0.1", false),
        ("https://name:password@example.com", false),
        ("https://example.com?query=secret", false),
        ("file:///etc/passwd", false),
    ] {
        assert!(GatewayConfig::new(url, TOKEN.into(), None, allow).is_err());
    }
    assert!(GatewayConfig::new("https://model.example", TOKEN.into(), None, false).is_ok());
}
