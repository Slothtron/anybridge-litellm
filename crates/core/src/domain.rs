//! 纯领域类型，不做任何 IO。

use std::time::Duration;

use crate::credential::CredentialRef;

/// 上游 API 协议类型。它是声明，不是翻译器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKind {
    /// OpenAI Chat Completions（`/chat/completions`，不是 legacy `/completions`）
    Completions,
    /// Anthropic Messages（`/messages`）
    Anthropic,
    /// OpenAI Responses（`/responses`）
    Responses,
}

impl ApiKind {
    /// 配置里的字面量。
    pub fn as_str(&self) -> &'static str {
        match self {
            ApiKind::Completions => "completions",
            ApiKind::Anthropic => "anthropic",
            ApiKind::Responses => "responses",
        }
    }

    /// 精确匹配，不做大小写归一。
    pub fn from_str_exact(s: &str) -> Option<Self> {
        match s {
            "completions" => Some(ApiKind::Completions),
            "anthropic" => Some(ApiKind::Anthropic),
            "responses" => Some(ApiKind::Responses),
            _ => None,
        }
    }

    /// `/models` 的 `extra.reasoning.param`，由 `api` 推导。
    pub fn reasoning_param(&self) -> &'static str {
        match self {
            ApiKind::Completions => "reasoning_effort",
            ApiKind::Anthropic => "thinking",
            ApiKind::Responses => "reasoning.effort",
        }
    }

    /// 仅 anthropic 有默认 `anthropic-version`。
    pub fn default_api_version(&self) -> Option<&'static str> {
        match self {
            ApiKind::Anthropic => Some("2023-06-01"),
            ApiKind::Completions | ApiKind::Responses => None,
        }
    }
}

/// 输入模态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modality {
    Text,
    Image,
}

impl Modality {
    pub fn as_str(&self) -> &'static str {
        match self {
            Modality::Text => "text",
            Modality::Image => "image",
        }
    }
}

/// 配置里的单个模型条目。`id` 即上游模型 id，也是客户端传的 `model`。
#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub id: String,
    pub name: Option<String>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub input: Option<Vec<Modality>>,
    /// `thinking_levels` 非空且未显式配置时，加载阶段已展开为 `Some(true)`。
    pub reasoning: Option<bool>,
    pub thinking_levels: Vec<String>,
}

/// 单个 provider 的解析后配置。
///
/// `api_key` 是**凭据引用**，不是明文：值由凭据存储在每次请求时解析，
/// 因此轮换密钥不需要重启。
#[derive(Clone)]
pub struct ProviderConfig {
    pub api: ApiKind,
    /// 可选展示名；为空时界面回退显示 provider id。
    pub name: Option<String>,
    pub base_url: String,
    pub api_key: CredentialRef,
    pub api_version: Option<String>,
    pub timeout: Duration,
    pub models: Vec<ModelEntry>,
}

/// 手写 `Debug`：只显示引用名，永远不出现密钥值。
impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("api", &self.api)
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.to_string())
            .field("api_version", &self.api_version)
            .field("timeout", &self.timeout)
            .field("models", &self.models)
            .finish()
    }
}

/// 请求路由结果：provider 配置 + 命中的模型条目，全部借用。
#[derive(Debug)]
pub struct ModelRoute<'a> {
    pub provider: &'a str,
    pub api: ApiKind,
    pub model: &'a ModelEntry,
    pub base_url: &'a str,
    pub api_key: &'a CredentialRef,
    pub api_version: Option<&'a str>,
    pub timeout: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_kind_literals_round_trip() {
        for api in [ApiKind::Completions, ApiKind::Anthropic, ApiKind::Responses] {
            assert_eq!(ApiKind::from_str_exact(api.as_str()), Some(api));
        }
        assert_eq!(ApiKind::from_str_exact("Completions"), None);
        assert_eq!(ApiKind::from_str_exact(""), None);
        assert_eq!(ApiKind::from_str_exact("chat"), None);
    }

    #[test]
    fn reasoning_param_per_api() {
        assert_eq!(ApiKind::Completions.reasoning_param(), "reasoning_effort");
        assert_eq!(ApiKind::Anthropic.reasoning_param(), "thinking");
        assert_eq!(ApiKind::Responses.reasoning_param(), "reasoning.effort");
    }

    #[test]
    fn default_api_version_only_anthropic() {
        assert_eq!(ApiKind::Anthropic.default_api_version(), Some("2023-06-01"));
        assert_eq!(ApiKind::Completions.default_api_version(), None);
        assert_eq!(ApiKind::Responses.default_api_version(), None);
    }

    #[test]
    fn provider_config_debug_masks_api_key() {
        let cfg = ProviderConfig {
            api: ApiKind::Completions,
            name: Some("WPS AI".into()),
            base_url: "http://127.0.0.1:1/v1".into(),
            api_key: CredentialRef::parse_at("credential/wpsai", "api_key").unwrap(),
            api_version: None,
            timeout: Duration::from_secs(1),
            models: Vec::new(),
        };
        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains("sk-secret"));
        assert!(rendered.contains("credential/wpsai"));
    }
}
