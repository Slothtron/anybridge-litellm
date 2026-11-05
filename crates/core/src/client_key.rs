//! 对外 API key：网关发给客户端、用于访问 `/llmproxy/*` 的密钥。
//!
//! 与上游凭据（[`crate::credential`]）共用同一份 `credentials.yaml`，
//! 但语义不同：上游凭据是「provider 拿去打上游」的机密，这里是「客户端出示给
//! 网关」的凭据。为了降低泄露面，**只存 `sha256` 摘要**，明文只在创建/轮换时
//! 返回一次；每把 key 带 `scope`（provider id 列表，`*` 表示全部）。
//!
//! 文件格式（顶层 `keys` 段）：
//!
//! ```yaml
//! keys:
//!   - name: alice
//!     hash: 9f2c...
//!     scope: ["wpsai"]
//!     description: Alice 的网关
//!     created_at: 2026-11-05T12:00:00Z
//!     enabled: true
//! ```

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_yaml::{Mapping, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::credential::{read_root, write_root, CredentialError, KEYS_SECTION, PREFIX};

/// key 名长度上限（字节）。
const MAX_NAME_BYTES: usize = 64;
/// 明文前缀，便于识别。
const TOKEN_PREFIX: &str = "ab-";
/// 摘要（sha256 十六进制）长度。
const HASH_HEX_LEN: usize = 64;

/// 对外 API key 的元信息：**永远不含明文与摘要**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyInfo {
    pub name: String,
    /// provider id 列表；`*` 表示全部。
    pub scope: Vec<String>,
    pub description: Option<String>,
    pub created_at: String,
    pub enabled: bool,
}

/// 控制台写接口接受的草稿。`name` 只在新建时使用（更新不允许改名）。
#[derive(Debug, Clone, Default)]
pub struct ApiKeyDraft {
    pub name: String,
    pub scope: Vec<String>,
    pub description: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ApiKeyError {
    #[error("凭据存储不可写（未指定凭据文件）")]
    ReadOnly,
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Invalid(String),
    #[error("API key `{0}` 不存在")]
    NotFound(String),
    #[error("API key `{0}` 已存在")]
    Conflict(String),
}

impl From<CredentialError> for ApiKeyError {
    fn from(err: CredentialError) -> Self {
        match err {
            CredentialError::ReadOnly => ApiKeyError::ReadOnly,
            CredentialError::Io(message) => ApiKeyError::Io(message),
            CredentialError::Invalid(message) => ApiKeyError::Invalid(message),
        }
    }
}

/// 凭据存储的插拔点：本地 YAML 实现是本期唯一实现。
pub trait ApiKeyStore: Send + Sync {
    /// 全部 key 的元信息（按文件顺序）。
    fn list(&self) -> Vec<ApiKeyInfo>;

    /// 按客户端出示的明文查找**启用**的 key。
    fn find(&self, presented: &str) -> Option<ApiKeyInfo>;

    /// 新建；返回元信息与**一次性明文**。
    fn create(&self, draft: &ApiKeyDraft) -> Result<(ApiKeyInfo, String), ApiKeyError>;

    /// 更新 scope / 说明 / 启停（名字不可改）。
    fn update(&self, name: &str, draft: &ApiKeyDraft) -> Result<ApiKeyInfo, ApiKeyError>;

    /// 轮换：生成新明文，旧明文立刻失效；返回新明文。
    fn rotate(&self, name: &str) -> Result<(ApiKeyInfo, String), ApiKeyError>;

    fn delete(&self, name: &str) -> Result<(), ApiKeyError>;

