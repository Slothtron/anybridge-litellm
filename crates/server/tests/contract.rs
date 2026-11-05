//! L1 契约测试：路由面、错误码矩阵、字段集合、字节级透传。
//!
//! 直接用 `tower::ServiceExt::oneshot` 打 `build_router`，不绑端口；
//! 上游是在本进程内起的假 axum server。

use std::collections::HashMap;

use anybridge_litellm_core::load_from_str;
use anybridge_litellm_server::{build_router, AppState};
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::{Json, Router};
use bytes::Bytes;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const MASTER_KEY: &str = "master-key";
const CONSOLE_TOKEN: &str = "console-token-test";
const OPENAI_KEY: &str = "sk-openai-test";
const ANTHROPIC_KEY: &str = "sk-anthropic-test";
const UPSTREAM_429_BODY: &str = r#"{"error":{"message":"rate limited","type":"rate_limit_error"}}"#;
const SSE_BODY: &str = "data: {\"id\":\"1\"}\n\ndata: [DONE]\n\n";

/// 假上游：回显 method/path/query/headers/body；按路径提供 raw / 429 / SSE。
async fn echo(req: Request) -> Response {
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or_default().to_string();
    let headers = header_map_to_json(req.headers());
    let body = axum::body::to_bytes(req.into_body(), 4 * 1024 * 1024)
        .await
        .expect("读取假上游请求体失败");

    let model = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    match model.as_str() {
        "raw" => return raw_response(body),
        "status429" => {
            return Response::builder()
                .status(StatusCode::TOO_MANY_REQUESTS)
                .header("content-type", "application/json")
                .header("retry-after", "7")
                .header("x-upstream-id", "req-429")
                .body(Body::from(UPSTREAM_429_BODY))
                .unwrap()
        }
        "stream" => return sse_response(),
        _ => {}
    }

    Json(json!({
        "method": method,
        "path": path,
        "query": query,
        "headers": headers,
        "body": String::from_utf8_lossy(&body),
    }))
    .into_response()
}

fn raw_response(body: Bytes) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/octet-stream")
        .body(Body::from(body))
        .unwrap()
}

fn sse_response() -> Response {
    let chunks = vec![
        Ok::<Bytes, std::io::Error>(Bytes::from_static(b"data: {\"id\":\"1\"}\n\n")),
        Ok(Bytes::from_static(b"data: [DONE]\n\n")),
    ];
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(futures_util::stream::iter(chunks)))
        .unwrap()
}

fn header_map_to_json(headers: &HeaderMap) -> Value {
    let mut map = serde_json::Map::new();
    for (name, value) in headers {
        map.insert(
            name.to_string(),
            Value::String(String::from_utf8_lossy(value.as_bytes()).into_owned()),
        );
    }
    Value::Object(map)
}

async fn spawn_upstream() -> String {
    let app = Router::new().fallback(any(echo));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/v1")
}

fn config_yaml(base_url: &str, anthropic_api_version: Option<&str>) -> String {
    let api_version = anthropic_api_version
        .map(|version| format!("    api_version: \"{version}\"\n"))
        .unwrap_or_default();
    format!(
        r#"
auth:
  master_key: {MASTER_KEY}
providers:
  openai:
    api: completions
    base_url: {base_url}
    api_key: {OPENAI_KEY}
    models:
      - id: gpt-4o
        name: GPT-4o
        context_window: 128000
        max_output_tokens: 16384
        input: [text, image]
        reasoning: true
        thinking_levels: [none, low, medium, high]
        default_thinking_level: high
      - id: echo
      - id: raw
      - id: status429
      - id: stream
  anthropic:
    api: anthropic
    base_url: {base_url}
    api_key: {ANTHROPIC_KEY}
{api_version}    models:
      - id: claude-sonnet
"#
    )
}

