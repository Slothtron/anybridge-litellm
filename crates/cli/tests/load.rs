//! 压测与取消：100 并发流 + 客户端断开后上游连接被取消。
//!
//! 两件事都必须对**真二进制**做：RSS 只有跨进程才量得准，取消只有真 socket
//! 关闭才触发得了 hyper 的 body drop。
//!
//! - `hundred_concurrent_streams_keep_rss_flat` 只在 Linux 上跑（读 `/proc/<pid>/status`）。
//! - 取消用例跨平台：假上游在响应体流里挂一个 `Drop` 标记，网关丢掉上游 body
//!   时标记翻转。

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use bytes::Bytes;
use futures_util::stream;
use futures_util::StreamExt;

const MASTER_KEY: &str = "sk-load-test";
const CHUNK_SIZE: usize = 16 * 1024;
const CHUNKS_PER_REQUEST: usize = 256;
const CONCURRENCY: usize = 100;
/// 100 并发 × 4 MiB = 400 MiB。缓冲整段流至少要多占这么多内存。
const TOTAL_PAYLOAD: usize = CONCURRENCY * CHUNKS_PER_REQUEST * CHUNK_SIZE;
/// 400 MiB payload 下的 RSS 增长上限：给每连接固定缓冲留 3 倍余量。
const RSS_LIMIT_MIB: f64 = 128.0;

#[derive(Clone)]
struct UpstreamState {
    /// 响应体流被 drop 时翻转：等价于「上游连接被取消」。
    cancelled: Arc<AtomicBool>,
}

struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

async fn upstream_chat(State(state): State<UpstreamState>, body: Bytes) -> Response {
    let model = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("model")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    if model == "cancel" {
        return never_ending(state.cancelled.clone());
    }
    echo_chunks().await
}

async fn echo_chunks() -> Response {
    let chunk = Bytes::from(vec![b'x'; CHUNK_SIZE]);
    let chunks = (0..CHUNKS_PER_REQUEST).map(move |_| Ok::<Bytes, std::io::Error>(chunk.clone()));
    // 每块之间让出一次，保证真的是「边收边发」而不是一次写满。
    let stream = stream::iter(chunks).then(|item| async move {
        tokio::time::sleep(Duration::from_millis(1)).await;
        item
    });
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}

