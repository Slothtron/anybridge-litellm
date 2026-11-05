//! Web 控制台：内嵌静态资源 + 状态/配置读写接口。
//!
//! 安全边界（设计 §3.5）：
//!
//! - 只服务回环 `Host`；`Origin` 若存在必须同源；所有 `/console/api/*` 必须带启动时
//!   生成的随机 token（常量时间比较）。
//! - 写接口额外要求 `Content-Type: application/json`（挡跨站简单表单）。
//! - 响应里永远没有上游 `api_key`：只给 `{"kind": "env", "env": "VAR"}` 这样的描述符。
//! - 配置写回只接受 `os.environ/VAR` 引用，明文密钥在进入 YAML 之前就被拒。

use std::collections::HashMap;

use anybridge_litellm_core::config::GATEWAY_NAME;
use anybridge_litellm_core::edit::{ModelDraft, ProviderDraft};
use anybridge_litellm_core::{ApiKeyKind, ConfigErrors, Document, GatewayConfig};
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::header::{self, HeaderValue};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::Json;
use axum::Router;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::assets::Assets;
use crate::store::{MutationError, Snapshot};
use crate::AppState;

/// 启动时生成的 32 位十六进制 token；只经 stderr 打印的 URL 分发。
pub fn new_token() -> String {
    let mut buf = [0u8; 16];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        if file.read_exact(&mut buf).is_ok() {
            return buf.iter().map(|byte| format!("{byte:02x}")).collect();
        }
    }
    // 非 unix 兜底：时间 + pid 的稳定散列。安全性弱于 urandom，仅用于本地开发。
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!(
        "{:016x}{:016x}",
        nanos as u64 ^ 0x9e37_79b9_7f4a_7c15,
        std::process::id()
    )
}

const TOKEN_HEADER: &str = "x-console-token";

/// 写接口请求体上限。配置项都很小，64 KiB 足够且能挡住误用。
const MAX_CONSOLE_BODY: usize = 64 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/console", get(index))
        .route("/console/", get(index))
        .route("/console/api/state", get(state_api))
        .route("/console/api/providers", post(create_provider))
        .route(
            "/console/api/providers/{provider}",
            patch(update_provider).delete(delete_provider),
        )
        .route(
            "/console/api/providers/{provider}/models",
            post(create_model).patch(update_model).delete(delete_model),
        )
        .route("/console/assets/{*path}", get(asset))
        .route("/console/{*path}", get(spa_fallback))
}

async fn index(headers: HeaderMap) -> Response {
    serve_index(&headers)
}

async fn spa_fallback(headers: HeaderMap) -> Response {
    serve_index(&headers)
}

fn serve_index(headers: &HeaderMap) -> Response {
    if let Some(response) = guard_host(headers) {
        return response;
    }
    match Assets::get_asset("index.html") {
        Some(file) => html(file.data.into_owned()),
        None => error_with(
            StatusCode::NOT_FOUND,
            "invalid_request",
            "控制台产物缺失，请先运行 `mise run build`",
        ),
    }
}

async fn asset(
    State(_state): State<AppState>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = guard_host(&headers) {
        return response;
    }
    match Assets::get_asset(&format!("assets/{path}")) {
        Some(file) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime.as_ref())
                .header(
                    header::CACHE_CONTROL,
                    // Vite 产物带内容哈希，可以长缓存。
                    "public, max-age=31536000, immutable",
                )
                .body(Body::from(file.data.into_owned()))
                .expect("构造资源响应失败")
        }
        None => error_with(StatusCode::NOT_FOUND, "invalid_request", "静态资源不存在"),
    }
}

// ---- 读 ----

async fn state_api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = guard_console(&state, &headers) {
        return response;
    }
    Json(console_state(&state, &state.snapshot())).into_response()
}

// ---- 写 ----

async fn create_provider(State(state): State<AppState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let payload: ProviderPayload = match read_json(&state, &parts.headers, body).await {
        Ok(payload) => payload,
        Err(response) => return *response,
    };
    let draft = payload.into_draft();
    mutate(&state, move |document| {
        document.upsert_provider(&draft, true)
    })
    .await
}

