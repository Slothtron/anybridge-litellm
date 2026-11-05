//! Web 控制台：内嵌静态资源 + 状态/配置读写接口。
//!
//! 安全边界（设计 §3.5）：
//!
//! - 只服务回环 `Host`；`Origin` 若存在必须同源；所有 `/console/api/*` 必须带启动时
//!   生成的随机 token（常量时间比较）。
//! - 写接口额外要求 `Content-Type: application/json`（挡跨站简单表单）。
//! - 响应里永远没有上游密钥值：provider 只给 `credential/<name>` 引用与
//!   `{configured, source, writable}` 描述符；密钥值只经 `PUT /console/api/credentials/{name}`
//!   单向下行到凭据文件，**永不回显**。
//! - 配置写回只接受 `credential/<name>` 引用，明文密钥在进入 YAML 之前就被拒。

use std::collections::HashMap;

use anybridge_litellm_core::config::GATEWAY_NAME;
use anybridge_litellm_core::credential::CredentialRef;
use anybridge_litellm_core::edit::{ModelDraft, ProviderDraft};
use anybridge_litellm_core::CredentialError;
use anybridge_litellm_core::CredentialInfo;
use anybridge_litellm_core::{ConfigErrors, Document, GatewayConfig};
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::header::{self, HeaderValue};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
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

/// 会话 cookie 名：浏览器登录态的载体，HttpOnly，JS 读不到。
const SESSION_COOKIE: &str = "anybridge-console";

/// 控制台会话密钥：**持久化**在磁盘上，重启不变。
///
/// 浏览器拿到的是一次性交换来的 cookie，服务端用这份密钥做常量时间比较。
/// 密钥文件缺失/为空时自动生成（只影响登录态，不影响配置）。
#[derive(Clone)]
pub struct ConsoleSession {
    secret: String,
}

impl std::fmt::Debug for ConsoleSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsoleSession")
            .field("secret", &"***")
            .finish()
    }
}

impl ConsoleSession {
    /// 只读/测试用：进程内随机，重启即失效。
    pub fn ephemeral() -> Self {
        Self {
            secret: new_token(),
        }
    }

    /// 读磁盘上的会话密钥，没有就生成并写回（`0600`）。`None` 等价于 [`Self::ephemeral`]。
    pub fn load_or_create(path: Option<std::path::PathBuf>) -> Self {
        let Some(path) = path else {
            return Self::ephemeral();
        };
        if let Ok(text) = std::fs::read_to_string(&path) {
            let secret = text.trim();
            if !secret.is_empty() {
                return Self {
                    secret: secret.to_string(),
                };
            }
        }
        let session = Self::ephemeral();
        if let Err(err) = write_secret(&path, &session.secret) {
            tracing::warn!(
                "无法写入控制台会话密钥 {}：{err}（重启后浏览器登录态会失效）",
                path.display()
            );
        }
        session
    }

    /// 校验出示的会话值（常量时间比较）。
    fn matches(&self, presented: Option<&str>) -> bool {
        anybridge_litellm_core::verify_master_key(&self.secret, presented)
    }

    /// `Set-Cookie` 值：会话级 cookie（关浏览器即失效），HttpOnly + SameSite=Strict。
    fn set_cookie(&self) -> String {
        format!(
            "{SESSION_COOKIE}={}; Path=/; HttpOnly; SameSite=Strict",
            self.secret
        )
    }
}

fn write_secret(path: &std::path::Path, secret: &str) -> std::io::Result<()> {
    use std::io::Write;

    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(secret.as_bytes())?;
    file.sync_all()
}

/// 从 `Cookie` 头里取指定名字的值（只做本仓需要的最小解析）。
fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|raw| raw.split(';'))
        .filter_map(|segment| {
            let (key, value) = segment.split_once('=')?;
            (key.trim() == name).then(|| value.trim())
        })
        .next()
}

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
            post(create_model)
                .put(sync_models_route)
                .patch(update_model)
                .delete(delete_model),
        )
        .route(
            "/console/api/providers/{provider}/models/import",
            post(import_models),
        )
        .route(
            "/console/api/providers/{provider}/upstream-models",
            get(upstream_models),
        )
        .route("/console/api/upstream-models", post(probe_upstream_models))
        .route("/console/api/credentials", get(list_credentials))
        .route(
            "/console/api/credentials/{name}",
            put(set_credential).delete(delete_credential),
        )
        .route("/console/assets/{*path}", get(asset))
        .route("/console/{*path}", get(spa_fallback))
}

async fn index(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Some(response) = exchange_token(&state, &headers, query.get("token").map(String::as_str))
    {
        return response;
    }
    serve_index(&headers)
}

