//! 配置解析与校验。
//!
//! 采用两遍解析：先按 `serde_yaml::Value` 遍历（收集未知键与全部字段错误），
//! 再组装成强类型结构。未知键在非 strict 模式下只产生 warning，strict 模式下
//! 与其它错误一样让加载失败。`os.environ/VAR` 在解析完成后展开，因此测试可用
//! `std::env::set_var` 注入假值。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use serde_yaml::{Mapping, Value};

use crate::domain::{ApiKind, Modality, ModelEntry, ModelRoute, ProviderConfig};
use crate::error::{ErrorKind, GatewayError};

/// 服务名，用于 `GET /` 的 `name` 字段。
pub const GATEWAY_NAME: &str = "anybridge-litellm";
pub const DEFAULT_BIND: &str = "127.0.0.1:4000";
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;
const RESERVED_PROVIDER_NAMES: [&str; 2] = ["v1", "llmproxy"];

const ROOT_KEYS: [&str; 3] = ["server", "auth", "providers"];
const SERVER_KEYS: [&str; 1] = ["bind"];
const AUTH_KEYS: [&str; 1] = ["master_key"];
const PROVIDER_KEYS: [&str; 6] = [
    "api",
    "base_url",
    "api_key",
    "timeout",
    "api_version",
    "models",
];
const MODEL_KEYS: [&str; 8] = [
    "id",
    "name",
    "context_window",
    "max_output_tokens",
    "input",
    "reasoning",
    "thinking_levels",
    "default_thinking_level",
];

/// 设计 §4：`api` 只在 provider 级提供，`alias` 不支持；模型级出现即启动失败。
const FORBIDDEN_MODEL_KEYS: [&str; 2] = ["api", "alias"];

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind: String,
}

#[derive(Clone)]
pub struct AuthConfig {
    pub master_key: String,
}

/// 手写 `Debug`：master key 永远不出现明文。
impl std::fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthConfig")
            .field("master_key", &"***")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub server: ServerConfig,
    pub auth: AuthConfig,
    pub providers: BTreeMap<String, ProviderConfig>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LoadOptions {
    /// 未知键是否视为错误（`--strict-config`）。
    pub strict: bool,
}

#[derive(Debug, Clone)]
pub struct LoadOutcome {
    pub config: GatewayConfig,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// 形如 `providers.openai.models[0].id`。
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigErrors(pub Vec<ConfigError>);

impl ConfigErrors {
    pub fn single(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self(vec![ConfigError {
            path: path.into(),
            message: message.into(),
        }])
    }
}

impl std::fmt::Display for ConfigErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, err) in self.0.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{}: {}", err.path, err.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigErrors {}

impl GatewayConfig {
    pub fn version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    /// 按 `(provider, model)` 精确查找；`model` 必须与配置里的 `id` 完全相等。
    pub fn route<'a>(
        &'a self,
        provider: &'a str,
        model: &str,
    ) -> Result<ModelRoute<'a>, GatewayError> {
        let provider_config = self.providers.get(provider).ok_or_else(|| {
            GatewayError::new(
                ErrorKind::ProviderNotFound,
                format!("provider `{provider}` 未配置"),
            )
        })?;
        let entry = provider_config
            .models
            .iter()
            .find(|m| m.id == model)
            .ok_or_else(|| {
                GatewayError::new(
                    ErrorKind::ModelNotFound,
                    format!("模型 `{model}` 不在 provider `{provider}` 的模型列表中"),
                )
            })?;
        Ok(ModelRoute {
            provider,
            api: provider_config.api,
            model: entry,
            base_url: &provider_config.base_url,
            api_key: &provider_config.api_key,
            api_version: provider_config.api_version.as_deref(),
            timeout: provider_config.timeout,
        })
    }

    pub fn provider_names(&self) -> Vec<&str> {
        self.providers.keys().map(String::as_str).collect()
    }

    /// `/llmproxy/{provider}/v1/models` 的 `data` 数组。
    pub fn model_cards(&self, provider: &str) -> Result<Vec<serde_json::Value>, GatewayError> {
        let provider_config = self.providers.get(provider).ok_or_else(|| {
            GatewayError::new(
                ErrorKind::ProviderNotFound,
                format!("provider `{provider}` 未配置"),
            )
        })?;
        Ok(provider_config
            .models
            .iter()
            .map(|model| model_card(provider, provider_config.api, model))
            .collect())
    }

    /// `GET /` 的发现响应。
    pub fn discovery(&self, version: &str) -> serde_json::Value {
        let providers: Vec<serde_json::Value> = self
            .providers
            .iter()
            .map(|(name, config)| {
                serde_json::json!({
                    "name": name,
                    "api": config.api.as_str(),
                    "base_path": format!("/llmproxy/{name}/v1"),
                })
            })
            .collect();
        serde_json::json!({
            "name": GATEWAY_NAME,
            "version": version,
            "providers": providers,
        })
    }

    /// `GET /health` 的响应。
    pub fn health(&self, version: &str) -> serde_json::Value {
        serde_json::json!({
            "ok": true,
            "version": version,
            "providers": self.provider_names(),
        })
    }
}

