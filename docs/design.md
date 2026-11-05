# anybridge-litellm 设计方案

状态：v15 纯代理版，**对外路径带 `/llmproxy` 前缀与 provider 段**，`api` 类型与上游同步、**不做协议/数据流转换**，**不做模型别名**（`model` 即上游 id，请求体一个字节不改），**一个 provider 一种 `api`**，配置按 provider 分组，工程形态为 Rust workspace 三 crate（`core` ← `server` ← `cli`）加可选控制台/包壳（2026-11-05），**M0–M8 已实现**（142 个测试全绿，含 systemd unit、实际并发压测、内嵌控制台、控制台增删改热加载与 Tailwind/daisyUI 设计系统；npm 包壳仍未建）。代码目录 `tools/anybridge/anybridge-litellm`。
v1–v13 的负载均衡、重试、SQLite、控制台接入主链路、记账、LiteLLM 扁平 `model_list`、**三种协议互转**、模型级 `api` 覆盖、**模型别名**、**只读控制台**全部作废。本文件为准。
证据分档：【文档】LiteLLM 官方文档（本次 WebFetch）；【源码】本机 file:line；【实测】命令输出；【推断】基于代码/文档形态。

---

## 0. 定位与边界

单二进制 Rust 服务：**只做上游 provider 的代理**，**不做模型名替换，更不做协议转换**。客户端按 provider 设置 base URL，用该 provider 上游原生的 API 协议（`completions` / `anthropic` / `responses`）发请求；网关只校验 `model` 在该 provider 的模型列表内，请求与响应体**逐字节透传**。

| 做 | 不做 |
|---|---|
| `{server_url}/llmproxy/{provider}/v1/*` 透传到 `{base_url}/*`、`/health`、`/` | 路由模块、负载均衡、重试、冷却、fallback |
| provider 分组 + 三种上游 API 类型：`completions` / `anthropic` / `responses` | 虚拟 key、rpm/tpm、预算、多租户 |
| 一个 `(provider, model)` → 一个上游模型 id | 账号池、换号、OAuth、Cookie、额度抓取、客户端伪装 |
| master key 鉴权 | JSONL/DB 记账、Prometheus、计费 |
| 单二进制 + systemd | 协议转换：Chat ↔ Messages ↔ Responses 的请求/响应/流事件翻译 |

一句话：**`{server_url}/llmproxy/{provider}/v1` 作为 base URL → 校验 `model` 在模型列表内 → 原样转发、原样返回**。

**「同步上游 API 类型」的含义**：`api` 是**声明**，不是**翻译器**。它只决定三件事——该 provider 允许哪些路径、`/models` 里 `reasoning.param` 叫什么、请求体里 `model` 之外是否需要改别的键（答案是不需要）。客户端用错协议（如向 `anthropic` provider 发 `/chat/completions`）直接 404 `api_mismatch`，网关不代为改写。数据流（含 SSE 帧、`[DONE]`、错误体）一律原样透传，不解析、不重排、不重组。

## 1. 对外路径格式

```
{server_url} = http://127.0.0.1:4000
{prefix}     = /llmproxy

POST {server_url}/llmproxy/{provider}/v1/chat/completions   # api: completions
POST {server_url}/llmproxy/{provider}/v1/messages           # api: anthropic
POST {server_url}/llmproxy/{provider}/v1/responses          # api: responses
GET  {server_url}/llmproxy/{provider}/v1/models
ANY  {server_url}/llmproxy/{provider}/v1/{tail}             # 透传（含 /models/{id} 等）
GET  {server_url}/health
GET  {server_url}/                      # 发现：版本 + provider 列表 + base_path
```

| 路径 | 方法 | 说明 |
|---|---|---|
| `/llmproxy/{provider}/v1/{tail}` | ANY | 透传到 `{base_url}/{tail}`；body 不动 |
| `/llmproxy/{provider}/v1/models` | GET | 网关生成：只列该 provider 的模型，标准 OpenAI 字段 + `extra` |
| `/health` | GET | `{ok, version, providers[]}` |
| `/` | GET | `{name, version, providers:[{name, api, base_path}]}`，`base_path` 形如 `/llmproxy/openai/v1` |

规则：
- 前缀在 v1 **固定 `/llmproxy`**，不做配置项；将来需要反代改写时再评估 `server.prefix`。
- `{provider}` 是配置键，`^[a-z][a-z0-9-]{0,31}$`；**保留字 `v1`、`llmproxy`**，配置用了即启动失败。
- provider 必须在路径里；**不提供无 provider 的 `/llmproxy/v1/*`**。
- 未知 provider → 404 `provider_not_found`，消息列出已配置 provider。
- 未知子路径 → 404 `not_found`。
- 无前缀的旧形态 `/v1/chat/completions` → 404 `provider_required`，消息给出 `{server_url}/llmproxy/{provider}/v1` 示例。
- `/llmproxy/v1/...`（缺 provider）→ 404 `provider_required`。
- 路径大小写敏感，provider 一律小写；**不做重定向**（避免客户端误以为配置已生效）。
- **`{tail}` 只允许该 provider `api` 声明的协议路径**，白名单见下表；其余返回 404 `api_mismatch`（不做改写、不做跨协议兜底）。

| api | 允许的 `{tail}` | 上游路径 |
|---|---|---|
| `completions` | `chat/completions`、`models` | `{base_url}/chat/completions` 等 |
| `anthropic` | `messages`、`messages/count_tokens`、`models` | `{base_url}/messages` 等 |
| `responses` | `responses`、`models` | `{base_url}/responses` 等 |

> `/models` 是唯一**不转发**的路径：由配置生成（见 §5），因为要输出容量与 `extra` 能力。**不提供 `/models/{id}`**——模型详情统一由列表承载，`/models/{id}` 返回 404 `api_mismatch`。

