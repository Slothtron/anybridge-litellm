//! anybridge-litellm 核心：纯逻辑，无 HTTP 设施。
//!
//! - [`config`]：配置解析与校验
//! - [`edit`]：可写配置文档（provider / model 增删改）
//! - [`credential`]：凭据引用与存储（`credential/<name>`，密钥值不进配置文件）
//! - [`domain`]：`ApiKind` / `ModelEntry` / `ProviderConfig` / `ModelRoute`
//! - [`auth`]：master key 常量时间比较
//! - [`passthrough`]：`{tail}` 白名单与请求体 `model` 提取
//! - [`error`]：`GatewayError` 错误信封

pub mod auth;
pub mod config;
pub mod credential;
pub mod domain;
pub mod edit;
pub mod error;
pub mod passthrough;

pub use auth::{extract_presented, verify_master_key};
pub use config::{
    load_from_path, load_from_str, AuthConfig, ConfigError, ConfigErrors, CredentialsConfig,
    GatewayConfig, LoadOptions, LoadOutcome, ServerConfig, GATEWAY_NAME,
};
pub use credential::{
    CredentialError, CredentialInfo, CredentialRef, CredentialStore, ResolvedCredential,
    YamlCredentialStore,
};
pub use domain::{ApiKind, Modality, ModelEntry, ModelRoute, ProviderConfig};
pub use edit::{api_key_refs_of, Document, ModelDraft, ProviderDraft};
pub use error::{ErrorKind, GatewayError};