    /// 存储位置；`None` 表示只读/无文件。
    fn location(&self) -> Option<&Path> {
        None
    }
}

/// 是否允许访问某个 provider。
pub fn scope_allows(scope: &[String], provider: &str) -> bool {
    scope.iter().any(|item| item == "*" || item == provider)
}

/// 明文 → sha256 小写十六进制。
pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut out = String::with_capacity(HASH_HEX_LEN);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 生成一把新明文：`ab-` + 32 位十六进制（128 bit 熵）。
pub fn generate_token() -> String {
    let mut bytes = [0u8; 16];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        if file.read_exact(&mut bytes).is_ok() {
            return format!("{TOKEN_PREFIX}{}", hex(&bytes));
        }
    }
    // 非 unix 兜底：时间 + pid，仍然只用于临时环境。
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let seed = format!("{nanos}-{}", std::process::id());
    let digest = Sha256::digest(seed.as_bytes());
    format!("{TOKEN_PREFIX}{}", hex(&digest[..16]))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// YAML 文件存储：`credentials.yaml` 里的 `keys` 段。
pub struct YamlApiKeyStore {
    path: Option<PathBuf>,
    write_gate: Mutex<()>,
}

impl YamlApiKeyStore {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            write_gate: Mutex::new(()),
        }
    }

    fn read(&self) -> Result<Vec<ApiKeyInfo>, ApiKeyError> {
        Ok(self
            .read_hashed()?
            .into_iter()
            .map(|(info, _)| info)
            .collect())
    }

    /// 读出带摘要的条目；只给存储内部用。
    fn read_hashed(&self) -> Result<Vec<(ApiKeyInfo, String)>, ApiKeyError> {
        let Some(path) = &self.path else {
            return Ok(Vec::new());
        };
        let root = read_root(path)?;
        parse_hashed(&root)
    }

    fn write(&self, keys: &[(ApiKeyInfo, String)]) -> Result<(), ApiKeyError> {
        let path = self.path.as_ref().ok_or(ApiKeyError::ReadOnly)?;
        let mut root = read_root(path)?;
        root.insert(
            Value::String(KEYS_SECTION.into()),
            Value::Sequence(keys.iter().map(render_key).collect()),
        );
        write_root(path, &root)?;
        Ok(())
    }
}

impl ApiKeyStore for YamlApiKeyStore {
    fn list(&self) -> Vec<ApiKeyInfo> {
        self.read().unwrap_or_default()
    }

    fn find(&self, presented: &str) -> Option<ApiKeyInfo> {
        let presented = presented.trim();
        if presented.is_empty() {
            return None;
        }
        let candidate = hash_token(presented);
        let keys = self.read_hashed().ok()?;
        let mut matched: Option<ApiKeyInfo> = None;
        for (info, hash) in keys {
            if !info.enabled || hash.len() != candidate.len() {
                continue;
            }
            // 常量时间比较，避免按摘要前缀计时。
            if hash.as_bytes().ct_eq(candidate.as_bytes()).into() {
                matched = Some(info);
            }
        }
        matched
    }

    fn create(&self, draft: &ApiKeyDraft) -> Result<(ApiKeyInfo, String), ApiKeyError> {
        if self.path.is_none() {
            return Err(ApiKeyError::ReadOnly);
        }
        let name = draft.name.trim().to_string();
        validate_name(&name)?;
        let scope = validate_scope(&draft.scope)?;
        let description = normalize_description(draft.description.as_deref());

        let _gate = self
            .write_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut keys = self.read_hashed()?;
        if keys.iter().any(|(info, _)| info.name == name) {
            return Err(ApiKeyError::Conflict(name));
        }

        let secret = generate_token();
        let info = ApiKeyInfo {
            name,
            scope,
            description,
            created_at: now_rfc3339(),
            enabled: draft.enabled,
        };
        keys.push((info.clone(), hash_token(&secret)));
        self.write(&keys)?;
        Ok((info, secret))
    }

    fn update(&self, name: &str, draft: &ApiKeyDraft) -> Result<ApiKeyInfo, ApiKeyError> {
        let scope = validate_scope(&draft.scope)?;
        let description = normalize_description(draft.description.as_deref());

        let _gate = self
            .write_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut keys = self.read_hashed()?;
        let index = keys
            .iter()
            .position(|(info, _)| info.name == name)
            .ok_or_else(|| ApiKeyError::NotFound(name.to_string()))?;

        let info = &mut keys[index].0;
        info.scope = scope;
        info.description = description;
        info.enabled = draft.enabled;
        let updated = info.clone();
        self.write(&keys)?;
        Ok(updated)
    }

    fn rotate(&self, name: &str) -> Result<(ApiKeyInfo, String), ApiKeyError> {
        let _gate = self
            .write_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut keys = self.read_hashed()?;
        let index = keys
            .iter()
            .position(|(info, _)| info.name == name)
            .ok_or_else(|| ApiKeyError::NotFound(name.to_string()))?;

        let secret = generate_token();
        keys[index].1 = hash_token(&secret);
        let info = keys[index].0.clone();
        self.write(&keys)?;
        Ok((info, secret))
    }

