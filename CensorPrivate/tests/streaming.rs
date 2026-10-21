use censorprivacy::{
    error::Error,
    gate::{GateOptions, PrivacyGate},
    json,
    policy::PrivacyPolicy,
    protocols::Api,
    streaming::SseTransformer,
};
use serde_json::{json, Value};

fn gate() -> PrivacyGate {
    PrivacyGate::new(PrivacyPolicy::default(), GateOptions::default()).unwrap()
}
fn event(value: &Value) -> String {
    format!("data: {value}\n\n")
}
fn events(raw: &[u8]) -> Vec<Value> {
    std::str::from_utf8(raw)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|line| *line != "[DONE]")
        .map(|line| json::from_slice(line.as_bytes()).unwrap())
        .collect()
}
fn text(api: Api, raw: &[u8]) -> String {
    events(raw)
        .iter()
        .filter_map(|value| match api {
            Api::Chat => value["choices"][0]["delta"]["content"].as_str(),
            Api::Messages => {
                if value["type"] == "content_block_delta" {
                    value["delta"]["text"].as_str()
                } else {
                    value["content_block"]["text"].as_str()
                }
            }
            Api::Responses => {
                if value["type"] == "response.output_text.delta" {
                    value["delta"].as_str()
                } else {
                    None
                }
            }
        })
        .collect()
}

#[test]
fn all_families_restore_tokens_and_unicode_across_arbitrary_network_bytes() {
    for api in [Api::Chat, Api::Messages, Api::Responses] {
        let mut gate = gate();
        let reference = gate
            .protect_text("姓名：张三", "")
            .unwrap()
            .strip_prefix("姓名：")
            .unwrap()
            .to_owned();
        let content = format!("🙂 您好 {reference}。");
        let fragments = [&content[..12], &content[12..25], &content[25..]];
        let wire = match api {
            Api::Chat => fragments.iter().map(|fragment| event(&json!({"choices":[{"index":0,"delta":{"content":fragment},"finish_reason":null}]}))).collect::<String>()
                + &event(&json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})) + "data: [DONE]\n\n",
            Api::Messages => event(&json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}))
                + &fragments.iter().map(|fragment| event(&json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":fragment}}))).collect::<String>()
                + &event(&json!({"type":"content_block_stop","index":0})) + &event(&json!({"type":"message_stop"})),
            Api::Responses => fragments.iter().map(|fragment| event(&json!({"type":"response.output_text.delta","item_id":"item","content_index":0,"delta":fragment}))).collect::<String>()
                + &event(&json!({"type":"response.output_text.done","item_id":"item","content_index":0,"text":content})) + &event(&json!({"type":"response.completed","response":{"output":[]}})),
        };
        for width in [1, 2, 3, 7, 31, 4096] {
            let mut processor = SseTransformer::new(api, &gate).unwrap();
            let mut output = Vec::new();
            for fragment in wire.as_bytes().chunks(width) {
                output.extend(processor.feed(fragment, &mut gate).unwrap());
            }
            output.extend(processor.finish(&gate).unwrap());
            assert_eq!(
                text(api, &output),
                "🙂 您好 张三。",
                "{api:?} width {width}"
            );
        }
    }
}

#[test]
fn raw_secrets_and_overlapping_matches_cannot_leak_across_sse_events() {
    let mut gate = gate();
    gate.register_transport_secret("abcab").unwrap();
    gate.register_transport_secret("张三密钥").unwrap();
    let mut processor = SseTransformer::new(Api::Chat, &gate).unwrap();
    let mut output = Vec::new();
    for fragment in ["start ", "abc", "abc", "ab", " 张三", "密", "钥 end"] {
        output.extend(
            processor
                .feed(
                    event(&json!({"choices":[{"index":0,"delta":{"content":fragment}}]}))
                        .as_bytes(),
                    &mut gate,
                )
                .unwrap(),
        );
    }
    output.extend(
        processor
            .feed(
                event(&json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}))
                    .as_bytes(),
                &mut gate,
            )
            .unwrap(),
    );
    processor.finish(&gate).unwrap();
    assert_eq!(
        text(Api::Chat, &output),
        "start [REDACTED_SECRET] [REDACTED_SECRET] end"
    );
}

