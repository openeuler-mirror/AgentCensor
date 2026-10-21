use censorprivacy::{
    error::Result,
    mcp::{
        safe_result, validate_arguments, HttpPrivateComputeClient, PrivateComputeClient,
        StdioBridge,
    },
};
use serde_json::{json, Value};
use std::{
    io::Cursor,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

const REF: &str = "<CP_NUMBER_0123456789abcdef0123456789abcdef>";
const TOKEN: &str = "local-test-token-01234567890123456789";
struct Client {
    calls: Arc<AtomicUsize>,
    result: Value,
}
impl PrivateComputeClient for Client {
    async fn compute(&self, _: Value) -> Result<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.result.clone())
    }
}
fn bridge(result: Value, aliases: bool) -> (StdioBridge<Client>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    (
        StdioBridge::new(
            Client {
                calls: calls.clone(),
                result,
            },
            aliases,
        ),
        calls,
    )
}
async fn initialize(bridge: &mut StdioBridge<Client>) {
    let result = bridge.handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}})).await.unwrap();
    assert_eq!(result["result"]["protocolVersion"], "2025-11-25");
    assert!(bridge
        .handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .await
        .is_none());
}
fn call(arguments: Value) -> Value {
    json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"private_compute","arguments":arguments}})
}

#[tokio::test]
async fn initialization_registers_one_tool_and_notifications_cannot_compute() {
    let (mut bridge, calls) = bridge(json!({"operation":"sum","results":{"sum":REF}}), false);
    let arguments = json!({"operation":"sum","refs":[REF]});
    assert_eq!(
        bridge.handle(&call(arguments.clone())).await.unwrap()["error"]["code"],
        -32002
    );
    initialize(&mut bridge).await;
    let tools = bridge
        .handle(&json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}))
        .await
        .unwrap();
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 1);
    assert_eq!(tools["result"]["tools"][0]["name"], "private_compute");
    let mut notification = call(arguments.clone());
    notification.as_object_mut().unwrap().remove("id");
    assert!(bridge.handle(&notification).await.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let response = bridge.handle(&call(arguments)).await.unwrap();
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(
        response["result"]["structuredContent"],
        json!({"results":{"sum":REF}})
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let reinitialize = bridge.handle(&json!({"jsonrpc":"2.0","id":9,"method":"initialize","params":{"protocolVersion":"2025-11-25"}})).await.unwrap();
    assert_eq!(reinitialize["error"]["code"], -32602);
}

#[test]
fn tools_reject_raw_values_bad_shapes_operations_and_options() {
    for arguments in [
        json!({"operation":"sum","refs":[100]}),
        json!({"operation":"sum","refs":["100"]}),
        json!({"operation":"eval","refs":[REF]}),
        json!({"operation":"sum","refs":[]}),
        json!({"operation":"sum","refs":[REF],"options":{"extra":"raw"}}),
        json!({"operation":"aggregate","refs":[REF],"options":{"operations":["multiply"]}}),
        json!({"operation":"divide","refs":[REF]}),
        json!({"operation":"date_diff","refs":[REF,REF],"options":{"unit":"hours"}}),
        json!({"operation":"sum","refs":[REF],"run_id":"forged"}),
    ] {
        assert!(validate_arguments(&arguments, false).is_err());
    }
    let tls = json!({"operation":"sum","refs":["ABC","~Pabcdefghijk"]});
    assert!(validate_arguments(&tls, false).is_err());
    assert!(validate_arguments(&tls, true).is_ok());
}

#[tokio::test]
async fn untrusted_http_or_inprocess_tool_results_never_release_plain_values() {
    for result in [
        json!({"results":{"sum":100}}),
        json!({"results":{"sum":"private-value"}}),
        json!({"results":{"sum":REF},"raw":100}),
        json!({"results":{"mean":REF}}),
        json!({"operation":"mean","results":{"sum":REF}}),
        json!({"results":{"sum":REF},"unit":"raw-private-unit"}),
        json!({"results":{"sum":REF},"result":"<CP_NUMBER_11111111111111111111111111111111>"}),
    ] {
        let (mut bridge, _) = bridge(result, false);
        initialize(&mut bridge).await;
        let response = bridge
            .handle(&call(json!({"operation":"sum","refs":[REF]})))
            .await
            .unwrap();
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            response["result"]["content"][0]["text"],
            "Private computation unavailable"
        );
    }
    let arguments = validate_arguments(&json!({"operation":"sum","refs":[REF]}), false).unwrap();
    assert_eq!(
        safe_result(
            &json!({"result":REF,"entity_type":"sensitive-free-text"}),
            &arguments
        )
        .unwrap(),
        json!({"results":{"sum":REF}})
    );
}

#[tokio::test]
async fn stdio_stdout_is_jsonrpc_only_and_oversized_lines_cannot_resume() {
    let (bridge, calls) = bridge(json!({"results":{"sum":REF}}), false);
    let input = b"not-json\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n";
    let mut output = Vec::new();
    bridge.run(Cursor::new(input), &mut output).await.unwrap();
    let lines: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["error"]["code"], -32700);
    assert_eq!(lines[1]["result"], json!({}));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let (bridge, _) = self::bridge(json!({}), false);
    let mut output = Vec::new();
    assert!(bridge
        .run(Cursor::new(vec![b' '; 1_048_577]), &mut output)
        .await
        .is_err());
    assert!(output.is_empty());
}

#[test]
fn private_tool_client_cannot_send_bearer_tokens_to_remote_or_arbitrary_gateway() {
    for (url, gateway) in [
        ("http://example.com", None),
        ("http://198.18.0.1", None),
        ("http://127.0.0.1/leak", None),
        ("http://127.0.0.1?leak=value", None),
        ("http://name@127.0.0.1", None),
        ("http://198.18.0.1", Some("8.8.8.8")),
    ] {
        assert!(HttpPrivateComputeClient::new(url, TOKEN.into(), None, gateway).is_err());
    }
    assert!(HttpPrivateComputeClient::new(
        "http://198.18.0.1",
        TOKEN.into(),
        None,
        Some("198.18.0.1")
    )
    .is_ok());
    assert!(HttpPrivateComputeClient::new("http://[::1]", TOKEN.into(), None, None).is_ok());
}