async fn update_provider(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let payload: ProviderPayload = match read_json(&state, &parts.headers, body).await {
        Ok(payload) => payload,
        Err(response) => return *response,
    };
    if !state.snapshot().config.providers.contains_key(&provider) {
        return provider_missing(&provider);
    }
    let mut draft = payload.into_draft();
    // 路径段是唯一真源，body 里的 name 只用于新建。
    draft.name = provider.clone();
    mutate(&state, move |document| {
        document.upsert_provider(&draft, false)
    })
    .await
}

async fn delete_provider(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    req: Request,
) -> Response {
    let (parts, _body) = req.into_parts();
    if let Some(response) = guard_console(&state, &parts.headers) {
        return response;
    }
    if !state.snapshot().config.providers.contains_key(&provider) {
        return provider_missing(&provider);
    }
    mutate(&state, move |document| document.delete_provider(&provider)).await
}

async fn create_model(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let payload: ModelPayload = match read_json(&state, &parts.headers, body).await {
        Ok(payload) => payload,
        Err(response) => return *response,
    };
    if !state.snapshot().config.providers.contains_key(&provider) {
        return provider_missing(&provider);
    }
    let draft = payload.into_draft();
    mutate(&state, move |document| {
        document.upsert_model(&provider, None, &draft)
    })
    .await
}

/// 更新（可改 id）：`original_id` 为 `None` 时按新 id 新建。
async fn update_model(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let payload: ModelUpdatePayload = match read_json(&state, &parts.headers, body).await {
        Ok(payload) => payload,
        Err(response) => return *response,
    };

    let snapshot = state.snapshot();
    let Some(provider_config) = snapshot.config.providers.get(&provider) else {
        return provider_missing(&provider);
    };
    if let Some(original) = payload.original_id.as_deref() {
        if !provider_config
            .models
            .iter()
            .any(|model| model.id == original)
        {
            return model_missing(original);
        }
    }

    let original = payload.original_id;
    let draft = payload.model.into_draft();
    mutate(&state, move |document| {
        document.upsert_model(&provider, original.as_deref(), &draft)
    })
    .await
}

async fn delete_model(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    req: Request,
) -> Response {
    let (parts, _body) = req.into_parts();
    if let Some(response) = guard_console(&state, &parts.headers) {
        return response;
    }
    let Some(model) = query
        .get("model")
        .filter(|value| !value.is_empty())
        .cloned()
    else {
        return error_with(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "缺少 `model` 查询参数",
        );
    };

    let snapshot = state.snapshot();
    let Some(provider_config) = snapshot.config.providers.get(&provider) else {
        return provider_missing(&provider);
    };
    if !provider_config.models.iter().any(|entry| entry.id == model) {
        return model_missing(&model);
    }

    mutate(&state, move |document| {
        document.delete_model(&provider, &model)
    })
    .await
}

// ---- 载荷 ----

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderPayload {
    name: String,
    api: String,
    base_url: String,
    /// 只接受 `os.environ/VAR`（或裸变量名）。空/缺省表示更新时保留原值。
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    api_version: Option<String>,
}

