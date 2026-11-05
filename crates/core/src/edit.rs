//! 可写配置文档：在**原始 YAML 树**上做 provider / model 的增删改。
//!
//! 纯逻辑、无 IO。写盘由 `server::store` 负责，本模块只保证「改完的文档」能在
//! 现有校验器（[`crate::config::load_from_str`]）下通过。
//!
//! 关键约束：
//!
//! - **只写 `credential/<name>` 引用**：控制台永不接受明文密钥，密钥值只进凭据文件。
//! - 以原映射为底逐字段覆盖，因此未知键（将来版本新增的字段）原样保留。
//! - `#` 注释与空行会在 `serde_yaml` 重写时丢失（已知限制，设计 §3.5）。
//! - 不做部分校验：调用方拿到文档后必须走 [`Document::validate`]，通过才可落盘。

use serde_yaml::{Mapping, Value};

use crate::config::{load_from_str, ConfigErrors, LoadOptions, LoadOutcome};
use crate::credential::{credential_ref_for, CredentialRef};

/// 控制台写接口接受的 provider 草稿。
///
/// `api_key_env` 是**环境变量名**（可带 `os.environ/` 前缀，会被归一化）；
/// `None` 或空串表示「更新时保留原值」，新建时视为必填缺失。
#[derive(Debug, Clone, Default)]
pub struct ProviderDraft {
    /// provider id（配置键、路径段）。
    pub name: String,
    /// 可选展示名；`None`/空表示「界面回退显示 id」。
    pub display_name: Option<String>,
    pub api: String,
    pub base_url: String,
    pub api_key_env: Option<String>,
    pub timeout: Option<u64>,
    pub api_version: Option<String>,
}

/// 控制台写接口接受的模型草稿。`None` 表示「删除该键」（回到未声明）。
#[derive(Debug, Clone, Default)]
pub struct ModelDraft {
    pub id: String,
    pub name: Option<String>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub input: Option<Vec<String>>,
    pub reasoning: Option<bool>,
    pub thinking_levels: Option<Vec<String>>,
    pub default_thinking_level: Option<String>,
}

/// 一份可编辑的 YAML 文档。只保证根是映射，不做字段级校验。
#[derive(Debug, Clone)]
pub struct Document {
    root: Mapping,
}

impl Document {
    pub fn parse(text: &str) -> Result<Self, ConfigErrors> {
        let value: Value = serde_yaml::from_str(text)
            .map_err(|e| ConfigErrors::single("config", format!("YAML 解析失败：{e}")))?;
        let root = value
            .as_mapping()
            .cloned()
            .ok_or_else(|| ConfigErrors::single("config", "顶层必须是 YAML 映射"))?;
        Ok(Self { root })
    }

    pub fn to_yaml(&self) -> Result<String, ConfigErrors> {
        serde_yaml::to_string(&Value::Mapping(self.root.clone()))
            .map_err(|e| ConfigErrors::single("config", format!("YAML 序列化失败：{e}")))
    }

    /// 整份文档跑一遍现有加载器（非 strict：控制台不会引入未知键）。
    ///
    /// `os.environ/VAR` 会在这里展开，因此引用未设置环境变量的写法会被拒绝。
    pub fn validate(&self) -> Result<LoadOutcome, ConfigErrors> {
        let yaml = self.to_yaml()?;
        load_from_str(&yaml, &LoadOptions { strict: false })
    }

