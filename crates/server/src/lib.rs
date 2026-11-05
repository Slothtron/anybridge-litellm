//! anybridge-litellm 的 HTTP 层：路由、鉴权、字节流透传。
//!
//! 只做代理，不做协议转换：请求体与响应体都按原始字节转发，SSE 不解析、
//! 不补 `[DONE]`、不重组。网关自己产生的错误走统一信封（[`GatewayError`]）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anybridge_litellm_core::passthrough::{allows, extract_model};
use anybridge_litellm_core::{
    extract_presented, verify_master_key, ApiKind, ErrorKind, GatewayConfig, GatewayError,
};
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::header::{self, HeaderName, HeaderValue};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};
use serde_json::json;

pub mod assets;
pub mod console;
pub mod store;

pub use console::new_token;
pub use store::{ConfigStore, MutationError, Snapshot};

/// 请求体上限：32 MiB。超过视为 `invalid_request`。
pub const MAX_REQUEST_BODY: usize = 32 * 1024 * 1024;

/// 上游连接池空闲超时。
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// 路由共享状态。
#[derive(Clone)]
pub struct AppState {
    /// 配置存储：读走内存快照，控制台写走「原子写盘 + 热加载」。
    pub config: Arc<ConfigStore>,
    pub client: reqwest::Client,
    /// 控制台随机 token：只经 stderr 打印的 URL 分发，不进配置、不进日志。
    pub console_token: String,
}

impl AppState {
    /// 按配置构造状态，使用默认的 `reqwest::Client` 参数，并生成新的控制台 token。
    pub fn new(config: GatewayConfig) -> Self {
        Self::with_token(config, new_token())
    }

    /// 指定 token 构造（测试用，避免随机值）；配置不可写。
    pub fn with_token(config: GatewayConfig, console_token: String) -> Self {
        Self::with_token_and_path(config, console_token, None, None)
    }

    /// 指定 token 与可回写的配置文件路径。
    ///
    /// `bind_override` 是 `--bind` 的命令行覆盖值，每次热加载后重新套用。
    pub fn with_token_and_path(
        config: GatewayConfig,
        console_token: String,
        config_path: Option<PathBuf>,
        bind_override: Option<String>,
    ) -> Self {
        Self {
            config: Arc::new(ConfigStore::new(config, config_path, bind_override)),
            client: build_client(),
            console_token,
        }
    }

    /// 当前配置快照。一次请求只取一次，保证同一请求内配置一致。
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.config.snapshot()
    }
}

fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .build()
        .expect("构造 reqwest::Client 失败")
}

/// 组装路由。`/health` 与 `/` 不需要鉴权，其余 `/llmproxy/*` 全部需要。
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/", get(discovery))
        .route("/llmproxy/{provider}/v1/models", any(models_route))
        .route("/llmproxy/{provider}/v1/{*tail}", any(proxy))
        .merge(console::router())
        .fallback(fallback)
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    let snapshot = state.snapshot();
    Json(snapshot.config.health(GatewayConfig::version()))
}

async fn discovery(State(state): State<AppState>) -> Json<serde_json::Value> {
    let snapshot = state.snapshot();
    Json(snapshot.config.discovery(GatewayConfig::version()))
}

/// `/models`：GET 由配置生成；其它方法按白名单透传到上游 `/models`。
async fn models_route(
    State(state): State<AppState>,
    Path(params): Path<HashMap<String, String>>,
    req: Request,
) -> Response {
    let provider = params.get("provider").cloned().unwrap_or_default();
    let (parts, body) = req.into_parts();

    if parts.method != axum::http::Method::GET {
        return proxy_request(
            state,
            provider,
            "models".to_string(),
            parts.method,
            parts.uri,
            parts.headers,
            body,
        )
        .await;
    }

    if let Err(err) = authorize(&state, &parts.headers) {
        return error_response(err);
    }
    let snapshot = state.snapshot();
    if !snapshot.config.providers.contains_key(&provider) {
        return error_response(provider_not_found(&snapshot, &provider));
    }

    match snapshot.config.model_cards(&provider) {
        Ok(data) => Json(json!({ "object": "list", "data": data })).into_response(),
        Err(err) => error_response(err),
    }
}

async fn proxy(
    State(state): State<AppState>,
    Path(params): Path<HashMap<String, String>>,
    req: Request,
) -> Response {
    let provider = params.get("provider").cloned().unwrap_or_default();
    let tail = params.get("tail").cloned().unwrap_or_default();
    let (parts, body) = req.into_parts();
    proxy_request(
        state,
        provider,
        tail,
        parts.method,
        parts.uri,
        parts.headers,
        body,
    )
    .await
}