async fn spa_fallback(headers: HeaderMap) -> Response {
    serve_index(&headers)
}

/// `?token=<launch>` 一次性交换：校验通过就下发会话 cookie 并 303 到干净地址。
///
/// 地址栏里的 token 只在这一次出现；之后浏览器靠 cookie，重启服务不会掉登录态。
fn exchange_token(state: &AppState, headers: &HeaderMap, token: Option<&str>) -> Option<Response> {
    let token = token?;
    if !anybridge_litellm_core::verify_master_key(&state.console_token, Some(token)) {
        return Some(unauthorized());
    }
    if let Some(response) = guard_host(headers) {
        return Some(response);
    }
    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, "./")
        .header(header::SET_COOKIE, state.session.set_cookie())
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::empty())
        .ok()
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

// ---- 上游模型导入 ----

/// `GET /console/api/providers/{provider}/upstream-models`：带上已存凭据向上游要一份模型列表。
async fn upstream_models(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = guard_console(&state, &headers) {
        return response;
    }
    let snapshot = state.snapshot();
    let Some(provider_config) = snapshot.config.providers.get(&provider) else {
        return provider_missing(&provider);
    };
    let Some(credential) = state.credentials.resolve(&provider_config.api_key) else {
        return error_with(
            StatusCode::CONFLICT,
            "credential_not_configured",
            "该 provider 的凭据还没配置：先写入密钥值再拉取上游模型",
        );
    };
    match fetch_upstream_models(
        &state,
        &provider_config.base_url,
        provider_config.api,
        provider_config.api_version.as_deref(),
        &credential.value,
    )
    .await
    {
        Ok(models) => Json(json!({ "models": models })).into_response(),
        Err(failure) => failure.into_response(),
    }
}

/// `POST /console/api/upstream-models`：还没建 provider 时，用表单里填的地址与密钥直接探一次。
///
/// 只回模型列表，不落任何状态。
async fn probe_upstream_models(State(state): State<AppState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let payload: ProbePayload = match read_json(&state, &parts.headers, body).await {
        Ok(payload) => payload,
        Err(response) => return *response,
    };
    let api = match anybridge_litellm_core::ApiKind::from_str_exact(payload.api.trim()) {
        Some(api) => api,
        None => {
            return error_with(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "api 只接受 completions / anthropic / responses",
            )
        }
    };
    if payload.api_key.trim().is_empty() {
        return error_with(StatusCode::BAD_REQUEST, "invalid_request", "先填 API 密钥");
    }
    match fetch_upstream_models(
        &state,
        payload.base_url.trim(),
        api,
        payload.api_version.as_deref(),
        payload.api_key.trim(),
    )
    .await
    {
        Ok(models) => Json(json!({ "models": models })).into_response(),
        Err(failure) => failure.into_response(),
    }
}

