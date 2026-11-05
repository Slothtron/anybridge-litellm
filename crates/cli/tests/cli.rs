//! L2 黑盒测试：spawn 真二进制，覆盖版本/帮助/坏配置退出码与 `/health` 冒烟。

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_anybridge-litellm");

fn run(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("启动二进制失败")
}

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "anybridge-litellm-cli-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn write_config(dir: &std::path::Path, bind: &str) -> PathBuf {
    let path = dir.join("config.yaml");
    let yaml = format!(
        r#"
server:
  bind: "{bind}"
auth:
  master_key: test-master-key
providers:
  openai:
    api: completions
    base_url: http://127.0.0.1:1/v1
    api_key: sk-test-cli
    models:
      - id: gpt-4o
"#
    );
    std::fs::write(&path, yaml).unwrap();
    path
}

fn http_get(addr: &str, path: &str) -> Option<String> {
    http_get_with(addr, path, &[])
}

fn http_get_with(addr: &str, path: &str, extra_headers: &[(&str, &str)]) -> Option<String> {
    let mut stream = std::net::TcpStream::connect(addr).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .ok()?;
    let mut request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (name, value) in extra_headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    write!(stream, "{request}").ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    Some(response)
}

/// 轮询等待服务就绪。
fn wait_for_health(addr: &str, child: &mut Child) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("服务在就绪前退出：{status:?}");
        }
        if let Some(response) = http_get(addr, "/health") {
            if response.contains("200 OK") {
                return response;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("等待 /health 超时");
}

#[test]
fn version_prints_package_version() {
    for flag in ["--version", "-V"] {
        let output = run(&[flag]);
        assert!(output.status.success(), "{flag} 应成功");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(env!("CARGO_PKG_VERSION")),
            "{flag} 输出：{stdout}"
        );
    }
}

#[test]
fn help_lists_options() {
    for flag in ["--help", "-h"] {
        let output = run(&[flag]);
        assert!(output.status.success(), "{flag} 应成功");
        let stdout = String::from_utf8_lossy(&output.stdout);
        for expected in ["--config", "--strict-config", "--bind", "--version"] {
            assert!(
                stdout.contains(expected),
                "{flag} 缺少 {expected}：{stdout}"
            );
        }
    }
}

#[test]
fn unknown_argument_exits_2() {
    let output = run(&["--nope"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("未知参数"), "stderr：{stderr}");
}

#[test]
fn bad_config_exits_2_with_field_path() {
    let dir = unique_dir("bad-config");
    let path = dir.join("config.yaml");
    // 缺 auth.master_key；providers 允许为空。
    std::fs::write(
        &path,
        "server:\n  bind: \"127.0.0.1:4000\"\nproviders: {}\n",
    )
    .unwrap();

    let output = run(&["--config", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2), "坏配置应以 2 退出");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("auth.master_key"),
        "stderr 应含字段路径：{stderr}"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn missing_config_file_exits_2() {
    let output = run(&["--config", "/nonexistent/anybridge-litellm.yaml"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("config"), "stderr：{stderr}");
}

#[test]
fn strict_config_rejects_unknown_keys() {
    let dir = unique_dir("strict");
    let path = dir.join("config.yaml");
    std::fs::write(
        &path,
        r#"
auth:
  master_key: test-master-key
providers:
  openai:
    api: completions
    base_url: http://127.0.0.1:1/v1
    api_key: sk-test-cli
    unknown_key: 1
    models:
      - id: gpt-4o
"#,
    )
    .unwrap();

    let output = run(&["--config", path.to_str().unwrap(), "--strict-config"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("providers.openai.unknown_key"),
        "stderr：{stderr}"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn serving_health_then_terminates() {
    let dir = unique_dir("serve");
    let port = free_port();
    let bind = format!("127.0.0.1:{port}");
    let config = write_config(&dir, &bind);

    let mut child = Command::new(BIN)
        .args(["--config", config.to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动服务失败");

    let response = wait_for_health(&bind, &mut child);
    assert!(response.contains("\"ok\":true"), "响应：{response}");
    assert!(
        response.contains("\"providers\":[\"openai\"]"),
        "响应：{response}"
    );

    // 根发现路径也应可用，且不需要 key。
    let discovery = http_get(&bind, "/").expect("请求 / 失败");
    assert!(discovery.contains("200 OK"), "响应：{discovery}");
    assert!(discovery.contains("anybridge-litellm"), "响应：{discovery}");

    child.kill().unwrap();
    child.wait().unwrap();

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn console_url_is_printed_and_serves_state() {
    use std::io::BufRead;

    let dir = unique_dir("console");
    let port = free_port();
    let bind = format!("127.0.0.1:{port}");
    let config = write_config(&dir, &bind);

    let mut child = Command::new(BIN)
        .args(["--config", config.to_str().unwrap()])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("启动服务失败");

    // 后台读 stdout，主线程等健康检查，避免管道缓冲互相阻塞。
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let stdout = child.stdout.take().expect("应有 stdout");
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    wait_for_health(&bind, &mut child);

    let mut console_line = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) if line.starts_with("console: ") => {
                console_line = Some(line);
                break;
            }
            Ok(_) => continue,
            Err(_) => continue,
        }
    }
    let console_line = console_line.expect("启动日志应打印带 token 的控制台地址");
    let token = console_line
        .rsplit_once("token=")
        .map(|(_, token)| token.to_string())
        .expect("控制台地址必须带 token");
    assert_eq!(token.len(), 32, "token 应是 32 位十六进制：{console_line}");
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));

    // 页面本身不需要 token（token 在 URL 里，由前端提取）。
    let index = http_get(&bind, "/console/").expect("请求 /console/ 失败");
    assert!(index.contains("200 OK"), "{index}");
    assert!(index.contains("text/html"), "{index}");

    // 接口必须带 token。
    let unauthorized = http_get(&bind, "/console/api/state").expect("请求失败");
    assert!(unauthorized.contains("401"), "{unauthorized}");

    let state = http_get_with(
        &bind,
        "/console/api/state",
        &[("x-console-token", token.as_str())],
    )
    .expect("请求失败");
    assert!(state.contains("200 OK"), "{state}");
    assert!(state.contains("anybridge-litellm"), "{state}");
    assert!(state.contains("/llmproxy/openai/v1"), "{state}");
    // 上游 key 不能出现在控制台响应里。
    assert!(!state.contains("sk-test-cli"), "{state}");

    child.kill().unwrap();
    child.wait().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}
