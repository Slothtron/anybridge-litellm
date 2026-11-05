//! systemd unit 的黑盒校验：结构断言 + 真实 `systemctl --user` 加载。
//!
//! 加载用例会把 unit 软链进用户 unit 目录再 `daemon-reload`，因此用独立的
//! 名字（`<服务名>-selftest.service`）并在 `Drop` 里无条件清理。
//! 没有用户级 systemd（容器/CI）时跳过，不算失败。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 服务名唯一来源是 `core::GATEWAY_NAME`，unit 文件名跟着它走。
const NAME: &str = anybridge_litellm_core::GATEWAY_NAME;

fn unit_name() -> String {
    format!("{NAME}.service")
}

fn selftest_name() -> String {
    format!("{NAME}-selftest.service")
}

fn unit_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../deploy")
        .join(unit_name())
}

fn systemctl_user(args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("systemctl").arg("--user").args(args).output()
}

fn user_systemd_available() -> bool {
    matches!(systemctl_user(&["show-environment"]), Ok(out) if out.status.success())
}

/// 清理软链并让 systemd 忘掉它；`Drop` 里跑，panic 也能收回。
struct LinkGuard {
    link: PathBuf,
}

impl Drop for LinkGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.link);
        let _ = systemctl_user(&["daemon-reload"]);
    }
}

#[test]
fn unit_file_has_required_directives() {
    let content = fs::read_to_string(unit_path()).expect("deploy/anybridge-litellm.service 应存在");

    let exec_start = content
        .lines()
        .find(|line| line.starts_with("ExecStart="))
        .expect("unit 必须有 ExecStart");

    // ExecStart 必须是绝对路径（systemd 不做 PATH 查找；`%h` 会展开成家目录）。
    let program = exec_start
        .trim_start_matches("ExecStart=")
        .split_whitespace()
        .next()
        .expect("ExecStart 里应有可执行文件");
    assert!(
        program.starts_with('/') || program.starts_with("%h/"),
        "ExecStart 必须是绝对路径，实际 `{program}`"
    );
    assert!(
        exec_start.contains("--config "),
        "ExecStart 必须显式传 --config"
    );
    assert!(
        exec_start.contains("--bind 127.0.0.1:"),
        "ExecStart 必须把 bind 钉在回环地址，实际 `{exec_start}`"
    );

    for directive in [
        "Restart=on-failure",
        "KillSignal=SIGTERM",
        "IPAddressAllow=localhost",
        "IPAddressDeny=any",
        "NoNewPrivileges=true",
        // 控制台写配置需要这个例外，否则 ProtectHome=read-only 会让写回失败。
        &format!("ReadWritePaths=%h/.config/{NAME}"),
        "WantedBy=default.target",
    ] {
        assert!(
            content.lines().any(|line| line.trim() == directive),
            "unit 缺少 `{directive}`"
        );
    }
}

#[test]
fn systemctl_user_can_load_unit() {
    if !user_systemd_available() {
        eprintln!("跳过：当前环境没有用户级 systemd");
        return;
    }

    let dir = std::env::temp_dir().join(format!("ablt-unit-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("创建临时目录失败");
    let copy = dir.join(selftest_name());
    fs::copy(unit_path(), &copy).expect("复制 unit 失败");

    // 先建 guard 再 link：即使后续断言失败也能清掉软链。
    let user_unit_dir = Command::new("sh")
        .arg("-c")
        .arg("printf %s \"${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user\"")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(PathBuf::from)
        .expect("解析用户 unit 目录失败");
    let _guard = LinkGuard {
        link: user_unit_dir.join(selftest_name()),
    };

    let link =
        systemctl_user(&["link", copy.to_str().unwrap()]).expect("执行 systemctl --user link");
    assert!(
        link.status.success(),
        "systemctl --user link 失败：{}",
        String::from_utf8_lossy(&link.stderr)
    );

    let reload = systemctl_user(&["daemon-reload"]).expect("执行 daemon-reload");
    assert!(reload.status.success(), "daemon-reload 失败");

    let show = systemctl_user(&["show", "-p", "LoadState", &selftest_name()]).expect("执行 show");
    let stdout = String::from_utf8_lossy(&show.stdout);
    assert_eq!(
        stdout.trim(),
        "LoadState=loaded",
        "systemd 未能加载 unit，实际输出：{stdout}"
    );
}
