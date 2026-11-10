//! Real offline BERT plus actual HTTP forwarding and the shared TLS transformer.
use axum::{body::to_bytes, extract::Request, response::IntoResponse, Json, Router};
use censorprivacy::{
    gate::{GateOptions, PrivacyGate},
    policy::PrivacyPolicy,
    professional::{AnalyzeOptions, ProfessionalConfig, ProfessionalDetector},
    protocols::Api,
    server::{GatewayConfig, GatewayService},
    tls_privacy::LengthPreservingTransformer,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const TOKEN: &str = "professional-local-test-token-01234567890123456789";

#[test]
#[ignore = "requires local ONNX Runtime and bert4ner-base-chinese ONNX bundle"]
fn committed_reference_matches_real_onnx_detector() {
    let detector = ProfessionalDetector::start(ProfessionalConfig::default()).unwrap();
    for (index, line) in include_str!("fixtures/native-detector-reference.jsonl")
        .lines()
        .enumerate()
    {
        let mut row: Value = serde_json::from_str(line).unwrap();
        let text = row["request"]["text"].as_str().unwrap().to_owned();
        row["request"].as_object_mut().unwrap().remove("text");
        let options: AnalyzeOptions = serde_json::from_value(row["request"].clone()).unwrap();
        let results = detector.analyze(&text, &options).unwrap();
        let mut actual = serde_json::to_value(results).unwrap();
        let expected = &mut row["results"];
        assert_eq!(
            actual.as_array().unwrap().len(),
            expected.as_array().unwrap().len(),
            "sample {index}"
        );
        for (a, b) in actual
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .zip(expected.as_array_mut().unwrap())
        {
            assert!(
                (a["score"].as_f64().unwrap() - b["score"].as_f64().unwrap()).abs() <= 2e-5,
                "sample {index}"
            );
            a.as_object_mut().unwrap().remove("score");
            b.as_object_mut().unwrap().remove("score");
            for item in [&mut *a, &mut *b] {
                for key in ["sources", "evidence"] {
                    item[key]
                        .as_array_mut()
                        .unwrap()
                        .sort_by_key(|v| v.as_str().unwrap().to_owned());
                }
            }
            assert_eq!(a, b, "sample {index}");
        }
    }
}

#[test]
#[ignore = "requires local ONNX Runtime and bert4ner-base-chinese ONNX bundle"]
fn private_compute_operation_stays_structural_across_model_turns() {
    let mut gate = PrivacyGate::with_professional_detector(
        PrivacyPolicy::default(),
        GateOptions::default(),
        ProfessionalConfig::default(),
    )
    .unwrap();
    let target = "/v1/chat/completions";
    let mut body = json!({"model":"configured-model","messages":[{"role":"user","content":"amountA=100.00元; amountB=200.00元"}],
        "tools":[{"type":"function","function":{"name":"private_compute","parameters":{"type":"object","properties":{"operation":{"type":"string","enum":["sum","mean","min","max","aggregate","date_diff"]}}}}}]});
    let permit = gate.protect(Api::Chat, &body, target).unwrap();
    let approved = gate.consume(permit, target, "POST").unwrap();
    let protected: Value = serde_json::from_slice(approved.body()).unwrap();
    let pattern = regex::Regex::new(r"<CP_MONEY_[0-9a-f]{32}>").unwrap();
    let refs: Vec<_> = pattern
        .find_iter(protected["messages"][0]["content"].as_str().unwrap())
        .map(|m| m.as_str().to_owned())
        .collect();
    assert_eq!(refs.len(), 2);
    let arguments = json!({"operation":"sum","refs":refs});
    let response = json!({"choices":[{"message":{"role":"assistant","content":"","tool_calls":[{"id":"call_00_test","index":0,"type":"function","function":{"name":"private_compute","arguments":arguments.to_string()}}]},"finish_reason":"tool_calls"}]});
    let restored = gate.restore(Api::Chat, &response).unwrap();
    let computed = gate.compute(&arguments).unwrap();
    let mut assistant = restored["choices"][0]["message"].clone();
    assistant["tool_calls"][0]
        .as_object_mut()
        .unwrap()
        .remove("index");
    body["messages"].as_array_mut().unwrap().push(assistant);
    body["messages"].as_array_mut().unwrap().push(json!({"role":"tool","tool_call_id":"call_00_test","content":serde_json::to_string(&computed).unwrap()}));
    let permit = gate.protect(Api::Chat, &body, target).unwrap();
    let approved = gate.consume(permit, target, "POST").unwrap();
    let protected: Value = serde_json::from_slice(approved.body()).unwrap();
    let args: Value = serde_json::from_str(
        protected["messages"][1]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(args, arguments);
    assert_eq!(protected["tools"], body["tools"]);
    let final_reply = json!({"choices":[{"message":{"role":"assistant","content":computed.result.unwrap().to_string()}}]});
    assert_eq!(
        gate.restore(Api::Chat, &final_reply).unwrap()["choices"][0]["message"]["content"],
        "300.00"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires local ONNX Runtime and bert4ner-base-chinese ONNX bundle; scripts/test/detector.sh --integration"]
async fn real_bert_gateway_tls_validation_restore_compute_and_reload() {
    let mut gate = PrivacyGate::with_professional_detector(
        PrivacyPolicy::default(),
        GateOptions::default(),
        ProfessionalConfig::default(),
    )
    .unwrap();
    // Unicode codepoint offsets from BERT must become valid Rust byte ranges.
    let text = "😀张三在清华大学工作，邮箱alice@example.com，工资100元。";
    let safe = gate.protect_text(text, "").unwrap();
    assert!(safe.contains("<CP_PERSON_"));
    assert!(safe.contains("<CP_ORG_"));
    assert!(safe.contains("<CP_EMAIL_"));
    assert!(safe.contains("<CP_MONEY_"));
    for value in ["张三", "清华大学", "alice@example.com"] {
        assert!(!safe.contains(value));
    }
    assert_eq!(gate.restore_display(&safe).unwrap(), text);
    let money = regex::Regex::new(r"<CP_MONEY_[0-9a-f]{32}>")
        .unwrap()
        .find(&safe)
        .unwrap()
        .as_str();
    let result = gate
        .compute(&json!({"operation":"sum","refs":[money,money]}))
        .unwrap();
    assert_eq!(
        gate.restore_display(&result.result.unwrap().to_string())
            .unwrap(),
        "200"
    );
    let secret = gate.protect_text("easyvalue", "/password").unwrap();
    assert!(secret.starts_with("<CP_SECRET_"));
    assert!(!gate.restore_display(&secret).unwrap().contains("easyvalue"));
    let body = json!({"model":"configured-model","messages":[{"role":"assistant","tool_calls":[{
        "id":"call_1","type":"function","function":{"name":"submit","arguments":"{\"password\":\"nested-value\"}"}}]}]});
    let permit = gate
        .protect(Api::Chat, &body, "/v1/chat/completions")
        .unwrap();
    let approved = gate
        .consume(permit, "/v1/chat/completions", "POST")
        .unwrap();
    assert!(!std::str::from_utf8(approved.body())
        .unwrap()
        .contains("nested-value"));
    // A model fragment beyond the first 512-token window must still be detected.
    let long = format!("{}张三在清华大学工作。", "普通测试文本。".repeat(100));
    let protected = gate.protect_text(&long, "").unwrap();
    assert!(!protected.contains("张三"));
    assert!(!protected.contains("清华大学"));
    // Same detector, different TLS aliases: byte counts and restoration survive.
    let raw = serde_json::to_vec(
        &json!({"model":"configured-model","messages":[{"role":"user","content":text}]}),
    )
    .unwrap();
    let mut tls = LengthPreservingTransformer::new(&gate).unwrap();
    let wire = tls.protect_json(&gate, Api::Chat, &raw).unwrap();
    assert_eq!(wire.len(), raw.len());
    let wire: Value = serde_json::from_slice(&wire).unwrap();
    let reply = serde_json::to_vec(&json!({"choices":[{"message":{"role":"assistant","content":wire["messages"][0]["content"]}}]})).unwrap();
    let restored = tls.restore_json(&mut gate, Api::Chat, &reply).unwrap();
    assert_eq!(reply.len(), restored.len());
    let restored: Value = serde_json::from_slice(&restored).unwrap();
    assert_eq!(restored["choices"][0]["message"]["content"], text);
    // Shipped TLS demo includes inline secret context and completion metadata.
    gate.reload(PrivacyPolicy::from_json(r#"{"epoch":2,"dictionaries":{"CUSTOMER":["DemoCustomerOrchid"],"SECRET":["DEMO_SECRET_DO_NOT_SEND"]}}"#).unwrap()).unwrap();
    let demo = "Customer=DemoCustomerOrchid; amountA=100.00元; amountB=200.00元; secret=DEMO_SECRET_DO_NOT_SEND.";
    let mut transformer = LengthPreservingTransformer::new(&gate).unwrap();
    let raw = serde_json::to_vec(&json!({"model":"configured-model","messages":[{"role":"user","content":demo}],"stream":false})).unwrap();
    let wire = transformer.protect_json(&gate, Api::Chat, &raw).unwrap();
    let body: Value = serde_json::from_slice(&wire).unwrap();
    let reply=serde_json::to_vec(&json!({"id":"cp-demo-response","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":body["messages"][0]["content"]},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}})).unwrap();
    let restored = transformer
        .restore_json(&mut gate, Api::Chat, &reply)
        .expect("TLS demo completion restores");
    assert_eq!(reply.len(), restored.len());
    gate.reload(PrivacyPolicy::from_json(r#"{"epoch":3,"mask_money":false,"mask_dates":false,"dictionaries":{"ORG":["custom-company"]}}"#).unwrap()).unwrap();
    assert_eq!(gate.protect_text("42元", "").unwrap(), "42元");
    assert!(!gate
        .protect_text("custom-company 张三", "")
        .unwrap()
        .contains("custom-company"));

    let capture = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = capture.clone();
    let app=Router::new().fallback(move |request:Request| {
        let seen=seen.clone();
        async move {
            let path=request.uri().path().to_owned();
            let raw=to_bytes(request.into_body(),1_048_576).await.unwrap();
            let body:Value=serde_json::from_slice(&raw).unwrap();
            seen.lock().unwrap().push(body.clone());
            let text=if path=="/v1/responses" {body["input"].as_str().unwrap()} else {body["messages"][0]["content"].as_str().unwrap()};
            if body["stream"]==true {
                let split=text.find("<CP_").unwrap()+12;
                let mut wire=String::new();
                for fragment in [&text[..split],&text[split..]] {
                    wire+=&format!("data: {}\n\n",json!({"choices":[{"index":0,"delta":{"content":fragment},"finish_reason":null}]}));
                }
                wire+=&format!("data: {}\n\ndata: [DONE]\n\n",json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}));
                return ([("content-type","text/event-stream")],wire).into_response();
            }
            let reply=match path.as_str() {
                "/v1/responses"=>json!({"output":[{"type":"message","content":[{"type":"output_text","text":text}]}]}),
                "/v1/messages"=>json!({"content":[{"type":"text","text":text}]}),
                _=>json!({"choices":[{"message":{"role":"assistant","content":text}}]})
            };
            Json(reply).into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = listener.local_addr().unwrap();
    let provider = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let service = Arc::new(
        GatewayService::new(
            gate,
            GatewayConfig::new(&format!("http://{origin}"), TOKEN.into(), None, true).unwrap(),
        )
        .unwrap(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = service.router();
    let gateway = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let text = "张三在清华大学工作，邮箱alice@example.com。密码：easyvalue";
    for path in ["/v1/chat/completions", "/v1/messages", "/v1/responses"] {
        let body = if path == "/v1/responses" {
            json!({"model":"configured-model","input":text})
        } else {
            json!({"model":"configured-model","messages":[{"role":"user","content":text}]})
        };
        let response = client
            .post(format!("http://{address}{path}"))
            .bearer_auth(TOKEN)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let response: Value = response.json().await.unwrap();
        let content = match path {
            "/v1/responses" => &response["output"][0]["content"][0]["text"],
            "/v1/messages" => &response["content"][0]["text"],
            _ => &response["choices"][0]["message"]["content"],
        };
        let content = content.as_str().unwrap();
        assert!(content.contains("张三在清华大学"));
        assert!(!content.contains("easyvalue"));
    }
    let response=client.post(format!("http://{address}/v1/chat/completions")).bearer_auth(TOKEN).json(&json!({"model":"configured-model","stream":true,"messages":[{"role":"user","content":text}]})).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let stream = response.text().await.unwrap();
    assert!(stream.contains("张三"));
    assert!(!stream.contains("easyvalue"));
    assert!(stream.contains("[DONE]"));
    let calls = capture.lock().unwrap();
    assert_eq!(calls.len(), 4);
    for call in calls.iter() {
        let wire = call.to_string();
        for private in ["张三", "清华大学", "alice@example.com", "easyvalue"] {
            assert!(!wire.contains(private));
        }
        assert!(wire.contains("<CP_"));
    }
    drop(calls);
    service.close().unwrap();
    gateway.abort();
    provider.abort();
}
