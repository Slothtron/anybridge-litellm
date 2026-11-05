//! L2 黑盒测试：控制台写接口对**真二进制**生效。
//!
//! 断言三件事：写回落到 `--config` 指定的文件（0600、只含 `os.environ/VAR`），
//! 新配置**不重启**即可路由，删除后立即 404。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_anybridge-litellm");
const MASTER_KEY: &str = "test-master-key";
const KEY_VAR: &str = "ANYBRIDGE_CRUD_KEY";
const KEY_VALUE: &str = "sk-crud-value";

const CONFIG_TEMPLATE: &str = r#"
server:
  bind: "{bind}"
auth:
  master_key: test-master-key
providers:
  openai:
    api: completions
    base_url: http://127.0.0.1:1/v1
    api_key: os.environ/ANYBRIDGE_CRUD_KEY
    models:
      - id: gpt-4o
  anthropic:
    api: anthropic
    base_url: http://127.0.0.1:1/v1
    api_key: os.environ/ANYBRIDGE_CRUD_KEY
    models:
      - id: claude-sonnet
"#;

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "anybridge-litellm-crud-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// 一进一出，读完即关。返回 `(status, body)`。
fn http(
    addr: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).expect("连接服务失败");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();

    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(body) = body {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    if let Some(body) = body {
        request.push_str(body);
    }
    stream.write_all(request.as_bytes()).expect("写请求失败");

    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("读响应失败");
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    (status, body)
}

fn wait_for_health(addr: &str, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("服务在就绪前退出：{status:?}");
        }
        if let Ok(mut stream) = TcpStream::connect(addr) {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let _ = stream.write_all(
                format!("GET /health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            );
            let mut raw = String::new();
            if stream.read_to_string(&mut raw).is_ok() && raw.contains("200 OK") {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("等待 /health 超时");
}

/// 从启动日志里取控制台 token。
fn read_console_token(child: &mut Child) -> String {
    use std::io::BufRead;

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

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(line) = rx.recv_timeout(Duration::from_millis(100)) {
            if let Some((_, token)) = line.rsplit_once("token=") {
                return token.to_string();
            }
        }
    }
    panic!("启动日志里没有带 token 的控制台地址");
}

struct Server {
    child: Child,
    dir: PathBuf,
    addr: String,
    token: String,
}

impl Server {
    fn start(tag: &str) -> Self {
        let dir = unique_dir(tag);
        let port = free_port();
        let addr = format!("127.0.0.1:{port}");

        let path = dir.join("config.yaml");
        std::fs::write(&path, CONFIG_TEMPLATE.replace("{bind}", &addr)).unwrap();

        let mut child = Command::new(BIN)
            .args(["--config", path.to_str().unwrap()])
            .env(KEY_VAR, KEY_VALUE)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("启动服务失败");

        wait_for_health(&addr, &mut child);
        let token = read_console_token(&mut child);
        Self {
            child,
            dir,
            addr,
            token,
        }
    }

    fn console(&self, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
        let mut headers = vec![("x-console-token", self.token.as_str())];
        if body.is_some() {
            headers.push(("content-type", "application/json"));
        }
        http(&self.addr, method, path, &headers, body)
    }

    fn config_path(&self) -> PathBuf {
        self.dir.join("config.yaml")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn provider_payload() -> String {
    format!(
        r#"{{"name":"local","api":"completions","base_url":"http://127.0.0.1:11434/v1","api_key":"os.environ/{KEY_VAR}","timeout":30}}"#
    )
}

#[test]
fn console_crud_persists_and_hot_reloads() {
    let server = Server::start("crud");
    let config = server.config_path();

    // 1. 新建 provider
    let (status, body) =
        server.console("POST", "/console/api/providers", Some(&provider_payload()));
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"local\""), "{body}");

    // 2. 新建模型
    let (status, body) = server.console(
        "POST",
        "/console/api/providers/local/models",
        Some(r#"{"id":"qwen3:8b","context_window":32768}"#),
    );
    assert_eq!(status, 200, "{body}");

    // 3. 落盘：0600 且只有环境变量引用
    let written = std::fs::read_to_string(&config).unwrap();
    assert!(written.contains("local:"), "{written}");
    assert!(written.contains("qwen3:8b"), "{written}");
    assert!(
        written.contains(&format!("api_key: os.environ/{KEY_VAR}")),
        "{written}"
    );
    assert!(!written.contains(KEY_VALUE), "不得写回明文密钥：{written}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&config).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "配置文件必须 0600");
    }

    // 4. 热加载：不重启就能列出新 provider 的模型
    let (status, body) = http(
        &server.addr,
        "GET",
        "/llmproxy/local/v1/models",
        &[("authorization", &format!("Bearer {MASTER_KEY}"))],
        None,
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("qwen3:8b"), "{body}");

    // 5. 改模型
    let (status, body) = server.console(
        "PATCH",
        "/console/api/providers/local/models",
        Some(r#"{"original_id":"qwen3:8b","model":{"id":"qwen3:8b","context_window":65536}}"#),
    );
    assert_eq!(status, 200, "{body}");
    assert!(
        std::fs::read_to_string(&config).unwrap().contains("65536"),
        "改后的字段应落盘"
    );

    // 6. 删模型
    let (status, _) = server.console(
        "DELETE",
        "/console/api/providers/local/models?model=qwen3%3A8b",
        None,
    );
    assert_eq!(status, 200);
    let (status, body) = http(
        &server.addr,
        "GET",
        "/llmproxy/local/v1/models",
        &[("authorization", &format!("Bearer {MASTER_KEY}"))],
        None,
    );
    assert_eq!(status, 200, "{body}");
    assert!(!body.contains("qwen3:8b"), "{body}");

    // 7. 删 provider 后立即 404
    let (status, _) = server.console("DELETE", "/console/api/providers/local", None);
    assert_eq!(status, 200);
    let (status, body) = http(
        &server.addr,
        "GET",
        "/llmproxy/local/v1/models",
        &[("authorization", &format!("Bearer {MASTER_KEY}"))],
        None,
    );
    assert_eq!(status, 404, "{body}");
    assert!(body.contains("provider_not_found"), "{body}");
}

#[test]
fn console_writes_require_token_and_json() {
    let server = Server::start("guards");

    let (status, _) = http(
        &server.addr,
        "POST",
        "/console/api/providers",
        &[("content-type", "application/json")],
        Some(&provider_payload()),
    );
    assert_eq!(status, 401, "缺 token 必须 401");

    let (status, _) = http(
        &server.addr,
        "POST",
        "/console/api/providers",
        &[
            ("x-console-token", server.token.as_str()),
            ("content-type", "text/plain"),
        ],
        Some(&provider_payload()),
    );
    assert_eq!(status, 415, "非 JSON content-type 必须 415");

    // 明文密钥被拒，且文件没被动过
    let before = std::fs::read_to_string(server.config_path()).unwrap();
    let (status, body) = server.console(
        "POST",
        "/console/api/providers",
        Some(r#"{"name":"plain","api":"completions","base_url":"http://127.0.0.1:1/v1","api_key":"sk-plaintext"}"#),
    );
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        std::fs::read_to_string(server.config_path()).unwrap(),
        before
    );
}