    /// 新建或更新 provider。`create = true` 时同名已存在视为错误。
    pub fn upsert_provider(
        &mut self,
        draft: &ProviderDraft,
        create: bool,
    ) -> Result<(), ConfigErrors> {
        let name = draft.name.trim().to_string();
        if name.is_empty() {
            return Err(ConfigErrors::single("providers", "provider 名不能为空"));
        }
        let path = format!("providers.{name}");
        let providers = self.providers_mut();

        let existing = providers.get(name.as_str()).cloned();
        match (create, &existing) {
            (true, Some(_)) => {
                return Err(ConfigErrors::single(
                    path,
                    "provider 已存在，改名请先删除再新建",
                ))
            }
            (false, None) => return Err(ConfigErrors::single(path, "provider 不存在")),
            _ => {}
        }

        let mut mapping = match existing {
            Some(Value::Mapping(mapping)) => mapping,
            _ => Mapping::new(),
        };

        insert_str(&mut mapping, "api", &draft.api);
        set_optional_str(
            &mut mapping,
            "name",
            draft.display_name.as_deref().map(str::trim),
        );
        insert_str(&mut mapping, "base_url", &draft.base_url);

        // 凭据引用由 provider id 派生（`upper(id)` + `_API_KEY`），不接受手填。
        // 显式传了 api_key 时仍然校验语法，避免静默接受非法引用。
        if let Some(raw) = draft.api_key_env.as_deref().map(str::trim) {
            if !raw.is_empty() {
                CredentialRef::parse_at(raw, &format!("{path}.api_key"))?;
            }
        }
        let derived = credential_ref_for(&name)?;
        insert_str(&mut mapping, "api_key", &derived.to_string());

        set_optional_u64(&mut mapping, "timeout", draft.timeout);
        set_optional_str(&mut mapping, "api_version", draft.api_version.as_deref());
        // 新建 provider 时 `models` 是必填键；空数组合法。
        if mapping.get("models").is_none() {
            mapping.insert(Value::String("models".into()), Value::Sequence(Vec::new()));
        }

        providers.insert(Value::String(name), Value::Mapping(mapping));
        Ok(())
    }

    pub fn delete_provider(&mut self, name: &str) -> Result<(), ConfigErrors> {
        let providers = self.providers_mut();
        if providers.remove(name).is_none() {
            return Err(ConfigErrors::single(
                format!("providers.{name}"),
                "provider 不存在",
            ));
        }
        Ok(())
    }

    /// 新建或更新模型。`original_id` 为 `Some` 时按旧 id 定位（支持改 id）。
    pub fn upsert_model(
        &mut self,
        provider: &str,
        original_id: Option<&str>,
        draft: &ModelDraft,
    ) -> Result<(), ConfigErrors> {
        let id = draft.id.trim().to_string();
        if id.is_empty() {
            return Err(ConfigErrors::single(
                format!("providers.{provider}.models"),
                "模型 id 不能为空",
            ));
        }

        let mut models = self.models_of(provider)?;

        let mut mapping = match original_id {
            Some(original) => {
                let index = models
                    .iter()
                    .position(|item| item.get("id").and_then(Value::as_str) == Some(original))
                    .ok_or_else(|| {
                        ConfigErrors::single(
                            format!("providers.{provider}.models"),
                            format!("模型 `{original}` 不存在"),
                        )
                    })?;
                match models.remove(index) {
                    Value::Mapping(mapping) => mapping,
                    _ => Mapping::new(),
                }
            }
            None => Mapping::new(),
        };

        insert_str(&mut mapping, "id", &id);
        set_optional_str(&mut mapping, "name", draft.name.as_deref());
        set_optional_u64(&mut mapping, "context_window", draft.context_window);
        set_optional_u64(&mut mapping, "max_output_tokens", draft.max_output_tokens);
        set_optional_sequence(&mut mapping, "input", draft.input.as_deref());
        set_optional_sequence(
            &mut mapping,
            "thinking_levels",
            draft.thinking_levels.as_deref(),
        );
        match draft.reasoning {
            Some(flag) => {
                mapping.insert(Value::String("reasoning".into()), Value::Bool(flag));
            }
            None => {
                mapping.remove("reasoning");
            }
        }
        set_optional_str(
            &mut mapping,
            "default_thinking_level",
            draft.default_thinking_level.as_deref(),
        );

        models.push(Value::Mapping(mapping));
        self.replace_models(provider, models);
        Ok(())
    }

    pub fn delete_model(&mut self, provider: &str, id: &str) -> Result<(), ConfigErrors> {
        let mut models = self.models_of(provider)?;
        let index = models
            .iter()
            .position(|item| item.get("id").and_then(Value::as_str) == Some(id))
            .ok_or_else(|| {
                ConfigErrors::single(
                    format!("providers.{provider}.models"),
                    format!("模型 `{id}` 不存在"),
                )
            })?;
        models.remove(index);
        self.replace_models(provider, models);
        Ok(())
    }

    /// 所有 provider 的凭据引用（只给引用名，永远不含值）。
    pub fn api_key_refs(&self) -> std::collections::BTreeMap<String, CredentialRef> {
        api_key_refs_of(&self.root)
    }