/// 透传实现：鉴权 → 白名单 → 读 body → 查表 → 转发 → 原样回传。
#[allow(clippy::too_many_arguments)]
async fn proxy_request(
    state: AppState,
    provider: String,
    tail: String,
    method: axum::http::Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    // 1. 鉴权先于一切业务判断。
    if let Err(err) = authorize(&state, &headers) {
        return error_response(err);
    }

    // 2. 未知 provider / 白名单校验都在读 body 之前完成。
    //    一次请求只取一份快照：控制台热加载不会让同一请求看到半个配置。
    let snapshot = state.snapshot();
    let api = match snapshot.config.providers.get(&provider) {
        Some(config) => config.api,
        None => return error_response(provider_not_found(&snapshot, &provider)),
    };
    if !allows(api, &tail) {
        return error_response(GatewayError::new(
            ErrorKind::ApiMismatch,
            format!(
                "provider `{provider}` 的 api `{}` 不允许路径 `{tail}`",
                api.as_str()
            ),
        ));
    }

    // 3. 读完整 body（带上限）。
    let body = match axum::body::to_bytes(body, MAX_REQUEST_BODY).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return error_response(GatewayError::new(
                ErrorKind::InvalidRequest,
                format!("请求体读取失败或超过 {MAX_REQUEST_BODY} 字节上限"),
            ))
        }
    };

    let method = match reqwest::Method::from_bytes(method.as_str().as_bytes()) {
        Ok(method) => method,
        Err(_) => {
            return error_response(GatewayError::new(
                ErrorKind::InvalidRequest,
                format!("不支持的 HTTP 方法 `{}`", method.as_str()),
            ))
        }
    };

    // 4. 有 body 的方法解析 `model` 并查表；GET/HEAD/DELETE 等无 body 方法
    //    （如 `GET /v1/models/{id}`）只按 provider 取连接信息，模型名取路径尾段。
    //    转发一律用原始字节。
    let (model, target) = if has_request_body(&method) {
        let model = match extract_model(&body) {
            Ok(model) => model,
            Err(err) => return error_response(err),
        };
        let route = match snapshot.config.route(&provider, &model) {
            Ok(route) => route,
            Err(err) => return error_response(err),
        };
        (
            model,
            Target {
                api: route.api,
                base_url: route.base_url,
                api_key: route.api_key,
                api_version: route.api_version,
                timeout: route.timeout,
            },
        )
    } else {
        let provider_config = &snapshot.config.providers[&provider];
        (
            "-".to_string(),
            Target {
                api: provider_config.api,
                base_url: &provider_config.base_url,
                api_key: &provider_config.api_key,
                api_version: provider_config.api_version.as_deref(),
                timeout: provider_config.timeout,
            },
        )
    };

    // 5. 拼上游 URL（保留原 query string）。
    let url = upstream_url(target.base_url, &tail, uri.query());

    let mut builder = state.client.request(method, &url);
    for (name, value) in headers.iter() {
        if !should_forward_request_header(name.as_str()) {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder = match target.api {
        ApiKind::Anthropic => {
            let version = target
                .api_version
                .or_else(|| target.api.default_api_version())
                .unwrap_or("2023-06-01");
            builder
                .header("x-api-key", target.api_key)
                .header("anthropic-version", version)
        }
        _ => builder.header("authorization", format!("Bearer {}", target.api_key)),
    };

    // 6. 发送；超时或连接错误统一 502。
    let started = Instant::now();
    let upstream = match tokio::time::timeout(target.timeout, builder.body(body).send()).await {
        Ok(Ok(response)) => response,
        Ok(Err(err)) => {
            tracing::warn!(
                provider = %provider,
                model = %model,
                error = %err,
                "upstream_error"
            );
            return error_response(GatewayError::upstream(
                format!("provider `{provider}` 模型 `{model}` 上游请求失败：{err}"),
                None,
            ));
        }
        Err(_) => {
            return error_response(GatewayError::upstream(
                format!(
                    "provider `{provider}` 模型 `{model}` 上游超时（{} 秒）",
                    target.timeout.as_secs()
                ),
                None,
            ))
        }
    };

    // 7. status / 头 / body 原样回传。
    let status = upstream.status();
    let upstream_headers = upstream.headers().clone();
    let stream = upstream.bytes_stream();
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    copy_response_headers(&mut response, &upstream_headers);
    insert_anybridge_headers(&mut response, &provider, &model, target.api);

    tracing::info!(
        provider = %provider,
        model = %model,
        status = status.as_u16(),
        ms = started.elapsed().as_millis() as u64,
        "proxy"
    );

    response
}

async fn fallback(State(state): State<AppState>, uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path();
    let missing_provider = path == "/v1"
        || path.starts_with("/v1/")
        || path == "/llmproxy/v1"
        || path.starts_with("/llmproxy/v1/");

    if missing_provider {
        let server_url = headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .map(|host| format!("http://{host}"))
            .unwrap_or_else(|| format!("http://{}", state.snapshot().config.server.bind));
        error_response(GatewayError::new(
            ErrorKind::ProviderRequired,
            format!(
                "路径缺少 provider 段，请使用 {server_url}/llmproxy/{{provider}}/v1 作为 base URL"
            ),
        ))
    } else {
        error_response(GatewayError::new(
            ErrorKind::InvalidRequest,
            format!("未知路径 `{path}`"),
        ))
    }
}

/// 未知 provider 的错误；消息里列出已配置的 provider（设计 §1）。
fn provider_not_found(snapshot: &Snapshot, provider: &str) -> GatewayError {
    let names = snapshot.config.provider_names().join(", ");
    GatewayError::new(
        ErrorKind::ProviderNotFound,
        format!("provider `{provider}` 未配置，已配置的 provider：{names}"),
    )
}

/// master key 校验。失败时带 `WWW-Authenticate: Bearer`。
fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), GatewayError> {
    let presented = extract_presented(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok()),
    );
    if verify_master_key(&state.snapshot().config.auth.master_key, presented) {
        Ok(())
    } else {
        Err(GatewayError::new(
            ErrorKind::AuthError,
            "master key 缺失或无效",
        ))
    }
}

