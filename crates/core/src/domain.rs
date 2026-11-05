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

/// provider 级自定义请求头的值来源。
///
/// `credential/<name>` 形式按请求从凭据存储解析，其余按字面量发送。
#[derive(Clone, PartialEq, Eq)]
pub enum HeaderSource {
    Literal(String),
    Credential(CredentialRef),
}

impl HeaderSource {
    /// 是否引用凭据文件。
    pub fn is_credential(&self) -> bool {
        matches!(self, HeaderSource::Credential(_))
    }
}

impl std::fmt::Display for HeaderSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeaderSource::Literal(value) => f.write_str(value),
            HeaderSource::Credential(reference) => write!(f, "{reference}"),
        }
    }
}

/// 手写 `Debug`：字面量打码，凭据引用只显示引用名。
impl std::fmt::Debug for HeaderSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeaderSource::Literal(_) => f.write_str("***"),
            HeaderSource::Credential(reference) => write!(f, "{reference}"),
        }
    }
}

/// 单个 provider 的解析后配置。
///
/// `api_key` 是**凭据引用**，不是明文：值由凭据存储在每次请求时解析，
/// 因此轮换密钥不需要重启。为 `None` 时不注入任何鉴权头（由 `headers` 承担，
/// 例如 Cookie 鉴权的上游）。
#[derive(Clone)]
pub struct ProviderConfig {
    pub api: ApiKind,
    /// 可选展示名；为空时界面回退显示 provider id。
    pub name: Option<String>,
    pub base_url: String,
    pub api_key: Option<CredentialRef>,
    /// 自定义请求头；保序，转发时覆盖同名客户端头与鉴权头。
    pub headers: Vec<(String, HeaderSource)>,
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
            .field("api_key", &self.api_key.as_ref().map(ToString::to_string))
            .field("headers", &self.headers)
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
    pub api_key: Option<&'a CredentialRef>,
    pub headers: &'a [(String, HeaderSource)],
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
            api_key: Some(CredentialRef::parse_at("credential/wpsai", "api_key").unwrap()),
            headers: vec![
                (
                    "Cookie".into(),
                    HeaderSource::Credential(
                        CredentialRef::parse_at("credential/wps_sid", "headers").unwrap(),
                    ),
                ),
                ("X-Tenant-Id".into(), HeaderSource::Literal("acme".into())),
            ],
            api_version: None,
            timeout: Duration::from_secs(1),
            models: Vec::new(),
        };
        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains("sk-secret"));
        assert!(
            !rendered.contains("acme"),
            "字面量请求头也要打码：{rendered}"
        );
        assert!(rendered.contains("credential/wpsai"));
        assert!(rendered.contains("credential/wps_sid"));
    }

    #[test]
    fn header_source_display_round_trip() {
        let literal = HeaderSource::Literal("acme".into());
        assert_eq!(literal.to_string(), "acme");
        assert!(!literal.is_credential());

        let reference = HeaderSource::Credential(
            CredentialRef::parse_at("credential/WPS_SID", "headers").unwrap(),
        );
        assert_eq!(reference.to_string(), "credential/WPS_SID");
        assert!(reference.is_credential());
    }
}
