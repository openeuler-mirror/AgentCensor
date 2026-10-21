use censorprivacy::{
    display::DisplayRestorer,
    error::Error,
    gate::{GateOptions, PrivacyGate},
    json,
    policy::PrivacyPolicy,
    protocols::Api,
    response::ResponseRestorer,
    vault::Clock,
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

fn gate() -> PrivacyGate {
    PrivacyGate::new(PrivacyPolicy::default(), GateOptions::default()).unwrap()
}
fn request(content: &str) -> Value {
    json!({"model":"configured-model", "messages":[{"role":"user", "content":content}]})
}

#[test]
fn whole_field_protection_cannot_downgrade_secret_rules() {
    let gate = PrivacyGate::new(
        PrivacyPolicy::default(),
        GateOptions {
            sensitive_paths: vec!["/metadata".into()],
            ..Default::default()
        },
    )
    .unwrap();
    let protected = gate
        .protect_text("customer password=privatecredentialvalue", "/metadata/note")
        .unwrap();
    assert!(protected.starts_with("<CP_SECRET_"));
    assert_eq!(
        gate.restore_display(&protected).unwrap(),
        "[REDACTED_SECRET]"
    );
    assert_eq!(
        gate.redact_known_secrets("privatecredentialvalue").unwrap(),
        "[REDACTED_SECRET]"
    );
}

#[test]
fn complete_context_is_protected_without_changing_control_types() {
    let mut gate = gate();
    let body = json!({
        "model":"configured-model", "temperature":0.5, "max_tokens":123,
        "messages":[{"role":"system","content":"姓名：张三"},
                    {"role":"user","content":"工资10000元"},
                    {"role":"assistant", "tool_calls":[{"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"query\":\"a@example.com\"}"}}]},
                    {"role":"tool","tool_call_id":"call_1","content":"+86 13800138000"}],
        "metadata":{"note":"a@example.com"},
        "tools":[{"type":"function","function":{"name":"lookup","description":"客户：a@example.com","parameters":{"type":"object","properties":{"query":{"type":"string","description":"联系人a@example.com"}}}}}]
    });
    let permit = gate
        .protect(Api::Chat, &body, "https://upstream/v1/chat/completions")
        .unwrap();
    let protected = json::from_slice(permit.body()).unwrap();
    let wire = String::from_utf8(permit.body().to_vec()).unwrap();
    for value in ["张三", "10000", "a@example.com", "13800138000"] {
        assert!(!wire.contains(value));
    }
    assert_eq!(protected["temperature"], 0.5);
    assert_eq!(protected["max_tokens"], 123);
    let approved = gate
        .consume(permit, "https://upstream/v1/chat/completions", "POST")
        .unwrap();
    assert_eq!(approved.body(), wire.as_bytes());
    assert_eq!(approved.target(), "https://upstream/v1/chat/completions");
}

#[test]
fn structural_data_business_numbers_opaque_channels_and_unknown_roots_fail_closed() {
    let mut gate = gate();
    for body in [
        json!({"model":"a@example.com","messages":[]}),
        json!({"messages":[],"unhandled":"a@example.com"}),
        json!({"messages":[{"role":"user","content":[{"type":"image_url","image_url":"https://image"}]}]}),
        json!({"messages":[],"metadata":{"salary":1000}}),
        json!({"messages":[],"logprobs":true}),
        json!({"messages":[],"top_logprobs":2}),
        json!({"messages":[],"temperature":true}),
        json!({"messages":[],"max_tokens":0}),
        json!({"messages":[],"temperature":3}),
        json!({"messages":[],"tools":[{"type":"function","function":{"name":"lookup","parameters":{"type":"object","enum":["a@example.com"]}}}]}),
    ] {
        assert!(gate.protect(Api::Chat, &body, "https://upstream").is_err());
    }
    assert!(gate
        .protect(
            Api::Responses,
            &json!({"input":"hello", "previous_response_id":"remote"}),
            "https://upstream"
        )
        .is_err());
    let body = json!({"messages":[{"role":"user","content":[{"type":"unknown","text":"hello"}]}]});
    assert!(gate
        .protect(Api::Messages, &body, "https://upstream")
        .is_err());
    let duplicate_args = json!({"messages":[{"role":"assistant","tool_calls":[{"function":{"name":"lookup","arguments":"{\"salary\":1,\"salary\":2}"}}]}]});
    assert!(gate
        .protect(Api::Chat, &duplicate_args, "https://upstream")
        .is_err());
}