/// 拉上游模型失败。用独立类型而不是 `Response`，避免 `Result` 变体过大。
struct UpstreamFailure {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl UpstreamFailure {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    fn into_response(self) -> Response {
        error_with(self.status, self.code, &self.message)
    }
}

/// 带上凭据请求 `{base_url}/models`，把响应归一化成 `[{id, name}]`。
/// 上游模型条目 → 控制台标准字段。别名映射覆盖常见网关/聚合器：
///
/// - `context_window` ← `context_window` | `context_length` | `max_input_tokens`
/// - `max_output_tokens` ← `max_output_tokens` | `max_completion_tokens`
/// - `input_modalities` ← `input_modalities` | `architecture.input_modalities` | `input`（只留 text/image）
/// - `reasoning` ← `reasoning` | `supports_reasoning` | `capabilities.reasoning`
///
/// 取不到的字段直接省略，不做推断。
fn normalize_upstream_model(id: &str, item: &Value) -> Value {
    let mut model = serde_json::Map::new();
    model.insert("id".into(), id.into());
    if let Some(name) = item
        .get("name")
        .or_else(|| item.get("display_name"))
        .and_then(Value::as_str)
    {
        model.insert("name".into(), name.into());
    }

    let context_window = ["context_window", "context_length", "max_input_tokens"]
        .iter()
        .find_map(|key| item.get(key).and_then(Value::as_u64));
    if let Some(value) = context_window {
        model.insert("context_window".into(), value.into());
    }

    let max_output_tokens = ["max_output_tokens", "max_completion_tokens"]
        .iter()
        .find_map(|key| item.get(key).and_then(Value::as_u64));
    if let Some(value) = max_output_tokens {
        model.insert("max_output_tokens".into(), value.into());
    }

    let mut modalities: Vec<&str> = Vec::new();
    for kind in ["input_modalities", "input"]
        .iter()
        .filter_map(|key| item.get(key).and_then(Value::as_array))
        .flatten()
        .chain(
            item.get("architecture")
                .and_then(|arch| arch.get("input_modalities"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        )
        .filter_map(Value::as_str)
        .filter(|kind| *kind == "text" || *kind == "image")
    {
        if !modalities.contains(&kind) {
            modalities.push(kind);
        }
    }
    if !modalities.is_empty() {
        model.insert("input_modalities".into(), modalities.into());
    }

    let reasoning = ["reasoning", "supports_reasoning"]
        .iter()
        .find_map(|key| item.get(key).and_then(Value::as_bool))
        .or_else(|| {
            item.get("capabilities")
                .and_then(|caps| caps.get("reasoning"))
                .and_then(Value::as_bool)
        });
    if let Some(supported) = reasoning {
        model.insert("reasoning".into(), supported.into());
    }

    Value::Object(model)
}

async fn fetch_upstream_models(
    state: &AppState,
    base_url: &str,
    api: anybridge_litellm_core::ApiKind,
    api_version: Option<&str>,
    api_key: &str,
) -> Result<Vec<Value>, UpstreamFailure> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut builder = state
        .client
        .get(&url)
        .timeout(std::time::Duration::from_secs(30));
    builder = match api {
        anybridge_litellm_core::ApiKind::Anthropic => builder
            .header("x-api-key", api_key)
            .header("anthropic-version", api_version.unwrap_or("2023-06-01")),
        _ => builder.header("authorization", format!("Bearer {api_key}")),
    };

    let response = builder.send().await.map_err(|err| {
        UpstreamFailure::new(
            StatusCode::BAD_GATEWAY,
            "upstream_error",
            format!("请求上游 {url} 失败：{err}"),
        )
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(UpstreamFailure::new(
            StatusCode::BAD_GATEWAY,
            "upstream_error",
            format!("上游 {url} 返回 HTTP {}", status.as_u16()),
        ));
    }

    let body: Value = match response.text().await {
        Ok(text) => serde_json::from_str(&text).map_err(|err| {
            UpstreamFailure::new(
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                format!("上游 {url} 响应不是 JSON：{err}"),
            )
        })?,
        Err(err) => {
            return Err(UpstreamFailure::new(
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                format!("读取上游 {url} 响应失败：{err}"),
            ))
        }
    };

    // OpenAI 用 `data` 包一层，Anthropic 直接给 `data`，部分自建服务给裸数组或 `models`。
    let items = body
        .get("data")
        .or_else(|| body.get("models"))
        .cloned()
        .or_else(|| body.is_array().then(|| body.clone()))
        .unwrap_or_else(|| Value::Array(Vec::new()));
    Ok(items
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let id = item.get("id").and_then(Value::as_str)?;
                    Some(normalize_upstream_model(id, item))
                })
                .collect()
        })
        .unwrap_or_default())
}

/// `PUT /console/api/providers/{provider}/models`：用整份 id 列表同步模型目录。
///
/// 控制台的「模型目录」一次提交整份列表：只增删差异，保留已有条目的其余字段。
async fn sync_models_route(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let payload: ImportPayload = match read_json(&state, &parts.headers, body).await {
        Ok(payload) => payload,
        Err(response) => return *response,
    };
    if !state.snapshot().config.providers.contains_key(&provider) {
        return provider_missing(&provider);
    }
    mutate(&state, move |document| {
        document.sync_models(&provider, &payload.ids)
    })
    .await
}

/// `POST /console/api/providers/{provider}/models/import`：批量导入上游模型。
///
/// 已存在的 id 跳过（幂等）；一条都没剩下时返回 400，避免「点了没反应」。
async fn import_models(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let payload: ImportPayload = match read_json(&state, &parts.headers, body).await {
        Ok(payload) => payload,
        Err(response) => return *response,
    };
    let snapshot = state.snapshot();
    let Some(provider_config) = snapshot.config.providers.get(&provider) else {
        return provider_missing(&provider);
    };

    let mut ids: Vec<String> = payload
        .ids
        .into_iter()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect();
    ids.sort();
    ids.dedup();
    let existing: Vec<&str> = provider_config
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    let fresh: Vec<String> = ids
        .into_iter()
        .filter(|id| !existing.contains(&id.as_str()))
        .collect();
    if fresh.is_empty() {
        return error_with(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "没有可导入的模型：所选项都已经存在",
        );
    }

    mutate(&state, move |document| {
        for id in &fresh {
            document.upsert_model(
                &provider,
                None,
                &ModelDraft {
                    id: id.clone(),
                    ..Default::default()
                },
            )?;
        }
        Ok(())
    })
    .await
}