#[test]
fn fragmented_tool_arguments_and_names_are_released_only_after_authorization() {
    let mut gate = gate();
    let reference = gate
        .protect_text("奖金100元", "")
        .unwrap()
        .strip_prefix("奖金")
        .unwrap()
        .strip_suffix("元")
        .unwrap()
        .to_owned();
    let args = json!({"operation":"sum","refs":[reference]}).to_string();
    let mut processor = SseTransformer::new(Api::Chat, &gate).unwrap();
    for (name, arguments) in [("private_", &args[..15]), ("compute", &args[15..])] {
        let partial = event(
            &json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":name,"arguments":arguments}}]}}]}),
        );
        let output = processor.feed(partial.as_bytes(), &mut gate).unwrap();
        assert!(!String::from_utf8(output).unwrap().contains("private_"));
    }
    let complete = processor
        .feed(
            event(&json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}))
                .as_bytes(),
            &mut gate,
        )
        .unwrap();
    let complete = events(&complete);
    let function = &complete[0]["choices"][0]["delta"]["tool_calls"][0]["function"];
    assert_eq!(function["name"], "private_compute");
    assert_eq!(
        json::from_slice(function["arguments"].as_str().unwrap().as_bytes()).unwrap(),
        json::from_slice(args.as_bytes()).unwrap()
    );
    processor.finish(&gate).unwrap();
    let mut rejected = SseTransformer::new(Api::Chat, &gate).unwrap();
    let wire = event(
        &json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"python","arguments":json!({"code":reference}).to_string()}}]}}]}),
    );
    rejected.feed(wire.as_bytes(), &mut gate).unwrap();
    assert!(rejected
        .feed(
            event(&json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}))
                .as_bytes(),
            &mut gate
        )
        .is_err());
}

#[test]
fn url_context_and_choice_channels_stay_separate() {
    let mut gate = gate();
    let reference = gate
        .protect_text("姓名：张三", "")
        .unwrap()
        .strip_prefix("姓名：")
        .unwrap()
        .to_owned();
    let mut processor = SseTransformer::new(Api::Chat, &gate).unwrap();
    let first = processor.feed(event(&json!({"choices":[{"index":0,"delta":{"content":"https://example/"}},{"index":1,"delta":{"content":"你好 "}}]})).as_bytes(), &mut gate).unwrap();
    let second = processor.feed(event(&json!({"choices":[{"index":0,"delta":{"content":reference},"finish_reason":"stop"},{"index":1,"delta":{"content":reference},"finish_reason":"stop"}]})).as_bytes(), &mut gate).unwrap();
    let result = events(&[first, second].concat());
    let mut channels = [String::new(), String::new()];
    for event in result {
        for choice in event["choices"].as_array().unwrap() {
            channels[choice["index"].as_u64().unwrap() as usize]
                .push_str(choice["delta"]["content"].as_str().unwrap_or(""));
        }
    }
    assert_eq!(
        channels,
        [format!("https://example/{reference}"), "你好 张三".into()]
    );
}

#[test]
fn malformed_frames_unfinished_channels_unknown_variants_and_reload_abort_streams() {
    let mut gate = gate();
    for wire in [
        b"data: {\"choices\":[],\"choices\":[]}\n\n".as_slice(),
        b"data: {\"choices\":[],\"reasoning_content\":\"opaque\"}\n\n",
        b"data: \xff\n\n",
    ] {
        let mut processor = SseTransformer::new(Api::Chat, &gate).unwrap();
        assert!(processor.feed(wire, &mut gate).is_err());
    }
    let mut processor = SseTransformer::new(Api::Chat, &gate).unwrap();
    processor.feed(b"data: {", &mut gate).unwrap();
    assert!(processor.finish(&gate).is_err());
    let mut processor = SseTransformer::new(Api::Messages, &gate).unwrap();
    assert!(processor
        .feed(
            event(&json!({"type":"content_block_delta","delta":{"type":"thinking_delta"}}))
                .as_bytes(),
            &mut gate
        )
        .is_err());
    let mut processor = SseTransformer::new(Api::Chat, &gate).unwrap();
    processor
        .feed(
            event(&json!({"choices":[{"index":0,"delta":{"content":"<CP_"}}]})).as_bytes(),
            &mut gate,
        )
        .unwrap();
    assert!(processor.finish(&gate).is_err());
    let mut processor = SseTransformer::new(Api::Chat, &gate).unwrap();
    gate.reload(PrivacyPolicy::from_json(r#"{"epoch":2}"#).unwrap())
        .unwrap();
    assert!(matches!(
        processor.feed(b"data: [DONE]\n\n", &mut gate),
        Err(Error::VaultDenied)
    ));
}
