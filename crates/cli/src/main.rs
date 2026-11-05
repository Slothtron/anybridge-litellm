//! 二进制入口：手写参数解析、日志、信号、启动 server。

use std::path::PathBuf;

use anybridge_litellm_core::{load_from_path, Document, GatewayConfig, LoadOptions};
use tracing_subscriber::EnvFilter;

/// 二进制名取自 Cargo（`[[bin]] name`），不在代码里手写。
const BIN_NAME: &str = env!("CARGO_BIN_NAME");
const DEFAULT_CONFIG: &str = "config.yaml";

fn help() -> String {
    format!(
        "\
{BIN_NAME} —— 上游 provider 的纯代理网关

用法:
  {BIN_NAME} [选项]

选项:
  --config <path>    配置文件路径（缺省 {DEFAULT_CONFIG}）
  --strict-config    未知配置键视为错误
  --bind <addr>      覆盖配置里的 server.bind
  -V, --version      打印版本
  -h, --help         打印本帮助

未显式指定 --config 且缺省文件不存在时，以空配置 + 只读模式启动
（控制台可浏览、不可写）；显式指定的路径不存在仍然直接报错。

环境变量:
  RUST_LOG           日志过滤（缺省 info）
  GATEWAY_MASTER_KEY 等 os.environ/VAR 引用的变量在配置加载时展开
"
    )
}

struct Options {
    config: PathBuf,
    /// 是否显式给了 `--config`。只有缺省路径不存在时才降级成只读模式。
    config_explicit: bool,
    strict: bool,
    bind: Option<String>,
}

enum Parsed {
    Run(Options),
    Version,
    Help,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut options = Options {
        config: PathBuf::from(DEFAULT_CONFIG),
        config_explicit: false,
        strict: false,
        bind: None,
    };

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" | "-V" => return Ok(Parsed::Version),
            "--help" | "-h" => return Ok(Parsed::Help),
            "--strict-config" => options.strict = true,
            "--config" => {
                let value = args
                    .next()
                    .ok_or_else(|| "`--config` 需要一个路径".to_string())?;
                options.config = PathBuf::from(value);
                options.config_explicit = true;
            }
            "--bind" => {
                let value = args
                    .next()
                    .ok_or_else(|| "`--bind` 需要一个地址".to_string())?;
                options.bind = Some(value);
            }
            other => return Err(format!("未知参数 `{other}`")),
        }
    }

    Ok(Parsed::Run(options))
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let options = match parse_args(std::env::args().skip(1)) {
        Ok(Parsed::Version) => {
            println!("{BIN_NAME} {}", GatewayConfig::version());
            return std::process::ExitCode::SUCCESS;
        }
        Ok(Parsed::Help) => {
            print!("{}", help());
            return std::process::ExitCode::SUCCESS;
        }
        Ok(Parsed::Run(options)) => options,
        Err(message) => {
            eprintln!("{BIN_NAME}: {message}");
            eprintln!("使用 `{BIN_NAME} --help` 查看用法");
            return std::process::ExitCode::from(2);
        }
    };

    init_tracing();

    // 没有显式 `--config` 且缺省文件不存在时，以「空配置 + 只读」启动：
    // 控制台仍可浏览（providers 为空），但写接口一律 409。
    // 显式 `--config` 指向不存在的文件仍然报错——那是打错路径，不该被静默吞掉。
    let read_only = !options.config_explicit && !options.config.exists();

    let (config, store_path) = if read_only {
        tracing::warn!(
            "未找到 {}，以只读模式启动（控制台不可写）",
            options.config.display()
        );
        const EMPTY: &str = "\
auth:
  master_key: os.environ/GATEWAY_MASTER_KEY
providers: {}
";
        match Document::parse(EMPTY).and_then(|document| document.validate()) {
            Ok(outcome) => (outcome.config, None),
            Err(errors) => {
                eprintln!("{errors}");
                return std::process::ExitCode::from(2);
            }
        }
    } else {
        match load_from_path(
            &options.config,
            &LoadOptions {
                strict: options.strict,
            },
        ) {
            Ok(outcome) => {
                for warning in &outcome.warnings {
                    tracing::warn!("{warning}");
                }
                (outcome.config, Some(options.config))
            }
            Err(errors) => {
                eprintln!("{errors}");
                return std::process::ExitCode::from(2);
            }
        }
    };

    // `--bind` 是命令行覆盖：既用于监听，也交给配置存储，保证控制台热加载后仍然生效。
    let bind = options
        .bind
        .clone()
        .unwrap_or_else(|| config.server.bind.clone());

    // 凭据文件：`credentials.file` 优先，否则落到 XDG 配置目录。
    // 只读模式下不提供凭据文件，凭据存储也就不可写（与「只读控制台」的承诺一致）。
    let credentials_path = if read_only {
        None
    } else {
        Some(
            config
                .credentials
                .file
                .clone()
                .unwrap_or_else(default_credentials_path),
        )
    };

    // 控制台 token 只在这里出现一次：打印带 token 的 URL，掉出日志即需重启换新。
    let console_token = anybridge_litellm_server::new_token();
    println!("listening on http://{bind}");
    println!("console: http://{bind}/console/?token={console_token}");

    // 会话密钥跟凭据文件同目录：重启后浏览器登录态仍然有效。
    let session_key = credentials_path
        .as_ref()
        .and_then(|path| path.parent())
        .map(|dir| dir.join("console-session.key"));

    let state = anybridge_litellm_server::AppState::with_token_and_path(
        config,
        console_token,
        store_path,
        options.bind,
        credentials_path,
    )
    .with_session_key(session_key);

    match anybridge_litellm_server::serve(state, &bind, shutdown_signal()).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{BIN_NAME}: 服务启动失败：{err}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// 默认凭据文件：`$XDG_CONFIG_HOME/anybridge-litellm/credentials.yaml`，
