//! 配置文件的可写存储：热加载 + 原子写盘。
//!
//! 控制台的写接口都经过 [`ConfigStore::mutate`]：拿写锁 → 从磁盘读当前文件
//! （顺带吸收外部改动）→ 交给闭包改 YAML 树 → 整份校验 → 原子替换 → 换内存快照。
//! 读路径只做一次 `Arc` 克隆，不阻塞请求。
//!
//! 写盘约束（设计 §3.5）：
//!
//! - 临时文件与目标同目录，`create_new` + 0600，`fsync` 后 `rename`，最后 fsync 目录。
//! - 只写 `os.environ/VAR` 引用；展开后的密钥永远不会出现在文件里。
//! - 校验不通过则**不落盘**，内存快照保持原样。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anybridge_litellm_core::{ConfigError, ConfigErrors, CredentialRef, Document, GatewayConfig};

/// 一份对外可见的配置快照。
#[derive(Debug)]
pub struct Snapshot {
    pub config: Arc<GatewayConfig>,
    /// provider → 凭据引用。只含引用名，永远不含值。
    pub refs: BTreeMap<String, CredentialRef>,
}

impl Snapshot {
    pub fn credential_ref(&self, provider: &str) -> Option<&CredentialRef> {
        self.refs.get(provider)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MutationError {
    /// 未以 `--config` 启动，没有可回写的文件。
    #[error("服务未以 --config 启动，配置不可写")]
    ReadOnly,
    /// 改完的文档没通过校验；不会落盘。
    #[error("配置校验失败")]
    Invalid(Vec<ConfigError>),
    #[error("{0}")]
    Io(String),
}

pub struct ConfigStore {
    path: Option<PathBuf>,
    /// `--bind` 覆盖：每次重新加载后都要重新套用，否则控制台改一次配置就“丢”了。
    bind_override: Option<String>,
    snapshot: RwLock<Arc<Snapshot>>,
    /// 写锁：保证「读文件 → 改 → 写文件」串行。
    write_gate: tokio::sync::Mutex<()>,
}

impl ConfigStore {
    pub fn new(
        mut config: GatewayConfig,
        path: Option<PathBuf>,
        bind_override: Option<String>,
    ) -> Self {
        let refs = path
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| Document::parse(&text).ok())
            .map(|document| document.api_key_refs())
            .unwrap_or_default();

        if let Some(bind) = &bind_override {
            config.server.bind = bind.clone();
        }

        Self {
            path,
            bind_override,
            snapshot: RwLock::new(Arc::new(Snapshot {
                config: Arc::new(config),
                refs,
            })),
            write_gate: tokio::sync::Mutex::new(()),
        }
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        // 读锁只活一次克隆；写线程 panic 也不会让读路径挂死。
        self.snapshot
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn is_writable(&self) -> bool {
        self.path.is_some()
    }

    /// 配置文件路径；`None` 表示只读模式。
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// 串行的读-改-写。闭包拿到的是**磁盘当前**文档，返回值不需要自己校验。
    pub async fn mutate<F>(&self, apply: F) -> Result<Arc<Snapshot>, MutationError>
    where
        F: FnOnce(&mut Document) -> Result<(), ConfigErrors>,
    {
        let path = self.path.clone().ok_or(MutationError::ReadOnly)?;
        let _gate = self.write_gate.lock().await;

        let text = tokio::fs::read_to_string(&path)
            .await
            .map_err(|err| MutationError::Io(format!("读取配置 {} 失败：{err}", path.display())))?;
        let mut document = Document::parse(&text).map_err(|err| MutationError::Invalid(err.0))?;

        apply(&mut document).map_err(|err| MutationError::Invalid(err.0))?;

        // 整份文档走现有加载器：任何字段错误都在这里被拒，绝不写半份配置。
        let outcome = document
            .validate()
            .map_err(|err| MutationError::Invalid(err.0))?;
        let yaml = document
            .to_yaml()
            .map_err(|err| MutationError::Invalid(err.0))?;

        write_atomic(&path, &yaml)
            .await
            .map_err(|err| MutationError::Io(format!("写入配置 {} 失败：{err}", path.display())))?;

        let mut config = outcome.config;
        if let Some(bind) = &self.bind_override {
            config.server.bind = bind.clone();
        }

        let snapshot = Arc::new(Snapshot {
            config: Arc::new(config),
            refs: document.api_key_refs(),
        });
        *self
            .snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = snapshot.clone();
        Ok(snapshot)
    }
}

/// 临时文件 + `fsync` + `rename`，并强制 0600。
async fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.yaml");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp = dir.join(format!(".{name}.tmp-{}-{nanos}", std::process::id()));

    let result = write_temp(&temp, contents).await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
        return result;
    }

    tokio::fs::rename(&temp, path).await?;