**为什么用 `/llmproxy` 而不是 `/gateway`**：
- `/llmproxy` 语义明确，就是 LLM 代理；`/gateway` 太泛，容易和将来的管理面/其他网关混淆。
- 与 manifold 既有的 `/llmproxy/{provider}/v1` 约定一致，已有的笔记、脚本、客户端配置可平移。
- 反代只需一个 `location /llmproxy/`，`/health` 与 `/` 留在前缀外。

**为什么 provider 在 `/v1` 之前**：OpenAI SDK 的 `base_url` 约定是「到 `/v1` 为止」，SDK 自己追加 `/chat/completions`、`/models`。客户端只需为每个 provider 各存一个 base URL，模型名保持裸名。

## 2. 功能模块（5 个）

| # | 模块 | 内容 | 阶段 |
|---|---|---|---|
| 1 | 接入 | 上述路径；`/llmproxy/{provider}/v1/{tail}` 透传（流式/非流式同一条路径） | M0/M1 |
| 2 | 配置 | provider 分组、模型白名单、provider 级 `api`（一个 provider 一种协议）、`os.environ/`、未知键 warn / strict | M0 |
| 3 | 透传 | 按 `api` 校验路径白名单；校验 `model` 在列表内；请求体与响应体（含 SSE）字节级直通 | M1 |
| 4 | 鉴权 | master key，常量时间比较 | M1 |
| 5 | 运维 | 单二进制、systemd | M5 |

没有路由模块、记账模块、**协议翻译模块**。除 stderr 进程日志外不落任何请求数据。控制台只写 `--config` 指向的 YAML，不产生第二份状态。

## 3. 项目结构

按 OV 通用设计方案（原生内核 + 可选 npm 分发 + 可选 web 控制台）重排结构；保留四件套、mise 任务入口与 `.agents/` 不入库。

### 3.1 产物划分

| 产物 | 语言 | 交付形态 | v1 |
|---|---|---|---|
| 主程序 | Rust | 原生二进制（优先 musl 静态）+ tarball + sha256 | ✅ |
| 包壳 | Node | npm 包，`files` 仅 `bin/lib/scripts`，≤300 行 | ⏸ 可选 |
| 控制台 | TS | Vite + Tailwind v4 + daisyUI 5 构建产物，编译期内嵌主程序 | ✅ |
| 文档 | Markdown | README + `docs/solutions/` | ✅ |

硬约束：一个可执行产物一种语言；Rust 与 TS 拆成两个产物；包壳只做「找二进制 / 修权限 / 透传」，禁止 import 内核符号或承载业务分支；控制台只经 HTTP 与内核通信，不共享进程内类型。

### 3.2 目录骨架

```
anybridge-litellm/
├── mise.toml                       # 工具版本 + 任务入口（唯一）
├── README.md / CHANGELOG.md / LICENSE
├── Cargo.toml / Cargo.lock         # Rust workspace
├── crates/
│   ├── core/                       # 纯逻辑，不依赖 axum
│   │   └── src/{lib,config,domain,auth,passthrough}.rs
│   ├── server/                     # axum 路由/字节流直通/鉴权/静态内嵌
│   └── cli/                        # 二进制入口、参数、信号、systemd
├── apps/
│   └── ui/                         # 可选控制台（Vite+TS+Tailwind+daisyUI），产物 dist/；v1 不建
├── npm/                            # 可选包壳（bin/lib/scripts）；v1 不建
├── scripts/                        # 编排（Python 标准库）：build/embed-check/package/verify
├── tests/
│   ├── contract/                   # 路由/字段/错误码矩阵（表驱动，不用字节 golden）
│   ├── cli/                        # 黑盒：启动、退出码、并发、幂等
│   ├── e2e/                        # 真上游全链路；默认不跑
│   ├── dist/                       # 分发链；有 npm 才建
│   └── fixtures/                   # 小而稳的样本
├── probes/                         # 平台/TLS/上游行为探针 + FINDINGS
├── docs/solutions/                 # 教训库
└── .agents/                        # 本机材料，不入库
```

### 3.3 crate 职责与依赖

| crate | 职责 | 依赖 |
|---|---|---|
| `core` | 配置解析、`ModelEntry`/`ApiKind`、路径白名单、`model` 校验、master key 校验 | 不依赖 axum / HTTP |
| `server` | axum 路由、`/llmproxy/{provider}/v1/*`、字节流直通、静态资源、HTTP 错误信封 | core |
| `cli` | 配置路径/参数、日志、信号、启动 server；唯一二进制 | core + server |

依赖单向：`core ← server ← cli`。新增 API 类型只改 `core`（`ApiKind` + 白名单表），新增路由只改 `server`。

### 3.4 构建链

1. `mise run build` → `scripts/build.py`：先 `apps/ui`（若存在）再 native，顺序显式。
2. `build.rs` 只校验 `apps/ui/dist` 存在且非占位，**禁止**在里面跑 npm。
3. 嵌入口单点：`crates/server/src/static.rs` 的 `#[folder = "../../apps/ui/dist/"]`。
4. 发布构建检测到占位页直接失败；开发态查找：`--ui-dir` > 环境变量 > debug 读磁盘 > 内嵌。
5. `apps/ui/dist` 不入库、不进 npm 包，只存在于二进制内。

### 3.5 控制台接缝

独立工程 `apps/ui`（Vite + TS + Tailwind CSS v4 + daisyUI 5）、独立包管理；服务端只认 `dist/` 一个路径（`crates/server/src/assets.rs` 单点 `#[folder]`）；数据流单向 UI → HTTP → server。

控制台是**配置编辑器**：`GET /console/api/state` 读，`POST|PATCH|DELETE /console/api/providers[/{provider}[/models]]` 写。写接口改的是 `--config` 指向的那一份 YAML，改完**立即热加载**，不需要重启进程。

本地安全要求：只接受回环 `Host`（`127.0.0.1` / `localhost` / `[::1]`）；`Origin` 若存在必须同源；启动生成 32 位随机 token，只经 stderr 打印的 URL 分发，接口校验 `X-Console-Token`（常量时间比较）；写接口额外要求 `Content-Type: application/json`（跨站简单表单只能发 `text/plain` / 表单编码）；禁止 `Access-Control-Allow-Origin: *`；`index.html` 走 `no-store`，带内容哈希的 `assets/` 走 `immutable` 长缓存。

