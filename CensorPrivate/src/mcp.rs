//! Stdio JSON-RPC bridge. Only checked references can enter tool output.
use crate::{
    compute::Operation,
    error::{Error, Result},
    json,
    references::{valid_reference, Reference, TLS_ALIAS_PATTERN, TOKEN_PATTERN},
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    future::Future,
    io::{BufRead, Write},
    net::{Ipv4Addr, Ipv6Addr},
    time::Duration,
};
use zeroize::Zeroizing;

const OPERATIONS: &[&str] = &[
    "add",
    "aggregate",
    "date_diff",
    "divide",
    "max",
    "mean",
    "min",
    "multiply",
    "subtract",
    "sum",
    "sum_and_mean",
];
const AGGREGATES: &[&str] = &["max", "mean", "min", "sum"];
const SAFE_UNITS: &[&str] = &[
    "CNY", "USD", "EUR", "GBP", "JPY", "HKD", "CAD", "AUD", "CHF", "days",
];

pub fn validate_arguments(value: &Value, allow_tls_alias: bool) -> Result<Value> {
    let object = value.as_object().ok_or(Error::ComputeInvalid)?;
    if object
        .keys()
        .any(|key| !["operation", "refs", "options"].contains(&key.as_str()))
    {
        return Err(Error::ComputeInvalid);
    }
    let operation: Operation = serde_json::from_value(
        value
            .get("operation")
            .cloned()
            .ok_or(Error::ComputeInvalid)?,
    )
    .map_err(|_| Error::ComputeInvalid)?;
    let refs = value
        .get("refs")
        .and_then(Value::as_array)
        .ok_or(Error::ComputeInvalid)?;
    if refs.is_empty()
        || refs.len() > 256
        || refs.iter().any(|value| {
            value
                .as_str()
                .is_none_or(|reference| !valid_reference(reference, allow_tls_alias))
        })
    {
        return Err(Error::InvalidReference);
    }
    let empty = json!({});
    let options = value
        .get("options")
        .unwrap_or(&empty)
        .as_object()
        .ok_or(Error::ComputeInvalid)?;
    match operation {
        Operation::Aggregate | Operation::SumAndMean => {
            if options.keys().any(|key| key != "operations") {
                return Err(Error::ComputeInvalid);
            }
            if let Some(selected) = options.get("operations") {
                let selected = selected.as_array().ok_or(Error::ComputeInvalid)?;
                if selected.is_empty()
                    || selected.len() > 4
                    || selected.iter().any(|value| {
                        value
                            .as_str()
                            .is_none_or(|value| !AGGREGATES.contains(&value))
                    })
                {
                    return Err(Error::ComputeInvalid);
                }
            }
        }
        Operation::DateDiff
            if refs.len() != 2
                || options.keys().any(|key| key != "unit")
                || options.get("unit").is_some_and(|unit| unit != "days") =>
        {
            return Err(Error::ComputeInvalid)
        }
        Operation::DateDiff => (),
        _ if !options.is_empty()
            || (matches!(
                operation,
                Operation::Add | Operation::Subtract | Operation::Multiply | Operation::Divide
            ) && refs.len() != 2) =>
        {
            return Err(Error::ComputeInvalid)
        }
        _ => (),
    }
    Ok(json!({"operation":operation,"refs":refs,"options":options}))
}
pub fn safe_result(value: &Value, arguments: &Value) -> Result<Value> {
    let object = value.as_object().ok_or(Error::ComputeInvalid)?;
    if object.keys().any(|key| {
        !["operation", "results", "result", "entity_type", "unit"].contains(&key.as_str())
    }) {
        return Err(Error::ComputeInvalid);
    }
    let operation = arguments["operation"]
        .as_str()
        .ok_or(Error::ComputeInvalid)?;
    let expected: BTreeSet<String> = if ["aggregate", "sum_and_mean"].contains(&operation) {
        arguments["options"]
            .get("operations")
            .cloned()
            .unwrap_or(json!(["sum", "mean"]))
            .as_array()
            .ok_or(Error::ComputeInvalid)?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or(Error::ComputeInvalid)
            })
            .collect::<Result<_>>()?
    } else {
        BTreeSet::from([operation.into()])
    };
    let fallback = if expected.len() == 1 && value.get("result").is_some() {
        json!({expected.first().ok_or(Error::ComputeInvalid)?:value["result"]})
    } else {
        Value::Null
    };
    let results = value
        .get("results")
        .unwrap_or(&fallback)
        .as_object()
        .ok_or(Error::ComputeInvalid)?;
    if results.keys().cloned().collect::<BTreeSet<_>>() != expected
        || results.values().any(|value| {
            value
                .as_str()
                .is_none_or(|reference| Reference::parse(reference).is_err())
        })
    {
        return Err(Error::ComputeInvalid);
    }
    if value
        .get("result")
        .is_some_and(|reference| results.len() != 1 || results.values().next() != Some(reference))
        || value
            .get("operation")
            .is_some_and(|value| value != operation)
    {
        return Err(Error::ComputeInvalid);
    }
    let mut safe = json!({"results":results});
    if let Some(unit) = value.get("unit").filter(|value| !value.is_null()) {
        if !unit.as_str().is_some_and(|unit| SAFE_UNITS.contains(&unit)) {
            return Err(Error::ComputeInvalid);
        }
        safe["unit"] = unit.clone();
    }
    Ok(safe)
}

