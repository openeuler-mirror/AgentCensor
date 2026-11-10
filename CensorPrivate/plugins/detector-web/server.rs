//! Local visualization of the production detector; assets originate at detector 007c7ea.
use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use censorprivacy::{
    detection::{merge_spans, Span},
    error::{Error, Result},
    professional::{
        AnalyzeOptions, Prediction, ProfessionalConfig, ProfessionalDetector, MODEL_ID,
    },
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
    time::Instant,
};
use tokio::sync::Semaphore;

#[derive(clap::Args)]
pub struct WebArgs {
    #[arg(long, default_value_t = 8766)]
    port: u16,
    #[arg(long)]
    detector_model_dir: Option<PathBuf>,
    #[arg(long)]
    detector_ort_library: Option<PathBuf>,
}

struct WebState {
    detector: ProfessionalDetector,
    capacity: Arc<Semaphore>,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Input {
    text: String,
    use_ner: bool,
    mask_threshold: f64,
    mask_style: String,
}
impl Default for Input {
    fn default() -> Self {
        Self {
            text: String::new(),
            use_ner: true,
            mask_threshold: 0.6,
            mask_style: "reference".into(),
        }
    }
}
impl Input {
    fn validate(&self) -> bool {
        !self.text.trim().is_empty()
            && self.text.chars().count() <= 5000
            && self.mask_threshold.is_finite()
            && (0.0..=1.0).contains(&self.mask_threshold)
            && ["reference", "label", "stars"].contains(&self.mask_style.as_str())
    }
}

fn failure(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error":message}))).into_response()
}

async fn local_only(State(port): State<u16>, request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|s| s.to_str().ok())
        .unwrap_or("");
    let local = host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}");
    let origin_ok = request
        .headers()
        .get(header::ORIGIN)
        .is_none_or(|origin| origin.to_str().ok() == Some(format!("http://{host}").as_str()));
    let cross_site = request
        .headers()
        .get("sec-fetch-site")
        .is_some_and(|v| v == "cross-site");
    let mut response = if !local || !origin_ok || cross_site {
        failure(StatusCode::FORBIDDEN, "仅接受本机页面的请求。")
    } else {
        next.run(request).await
    };
    for (name, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("content-security-policy", "default-src 'self'; style-src 'self'; script-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"),
    ] {
        response.headers_mut().insert(name, HeaderValue::from_static(value));
    }
    response
}

fn router(state: Arc<WebState>, port: u16) -> Router {
    Router::new()
        .route("/", get(|| async { Html(include_str!("index.html")) }))
        .route(
            "/static/app.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("app.css"),
                )
            }),
        )
        .route(
            "/static/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("app.js"),
                )
            }),
        )
        .route("/api/analyze", post(analyze))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(port, local_only))
        .with_state(state)
}

async fn analyze(
    State(state): State<Arc<WebState>>,
    payload: std::result::Result<Json<Input>, JsonRejection>,
) -> Response {
    let input = match payload {
        Ok(Json(input)) if input.validate() => input,
        Ok(_) => {
            return failure(
                StatusCode::BAD_REQUEST,
                "请输入 1–5000 字符、有效遮蔽方式及 0–1 的分数阈值。",
            )
        }
        Err(error) => {
            let status = if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                StatusCode::BAD_REQUEST
            };
            return failure(status, "请发送有效且不超过 64 KiB 的 JSON 请求。");
        }
    };
    let Ok(permit) = state.capacity.clone().try_acquire_owned() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "检测器忙，请稍后重试。");
    };
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let started = Instant::now();
        let options = AnalyzeOptions {
            structural: !input.use_ner,
            // Display the numeric baseline for review; masking still uses its own threshold.
            mask_all_numbers: true,
            ..Default::default()
        };
        let predictions = state.detector.analyze(&input.text, &options)?;
        let mut result = preview(
            &input.text,
            predictions,
            &input.mask_style,
            input.mask_threshold,
        )?;
        result["elapsed_ms"] = json!(started.elapsed().as_millis());
        Ok::<_, Error>(result)
    })
    .await;
    match result {
        Ok(Ok(result)) => Json(result).into_response(),
        _ => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "检测未完成，请检查本地模型与运行库后重启服务。",
        ),
    }
}