### 3.5.1 写入语义

| 关注点 | 规则 |
|---|---|
| 真源 | 只有配置文件一份。内存里是它的 `Arc` 快照，不产生第二份状态 |
| 读-改-写 | 拿写锁 → **重新从磁盘读**（吸收外部手工编辑）→ 改 YAML 树 → 整份校验 → 原子替换 → 换内存快照 |
| 落盘 | 同目录临时文件（`create_new` + `0600`）→ `fsync` → `rename` → fsync 目录；校验不过**不落盘** |
| 校验 | 完全复用启动加载器 `core::config::load_from_str`，不复制规则；错误以 `details:[{path,message}]` 回传 |
| 热加载 | 每个请求只取一次快照，因此同一请求内配置一致；写成功后新请求立即看到新配置 |
| `--bind` | 命令行覆盖在每次热加载后重新套用，不能被写盘“吃掉” |
| 密钥 | 写接口**只接受 `os.environ/VAR`**（或裸变量名）；`/console/api/state` 只回 `{kind:"env",env:"VAR"}` 描述符，永不含值 |
| 未知键 | 以原映射为底逐字段覆盖，因此将来版本新增的字段原样保留 |
| 已知限制 | `serde_yaml` 整份重写会丢 `#` 注释与空行（键顺序保留）；provider 不支持改名（删 + 建）；**允许删空**（`providers: {}` 合法，控制台展示空态） |

`AppState` 持有 `Arc<ConfigStore>`（`crates/server/src/store.rs`），`snapshot()` 返回 `Arc<Snapshot>` 供读路径使用；写路径只经 `ConfigStore::mutate`，串行化由 `tokio::sync::Mutex` 保证。

### 3.5.2 UI 设计系统

控制台不再维护手写设计令牌，统一到 **Tailwind CSS v4 + daisyUI 5**：

| 关注点 | 规则 |
|---|---|
| 构建 | `@tailwindcss/vite` 插件参与 Vite 构建；产物仍是 `dist/` 里的单个哈希 CSS，`rust-embed` 嵌入方式不变 |
| 运行时 | 零运行时 JS 依赖：Tailwind/daisyUI 只在构建期生效，页面不加载组件库脚本 |
| 扫描范围 | `app.css` 用 `@import "tailwindcss" source(none)` + 显式 `@source "../index.html"` / `@source "./"`，避免把 `dist/`、`node_modules/` 里的产物当候选类名 |
| 主题 | 自定义 `anybridge`（浅色，`default`）与 `anybridge-dark`（`prefersdark`）；OKLCH 令牌，主色/强调色取 `design-system/anybridge-litellm/MASTER.md` 推荐（蓝 `#1E40AF` + 琥珀 `#D97706`），`prefers-color-scheme` 自动切换 |
| 组件词汇 | `card` / `btn` / `badge` / `table` / `modal` / `alert` / `skeleton` / `input` / `select` / `checkbox`；新增界面只复用这套词汇，不写一次性样式 |
| 对话框 | 原生 `<dialog class="modal">` + `modal-box`；表单与**删除确认**共用同一套模态，不再用原生 `confirm()` |
| 焦点 | `input` / `select` 的焦点环统一改为主色（`--input-color: var(--color-primary)`），不跟 daisyUI 默认的 content 近黑色 |
| 可读性 | 模型表 `min-w-[760px]` + 外层 `overflow-x-auto`，窄视口出横向滚动条而不是把 id / 展示名挤成竖排 |

#### 3.5.2.1 视觉契约（硬约束）

以下四条是**验收项**，不满足即视为未完成。**唯一真源是 [`ui-design.md`](ui-design.md)**（入库、可评审）；本节的令牌映射与裁决记录见 `design-system/anybridge-litellm/pages/console.md`。

| # | 规则 | 量化标准 | 反例 |
|---|---|---|---|
| 1 | **字号只用四档** | `text-xs`(12) / `text-sm`(14) / `text-lg`(18) / `text-2xl`(24)；相邻步进 ≥ 1.25 | `text-[10.5px]` / `text-[11px]` / `text-[11.5px]`（三档差 1px，人眼不可辨） |
| 2 | **每页 1 个主操作** | `btn-primary` 计数 = 1；`btn-error` 只出现在确认弹窗内 | 常驻 4 个 `btn-error` 删除按钮，红色主导页面 |
| 3 | **正文对比度 ≥ 4.5:1** | 次要文字用 `.text-muted`(78%) / `.text-subtle`(70%)；**禁止** `text-base-content/45`、`/50` | 实测 3.1:1 / 3.2:1，不过 WCAG AA |
| 4 | **彩色元素一屏 ≤ 2 个** | 语义色只用于状态点 / 徽章，不铺背景 | 每张卡片一个彩色底，强调失效 |

> 契约 1 的依据不是审美：11 / 11.5 / 12 三档的相邻步进只有 1.04–1.09，等于「写了 5 档只得到 3 个可感知层级」，属于无效复杂度。

#### 3.5.2.2 主题令牌

| 令牌 | 浅色 `anybridge` | 深色 `anybridge-dark` | 对比度 |
|---|---|---|---|
| `--color-primary` | `oklch(42.44% 0.1809 265.64)` | `oklch(72.5% 0.135 265.64)` | 白字 8.72:1 / 7.36:1 |
| `--color-accent` | `oklch(66.58% 0.1574 58.32)` | `oklch(87% 0.1 58.32)` | 黑字 6.59:1 / 10.43:1 |
| `--color-warning` | `oklch(78% 0.15 92)` | `oklch(86% 0.13 92)` | 深色 content 9.0:1 / 11.27:1 |