    fn providers_mut(&mut self) -> &mut Mapping {
        if !matches!(self.root.get("providers"), Some(Value::Mapping(_))) {
            self.root.insert(
                Value::String("providers".into()),
                Value::Mapping(Mapping::new()),
            );
        }
        match self.root.get_mut("providers") {
            Some(Value::Mapping(mapping)) => mapping,
            _ => unreachable!("providers 刚刚被规范化为映射"),
        }
    }

    fn models_of(&self, provider: &str) -> Result<Vec<Value>, ConfigErrors> {
        let value = self
            .root
            .get("providers")
            .and_then(|providers| providers.get(provider))
            .ok_or_else(|| {
                ConfigErrors::single(format!("providers.{provider}"), "provider 不存在")
            })?;
        match value.get("models") {
            Some(Value::Sequence(items)) => Ok(items.clone()),
            _ => Ok(Vec::new()),
        }
    }

    /// 用一份 id 列表**同步**模型目录：保序、去重，保留已有条目的其余字段。
    ///
    /// 控制台的「模型目录」一次提交整份列表，这里只增删差异，
    /// 因此手动填过的 `context_window` 等字段不会被覆盖。
    pub fn sync_models(&mut self, provider: &str, ids: &[String]) -> Result<(), ConfigErrors> {
        let mut wanted: Vec<String> = Vec::new();
        for id in ids {
            let id = id.trim();
            if !id.is_empty() && !wanted.iter().any(|seen| seen == id) {
                wanted.push(id.to_string());
            }
        }

        let mut models = self.models_of(provider)?;
        models.retain(|item| {
            item.get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| wanted.iter().any(|keep| keep == id))
        });
        for id in &wanted {
            let present = models
                .iter()
                .any(|item| item.get("id").and_then(Value::as_str) == Some(id.as_str()));
            if present {
                continue;
            }
            let mut mapping = Mapping::new();
            insert_str(&mut mapping, "id", id);
            models.push(Value::Mapping(mapping));
        }

        self.replace_models(provider, models);
        Ok(())
    }

    fn replace_models(&mut self, provider: &str, models: Vec<Value>) {
        let providers = self.providers_mut();
        let entry = match providers.get_mut(provider) {
            Some(Value::Mapping(mapping)) => mapping,
            _ => {
                providers.insert(
                    Value::String(provider.to_string()),
                    Value::Mapping(Mapping::new()),
                );
                match providers.get_mut(provider) {
                    Some(Value::Mapping(mapping)) => mapping,
                    _ => unreachable!("provider 刚刚被插入"),
                }
            }
        };
        entry.insert(Value::String("models".into()), Value::Sequence(models));
    }
}

/// 从配置文档里读出每个 provider 的凭据引用。
///
/// 只做语法层面的读取（不解析值），供控制台展示「引用的是哪条凭据」。
pub fn api_key_refs_of(document: &Mapping) -> std::collections::BTreeMap<String, CredentialRef> {
    let mut refs = std::collections::BTreeMap::new();
    let Some(Value::Mapping(providers)) = document.get("providers") else {
        return refs;
    };
    for (key, value) in providers {
        let Some(name) = key.as_str() else { continue };
        let raw = value.get("api_key").and_then(Value::as_str);
        if let Some(reference) =
            raw.and_then(|raw| CredentialRef::parse_at(raw, "providers.api_key").ok())
        {
            refs.insert(name.to_string(), reference);
        }
    }
    refs
}

fn insert_str(mapping: &mut Mapping, key: &str, value: &str) {
    mapping.insert(
        Value::String(key.to_string()),
        Value::String(value.to_string()),
    );
}

fn set_optional_str(mapping: &mut Mapping, key: &str, value: Option<&str>) {
    match value.map(str::trim) {
        Some(value) if !value.is_empty() => insert_str(mapping, key, value),
        _ => {
            mapping.remove(key);
        }
    }
}

fn set_optional_u64(mapping: &mut Mapping, key: &str, value: Option<u64>) {
    match value {
        Some(value) => {
            mapping.insert(Value::String(key.to_string()), Value::Number(value.into()));
        }
        None => {
            mapping.remove(key);
        }
    }
}