impl ProviderPayload {
    fn into_draft(self) -> ProviderDraft {
        ProviderDraft {
            name: self.name,
            api: self.api,
            base_url: self.base_url,
            api_key_env: self.api_key,
            timeout: self.timeout,
            api_version: self.api_version,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelPayload {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    context_window: Option<u64>,
    #[serde(default)]
    max_output_tokens: Option<u64>,
    #[serde(default)]
    input: Option<Vec<String>>,
    #[serde(default)]
    reasoning: Option<bool>,
    #[serde(default)]
    thinking_levels: Option<Vec<String>>,
    #[serde(default)]
    default_thinking_level: Option<String>,
}

impl ModelPayload {
    fn into_draft(self) -> ModelDraft {
        ModelDraft {
            id: self.id,
            name: self.name,
            context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
            input: self.input,
            reasoning: self.reasoning,
            thinking_levels: self.thinking_levels,
            default_thinking_level: self.default_thinking_level,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelUpdatePayload {
    #[serde(default)]
    original_id: Option<String>,
    model: ModelPayload,
}

// ---- 响应 ----

/// 控制台状态。`api_key` 只给描述符，永远不含值。
fn console_state(state: &AppState, snapshot: &Snapshot) -> Value {
    let config = &snapshot.config;
    let providers: Vec<Value> = config
        .providers
        .iter()
        .map(|(name, provider)| {
            let models: Vec<Value> = config
                .model_cards(name)
                .unwrap_or_default()
                .iter()
                .map(console_model)
                .collect();
            json!({
                "name": name,
                "api": provider.api.as_str(),
                "base_path": format!("/llmproxy/{name}/v1"),
                "base_url": provider.base_url,
                "timeout": provider.timeout.as_secs(),
                "api_version": provider.api_version,
                "api_key": match snapshot.key_kind(name) {
                    ApiKeyKind::Env(var) => json!({ "kind": "env", "env": var }),
                    ApiKeyKind::Literal => json!({ "kind": "literal" }),
                    ApiKeyKind::Missing => json!({ "kind": "missing" }),
                },
                "models": models,
            })
        })
        .collect();

    json!({
        "name": GATEWAY_NAME,
        "version": GatewayConfig::version(),
        "bind": config.server.bind,
        "prefix": "/llmproxy",
        "editable": state.config.is_writable(),
        "providers": providers,
    })
}

/// `/models` 的卡片 → 控制台模型对象。字段来源与 `/models` 保持一致。
fn console_model(card: &Value) -> Value {
    let mut model = serde_json::Map::new();
    for key in ["id", "name", "context_window", "max_output_tokens"] {
        if let Some(value) = card.get(key) {
            model.insert(key.to_string(), value.clone());
        }
    }
    if let Some(extra) = card.get("extra") {
        for key in ["input_modalities", "reasoning"] {
            if let Some(value) = extra.get(key) {
                model.insert(key.to_string(), value.clone());
            }
        }
    }
    Value::Object(model)
}

async fn mutate<F>(state: &AppState, apply: F) -> Response
where
    F: FnOnce(&mut Document) -> Result<(), ConfigErrors>,
{
    match state.config.mutate(apply).await {
        // 写接口统一回最新状态，前端不需要再发一次 GET。
        Ok(snapshot) => Json(console_state(state, &snapshot)).into_response(),
        Err(err) => mutation_error(err),
    }
}

fn mutation_error(err: MutationError) -> Response {
    match err {
        MutationError::ReadOnly => error_with(
            StatusCode::CONFLICT,
            "invalid_request",
            "服务未以 --config 启动，配置不可写",
        ),
        MutationError::Invalid(errors) => {
            let details: Vec<Value> = errors
                .iter()
                .map(|error| json!({ "path": error.path, "message": error.message }))
                .collect();
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": {
                        "message": "配置校验失败",
                        "type": "invalid_request",
                        "code": "invalid_request",
                        "details": details,
                    }
                })),
            )
                .into_response()
        }
        MutationError::Io(message) => error_with(
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid_request",
            &message,
        ),
    }
}

fn provider_missing(provider: &str) -> Response {
    error_with(
        StatusCode::NOT_FOUND,
        "provider_not_found",
        &format!("provider `{provider}` 不存在"),
    )
}

fn model_missing(model: &str) -> Response {
    error_with(
        StatusCode::NOT_FOUND,
        "model_not_found",
        &format!("模型 `{model}` 不存在"),
    )
}

// ---- 守卫 ----

/// `Host` + token 双校验。写接口在读 body 之前调用。
fn guard_console(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    if let Some(response) = guard_host(headers) {
        return Some(response);
    }
    let presented = headers
        .get(TOKEN_HEADER)
        .and_then(|value| value.to_str().ok());
    if anybridge_litellm_core::verify_master_key(&state.console_token, presented) {
        None
    } else {
        Some(unauthorized())
    }
}

/// 只接受回环 `Host`；`Origin` 若存在必须同源。
fn guard_host(headers: &HeaderMap) -> Option<Response> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !is_loopback_host(host) {
        return Some(error_with(
            StatusCode::FORBIDDEN,
            "auth_error",
            "控制台只允许通过回环地址访问",
        ));
    }

    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let expected = format!("http://{host}");
        let expected_https = format!("https://{host}");
        if origin != expected && origin != expected_https {
            return Some(error_with(
                StatusCode::FORBIDDEN,
                "auth_error",
                "控制台拒绝跨源请求",
            ));
        }
    }
    None
}

