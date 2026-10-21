use censorprivacy::{
    gate::{GateOptions, PrivacyGate},
    policy::PrivacyPolicy,
    protocols::Api,
    tls_broker::{decode_frame, TlsBroker, DEFER, REPLACE},
    tls_privacy::LengthPreservingTransformer,
};
use serde_json::{json, Value};
fn gate() -> PrivacyGate {
    PrivacyGate::new(
        PrivacyPolicy::from_json(
            r#"{"mask_all_numbers":true,"dictionaries":{"PERSON":["张三","Alice","𝄞Alice"]}}"#,
        )
        .unwrap(),
        GateOptions::default(),
    )
    .unwrap()
}
fn request(text: &str) -> Vec<u8> {
    serde_json::to_vec(
        &json!({"model":"configured-model","messages":[{"role":"user","content":text}]}),
    )
    .unwrap()
}
fn reply(text: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"choices":[{"message":{"role":"assistant","content":text}}]}))
        .unwrap()
}
fn content(raw: &[u8]) -> String {
    serde_json::from_slice::<Value>(raw).unwrap()["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn display(raw: &[u8]) -> String {
    serde_json::from_slice::<Value>(raw).unwrap()["choices"][0]["message"]["content"]
        .as_str()
        .unwrap()
        .to_owned()
}
#[test]
fn exact_wire_unicode_escapes_surrogate_pairs_and_short_lossy_masks() {
    let mut gate = gate();
    let mut transformer = LengthPreservingTransformer::new(&gate).unwrap();
    for (wire, original) in [
        (r"\u5f20\u4e09", "张三"),
        (r"\ud834\udd1eAlice", "𝄞Alice"),
        ("张三", "张三"),
        ("Alice", "Alice"),
    ] {
        let raw = format!(
            r#"{{ "model":"configured-model", "messages":[{{"role":"user","content":"{wire}"}}] }}"#
        )
        .into_bytes();
        let safe = transformer.protect_json(&gate, Api::Chat, &raw).unwrap();
        assert_eq!(safe.len(), raw.len());
        let alias = content(&safe);
        assert_ne!(alias, original);
        let response = reply(&alias);
        let restored = transformer
            .restore_json(&mut gate, Api::Chat, &response)
            .unwrap();
        assert_eq!(restored.len(), response.len());
        assert_eq!(display(&restored), original);
        assert!(transformer.resolve_alias(&gate, &alias).is_ok());
    }
    let safe = transformer
        .protect_json(&gate, Api::Chat, &request("1 22"))
        .unwrap();
    assert_eq!(content(&safe), "* **");
    assert!(transformer.resolve_alias(&gate, "**").is_err());
}
#[test]
fn restore_only_passive_display_and_never_credentials_or_tool_arguments() {
    let mut gate = gate();
    let mut transformer = LengthPreservingTransformer::new(&gate).unwrap();
    let alias = content(
        &transformer
            .protect_json(&gate, Api::Chat, &request("Alice"))
            .unwrap(),
    );
    for text in [
        format!("https://example.test/{alias}"),
        format!("`{alias}`"),
        format!("[x]({alias})"),
        format!("<script>{alias}</script>"),
        format!("\x1b[31m{alias}"),
    ] {
        assert_eq!(
            display(
                &transformer
                    .restore_json(&mut gate, Api::Chat, &reply(&text))
                    .unwrap()
            ),
            text
        );
    }
    let body=serde_json::to_vec(&json!({"choices":[{"message":{"tool_calls":[{"type":"function","function":{"name":"shell","arguments":format!("{{\"command\":\"{alias}\"}}")}}]}}]})).unwrap();
    assert_eq!(
        transformer
            .restore_json(&mut gate, Api::Chat, &body)
            .unwrap(),
        body
    );
    gate.register_transport_secret("Alice").unwrap();
    assert!(transformer.resolve_alias(&gate, &alias).is_err());
    assert_eq!(
        display(
            &transformer
                .restore_json(&mut gate, Api::Chat, &reply(&alias))
                .unwrap()
        ),
        alias
    );
    assert_eq!(
        display(
            &transformer
                .restore_json(&mut gate, Api::Chat, &reply("Alice"))
                .unwrap()
        ),
        "*****"
    );
}
#[test]
fn nested_tool_json_projection_and_long_compute_result_padding() {
    let mut gate = gate();
    let mut transformer = LengthPreservingTransformer::new(&gate).unwrap();
    let raw=br#"{"model":"configured-model","messages":[{"role":"assistant","tool_calls":[{"id":"call","type":"function","function":{"name":"private_compute","arguments":"{\"value\":\"\u5f20\u4e09\"}"}}]}]}"#;
    let protected = transformer.protect_json(&gate, Api::Chat, raw).unwrap();
    assert_eq!(protected.len(), raw.len());
    assert!(!String::from_utf8_lossy(&protected).contains("5f20"));
    let alias = content(
        &transformer
            .protect_json(&gate, Api::Chat, &request("100.00"))
            .unwrap(),
    );
    let reference = transformer.resolve_alias(&gate, &alias).unwrap();
    let result = gate
        .compute(&json!({"operation":"sum","refs":[reference]}))
        .unwrap();
    let token = serde_json::to_value(result).unwrap()["result"]
        .as_str()
        .unwrap()
        .to_owned();
    let response = reply(&token);
    let restored = transformer
        .restore_json(&mut gate, Api::Chat, &response)
        .unwrap();
    assert_eq!(restored.len(), response.len());
    assert_eq!(display(&restored).trim_end(), "100.00");
}
#[test]
fn strict_structure_numbers_epoch_and_secret_key_collision_fail_closed() {
    let mut gate = gate();
    let mut transformer = LengthPreservingTransformer::new(&gate).unwrap();
    for body in [br#"{"model":"configured-model","messages":[{"role":"user","content":"Alice"}],"metadata":{"x":3}}"#.as_slice(),br#"{"model":"configured-model","model":"x","messages":[]}"#,br#"{"model":"Alice","messages":[]}"#] {assert!(transformer.protect_json(&gate,Api::Chat,body).is_err());}
    gate.register_transport_secret("abc").unwrap();
    gate.register_transport_secret("def").unwrap();
    assert!(transformer
        .restore_json(&mut gate, Api::Chat, br#"{"choices":[],"abc":1,"def":2}"#)
        .is_err());
    gate.reload(PrivacyPolicy::from_json(r#"{"epoch":2}"#).unwrap())
        .unwrap();
    assert!(transformer
        .protect_json(&gate, Api::Chat, &request("Alice"))
        .is_err());
}
fn http(body: &[u8], response: bool) -> Vec<u8> {
    let head = if response {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
    } else {
        format!("POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1:443\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",body.len())
    };
    [head.as_bytes(), body].concat()
}
#[test]
fn broker_withholds_every_fragment_and_enforces_single_transaction() {
    for width in [1, 2, 3, 7, 31, 4096] {
        let mut gate = gate();
        let mut broker = TlsBroker::new(&gate, "https://127.0.0.1", None).unwrap();
        let raw = http(&request("Alice"), false);
        let mut output = Vec::new();
        let chunks = raw.chunks(width).collect::<Vec<_>>();
        for (index, chunk) in chunks.iter().enumerate() {
            let (action, bytes) = broker.decide(&mut gate, 42, chunk, 1).unwrap();
            if index + 1 == chunks.len() {
                assert_eq!(action, REPLACE);
                output = bytes;
            } else {
                assert_eq!(action, DEFER);
                assert!(bytes.is_empty());
            }
        }
        let boundary = output.windows(4).position(|b| b == b"\r\n\r\n").unwrap() + 4;
        let alias = content(&output[boundary..]);
        let response = http(&reply(&alias), true);
        let chunks = response.chunks(width).collect::<Vec<_>>();
        for (index, chunk) in chunks.iter().enumerate() {
            let (action, bytes) = broker.decide(&mut gate, 42, chunk, 2).unwrap();
            if index + 1 == chunks.len() {
                assert_eq!(action, REPLACE);
                let boundary = bytes.windows(4).position(|b| b == b"\r\n\r\n").unwrap() + 4;
                assert_eq!(display(&bytes[boundary..]), "Alice");
            } else {
                assert_eq!(action, DEFER);
            }
        }
        assert!(broker.decide(&mut gate, 42, &response, 2).is_err());
    }
}
#[test]
fn broker_rejects_smuggling_credentials_bad_frame_and_pipeline() {
    let mut gate = gate();
    let mut broker = TlsBroker::new(&gate, "https://127.0.0.1", Some("service-key")).unwrap();
    let raw = http(&request("Alice"), false);
    assert!(broker
        .decide(&mut gate, 1, &[raw.clone(), raw.clone()].concat(), 1)
        .is_err());
    for header in [
        "Transfer-Encoding: chunked",
        "Content-Length: 1",
        "Authorization: Bearer wrong",
        "X-Data: Alice",
    ] {
        let boundary = raw.windows(4).position(|b| b == b"\r\n\r\n").unwrap();
        let changed = [
            &raw[..boundary],
            format!("\r\n{header}").as_bytes(),
            &raw[boundary..],
        ]
        .concat();
        assert!(broker.decide(&mut gate, 2, &changed, 1).is_err());
    }
    assert!(decode_frame(b"bad", 1, 100).is_err());
    broker.decide(&mut gate, 3, &raw, 1).unwrap();
    assert!(broker.decide(&mut gate, 3, &raw, 1).is_err());
}

#[test]
fn complete_large_wire_messages_are_detected_without_skipping_required_rules() {
    let mut gate = gate();
    let mut transformer = LengthPreservingTransformer::new(&gate).unwrap();
    let text = format!("Alice 100.00 {}", "x".repeat(200000));
    let raw = request(&text);
    let safe = transformer.protect_json(&gate, Api::Chat, &raw).unwrap();
    assert_eq!(raw.len(), safe.len());
    assert!(!content(&safe).contains("Alice"));
    let response = reply(&content(&safe));
    let restored = transformer
        .restore_json(&mut gate, Api::Chat, &response)
        .unwrap();
    assert_eq!(display(&restored), text);
    assert_eq!(restored.len(), response.len());
}