fn never_ending(cancelled: Arc<AtomicBool>) -> Response {
    let flag = DropFlag(cancelled);
    let stream = stream::unfold((flag, 0u8), |(flag, i)| async move {
        if i == 0 {
            Some((
                Ok::<Bytes, std::io::Error>(Bytes::from_static(b"data: {}\n\n")),
                (flag, 1u8),
            ))
        } else {
            // 挂住不返回：只有 body 被 drop 才会结束。
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Some((Ok(Bytes::new()), (flag, 2u8)))
        }
    });
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

struct Gateway {
    child: Child,
    base: String,
    cancelled: Arc<AtomicBool>,
}

impl Gateway {
    async fn start() -> Self {
        let cancelled = Arc::new(AtomicBool::new(false));
        let app = Router::new()
            .route("/v1/chat/completions", post(upstream_chat))
            .with_state(UpstreamState {
                cancelled: cancelled.clone(),
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let port = free_port();
        let dir = std::env::temp_dir().join(format!("ablt-load-{port}"));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("config.yaml");
        let credentials = dir.join("credentials.yaml");
        std::fs::write(&credentials, "credentials:\n  upstream: sk-load-test\n").unwrap();
        let credentials_display = credentials.display().to_string();
        std::fs::write(
            &config,
            format!(
                r#"
server:
  bind: "127.0.0.1:{port}"
auth:
  master_key: {MASTER_KEY}
credentials:
  file: "{credentials_display}"
providers:
  fake:
    api: completions
    base_url: http://{upstream}/v1
    api_key: credential/upstream
    timeout: 60
    models:
      - id: stream
      - id: cancel
"#
            ),
        )
        .unwrap();

        let child = Command::new(env!("CARGO_BIN_EXE_anybridge-litellm"))
            .arg("--config")
            .arg(&config)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("启动 anybridge-litellm 失败");

        let base = format!("http://127.0.0.1:{port}");
        let gateway = Self {
            child,
            base: base.clone(),
            cancelled,
        };
        let client = reqwest::Client::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(response) = client.get(format!("{base}/health")).send().await {
                if response.status().is_success() {
                    return gateway;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // 显式收尸（Drop 里 kill + wait）后再报错，避免留下僵尸进程。
        drop(gateway);
        panic!("网关 10 秒内没起来");
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(target_os = "linux")]
fn rss_kb(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status.lines().find_map(|line| {
        line.strip_prefix("VmRSS:")
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kb| kb.parse().ok())
    })
}

async fn one_stream(client: &reqwest::Client, url: &str) -> usize {
    let response = client
        .post(url)
        .bearer_auth(MASTER_KEY)
        .json(&serde_json::json!({ "model": "stream", "stream": true }))
        .send()
        .await
        .expect("请求失败");
    assert_eq!(response.status(), 200);
    let mut body = response.bytes_stream();
    let mut total = 0;
    while let Some(chunk) = body.next().await {
        total += chunk.expect("读流失败").len();
    }
    total
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hundred_concurrent_streams_keep_rss_flat() {
    let gateway = Gateway::start().await;
    let client = reqwest::Client::new();
    let url = format!("{}/llmproxy/fake/v1/chat/completions", gateway.base);

    // 预热：建好连接池、跑过一次分配，基线才稳定。
    assert_eq!(one_stream(&client, &url).await, TOTAL_PAYLOAD / CONCURRENCY);
    let baseline = rss_kb(gateway.pid()).expect("读不到网关 RSS");

    // 采样任务：每 5ms 记一次峰值。
    let pid = gateway.pid();
    let stop = Arc::new(AtomicBool::new(false));
    let peak = Arc::new(std::sync::atomic::AtomicU64::new(baseline));
    let sampler = {
        let stop = stop.clone();
        let peak = peak.clone();
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                if let Some(rss) = rss_kb(pid) {
                    peak.fetch_max(rss, Ordering::Relaxed);
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    };

    let mut tasks = Vec::with_capacity(CONCURRENCY);
    for _ in 0..CONCURRENCY {
        let client = client.clone();
        let url = url.clone();
        tasks.push(tokio::spawn(async move { one_stream(&client, &url).await }));
    }
    for task in tasks {
        assert_eq!(task.await.expect("任务 panic"), TOTAL_PAYLOAD / CONCURRENCY);
    }

    stop.store(true, Ordering::Relaxed);
    sampler.await.unwrap();
    let peak = peak.load(Ordering::Relaxed);

    let delta_mib = peak.saturating_sub(baseline) as f64 / 1024.0;
    let payload_mib = TOTAL_PAYLOAD as f64 / (1024.0 * 1024.0);
    eprintln!(
        "baseline={baseline} KiB peak={peak} KiB delta={delta_mib:.1} MiB（100 并发共 {payload_mib:.0} MiB）"
    );

    // 缓冲式实现会把 400 MiB 全吞进内存；流式透传只多出每连接的固定缓冲。
    assert!(
        delta_mib < RSS_LIMIT_MIB,
        "RSS 增长 {delta_mib:.1} MiB 过大（payload {payload_mib:.0} MiB），疑似缓冲整段流"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_disconnect_cancels_upstream() {
    let gateway = Gateway::start().await;
    let client = reqwest::Client::new();
    let url = format!("{}/llmproxy/fake/v1/chat/completions", gateway.base);

    let mut response = client
        .post(&url)
        .bearer_auth(MASTER_KEY)
        .json(&serde_json::json!({ "model": "cancel" }))
        .send()
        .await
        .expect("请求失败");
    assert_eq!(response.status(), 200);

    // 读到首块，确认流已经建立、上游 body 正在被轮询。
    let first = response
        .chunk()
        .await
        .expect("读首块失败")
        .expect("流应立即有数据");
    assert_eq!(first.as_ref(), b"data: {}\n\n");
    assert!(
        !gateway.cancelled.load(Ordering::SeqCst),
        "还没断开就不该取消"
    );

    // 半途丢响应 = 客户端断开。
    drop(response);

    // 网关必须随之丢掉上游 body：假上游的 Drop 标记在几秒内翻转。
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if gateway.cancelled.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("客户端断开 5 秒内上游连接仍未取消");
}