/// 网关错误 → 统一信封响应。
fn error_response(err: GatewayError) -> Response {
    let status = StatusCode::from_u16(err.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let is_auth = err.kind == ErrorKind::AuthError;
    let mut response = (status, Json(err.to_json())).into_response();
    if is_auth {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    response
}

/// 一次转发所需的连接信息。有 body 的方法来自 `ModelRoute`，无 body 的方法
/// （如 `GET /v1/models/{id}`）直接取 provider 级配置。
struct Target<'a> {
    api: ApiKind,
    base_url: &'a str,
    api_key: &'a str,
    api_version: Option<&'a str>,
    timeout: Duration,
}

/// 只有带 body 的方法才需要 `model` 字段做查表；其余方法按路径透传。
fn has_request_body(method: &axum::http::Method) -> bool {
    matches!(
        *method,
        axum::http::Method::POST | axum::http::Method::PUT | axum::http::Method::PATCH
    )
}

/// 请求头是否转发。跳列表按设计 §6 与 hop-by-hop 头；客户端鉴权头一律丢弃后重加。
fn should_forward_request_header(name: &str) -> bool {
    !matches!(
        name,
        "host"
            | "content-length"
            | "accept-encoding"
            | "connection"
            | "transfer-encoding"
            | "authorization"
            | "x-api-key"
    )
}

/// 上游 URL：`{base_url}/{tail}`，`base_url` 尾斜杠先去掉，query 原样保留。
fn upstream_url(base_url: &str, tail: &str, query: Option<&str>) -> String {
    let mut url = format!("{}/{}", base_url.trim_end_matches('/'), tail);
    if let Some(query) = query {
        if !query.is_empty() {
            url.push('?');
            url.push_str(query);
        }
    }
    url
}

/// 透传上游响应头；hop-by-hop 头交给 hyper 重新生成。
fn copy_response_headers(response: &mut Response, headers: &HeaderMap) {
    let target = response.headers_mut();
    for (name, value) in headers {
        if name == header::TRANSFER_ENCODING || name == header::CONNECTION {
            continue;
        }
        target.insert(name.clone(), value.clone());
    }
}

/// 追加 `x-anybridge-*`；与上游同名时以上游为准。
fn insert_anybridge_headers(response: &mut Response, provider: &str, model: &str, api: ApiKind) {
    let headers = response.headers_mut();
    for (name, value) in [
        ("x-anybridge-provider", provider),
        ("x-anybridge-model", model),
        ("x-anybridge-api", api.as_str()),
    ] {
        let name = HeaderName::from_static(name);
        if headers.contains_key(&name) {
            continue;
        }
        if let Ok(value) = HeaderValue::from_str(value) {
            headers.insert(name, value);
        }
    }
}

/// 启动服务。`bind` 形如 `127.0.0.1:4000`。
pub async fn serve(
    state: AppState,
    bind: &str,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> GatewayConfig {
        anybridge_litellm_core::load_from_str(
            r#"
auth:
  master_key: master-key
providers:
  openai:
    api: completions
    base_url: https://api.openai.com/v1/
    api_key: sk-test
    models:
      - id: gpt-4o
  anthropic:
    api: anthropic
    base_url: https://api.anthropic.com/v1
    api_key: sk-anthropic
    models:
      - id: claude-sonnet
"#,
            &Default::default(),
        )
        .unwrap()
        .config
    }

    #[test]
    fn request_header_filter_drops_hop_by_hop_and_client_auth() {
        for name in [
            "host",
            "content-length",
            "accept-encoding",
            "connection",
            "transfer-encoding",
            "authorization",
            "x-api-key",
        ] {
            assert!(!should_forward_request_header(name), "{name} 不应转发");
        }
        for name in [
            "content-type",
            "user-agent",
            "accept",
            "x-request-id",
            "anthropic-version",
        ] {
            assert!(should_forward_request_header(name), "{name} 应转发");
        }
    }

    #[test]
    fn upstream_url_joins_and_keeps_query() {
        assert_eq!(
            upstream_url("https://api.openai.com/v1", "chat/completions", None),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            upstream_url("https://api.openai.com/v1/", "chat/completions", None),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            upstream_url(
                "https://api.openai.com/v1",
                "models/gpt-4o",
                Some("a=1&b=2")
            ),
            "https://api.openai.com/v1/models/gpt-4o?a=1&b=2"
        );
        // 空 query 不产生孤立的 `?`
        assert_eq!(
            upstream_url("http://127.0.0.1:1/v1", "messages", Some("")),
            "http://127.0.0.1:1/v1/messages"
        );
        // 尾斜杠只去一次，不吞掉 base 的路径
        assert_eq!(
            upstream_url("http://127.0.0.1:1/v1//", "responses", None),
            "http://127.0.0.1:1/v1/responses"
        );
    }

    #[test]
    fn error_response_maps_status_and_type() {
        let cases = [
            (ErrorKind::AuthError, 401, "auth_error"),
            (ErrorKind::ProviderNotFound, 404, "provider_not_found"),
            (ErrorKind::ProviderRequired, 404, "provider_required"),
            (ErrorKind::ApiMismatch, 404, "api_mismatch"),
            (ErrorKind::ModelNotFound, 404, "model_not_found"),
            (ErrorKind::InvalidRequest, 400, "invalid_request"),
            (ErrorKind::UpstreamError, 502, "upstream_error"),
        ];
        for (kind, status, name) in cases {
            let response = error_response(GatewayError::new(kind, "boom"));
            assert_eq!(response.status().as_u16(), status, "{name}");
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok()),
                Some("application/json"),
                "{name}"
            );
            if kind == ErrorKind::AuthError {
                assert_eq!(
                    response
                        .headers()
                        .get(header::WWW_AUTHENTICATE)
                        .and_then(|v| v.to_str().ok()),
                    Some("Bearer")
                );
            } else {
                assert!(response.headers().get(header::WWW_AUTHENTICATE).is_none());
            }
        }
    }

    #[test]
    fn authorize_accepts_bearer_and_x_api_key() {
        let state = AppState::with_token(config(), "console-token".to_string());

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer master-key"),
        );
        assert!(authorize(&state, &headers).is_ok());

        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("master-key"));
        assert!(authorize(&state, &headers).is_ok());

        let headers = HeaderMap::new();
        let err = authorize(&state, &headers).unwrap_err();
        assert_eq!(err.kind, ErrorKind::AuthError);

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer wrong"),
        );
        assert_eq!(
            authorize(&state, &headers).unwrap_err().kind,
            ErrorKind::AuthError
        );
    }

    #[test]
    fn anybridge_headers_do_not_override_upstream() {
        let mut response = Response::new(Body::empty());
        response
            .headers_mut()
            .insert("x-anybridge-provider", HeaderValue::from_static("upstream"));
        insert_anybridge_headers(&mut response, "openai", "gpt-4o", ApiKind::Completions);
        assert_eq!(
            response
                .headers()
                .get("x-anybridge-provider")
                .and_then(|v| v.to_str().ok()),
            Some("upstream")
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
}