    fn delete(&self, name: &str) -> Result<(), ApiKeyError> {
        let _gate = self
            .write_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut keys = self.read_hashed()?;
        let index = keys
            .iter()
            .position(|(info, _)| info.name == name)
            .ok_or_else(|| ApiKeyError::NotFound(name.to_string()))?;
        keys.remove(index);
        self.write(&keys)
    }

    fn location(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

fn parse_hashed(root: &Mapping) -> Result<Vec<(ApiKeyInfo, String)>, ApiKeyError> {
    if !root.contains_key(KEYS_SECTION) && root.contains_key("credentials") {
        return Err(ApiKeyError::Invalid(
            "凭据文件使用旧布局（顶层 `credentials`）：请把条目嵌套到 `refs:` 映射下，值不变"
                .into(),
        ));
    }
    let Some(value) = root.get(KEYS_SECTION) else {
        return Ok(Vec::new());
    };
    let items = match value {
        Value::Null => return Ok(Vec::new()),
        Value::Sequence(items) => items,
        _ => {
            return Err(ApiKeyError::Invalid(
                "凭据文件的顶层 `keys` 必须是数组".into(),
            ))
        }
    };

    let mut keys = Vec::with_capacity(items.len());
    let mut seen = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let path = format!("keys[{index}]");
        let entry = item
            .as_mapping()
            .ok_or_else(|| ApiKeyError::Invalid(format!("{path} 必须是映射")))?;
        for key in entry.keys() {
            let Some(name) = key.as_str() else {
                return Err(ApiKeyError::Invalid(format!("{path} 的键必须是字符串")));
            };
            if !matches!(
                name,
                "name" | "hash" | "scope" | "description" | "created_at" | "enabled"
            ) {
                return Err(ApiKeyError::Invalid(format!("{path}.{name}: 未知字段")));
            }
        }

        let name = required_str(entry, "name", &path)?.to_string();
        validate_name(&name).map_err(|err| ApiKeyError::Invalid(format!("{path}.name: {err}")))?;
        if seen.contains(&name) {
            return Err(ApiKeyError::Invalid(format!(
                "{path}.name: API key `{name}` 重复"
            )));
        }
        seen.push(name.clone());

        let hash = required_str(entry, "hash", &path)?.to_string();
        if hash.len() != HASH_HEX_LEN
            || !hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ApiKeyError::Invalid(format!(
                "{path}.hash: 必须是 {HASH_HEX_LEN} 位小写十六进制"
            )));
        }

        let scope = parse_scope(entry.get("scope"), &path)?;
        let description = match entry.get("description") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => normalize_description(Some(text)),
            Some(_) => {
                return Err(ApiKeyError::Invalid(format!(
                    "{path}.description: 必须是字符串"
                )))
            }
        };
        let created_at = required_str(entry, "created_at", &path)?.to_string();
        if !valid_timestamp(&created_at) {
            return Err(ApiKeyError::Invalid(format!(
                "{path}.created_at: 必须是 RFC3339 时间"
            )));
        }
        let enabled = match entry.get("enabled") {
            None => true,
            Some(Value::Bool(flag)) => *flag,
            Some(_) => {
                return Err(ApiKeyError::Invalid(format!(
                    "{path}.enabled: 必须是布尔值"
                )))
            }
        };

        keys.push((
            ApiKeyInfo {
                name,
                scope,
                description,
                created_at,
                enabled,
            },
            hash,
        ));
    }
    Ok(keys)
}

fn render_key((info, hash): &(ApiKeyInfo, String)) -> Value {
    let mut mapping = Mapping::new();
    mapping.insert(
        Value::String("name".into()),
        Value::String(info.name.clone()),
    );
    mapping.insert(Value::String("hash".into()), Value::String(hash.clone()));
    mapping.insert(
        Value::String("scope".into()),
        Value::Sequence(
            info.scope
                .iter()
                .map(|item| Value::String(item.clone()))
                .collect(),
        ),
    );
    if let Some(description) = &info.description {
        mapping.insert(
            Value::String("description".into()),
            Value::String(description.clone()),
        );
    }
    mapping.insert(
        Value::String("created_at".into()),
        Value::String(info.created_at.clone()),
    );
    mapping.insert(Value::String("enabled".into()), Value::Bool(info.enabled));
    Value::Mapping(mapping)
}

