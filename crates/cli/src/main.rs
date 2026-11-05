//! anybridge-litellm 二进制入口：手写参数解析、日志、信号、启动 server。

use std::path::PathBuf;

use anybridge_litellm_core::{load_from_path, GatewayConfig, LoadOptions};
use tracing_subscriber::EnvFilter;

const BIN_NAME: &str = "anybridge-litellm";
const DEFAULT_CONFIG: &str = "config.yaml";

const HELP: &str = "\
anybridge-litellm —— 上游 provider 的纯代理网关

用法:
  anybridge-litellm [选项]

选项:
  --config <path>    配置文件路径（缺省 config.yaml）
  --strict-config    未知配置键视为错误
  --bind <addr>      覆盖配置里的 server.bind
  -V, --version      打印版本
  -h, --help         打印本帮助

环境变量:
  RUST_LOG           日志过滤（缺省 info）
  GATEWAY_MASTER_KEY 等 os.environ/VAR 引用的变量在配置加载时展开
";

struct Options {
    config: PathBuf,
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
            print!("{HELP}");
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

    let config = match load_from_path(
        &options.config,
        &LoadOptions {
            strict: options.strict,
        },
    ) {
        Ok(outcome) => {
            for warning in &outcome.warnings {
                tracing::warn!("{warning}");
            }
            outcome.config
        }
        Err(errors) => {
            eprintln!("{errors}");
            return std::process::ExitCode::from(2);
        }
    };

    // `--bind` 是命令行覆盖：既用于监听，也交给配置存储，保证控制台热加载后仍然生效。
    let bind = options
        .bind
        .clone()
        .unwrap_or_else(|| config.server.bind.clone());

    // 控制台 token 只在这里出现一次：打印带 token 的 URL，掉出日志即需重启换新。
    let console_token = anybridge_litellm_server::new_token();
    println!("listening on http://{bind}");
    println!("console: http://{bind}/console/?token={console_token}");

    let state = anybridge_litellm_server::AppState::with_token_and_path(
        config,
        console_token,
        Some(options.config),
        options.bind,
    );

    match anybridge_litellm_server::serve(state, &bind, shutdown_signal()).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{BIN_NAME}: 服务启动失败：{err}");
            std::process::ExitCode::FAILURE
        }
    }
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

    fn parse(args: &[&str]) -> Result<Parsed, String> {
        parse_args(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn defaults() {
        let Parsed::Run(options) = parse(&[]).unwrap() else {
            panic!("应进入运行模式");
        };
        assert_eq!(options.config, PathBuf::from(DEFAULT_CONFIG));
        assert!(!options.strict);
        assert!(options.bind.is_none());
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
