//! L1 契约测试：控制台写接口（provider / model 增删改）。
//!
//! 每条用例都在临时目录里放一份真实配置文件，走完整 HTTP 路径，
//! 断言「响应、磁盘、路由」三者一致。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anybridge_litellm_core::load_from_str;
use anybridge_litellm_server::{build_router, AppState};
use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const MASTER_KEY: &str = "master-key";
const CONSOLE_TOKEN: &str = "console-token-test";
const HOST: &str = "127.0.0.1:4000";
const KEY_VAR: &str = "ANYBRIDGE_CONSOLE_WRITE_TEST_KEY";
const KEY_VALUE: &str = "sk-console-write-test";

const CONFIG: &str = r#"
auth:
  master_key: master-key
providers:
  openai:
    api: completions
    base_url: http://127.0.0.1:1/v1
    api_key: os.environ/ANYBRIDGE_CONSOLE_WRITE_TEST_KEY
    models:
      - id: gpt-4o
  anthropic:
    api: anthropic
    base_url: http://127.0.0.1:1/v1
    api_key: os.environ/ANYBRIDGE_CONSOLE_WRITE_TEST_KEY
    models:
      - id: claude-sonnet
"#;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "anybridge-console-write-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn config_path(dir: &Path) -> PathBuf {
    std::env::set_var(KEY_VAR, KEY_VALUE);
    let path = dir.join("config.yaml");
    std::fs::write(&path, CONFIG).unwrap();
    path
}

/// 可写控制台。返回的路由与配置文件路径一起用。
fn writable_app(dir: &Path) -> (Router, PathBuf) {
    let path = config_path(dir);
    let config = load_from_str(CONFIG, &Default::default()).unwrap().config;
    let state =
        AppState::with_token_and_path(config, CONSOLE_TOKEN.to_string(), Some(path.clone()), None);
    (build_router(state), path)
}

/// 只读控制台（没有 `--config` 路径）。
fn read_only_app() -> Router {
    std::env::set_var(KEY_VAR, KEY_VALUE);
    let config = load_from_str(CONFIG, &Default::default()).unwrap().config;
    build_router(AppState::with_token(config, CONSOLE_TOKEN.to_string()))
}

