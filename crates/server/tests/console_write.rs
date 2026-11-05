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
const KEY_NAME: &str = "LOCAL_API_KEY";
const KEY_VALUE: &str = "sk-console-write-test";

const CONFIG: &str = r#"
auth:
  master_key: master-key
providers:
  openai:
    api: completions
    base_url: http://127.0.0.1:1/v1
    api_key: credential/OPENAI_API_KEY
    models:
      - id: gpt-4o
  anthropic:
    api: anthropic
    base_url: http://127.0.0.1:1/v1
    api_key: credential/ANTHROPIC_API_KEY
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
    let path = dir.join("config.yaml");
    std::fs::write(&path, CONFIG).unwrap();
    path
}

/// 凭据文件：默认就写入一条可用凭据，供上游转发解析。
fn credentials_path(dir: &Path) -> PathBuf {
    let path = dir.join("credentials.yaml");
    std::fs::write(
        &path,
        format!(
            "credentials:\n  OPENAI_API_KEY: {KEY_VALUE}\n  ANTHROPIC_API_KEY: {KEY_VALUE}\n  {KEY_NAME}: {KEY_VALUE}\n  HOT_API_KEY: {KEY_VALUE}\n"
        ),
    )
    .unwrap();
    path
}

/// 可写控制台。返回的路由与配置文件路径一起用。
fn writable_app(dir: &Path) -> (Router, PathBuf) {
    let path = config_path(dir);
    let credentials = credentials_path(dir);
    let config = load_from_str(CONFIG, &Default::default()).unwrap().config;
    let state = AppState::with_token_and_path(
        config,
        CONSOLE_TOKEN.to_string(),
        Some(path.clone()),
        None,
        Some(credentials),
    );
    (build_router(state), path)
}

/// 只读控制台（没有 `--config` 路径）。
fn read_only_app() -> Router {
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
        "api_key": format!("credential/{KEY_NAME}"),
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
    assert_eq!(local["api_key"]["kind"], "credential");
    assert_eq!(local["api_key"]["name"], KEY_NAME);
    assert_eq!(local["api_key"]["configured"], true);
    assert_eq!(local["api_key"]["source"], "file");
    assert!(local["models"].as_array().unwrap().is_empty());

    // 落盘且只写凭据引用
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("local:"), "{written}");
    assert!(
        written.contains(&format!("api_key: credential/{KEY_NAME}")),
        "{written}"
    );
    assert!(!written.contains(KEY_VALUE), "不得把密钥写进配置文件");

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
    assert_eq!(
        local["api_key"]["reference"],
        format!("credential/{KEY_NAME}"),
        "省略 api_key 应保留原引用"
    );
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
                "api_key": format!("credential/{KEY_NAME}"),
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
                "api_key": format!("credential/{KEY_NAME}"),
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
        let expected = format!(
            "{}_API_KEY",
            provider["name"].as_str().unwrap().to_ascii_uppercase()
        );
        assert_eq!(provider["api_key"]["kind"], "credential");
        assert_eq!(provider["api_key"]["name"], expected.as_str());
        assert!(provider["api_key"].get("value").is_none());
        assert!(!raw.contains(KEY_VALUE), "控制台泄露了凭据值");
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
                "api_key": format!("credential/{KEY_NAME}"),
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

// ---- 凭据 ----

