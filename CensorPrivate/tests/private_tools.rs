#![cfg(target_os = "linux")]

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use censorprivacy::{
    error::Error,
    gate::{GateOptions, PrivacyGate},
    policy::PrivacyPolicy,
    protocols::Api,
    server::{GatewayConfig, GatewayService},
};
use serde_json::{json, Value};
use tower::ServiceExt;

const TOKEN: &str = "private-tools-test-token-0123456789";

#[tokio::test]
async fn registry_runs_python_and_shell_offline_and_returns_only_references() {
    let policy = PrivacyPolicy::from_json(
        r#"{"dictionaries":{"CUSTOMER":["Fictional Zebra","Fictional Apple"],"SECRET":["LOCAL_ONLY_SECRET"]}}"#,
    )
    .unwrap();
    let mut gate = PrivacyGate::new(policy, GateOptions::default()).unwrap();
    let scripts = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/private-tools");
    let definition = |name: &str, command: &[&str], output, timeout_ms| {
        json!({"name":name, "description":"Run a trusted local operation.", "command":command,
               "output":output, "timeout_ms":timeout_ms})
    };
    let manifest = json!({"tools":[
        definition("private_sort", &["/usr/bin/python3", "-I", &format!("{scripts}/sort.py")], "order", 3000),
        definition("private_sort_shell", &["/bin/sh", &format!("{scripts}/sort.sh")], "order", 3000),
        definition("private_count", &["/usr/bin/python3", "-I", "-c", r#"
import json, os, socket, sys
assert "CENSORPRIVACY_TOKEN" not in os.environ and "HOME" not in os.environ
for family in (socket.AF_INET, socket.AF_INET6, socket.AF_UNIX):
    try:
        socket.socket(family)
    except PermissionError:
        pass
    else:
        raise AssertionError("socket was not blocked")
print(json.dumps({"values": [str(len(json.load(sys.stdin)["inputs"]))]}))
"#], "values", 3000),
        definition("private_slow", &["/bin/sh", "-c", "sleep 3 & wait"], "order", 150),
        definition("private_delayed", &["/bin/sh", "-c", "cat >/dev/null; sleep 0.15; printf '{\"values\":[\"derived text\"]}'"], "values", 3000),
        definition("private_bad", &["/bin/sh", "-c", "printf '{\"order\":[0,0]}'"], "order", 3000),
        definition("private_secret", &["/bin/sh", "-c", "printf '{\"values\":[\"LOCAL_ONLY_SECRET\"]}'"], "values", 3000),
    ]});
    gate.configure_private_tools(&manifest.to_string()).unwrap();
    let zebra = gate.protect_text("Fictional Zebra", "").unwrap();
    let apple = gate.protect_text("Fictional Apple", "").unwrap();
    let secret = gate.protect_text("LOCAL_ONLY_SECRET", "").unwrap();
    let money = gate.protect_text("700.00元", "").unwrap();
    let amount = regex::Regex::new(r"<CP_MONEY_[a-z0-9]{8}>")
        .unwrap()
        .find(&money)
        .unwrap()
        .as_str()
        .to_owned();
    let refs = json!({"refs":[zebra, apple]});
    let tool_call = |name: &str, args: &Value| {
        json!({"choices":[{"message":{"role":"assistant","content":null,
            "tool_calls":[{"id":"call_1","type":"function","function":{"name":name,"arguments":args.to_string()}}]},
            "finish_reason":"tool_calls"}]})
    };
    assert!(gate
        .restore(Api::Chat, &tool_call("private_sort", &refs))
        .is_ok());
    assert_eq!(
        gate.restore(Api::Chat, &tool_call("private_unregistered", &refs)),
        Err(Error::ProtocolBlocked)
    );
    assert!(gate
        .restore(
            Api::Chat,
            &tool_call("private_sort", &json!({"refs":["raw"]}))
        )
        .is_err());

    let mut config = GatewayConfig::new("http://127.0.0.1:1", TOKEN.into(), None, true).unwrap();
    config.private_tool_worker = Some(env!("CARGO_BIN_EXE_censorprivacy").into());
    let service = GatewayService::new(gate, config).unwrap();
    let router = service.router();
    let send = |method: &'static str, path: &'static str, body: Value, auth: bool| {
        let router = router.clone();
        async move {
            let mut request = Request::builder()
                .uri(path)
                .method(method)
                .header("content-type", "application/json");
            if auth {
                request = request.header("authorization", format!("Bearer {TOKEN}"));
            }
            let reply = router
                .oneshot(request.body(Body::from(body.to_string())).unwrap())
                .await
                .unwrap();
            let status = reply.status();
            let value: Value =
                serde_json::from_slice(&to_bytes(reply.into_body(), 65_536).await.unwrap())
                    .unwrap();
            (status, value)
        }
    };
    assert_eq!(
        send("GET", "/v1/private_tools", Value::Null, false).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, catalog) = send("GET", "/v1/private_tools", Value::Null, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        catalog["tools"][0],
        json!({"name":"private_compute","kind":"builtin"})
    );
    assert!(!catalog.to_string().contains("/bin/"));
    for endpoint in [
        "/v1/private_tools/private_sort",
        "/v1/private_tools/private_sort_shell",
    ] {
        assert_eq!(
            send("POST", endpoint, refs.clone(), true).await,
            (StatusCode::OK, json!({"refs":[apple, zebra]}))
        );
    }
    let (status, count) = send(
        "POST",
        "/v1/private_tools/private_count",
        refs.clone(),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{count}");
    assert!(count["refs"][0]
        .as_str()
        .unwrap()
        .starts_with("<CP_PRIVATE_RESULT_"));
    for (path, payload, code) in [
        (
            "/v1/private_tools/private_bad",
            refs.clone(),
            "private_tool_failed",
        ),
        (
            "/v1/private_tools/private_secret",
            refs.clone(),
            "vault_denied",
        ),
        (
            "/v1/private_tools/private_sort",
            json!({"refs":[secret]}),
            "vault_denied",
        ),
        (
            "/v1/private_tools/private_sort",
            json!({"refs":[zebra], "command":"/bin/sh"}),
            "compute_invalid",
        ),
        (
            "/v1/private_tools/private_unknown",
            refs.clone(),
            "compute_invalid",
        ),
    ] {
        let (status, reply) = send("POST", path, payload, true).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(reply, json!({"error":{"code":code}}));
    }
    let (slow, busy, ready) = tokio::join!(
        send("POST", "/v1/private_tools/private_slow", refs.clone(), true),
        send("POST", "/v1/private_tools/private_sort", refs.clone(), true),
        send("GET", "/ready", Value::Null, true),
    );
    assert_eq!(slow.1, json!({"error":{"code":"runtime_timeout"}}));
    assert_eq!(busy.1, json!({"error":{"code":"private_tool_busy"}}));
    assert_eq!(ready.0, StatusCode::OK);
    assert_eq!(
        send("POST", "/v1/private_tools/private_sort", refs.clone(), true)
            .await
            .0,
        StatusCode::OK
    );
    let args = json!({"operation":"sum","refs":[amount]});
    let original = send("POST", "/v1/private_compute", args.clone(), true).await;
    assert_eq!(original.0, StatusCode::OK);
    assert_eq!(
        original,
        send("POST", "/v1/private_tools/private_compute", args, true).await
    );
    let (stale, ()) = tokio::join!(
        send("POST", "/v1/private_tools/private_delayed", refs, true),
        async {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            service
                .reload(PrivacyPolicy::from_json(r#"{"epoch":2}"#).unwrap())
                .unwrap();
        }
    );
    assert_eq!(stale.1, json!({"error":{"code":"vault_denied"}}));
}

#[test]
fn registry_rejects_duplicate_reserved_and_relative_program_definitions() {
    let mut gate = PrivacyGate::new(PrivacyPolicy::default(), GateOptions::default()).unwrap();
    let definition = json!({"name":"private_echo", "description":"Echo locally.",
        "command":["/bin/sh","-c","cat"],"output":"values"});
    let mut reserved = definition.clone();
    reserved["name"] = "private_compute".into();
    let mut relative = definition.clone();
    relative["command"] = json!(["sh", "-c", "cat"]);
    for invalid in [
        json!({"tools":[definition.clone(), definition.clone()]}),
        json!({"tools":[reserved]}),
        json!({"tools":[relative]}),
    ] {
        assert!(gate.configure_private_tools(&invalid.to_string()).is_err());
    }
}