`warning` 由琥珀推到黄色，与 `accent`（58.32°）拉开 33.7°、OKLab ΔE 0.145 / 0.073——否则深色主题下「警告徽章」与「强调按钮」几乎同色。黄色是浅色，`warning-content` 必须用深色（白字只有 2.01:1）。

强调色**只可作填充**（`bg-accent` + `text-accent-content`）：作正文前景叠在 `base-200` 上仅 3.04:1，正文强调一律用 `text-primary`（8.34:1）或 `text-base-content`。

#### 3.5.2.3 门禁

| 层 | 命令 | 通过标准 |
|---|---|---|
| 构建 | `cd apps/ui && npm run build` | `tsc --noEmit` 通过；单 CSS 产物 |
| 全量 | `mise run check` | fmt + clippy `-D warnings` + workspace 测试全绿 |
| 视觉 | 1440 / 420px 视口实机走查 | §3.5.2.1 四条契约逐条核对 |

> 视觉层无自动化门禁，靠走查；要自动化需在 `crates/cli/tests` 补 DOM 断言，不引外部检测器。

### 3.6 分发链（可选，v1 不建）

平台表单一真源 `<platform>-<arch> → { target_triple, binary_name, entries, released }`，launcher/postinstall/CI/tarball 命名都从它派生；launcher 只五件事（查表、存在性校验、修权限、spawn、退出码透传）；postinstall 幂等；版本单一真源；tarball 带 `.sha256`；纯 daemon 优先 musl 静态链接。

### 3.7 测试分层

| 层 | 位置 | 断言 | 默认门禁 |
|---|---|---|---|
| L0 单元 | 各 crate `src` 内 | 纯逻辑、错误映射 | 是 |
| L1 契约 | `tests/contract/` | 路由面完整性、错误码矩阵、字段集合；**不用字节 golden** | 是 |
| L2 黑盒 | `tests/cli/` | spawn 真二进制；临时目录/端口；退出码、并发、幂等 | 是 |
| L3 E2E | `tests/e2e/` | 真上游全链路 | 否，nightly |
| L4 分发 | `tests/dist/` | launcher 三档报错、postinstall 幂等、平台表一致 | 有 npm 才建 |

### 3.8 门禁顺序

`fmt → clippy -D warnings → L0/L1/L2 → (UI build → 产物校验) → native build → package → 一致性校验 → 体积/链接基线`。

发布硬断言：二进制内不得是占位页；tarball 必带 sha256；启动后 `/health` 200。

### 3.9 反模式

在 `build.rs` 里跑前端包管理器；改前端触发内核全量重编；把 `apps/ui` 塞进 Rust workspace 或把 Rust 塞进 JS workspace glob；用字节 golden 当唯一契约门禁；为「统一目录树」对抗语言工具链默认布局；包壳里出现业务分支；大媒体直接入库。

> 与 v10 单 crate 的差异：按通用设计改为 `core/server/cli` 三 crate。理由：`core` 不依赖 HTTP 框架、`server` 可独立测、`cli` 只装配；交付物仍只有一个可执行文件（`cli`）。

## 4. 配置（provider 分组 + API 类型）

```yaml
server:
  bind: "127.0.0.1:4000"

auth:
  master_key: os.environ/GATEWAY_MASTER_KEY

# 凭据文件：密钥值单独存放，配置里只留 `credential/<name>` 引用。
# 缺省 $XDG_CONFIG_HOME/anybridge-litellm/credentials.yaml（0600）。
# credentials:
#   file: /path/to/credentials.yaml

providers:
  openai:                              # 配置键 = 路径里的 provider 段
    api: completions                   # completions | anthropic | responses
    base_url: https://api.openai.com/v1
    api_key: credential/OPENAI_API_KEY
    timeout: 600
    models:
      - id: gpt-4o
        name: GPT-4o
        context_window: 128000
        max_output_tokens: 16384
        input: [text, image]
        reasoning: true
        thinking_levels: [none, low, medium, high]
  openai-responses:                    # 一个 provider 只有一种 api；要换协议就另开 provider
    api: responses
    base_url: https://api.openai.com/v1
    api_key: credential/OPENAI_API_KEY
    timeout: 600
    models:
      - id: gpt-5.6-luna
  anthropic:
    api: anthropic
    base_url: https://api.anthropic.com/v1
    api_key: credential/ANTHROPIC_API_KEY
    timeout: 600
    models:
      - id: claude-sonnet-4-20250514
      - id: claude-haiku-4-20250514
  local:
    api: completions
    base_url: http://127.0.0.1:11434/v1
    api_key: credential/LOCAL_API_KEY
    timeout: 300
    models:
      - id: qwen3:8b
```

规则：
- `api` 取值 `completions` / `anthropic` / `responses`；**provider 级必填，模型级不提供**。一个 provider 只有一种协议，要混协议就拆 provider。它是**协议声明**，决定路径白名单（§1）与 `extra.reasoning.param`，不触发任何翻译。
- `completions` 指 **OpenAI Chat Completions**（`/chat/completions`），不是 legacy `/v1/completions`。
- `base_url` 是**该协议的版本根**，`{tail}` 原样拼在其后：
  - `completions` → `{base}/chat/completions`
  - `anthropic` → `{base}/messages`
  - `responses` → `{base}/responses`
- provider 级字段：`api`、`name`（可选展示名，空则回退 id）、`base_url`、`api_key`、`timeout`、`api_version`（仅 `completions`/`responses`，Azure 用）。
- 模型级字段：`id`（必填，**即上游模型 id，也是客户端传的 `model`**）、`name`（可选展示名）、`context_window`（可选）、`max_output_tokens`（可选）、`input`、`reasoning`、`thinking_levels`（均可选；默认档位属于客户端配置，网关不提供）。`name`/容量/能力只用于 `/models`，不参与转发；`api` 与 `alias` 出现在模型级即启动失败（`unknown_field`）。
- **没有模型别名**：`model` 必须与配置里的 `id` 完全一致，不做大小写归一、不做前缀/后缀推断、不做模糊匹配；不一致就是 `model_not_found`。
- **同一 provider 内 `id` 唯一**；跨 provider 允许同名（路径已限定 provider）。
- provider 名不得用保留字 `v1`、`llmproxy`。
- `api_key` 只接受 `credential/<name>` **引用**；值的归属见下面的「凭据」小节。`master_key` 支持字面量与 `os.environ/VAR`。
- 旧写法（`api_key: os.environ/VAR` / 裸变量名 / 明文）在加载时报错并提示迁移，不做静默兼容。

