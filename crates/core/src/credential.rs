//! 凭据（provider 的 `api_key` / cookie 等机密）的引用与存储。
//!
//! 配置文件里只出现**引用**：`api_key: credential/wpsai`。值由
//! [`CredentialStore`] 提供，消费方（HTTP 转发层）**每次请求重新解析一次**，
//! 因此控制台轮换密钥后，下一次请求立刻生效，不需要重启。
//!
//! 本期只有一种实现：[`YamlCredentialStore`]，值落在独立的 YAML 文件里
//! （默认 `~/.config/anybridge-litellm/credentials.yaml`，权限 `0600`）。
//! [`CredentialStore`] 是插拔点：以后接 keyring / Vault / 远端配置中心只需新增实现，
//! 上层（server / console）只依赖 trait。
//!
//! 文件格式（顶层 `refs` 段；对齐 dsh）：
//!
//! ```yaml
//! refs:
//!   wpsai: sk-...
//!   openai: sk-...
//! keys:            # 对外 API key（见 client_key 模块）
//!   - name: alice
//!     hash: 9f2c...
//!     scope: ["*"]
//!     created_at: 2026-11-05T12:00:00Z
//!     enabled: true
//! ```
//!
//! 空值等价于「未配置」（与 dsh 的 credential seam 同一条规则）。
//! 旧布局（顶层 `credentials:`）在读取时报错并提示迁移，不做静默回退。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_yaml::{Mapping, Value};

use crate::config::ConfigErrors;

/// 引用前缀。配置里写成 `credential/<name>`。
pub const PREFIX: &str = "credential/";

/// 上游凭据引用所在的顶层段。
pub const REFS_SECTION: &str = "refs";

/// 对外 API key 所在的顶层段。
pub const KEYS_SECTION: &str = "keys";

/// 旧布局的顶层键；读取时提示迁移。
const LEGACY_SECTION: &str = "credentials";

/// 凭据名长度上限（字节）。
const MAX_NAME_BYTES: usize = 64;

/// 凭据文件权限：只允许属主读写。
const FILE_MODE: u32 = 0o600;

/// 一条凭据的引用：POSIX 风格标识符，构造时校验语法。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CredentialRef(String);

impl CredentialRef {
    /// 在配置字段 `path` 下解析引用，错误直接带出字段路径。
    pub fn parse_at(raw: &str, path: &str) -> Result<Self, ConfigErrors> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(ConfigErrors::single(path, "不能为空"));
        }
        let Some(name) = raw.strip_prefix(PREFIX) else {
            return Err(ConfigErrors::single(
                path,
                format!(
                    "必须是 `{PREFIX}<name>` 形式的凭据引用（`{raw}` 已废弃）；\
                     密钥值请写进凭据文件，配置里只留引用"
                ),
            ));
        };
        if !valid_name(name) {
            return Err(ConfigErrors::single(
                path,
                format!(
                    "凭据名 `{name}` 不合法，需匹配 ^[A-Za-z_][A-Za-z0-9_]*$ 且不超过 {MAX_NAME_BYTES} 字节"
                ),
            ));
        }
        Ok(Self(name.to_string()))
    }

    /// 裸名字（不带 `credential/` 前缀）。
    pub fn name(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CredentialRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{PREFIX}{}", self.0)
    }
}

/// 凭据名的语法：POSIX 环境变量名风格。
pub fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().expect("非空已校验");
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 由 provider id 派生「约定」凭据引用：`upper(id)` + `_API_KEY`。
///
/// `-` 等非字母数字字符归一成 `_`（provider id 允许连字符，凭据名不允许），
/// 例如 `openai-responses` → `credential/OPENAI_RESPONSES_API_KEY`。
/// 控制台新建/更新 provider 时用它，保证同一份配置的命名可预测。
pub fn credential_ref_for(provider_id: &str) -> Result<CredentialRef, ConfigErrors> {
    let normalized: String = provider_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    CredentialRef::parse_at(
        &format!("{PREFIX}{normalized}_API_KEY"),
        "providers.api_key",
    )
}

/// 解析结果：值 + 提供该值的来源层。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCredential {
    pub value: String,
    /// 来源层 id；本地 YAML 实现固定为 `file`。
    pub source: String,
}