fn preview(text: &str, predictions: Vec<Prediction>, style: &str, threshold: f64) -> Result<Value> {
    let spans = predictions
        .iter()
        .filter(|p| p.score >= threshold)
        .map(|p| Span {
            entity_type: p.entity_type.clone(),
            start: p.start,
            end: p.end,
            labels: BTreeSet::from(["private".into()]),
            metadata: BTreeMap::new(),
        })
        .collect();
    let spans = merge_spans(text, spans)?;
    let mut masked = String::new();
    let mut cursor = 0;
    let mut replacements = BTreeMap::new();
    let mut counters = BTreeMap::<&str, usize>::new();
    for span in &spans {
        masked.push_str(&text[cursor..span.start]);
        let value = &text[span.start..span.end];
        let kind = span.entity_type.as_str();
        let replacement = if style == "stars" {
            "*".repeat(value.chars().count())
        } else if kind == "SECRET" {
            "[REDACTED_SECRET]".into()
        } else {
            let key = (kind, value);
            if !replacements.contains_key(&key) {
                let replacement = loop {
                    let candidate = if style == "reference" {
                        let mut bytes = [0u8; 16];
                        getrandom::getrandom(&mut bytes).map_err(|_| Error::ServiceUnavailable)?;
                        format!("<CP_{kind}_{:032x}>", u128::from_be_bytes(bytes))
                    } else {
                        let counter = counters.entry(kind).or_default();
                        *counter += 1;
                        format!("[{kind}_{counter}]")
                    };
                    if !text.contains(&candidate) && !replacements.values().any(|v| v == &candidate)
                    {
                        break candidate;
                    }
                };
                replacements.insert(key, replacement);
            }
            replacements[&key].clone()
        };
        masked.push_str(&replacement);
        cursor = span.end;
    }
    masked.push_str(&text[cursor..]);
    // The browser highlights Unicode scalars, while production uses UTF-8 bytes.
    let offsets: Vec<_> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .collect();
    let categories: BTreeSet<_> = predictions.iter().map(|p| p.entity_type.as_str()).collect();
    let results: Result<Vec<_>> = predictions
        .iter()
        .map(|p| {
            let mut result = serde_json::to_value(p).map_err(|_| Error::DetectionFailed)?;
            result["start"] = json!(offsets
                .binary_search(&p.start)
                .map_err(|_| Error::DetectionFailed)?);
            result["end"] = json!(offsets
                .binary_search(&p.end)
                .map_err(|_| Error::DetectionFailed)?);
            result["mask_eligible"] = json!(p.score >= threshold);
            Ok(result)
        })
        .collect();
    Ok(
        json!({"results":results?, "count":predictions.len(), "category_count":categories.len(),
        "masked_text":masked, "masked_count":spans.len(), "mask_threshold":threshold,
        "mask_style":style, "models":{"ner":MODEL_ID}}),
    )
}

pub async fn serve(args: WebArgs) -> Result<()> {
    let mut config = ProfessionalConfig::default();
    if let Some(path) = args.detector_model_dir {
        config.model_directory = Some(path);
    }
    if let Some(path) = args.detector_ort_library {
        config.runtime_library = Some(path);
    }
    if config.model_directory.as_ref().is_none_or(|directory| {
        [
            "manifest.json",
            "model.onnx",
            "tokenizer.json",
            "config.json",
        ]
        .iter()
        .any(|name| !directory.join(name).is_file())
    }) {
        eprintln!("未找到完整的 ONNX 模型包；请设置 CP_DETECTOR_MODEL_DIR 或 --detector-model-dir。准备方法见 docs/rust-detector.md。");
        return Err(Error::DetectionFailed);
    }
    if config
        .runtime_library
        .as_ref()
        .is_none_or(|library| !library.is_file())
    {
        eprintln!("未找到 ONNX Runtime 原生运行库；请设置 CP_DETECTOR_ORT_LIBRARY 或 --detector-ort-library，指向 libonnxruntime.so（也支持版本文件名）。Cargo 构建不会下载此运行库。");
        return Err(Error::DetectionFailed);
    }
    let state = Arc::new(WebState {
        detector: ProfessionalDetector::start(config)?,
        capacity: Arc::new(Semaphore::new(1)),
    });
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, args.port))
        .await
        .map_err(|_| Error::ServiceUnavailable)?;
    let port = listener
        .local_addr()
        .map_err(|_| Error::ServiceUnavailable)?
        .port();
    println!("Privacy Detector: http://127.0.0.1:{port}/");
    axum::serve(listener, router(state, port)).with_graceful_shutdown(async {
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => (),
            _ = async { match &mut terminate { Some(signal) => { signal.recv().await; }, None => std::future::pending().await } } => (),
        }
    }).await.map_err(|_| Error::ServiceUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    #[test]
    fn preview_handles_unicode_overlaps_repeats_and_thresholds() {
        let text = "😀张三张三；42";
        let p = |kind: &str, start, end, score| Prediction {
            entity_type: kind.into(),
            start,
            end,
            score,
            sources: vec!["rule".into()],
            validation: "not_checked".into(),
            evidence: vec![],
            unit: None,
        };
        let predictions = vec![
            p("PERSON", 4, 10, 0.9),
            p("PERSON", 10, 16, 0.9),
            p("LOCATION", 7, 10, 0.8),
            p("NUMBER", 19, 21, 0.4),
        ];
        let result = preview(text, predictions.clone(), "label", 0.6).unwrap();
        assert_eq!(result["masked_text"], "😀[PERSON_1][PERSON_1]；42");
        assert_eq!(result["masked_count"], 2);
        assert_eq!(
            (
                result["results"][0]["start"].as_u64(),
                result["results"][0]["end"].as_u64()
            ),
            (Some(1), Some(3))
        );
        assert_eq!(result["results"][3]["mask_eligible"], false);
        assert_eq!(
            preview(text, predictions, "stars", 0.4).unwrap()["masked_text"],
            "😀****；**"
        );
    }

    #[tokio::test]
    async fn local_boundary_blocks_foreign_origins_and_hosts() {
        let app = Router::new()
            .route("/", get(|| async { "ok" }))
            .layer(middleware::from_fn_with_state(8766u16, local_only));
        for (host, origin, expected) in [
            ("127.0.0.1:8766", "http://127.0.0.1:8766", StatusCode::OK),
            ("localhost:8766", "http://localhost:8766", StatusCode::OK),
            (
                "foreign.example:8766",
                "http://foreign.example:8766",
                StatusCode::FORBIDDEN,
            ),
            (
                "127.0.0.1:8766",
                "https://foreign.example",
                StatusCode::FORBIDDEN,
            ),
        ] {
            let request = Request::builder()
                .uri("/")
                .header("host", host)
                .header("origin", origin)
                .body(axum::body::Body::empty())
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), expected);
            assert_eq!(response.headers()["cache-control"], "no-store");
        }
    }
}