### 4.1 凭据

密钥与配置彻底分开，参考 dsh 的 credential seam：

| 关注点 | 结论 |
|---|---|
| 引用语法 | `credential/<name>`，`<name>` 匹配 `^[A-Za-z_][A-Za-z0-9_]*$`，≤ 64 字节 |
| 命名约定 | 控制台按 `upper(provider id)` + `_API_KEY` 派生（`-` → `_`），表单里只读展示，不接受手填 |
| 存储 | `credentials.yaml` 的 `refs` 映射（对齐 dsh；旧 `credentials:` 顶层键加载即报错并提示迁移）；默认 `$XDG_CONFIG_HOME/anybridge-litellm/credentials.yaml`，可被 `credentials.file` 覆盖 |
| 对外 key | 同一文件的 `keys` 段：`{name, hash(sha256), scope[], description, created_at, enabled}`；明文只在创建/轮换时返回一次，scope 为 provider id 列表或 `*` |
| 鉴权 | master key 不受 scope 限制；对外 key 越权返回 `403 scope_denied` |
| 权限 | `0600`，写盘走「同目录临时文件 + `fsync` + `rename`」；文件不存在 = 全部未配置 |
| 解析时机 | **每次请求**调用 `CredentialStore::resolve`，轮换后下一次请求即生效，不重启 |
| 插拔点 | `trait CredentialStore { resolve, describe, list, set, unset }`；本期只有 `YamlCredentialStore` |
| 未配置 | 转发返回 `502 credential_not_configured`，不会带空 key 打上游 |
| 只读模式 | 未显式 `--config` 时凭据存储也不可写，写接口 409 `read_only` |
| 回显 | 控制台只回 `{kind:"credential", name, reference, configured, source, writable}`，值永不进入任何响应 |
- 未知键默认 warn 并忽略；`--strict-config` 下启动失败。非法配置启动即失败并报字段路径。
- 不再兼容 LiteLLM 的扁平 `model_list`。

## 5. 契约（写 README，不建 spec）

`crates/core/src/domain.rs` 只放纯类型，无 IO：

| 类型 | 字段 |
|---|---|
| `ApiKind` | `Completions` \| `Anthropic` \| `Responses` |
| `Modality` | `Text` \| `Image` |
| `ProviderConfig` | `name, api, base_url, api_key: Option<CredentialRef>, headers: Vec<(String, HeaderSource)>, api_version, timeout, models[]` |
| `ModelEntry` | `id, name, context_window, max_output_tokens, input: Option<Vec<Modality>>, reasoning: Option<bool>, thinking_levels: Vec<String>`（`api` 只在 provider 级；无 `alias`；默认档位属于客户端配置） |
| `ModelRoute` | `provider, api, model_id, base_url, api_key: Option<&CredentialRef>, headers, api_version, timeout` + `ModelEntry` 的展示/能力字段 |
| `ApiKeyInfo` | `name, scope: Vec<String>, description, created_at, enabled`（对外 API key；摘要只在存储内部） |
| `PassthroughRequest` | `provider, tail, model_id, body: Bytes`（原始字节，原样转发） |
| `UpstreamResponse` | `status, headers, body: Stream<Bytes>`（JSON 与 SSE 同一条通道） |
| `GatewayError` | `kind, status, code, message, upstream_status` |

- 查找键是 `(provider, model)`，`model` 必须与配置的 `id` 完全相等。
- **请求体一个字节不改**：只解析出来读 `model` 做白名单校验，转发时用原始 `Bytes`；不重新序列化，因此键序、空白、数值精度、未知字段全部保持原样。非 JSON body 直接 400 `invalid_request`（三种协议都是 JSON）。
- **响应体不改**：status、`content-type`、其它头、body 字节直通；`text/event-stream` 用 `Body::from_stream` 逐块透传，**不解帧、不补 `[DONE]`、不重组**。
- 转发头策略：丢弃 `host`、`content-length`、`accept-encoding`、`connection`、`transfer-encoding` 与客户端鉴权头；先注入 `authorization`/`x-api-key`（按 `api` 类型：`anthropic` 用 `x-api-key` + `anthropic-version`，其余用 `authorization: Bearer`），再写入 provider 的 `headers`（最后写入，可覆盖同名头）。`api_key` 省略时不注入任何鉴权头。
- 鉴权：先比 `auth.master_key`，再查启用的对外 API key；对外 key 的 `scope` 不含请求的 provider 时返回 `403 scope_denied`。
- `/llmproxy/{provider}/v1/models` 返回标准四字段 + 三项可选扩展；不出现 `x_*` 自定义命名空间：

```json
{
  "object": "list",
  "data": [
    {
      "id": "gpt-4o",
      "object": "model",
      "created": 0,
      "owned_by": "openai",
      "name": "GPT-4o",
      "context_window": 128000,
      "max_output_tokens": 16384,
      "extra": {
        "input_modalities": ["text", "image"],
        "reasoning": {
          "supported": true,
          "param": "reasoning_effort",
          "levels": ["none", "low", "medium", "high"],
          "default": "high"
        }
      }
    }
  ]
}
```