pub trait PrivateComputeClient: Send + Sync {
    fn compute(&self, arguments: Value) -> impl Future<Output = Result<Value>> + Send;
}
pub struct HttpPrivateComputeClient {
    client: reqwest::Client,
    url: url::Url,
    token: Zeroizing<String>,
    run_id: Option<String>,
}
impl HttpPrivateComputeClient {
    pub fn new(
        url: &str,
        token: String,
        run_id: Option<String>,
        gateway_ip: Option<&str>,
    ) -> Result<Self> {
        let url = url::Url::parse(url).map_err(|_| Error::ServiceUnavailable)?;
        let gateway = gateway_ip
            .map(|value| {
                value
                    .parse::<Ipv4Addr>()
                    .map_err(|_| Error::ServiceUnavailable)
            })
            .transpose()?;
        if gateway.is_some_and(|address| u32::from(address) & 0xfffe_0000 != 0xc612_0000) {
            return Err(Error::ServiceUnavailable);
        }
        let host = url.host().ok_or(Error::ServiceUnavailable)?;
        let local = match host {
            url::Host::Ipv4(ip) => ip == Ipv4Addr::LOCALHOST || Some(ip) == gateway,
            url::Host::Ipv6(ip) => ip == Ipv6Addr::LOCALHOST,
            url::Host::Domain(name) => name == "localhost",
        };
        if !local
            || !matches!(url.scheme(), "http" | "https")
            || url.as_str().contains('@')
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
            || !(24..=512).contains(&token.len())
            || !token.bytes().all(|byte| (33..=126).contains(&byte))
        {
            return Err(Error::ServiceUnavailable);
        }
        let url = url
            .join("v1/private_compute")
            .map_err(|_| Error::ServiceUnavailable)?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| Error::ServiceUnavailable)?;
        Ok(Self {
            client,
            url,
            token: Zeroizing::new(token),
            run_id,
        })
    }
    pub fn from_environment() -> Result<Self> {
        Self::new(
            &std::env::var("CENSORPRIVACY_URL").unwrap_or_default(),
            std::env::var("CENSORPRIVACY_TOKEN").unwrap_or_default(),
            std::env::var("CENSORPRIVACY_RUN_ID").ok(),
            std::env::var("CENSORPRIVACY_GATEWAY_IP").ok().as_deref(),
        )
    }
}
impl PrivateComputeClient for HttpPrivateComputeClient {
    async fn compute(&self, arguments: Value) -> Result<Value> {
        let mut payload = arguments.clone();
        if let Some(run_id) = &self.run_id {
            payload["run_id"] = run_id.clone().into();
        }
        let response = self
            .client
            .post(self.url.clone())
            .bearer_auth(self.token.as_str())
            .json(&payload)
            .send()
            .await
            .map_err(|_| Error::ServiceUnavailable)?;
        if response.status() != reqwest::StatusCode::OK
            || response
                .headers()
                .get("content-encoding")
                .is_some_and(|value| value != "identity")
        {
            return Err(Error::ServiceUnavailable);
        }
        let mut source = response.bytes_stream();
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = source.next().await {
            let chunk = chunk.map_err(|_| Error::ServiceUnavailable)?;
            if bytes.len() + chunk.len() > 65_536 {
                return Err(Error::ResourceLimit);
            }
            bytes.extend_from_slice(&chunk);
        }
        safe_result(&json::from_slice(&bytes)?, &arguments)
    }
}