    // rename 只保证目录项可见；fsync 目录才保证掉电后仍指向新 inode。
    if let Ok(handle) = std::fs::File::open(&dir) {
        let _ = handle.sync_all();
    }
    Ok(())
}

async fn write_temp(path: &Path, contents: &str) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;

    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        // 配置里可能有明文密钥（老配置文件），写回时一律收紧到 0600。
        options.mode(0o600);
    }
    let mut file = options.open(path).await?;
    file.write_all(contents.as_bytes()).await?;
    file.sync_all().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anybridge_litellm_core::load_from_str;

    const YAML: &str = r#"
auth:
  master_key: master-key
providers:
  openai:
    api: completions
    base_url: https://api.openai.com/v1
    api_key: credential/openai
    models:
      - id: gpt-4o
"#;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "anybridge-store-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn store(dir: &Path) -> ConfigStore {
        let path = dir.join("config.yaml");
        std::fs::write(&path, YAML).unwrap();
        let config = load_from_str(YAML, &Default::default()).unwrap().config;
        ConfigStore::new(config, Some(path), None)
    }

    #[tokio::test]
    async fn mutate_reloads_and_persists() {
        let dir = temp_dir("persist");
        let store = store(&dir);

        let snapshot = store
            .mutate(|document| {
                document.upsert_provider(
                    &anybridge_litellm_core::ProviderDraft {
                        name: "local".into(),
                        display_name: Some("Local".into()),
                        api: "completions".into(),
                        base_url: "http://127.0.0.1:11434/v1".into(),
                        api_key_env: Some("credential/local".into()),
                        timeout: Some(15),
                        api_version: None,
                    },
                    true,
                )
            })
            .await
            .expect("写入应成功");

        assert!(snapshot.config.providers.contains_key("local"));
        assert!(store.snapshot().config.providers.contains_key("local"));

        let written = std::fs::read_to_string(dir.join("config.yaml")).unwrap();
        assert!(written.contains("local:"));
        assert!(
            written.contains("api_key: credential/LOCAL_API_KEY"),
            "凭据引用由 provider id 派生：{written}"
        );
        assert_eq!(
            snapshot.credential_ref("local").map(ToString::to_string),
            Some("credential/LOCAL_API_KEY".to_string())
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn written_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("mode");
        let store = store(&dir);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            store
                .mutate(|document| {
                    document.upsert_model(
                        "openai",
                        Some("gpt-4o"),
                        &anybridge_litellm_core::ModelDraft {
                            id: "gpt-4o".into(),
                            ..Default::default()
                        },
                    )
                })
                .await
                .unwrap();
        });
        let mode = std::fs::metadata(dir.join("config.yaml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "配置文件必须 0600");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn invalid_document_does_not_touch_disk() {
        let dir = temp_dir("invalid");
        let store = store(&dir);
        let before = std::fs::read_to_string(dir.join("config.yaml")).unwrap();

        let err = store
            .mutate(|document| {
                document.upsert_provider(
                    &anybridge_litellm_core::ProviderDraft {
                        name: "bad".into(),
                        display_name: None,
                        api: "chat".into(),
                        base_url: "https://example.com/v1".into(),
                        api_key_env: Some("credential/local".into()),
                        timeout: None,
                        api_version: None,
                    },
                    true,
                )
            })
            .await
            .unwrap_err();

        match err {
            MutationError::Invalid(errors) => {
                assert_eq!(errors[0].path, "providers.bad.api");
            }
            other => panic!("应为校验错误，实际 {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("config.yaml")).unwrap(),
            before,
            "校验失败不得落盘"
        );
        assert!(!store.snapshot().config.providers.contains_key("bad"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn read_only_store_rejects_mutations() {
        let config = load_from_str(YAML, &Default::default()).unwrap().config;
        let store = ConfigStore::new(config, None, None);
        assert!(!store.is_writable());
        let err = store.mutate(|_| Ok(())).await.unwrap_err();
        assert!(matches!(err, MutationError::ReadOnly));
    }

    #[tokio::test]
    async fn bind_override_survives_mutation() {
        let dir = temp_dir("bind");
        let path = dir.join("config.yaml");
        std::fs::write(&path, YAML).unwrap();
        let config = load_from_str(YAML, &Default::default()).unwrap().config;
        let store = ConfigStore::new(config, Some(path), Some("127.0.0.1:4999".into()));

        let snapshot = store
            .mutate(|document| {
                document.upsert_model(
                    "openai",
                    None,
                    &anybridge_litellm_core::ModelDraft {
                        id: "gpt-4o-mini".into(),
                        ..Default::default()
                    },
                )
            })
            .await
            .unwrap();
        assert_eq!(snapshot.config.server.bind, "127.0.0.1:4999");
        std::fs::remove_dir_all(&dir).ok();
    }
}