字段规则：
- 必返：`id`（配置里的上游模型 id）、`object: "model"`、`created: 0`、`owned_by`（provider）。
- 顶层选返：`name`、`context_window`、`max_output_tokens`；配置里没写就不出现（不编造）。这三项是 DSH 模型导入实际会读的字段：【源码】`external/deepseek-harness/packages/llm/llm-pi-ai/src/discovery.ts:206-227` 读取 `name`/`display_name`、`context_window`/`context_length`、`max_output_tokens`/`max_tokens` 等别名；类型见 `packages/llm/llm/src/types.ts:341-352`。
- **其余扩展字段统一放 `extra` 对象**，不再平铺到顶层：
  - `extra.input_modalities`: `["text"]` / `["text","image"]`
  - `extra.reasoning`: `{ supported, param, levels: [...], default?: "..." }`；`param` 由该 provider 的 `api` 推导，不单独配置：`completions` → `reasoning_effort`，`anthropic` → `thinking`，`responses` → `reasoning.effort`。
- 顶层字段白名单固定为：`id` / `object` / `created` / `owned_by` / `name` / `context_window` / `max_output_tokens` / `extra`；出现其它顶层键即视为 bug。
- `extra` 里的字段**没有跨厂商标准**：DSH 的 `readListing` 不解析它们（`discovery.ts:181-230`），其配置注释也明确「无法向网关询问 modalities」（`config.ts:70-80`），缺省按 `['text']`。OpenRouter 系客户端可能识别相近命名。
- **不转发上游 `/models` 的未知字段**：列表由配置生成，`extra` 只装网关主动声明的能力，不是上游响应的透传袋。
- 不使用 `x_manifold_*` / `x_anybridge_*` 一类命名空间；扩展统一收在 `extra` 下。

### 5.1 模型字段规范（参考 wpsai provider）

三层字段，不混用：

| 层 | 字段 | 说明 |
|---|---|---|
| 配置 `providers.<p>.models[]` | `id, name, context_window, max_output_tokens, input, reasoning, thinking_levels` | 人写，启动即校验 |
| 内部 `ModelRoute` | 上述全部 + `provider, api, base_url, api_key_ref, api_version, timeout` | 启动时从 provider 继承并展开 |
| `/models` 输出 | `id, object, created, owned_by` + 可选 `name, context_window, max_output_tokens` + 可选 `extra` 对象 | 客户端可见；扩展能力统一收在 `extra` |

与 wpsai provider / DSH 的对应关系：

| 语义 | anybridge 配置 | wpsai `CatalogModel` / `PiModel` | DSH `LlmDiscoveredModel` | v1 |
|---|---|---|---|---|
| 上游模型 id | `id` | `CatalogModel.id` / `PiModel.id` | `id` | ✅ |
| 对外别名 | —（不支持） | `PiModel.id`（网关别名场景） | `id` | ⏸ 客户端直接用上游 id |
| API 类型 | `api`（provider 级，唯一） | `PiModel.api`（`azure/gpt-*` → responses） | —（不在 `/models`） | ✅ 内部；不对外暴露 |
| 展示名 | `name` | `CatalogModel.name?` / `PiModel.name` | `name?` | ✅ |
| 上下文窗口 | `context_window` | `CatalogModel.contextWindow` / `PiModel.contextWindow` | `contextWindow?` | ✅ |
| 输出上限 | `max_output_tokens` | `CatalogModel.maxTokens` / `PiModel.maxTokens` | `maxTokens?` | ✅ |
| 输入模态 | `input` | `CatalogModel.supportsImages` / `PiModel.input` | `inputModalities?`（列表不解析） | ✅ 配置 → `extra.input_modalities` |
| 思考能力 | `reasoning` / `thinking_levels` | `CatalogModel.reasoning` / `thinkingLevelMap` | —（DSH 从配置读；默认档位是客户端自己的配置） | ✅ 配置 → `extra.reasoning`；不重映射档位 |
| 成本 | — | `PiModel.cost` | — | ⏸ 无 USD 价格，不计算 |
| 协议兼容 | — | `PiModel.compat` | — | ⏸ 由 `api` 类型固定 |

校验规则：

- `id`：非空，同一 provider 内唯一，即上游模型 id；不做大小写/前缀归一。
- `alias`：**不支持**，出现即 `unknown_field`。
- `api`：provider 级必填，取值 `completions` / `anthropic` / `responses`；模型级不提供，出现即 `unknown_field`。同一 provider 内所有模型共享一种协议。
- `name`：可选，非空；缺省用 `id`。
- `context_window` / `max_output_tokens`：可选正整数；两者都写且 `max_output_tokens > context_window` 时启动失败。
- `input`：可选数组，取值 `text` / `image`；去重；含 `image` 时必须同时含 `text`。
- `reasoning`：可选布尔；`thinking_levels` 非空时缺省为 true；`reasoning: false` 与 `thinking_levels` 非空同时出现即启动失败。
- `thinking_levels`：可选字符串数组，去重、非空；值即上游 `reasoning_effort` 的可传值，`none` 表示可关闭思考。
- 容量/能力未声明时**不补默认值**，`/models` 不返回对应字段。

与 wpsai 的默认值差异（有意为之）：

| 行为 | wpsai provider | anybridge-litellm |
|---|---|---|
| 未知模型容量 | 兜底 128k / 32k、text-only，并 warn（`core/src/catalog.ts:15-17,74-81`） | 不兜底，不返回；由客户端决定 |
| 展示名 | 缺省用 id | 缺省用 id |
| 输入模态 | `supportsImages` → `input`（`pi/src/translate.ts:57`） | 配置 `input` → `extra.input_modalities` |
| 思考档位 | `thinkingLevelMap`（pi 级 → 上游值） | `thinking_levels` → `extra.reasoning.levels`；不重映射 |
| 成本 | 全 0，额度计费（`pi/src/translate.ts:58-59`） | 不计算、不返回 |

理由：网关不知道上游真实容量，编一个会误导客户端；DSH 也明确倾向 under-claim（`config.ts:70-80`）。

契约表（路径、字段来源、默认值、错误码）写进 `README.md`；行为由各 crate 内单测覆盖，不建 fixture 目录、不做 golden 对拍。

## 6. 透传