async fn app_with(base_url: &str, anthropic_api_version: Option<&str>) -> Router {
    let config = load_from_str(
        &config_yaml(base_url, anthropic_api_version),
        &Default::default(),
    )
    .expect("测试配置应可解析")
    .config;
    build_router(AppState::with_token(config, CONSOLE_TOKEN.to_string()))
}

/// 控制台用例需要显式 `Host`（回环）与可选的 `Origin`。
async fn console_send(
    app: &Router,
    uri: &str,
    host: &str,
    origin: Option<&str>,
    token: Option<&str>,
) -> Response {
    let mut builder = Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", host);
    if let Some(origin) = origin {
        builder = builder.header("origin", origin);
    }
    if let Some(token) = token {
        builder = builder.header("x-console-token", token);
    }
    app.clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    key: Option<&str>,
    body: Option<&str>,
) -> Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(key) = key {
        builder = builder.header("authorization", format!("Bearer {key}"));
    }
    let request = builder
        .body(Body::from(body.unwrap_or_default().to_string()))
        .unwrap();
    app.clone().oneshot(request).await.unwrap()
}

async fn body_bytes(response: Response) -> Bytes {
    response.into_body().collect().await.unwrap().to_bytes()
}

async fn body_json(response: Response) -> Value {
    serde_json::from_slice(&body_bytes(response).await).expect("响应体应是 JSON")
}

async fn body_chunks(response: Response) -> Vec<Bytes> {
    use futures_util::StreamExt;
    let mut stream = response.into_body().into_data_stream();
    let mut chunks = Vec::new();
    while let Some(chunk) = stream.next().await {
        chunks.push(chunk.expect("读取响应分块失败"));
    }
    chunks
}