/// 配置界面可见的描述：**永远不含值**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialInfo {
    pub configured: bool,
    pub source: Option<String>,
    pub writable: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    /// 没有可回写的凭据文件（只读模式）。
    #[error("凭据存储不可写（未指定凭据文件）")]
    ReadOnly,
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Invalid(String),
}

/// 凭据存储的插拔点。实现必须 `Send + Sync`，`resolve` 按操作调用。
pub trait CredentialStore: Send + Sync {
    /// 解析一条引用；未配置返回 `None`。
    fn resolve(&self, reference: &CredentialRef) -> Option<ResolvedCredential>;

    /// 配置界面用的描述（不含值）。
    fn describe(&self, reference: &CredentialRef) -> CredentialInfo;

    /// 已登记的凭据名（按名字排序）。
    fn list(&self) -> Vec<(String, CredentialInfo)>;

    /// 写入/轮换一条凭据。空值不接受（用 [`CredentialStore::unset`] 删除）。
    fn set(&self, reference: &CredentialRef, value: &str) -> Result<(), CredentialError>;

    /// 删除一条凭据。
    fn unset(&self, reference: &CredentialRef) -> Result<(), CredentialError>;

    /// 存储位置（给「关于」页展示）；`None` 表示只读/无文件。
    fn location(&self) -> Option<&Path> {
        None
    }
}

/// YAML 文件存储：`credentials.yaml` 里的 `refs` 映射。
///
/// 每次 `resolve` 都重新读盘（文件只有几 KB），因此外部手工编辑与控制器写入
/// 都立刻可见，不需要重启也不需要失效缓存。`path = None` 表示只读模式。
pub struct YamlCredentialStore {
    path: Option<PathBuf>,
    /// 串行化写入，避免两次 set 互相覆盖。
    write_gate: Mutex<()>,
}

impl YamlCredentialStore {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            write_gate: Mutex::new(()),
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// 读取 `refs` 映射；文件不存在或为空时视为空映射。
    fn read(&self) -> Result<Mapping, CredentialError> {
        let Some(path) = &self.path else {
            return Ok(Mapping::new());
        };
        let root = read_root(path)?;
        extract_section(&root, REFS_SECTION, path)
    }

    fn lookup(&self, reference: &CredentialRef) -> Option<String> {
        let mapping = self.read().ok()?;
        let value = mapping.get(Value::String(reference.name().to_string()))?;
        let text = value.as_str()?.trim().to_string();
        if text.is_empty() {
            return None;
        }
        Some(text)
    }

    fn write(&self, mapping: &Mapping) -> Result<(), CredentialError> {
        let path = self.path.as_ref().ok_or(CredentialError::ReadOnly)?;
        let mut root = read_root(path)?;
        root.insert(
            Value::String(REFS_SECTION.into()),
            Value::Mapping(mapping.clone()),
        );
        write_root(path, &root)
    }
}

/// 读取整份凭据文档；文件不存在或为空时视为空映射。
pub(crate) fn read_root(path: &Path) -> Result<Mapping, CredentialError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Mapping::new()),
        Err(err) => {
            return Err(CredentialError::Io(format!(
                "读取凭据文件 {} 失败：{err}",
                path.display()
            )));
        }
    };
    if text.trim().is_empty() {
        return Ok(Mapping::new());
    }
    let value: Value = serde_yaml::from_str(&text).map_err(|err| {
        CredentialError::Io(format!("解析凭据文件 {} 失败：{err}", path.display()))
    })?;
    match value {
        Value::Null => Ok(Mapping::new()),
        Value::Mapping(mapping) => Ok(mapping),
        _ => Err(CredentialError::Invalid("凭据文件的顶层必须是映射".into())),
    }
}