fn model_card(provider: &str, api: ApiKind, model: &ModelEntry) -> serde_json::Value {
    let mut card = serde_json::Map::new();
    card.insert("id".into(), model.id.as_str().into());
    card.insert("object".into(), "model".into());
    card.insert("created".into(), 0u64.into());
    card.insert("owned_by".into(), provider.into());
    if let Some(name) = &model.name {
        card.insert("name".into(), name.as_str().into());
    }
    if let Some(context_window) = model.context_window {
        card.insert("context_window".into(), context_window.into());
    }
    if let Some(max_output_tokens) = model.max_output_tokens {
        card.insert("max_output_tokens".into(), max_output_tokens.into());
    }

    let mut extra = serde_json::Map::new();
    if let Some(input) = &model.input {
        let modalities: Vec<&str> = input.iter().map(Modality::as_str).collect();
        extra.insert("input_modalities".into(), modalities.into());
    }
    // reasoning 与 thinking_levels 任一存在即声明能力；加载阶段已保证 false 不与档位并存。
    if model.reasoning.is_some() || !model.thinking_levels.is_empty() {
        let mut reasoning = serde_json::Map::new();
        reasoning.insert("supported".into(), model.reasoning.unwrap_or(true).into());
        reasoning.insert("param".into(), api.reasoning_param().into());
        reasoning.insert("levels".into(), model.thinking_levels.clone().into());
        if let Some(default) = &model.default_thinking_level {
            reasoning.insert("default".into(), default.as_str().into());
        }
        extra.insert("reasoning".into(), reasoning.into());
    }
    if !extra.is_empty() {
        card.insert("extra".into(), extra.into());
    }

    card.into()
}

/// 从 YAML 文本加载配置。
pub fn load_from_str(yaml: &str, opts: &LoadOptions) -> Result<LoadOutcome, ConfigErrors> {
    let value: Value = serde_yaml::from_str(yaml)
        .map_err(|e| ConfigErrors::single("config", format!("YAML 解析失败：{e}")))?;
    let mut ctx = Ctx::new(opts.strict);
    let config = ctx.build_config(&value);

    if !ctx.errors.is_empty() {
        return Err(ConfigErrors(ctx.errors));
    }
    let config = config.ok_or_else(|| ConfigErrors::single("config", "顶层必须是 YAML 映射"))?;
    Ok(LoadOutcome {
        config,
        warnings: ctx.warnings,
    })
}

/// 从文件加载配置。
pub fn load_from_path(path: &Path, opts: &LoadOptions) -> Result<LoadOutcome, ConfigErrors> {
    let yaml = std::fs::read_to_string(path).map_err(|e| {
        ConfigErrors::single(
            "config",
            format!("无法读取配置文件 {}：{e}", path.display()),
        )
    })?;
    load_from_str(&yaml, opts)
}

struct Ctx {
    strict: bool,
    errors: Vec<ConfigError>,
    warnings: Vec<String>,
}