fn required_str<'a>(mapping: &'a Mapping, key: &str, path: &str) -> Result<&'a str, ApiKeyError> {
    match mapping.get(key) {
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(value),
        _ => Err(ApiKeyError::Invalid(format!("{path}.{key}: 必填"))),
    }
}

fn parse_scope(value: Option<&Value>, path: &str) -> Result<Vec<String>, ApiKeyError> {
    let Some(value) = value else {
        return Err(ApiKeyError::Invalid(format!("{path}.scope: 必填")));
    };
    let items = match value {
        Value::Sequence(items) => items,
        _ => return Err(ApiKeyError::Invalid(format!("{path}.scope: 必须是数组"))),
    };
    let mut scope: Vec<String> = Vec::new();
    for item in items {
        let Some(entry) = item.as_str() else {
            return Err(ApiKeyError::Invalid(format!("{path}.scope: 只接受字符串")));
        };
        let entry = entry.trim();
        if !valid_scope_entry(entry) {
            return Err(ApiKeyError::Invalid(format!(
                "{path}.scope: `{entry}` 不是合法的 provider id（或 `*`）"
            )));
        }
        if !scope.iter().any(|existing| existing == entry) {
            scope.push(entry.to_string());
        }
    }
    if scope.is_empty() {
        return Err(ApiKeyError::Invalid(format!(
            "{path}.scope: 至少包含一个 provider（或 `*`）"
        )));
    }
    Ok(scope)
}

fn validate_name(name: &str) -> Result<(), ApiKeyError> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err(ApiKeyError::Invalid("name 不能为空".into()));
    };
    let ok = name.len() <= MAX_NAME_BYTES
        && first.is_ascii_alphanumeric()
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok {
        Ok(())
    } else {
        Err(ApiKeyError::Invalid(
            "name 需匹配 ^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$".into(),
        ))
    }
}

fn validate_scope(scope: &[String]) -> Result<Vec<String>, ApiKeyError> {
    let mut out: Vec<String> = Vec::new();
    for entry in scope {
        let entry = entry.trim();
        if !valid_scope_entry(entry) {
            return Err(ApiKeyError::Invalid(format!(
                "`{entry}` 不是合法的 provider id（或 `*`）"
            )));
        }
        if !out.iter().any(|existing| existing == entry) {
            out.push(entry.to_string());
        }
    }
    if out.is_empty() {
        return Err(ApiKeyError::Invalid(
            "scope 至少包含一个 provider（或 `*`）".into(),
        ));
    }
    Ok(out)
}