/// 取一个顶层段。段缺失/为空 → 空映射；非映射 → 错误。
///
/// 读取 `refs` 时若只看到旧布局的顶层 `credentials`，报错并提示迁移：
/// 键名是安全的，值永远不出现在错误消息里。
pub(crate) fn extract_section(
    root: &Mapping,
    section: &str,
    path: &Path,
) -> Result<Mapping, CredentialError> {
    if section == REFS_SECTION
        && !root.contains_key(REFS_SECTION)
        && root.contains_key(LEGACY_SECTION)
    {
        return Err(CredentialError::Invalid(format!(
            "凭据文件 {} 使用旧布局（顶层 `{LEGACY_SECTION}`）：请把条目嵌套到 `{REFS_SECTION}:` 映射下，值不变",
            path.display()
        )));
    }
    match root.get(section) {
        None | Some(Value::Null) => Ok(Mapping::new()),
        Some(Value::Mapping(mapping)) => Ok(mapping.clone()),
        Some(_) => Err(CredentialError::Invalid(format!(
            "凭据文件的顶层 `{section}` 必须是映射"
        ))),
    }
}

/// 原子写回整份凭据文档（保留调用方未触碰的其它顶层段）。
pub(crate) fn write_root(path: &Path, root: &Mapping) -> Result<(), CredentialError> {
    let yaml = serde_yaml::to_string(&Value::Mapping(root.clone()))
        .map_err(|err| CredentialError::Io(format!("序列化凭据失败：{err}")))?;
    write_atomic(path, &yaml)
        .map_err(|err| CredentialError::Io(format!("写入凭据文件 {} 失败：{err}", path.display())))
}

impl CredentialStore for YamlCredentialStore {
    fn resolve(&self, reference: &CredentialRef) -> Option<ResolvedCredential> {
        self.lookup(reference).map(|value| ResolvedCredential {
            value,
            source: "file".to_string(),
        })
    }

    fn describe(&self, reference: &CredentialRef) -> CredentialInfo {
        let resolved = self.resolve(reference);
        CredentialInfo {
            configured: resolved.is_some(),
            source: resolved.map(|resolved| resolved.source),
            writable: self.path.is_some(),
        }
    }