impl Ctx {
    fn new(strict: bool) -> Self {
        Self {
            strict,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn err(&mut self, path: &str, message: impl Into<String>) {
        self.errors.push(ConfigError {
            path: path.to_string(),
            message: message.into(),
        });
    }

    fn unknown_key(&mut self, path: &str) {
        if self.strict {
            self.err(path, "unknown_field: 未知字段");
        } else {
            self.warnings.push(format!("unknown key: {path}"));
        }
    }

    fn check_keys(&mut self, mapping: &Mapping, base: &str, allowed: &[&str]) {
        for key in mapping.keys() {
            match key.as_str() {
                Some(name) if allowed.contains(&name) => {}
                Some(name) => self.unknown_key(&join_path(base, name)),
                None => self.err(location(base), "配置键必须是字符串"),
            }
        }
    }

    /// 模型级被明确禁用的键（`api` / `alias`）：无论是否 strict 都启动失败。
    fn check_model_keys(&mut self, mapping: &Mapping, base: &str) {
        for key in mapping.keys() {
            let Some(name) = key.as_str() else {
                self.err(location(base), "配置键必须是字符串");
                continue;
            };
            if MODEL_KEYS.contains(&name) {
                continue;
            }
            if FORBIDDEN_MODEL_KEYS.contains(&name) {
                self.err(
                    &join_path(base, name),
                    "unknown_field: 该字段只在 provider 级提供，模型级不支持",
                );
                continue;
            }
            self.unknown_key(&join_path(base, name));
        }
    }

    fn build_config(&mut self, root: &Value) -> Option<GatewayConfig> {
        let root = root.as_mapping()?;
        self.check_keys(root, "", &ROOT_KEYS);

        let bind = self.parse_server(root);
        let master_key = self.parse_auth(root);
        let providers = self.parse_providers(root);

        Some(GatewayConfig {
            server: ServerConfig { bind },
            auth: AuthConfig { master_key },
            providers,
        })
    }

    fn parse_server(&mut self, root: &Mapping) -> String {
        let mut bind = DEFAULT_BIND.to_string();
        match get(root, "server") {
            None | Some(Value::Null) => {}
            Some(Value::Mapping(mapping)) => {
                self.check_keys(mapping, "server", &SERVER_KEYS);
                if let Some(value) = self.optional_str(mapping, "bind", "server") {
                    if value.trim().is_empty() {
                        self.err("server.bind", "不能为空");
                    } else {
                        bind = value;
                    }
                }
            }
            Some(_) => self.err("server", "必须是映射"),
        }
        bind
    }

    fn parse_auth(&mut self, root: &Mapping) -> String {
        match get(root, "auth") {
            None | Some(Value::Null) => {
                self.err("auth.master_key", "必填");
                String::new()
            }
            Some(Value::Mapping(mapping)) => {
                self.check_keys(mapping, "auth", &AUTH_KEYS);
                match self.required_str(mapping, "master_key", "auth") {
                    Some(raw) => self
                        .expand_secret(&raw, "auth.master_key")
                        .unwrap_or_default(),
                    None => String::new(),
                }
            }
            Some(_) => {
                self.err("auth", "必须是映射");
                String::new()
            }
        }
    }

    fn parse_providers(&mut self, root: &Mapping) -> BTreeMap<String, ProviderConfig> {
        let mut providers = BTreeMap::new();
        match get(root, "providers") {
            // 允许空：控制台支持把 provider 删到 0，启动一份空配置也是合法状态。
            None | Some(Value::Null) => {}
            Some(Value::Mapping(mapping)) => {
                for (key, value) in mapping {
                    match key.as_str() {
                        Some(name) => {
                            if let Some(provider) = self.parse_provider(name, value) {
                                providers.insert(name.to_string(), provider);
                            }
                        }
                        None => self.err("providers", "provider 名必须是字符串"),
                    }
                }
            }
            Some(_) => self.err("providers", "必须是映射"),
        }
        providers
    }

    fn parse_provider(&mut self, name: &str, value: &Value) -> Option<ProviderConfig> {
        let base = format!("providers.{name}");
        if RESERVED_PROVIDER_NAMES.contains(&name) {
            self.err(&base, format!("provider 名 `{name}` 是保留字"));
        }
        if !valid_provider_name(name) {
            self.err(
                &base,
                format!("provider 名 `{name}` 不合法，需匹配 ^[a-z][a-z0-9-]{{0,31}}$"),
            );
        }

        let mapping = value.as_mapping().or_else(|| {
            self.err(&base, "必须是映射");
            None
        })?;
        self.check_keys(mapping, &base, &PROVIDER_KEYS);

        let api = self.parse_api(mapping, &base);
        let base_url = self.parse_base_url(mapping, &base);
        let api_key = match self.required_str(mapping, "api_key", &base) {
            Some(raw) => self.expand_secret(&raw, &format!("{base}.api_key")),
            None => None,
        };
        let timeout = self
            .optional_positive_u64(mapping, "timeout", &base)
            .map_or(
                Duration::from_secs(DEFAULT_TIMEOUT_SECS),
                Duration::from_secs,
            );
        let api_version = self.optional_str(mapping, "api_version", &base);
        let models = self.parse_models(mapping, &base);

        Some(ProviderConfig {
            api: api?,
            base_url: base_url?,
            api_key: api_key?,
            api_version,
            timeout,
            models,
        })
    }

    fn parse_api(&mut self, mapping: &Mapping, base: &str) -> Option<ApiKind> {
        let path = format!("{base}.api");
        match get(mapping, "api") {
            None | Some(Value::Null) => {
                self.err(&path, "必填");
                None
            }
            Some(Value::String(raw)) => {
                match ApiKind::from_str_exact(raw) {
                    Some(api) => Some(api),
                    None => {
                        self.err(
                        &path,
                        format!("不支持的 api 类型 `{raw}`，只接受 completions / anthropic / responses"),
                    );
                        None
                    }
                }
            }
            Some(_) => {
                self.err(&path, "必须是字符串");
                None
            }
        }
    }

    fn parse_base_url(&mut self, mapping: &Mapping, base: &str) -> Option<String> {
        let path = format!("{base}.base_url");
        match get(mapping, "base_url") {
            None | Some(Value::Null) => {
                self.err(&path, "必填");
                None
            }
            Some(Value::String(raw)) => {
                if !valid_base_url(raw) {
                    self.err(&path, format!("`{raw}` 不是合法的 http(s) URL"));
                    None
                } else {
                    Some(raw.trim_end_matches('/').to_string())
                }
            }
            Some(_) => {
                self.err(&path, "必须是字符串");
                None
            }
        }
    }

    fn parse_models(&mut self, mapping: &Mapping, base: &str) -> Vec<ModelEntry> {
        let mut models = Vec::new();
        let path = format!("{base}.models");
        match get(mapping, "models") {
            None | Some(Value::Null) => self.err(&path, "必填"),
            Some(Value::Sequence(items)) => {
                let mut seen = BTreeSet::new();
                for (index, item) in items.iter().enumerate() {
                    let item_base = format!("{path}[{index}]");
                    if let Some(entry) = self.parse_model(item, &item_base, &mut seen) {
                        models.push(entry);
                    }
                }
            }
            Some(_) => self.err(&path, "必须是数组"),
        }
        models
    }

    fn parse_model(
        &mut self,
        value: &Value,
        base: &str,
        seen: &mut BTreeSet<String>,
    ) -> Option<ModelEntry> {
        let mapping = value.as_mapping().or_else(|| {
            self.err(base, "必须是映射");
            None
        })?;
        self.check_model_keys(mapping, base);

        let id = self.required_str(mapping, "id", base).and_then(|id| {
            if id.is_empty() {
                self.err(&format!("{base}.id"), "不能为空");
                None
            } else {
                Some(id)
            }
        });
        if let Some(id) = &id {
            if !seen.insert(id.clone()) {
                self.err(
                    &format!("{base}.id"),
                    format!("模型 id `{id}` 在同一 provider 内重复"),
                );
            }
        }

        let name = self.optional_str(mapping, "name", base).and_then(|name| {
            if name.is_empty() {
                self.err(&format!("{base}.name"), "不能为空");
                None
            } else {
                Some(name)
            }
        });
        let context_window = self.optional_positive_u64(mapping, "context_window", base);
        let max_output_tokens = self.optional_positive_u64(mapping, "max_output_tokens", base);
        if let (Some(context_window), Some(max_output_tokens)) = (context_window, max_output_tokens)
        {
            if max_output_tokens > context_window {
                self.err(
                    &format!("{base}.max_output_tokens"),
                    format!(
                        "max_output_tokens ({max_output_tokens}) 不能大于 context_window ({context_window})"
                    ),
                );
            }
        }

        let input = self.parse_input(mapping, base);
        let thinking_levels = self.parse_thinking_levels(mapping, base);

        let reasoning = match (
            self.optional_bool(mapping, "reasoning", base),
            thinking_levels.is_empty(),
        ) {
            (Some(false), false) => {
                self.err(
                    &format!("{base}.reasoning"),
                    "reasoning 为 false 时不能配置 thinking_levels",
                );
                Some(false)
            }
            (Some(flag), _) => Some(flag),
            // thinking_levels 非空且未写 reasoning → 视为 true
            (None, false) => Some(true),
            (None, true) => None,
        };

        let default_thinking_level = self
            .optional_str(mapping, "default_thinking_level", base)
            .and_then(|level| {
                if thinking_levels.contains(&level) {
                    Some(level)
                } else {
                    self.err(
                        &format!("{base}.default_thinking_level"),
                        format!("`{level}` 不在 thinking_levels 内"),
                    );
                    None
                }
            });

        Some(ModelEntry {
            id: id?,
            name,
            context_window,
            max_output_tokens,
            input,
            reasoning,
            thinking_levels,
            default_thinking_level,
        })
    }

    fn parse_input(&mut self, mapping: &Mapping, base: &str) -> Option<Vec<Modality>> {
        let items = self.optional_seq(mapping, "input", base)?;
        let mut modalities: Vec<Modality> = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let path = format!("{base}.input[{index}]");
            match item.as_str() {
                Some("text") => {
                    if !modalities.contains(&Modality::Text) {
                        modalities.push(Modality::Text);
                    }
                }
                Some("image") => {
                    if !modalities.contains(&Modality::Image) {
                        modalities.push(Modality::Image);
                    }
                }
                _ => self.err(&path, "只接受 text / image"),
            }
        }
        if modalities.contains(&Modality::Image) && !modalities.contains(&Modality::Text) {
            self.err(&format!("{base}.input"), "含 image 时必须同时含 text");
        }
        Some(modalities)
    }

    fn parse_thinking_levels(&mut self, mapping: &Mapping, base: &str) -> Vec<String> {
        let Some(items) = self.optional_seq(mapping, "thinking_levels", base) else {
            return Vec::new();
        };
        let mut levels: Vec<String> = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let path = format!("{base}.thinking_levels[{index}]");
            match item.as_str() {
                Some(level) if !level.is_empty() => {
                    if !levels.iter().any(|existing| existing == level) {
                        levels.push(level.to_string());
                    }
                }
                _ => self.err(&path, "必须是非空字符串"),
            }
        }
        levels
    }

