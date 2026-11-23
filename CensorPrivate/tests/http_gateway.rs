use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use censorprivacy::{
    conversations::{agent_session_id, ConversationStore},
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
        6 => return Json(json!({"choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{"id":"read","type":"function","function":{"name":"file_read","arguments":"{}"}}]},"finish_reason":"tool_calls"}]})).into_response(),
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
    setup_with_store(limit, None).await
}

#[tokio::test]
async fn ordinary_value_propagation_is_session_scoped_even_without_recording() {
    let (service, mock, handle) = setup(1_048_576).await;
    for (agent, session, text, expected) in [
        ("dsh", "one", "姓名：代号", "PERSON"),
        ("dsh", "one", "代号", "PERSON"),
        ("dsh", "two", "代号", "plain"),
        ("other", "one", "代号", "plain"),
        ("dsh", "one", "代号", "PERSON"),
        ("dsh", "one", "password=hiddencredential", "SECRET"),
        ("dsh", "two", "hiddencredential", "SECRET"),
        ("dsh", "one", "1.3元", "MONEY"),
        ("dsh", "one", "1.13和1.3哪个大", "plain"),
        ("dsh", "two", "1.13和1.3哪个大", "plain"),
        ("", "", "代号", "plain"),
        ("", "", "姓名：未名", "PERSON"),
        ("", "", "未名", "plain"),
    ] {
        let mut req = request(
            "/v1/chat/completions",
            json!({"model":"configured-model","messages":[{"role":"user","content":text}]}),
        );
        if !agent.is_empty() {
            req.headers_mut()
                .insert("x-censorprivacy-agent", agent.parse().unwrap());
            req.headers_mut()
                .insert("x-censorprivacy-session", session.parse().unwrap());
        }
        let response = service.router().oneshot(req).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{agent}/{session}: {text}"
        );
        let restored = decode(response).await;
        let calls = mock.calls.lock().unwrap();
        let wire = &calls.last().unwrap().2["messages"][0]["content"];
        if expected == "plain" {
            assert_eq!(wire, text, "{agent}/{session}");
        } else {
            assert!(
                wire.as_str().unwrap().contains(&format!("<CP_{expected}_")),
                "{wire}"
            );
        }
        if expected == "SECRET" {
            assert!(!restored.to_string().contains("hiddencredential"));
        } else {
            assert_eq!(restored["choices"][0]["message"]["content"], text);
        }
    }
    handle.abort();
}

#[tokio::test]
async fn xiaoo_preparation_stays_local_and_disabled_tools_are_enforced() {
    let directory = std::env::temp_dir().join(format!(
        "cp-tool-switch-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = Arc::new(ConversationStore::open(directory.clone()).unwrap());
    let (service, mock, handle) = setup_with_store(1_048_576, Some(store.clone())).await;
    let session = "0123456789abcdef0123456789abcdef";
    let body = json!({"model":"configured-model","messages":[{"role":"user","content":"hello"}],
        "tools":[{"type":"function","function":{"name":"file_read","description":"Read a local file","parameters":{"type":"object","properties":{}}}}],"tool_choice":"auto"});
    for missing in [Value::Null, json!([])] {
        let mut warm = body.clone();
        warm["tools"] = missing;
        let response = service
            .router()
            .oneshot(request(
                &format!("/xiaoo/{session}/prepare/v1/chat/completions"),
                warm,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            decode(response).await["error"]["code"],
            "tool_preparation_requires_tools"
        );
    }
    for streaming in [false, true] {
        let mut warm = body.clone();
        warm["stream"] = json!(streaming);
        let response = service
            .router()
            .oneshot(request(
                &format!("/xiaoo/{session}/prepare/v1/chat/completions"),
                warm,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1_048_576).await.unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("READY"));
        assert!(mock.calls.lock().unwrap().is_empty());
        assert!(store.sessions().unwrap().is_empty());
    }
    let disabled = format!("/xiaoo/{session}/no-tools/v1/chat/completions");
    // A tool description which would be rejected if inspected as structural
    // data must never leave the disabled-mode TUI request.
    let mut hidden = body.clone();
    hidden["tools"][0]["function"]["name"] = json!("secret=NeverSendThisToolSecret");
    let response = service
        .router()
        .oneshot(request(&disabled, hidden))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let _ = decode(response).await;
    let wire = mock.calls.lock().unwrap().last().unwrap().2.clone();
    assert!(wire.get("tools").is_none());
    assert!(wire.get("tool_choice").is_none());
    assert!(!wire.to_string().contains("NeverSendThisToolSecret"));
    assert_eq!(store.exchanges(session).unwrap().len(), 1);
    mock.mode.store(6, Ordering::SeqCst);
    let response = service
        .router()
        .oneshot(request(&disabled, body.clone()))
        .await
        .unwrap();
    assert_eq!(decode(response).await["error"]["code"], "tools_disabled");
    let before = mock.calls.lock().unwrap().len();
    let mut history = body.clone();
    history["messages"][0] =
        json!({"role":"tool","tool_call_id":"read","content":"should stay local"});
    let response = service
        .router()
        .oneshot(request(&disabled, history))
        .await
        .unwrap();
    assert_eq!(decode(response).await["error"]["code"], "tools_disabled");
    assert_eq!(mock.calls.lock().unwrap().len(), before);
    // Legacy/enabled routes retain the catalog and existing tool behavior.
    mock.mode.store(0, Ordering::SeqCst);
    let response = service
        .router()
        .oneshot(request(
            &format!("/xiaoo/{session}/v1/chat/completions"),
            body,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let _ = decode(response).await;
    assert!(mock
        .calls
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .2
        .get("tools")
        .is_some());
    handle.abort();
    std::fs::remove_dir_all(directory).unwrap();
}
async fn setup_with_store(
    limit: usize,
    store: Option<Arc<ConversationStore>>,
) -> (GatewayService, Mock, tokio::task::JoinHandle<()>) {
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
    config.conversations = store;
    let service = GatewayService::new(gate, config).unwrap();
    (service, state, handle)
}

#[tokio::test]
async fn xiaoo_records_exact_release_restored_streams_and_isolated_persistent_sessions() {
    use std::os::unix::fs::MetadataExt;
    let directory = std::env::temp_dir().join(format!(
        "cp-conversations-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = Arc::new(ConversationStore::open(directory.clone()).unwrap());
    let (service, mock, handle) = setup_with_store(1_048_576, Some(store.clone())).await;
    let first = "0123456789abcdef0123456789abcdef";
    let second = "fedcba9876543210fedcba9876543210";
    let text = "😀姓名：张三；邮箱：alice@example.com；密码：Demo!2026";
    let body = json!({"model":"configured-model","messages":[{"role":"user","content":text}]});
    let response = service
        .router()
        .oneshot(request(
            &format!("/xiaoo/{first}/v1/chat/completions"),
            body.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let restored = decode(response).await;
    let records = store.exchanges(first).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, "completed");
    assert_eq!(records[0].messages[0]["content"], text);
    assert_eq!(records[0].response, restored["choices"][0]["message"]);
    let snapshot = &records[0].snapshots[0];
    assert_eq!(snapshot.text, text);
    assert_eq!(snapshot.path, "/messages/0/content");
    assert_eq!(
        snapshot.analysis["masked_text"],
        mock.calls.lock().unwrap()[0].2["messages"][0]["content"]
    );
    assert!(!snapshot.analysis["masked_text"]
        .as_str()
        .unwrap()
        .contains("Demo!2026"));
    assert!(!records[0].response.to_string().contains("Demo!2026"));
    let characters: Vec<_> = text.chars().collect();
    assert!(snapshot.analysis["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| {
            characters[s["start"].as_u64().unwrap() as usize..s["end"].as_u64().unwrap() as usize]
                .iter()
                .collect::<String>()
                == "alice@example.com"
        }));
    mock.mode.store(4, Ordering::SeqCst);
    let response = service
        .router()
        .oneshot(request(
            &format!("/xiaoo/{second}/v1/chat/completions"),
            body.clone(),
        ))
        .await
        .unwrap();
    let streamed = to_bytes(response.into_body(), 1_048_576).await.unwrap();
    assert!(String::from_utf8_lossy(&streamed).contains("[DONE]"));
    let records = store.exchanges(second).unwrap();
    assert_eq!(records[0].status, "completed");
    assert_eq!(
        records[0].response["content"],
        restored["choices"][0]["message"]["content"]
    );
    assert_eq!(store.sessions().unwrap().len(), 2);
    // Dropping an unconsumed streaming response must not leave a running badge.
    let response = service
        .router()
        .oneshot(request(
            &format!("/xiaoo/{second}/v1/chat/completions"),
            body.clone(),
        ))
        .await
        .unwrap();
    drop(response);
    let records = store.exchanges(second).unwrap();
    assert_eq!(records.last().unwrap().status, "error");
    assert_eq!(
        records.last().unwrap().error.as_deref(),
        Some("interrupted")
    );
    let before = store.exchanges(first).unwrap().len();
    let response = service
        .router()
        .oneshot(request("/v1/chat/completions", body))
        .await
        .unwrap();
    let _ = to_bytes(response.into_body(), 1_048_576).await.unwrap();
    assert_eq!(
        store.exchanges(first).unwrap().len(),
        before,
        "untagged clients must not be recorded"
    );
    assert!(store.exchanges("../").is_err());
    let reopened = ConversationStore::open(directory.clone()).unwrap();
    assert_eq!(reopened.sessions().unwrap().len(), 2);
    assert_eq!(std::fs::metadata(&directory).unwrap().mode() & 0o777, 0o700);
    let record_file = directory
        .join(first)
        .join(format!("{}.json", store.exchanges(first).unwrap()[0].id));
    assert_eq!(
        std::fs::metadata(record_file).unwrap().mode() & 0o777,
        0o600
    );
    // A process crash cannot run Drop. Startup recovery marks that exchange as
    // interrupted without changing completed history or any masking snapshot.
    let abandoned = store.begin(first, &json!({"model":"configured-model","messages":[{"role":"user","content":"unfinished"}]})).unwrap();
    std::mem::forget(abandoned);
    reopened.recover_interrupted().unwrap();
    let recovered = reopened.exchanges(first).unwrap();
    assert_eq!(recovered.last().unwrap().status, "error");
    assert_eq!(
        recovered.last().unwrap().error.as_deref(),
        Some("gateway_restarted")
    );
    assert_eq!(recovered[0].status, "completed");
    handle.abort();
    std::fs::remove_dir_all(directory).unwrap();
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
async fn agent_conversations_group_headers_and_base_urls_without_leaking_metadata() {
    let directory = std::env::temp_dir().join(format!(
        "cp-agent-conversations-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = Arc::new(ConversationStore::open(directory.clone()).unwrap());
    let (service, mock, handle) = setup_with_store(1_048_576, Some(store.clone())).await;
    let body = json!({"model":"configured-model","messages":[{"role":"user","content":"邮箱：alice@example.com"}]});
    let tagged = |agent: &str, session: &str| {
        let mut req = request("/v1/chat/completions", body.clone());
        req.headers_mut()
            .insert("x-censorprivacy-agent", agent.parse().unwrap());
        req.headers_mut()
            .insert("x-censorprivacy-session", session.parse().unwrap());
        req
    };
    let response = service
        .router()
        .oneshot(tagged("deepseek-harness", "session-one"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let restored = decode(response).await;
    let id = agent_session_id("deepseek-harness", "session-one").unwrap();
    let records = store.exchanges(&id).unwrap();
    assert_eq!(records[0].agent, "deepseek-harness");
    assert_eq!(records[0].response, restored["choices"][0]["message"]);
    assert_eq!(
        records[0].snapshots[0].analysis["masked_text"],
        mock.calls.lock().unwrap()[0].2["messages"][0]["content"]
    );
    mock.mode.store(4, Ordering::SeqCst);
    let response = service
        .router()
        .oneshot(request(
            "/agents/deepseek-harness/session-one/v1/chat/completions",
            body.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let _ = to_bytes(response.into_body(), 1_048_576).await.unwrap();
    assert_eq!(
        store.exchanges(&id).unwrap().len(),
        2,
        "header and Base URL must identify the same session"
    );
    assert_eq!(store.exchanges(&id).unwrap()[1].status, "completed");
    for (agent, session) in [
        ("other-agent", "session-one"),
        ("deepseek-harness", "session-two"),
    ] {
        let response = service
            .router()
            .oneshot(tagged(agent, session))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let _ = to_bytes(response.into_body(), 1_048_576).await.unwrap();
    }
    let sessions = store.sessions().unwrap();
    assert_eq!(
        sessions.len(),
        3,
        "different agents and sessions must remain isolated"
    );
    assert_eq!(
        sessions.iter().find(|s| s["id"] == id).unwrap()["agent"],
        "deepseek-harness"
    );
    for (path, headers, payload) in mock.calls.lock().unwrap().iter() {
        assert_eq!(path, "/v1/chat/completions");
        assert!(!headers.contains_key("x-censorprivacy-agent"));
        assert!(!headers.contains_key("x-censorprivacy-session"));
        assert!(!payload.to_string().contains("session-one"));
        assert!(!payload.to_string().contains("alice@example.com"));
    }
    let before = mock.calls.lock().unwrap().len();
    let mut duplicate = tagged("other-agent", "session-one");
    duplicate
        .headers_mut()
        .append("x-censorprivacy-session", "session-two".parse().unwrap());
    let mut missing = tagged("other-agent", "session-one");
    missing.headers_mut().remove("x-censorprivacy-agent");
    let mut ambiguous = tagged("other-agent", "session-one");
    *ambiguous.uri_mut() = "/agents/other-agent/session-one/v1/chat/completions"
        .parse()
        .unwrap();
    for req in [
        duplicate,
        missing,
        ambiguous,
        tagged("../escape", "session-one"),
        tagged("other-agent", &"a".repeat(129)),
        request("/agents/other-agent/%2e%2e/v1/chat/completions", body),
    ] {
        let response = service.router().oneshot(req).await.unwrap();
        assert_ne!(response.status(), StatusCode::OK);
        assert_eq!(decode(response).await["error"]["code"], "invalid_session");
    }
    assert_eq!(mock.calls.lock().unwrap().len(), before);
    let reopened = ConversationStore::open(directory.clone()).unwrap();
    assert_eq!(
        reopened.exchanges(&id).unwrap()[0].agent,
        "deepseek-harness"
    );
    handle.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn deepseek_gateway_defaults_to_visible_output_without_overriding_explicit_thinking() {
    let (service, mock, handle) = setup(1_048_576).await;
    for model in ["deepseek-flash", "configured-model"] {
        let response = service
            .router()
            .oneshot(request(
                "/v1/chat/completions",
                json!({"model":model,"messages":[{"role":"user","content":"姓名：张三"}],"stream":false}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            decode(response).await["choices"][0]["message"]["content"],
            "姓名：张三"
        );
    }
    {
        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls[0].2["thinking"], json!({"type":"disabled"}));
        assert!(calls[1].2.get("thinking").is_none());
        assert!(!calls[0].2.to_string().contains("张三"));
    }
    for thinking in [json!({"type":"enabled"}), json!(null), json!({})] {
        let response = service
            .router()
            .oneshot(request(
                "/v1/chat/completions",
                json!({"model":"deepseek-flash","messages":[],"thinking":thinking}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert_eq!(mock.calls.lock().unwrap().len(), 2);
    handle.abort();
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
    let tokens = regex::Regex::new(r"<CP_[A-Z][A-Z0-9_]*_[a-z0-9]{8}>").unwrap();
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
        regex::Regex::new(r"<CP_MONEY_[a-z0-9]{8}>")
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