/// 没设 `XDG_CONFIG_HOME` 时退回 `~/.config/anybridge-litellm/credentials.yaml`。
fn default_credentials_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(BIN_NAME).join("credentials.yaml")
}

/// `ctrl_c` 与 `SIGTERM` 都触发优雅关闭。
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::warn!("监听 ctrl_c 失败：{err}");
        }
    };

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(sigterm) => sigterm,
            Err(err) => {
                tracing::warn!("监听 SIGTERM 失败：{err}");
                ctrl_c.await;
                return;
            }
        };
        tokio::select! {
            _ = ctrl_c => {}
            _ = sigterm.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anybridge_litellm_core::GATEWAY_NAME;

    fn parse(args: &[&str]) -> Result<Parsed, String> {
        parse_args(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn defaults() {
        let Parsed::Run(options) = parse(&[]).unwrap() else {
            panic!("应进入运行模式");
        };
        assert_eq!(options.config, PathBuf::from(DEFAULT_CONFIG));
        // 缺省路径不算显式指定：文件不存在时降级为只读模式。
        assert!(!options.config_explicit);
        assert!(!options.strict);
        assert!(options.bind.is_none());
    }

    #[test]
    fn binary_name_matches_gateway_name() {
        // 二进制名来自 Cargo，服务名来自 core 常量；改名时这条会挂。
        assert_eq!(BIN_NAME, GATEWAY_NAME);
    }

    #[test]
    fn read_only_document_has_empty_providers() {
        std::env::set_var("GATEWAY_MASTER_KEY", "sk-test");
        let outcome =
            Document::parse("auth:\n  master_key: os.environ/GATEWAY_MASTER_KEY\nproviders: {}\n")
                .and_then(|document| document.validate())
                .expect("空配置应可加载");
        assert!(outcome.config.providers.is_empty());
    }

    #[test]
    fn all_flags_parse() {
        let Parsed::Run(options) = parse(&[
            "--config",
            "custom.yaml",
            "--strict-config",
            "--bind",
            "127.0.0.1:5000",
        ])
        .unwrap() else {
            panic!("应进入运行模式");
        };
        assert_eq!(options.config, PathBuf::from("custom.yaml"));
        assert!(options.config_explicit);
        assert!(options.strict);
        assert_eq!(options.bind.as_deref(), Some("127.0.0.1:5000"));
    }

    #[test]
    fn version_and_help_short_and_long() {
        for flag in ["--version", "-V"] {
            assert!(matches!(parse(&[flag]).unwrap(), Parsed::Version));
        }
        for flag in ["--help", "-h"] {
            assert!(matches!(parse(&[flag]).unwrap(), Parsed::Help));
        }
    }

    #[test]
    fn unknown_and_missing_values_are_errors() {
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["--config"]).is_err());
        assert!(parse(&["--bind"]).is_err());
        assert!(parse(&["extra"]).is_err());
    }
}