    /// 展开 `os.environ/VAR`；字面量原样返回。空值视为未配置。
    fn expand_secret(&mut self, raw: &str, path: &str) -> Option<String> {
        let Some(var) = raw.strip_prefix("os.environ/") else {
            if raw.trim().is_empty() {
                self.err(path, "不能为空");
                return None;
            }
            return Some(raw.to_string());
        };

        match std::env::var(var) {
            Ok(value) if !value.is_empty() => Some(value),
            Ok(_) => {
                self.err(path, format!("环境变量 `{var}` 为空"));
                None
            }
            Err(_) => {
                self.err(path, format!("环境变量 `{var}` 未设置"));
                None
            }
        }
    }

    // ---- 字段读取辅助 ----

    fn required_str(&mut self, mapping: &Mapping, key: &str, base: &str) -> Option<String> {
        let path = join_path(base, key);
        match get(mapping, key) {
            None | Some(Value::Null) => {
                self.err(&path, "必填");
                None
            }
            Some(Value::String(value)) => Some(value.clone()),
            Some(_) => {
                self.err(&path, "必须是字符串");
                None
            }
        }
    }

    fn optional_str(&mut self, mapping: &Mapping, key: &str, base: &str) -> Option<String> {
        let path = join_path(base, key);
        match get(mapping, key) {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value.clone()),
            Some(_) => {
                self.err(&path, "必须是字符串");
                None
            }
        }
    }

    fn optional_bool(&mut self, mapping: &Mapping, key: &str, base: &str) -> Option<bool> {
        let path = join_path(base, key);
        match get(mapping, key) {
            None | Some(Value::Null) => None,
            Some(Value::Bool(value)) => Some(*value),
            Some(_) => {
                self.err(&path, "必须是布尔值");
                None
            }
        }
    }

    fn optional_positive_u64(&mut self, mapping: &Mapping, key: &str, base: &str) -> Option<u64> {
        let path = join_path(base, key);
        match get(mapping, key) {
            None | Some(Value::Null) => None,
            Some(Value::Number(number)) => match number.as_u64() {
                Some(value) if value > 0 => Some(value),
                _ => {
                    self.err(&path, "必须是正整数");
                    None
                }
            },
            Some(_) => {
                self.err(&path, "必须是正整数");
                None
            }
        }
    }

    fn optional_seq<'a>(
        &mut self,
        mapping: &'a Mapping,
        key: &str,
        base: &str,
    ) -> Option<&'a [Value]> {
        let path = join_path(base, key);
        match get(mapping, key) {
            None | Some(Value::Null) => None,
            Some(Value::Sequence(items)) => Some(items.as_slice()),
            Some(_) => {
                self.err(&path, "必须是数组");
                None
            }
        }
    }
}

fn get<'a>(mapping: &'a Mapping, key: &str) -> Option<&'a Value> {
    mapping.iter().find_map(|(k, v)| match k.as_str() {
        Some(name) if name == key => Some(v),
        _ => None,
    })
}

fn join_path(base: &str, key: &str) -> String {
    if base.is_empty() {
        key.to_string()
    } else {
        format!("{base}.{key}")
    }
}

fn location(base: &str) -> &str {
    if base.is_empty() {
        "config"
    } else {
        base
    }
}