fn tool(aliases: bool) -> Value {
    let pattern = if aliases {
        format!("^(?:{TOKEN_PATTERN}|{TLS_ALIAS_PATTERN})$")
    } else {
        format!("^{TOKEN_PATTERN}$")
    };
    json!({"name":"private_compute","description":"Compute locally using existing CensorPrivacy references. Operands remain local; results are new references. Never pass raw private values. Date difference uses calendar days.",
        "inputSchema":{"type":"object","additionalProperties":false,"required":["operation","refs"],"properties":{
            "operation":{"type":"string","enum":OPERATIONS},
            "refs":{"type":"array","minItems":1,"maxItems":256,"items":{"type":"string","pattern":pattern}},
            "options":{"type":"object","additionalProperties":false,"properties":{
                "operations":{"type":"array","minItems":1,"maxItems":4,"items":{"type":"string","enum":AGGREGATES}},
                "unit":{"type":"string","enum":["days"]}}}}}})
}
pub struct StdioBridge<C> {
    client: C,
    allow_aliases: bool,
    initialized: bool,
    ready: bool,
}
impl<C: PrivateComputeClient> StdioBridge<C> {
    pub fn new(client: C, allow_aliases: bool) -> Self {
        Self {
            client,
            allow_aliases,
            initialized: false,
            ready: false,
        }
    }
    pub fn error(id: Value, code: i32, message: &'static str) -> Value {
        json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
    }
    pub async fn handle(&mut self, request: &Value) -> Option<Value> {
        let Some(method) = request
            .get("method")
            .and_then(Value::as_str)
            .filter(|_| request["jsonrpc"] == "2.0")
        else {
            return Some(Self::error(Value::Null, -32600, "Invalid request"));
        };
        let id = request.get("id");
        if id.is_some_and(|id| {
            !(id.is_null()
                || id.is_string()
                || id
                    .as_number()
                    .is_some_and(|n| !n.to_string().contains(['.', 'e', 'E'])))
        }) {
            return Some(Self::error(Value::Null, -32600, "Invalid request"));
        }
        let Some(id) = id.cloned() else {
            if method == "notifications/initialized" && self.initialized {
                self.ready = true;
            }
            return None; // Notifications never execute private computations.
        };
        let empty = json!({});
        let params = request.get("params").unwrap_or(&empty);
        if !params.is_object() {
            return Some(Self::error(id, -32602, "Invalid parameters"));
        }
        let result = match method {
            "initialize" => {
                let version = params.get("protocolVersion").and_then(Value::as_str);
                if self.initialized || !matches!(version, Some("2025-11-25" | "2025-06-18")) {
                    return Some(Self::error(id, -32602, "Unsupported initialization"));
                }
                self.initialized = true;
                json!({"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{"name":"censorprivacy-private-compute","version":env!("CARGO_PKG_VERSION")}})
            }
            "ping" => json!({}),
            _ if !self.ready => return Some(Self::error(id, -32002, "Server not initialized")),
            "tools/list" => json!({"tools":[tool(self.allow_aliases)]}),
            "tools/call" => {
                if params.get("name") != Some(&Value::from("private_compute")) {
                    return Some(Self::error(id, -32602, "Unknown private tool"));
                }
                let arguments = match validate_arguments(
                    params.get("arguments").unwrap_or(&Value::Null),
                    self.allow_aliases,
                ) {
                    Ok(arguments) => arguments,
                    Err(_) => {
                        return Some(Self::error(
                            id,
                            -32602,
                            "Invalid private computation parameters",
                        ))
                    }
                };
                match self
                    .client
                    .compute(arguments.clone())
                    .await
                    .and_then(|value| safe_result(&value, &arguments))
                {
                    Ok(safe) => {
                        json!({"content":[{"type":"text","text":safe.to_string()}],"structuredContent":safe,"isError":false})
                    }
                    Err(_) => {
                        json!({"content":[{"type":"text","text":"Private computation unavailable"}],"isError":true})
                    }
                }
            }
            _ => return Some(Self::error(id, -32601, "Method not found")),
        };
        Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
    }
    pub async fn run(mut self, mut input: impl BufRead, mut output: impl Write) -> Result<()> {
        loop {
            let mut line = Zeroizing::new(Vec::new());
            let read = std::io::Read::take(&mut input, 1_048_577)
                .read_until(b'\n', &mut line)
                .map_err(|_| Error::ServiceUnavailable)?;
            if read == 0 {
                return Ok(());
            }
            if read > 1_048_576 {
                return Err(Error::ResourceLimit);
            }
            let response = match json::from_slice(&line) {
                Ok(request) => self.handle(&request).await,
                Err(_) => Some(Self::error(Value::Null, -32700, "Parse error")),
            };
            if let Some(response) = response {
                writeln!(output, "{response}")
                    .and_then(|()| output.flush())
                    .map_err(|_| Error::ServiceUnavailable)?;
            }
        }
    }
}