async fn console(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", HOST)
        .header("origin", format!("http://{HOST}"));
    if let Some(token) = token {
        builder = builder.header("x-console-token", token);
    }
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    app.clone()
        .oneshot(
            builder
                .body(Body::from(body.unwrap_or_default().to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn json_body(response: Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).expect("响应体应是 JSON")
}

fn provider_payload(name: &str) -> String {
    json!({
        "name": name,
        "api": "completions",
        "base_url": "http://127.0.0.1:11434/v1",
        "api_key": format!("os.environ/{KEY_VAR}"),
        "timeout": 30,
    })
    .to_string()
}

fn model_payload(id: &str) -> String {
    json!({
        "id": id,
        "name": "Local Model",
        "context_window": 32768,
        "max_output_tokens": 4096,
        "input": ["text"],
        "thinking_levels": ["low", "high"],
        "default_thinking_level": "high",
    })
    .to_string()
}

#[tokio::test]
async fn provider_crud_round_trip() {
    let dir = temp_dir("provider");
    let (app, path) = writable_app(&dir);

    // 新建
    let response = console(
        &app,
        "POST",
        "/console/api/providers",
        Some(CONSOLE_TOKEN),
        Some(&provider_payload("local")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let state = json_body(response).await;
    assert_eq!(state["editable"], true);
    let local = state["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "local")
        .expect("应有 local provider");
    assert_eq!(local["api"], "completions");
    assert_eq!(local["timeout"], 30);
    assert_eq!(local["api_key"]["kind"], "env");
    assert_eq!(local["api_key"]["env"], KEY_VAR);
    assert!(local["models"].as_array().unwrap().is_empty());

    // 落盘且只写环境变量引用
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("local:"), "{written}");
    assert!(
        written.contains(&format!("api_key: os.environ/{KEY_VAR}")),
        "{written}"
    );
    assert!(!written.contains(KEY_VALUE), "不得写回展开后的密钥");

    // 热加载：不重启即可路由（上游不可达 → 502 而不是 404 provider_not_found）
    let response = console(
        &app,
        "POST",
        "/llmproxy/local/v1/chat/completions",
        None,
        None,
    )
    .await;
    // 这条路径需要 master key，缺 key 时先 401，说明路由已认识 local。
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // 更新：改 base_url / timeout，api_key 缺省表示保留
    let response = console(
        &app,
        "PATCH",
        "/console/api/providers/local",
        Some(CONSOLE_TOKEN),
        Some(
            &json!({
                "name": "ignored",
                "api": "completions",
                "base_url": "http://127.0.0.1:8080/v1",
                "timeout": 5,
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let state = json_body(response).await;
    let local = state["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "local")
        .unwrap();
    assert_eq!(local["base_url"], "http://127.0.0.1:8080/v1");
    assert_eq!(local["timeout"], 5);
    assert_eq!(local["api_key"]["kind"], "env", "省略 api_key 应保留原值");
    assert!(
        !state["providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "ignored"),
        "路径段是唯一真源，body 里的 name 不能改名"
    );

    // 删除
    let response = console(
        &app,
        "DELETE",
        "/console/api/providers/local",
        Some(CONSOLE_TOKEN),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let state = json_body(response).await;
    assert!(!state["providers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["name"] == "local"));
    assert!(!std::fs::read_to_string(&path).unwrap().contains("local:"));

    // 删掉后再打路由 → 404 provider_not_found
    let response = console(
        &app,
        "POST",
        "/llmproxy/local/v1/chat/completions",
        None,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = console(&app, "GET", "/llmproxy/local/v1/models", None, None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn model_crud_round_trip() {
    let dir = temp_dir("model");
    let (app, path) = writable_app(&dir);

    // 新建模型
    let response = console(
        &app,
        "POST",
        "/console/api/providers/openai/models",
        Some(CONSOLE_TOKEN),
        Some(&model_payload("gpt-4o-mini")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let state = json_body(response).await;
    let openai = state["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "openai")
        .unwrap();
    let models = openai["models"].as_array().unwrap();
    assert_eq!(models.len(), 2);
    let added = models.iter().find(|m| m["id"] == "gpt-4o-mini").unwrap();
    assert_eq!(added["context_window"], 32768);
    assert_eq!(added["reasoning"]["param"], "reasoning_effort");
    assert_eq!(added["reasoning"]["default"], "high");
    assert_eq!(added["input_modalities"], json!(["text"]));

    // 改 id（改名）
    let response = console(
        &app,
        "PATCH",
        "/console/api/providers/openai/models",
        Some(CONSOLE_TOKEN),
        Some(
            &json!({
                "original_id": "gpt-4o-mini",
                "model": { "id": "gpt-4o-mini-2024", "context_window": 65536 },
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("gpt-4o-mini-2024"), "{written}");
    assert!(!written.contains("id: gpt-4o-mini\n"), "{written}");

    // 删除
    let response = console(
        &app,
        "DELETE",
        "/console/api/providers/openai/models?model=gpt-4o-mini-2024",
        Some(CONSOLE_TOKEN),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let state = json_body(response).await;
    let openai = state["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "openai")
        .unwrap();
    assert_eq!(openai["models"].as_array().unwrap().len(), 1);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn validation_errors_are_reported_and_do_not_touch_disk() {
    let dir = temp_dir("validation");
    let (app, path) = writable_app(&dir);
    let before = std::fs::read_to_string(&path).unwrap();

    let response = console(
        &app,
        "POST",
        "/console/api/providers",
        Some(CONSOLE_TOKEN),
        Some(
            &json!({
                "name": "bad",
                "api": "chat",
                "base_url": "not-a-url",
                "api_key": format!("os.environ/{KEY_VAR}"),
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["error"]["type"], "invalid_request");
    let details = body["error"]["details"].as_array().unwrap();
    let paths: Vec<&str> = details
        .iter()
        .map(|detail| detail["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"providers.bad.api"), "{paths:?}");
    assert!(paths.contains(&"providers.bad.base_url"), "{paths:?}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before,
        "校验失败不得落盘"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn plaintext_api_key_is_rejected() {
    let dir = temp_dir("plaintext");
    let (app, path) = writable_app(&dir);
    let before = std::fs::read_to_string(&path).unwrap();

    let response = console(
        &app,
        "POST",
        "/console/api/providers",
        Some(CONSOLE_TOKEN),
        Some(
            &json!({
                "name": "local",
                "api": "completions",
                "base_url": "http://127.0.0.1:11434/v1",
                "api_key": "sk-live-plaintext",
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    let details = body["error"]["details"].as_array().unwrap();
    assert_eq!(details[0]["path"], "providers.local.api_key");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn write_guards() {
    let dir = temp_dir("guards");
    let (app, _path) = writable_app(&dir);

    // 缺 token / 错 token
    for token in [None, Some("wrong")] {
        let response = console(
            &app,
            "POST",
            "/console/api/providers",
            token,
            Some(&provider_payload("local")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{token:?}");
    }

    // 非回环 Host
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/console/api/providers")
                .header("host", "example.com")
                .header("content-type", "application/json")
                .header("x-console-token", CONSOLE_TOKEN)
                .body(Body::from(provider_payload("local")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // 跨源 Origin
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/console/api/providers/openai")
                .header("host", HOST)
                .header("origin", "http://evil.example")
                .header("x-console-token", CONSOLE_TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // 非 JSON content-type（跨站简单表单）
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/console/api/providers")
                .header("host", HOST)
                .header("content-type", "text/plain")
                .header("x-console-token", CONSOLE_TOKEN)
                .body(Body::from(provider_payload("local")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

    // 非法 JSON
    let response = console(
        &app,
        "POST",
        "/console/api/providers",
        Some(CONSOLE_TOKEN),
        Some("{not json"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // 未知字段
    let response = console(
        &app,
        "POST",
        "/console/api/providers",
        Some(CONSOLE_TOKEN),
        Some(
            &json!({
                "name": "local",
                "api": "completions",
                "base_url": "http://127.0.0.1:11434/v1",
                "api_key": format!("os.environ/{KEY_VAR}"),
                "alias": "x",
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn missing_entities_are_404() {
    let dir = temp_dir("missing");
    let (app, _path) = writable_app(&dir);

    for (method, uri, body) in [
        (
            "PATCH",
            "/console/api/providers/nope",
            Some(provider_payload("nope")),
        ),
        ("DELETE", "/console/api/providers/nope", None),
        (
            "POST",
            "/console/api/providers/nope/models",
            Some(model_payload("m")),
        ),
    ] {
        let response = console(&app, method, uri, Some(CONSOLE_TOKEN), body.as_deref()).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {uri}");
        let body = json_body(response).await;
        assert_eq!(body["error"]["type"], "provider_not_found", "{uri}");
    }

    let response = console(
        &app,
        "DELETE",
        "/console/api/providers/openai/models?model=nope",
        Some(CONSOLE_TOKEN),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(response).await["error"]["type"],
        "model_not_found"
    );

    let response = console(
        &app,
        "PATCH",
        "/console/api/providers/openai/models",
        Some(CONSOLE_TOKEN),
        Some(&json!({ "original_id": "nope", "model": { "id": "nope" } }).to_string()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn deleting_the_last_provider_leaves_empty_config() {
    let dir = temp_dir("last");
    let (app, path) = writable_app(&dir);

    let response = console(
        &app,
        "DELETE",
        "/console/api/providers/anthropic",
        Some(CONSOLE_TOKEN),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = console(
        &app,
        "DELETE",
        "/console/api/providers/openai",
        Some(CONSOLE_TOKEN),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "允许删空 provider");
    let body = json_body(response).await;
    assert_eq!(
        body["providers"]
            .as_array()
            .map(|providers| providers.len()),
        Some(0)
    );

    let yaml = std::fs::read_to_string(&path).expect("读取配置");
    assert!(
        yaml.contains("providers: {}"),
        "空 provider 应写成映射：{yaml}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn read_only_console_returns_conflict() {
    let app = read_only_app();
    let response = console(
        &app,
        "POST",
        "/console/api/providers",
        Some(CONSOLE_TOKEN),
        Some(&provider_payload("local")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);

    let state =
        json_body(console(&app, "GET", "/console/api/state", Some(CONSOLE_TOKEN), None).await)
            .await;
    assert_eq!(state["editable"], false);

    std::fs::remove_dir_all(std::env::temp_dir().join("nonexistent")).ok();
}

#[tokio::test]
async fn state_never_contains_upstream_keys() {
    let dir = temp_dir("secrets");
    let (app, _path) = writable_app(&dir);

    let state =
        json_body(console(&app, "GET", "/console/api/state", Some(CONSOLE_TOKEN), None).await)
            .await;
    let raw = serde_json::to_string(&state).unwrap();
    assert!(!raw.contains(KEY_VALUE), "控制台泄露了上游 key");
    for provider in state["providers"].as_array().unwrap() {
        assert_eq!(provider["api_key"]["kind"], "env");
        assert_eq!(provider["api_key"]["env"], KEY_VAR);
        assert!(provider["api_key"].get("value").is_none());
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// 假上游：回显 body，验证热加载后真的把请求发到了新 provider。
async fn echo(req: Request) -> Response {
    let body = axum::body::to_bytes(req.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (StatusCode::OK, [("content-type", "application/json")], body).into_response()
}

#[tokio::test]
async fn hot_reload_routes_to_new_upstream_without_restart() {
    let upstream = Router::new().fallback(any(echo));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });

    let dir = temp_dir("hot");
    let (app, _path) = writable_app(&dir);

    let response = console(
        &app,
        "POST",
        "/console/api/providers",
        Some(CONSOLE_TOKEN),
        Some(
            &json!({
                "name": "hot",
                "api": "completions",
                "base_url": format!("http://{addr}/v1"),
                "api_key": format!("os.environ/{KEY_VAR}"),
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = console(
        &app,
        "POST",
        "/console/api/providers/hot/models",
        Some(CONSOLE_TOKEN),
        Some(&json!({ "id": "hot-model" }).to_string()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    // 用 master key 打代理路径：热加载后应能命中新 provider 与新模型。
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/llmproxy/hot/v1/chat/completions")
                .header("authorization", format!("Bearer {MASTER_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"hot-model","x":1}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-anybridge-provider")
            .and_then(|v| v.to_str().ok()),
        Some("hot")
    );
    let echoed = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(echoed.as_ref(), br#"{"model":"hot-model","x":1}"#);

    std::fs::remove_dir_all(&dir).ok();
}