不做协议适配层。三种 `api` 只共享一条转发实现：

```rust
struct Route { provider: String, api: ApiKind, base_url: Url, model_id: String, api_key: Secret, timeout: Duration }

impl ApiKind {
    fn allows(&self, tail: &str) -> bool;          // §1 白名单
    fn auth_headers(&self, key: &Secret) -> HeaderMap;
    fn reasoning_param(&self) -> &'static str;     // 仅供 /models 的 extra.reasoning
}
```

| api | 路径 | 上游 | 特殊处理 |
|---|---|---|---|
| `completions` | `{base}/chat/completions` | 原样 | 无 |
| `anthropic` | `{base}/messages` | 原样 | `anthropic-version` 取 `api_version`，缺省 `2023-06-01` |
| `responses` | `{base}/responses` | 原样 | 无 |

实现要点：
- 请求体只解析出 `model` 做白名单校验，**转发用原始 `Bytes`**；不重新序列化，键序/空白/未知字段/数值精度全部保持。
- 若 body 是流式上传（非 JSON），直接 400；v1 不做文件/音频 multipart 透传。
- 响应：`reqwest::Response::bytes_stream()` 直接塞进 `Body::from_stream`；`content-type` 与 `content-length`（若有）透传。**不解析 SSE**，因此不需要 `[DONE]` 补帧、错误帧、usage 归零等逻辑。
- 上游非 2xx：**status + 头 + body 原样返回**，只在 stderr 记一行 `{provider, model, status}`；网关**不改写**上游错误体。
- 客户端断开：响应 body 被 drop → 上游流随之 drop，天然取消；另用 `route.timeout` 包首字节与整体空闲超时。

## 7. 请求路径

1. 解析路径：`/llmproxy/{provider}/v1/...` → provider；不存在 → `provider_not_found`。
2. 鉴权：`Authorization: Bearer` / `x-api-key`，常量时间比较 master key。
3. 校验路径：`{tail}` 是否在该 provider 的 `api` 白名单内；不在 → 404 `api_mismatch`。
4. 解析 body：JSON 解析取 `model`；缺 `model` → 400 `invalid_request`。
5. 查表：`model` 与该 provider 的某个 `id` 完全相等 → `Route`；否则 `model_not_found`。
6. 发送：`route.timeout` + 上游鉴权头；**不重试**；请求体与头原样。
7. 响应：status + 头 + body 原样回传（JSON 与 SSE 同一路径，逐块流式）。
8. 结束：不记账、不落盘、不改写错误体；仅 stderr 记 `{provider, model, status, ms}`。

响应头：透传上游头；追加 `x-anybridge-provider`、`x-anybridge-model`、`x-anybridge-api`（不泄漏 key）。若与上游头同名，以上游为准。

## 8. 错误映射

只对**网关自己产生**的错误用信封；**上游响应（无论状态码）原样透传**，不解析、不归一。

| 场景 | type | status |
|---|---|---|
| 缺/错 master key | `auth_error` | 401 |
| 未知 provider | `provider_not_found` | 404 |
| 缺前缀/缺 provider 的 `/v1/*`、`/llmproxy/v1/*` | `provider_required` | 404 |
| `{tail}` 不在该 provider 的 `api` 白名单内 | `api_mismatch` | 404 |
| 该 provider 下无此 model | `model_not_found` | 404 |
| body 非 JSON / 缺 `model` / 未知子路径 | `invalid_request` | 400 |
| 上游不可达 / 连接超时 / 首字节超时 | `upstream_error` | 502 |
| 上游中途断开（已开始回传） | 不发信封，直接结束流 | — |

错误信封 `{error:{message,type,code}}`；`type` 为上述七种之一（上游 429/400/5xx 走透传，不在此表）。

## 9. 里程碑

| 阶段 | 产出 | 验收 |
|---|---|---|
| M0 | 四件套 + `mise.toml` + `crates/core/src/{config,domain}.rs` + `crates/server` 路由骨架 + `/health` + `/` + `/llmproxy/{provider}/v1/models` | `mise run check` 通过；示例配置能解析；保留字 provider（`v1`/`llmproxy`）启动失败；同 provider 内 `id` 重复失败；模型级 `alias`/`api` 报 `unknown_field`；未知 provider 404；`/v1/*` 与 `/llmproxy/v1/*` 都报 `provider_required`；`/models` 顶层只出现白名单键，扩展能力只出现在 `extra`，断言无其它顶层键与 `x_*` |
| M1 | `completions` 透传（非流式）+ `model` 校验 + master key | 假上游逐字节对拍（status/头/body 完全一致）；请求体字节与客户端发出的一致；`model` 不在列表 → 404；白名单外路径 404 `api_mismatch`；401/404/400 正确 |
| M2 | 流式透传 + 超时/取消 | 假上游分块响应逐块到达（不缓冲整段）；断开即取消上游；上游中途断开发完即止 |
| M3 | `anthropic` 协议：路径白名单 + `x-api-key`/`anthropic-version` | `messages` 与 `models` 路径通、`chat/completions` 404；头正确；`extra.reasoning.param` 为 `thinking` |
| M4 | `responses` 协议：路径白名单 | `responses` 路径通、其它 404；`extra.reasoning.param` 为 `reasoning.effort` |
| M5 | `mise run package` + `deploy/anybridge-litellm.service` + 压测 | 单二进制无运行时依赖；100 并发流（400 MiB payload）RSS 增长 < 128 MiB；客户端断开后上游 body 5 秒内被 drop；`systemctl --user` 加载 unit 得 `LoadState=loaded` |
| M6 | `apps/ui` 控制台 + `rust-embed` 内嵌 + `/console/*` | 页面与静态资源可达；无 token 401、非回环 Host 403、跨源 Origin 403；响应不含上游 key；release 构建拒绝占位页 |
| M7 | 控制台增删改 Provider / 模型（`core::edit` + `server::store` + 写接口 + 表单） | 增删改后**不重启**即可路由；文件落盘 `0600` 且只含 `os.environ/VAR`；校验失败不落盘；明文 `api_key` 被拒；401/403/415/404/409 矩阵；`ReadWritePaths` 进 systemd unit |
| M8 | UI/UX 统一：Tailwind CSS v4 + daisyUI 5 自定义双主题、组件词汇、主题化确认框、§3.5.2.1 四条视觉契约 | 深浅主题、1440/420px 视口实机走查通过；控制台无本应用报错；构建产物仍为单 CSS；字号/主操作/对比度/彩色元素四条契约全过；`mise run check` 全绿 |