fn error_type(body: &Value) -> &str {
    body["error"]["type"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn health_and_discovery_shapes() {
    let app = app_with("http://127.0.0.1:1/v1", None).await;

    // /health 与 / 不需要 key。
    let response = send(&app, "GET", "/health", None, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let health = body_json(response).await;
    assert_eq!(health["ok"], true);
    assert_eq!(health["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        health["providers"],
        json!(["anthropic", "openai"]),
        "provider 列表应排序且只含配置项"
    );

    let response = send(&app, "GET", "/", None, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let discovery = body_json(response).await;
    assert_eq!(discovery["name"], "anybridge-litellm");
    assert_eq!(discovery["version"], env!("CARGO_PKG_VERSION"));
    let providers = discovery["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 2);
    let openai = providers.iter().find(|p| p["name"] == "openai").unwrap();
    let mut keys: Vec<&str> = openai
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["api", "base_path", "name"]);
    assert_eq!(openai["api"], "completions");
    assert_eq!(openai["base_path"], "/llmproxy/openai/v1");
}

#[tokio::test]
async fn models_top_level_keys_are_exactly_whitelisted() {
    let app = app_with("http://127.0.0.1:1/v1", None).await;
    let response = send(
        &app,
        "GET",
        "/llmproxy/openai/v1/models",
        Some(MASTER_KEY),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );

    let body = body_json(response).await;
    assert_eq!(body["object"], "list");
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 5);

    // 全字段模型：顶层键恰好是白名单。
    let full = &data[0];
    let mut keys: Vec<&str> = full
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "context_window",
            "created",
            "extra",
            "id",
            "max_output_tokens",
            "name",
            "object",
            "owned_by"
        ]
    );
    assert_eq!(full["id"], "gpt-4o");
    assert_eq!(full["object"], "model");
    assert_eq!(full["created"], 0);
    assert_eq!(full["owned_by"], "openai");
    assert_eq!(full["name"], "GPT-4o");
    assert_eq!(full["context_window"], 128000);
    assert_eq!(full["max_output_tokens"], 16384);
    assert_eq!(full["extra"]["input_modalities"], json!(["text", "image"]));
    assert_eq!(full["extra"]["reasoning"]["param"], "reasoning_effort");
    assert_eq!(full["extra"]["reasoning"]["default"], "high");

    // 只有 id 的模型：恰好四字段，没有 extra。
    let minimal = &data[1];
    let mut keys: Vec<&str> = minimal
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["created", "id", "object", "owned_by"]);

    // 白名单以外不允许出现任何顶层键。
    const ALLOWED: [&str; 8] = [
        "id",
        "object",
        "created",
        "owned_by",
        "name",
        "context_window",
        "max_output_tokens",
        "extra",
    ];
    for card in data {
        for key in card.as_object().unwrap().keys() {
            assert!(ALLOWED.contains(&key.as_str()), "非白名单顶层键 {key}");
        }
    }
}

#[tokio::test]
async fn auth_error_without_or_with_wrong_key() {
    for (method, uri, key) in [
        ("GET", "/llmproxy/openai/v1/models", None),
        ("GET", "/llmproxy/openai/v1/models", Some("wrong-key")),
        ("POST", "/llmproxy/openai/v1/chat/completions", None),
    ] {
        let app = app_with("http://127.0.0.1:1/v1", None).await;
        let response = send(&app, method, uri, key, Some(r#"{"model":"gpt-4o"}"#)).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(
            response
                .headers()
                .get("www-authenticate")
                .and_then(|v| v.to_str().ok()),
            Some("Bearer")
        );
        let body = body_json(response).await;
        assert_eq!(error_type(&body), "auth_error");
    }
}

#[tokio::test]
async fn unknown_provider_is_404() {
    let app = app_with("http://127.0.0.1:1/v1", None).await;
    for uri in [
        "/llmproxy/nope/v1/models",
        "/llmproxy/nope/v1/chat/completions",
    ] {
        let response = send(
            &app,
            "POST",
            uri,
            Some(MASTER_KEY),
            Some(r#"{"model":"m"}"#),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
        let body = body_json(response).await;
        assert_eq!(error_type(&body), "provider_not_found", "{uri}");
        let message = body["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("anthropic"),
            "消息应列出已配置 provider：{message}"
        );
        assert!(
            message.contains("openai"),
            "消息应列出已配置 provider：{message}"
        );
    }
}

#[tokio::test]
async fn missing_provider_is_404_provider_required() {
    let app = app_with("http://127.0.0.1:1/v1", None).await;
    for (method, uri) in [
        ("POST", "/v1/chat/completions"),
        ("POST", "/llmproxy/v1/chat/completions"),
        ("GET", "/v1"),
        ("GET", "/v1/models"),
    ] {
        let response = send(&app, method, uri, Some(MASTER_KEY), None).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
        let body = body_json(response).await;
        assert_eq!(error_type(&body), "provider_required", "{uri}");
        let message = body["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("/llmproxy/{provider}/v1"),
            "消息应给出示例：{message}"
        );
    }
}

#[tokio::test]
async fn unknown_path_is_400_invalid_request() {
    let app = app_with("http://127.0.0.1:1/v1", None).await;
    for uri in ["/nope", "/llmproxy", "/llmproxy/openai/v1"] {
        let response = send(&app, "GET", uri, Some(MASTER_KEY), None).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        let body = body_json(response).await;
        assert_eq!(error_type(&body), "invalid_request", "{uri}");
    }
}

#[tokio::test]
async fn api_mismatch_when_client_uses_wrong_protocol() {
    let app = app_with("http://127.0.0.1:1/v1", None).await;
    let response = send(
        &app,
        "POST",
        "/llmproxy/anthropic/v1/chat/completions",
        Some(MASTER_KEY),
        Some(r#"{"model":"claude-sonnet"}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(error_type(&body), "api_mismatch");
}

#[tokio::test]
async fn model_not_found() {
    let app = app_with("http://127.0.0.1:1/v1", None).await;
    let response = send(
        &app,
        "POST",
        "/llmproxy/openai/v1/chat/completions",
        Some(MASTER_KEY),
        Some(r#"{"model":"GPT-4O"}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(error_type(&body), "model_not_found");
}

#[tokio::test]
async fn invalid_body_is_400() {
    let app = app_with("http://127.0.0.1:1/v1", None).await;
    for body in ["not json", "{}", r#"{"model":123}"#] {
        let response = send(
            &app,
            "POST",
            "/llmproxy/openai/v1/chat/completions",
            Some(MASTER_KEY),
            Some(body),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        let json = body_json(response).await;
        assert_eq!(error_type(&json), "invalid_request", "{body}");
    }
}

#[tokio::test]
async fn request_body_is_forwarded_byte_for_byte() {
    let base_url = spawn_upstream().await;
    let app = app_with(&base_url, None).await;

    // 含多余空白、非顺序键、指数与小数；一个字节都不许改。
    let body = "{ \"zeta\" : 1 ,\n  \"model\" : \"raw\",  \"alpha\" : [ 1.50, 2e10, \"\\u00e9\" ] , \"nested\": {\"b\": null, \"a\": true} }\n";
    let response = send(
        &app,
        "POST",
        "/llmproxy/openai/v1/chat/completions",
        Some(MASTER_KEY),
        Some(body),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/octet-stream")
    );
    let echoed = body_bytes(response).await;
    assert_eq!(echoed.as_ref(), body.as_bytes());
}

#[tokio::test]
async fn upstream_error_status_headers_and_body_pass_through() {
    let base_url = spawn_upstream().await;
    let app = app_with(&base_url, None).await;
    let response = send(
        &app,
        "POST",
        "/llmproxy/openai/v1/chat/completions",
        Some(MASTER_KEY),
        Some(r#"{"model":"status429"}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    assert_eq!(
        response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok()),
        Some("7")
    );
    assert_eq!(
        response
            .headers()
            .get("x-upstream-id")
            .and_then(|v| v.to_str().ok()),
        Some("req-429")
    );
    // 上游错误体不改写、不包信封。
    assert_eq!(
        body_bytes(response).await.as_ref(),
        UPSTREAM_429_BODY.as_bytes()
    );
}

#[tokio::test]
async fn sse_streams_chunk_by_chunk() {
    let base_url = spawn_upstream().await;
    let app = app_with(&base_url, None).await;
    let response = send(
        &app,
        "POST",
        "/llmproxy/openai/v1/chat/completions",
        Some(MASTER_KEY),
        Some(r#"{"model":"stream","stream":true}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );

    let chunks = body_chunks(response).await;
    assert!(chunks.len() >= 2, "应逐块到达，实际 {} 块", chunks.len());
    let joined: Vec<u8> = chunks.iter().flat_map(|chunk| chunk.to_vec()).collect();
    assert_eq!(joined.as_slice(), SSE_BODY.as_bytes());
}

#[tokio::test]
async fn anybridge_response_headers_are_appended() {
    let base_url = spawn_upstream().await;
    let app = app_with(&base_url, None).await;
    let response = send(
        &app,
        "POST",
        "/llmproxy/openai/v1/chat/completions",
        Some(MASTER_KEY),
        Some(r#"{"model":"gpt-4o"}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-anybridge-provider")
            .and_then(|v| v.to_str().ok()),
        Some("openai")
    );
    assert_eq!(
        response
            .headers()
            .get("x-anybridge-model")
            .and_then(|v| v.to_str().ok()),
        Some("gpt-4o")
    );
    assert_eq!(
        response
            .headers()
            .get("x-anybridge-api")
            .and_then(|v| v.to_str().ok()),
        Some("completions")
    );
}

#[tokio::test]
async fn request_headers_are_filtered_and_rewritten() {
    let base_url = spawn_upstream().await;
    let app = app_with(&base_url, None).await;

    let request = Request::builder()
        .method("POST")
        .uri("/llmproxy/openai/v1/chat/completions?trace=1")
        .header("authorization", format!("Bearer {MASTER_KEY}"))
        .header("x-api-key", "client-side-key")
        .header("x-custom", "keep-me")
        .header("accept-encoding", "gzip, br")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"model":"gpt-4o"}"#))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let echo = body_json(response).await;

    let headers = echo["headers"].as_object().unwrap();
    let lower: HashMap<&str, &str> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str().unwrap_or_default()))
        .collect();
    assert_eq!(lower.get("x-custom"), Some(&"keep-me"));
    assert_eq!(lower.get("content-type"), Some(&"application/json"));
    // 跳列表与客户端鉴权头不转发（host 由 reqwest 按上游地址重新生成）。
    assert!(
        !lower.contains_key("accept-encoding"),
        "accept-encoding 不应转发"
    );
    assert!(
        !lower.contains_key("x-api-key"),
        "客户端的 x-api-key 不应转发"
    );
    // 上游看到的是配置里的 key，不是客户端出示的 master key。
    assert_eq!(
        lower.get("authorization"),
        Some(&format!("Bearer {OPENAI_KEY}").as_str())
    );

    // query string 原样保留。
    assert_eq!(echo["query"], "trace=1");
    assert_eq!(echo["path"], "/v1/chat/completions");
    assert_eq!(echo["method"], "POST");
}

#[tokio::test]
async fn anthropic_uses_x_api_key_and_api_version() {
    let base_url = spawn_upstream().await;
    let app = app_with(&base_url, Some("2024-06-01")).await;

    let request = Request::builder()
        .method("POST")
        .uri("/llmproxy/anthropic/v1/messages")
        // 客户端用 x-api-key 出示 master key，上游 key 必须覆盖它。
        .header("x-api-key", MASTER_KEY)
        .header("content-type", "application/json")
        .body(Body::from(r#"{"model":"claude-sonnet"}"#))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let echo = body_json(response).await;
    let headers = echo["headers"].as_object().unwrap();

    assert_eq!(headers["x-api-key"], ANTHROPIC_KEY);
    assert_eq!(headers["anthropic-version"], "2024-06-01");
    assert!(
        !headers.contains_key("authorization"),
        "anthropic 不应带 authorization 头"
    );
}

#[tokio::test]
async fn anthropic_api_version_defaults_to_2023_06_01() {
    let base_url = spawn_upstream().await;
    let app = app_with(&base_url, None).await;
    let response = send(
        &app,
        "POST",
        "/llmproxy/anthropic/v1/messages",
        Some(MASTER_KEY),
        Some(r#"{"model":"claude-sonnet"}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let echo = body_json(response).await;
    assert_eq!(echo["headers"]["anthropic-version"], "2023-06-01");
    assert_eq!(echo["headers"]["x-api-key"], ANTHROPIC_KEY);
}

#[tokio::test]
async fn models_id_path_is_rejected() {
    let base_url = spawn_upstream().await;
    let app = app_with(&base_url, None).await;
    // 只提供 /models 列表，不提供 /models/{id}（设计 §1）。
    let response = send(
        &app,
        "GET",
        "/llmproxy/openai/v1/models/gpt-4o",
        Some(MASTER_KEY),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(error_type(&body), "api_mismatch");
}

#[tokio::test]
async fn unreachable_upstream_is_502() {
    // 绑一个端口再释放，保证连接立刻被拒（避免黑洞地址的 TCP 超时）。
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let base_url = format!("http://127.0.0.1:{port}/v1");
    let app = app_with(&base_url, None).await;
    let response = send(
        &app,
        "POST",
        "/llmproxy/openai/v1/chat/completions",
        Some(MASTER_KEY),
        Some(r#"{"model":"gpt-4o"}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = body_json(response).await;
    assert_eq!(error_type(&body), "upstream_error");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("openai"), "{message}");
    assert!(message.contains("gpt-4o"), "{message}");
    assert!(!message.contains(OPENAI_KEY), "错误消息不得含上游 key");
}

#[tokio::test]
async fn console_index_and_assets_are_served() {
    let app = app_with("http://127.0.0.1:1/v1", None).await;

    let index = console_send(&app, "/console/", "127.0.0.1:4000", None, None).await;
    assert_eq!(index.status(), StatusCode::OK);
    assert_eq!(
        index
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(
        index
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    let html = String::from_utf8(body_bytes(index).await.to_vec()).unwrap();
    assert!(
        html.contains("/console/assets/"),
        "index 应引用 /console/assets/"
    );

    // 从 index 里取一个真实资源名，验证 content-type 与长缓存。
    let asset = html
        .split("/console/assets/")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("index 里应有资源引用");
    let response = console_send(
        &app,
        &format!("/console/assets/{asset}"),
        "localhost:4000",
        None,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.starts_with("text/javascript")
            || content_type.starts_with("text/css")
            || content_type.starts_with("application/javascript"),
        "意外的资源类型：{content_type}"
    );
}

#[tokio::test]
async fn console_spa_fallback_serves_index() {
    let app = app_with("http://127.0.0.1:1/v1", None).await;
    let response = console_send(&app, "/console/whatever/deep", "127.0.0.1:4000", None, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = String::from_utf8(body_bytes(response).await.to_vec()).unwrap();
    assert!(html.contains("<div id=\"app\"") || html.contains("/console/assets/"));
}

#[tokio::test]
async fn console_api_requires_token() {
    let app = app_with("http://127.0.0.1:1/v1", None).await;

    for token in [None, Some("wrong")] {
        let response =
            console_send(&app, "/console/api/state", "127.0.0.1:4000", None, token).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "token={token:?}"
        );
        assert_eq!(
            response
                .headers()
                .get("www-authenticate")
                .and_then(|v| v.to_str().ok()),
            Some("Bearer")
        );
    }

    let response = console_send(
        &app,
        "/console/api/state",
        "127.0.0.1:4000",
        None,
        Some(CONSOLE_TOKEN),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let state = body_json(response).await;
    assert_eq!(state["name"], "anybridge-litellm");
    assert_eq!(state["prefix"], "/llmproxy");
    assert!(state["version"].is_string());
    assert_eq!(state["providers"].as_array().unwrap().len(), 2); // openai / anthropic

    // providers 按名字排序（BTreeMap），不能按下标取。
    let openai = state["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "openai")
        .expect("应有 openai provider");
    assert_eq!(openai["api"], "completions");
    assert_eq!(openai["base_path"], "/llmproxy/openai/v1");
    assert_eq!(openai["models"].as_array().unwrap().len(), 5);
    let model = openai["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "gpt-4o")
        .expect("应有 gpt-4o");
    assert_eq!(model["context_window"], 128000);
    assert_eq!(model["input_modalities"][1], "image");
    assert_eq!(model["reasoning"]["param"], "reasoning_effort");

    // 上游 key 绝不能出现在控制台响应里。
    let raw = serde_json::to_string(&state).unwrap();
    assert!(!raw.contains(OPENAI_KEY), "控制台泄露了上游 key");
    assert!(!raw.contains(ANTHROPIC_KEY), "控制台泄露了上游 key");
}

#[tokio::test]
async fn console_rejects_non_loopback_host_and_cross_origin() {
    let app = app_with("http://127.0.0.1:1/v1", None).await;

    let response = console_send(&app, "/console/", "example.com", None, None).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(error_type(&body_json(response).await), "auth_error");

    let response = console_send(
        &app,
        "/console/api/state",
        "127.0.0.1:4000",
        Some("http://evil.example"),
        Some(CONSOLE_TOKEN),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // 同源 Origin 放行。
    let response = console_send(
        &app,
        "/console/api/state",
        "127.0.0.1:4000",
        Some("http://127.0.0.1:4000"),
        Some(CONSOLE_TOKEN),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}