fn valid_scope_entry(entry: &str) -> bool {
    if entry == "*" {
        return true;
    }
    let bytes = entry.as_bytes();
    let Some((&first, rest)) = bytes.split_first() else {
        return false;
    };
    first.is_ascii_lowercase()
        && rest.len() <= 31
        && rest
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn normalize_description(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// `YYYY-MM-DDTHH:MM:SS` 后跟 `Z` 或 `±HH:MM`；不做日历边界校验。
fn valid_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    if !(digits(0..4)
        && digits(5..7)
        && digits(8..10)
        && digits(11..13)
        && digits(14..16)
        && digits(17..19))
    {
        return false;
    }
    let tail = &value[19..];
    tail == "Z"
        || (tail.len() == 6
            && (tail.starts_with('+') || tail.starts_with('-'))
            && tail[1..3].bytes().all(|b| b.is_ascii_digit())
            && tail.as_bytes()[3] == b':'
            && tail[4..6].bytes().all(|b| b.is_ascii_digit()))
}

fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant 的 civil_from_days：Unix 天数 → (年, 月, 日)。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// 凭据文件里 `refs` 的展示形式（供控制台回显引用）。
pub fn credential_reference(name: &str) -> String {
    format!("{PREFIX}{name}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!("anybridge-keys-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    fn draft(name: &str, scope: &[&str]) -> ApiKeyDraft {
        ApiKeyDraft {
            name: name.into(),
            scope: scope.iter().map(|item| item.to_string()).collect(),
            description: Some("测试".into()),
            enabled: true,
        }
    }

    #[test]
    fn token_and_hash_shape() {
        let token = generate_token();
        assert!(token.starts_with("ab-"), "{token}");
        assert_eq!(token.len(), 3 + 32, "{token}");
        assert_eq!(hash_token("hello").len(), HASH_HEX_LEN);
        assert_eq!(hash_token("hello"), hash_token("hello"));
    }

    #[test]
    fn scope_matching() {
        assert!(scope_allows(&["*".into()], "wpsai"));
        assert!(scope_allows(&["wpsai".into()], "wpsai"));
        assert!(!scope_allows(&["openai".into()], "wpsai"));
        assert!(!scope_allows(&[], "wpsai"));
    }

    #[test]
    fn create_find_rotate_delete_round_trip() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("credentials.yaml");
        std::fs::write(&path, "refs:\n  UPSTREAM: sk-up\n").unwrap();
        let store = YamlApiKeyStore::new(Some(path.clone()));

        assert!(store.list().is_empty());
        assert!(store.find("ab-nope").is_none());

        let (info, secret) = store.create(&draft("alice", &["wpsai"])).unwrap();
        assert_eq!(info.name, "alice");
        assert_eq!(info.scope, vec!["wpsai".to_string()]);
        assert!(secret.starts_with("ab-"));

        let found = store.find(&secret).expect("新 key 应能解析");
        assert_eq!(found.name, "alice");
        assert!(store.find("ab-wrong").is_none());

        // 写回保留 refs 段。
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("UPSTREAM: sk-up"), "{written}");

        // 停用后不再解析。
        let mut disabled = draft("alice", &["*"]);
        disabled.enabled = false;
        store.update("alice", &disabled).unwrap();
        assert!(store.find(&secret).is_none());

        // 重新启用并轮换。
        store.update("alice", &draft("alice", &["*"])).unwrap();
        let (_, rotated) = store.rotate("alice").unwrap();
        assert_ne!(rotated, secret);
        assert!(store.find(&secret).is_none(), "旧明文必须失效");
        assert!(store.find(&rotated).is_some());

        store.delete("alice").unwrap();
        assert!(store.list().is_empty());
        assert!(store.find(&rotated).is_none());
        assert!(matches!(
            store.delete("alice"),
            Err(ApiKeyError::NotFound(_))
        ));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn duplicate_name_is_conflict() {
        let dir = temp_dir("conflict");
        let store = YamlApiKeyStore::new(Some(dir.join("credentials.yaml")));
        store.create(&draft("alice", &["*"])).unwrap();
        assert!(matches!(
            store.create(&draft("alice", &["*"])),
            Err(ApiKeyError::Conflict(_))
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalid_drafts_are_rejected() {
        let dir = temp_dir("invalid");
        let store = YamlApiKeyStore::new(Some(dir.join("credentials.yaml")));
        for name in ["", "a b", "a/b"] {
            assert!(
                store.create(&draft(name, &["*"])).is_err(),
                "应拒绝 name `{name}`"
            );
        }
        for scope in [vec![], vec!["WPSAI"], vec!["a b"], vec!["*", ""]] {
            assert!(
                store.create(&draft("ok", &scope)).is_err(),
                "应拒绝 scope {scope:?}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_only_store_rejects_writes() {
        let store = YamlApiKeyStore::new(None);
        assert!(store.list().is_empty());
        assert!(store.find("ab-x").is_none());
        assert!(matches!(
            store.create(&draft("alice", &["*"])),
            Err(ApiKeyError::ReadOnly)
        ));
    }

    #[test]
    fn malformed_document_is_rejected() {
        let dir = temp_dir("malformed");
        let path = dir.join("credentials.yaml");
        let cases = [
            "keys: nope\n",
            "keys:\n  - name: alice\n    hash: short\n    scope: [\"*\"]\n    created_at: 2026-01-01T00:00:00Z\n",
            "keys:\n  - name: alice\n    hash: 9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c\n    scope: []\n    created_at: 2026-01-01T00:00:00Z\n",
        ];
        for text in cases {
            std::fs::write(&path, text).unwrap();
            let store = YamlApiKeyStore::new(Some(path.clone()));
            assert!(store.read().is_err(), "应拒绝：{text}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn timestamps_round_trip() {
        let now = now_rfc3339();
        assert!(valid_timestamp(&now), "{now}");
        assert!(!valid_timestamp("2026-01-01 00:00:00"));
        assert!(valid_timestamp("2026-01-01T00:00:00+08:00"));
    }
}