#[test]
fn explicitly_sensitive_paths_and_mask_all_numbers_preserve_protocol_knobs() {
    let policy = PrivacyPolicy::from_json(r#"{"mask_all_numbers":true}"#).unwrap();
    let options = GateOptions {
        public_identifiers: vec!["model-2026-10-15".into()],
        sensitive_paths: vec!["/metadata/client".into()],
        ..Default::default()
    };
    let mut gate = PrivacyGate::new(policy, options).unwrap();
    let body = json!({"model":"model-2026-10-15", "max_tokens":100,
        "messages":[{"role":"user","content":"hello"}], "metadata":{"client":"ordinary name"},
        "tools":[{"type":"function","function":{"name":"lookup","parameters":{"type":"array","minItems":1,"maxItems":100,"items":{"type":"string"}}}}]});
    let permit = gate.protect(Api::Chat, &body, "https://upstream").unwrap();
    let wire = String::from_utf8(permit.body().to_vec()).unwrap();
    assert!(!wire.contains("ordinary name"));
    let body = json::from_slice(permit.body()).unwrap();
    assert_eq!(body["model"], "model-2026-10-15");
    assert_eq!(body["max_tokens"], 100);
    assert_eq!(body["tools"][0]["function"]["parameters"]["maxItems"], 100);
    let business_control_name = json!({"messages":[{"role":"assistant","tool_calls":[{"function":{"name":"lookup","arguments":"{\"max_tokens\":42}"}}]}]});
    assert!(gate
        .protect(Api::Chat, &business_control_name, "https://upstream")
        .is_err());
}

#[derive(Default)]
struct ManualClock(AtomicU64);
impl Clock for ManualClock {
    fn now(&self) -> Duration {
        Duration::from_secs(self.0.load(Ordering::SeqCst))
    }
}

#[test]
fn release_requires_same_target_method_gate_epoch_and_live_permit() {
    let clock = Arc::new(ManualClock::default());
    let mut gate = PrivacyGate::with_clock(
        PrivacyPolicy::default(),
        GateOptions::default(),
        clock.clone(),
    )
    .unwrap();
    let permit = gate
        .protect(Api::Chat, &request("hello"), "https://upstream")
        .unwrap();
    assert!(matches!(
        gate.consume(permit, "https://attacker", "POST"),
        Err(Error::ReleaseDenied)
    ));
    let permit = gate
        .protect(Api::Chat, &request("hello"), "https://upstream")
        .unwrap();
    assert!(matches!(
        gate.consume(permit, "https://upstream", "GET"),
        Err(Error::ReleaseDenied)
    ));
    let permit = gate
        .protect(Api::Chat, &request("hello"), "https://upstream")
        .unwrap();
    let mut other = self::gate();
    assert!(matches!(
        other.consume(permit, "https://upstream", "POST"),
        Err(Error::ReleaseDenied)
    ));
    let permit = gate
        .protect(Api::Chat, &request("hello"), "https://upstream")
        .unwrap();
    clock.0.store(30, Ordering::SeqCst);
    assert!(matches!(
        gate.consume(permit, "https://upstream", "POST"),
        Err(Error::ReleaseDenied)
    ));
    let permit = gate
        .protect(Api::Chat, &request("hello"), "https://upstream")
        .unwrap();
    gate.reload(PrivacyPolicy::from_json(r#"{"epoch":2}"#).unwrap())
        .unwrap();
    assert!(gate.consume(permit, "https://upstream", "POST").is_err());
}

#[test]
fn display_only_restores_passive_prose_in_all_three_response_families() {
    let mut gate = gate();
    let reference = gate
        .protect_text("姓名：张三", "")
        .unwrap()
        .strip_prefix("姓名：")
        .unwrap()
        .to_owned();
    let chat = gate.restore(Api::Chat, &json!({"id":reference,"choices":[{"message":{"role":"assistant","content":format!("您好 {reference}")}}], "metadata":{"note":reference}})).unwrap();
    assert_eq!(chat["choices"][0]["message"]["content"], "您好 张三");
    assert_eq!(chat["id"], reference);
    assert_eq!(chat["metadata"]["note"], reference);
    let messages = gate
        .restore(
            Api::Messages,
            &json!({"content":[{"type":"text","text":reference}]}),
        )
        .unwrap();
    assert_eq!(messages["content"][0]["text"], "张三");
    let responses = gate.restore(Api::Responses, &json!({"output":[{"type":"message","content":[{"type":"output_text","text":reference}]}]})).unwrap();
    assert_eq!(responses["output"][0]["content"][0]["text"], "张三");
    for text in [
        format!("https://example/{reference}"),
        format!("`{reference}`"),
        format!("[link]({reference})"),
        format!("<a href=\"{reference}\">"),
        format!("<script>{reference}</script>"),
        format!("\x1b]8;;{reference}"),
    ] {
        assert_eq!(gate.restore_display(&text).unwrap(), text);
    }
    let url_reference = gate
        .protect_text("https://a@example.com/path", "/metadata")
        .unwrap();
    // The email inside the URL remains a reference in that URL.
    assert_eq!(gate.restore_display(&url_reference).unwrap(), url_reference);
}

#[test]
fn active_context_and_reference_state_survive_every_unicode_split_point() {
    let gate = gate();
    let reference = gate
        .protect_text("姓名：张三", "")
        .unwrap()
        .strip_prefix("姓名：")
        .unwrap()
        .to_owned();
    let patterns = [
        (format!("🙂 您好 {reference}。"), "🙂 您好 张三。".into()),
        (
            format!("🙂 https://example/{reference}"),
            format!("🙂 https://example/{reference}"),
        ),
        (
            format!("[link]({reference})"),
            format!("[link]({reference})"),
        ),
        (format!("`{reference}`"), format!("`{reference}`")),
    ];
    for (input, expected) in patterns {
        for split in input.char_indices().map(|(i, _)| i).chain([input.len()]) {
            let mut renderer = DisplayRestorer::new();
            let restore = |token: &str| {
                if token == reference {
                    Ok("张三".into())
                } else {
                    Err(Error::VaultDenied)
                }
            };
            let output = renderer.feed(&input[..split], restore).unwrap()
                + &renderer.feed(&input[split..], restore).unwrap()
                + &renderer.finish().unwrap();
            assert_eq!(output, expected, "split {split}");
        }
    }
}

#[test]
fn response_tools_keep_authorized_refs_and_reject_arbitrary_execution() {
    let gate = gate();
    let reference = gate
        .protect_text("奖金100元", "")
        .unwrap()
        .strip_prefix("奖金")
        .unwrap()
        .strip_suffix("元")
        .unwrap()
        .to_owned();
    let args = json!({"operation":"sum","refs":[reference]});
    assert_eq!(gate.tool_arguments("private_compute", &args).unwrap(), args);
    assert!(gate
        .tool_arguments("python", &json!({"code":reference}))
        .is_err());
    let approved = gate
        .tool_arguments(
            "shell",
            &json!({"command":format!("privcalc sum '{reference}'")}),
        )
        .unwrap();
    assert!(approved["command"].as_str().unwrap().contains(&reference));
    for command in [
        format!("echo '{reference}'"),
        format!("privcalc sum '{reference}'; id"),
        format!("privcalc sum '{reference}' $(id)"),
        format!("privcalc sum '{reference}' > /tmp/out"),
    ] {
        assert!(gate
            .tool_arguments("shell", &json!({"command":command}))
            .is_err());
    }
    assert_eq!(
        gate.tool_arguments("ordinary", &json!({"query":"hello"}))
            .unwrap(),
        json!({"query":"hello"})
    );
}

#[test]
fn secrets_are_redacted_in_response_keys_metadata_and_nested_tool_json() {
    let mut gate = gate();
    gate.register_transport_secret("sample-credential").unwrap();
    let response = json!({"choices":[{"message":{"content":"sample-credential", "tool_calls":[{"function":{"name":"lookup","arguments":"{\"value\":\"sample-credential\"}"}}]}}], "metadata":{"sample-credential":"sample-credential"}});
    let restored = gate.restore(Api::Chat, &response).unwrap();
    let wire = serde_json::to_string(&restored).unwrap();
    assert!(!wire.contains("sample-credential"));
    assert_eq!(
        restored["metadata"]["[REDACTED_SECRET]"],
        "[REDACTED_SECRET]"
    );
    let collision = json!({"choices":[],"metadata":{"sample-credential":1,"[REDACTED_SECRET]":2}});
    assert!(gate.restore(Api::Chat, &collision).is_err());
}

#[test]
fn multipart_reference_boundaries_stay_masked_and_unknown_response_channels_fail() {
    let mut gate = gate();
    let reference = gate
        .protect_text("姓名：张三", "")
        .unwrap()
        .strip_prefix("姓名：")
        .unwrap()
        .to_owned();
    let parts = json!({"choices":[{"message":{"content":[{"type":"text","text":&reference[..10]},{"type":"text","text":&reference[10..]}]}}]});
    assert_eq!(gate.restore(Api::Chat, &parts).unwrap(), parts);
    for response in [
        json!({"choices":[],"logprobs":["sample"]}),
        json!({"choices":[],"reasoning_content":"private"}),
        json!({"choices":[],"opaque":{"type":"reasoning"}}),
    ] {
        assert!(gate.restore(Api::Chat, &response).is_err());
    }
}