fn valid_provider_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let Some((&first, rest)) = bytes.split_first() else {
        return false;
    };
    first.is_ascii_lowercase()
        && rest.len() <= 31
        && rest
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn valid_base_url(url: &str) -> bool {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"));
    let Some(rest) = rest else {
        return false;
    };
    if rest.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    let authority = rest.split('/').next().unwrap_or("");
    !authority.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../../../config.example.yaml");

    fn load(yaml: &str) -> Result<LoadOutcome, ConfigErrors> {
        load_from_str(yaml, &LoadOptions { strict: false })
    }

    fn load_strict(yaml: &str) -> Result<LoadOutcome, ConfigErrors> {
        load_from_str(yaml, &LoadOptions { strict: true })
    }

    fn errors_of(yaml: &str) -> Vec<ConfigError> {
        load(yaml).expect_err("应加载失败").0
    }

    fn paths(errors: &[ConfigError]) -> Vec<&str> {
        errors.iter().map(|e| e.path.as_str()).collect()
    }

    fn set_example_env() {
        // Rust 2021 下 set_var 是安全函数。
        std::env::set_var("ANYBRIDGE_TEST_MASTER_KEY", "test-master-key");
        std::env::set_var("ANYBRIDGE_TEST_OPENAI_KEY", "sk-openai");
        std::env::set_var("ANYBRIDGE_TEST_ANTHROPIC_KEY", "sk-anthropic");
    }

    fn example_with_env() -> String {
        set_example_env();
        EXAMPLE
            .replace(
                "os.environ/GATEWAY_MASTER_KEY",
                "os.environ/ANYBRIDGE_TEST_MASTER_KEY",
            )
            .replace(
                "os.environ/OPENAI_API_KEY",
                "os.environ/ANYBRIDGE_TEST_OPENAI_KEY",
            )
            .replace(
                "os.environ/ANTHROPIC_API_KEY",
                "os.environ/ANYBRIDGE_TEST_ANTHROPIC_KEY",
            )
    }

    const MINIMAL: &str = r#"
server:
  bind: "127.0.0.1:4000"
auth:
  master_key: literal-key
providers:
  openai:
    api: completions
    base_url: https://api.openai.com/v1/
    api_key: sk-test
    models:
      - id: gpt-4o
"#;

    #[test]
    fn example_config_loads() {
        let outcome = load(&example_with_env()).expect("示例配置应可解析");
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
        let config = outcome.config;
        assert_eq!(config.server.bind, "127.0.0.1:4000");
        assert_eq!(config.auth.master_key, "test-master-key");
        assert_eq!(
            config.provider_names(),
            vec!["anthropic", "local", "openai", "openai-responses"]
        );

        let openai = config.providers.get("openai").unwrap();
        assert_eq!(openai.api, ApiKind::Completions);
        assert_eq!(openai.base_url, "https://api.openai.com/v1");
        assert_eq!(openai.api_key, "sk-openai");
        assert_eq!(openai.timeout, Duration::from_secs(600));
        let model = &openai.models[0];
        assert_eq!(model.id, "gpt-4o");
        assert_eq!(model.name.as_deref(), Some("GPT-4o"));
        assert_eq!(model.context_window, Some(128_000));
        assert_eq!(model.max_output_tokens, Some(16_384));
        assert_eq!(model.input, Some(vec![Modality::Text, Modality::Image]));
        assert_eq!(model.reasoning, Some(true));
        assert_eq!(model.default_thinking_level.as_deref(), Some("high"));

        assert_eq!(
            config.providers.get("local").unwrap().base_url,
            "http://127.0.0.1:11434/v1"
        );
        assert_eq!(
            config.providers.get("anthropic").unwrap().api,
            ApiKind::Anthropic
        );
        assert_eq!(
            config.providers.get("openai-responses").unwrap().models[0].id,
            "gpt-5.6-luna"
        );
    }

    #[test]
    fn route_hits_and_misses() {
        let config = load(MINIMAL).unwrap().config;
        let route = config.route("openai", "gpt-4o").unwrap();
        assert_eq!(route.provider, "openai");
        assert_eq!(route.api, ApiKind::Completions);
        assert_eq!(route.model.id, "gpt-4o");
        assert_eq!(route.base_url, "https://api.openai.com/v1");
        assert_eq!(route.api_key, "sk-test");
        assert_eq!(route.api_version, None);
        assert_eq!(route.timeout, Duration::from_secs(600));

        let missing_provider = config.route("nope", "gpt-4o").unwrap_err();
        assert_eq!(missing_provider.kind, ErrorKind::ProviderNotFound);
        assert_eq!(missing_provider.status(), 404);
        assert!(missing_provider.message.contains("nope"));

        let missing_model = config.route("openai", "GPT-4O").unwrap_err();
        assert_eq!(missing_model.kind, ErrorKind::ModelNotFound);
        assert_eq!(missing_model.status(), 404);
        assert!(missing_model.message.contains("GPT-4O"));
    }

    #[test]
    fn model_cards_top_level_keys_are_exactly_whitelisted() {
        let config = load(&example_with_env()).unwrap().config;
        let cards = config.model_cards("openai").unwrap();
        assert_eq!(cards.len(), 1);
        let card = cards[0].as_object().unwrap();
        let mut keys: Vec<&str> = card.keys().map(String::as_str).collect();
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
        assert_eq!(card["id"], "gpt-4o");
        assert_eq!(card["object"], "model");
        assert_eq!(card["created"], 0);
        assert_eq!(card["owned_by"], "openai");

        let extra = card["extra"].as_object().unwrap();
        let mut extra_keys: Vec<&str> = extra.keys().map(String::as_str).collect();
        extra_keys.sort_unstable();
        assert_eq!(extra_keys, vec!["input_modalities", "reasoning"]);
        assert_eq!(
            extra["input_modalities"],
            serde_json::json!(["text", "image"])
        );

        let reasoning = extra["reasoning"].as_object().unwrap();
        let mut reasoning_keys: Vec<&str> = reasoning.keys().map(String::as_str).collect();
        reasoning_keys.sort_unstable();
        assert_eq!(
            reasoning_keys,
            vec!["default", "levels", "param", "supported"]
        );
        assert_eq!(reasoning["supported"], true);
        assert_eq!(reasoning["param"], "reasoning_effort");
        assert_eq!(
            reasoning["levels"],
            serde_json::json!(["none", "low", "medium", "high"])
        );
        assert_eq!(reasoning["default"], "high");

        // 未配 input / reasoning 的模型不出现 extra
        let local_card = &config.model_cards("local").unwrap()[0];
        assert!(local_card.get("extra").is_none());
    }

    #[test]
    fn reasoning_param_is_derived_from_api() {
        for (api, expected) in [
            ("completions", "reasoning_effort"),
            ("anthropic", "thinking"),
            ("responses", "reasoning.effort"),
        ] {
            let yaml = MINIMAL.replace("api: completions", &format!("api: {api}"));
            let yaml = yaml.replace(
                "      - id: gpt-4o",
                "      - id: gpt-4o\n        thinking_levels: [low]",
            );
            let config = load(&yaml).unwrap().config;
            let card = &config.model_cards("openai").unwrap()[0];
            assert_eq!(card["extra"]["reasoning"]["param"], expected, "{api}");
            assert_eq!(card["extra"]["reasoning"]["supported"], true);
            assert_eq!(
                card["extra"]["reasoning"]["levels"],
                serde_json::json!(["low"])
            );
        }
    }

    #[test]
    fn responses_reasoning_param() {
        let config = load(&example_with_env()).unwrap().config;
        let cards = config.model_cards("openai-responses").unwrap();
        assert_eq!(cards[0]["id"], "gpt-5.6-luna");
        assert!(cards[0].get("extra").is_none());
        assert!(cards[0].get("name").is_none());
        assert!(cards[0].get("context_window").is_none());
        assert!(cards[0].get("max_output_tokens").is_none());
    }

    #[test]
    fn model_cards_unknown_provider() {
        let config = load(MINIMAL).unwrap().config;
        let err = config.model_cards("nope").unwrap_err();
        assert_eq!(err.kind, ErrorKind::ProviderNotFound);
    }

    #[test]
    fn discovery_and_health_shapes() {
        let config = load(&example_with_env()).unwrap().config;

        let discovery = config.discovery("1.2.3");
        assert_eq!(discovery["name"], "anybridge-litellm");
        assert_eq!(discovery["version"], "1.2.3");
        let providers = discovery["providers"].as_array().unwrap();
        assert_eq!(providers.len(), 4);
        let openai = providers
            .iter()
            .find(|p| p["name"] == "openai")
            .unwrap()
            .as_object()
            .unwrap();
        let mut keys: Vec<&str> = openai.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["api", "base_path", "name"]);
        assert_eq!(openai["api"], "completions");
        assert_eq!(openai["base_path"], "/llmproxy/openai/v1");

        let health = config.health("1.2.3");
        assert_eq!(health["ok"], true);
        assert_eq!(health["version"], "1.2.3");
        assert_eq!(
            health["providers"],
            serde_json::json!(["anthropic", "local", "openai", "openai-responses"])
        );

        assert_eq!(GatewayConfig::version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn reserved_provider_names_fail() {
        for name in ["v1", "llmproxy"] {
            let yaml = format!(
                r#"
auth:
  master_key: k
providers:
  {name}:
    api: completions
    base_url: https://example.com/v1
    api_key: k
    models:
      - id: m
"#
            );
            let errors = errors_of(&yaml);
            assert!(
                errors.iter().any(|e| e.message.contains("保留字")),
                "{name}: {errors:?}"
            );
        }
    }

    #[test]
    fn invalid_provider_name_fails() {
        for name in ["OpenAI", "1openai", "open_ai", "openai.", ""] {
            let yaml = format!(
                r#"
auth:
  master_key: k
providers:
  "{name}":
    api: completions
    base_url: https://example.com/v1
    api_key: k
    models:
      - id: m
"#
            );
            assert!(!load(&yaml).is_ok(), "{name:?} 应加载失败");
        }
    }

    #[test]
    fn provider_name_max_length() {
        let name_32 = format!("a{}", "b".repeat(31));
        assert!(valid_provider_name(&name_32));
        let name_33 = format!("a{}", "b".repeat(32));
        assert!(!valid_provider_name(&name_33));
        assert!(valid_provider_name("openai-responses"));
        assert!(!valid_provider_name("-openai"));
    }

    #[test]
    fn duplicate_model_id_fails() {
        let errors = errors_of(
            r#"
auth:
  master_key: k
providers:
  p:
    api: completions
    base_url: https://example.com/v1
    api_key: k
    models:
      - id: dup
      - id: dup
"#,
        );
        assert_eq!(paths(&errors), vec!["providers.p.models[1].id"]);
        assert!(errors[0].message.contains("重复"));
    }

    #[test]
    fn model_level_api_and_alias_are_unknown_fields() {
        for key in ["api: anthropic", "alias: other"] {
            let yaml = format!(
                r#"
auth:
  master_key: k
providers:
  p:
    api: completions
    base_url: https://example.com/v1
    api_key: k
    models:
      - id: m
        {key}
"#
            );
            // 模型级 `api` / `alias` 无论是否 strict 都失败（设计 §4）。
            let errors = errors_of(&yaml);
            let unknown = errors
                .iter()
                .find(|e| e.message.contains("unknown_field"))
                .unwrap_or_else(|| panic!("{key} 应报 unknown_field，实际 {errors:?}"));
            let field = key.split(':').next().unwrap();
            assert_eq!(unknown.path, format!("providers.p.models[0].{field}"));
        }
    }

    #[test]
    fn max_output_tokens_greater_than_context_window_fails() {
        let errors = errors_of(
            r#"
auth:
  master_key: k
providers:
  p:
    api: completions
    base_url: https://example.com/v1
    api_key: k
    models:
      - id: m
        context_window: 100
        max_output_tokens: 101
"#,
        );
        assert_eq!(
            paths(&errors),
            vec!["providers.p.models[0].max_output_tokens"]
        );
        assert!(errors[0].message.contains("context_window"));
    }

    #[test]
    fn default_thinking_level_must_be_known() {
        let errors = errors_of(
            r#"
auth:
  master_key: k
providers:
  p:
    api: completions
    base_url: https://example.com/v1
    api_key: k
    models:
      - id: m
        thinking_levels: [low, high]
        default_thinking_level: medium
"#,
        );
        assert_eq!(
            paths(&errors),
            vec!["providers.p.models[0].default_thinking_level"]
        );
    }

    #[test]
    fn reasoning_defaults_true_when_levels_present() {
        let config = load(
            r#"
auth:
  master_key: k
providers:
  p:
    api: anthropic
    base_url: https://example.com/v1
    api_key: k
    models:
      - id: m
        thinking_levels: [low, high]
"#,
        )
        .unwrap()
        .config;
        let model = &config.providers.get("p").unwrap().models[0];
        assert_eq!(model.reasoning, Some(true));
        let card = &config.model_cards("p").unwrap()[0];
        assert_eq!(card["extra"]["reasoning"]["supported"], true);
        assert_eq!(card["extra"]["reasoning"]["param"], "thinking");
    }

    #[test]
    fn reasoning_false_with_levels_fails() {
        let errors = errors_of(
            r#"
auth:
  master_key: k
providers:
  p:
    api: completions
    base_url: https://example.com/v1
    api_key: k
    models:
      - id: m
        reasoning: false
        thinking_levels: [low]
"#,
        );
        assert_eq!(paths(&errors), vec!["providers.p.models[0].reasoning"]);
    }

    #[test]
    fn input_image_requires_text() {
        let errors = errors_of(
            r#"
auth:
  master_key: k
providers:
  p:
    api: completions
    base_url: https://example.com/v1
    api_key: k
    models:
      - id: m
        input: [image]
"#,
        );
        assert_eq!(paths(&errors), vec!["providers.p.models[0].input"]);

        let errors = errors_of(
            r#"
auth:
  master_key: k
providers:
  p:
    api: completions
    base_url: https://example.com/v1
    api_key: k
    models:
      - id: m
        input: [text, audio]
"#,
        );
        assert_eq!(paths(&errors), vec!["providers.p.models[0].input[1]"]);
    }

    #[test]
    fn input_deduplicates() {
        let config = load(
            r#"
auth:
  master_key: k
providers:
  p:
    api: completions
    base_url: https://example.com/v1
    api_key: k
    models:
      - id: m
        input: [text, image, image, text]
"#,
        )
        .unwrap()
        .config;
        assert_eq!(
            config.providers.get("p").unwrap().models[0].input,
            Some(vec![Modality::Text, Modality::Image])
        );
    }

    #[test]
    fn unknown_keys_warn_when_not_strict() {
        let yaml = r#"
server:
  bind: 127.0.0.1:4000
  extra: 1
auth:
  master_key: k
  rotate: false
providers:
  openai:
    api: completions
    base_url: https://example.com/v1
    api_key: k
    region: us
    models:
      - id: m
        cost: 1
"#;
        let outcome = load(yaml).unwrap();
        assert_eq!(
            outcome.warnings,
            vec![
                "unknown key: server.extra",
                "unknown key: auth.rotate",
                "unknown key: providers.openai.region",
                "unknown key: providers.openai.models[0].cost",
            ]
        );

        let errors = load_strict(yaml).expect_err("strict 应失败").0;
        let mut got = paths(&errors);
        got.sort_unstable();
        assert_eq!(
            got,
            vec![
                "auth.rotate",
                "providers.openai.models[0].cost",
                "providers.openai.region",
                "server.extra",
            ]
        );
        assert!(errors.iter().all(|e| e.message.contains("unknown_field")));
    }

    #[test]
    fn top_level_unknown_key_path() {
        let outcome = load(&format!("{MINIMAL}\nfoo: bar\n")).unwrap();
        assert_eq!(outcome.warnings, vec!["unknown key: foo"]);
    }

    #[test]
    fn empty_providers_is_allowed() {
        let outcome = load(
            r#"
auth:
  master_key: k
providers: {}
"#,
        )
        .unwrap();
        assert!(outcome.config.providers.is_empty());

        // 整段省略 `providers` 也等价于空配置。
        let outcome = load(
            r#"
auth:
  master_key: k
"#,
        )
        .unwrap();
        assert!(outcome.config.providers.is_empty());
    }

    #[test]
    fn missing_required_fields_are_reported_with_paths() {
        let errors = errors_of(
            r#"
providers:
  p:
    models:
      - {}
"#,
        );
        let got = paths(&errors);
        assert!(got.contains(&"auth.master_key"), "{got:?}");
        assert!(got.contains(&"providers.p.api"), "{got:?}");
        assert!(got.contains(&"providers.p.base_url"), "{got:?}");
        assert!(got.contains(&"providers.p.api_key"), "{got:?}");
        assert!(got.contains(&"providers.p.models[0].id"), "{got:?}");
    }

    #[test]
    fn invalid_api_value_fails() {
        let errors = errors_of(&MINIMAL.replace("api: completions", "api: chat"));
        assert_eq!(paths(&errors), vec!["providers.openai.api"]);
        assert!(errors[0].message.contains("completions"));

        let errors = errors_of(&MINIMAL.replace("api: completions", "api: Completions"));
        assert_eq!(paths(&errors), vec!["providers.openai.api"]);
    }

    #[test]
    fn invalid_base_url_fails() {
        for url in [
            "ftp://example.com/v1",
            "example.com/v1",
            "https://",
            "http://exa mple.com",
        ] {
            let errors = errors_of(&MINIMAL.replace("https://api.openai.com/v1/", url));
            assert_eq!(paths(&errors), vec!["providers.openai.base_url"], "{url}");
        }
    }

    #[test]
    fn base_url_trailing_slash_is_stripped() {
        let config = load(MINIMAL).unwrap().config;
        assert_eq!(
            config.providers.get("openai").unwrap().base_url,
            "https://api.openai.com/v1"
        );
    }

    #[test]
    fn timeout_defaults_and_validates() {
        let config = load(MINIMAL).unwrap().config;
        assert_eq!(
            config.providers.get("openai").unwrap().timeout,
            Duration::from_secs(DEFAULT_TIMEOUT_SECS)
        );

        let with_timeout = MINIMAL.replace("api_key: sk-test", "api_key: sk-test\n    timeout: 30");
        let config = load(&with_timeout).unwrap().config;
        assert_eq!(
            config.providers.get("openai").unwrap().timeout,
            Duration::from_secs(30)
        );

        for bad in ["0", "-1", "1.5", "true", "\"30\""] {
            let yaml = MINIMAL.replace(
                "api_key: sk-test",
                &format!("api_key: sk-test\n    timeout: {bad}"),
            );
            let errors = errors_of(&yaml);
            assert_eq!(paths(&errors), vec!["providers.openai.timeout"], "{bad}");
        }
    }

    #[test]
    fn context_window_must_be_positive() {
        for bad in ["0", "-5"] {
            let yaml = MINIMAL.replace(
                "      - id: gpt-4o",
                &format!("      - id: gpt-4o\n        context_window: {bad}"),
            );
            let errors = errors_of(&yaml);
            assert_eq!(
                paths(&errors),
                vec!["providers.openai.models[0].context_window"],
                "{bad}"
            );
        }
    }

    #[test]
    fn api_version_is_optional() {
        let yaml = MINIMAL.replace(
            "api_key: sk-test",
            "api_key: sk-test\n    api_version: \"2024-10-21\"",
        );
        let config = load(&yaml).unwrap().config;
        let route = config.route("openai", "gpt-4o").unwrap();
        assert_eq!(route.api_version, Some("2024-10-21"));
    }

    #[test]
    fn literal_api_key_and_master_key_are_kept() {
        let config = load(MINIMAL).unwrap().config;
        assert_eq!(config.auth.master_key, "literal-key");
        assert_eq!(config.providers.get("openai").unwrap().api_key, "sk-test");
    }

    #[test]
    fn missing_environment_variable_fails() {
        std::env::remove_var("ANYBRIDGE_TEST_MISSING_VAR");
        let yaml = MINIMAL.replace(
            "api_key: sk-test",
            "api_key: os.environ/ANYBRIDGE_TEST_MISSING_VAR",
        );
        let errors = errors_of(&yaml);
        assert_eq!(paths(&errors), vec!["providers.openai.api_key"]);
        assert!(errors[0].message.contains("未设置"), "{errors:?}");
    }

    #[test]
    fn environment_variable_is_expanded() {
        std::env::set_var("ANYBRIDGE_TEST_EXPANDED_KEY", "expanded-value");
        let yaml = MINIMAL.replace(
            "api_key: sk-test",
            "api_key: os.environ/ANYBRIDGE_TEST_EXPANDED_KEY",
        );
        let config = load(&yaml).unwrap().config;
        assert_eq!(
            config.providers.get("openai").unwrap().api_key,
            "expanded-value"
        );
    }

    #[test]
    fn empty_secret_fails() {
        let errors = errors_of(&MINIMAL.replace("api_key: sk-test", "api_key: \"\""));
        assert_eq!(paths(&errors), vec!["providers.openai.api_key"]);
    }

    #[test]
    fn yaml_syntax_error_fails() {
        let errors = load_from_str("server: [", &LoadOptions::default()).unwrap_err();
        assert_eq!(errors.0[0].path, "config");
        assert!(errors.to_string().contains("YAML"));
    }

    #[test]
    fn non_mapping_root_fails() {
        let errors = load_from_str("- a\n- b\n", &LoadOptions::default()).unwrap_err();
        assert_eq!(errors.0[0].path, "config");
    }

    #[test]
    fn models_must_be_a_sequence() {
        let errors = errors_of(
            r#"
auth:
  master_key: k
providers:
  p:
    api: completions
    base_url: https://example.com/v1
    api_key: k
    models: nope
"#,
        );
        assert_eq!(paths(&errors), vec!["providers.p.models"]);
    }

    #[test]
    fn provider_must_be_a_mapping() {
        let errors = errors_of(
            r#"
auth:
  master_key: k
providers:
  p: nope
"#,
        );
        assert_eq!(paths(&errors), vec!["providers.p"]);
    }

    #[test]
    fn load_from_path_reads_file() {
        let dir =
            std::env::temp_dir().join(format!("anybridge-litellm-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        std::fs::write(&path, MINIMAL).unwrap();
        let outcome = load_from_path(&path, &LoadOptions::default()).unwrap();
        assert_eq!(outcome.config.provider_names(), vec!["openai"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_from_path_missing_file_fails() {
        let errors = load_from_path(
            Path::new("/nonexistent/anybridge.yaml"),
            &LoadOptions::default(),
        )
        .unwrap_err();
        assert_eq!(errors.0[0].path, "config");
        assert_eq!(errors.to_string().lines().count(), 1);
    }

    #[test]
    fn config_errors_display_one_per_line() {
        let errors = ConfigErrors(vec![
            ConfigError {
                path: "providers.a".into(),
                message: "坏".into(),
            },
            ConfigError {
                path: "providers.b".into(),
                message: "也坏".into(),
            },
        ]);
        assert_eq!(errors.to_string(), "providers.a: 坏\nproviders.b: 也坏");
    }
}