    fn list(&self) -> Vec<(String, CredentialInfo)> {
        let writable = self.path.is_some();
        let mut names: Vec<String> = self
            .read()
            .map(|mapping| {
                mapping
                    .keys()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
            .into_iter()
            .map(|name| {
                let configured =
                    match CredentialRef::parse_at(&format!("{PREFIX}{name}"), "credentials") {
                        Ok(reference) => self.resolve(&reference).is_some(),
                        Err(_) => false,
                    };
                (
                    name,
                    CredentialInfo {
                        configured,
                        source: configured.then(|| "file".to_string()),
                        writable,
                    },
                )
            })
            .collect()
    }

    fn set(&self, reference: &CredentialRef, value: &str) -> Result<(), CredentialError> {
        if self.path.is_none() {
            return Err(CredentialError::ReadOnly);
        }
        let value = value.trim();
        if value.is_empty() {
            return Err(CredentialError::Invalid("凭据值不能为空".into()));
        }
        let _gate = self
            .write_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut mapping = self.read()?;
        mapping.insert(
            Value::String(reference.name().to_string()),
            Value::String(value.to_string()),
        );
        self.write(&mapping)
    }

    fn location(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn unset(&self, reference: &CredentialRef) -> Result<(), CredentialError> {
        if self.path.is_none() {
            return Err(CredentialError::ReadOnly);
        }
        let _gate = self
            .write_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut mapping = self.read()?;
        mapping.remove(Value::String(reference.name().to_string()));
        self.write(&mapping)
    }
}

/// 临时文件 + `fsync` + `rename`，并强制 `0600`。
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;

    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&dir)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("credentials.yaml");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp = dir.join(format!(".{name}.tmp-{}-{nanos}", std::process::id()));

    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(FILE_MODE);
        }
        let mut file = options.open(&temp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        if let Ok(dir_handle) = std::fs::File::open(&dir) {
            let _ = dir_handle.sync_all();
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!("anybridge-cred-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    #[test]
    fn reference_syntax_is_enforced() {
        assert_eq!(
            CredentialRef::parse_at("credential/wpsai", "p")
                .unwrap()
                .name(),
            "wpsai"
        );
        assert!(CredentialRef::parse_at("credential/_A1", "p").is_ok());
        for bad in [
            "",
            "wpsai",
            "os.environ/WPSAI_API_KEY",
            "credential/",
            "credential/1abc",
            "credential/a-b",
        ] {
            assert!(
                CredentialRef::parse_at(bad, "providers.p.api_key").is_err(),
                "应拒绝 `{bad}`"
            );
        }
    }

    #[test]
    fn unresolved_reference_reports_the_field_path() {
        let errors = CredentialRef::parse_at("os.environ/X", "providers.p.api_key").unwrap_err();
        assert_eq!(errors.0[0].path, "providers.p.api_key");
        assert!(errors.0[0].message.contains("credential/"));
    }

    #[test]
    fn set_resolve_unset_round_trip() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("credentials.yaml");
        let store = YamlCredentialStore::new(Some(path.clone()));
        let reference = CredentialRef::parse_at("credential/wpsai", "p").unwrap();

        assert!(store.resolve(&reference).is_none());
        assert!(!store.describe(&reference).configured);
        assert!(store.describe(&reference).writable);

        store.set(&reference, "sk-live").unwrap();
        assert_eq!(
            store.resolve(&reference).unwrap().value,
            "sk-live",
            "写入后应能解析"
        );
        assert_eq!(store.resolve(&reference).unwrap().source, "file");

        let mode = std::fs::metadata(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                mode.permissions().mode() & 0o777,
                0o600,
                "凭据文件必须是 0600"
            );
        }

        store.set(&reference, "sk-rotated").unwrap();
        assert_eq!(store.resolve(&reference).unwrap().value, "sk-rotated");
        assert_eq!(store.list().len(), 1);

        store.unset(&reference).unwrap();
        assert!(store.resolve(&reference).is_none());
        assert!(store.list().is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_value_counts_as_unconfigured() {
        let dir = temp_dir("empty");
        let path = dir.join("credentials.yaml");
        std::fs::write(&path, "refs:\n  wpsai: \"\"\n").unwrap();
        let store = YamlCredentialStore::new(Some(path));
        let reference = CredentialRef::parse_at("credential/wpsai", "p").unwrap();
        assert!(store.resolve(&reference).is_none(), "空值视为未配置");
        assert!(!store.describe(&reference).configured);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn legacy_credentials_key_is_rejected_with_migration_hint() {
        let dir = temp_dir("legacy");
        let path = dir.join("credentials.yaml");
        std::fs::write(&path, "credentials:\n  wpsai: sk-secret-value\n").unwrap();
        let store = YamlCredentialStore::new(Some(path));
        let err = store.read().unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("refs"), "{rendered}");
        assert!(rendered.contains("旧布局"), "{rendered}");
        assert!(
            !rendered.contains("sk-secret-value"),
            "错误消息不能泄露值：{rendered}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn writing_refs_preserves_the_keys_section() {
        let dir = temp_dir("sections");
        let path = dir.join("credentials.yaml");
        std::fs::write(&path, "refs:\n  wpsai: sk-a\nkeys:\n  - name: alice\n").unwrap();
        let store = YamlCredentialStore::new(Some(path.clone()));
        let reference = CredentialRef::parse_at("credential/wpsai", "p").unwrap();
        store.set(&reference, "sk-b").unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("keys:"), "{written}");
        assert!(written.contains("name: alice"), "{written}");
        assert!(written.contains("sk-b"), "{written}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_only_store_rejects_writes() {
        let store = YamlCredentialStore::new(None);
        let reference = CredentialRef::parse_at("credential/wpsai", "p").unwrap();
        assert!(store.resolve(&reference).is_none());
        assert!(!store.describe(&reference).writable);
        assert!(matches!(
            store.set(&reference, "sk-x"),
            Err(CredentialError::ReadOnly)
        ));
        assert!(matches!(
            store.unset(&reference),
            Err(CredentialError::ReadOnly)
        ));
    }

    #[test]
    fn missing_file_is_treated_as_empty() {
        let dir = temp_dir("missing");
        let store = YamlCredentialStore::new(Some(dir.join("nope.yaml")));
        let reference = CredentialRef::parse_at("credential/wpsai", "p").unwrap();
        assert!(store.resolve(&reference).is_none());
        assert!(store.list().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