fn set_optional_sequence(mapping: &mut Mapping, key: &str, value: Option<&[String]>) {
    match value {
        Some(items) if !items.is_empty() => {
            let items: Vec<Value> = items
                .iter()
                .map(|item| Value::String(item.trim().to_string()))
                .filter(|item| item.as_str().is_some_and(|s| !s.is_empty()))
                .collect();
            if items.is_empty() {
                mapping.remove(key);
            } else {
                mapping.insert(Value::String(key.to_string()), Value::Sequence(items));
            }
        }
        _ => {
            mapping.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"
server:
  bind: "127.0.0.1:4000"
auth:
  master_key: os.environ/ANYBRIDGE_EDIT_TEST_MASTER
providers:
  openai:
    api: completions
    base_url: https://api.openai.com/v1
    api_key: credential/openai
    models:
      - id: gpt-4o
"#;

    fn document() -> Document {
        std::env::set_var("ANYBRIDGE_EDIT_TEST_MASTER", "master");
        Document::parse(BASE).expect("基线文档应可解析")
    }

    fn provider_draft(name: &str) -> ProviderDraft {
        ProviderDraft {
            name: name.to_string(),
            display_name: Some("本地模型".into()),
            api: "completions".into(),
            base_url: "http://127.0.0.1:11434/v1".into(),
            api_key_env: Some("credential/openai".into()),
            timeout: Some(30),
            api_version: None,
        }
    }

    fn model_draft(id: &str) -> ModelDraft {
        ModelDraft {
            id: id.to_string(),
            ..Default::default()
        }
    }

    fn paths(errors: &ConfigErrors) -> Vec<&str> {
        errors.0.iter().map(|e| e.path.as_str()).collect()
    }

    #[test]
    fn create_provider_writes_credential_reference_verbatim() {
        let mut doc = document();
        doc.upsert_provider(&provider_draft("local"), true)
            .expect("新建 provider 应成功");
        let yaml = doc.to_yaml().unwrap();
        assert!(yaml.contains("api_key: credential/openai"), "{yaml}");
        assert!(!yaml.contains("sk-openai"), "不得把密钥写回配置文件");
        doc.validate().expect("文档应通过校验");
    }

    #[test]
    fn legacy_environment_reference_is_rejected() {
        let mut doc = document();
        let mut draft = provider_draft("local");
        draft.api_key_env = Some("os.environ/ANYBRIDGE_EDIT_TEST_OPENAI".into());
        let errors = doc.upsert_provider(&draft, true).unwrap_err();
        assert_eq!(paths(&errors), vec!["providers.local.api_key"]);
        assert!(errors.0[0].message.contains("credential/"));
    }

    #[test]
    fn literal_api_key_is_rejected() {
        let mut doc = document();
        let mut draft = provider_draft("local");
        draft.api_key_env = Some("sk-live-plaintext".into());
        let errors = doc.upsert_provider(&draft, true).unwrap_err();
        assert_eq!(paths(&errors), vec!["providers.local.api_key"]);
        assert!(errors.0[0].message.contains("credential/"));
    }

    #[test]
    fn duplicate_provider_on_create_is_rejected() {
        let mut doc = document();
        let errors = doc
            .upsert_provider(&provider_draft("openai"), true)
            .unwrap_err();
        assert_eq!(paths(&errors), vec!["providers.openai"]);
    }

    #[test]
    fn api_key_reference_is_derived_from_provider_id() {
        let mut doc = document();
        let mut draft = provider_draft("openai");
        draft.api_key_env = None;
        draft.timeout = Some(120);
        doc.upsert_provider(&draft, false).unwrap();
        let yaml = doc.to_yaml().unwrap();
        assert!(
            yaml.contains("api_key: credential/OPENAI_API_KEY"),
            "{yaml}"
        );
        assert!(yaml.contains("timeout: 120"), "{yaml}");

        // 连字符归一成下划线。
        let mut draft = provider_draft("openai-responses");
        draft.api_key_env = None;
        doc.upsert_provider(&draft, true).unwrap();
        let yaml = doc.to_yaml().unwrap();
        assert!(
            yaml.contains("api_key: credential/OPENAI_RESPONSES_API_KEY"),
            "{yaml}"
        );
    }

    #[test]
    fn update_clears_optional_fields() {
        let mut doc = document();
        let mut draft = provider_draft("openai");
        draft.api_key_env = None;
        draft.timeout = None;
        doc.upsert_provider(&draft, false).unwrap();
        let yaml = doc.to_yaml().unwrap();
        assert!(!yaml.contains("timeout"), "{yaml}");
        assert!(yaml.contains("models:"), "{yaml}");
    }

    #[test]
    fn unknown_keys_survive_round_trip() {
        let mut doc = Document::parse(
            r#"
auth:
  master_key: k
  future_field: keep-me
providers:
  openai:
    api: completions
    base_url: https://api.openai.com/v1
    api_key: credential/openai
    region: us-east
    models:
      - id: gpt-4o
        cost_hint: 1
"#,
        )
        .unwrap();
        doc.upsert_model("openai", Some("gpt-4o"), &model_draft("gpt-4o"))
            .unwrap();
        let yaml = doc.to_yaml().unwrap();
        assert!(yaml.contains("future_field: keep-me"), "{yaml}");
        assert!(yaml.contains("region: us-east"), "{yaml}");
        assert!(yaml.contains("cost_hint: 1"), "{yaml}");
    }

    #[test]
    fn create_model_appends_and_validates() {
        let mut doc = document();
        let mut draft = model_draft("gpt-4o-mini");
        draft.context_window = Some(128_000);
        draft.max_output_tokens = Some(16_384);
        draft.input = Some(vec!["text".into(), "image".into()]);
        draft.thinking_levels = Some(vec!["low".into(), "high".into()]);
        draft.default_thinking_level = Some("high".into());
        doc.upsert_model("openai", None, &draft).unwrap();
        let outcome = doc.validate().unwrap();
        let models = &outcome.config.providers["openai"].models;
        assert_eq!(models.len(), 2);
        let added = &models[1];
        assert_eq!(added.id, "gpt-4o-mini");
        assert_eq!(added.context_window, Some(128_000));
        assert_eq!(added.reasoning, Some(true));
        assert_eq!(added.default_thinking_level.as_deref(), Some("high"));
    }

    #[test]
    fn rename_model_keeps_single_entry() {
        let mut doc = document();
        doc.upsert_model("openai", Some("gpt-4o"), &model_draft("gpt-4o-2024"))
            .unwrap();
        let outcome = doc.validate().unwrap();
        let models = &outcome.config.providers["openai"].models;
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-4o-2024");
    }

    #[test]
    fn rename_to_existing_id_fails_validation() {
        let mut doc = document();
        doc.upsert_model("openai", None, &model_draft("other"))
            .unwrap();
        doc.upsert_model("openai", Some("other"), &model_draft("gpt-4o"))
            .unwrap();
        let errors = doc.validate().unwrap_err();
        assert!(
            errors.0.iter().any(|e| e.message.contains("重复")),
            "{errors:?}"
        );
    }

    #[test]
    fn delete_model_and_provider() {
        let mut doc = document();
        doc.delete_model("openai", "gpt-4o").unwrap();
        assert!(doc
            .validate()
            .unwrap()
            .config
            .providers
            .get("openai")
            .unwrap()
            .models
            .is_empty());

        doc.delete_provider("openai").unwrap();
        assert!(
            doc.validate().unwrap().config.providers.is_empty(),
            "允许把 provider 删空"
        );

        let errors = doc.delete_provider("openai").unwrap_err();
        assert_eq!(paths(&errors), vec!["providers.openai"]);
    }

    #[test]
    fn delete_unknown_model_is_rejected() {
        let mut doc = document();
        let errors = doc.delete_model("openai", "nope").unwrap_err();
        assert!(errors.0[0].message.contains("不存在"));
    }

    #[test]
    fn invalid_api_value_fails_validation() {
        let mut doc = document();
        let mut draft = provider_draft("local");
        draft.api = "chat".into();
        doc.upsert_provider(&draft, true).unwrap();
        let errors = doc.validate().unwrap_err();
        assert_eq!(paths(&errors), vec!["providers.local.api"]);
    }

    #[test]
    fn invalid_provider_name_fails_validation() {
        let mut doc = document();
        let mut draft = provider_draft("OpenAI2");
        draft.name = "OpenAI2".into();
        doc.upsert_provider(&draft, true).unwrap();
        let errors = doc.validate().unwrap_err();
        assert_eq!(paths(&errors), vec!["providers.OpenAI2"]);
    }

    #[test]
    fn max_output_tokens_over_context_window_fails() {
        let mut doc = document();
        let mut draft = model_draft("m");
        draft.context_window = Some(100);
        draft.max_output_tokens = Some(101);
        doc.upsert_model("openai", None, &draft).unwrap();
        let errors = doc.validate().unwrap_err();
        assert_eq!(
            paths(&errors),
            vec!["providers.openai.models[1].max_output_tokens"]
        );
    }

    #[test]
    fn legacy_reference_in_document_fails_validation() {
        // 手工编辑留下的旧写法：整份文档在校验阶段就被拒，不会热加载成半份配置。
        let doc = Document::parse(
            r#"
auth:
  master_key: k
providers:
  local:
    api: completions
    base_url: http://127.0.0.1:11434/v1
    api_key: os.environ/ANYBRIDGE_EDIT_TEST_UNSET
    models: []
"#,
        )
        .unwrap();
        let errors = doc.validate().unwrap_err();
        assert_eq!(paths(&errors), vec!["providers.local.api_key"]);
        assert!(errors.0[0].message.contains("credential/"));
    }

    #[test]
    fn sync_models_keeps_metadata_and_order() {
        let mut doc = document();
        // 先给 gpt-4o 加上元数据，再整份同步：保留字段、按新列表增删。
        let mut draft = model_draft("gpt-4o");
        draft.context_window = Some(128_000);
        doc.upsert_model("openai", Some("gpt-4o"), &draft).unwrap();

        doc.sync_models(
            "openai",
            &["gpt-4o".into(), "claude-sonnet".into(), "gpt-4o".into()],
        )
        .unwrap();

        let yaml = doc.to_yaml().unwrap();
        assert_eq!(yaml.matches("- id:").count(), 2, "重复 id 应去重：{yaml}");
        assert!(yaml.contains("gpt-4o"), "{yaml}");
        assert!(yaml.contains("claude-sonnet"), "{yaml}");
        assert!(
            yaml.contains("context_window: 128000"),
            "已有条目的其它字段不能丢：{yaml}"
        );

        // 再同步成只剩一条：多余的被移除。
        doc.sync_models("openai", &["claude-sonnet".into()])
            .unwrap();
        let yaml = doc.to_yaml().unwrap();
        assert!(!yaml.contains("gpt-4o"), "{yaml}");
        assert!(yaml.contains("claude-sonnet"), "{yaml}");
    }

    #[test]
    fn api_key_refs_never_expose_values() {
        let doc = Document::parse(
            r#"
auth:
  master_key: k
providers:
  a:
    api: completions
    base_url: https://example.com/v1
    api_key: credential/some_var
  b:
    api: completions
    base_url: https://example.com/v1
    api_key: credential/other
  c:
    api: completions
    base_url: https://example.com/v1
    api_key: ""
"#,
        )
        .unwrap();
        let refs = doc.api_key_refs();
        assert_eq!(refs["a"].name(), "some_var");
        assert_eq!(refs["b"].to_string(), "credential/other");
        // 空值不是合法引用，不进入表；也永远没有值可泄露。
        assert!(!refs.contains_key("c"));
        let rendered = format!("{refs:?}");
        assert!(!rendered.contains("sk-"));
    }

    #[test]
    fn comments_are_not_preserved() {
        // 已知限制：整份重写会丢注释与空行（设计 §3.5）。
        let mut doc = Document::parse(
            r#"
# 顶部注释
auth:
  master_key: k
providers:
  openai:  # 行尾注释
    api: completions
    base_url: https://example.com/v1
    api_key: credential/openai
    models:
      - id: gpt-4o
"#,
        )
        .unwrap();
        doc.upsert_model("openai", None, &model_draft("m2"))
            .unwrap();
        let yaml = doc.to_yaml().unwrap();
        assert!(!yaml.contains('#'), "注释应当丢失：{yaml}");
    }

    #[test]
    fn provider_writes_display_name_and_derived_credential() {
        let mut doc = Document::parse(
            r#"
auth:
  master_key: k
providers: {}
"#,
        )
        .unwrap();
        let mut draft = provider_draft("openai");
        draft.display_name = Some("OpenAI 官方".into());
        doc.upsert_provider(&draft, true).unwrap();
        let yaml = doc.to_yaml().unwrap();
        assert!(yaml.contains("name: OpenAI 官方"), "{yaml}");
        assert!(
            yaml.contains("api_key: credential/OPENAI_API_KEY"),
            "{yaml}"
        );

        // 显式传非法引用仍然被拒，且错误指向 api_key 字段。
        for bad in ["os.environ/OPENAI_API_KEY", "sk-live-plaintext"] {
            let mut draft = provider_draft("local");
            draft.api_key_env = Some(bad.into());
            let errors = doc.upsert_provider(&draft, true).unwrap_err();
            assert_eq!(paths(&errors), vec!["providers.local.api_key"]);
        }
    }
}