fn is_loopback_host(host: &str) -> bool {
    let without_port = if let Some(rest) = host.strip_prefix('[') {
        // IPv6 字面量：[::1]:4000
        match rest.split_once(']') {
            Some((addr, _)) => format!("[{addr}]"),
            None => return false,
        }
    } else {
        host.split(':').next().unwrap_or(host).to_string()
    };
    matches!(
        without_port.as_str(),
        "127.0.0.1" | "localhost" | "[::1]" | "::1"
    )
}

/// 写接口要求 JSON 请求体：跨站简单表单只能发 `text/plain` / 表单编码。
fn is_json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
        .unwrap_or(false)
}

/// 校验 + 解析请求体。错误用 `Box<Response>` 包一层，避免 clippy 的
/// `result_large_err`（`Response` 本身有 128 字节）。
async fn read_json<T: DeserializeOwned>(
    state: &AppState,
    headers: &HeaderMap,
    body: Body,
) -> Result<T, Box<Response>> {
    if let Some(response) = guard_console(state, headers) {
        return Err(Box::new(response));
    }
    if !is_json_content_type(headers) {
        return Err(Box::new(error_with(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "invalid_request",
            "写接口要求 Content-Type: application/json",
        )));
    }
    let bytes = axum::body::to_bytes(body, MAX_CONSOLE_BODY)
        .await
        .map_err(|_| {
            Box::new(error_with(
                StatusCode::PAYLOAD_TOO_LARGE,
                "invalid_request",
                &format!("请求体超过 {MAX_CONSOLE_BODY} 字节"),
            ))
        })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        Box::new(error_with(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            &format!("请求体不是合法 JSON：{error}"),
        ))
    })
}

fn html(body: Vec<u8>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .expect("构造 HTML 响应失败")
}

fn error_with(status: StatusCode, kind: &str, message: &str) -> Response {
    (
        status,
        Json(json!({
            "error": { "message": message, "type": kind, "code": kind }
        })),
    )
        .into_response()
}

fn unauthorized() -> Response {
    let mut response = error_with(
        StatusCode::UNAUTHORIZED,
        "auth_error",
        "控制台 token 缺失或无效",
    );
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts() {
        for host in ["127.0.0.1:4000", "localhost:1", "[::1]:4000", "127.0.0.1"] {
            assert!(is_loopback_host(host), "{host} 应被接受");
        }
        for host in ["0.0.0.0:4000", "example.com", "192.168.1.2:4000", ""] {
            assert!(!is_loopback_host(host), "{host:?} 应被拒绝");
        }
    }

    #[test]
    fn token_is_32_hex_chars() {
        let token = new_token();
        assert_eq!(token.len(), 32);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(token, new_token(), "token 必须每次不同");
    }

    #[test]
    fn json_content_type_detection() {
        for value in [
            "application/json",
            "application/json; charset=utf-8",
            "Application/JSON",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(value));
            assert!(is_json_content_type(&headers), "{value}");
        }
        for value in [
            "text/plain",
            "application/x-www-form-urlencoded",
            "text/json",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(value));
            assert!(!is_json_content_type(&headers), "{value}");
        }
        assert!(!is_json_content_type(&HeaderMap::new()));
    }
}