#[tokio::test]
async fn credential_set_rotate_and_unset_round_trip() {
    let dir = temp_dir("credential");
    let (app, _config) = writable_app(&dir);
    let credentials = dir.join("credentials.yaml");

    // 初始：引用已配置，来源是凭据文件。
    let state =
        json_body(console(&app, "GET", "/console/api/state", Some(CONSOLE_TOKEN), None).await)
            .await;
    let openai = state["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["name"] == "openai")
        .unwrap();
    assert_eq!(openai["api_key"]["kind"], "credential");
    assert_eq!(openai["api_key"]["configured"], true);
    assert_eq!(openai["api_key"]["source"], "file");
    assert_eq!(openai["api_key"]["writable"], true);

    // 轮换：值单向下行，响应里不能出现。
    let response = console(
        &app,
        "PUT",
        "/console/api/credentials/OPENAI_API_KEY",
        Some(CONSOLE_TOKEN),
        Some(r#"{"value":"sk-rotated"}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["credential"]["configured"], true);
    assert!(
        !body.to_string().contains("sk-rotated"),
        "响应不得回显凭据值"
    );

    let written = std::fs::read_to_string(&credentials).unwrap();
    assert!(written.contains("OPENAI_API_KEY: sk-rotated"), "{written}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&credentials)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "凭据文件必须 0600");
    }

    // 列表只给名字与状态。
    let list = json_body(
        console(
            &app,
            "GET",
            "/console/api/credentials",
            Some(CONSOLE_TOKEN),
            None,
        )
        .await,
    )
    .await;
    assert!(list["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["name"] == "OPENAI_API_KEY"));
    assert!(!list.to_string().contains("sk-rotated"));

    // 删除后回到未配置。
    let response = console(
        &app,
        "DELETE",
        "/console/api/credentials/OPENAI_API_KEY",
        Some(CONSOLE_TOKEN),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["credential"]["configured"], false);
    assert!(!std::fs::read_to_string(&credentials)
        .unwrap()
        .contains("sk-rotated"));

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn credential_endpoints_require_token_and_validate_name() {
    let dir = temp_dir("credential-guard");
    let (app, _config) = writable_app(&dir);

    let response = console(&app, "GET", "/console/api/credentials", None, None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = console(
        &app,
        "PUT",
        "/console/api/credentials/OPENAI_API_KEY",
        None,
        Some(r#"{"value":"sk-x"}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // 凭据名语法：路径段里塞 `os.environ/...` 之类一律 400。
    let response = console(
        &app,
        "PUT",
        "/console/api/credentials/os.environ%2FX",
        Some(CONSOLE_TOKEN),
        Some(r#"{"value":"sk-x"}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn read_only_console_cannot_write_credentials() {
    let app = read_only_app();
    let response = console(
        &app,
        "PUT",
        "/console/api/credentials/OPENAI_API_KEY",
        Some(CONSOLE_TOKEN),
        Some(r#"{"value":"sk-x"}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn unconfigured_credential_blocks_forwarding() {
    let dir = temp_dir("unconfigured");
    let (app, _config) = writable_app(&dir);

    // 删掉凭据 → 转发阶段必须 502 `credential_not_configured`，而不是带着空 key 打上游。
    let response = console(
        &app,
        "DELETE",
        "/console/api/credentials/OPENAI_API_KEY",
        Some(CONSOLE_TOKEN),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/llmproxy/openai/v1/chat/completions")
                .header("authorization", format!("Bearer {MASTER_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"gpt-4o"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = json_body(response).await;
    assert_eq!(body["error"]["type"], "credential_not_configured");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("credential/OPENAI_API_KEY"));

    std::fs::remove_dir_all(&dir).ok();
}

// ---- 展示名 / 上游模型导入 ----

#[tokio::test]
async fn provider_display_name_falls_back_to_id() {
    let dir = temp_dir("display-name");
    let (app, path) = writable_app(&dir);

    let response = console(
        &app,
        "POST",
        "/console/api/providers",
        Some(CONSOLE_TOKEN),
        Some(
            &json!({
                "name": "wpsai",
                "display_name": "WPS AI 网关",
                "api": "completions",
                "base_url": "http://127.0.0.1:1/v1",
                "timeout": 30,
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let state = json_body(response).await;
    let wpsai = state["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["name"] == "wpsai")
        .expect("应有 wpsai");
    assert_eq!(wpsai["display_name"], "WPS AI 网关");
    assert_eq!(wpsai["api_key"]["reference"], "credential/WPSAI_API_KEY");

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("name: WPS AI 网关"), "{written}");
    assert!(
        written.contains("api_key: credential/WPSAI_API_KEY"),
        "凭据引用由 id 派生：{written}"
    );

    // 不传展示名时是 null，前端回退显示 id。
    let response = console(
        &app,
        "POST",
        "/console/api/providers",
        Some(CONSOLE_TOKEN),
        Some(&provider_payload("plain")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let state = json_body(response).await;
    let plain = state["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["name"] == "plain")
        .unwrap();
    assert!(plain["display_name"].is_null(), "{plain}");

    std::fs::remove_dir_all(&dir).ok();
}

/// 假上游：`GET /v1/models` 返回条目覆盖标准字段与各种别名写法。
async fn upstream_models_handler() -> axum::Json<Value> {
    axum::Json(json!({
        "object": "list",
        "data": [
            // 裸条目：只有 id，不映射任何标准字段。
            {"id": "gpt-4o", "object": "model"},
            // 常见别名：OpenRouter 风格 context_length / max_completion_tokens / architecture 嵌套。
            {"id": "gpt-4o-mini", "name": "GPT-4o mini", "context_length": 128000,
             "max_completion_tokens": 16384, "supports_reasoning": true,
             "architecture": {"input_modalities": ["text", "image", "audio"]}},
            // 直写风格：context_window / max_output_tokens / input / reasoning，混入非法模态与去重。
            {"id": "o4-reasoning", "context_window": 200000, "max_output_tokens": 100000,
             "input": ["text", "text", "image"], "reasoning": false},
            // DSH readListing 的别名：maxTokens / limit.{context,output} / top_provider / displayName。
            {"id": "dsh-style", "displayName": "DSH Style", "maxTokens": 8192,
             "limit": {"context": 4096, "output": 2048}, "top_provider": {"max_completion_tokens": 8191}},
            // ai-kas codeplan 风格：只带 thinking_levels 数组，混入空串与重复档位。
            {"id": "codeplan", "thinking_levels": ["high", "max", "", "low", "high"]},
            // reasoning: false 与档位并存 → 显式 false 优先，不透出档位。
            {"id": "conflict", "reasoning": false, "thinking_levels": ["low"]},
            // 数字字段是字符串或缺失 → 不猜，整段省略。
            {"id": "legacy", "context_window": "128k", "max_completion_tokens": null},
        ],
    }))
}

/// 假上游原生列表（`universal/model_list`）：容量与多模态只在这里。
async fn universal_models_handler() -> axum::Json<Value> {
    axum::Json(json!({
        "code": 0,
        "msg": "success",
        "data": {
            "model_list": [
                // 与标准列表同 id：补 context_window / input_modalities；
                // 标准列表已有的 thinking_levels 不被覆盖。
                {"model_name": "codeplan", "max_tokens": 1000000, "is_multi_modal": false,
                 "thinking_levels": ["high", "max"]},
                {"model_name": "o4-reasoning", "max_tokens": 300000, "is_multi_modal": true,
                 "thinking_levels": ["low"]},
                // 原生列表独有：追加成新条目。
                {"model_name": "rich-only", "max_tokens": 128000, "is_multi_modal": true},
                // 没有 model_name 也没有 id：跳过。
                {"max_tokens": 1},
            ],
        },
    }))
}

#[tokio::test]
async fn upstream_models_can_be_imported_selectively() {
    // 1. 起一个假上游（标准列表 + 原生列表）。
    let upstream = Router::new()
        .route("/v1/models", axum::routing::get(upstream_models_handler))
        .route(
            "/v1/universal/model_list",
            axum::routing::get(universal_models_handler),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });

    // 2. 配置一个指向它的 provider。
    let dir = temp_dir("import");
    let config = dir.join("config.yaml");
    let credentials = dir.join("credentials.yaml");
    std::fs::write(
        &config,
        format!(
            "auth:\n  master_key: {MASTER_KEY}\nproviders:\n  fake:\n    api: completions\n    base_url: http://{addr}/v1\n    api_key: credential/FAKE_API_KEY\n    models: []\n"
        ),
    )
    .unwrap();
    std::fs::write(&credentials, "credentials:\n  FAKE_API_KEY: sk-fake\n").unwrap();
    let loaded = load_from_str(
        &std::fs::read_to_string(&config).unwrap(),
        &Default::default(),
    )
    .unwrap()
    .config;
    let app = build_router(AppState::with_token_and_path(
        loaded,
        CONSOLE_TOKEN.to_string(),
        Some(config),
        None,
        Some(credentials),
    ));

    // 3. 拉上游列表。
    let response = console(
        &app,
        "GET",
        "/console/api/providers/fake/upstream-models",
        Some(CONSOLE_TOKEN),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let ids: Vec<&str> = body["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec![
            "gpt-4o",
            "gpt-4o-mini",
            "o4-reasoning",
            "dsh-style",
            "codeplan",
            "conflict",
            "legacy",
            "rich-only"
        ],
        "原生列表独有的模型追加到末尾"
    );

    // 标准字段映射：别名归一、非法值省略，取不到就不出现键。
    let models = body["models"].as_array().unwrap();
    let mini = models.iter().find(|m| m["id"] == "gpt-4o-mini").unwrap();
    assert_eq!(
        mini["context_window"], 128000,
        "context_length 别名：{mini:?}"
    );
    assert_eq!(mini["max_output_tokens"], 16384);
    assert_eq!(mini["reasoning"], true);
    assert_eq!(
        mini["input_modalities"],
        json!(["text", "image"]),
        "audio 被过滤，未知模态不进目录：{mini:?}"
    );
    let o4 = models.iter().find(|m| m["id"] == "o4-reasoning").unwrap();
    assert_eq!(o4["reasoning"], false);
    assert_eq!(
        o4["input_modalities"],
        json!(["text", "image"]),
        "去重：{o4:?}"
    );
    let dsh = models.iter().find(|m| m["id"] == "dsh-style").unwrap();
    assert_eq!(dsh["name"], "DSH Style", "displayName 别名：{dsh:?}");
    assert_eq!(dsh["context_window"], 4096, "limit.context：{dsh:?}");
    assert_eq!(
        dsh["max_output_tokens"], 8192,
        "maxTokens 先于 limit/top_provider：{dsh:?}"
    );
    let codeplan = models.iter().find(|m| m["id"] == "codeplan").unwrap();
    assert_eq!(
        codeplan["thinking_levels"],
        json!(["high", "max", "low"]),
        "档位去重保序、空串剔除，且不被原生列表覆盖：{codeplan:?}"
    );
    assert_eq!(
        codeplan["reasoning"], true,
        "档位非空即支持推理：{codeplan:?}"
    );
    // 原生列表（universal/model_list）补齐容量与模态：max_tokens → context_window，
    // is_multi_modal → input_modalities（false 只给 text）。
    assert_eq!(
        codeplan["context_window"], 1000000,
        "max_tokens 补 context_window：{codeplan:?}"
    );
    assert_eq!(
        codeplan["input_modalities"],
        json!(["text"]),
        "is_multi_modal=false → 仅文本：{codeplan:?}"
    );
    let o4 = models.iter().find(|m| m["id"] == "o4-reasoning").unwrap();
    assert_eq!(o4["reasoning"], false, "标准列表的显式声明优先：{o4:?}");
    assert_eq!(
        o4["context_window"], 200000,
        "标准列表已有的容量不被原生列表覆盖：{o4:?}"
    );
    assert!(
        o4.get("thinking_levels").is_none(),
        "reasoning:false 时原生列表的档位不能插入（否则配置校验会拒绝）：{o4:?}"
    );
    let rich_only = models.iter().find(|m| m["id"] == "rich-only").unwrap();
    assert_eq!(
        rich_only["input_modalities"],
        json!(["text", "image"]),
        "is_multi_modal=true → 文本+图片：{rich_only:?}"
    );
    assert_eq!(rich_only["context_window"], 128000);
    let conflict = models.iter().find(|m| m["id"] == "conflict").unwrap();
    assert_eq!(
        conflict["reasoning"], false,
        "显式 false 优先于档位：{conflict:?}"
    );
    assert!(
        conflict.get("thinking_levels").is_none(),
        "false 不带档位，避免矛盾载荷：{conflict:?}"
    );
    let bare = models.iter().find(|m| m["id"] == "gpt-4o").unwrap();
    assert!(bare.get("name").is_none(), "无 name 不给 null：{bare:?}");
    assert!(bare.get("context_window").is_none());
    let legacy = models.iter().find(|m| m["id"] == "legacy").unwrap();
    assert!(
        legacy.get("context_window").is_none() && legacy.get("max_output_tokens").is_none(),
        "字符串/null 数字不猜：{legacy:?}"
    );

    // 4. 只导入其中一条。
    let response = console(
        &app,
        "POST",
        "/console/api/providers/fake/models/import",
        Some(CONSOLE_TOKEN),
        Some(&json!({ "ids": ["gpt-4o-mini"] }).to_string()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let state = json_body(response).await;
    let fake = state["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["name"] == "fake")
        .unwrap();
    let models = fake["models"].as_array().unwrap();
    assert_eq!(models.len(), 1, "只应导入勾选的那条：{models:?}");
    assert_eq!(models[0]["id"], "gpt-4o-mini");

    // 5. 重复导入 → 400，避免「点了没反应」。
    let response = console(
        &app,
        "POST",
        "/console/api/providers/fake/models/import",
        Some(CONSOLE_TOKEN),
        Some(&json!({ "ids": ["gpt-4o-mini"] }).to_string()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn upstream_models_require_configuration() {
    let dir = temp_dir("import-guard");
    let (app, _config) = writable_app(&dir);

    // 没有 token → 401。
    let response = console(
        &app,
        "GET",
        "/console/api/providers/openai/upstream-models",
        None,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // provider 不存在 → 404。
    let response = console(
        &app,
        "GET",
        "/console/api/providers/nope/upstream-models",
        Some(CONSOLE_TOKEN),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn models_directory_syncs_in_one_request() {
    let dir = temp_dir("sync");
    let (app, path) = writable_app(&dir);

    // 先建一个 provider，再整份同步模型目录（PUT 幂等：只增删差异）。
    let response = console(
        &app,
        "POST",
        "/console/api/providers",
        Some(CONSOLE_TOKEN),
        Some(&provider_payload("sync")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = console(
        &app,
        "PUT",
        "/console/api/providers/sync/models",
        Some(CONSOLE_TOKEN),
        Some(&json!({ "ids": ["gpt-4o", "claude-sonnet", "gpt-4o"] }).to_string()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let state = json_body(response).await;
    let provider = state["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["name"] == "sync")
        .unwrap();
    assert_eq!(
        provider["models"].as_array().unwrap().len(),
        2,
        "重复 id 应去重：{provider}"
    );

    // 再同步成空列表：模型全删，provider 保留。
    let response = console(
        &app,
        "PUT",
        "/console/api/providers/sync/models",
        Some(CONSOLE_TOKEN),
        Some(&json!({ "ids": [] }).to_string()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let state = json_body(response).await;
    let provider = state["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["name"] == "sync")
        .unwrap();
    assert!(provider["models"].as_array().unwrap().is_empty());

    std::fs::remove_dir_all(&dir).ok();
    let _ = path;
}

// ---- 会话 cookie ----

#[tokio::test]
async fn launch_token_exchanges_for_a_session_cookie() {
    let dir = temp_dir("session");
    let (app, _config) = writable_app(&dir);

    // 1. `?token=` 换 cookie：303 + 干净地址。
    let response = console(
        &app,
        "GET",
        &format!("/console/?token={CONSOLE_TOKEN}"),
        None,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok()),
        Some("./")
    );
    let cookie = response
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .expect("应下发会话 cookie")
        .to_string();
    assert!(cookie.starts_with("anybridge-console="), "{cookie}");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Strict"), "{cookie}");
    assert!(
        !cookie.contains("Max-Age") && !cookie.contains("Expires"),
        "会话级 cookie 不该带过期时间：{cookie}"
    );

    // 2. 只带 cookie 就能读状态，不需要再带 token 头。
    let cookie_pair = cookie.split(';').next().unwrap().to_string();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/console/api/state")
                .header("host", HOST)
                .header("cookie", &cookie_pair)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 3. 伪造的 cookie 一律 401。
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/console/api/state")
                .header("host", HOST)
                .header("cookie", "anybridge-console=forged")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // 4. 错误的 launch token 不给 cookie。
    let response = console(&app, "GET", "/console/?token=wrong", None, None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn session_cookie_survives_a_restart() {
    let dir = temp_dir("session-restart");
    let key = dir.join("console-session.key");

    let build = || {
        let config = load_from_str(CONFIG, &Default::default()).unwrap().config;
        let state = AppState::with_token(config, CONSOLE_TOKEN.to_string())
            .with_session_key(Some(key.clone()));
        build_router(state)
    };

    // 第一次启动：换到 cookie。
    let first = build();
    let response = console(
        &first,
        "GET",
        &format!("/console/?token={CONSOLE_TOKEN}"),
        None,
        None,
    )
    .await;
    let cookie = response
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .expect("应下发会话 cookie")
        .split(';')
        .next()
        .unwrap()
        .to_string();
    drop(first);

    // 模拟重启：新进程、新的 AppState，但密钥文件还在。
    let restarted = build();
    let response = restarted
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/console/api/state")
                .header("host", HOST)
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "重启后旧 cookie 必须仍然有效"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "会话密钥必须 0600");
    }

    std::fs::remove_dir_all(&dir).ok();
}