// ---- 凭据 ----

/// `GET /console/api/credentials`：列出已登记的凭据（只有名字与状态，没有值）。
async fn list_credentials(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = guard_console(&state, &headers) {
        return response;
    }
    let items: Vec<Value> = state
        .credentials
        .list()
        .into_iter()
        .map(|(name, info)| credential_json(&name, &info))
        .collect();
    Json(json!({ "credentials": items })).into_response()
}

/// `PUT /console/api/credentials/{name}`：写入/轮换一条凭据。
///
/// 值是**单向下行**：写进凭据文件后不再出现在任何响应里。
async fn set_credential(
    State(state): State<AppState>,
    Path(name): Path<String>,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let payload: CredentialPayload = match read_json(&state, &parts.headers, body).await {
        Ok(payload) => payload,
        Err(response) => return *response,
    };
    let reference = match CredentialRef::parse_at(&format!("credential/{name}"), "credentials") {
        Ok(reference) => reference,
        Err(errors) => {
            return error_with(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &errors.to_string(),
            )
        }
    };
    match state.credentials.set(&reference, &payload.value) {
        Ok(()) => Json(json!({ "credential": credential_json(&name, &state.credentials.describe(&reference)) }))
            .into_response(),
        Err(err) => credential_error(err),
    }
}

/// `DELETE /console/api/credentials/{name}`：删除一条凭据。
async fn delete_credential(
    State(state): State<AppState>,
    Path(name): Path<String>,
    req: Request,
) -> Response {
    let (parts, _body) = req.into_parts();
    if let Some(response) = guard_console(&state, &parts.headers) {
        return response;
    }
    let reference = match CredentialRef::parse_at(&format!("credential/{name}"), "credentials") {
        Ok(reference) => reference,
        Err(errors) => {
            return error_with(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &errors.to_string(),
            )
        }
    };
    match state.credentials.unset(&reference) {
        Ok(()) => Json(json!({ "credential": credential_json(&name, &state.credentials.describe(&reference)) }))
            .into_response(),
        Err(err) => credential_error(err),
    }
}

fn credential_json(name: &str, info: &CredentialInfo) -> Value {
    json!({
        "name": name,
        "reference": format!("credential/{name}"),
        "configured": info.configured,
        "source": info.source,
        "writable": info.writable,
    })
}

fn credential_error(err: CredentialError) -> Response {
    match err {
        CredentialError::ReadOnly => error_with(
            StatusCode::CONFLICT,
            "read_only",
            "服务以只读方式启动，凭据不可写",
        ),
        CredentialError::Invalid(message) => {
            error_with(StatusCode::BAD_REQUEST, "invalid_request", &message)
        }
        CredentialError::Io(message) => error_with(
            StatusCode::INTERNAL_SERVER_ERROR,
            "credential_error",
            &message,
        ),
    }
}

// ---- 载荷 ----

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderPayload {
    /// provider id（配置键、路径段）。
    name: String,
    /// 可选展示名；为空时界面回退显示 id。
    #[serde(default)]
    display_name: Option<String>,
    api: String,
    base_url: String,
    /// 只接受 `credential/<name>` 引用；空/缺省表示更新时保留原引用。
    /// 密钥值走 `PUT /console/api/credentials/{name}`，永远不进配置文件。
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
            display_name: self.display_name,
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
struct ProbePayload {
    base_url: String,
    api: String,
    #[serde(default)]
    api_version: Option<String>,
    /// 表单里刚填的明文密钥；只用于本次探活，不落盘、不回显。
    api_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportPayload {
    ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialPayload {
    value: String,
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
            let credential = state.credentials.describe(&provider.api_key);
            json!({
                "name": name,
                "display_name": provider.name,
                "api": provider.api.as_str(),
                "base_path": format!("/llmproxy/{name}/v1"),
                "base_url": provider.base_url,
                "timeout": provider.timeout.as_secs(),
                "api_version": provider.api_version,
                "api_key": {
                    "kind": "credential",
                    "name": provider.api_key.name(),
                    "reference": provider.api_key.to_string(),
                    "configured": credential.configured,
                    "source": credential.source,
                    "writable": credential.writable,
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
        "config_file": state.config.path().map(|path| path.display().to_string()),
        "credentials_file": state
            .credentials
            .location()
            .map(|path| path.display().to_string()),
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
    // 浏览器走会话 cookie；脚本/curl 仍可用 `X-Console-Token` 头。
    if state.session.matches(cookie_value(headers, SESSION_COOKIE)) {
        return None;
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