`mise.toml` 任务：`build` / `test` / `check`（test + fmt + clippy）/ `package`；Rust 无 `install`。跨语言入口只用 mise（`AGENTS.md:204`）。

## 10. 硬约束

- **Rust workspace 三 crate**：`crates/core`（无 HTTP 依赖）、`crates/server`（axum/流式直通/静态）、`crates/cli`（唯一二进制）；依赖单向 `core ← server ← cli`；不建 `core/spec/`。
- 一个可执行产物一种语言；控制台与包壳（若启用）是独立产物，分别放 `apps/ui` 与 `npm/`，不塞进 Rust workspace。
- 契约只写 README；不建 `spec/`、不维护字节 golden（`AGENTS.md:51-52`）。
- 四件套齐备 + mise 任务名（`AGENTS.md:110-113`）。
- **纯代理 + 零转换**：无路由/负载均衡/重试/冷却/fallback；无请求落盘；**无协议翻译**（Chat ↔ Messages ↔ Responses 的请求/响应/流事件一律不做）。控制台是唯一的写接口，只改本机配置文件。
- **`api` 是声明不是翻译器**：只决定路径白名单、上游鉴权头、`extra.reasoning.param`；客户端协议不匹配返回 404 `api_mismatch`，不代为改写。
- **请求体与响应体都一个字节不改**；SSE 不做帧解析、不补 `[DONE]`、不重组。
- **不支持模型别名**：`model` 必须等于配置里的 `id`，无归一、无推断、无模糊匹配。
- **前缀固定 `/llmproxy`，provider 必须在路径里**；无 provider 一律 404，不做重定向。
- 配置以 provider 分组为唯一形态；`api` 只允许 `completions` / `anthropic` / `responses`，**provider 级必填、模型级不提供**（出现即 `unknown_field`）；混协议拆 provider。
- **`/models` 顶层字段白名单**：`id` / `object` / `created` / `owned_by`，可选 `name` / `context_window` / `max_output_tokens` / `extra`；`extra` 内只允许 `input_modalities` 与 `reasoning`，不得出现其它顶层键或 `x_*` 命名空间。
- 只接标准 API：API key / cloud 凭据 / `openai-compatible` base URL；**不实现**账号池、OAuth、Cookie、额度抓取、客户端伪装。
- 上游 key 只来自配置或环境变量；不回显、不进日志。
- master key 常量时间比较。
- 不修改 manifold、anybridge-im 任何代码；本包自成仓。
- 依赖最小：`tokio`、`axum`、`reqwest`（rustls）、`serde`/`serde_json`、`serde_yaml`、`thiserror`、`tracing`、`subtle`、`rust-embed` + `mime_guess`（仅控制台内嵌）。不引 ORM、metrics、redis、rand（token 取 `/dev/urandom`）。

## 11. 待确认

| # | 问题 | 阻塞 | 验证方式 |
|---|---|---|---|
| 1 | 前缀是否需要可配置（反代已有 `/llmproxy` 时） | M0 | v1 固定；真实反代需求出现再加 `server.prefix` |
| 2 | 是否额外提供聚合的 `GET /llmproxy/v1/models`（跨 provider） | M0 | v1 不做；路径带 provider 后聚合会引入歧义 |
| 3 | 上游错误体原样透传后客户端能否正确识别（各家形状不同） | M1 | 假上游造 4 种错误形状对拍；必要时加 `--normalize-errors` 开关，v1 默认关 |
| 4 | Azure 的 URL 形态（deployment 路径 + `api-version`） | M1 | 有真实 endpoint 才验；否则 v1 只保证标准 OpenAI |
| 5 | 是否需要最少一行 stderr 请求日志 | M0 | 若需要只 stderr 一行，不落文件 |
| 6 | `/models` 的 `created` 固定 0 是否够（部分客户端会显示 1970） | M0 | 先固定 0；若要真实时间，改为配置加载时刻，不读上游 |
| 7 | `extra` 的字段名与版本策略（内部键是否需要 `version`） | M0 | 先固定 `input_modalities` / `reasoning` 两个键；加字段时按增量扩，不预告 |

已定（不再待确认）：**一个 provider 只有一种 `api`**，模型级不提供 `api`；要混协议就拆 provider（配置示例里 `openai` 与 `openai-responses` 各占一个）。**不支持模型别名**，`model` 即上游 `id`，配置里出现 `alias` 即 `unknown_field`。

## 12. 与 LiteLLM / manifold / 历史版本

- **LiteLLM**：只借字段语义（`model`、`api_key`、`api_base`、`timeout`），配置形态与路径都不同；不再兼容其扁平 `model_list` 与无 provider 的 `/v1/*`。
- **manifold**：只读参考。复用 `/llmproxy/{provider}/v1` 路径约定、错误信封、master key 思路；**不复用**其 `x_manifold_*` 私有命名空间，也不复用其「上游 → Chat Completions」的翻译层。`name` / `context_window` / `max_output_tokens` 保留，因为 DSH 模型导入确实读取（`discovery.ts:206-227`），不属于 manifold 私有字段。丢弃 provider 实现、号池、设备登录、Cookie、额度、指纹剥离、日志/用量/控制台。
- **v1–v9**：负载均衡、重试、SQLite、JSONL 记账、控制台、LiteLLM 配置兼容、**三种协议互转**全部推迟；需要时按增量加，不预搭骨架（`AGENTS.md:43`）。
