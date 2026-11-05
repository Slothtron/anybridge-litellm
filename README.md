# anybridge-litellm

单二进制 Rust 服务，**只做上游 provider 的代理**：`/llmproxy/{provider}/v1/*` 原样转发到对应上游，**不做协议转换、不做模型别名替换**。客户端用该 provider 上游原生的 API 协议（`completions` / `anthropic` / `responses`）发请求，网关只校验 `model` 在该 provider 的模型列表内，请求与响应体逐字节透传。

一句话：`{server_url}/llmproxy/{provider}/v1` 作为 base URL → 校验 `model` 在模型列表内 → 原样转发、原样返回。

## 不做什么

以下能力**明确不做**，请求出现时不会被网关兜底或改写：

| 不做 | 说明 |
|---|---|
| 路由 / 负载均衡 / 重试 / 冷却 / fallback | 一个 `(provider, model)` 对应一个上游地址，失败即失败，不重试 |
| 虚拟 key / rpm-tpm 限流 / 预算 / 多租户 | 只有单一 master key |
| 账号池 / 换号 / OAuth / Cookie / 额度抓取 / 客户端伪装 | 上游凭据只来自配置或环境变量 |
| 控制台 / JSONL 与数据库记账 / Prometheus / 计费 | 除 stderr 进程日志外不落任何请求数据 |
| 协议转换 | Chat ↔ Messages ↔ Responses 的请求 / 响应 / 流事件翻译一律不做；SSE 不解帧、不补 `[DONE]`、不重组 |

`api` 是**声明**而不是翻译器：它只决定该 provider 允许哪些路径、`/models` 里 `reasoning.param` 叫什么。客户端用错协议（例如向 `anthropic` provider 发 `/chat/completions`）直接返回 404 `api_mismatch`，网关不代为改写。

## 快速开始

```bash
# 构建（产物在 target/release/）
mise run build

# 启动
./target/release/anybridge-litellm --config config.example.yaml
```

默认只绑定 `127.0.0.1:4000`。

```bash
# 健康检查
curl http://127.0.0.1:4000/health

# 列出某个 provider 的模型
curl http://127.0.0.1:4000/llmproxy/openai/v1/models \
  -H "Authorization: Bearer sk-xxxx"

# 透传一次 chat completions（body 原样转给上游）
curl -X POST http://127.0.0.1:4000/llmproxy/openai/v1/chat/completions \
  -H "Authorization: Bearer sk-xxxx" \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}'
```

## 配置参考

配置以 **provider 分组**为唯一形态，不再兼容 LiteLLM 的扁平 `model_list`。

```yaml
server:
  bind: "127.0.0.1:4000"

auth:
  master_key: os.environ/GATEWAY_MASTER_KEY

# 凭据文件：密钥值单独存放，配置文件里只留 `credential/<name>` 引用。
# 缺省路径 ~/.config/anybridge-litellm/credentials.yaml（0600）。
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
        default_thinking_level: high

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

### 顶层字段

| 字段 | 必填 | 说明 |
|---|---|---|
| `server.bind` | 是 | 监听地址，形如 `127.0.0.1:4000` |
| `auth.master_key` | 是 | 客户端鉴权用的 master key，支持字面量与 `os.environ/VAR` |
| `credentials.file` | 否 | 凭据文件路径；缺省 `$XDG_CONFIG_HOME/anybridge-litellm/credentials.yaml`（退回 `~/.config/...`） |

### provider 级字段

| 字段 | 必填 | 说明 |
|---|---|---|
| `api` | 是 | 协议声明，取值仅 `completions` / `anthropic` / `responses` |
| `name` | 否 | provider 展示名；为空时界面回退显示 provider id |
| `base_url` | 是 | 该协议的版本根，`{tail}` 原样拼在其后 |
| `api_key` | 是 | 上游凭据**引用**；控制台按 `upper(provider id)` + `_API_KEY` 派生（`openai-responses` → `credential/OPENAI_RESPONSES_API_KEY`），值写在凭据文件里 |
| `api_version` | 否 | 仅 `completions` / `responses` 使用（Azure 场景）；`anthropic` 的版本头缺省 `2023-06-01` |
| `timeout` | 否 | 上游超时（秒），同时用作首字节与整体空闲超时 |
| `models` | 是 | 模型列表，见下 |

`api` 的三种取值含义：

| 取值 | 含义 | 上游路径 |
|---|---|---|
| `completions` | OpenAI Chat Completions（`/chat/completions`），不是 legacy `/v1/completions` | `{base_url}/chat/completions` |
| `anthropic` | Anthropic Messages | `{base_url}/messages` |
| `responses` | OpenAI Responses | `{base_url}/responses` |

**一个 provider 只有一种协议**，要混协议就拆成多个 provider。`api` 在模型级不提供。

### 模型级字段

| 字段 | 必填 | 说明 |
|---|---|---|
| `id` | 是 | 上游模型 id，也是客户端传的 `model`；同一 provider 内唯一 |
| `name` | 否 | 展示名，仅用于 `/models`；配置里没写就不出现（不编造） |
| `context_window` | 否 | 上下文窗口（正整数），仅用于 `/models` |
| `max_output_tokens` | 否 | 输出上限（正整数），仅用于 `/models` |
| `input` | 否 | 输入模态数组，取值 `text` / `image` |
| `reasoning` | 否 | 是否支持思考；`thinking_levels` 非空时缺省为 `true` |
| `thinking_levels` | 否 | 思考档位字符串数组，值即上游可传的档位值 |
| `default_thinking_level` | 否 | 默认思考档位，必须在 `thinking_levels` 内 |

模型级不参与转发的字段（`name` / 容量 / 能力）只影响 `/models` 输出；请求体里的 `model` 必须与 `id` 完全一致，**无大小写归一、无前后缀推断、无模糊匹配**。

### 校验规则

- `api`：provider 级必填，模型级不提供——**模型级出现 `api` 或 `alias` 即启动失败**（`unknown_field`）。
- `id`：非空，同一 provider 内唯一；跨 provider 允许同名（路径已限定 provider）。
- `name`：可选，非空。
- `context_window` / `max_output_tokens`：可选正整数；两者都写且 `max_output_tokens > context_window` 时启动失败。
- `input`：可选数组，去重；含 `image` 时必须同时含 `text`。
- `reasoning`：可选布尔；`reasoning: false` 与 `thinking_levels` 非空同时出现即启动失败。
- `thinking_levels`：可选字符串数组，去重、非空。
- `default_thinking_level`：可选，必须在 `thinking_levels` 内。
- 容量 / 能力未声明时**不补默认值**，`/models` 不返回对应字段。

### provider 命名

- 配置键即路径里的 `{provider}` 段，必须匹配 `^[a-z][a-z0-9-]{0,31}$`。
- 保留字：`v1`、`llmproxy`，配置用了即启动失败。
- 路径大小写敏感，provider 一律小写。

### 其它规则

- `api_key` 是 `credential/<name>` 引用，值由凭据存储按请求解析；`master_key` 支持字面量与 `os.environ/VAR`。
- 旧写法（`api_key: os.environ/VAR` 或裸变量名）会在加载时报错并提示迁移到 `credential/<name>`。
- 未知键默认 warn 并忽略；`--strict-config` 下启动失败。
- 非法配置启动即失败，并报出字段路径。

## API 参考

### 路径

| 路径 | 方法 | 说明 |
|---|---|---|
| `/llmproxy/{provider}/v1/{tail}` | ANY | 透传到 `{base_url}/{tail}`；body 不动 |
| `/llmproxy/{provider}/v1/models` | GET | 网关生成：只列该 provider 的模型，标准 OpenAI 字段 + `extra` |
| `/console/` | GET | 本地 Web 控制台（回环 + token） |
| `/console/api/state` | GET | 控制台状态接口，需 `X-Console-Token` |
| `/console/api/providers` | POST | 新建 provider |
| `/console/api/providers/{provider}` | PATCH / DELETE | 更新 / 删除 provider |
| `/console/api/providers/{provider}/models` | POST / PATCH / DELETE | 新建 / 更新（可改 id）/ 删除模型，删除时用 `?model=<id>` |
| `/health` | GET | `{ok, version, providers[]}` |
| `/` | GET | `{name, version, providers:[{name, api, base_path}]}`，`base_path` 形如 `/llmproxy/openai/v1` |

`{tail}` 受该 provider `api` 的白名单约束，其余路径返回 404 `api_mismatch`：

| api | 允许的 `{tail}` | 上游路径 |
|---|---|---|
| `completions` | `chat/completions`、`models` | `{base_url}/chat/completions` 等 |
| `anthropic` | `messages`、`messages/count_tokens`、`models` | `{base_url}/messages` 等 |
| `responses` | `responses`、`models` | `{base_url}/responses` 等 |

`/models` 是唯一**不转发**的路径，由配置生成。**不提供 `/models/{id}`**：模型详情统一由 `/models` 列表承载，`/models/{id}` 返回 404 `api_mismatch`。

路径规则补充：

- 前缀固定 `/llmproxy`，不是配置项。
- provider 必须在路径里，**不提供无 provider 的 `/llmproxy/v1/*`**。
- 未知 provider → 404 `provider_not_found`（消息列出已配置 provider）。
- 未知子路径 → 400 `invalid_request`。
- 无前缀的旧形态 `/v1/chat/completions`、以及缺 provider 的 `/llmproxy/v1/...` → 404 `provider_required`。
- 不做重定向。

### `/models` 响应

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

- **必返**：`id`（配置里的上游模型 id）、`object: "model"`、`created: 0`、`owned_by`（provider 名）。
- **选返**：`name`、`context_window`、`max_output_tokens`；配置里没写就不出现，不编造。
- **顶层字段白名单固定为**：`id` / `object` / `created` / `owned_by` / `name` / `context_window` / `max_output_tokens` / `extra`；出现其它顶层键即视为 bug。
- 扩展能力统一收在 `extra` 下，**只允许 `input_modalities` 与 `reasoning` 两个键**，不使用 `x_*` 一类自定义命名空间。
- `extra.input_modalities`：`["text"]` 或 `["text","image"]`。
- `extra.reasoning`：`{ supported, param, levels, default? }`；`param` 由该 provider 的 `api` 推导，不单独配置：

| api | `extra.reasoning.param` |
|---|---|
| `completions` | `reasoning_effort` |
| `anthropic` | `thinking` |
| `responses` | `reasoning.effort` |

`extra` 里的字段没有跨厂商标准，客户端可能忽略；网关只声明自己配置里有的能力。

### 错误

网关**自己产生**的错误使用统一信封，`type` 为下表取值：

```json
{ "error": { "message": "...", "type": "...", "code": "..." } }
```

| 场景 | type | status |
|---|---|---|
| 缺 / 错 master key | `auth_error` | 401 |
| 未知 provider | `provider_not_found` | 404 |
| 缺前缀 / 缺 provider 的 `/v1/*`、`/llmproxy/v1/*` | `provider_required` | 404 |
| `{tail}` 不在该 provider 的 `api` 白名单内 | `api_mismatch` | 404 |
| 该 provider 下无此 model | `model_not_found` | 404 |
| body 非 JSON / 缺 `model` / 未知子路径 | `invalid_request` | 400 |
| 上游不可达 / 连接超时 / 首字节超时 | `upstream_error` | 502 |
| 上游中途断开（已开始回传） | 不发信封，直接结束流 | — |

**上游响应（含 429 / 400 / 5xx）原样透传，不归一、不改写**，只在 stderr 记一行 `{provider, model, status}`。

### 请求与响应头

- 转发请求头时**丢弃**：`host`、`content-length`、`accept-encoding`、`connection`、`transfer-encoding`，以及客户端自己的 `authorization` / `x-api-key`（由网关重写）；其余原样带上。
- 追加上游鉴权头：
  - `anthropic`：`x-api-key` + `anthropic-version`（取 `api_version`，缺省 `2023-06-01`）。
  - 其余：`authorization: Bearer <api_key>`。
- 响应透传上游头，并**追加** `x-anybridge-provider`、`x-anybridge-model`、`x-anybridge-api`；与上游头同名时**以上游为准**，不泄漏 key。

### 透传语义

- 请求体只解析出 `model` 做白名单校验，**转发用原始字节**；不重新序列化，键序 / 空白 / 未知字段 / 数值精度全部保持原样。
- 响应 status、`content-type`、其它头、body 字节直通；`text/event-stream` 逐块流式透传，不缓冲整段、不解帧、不补 `[DONE]`。
- 非 JSON body 直接 400 `invalid_request`（三种协议都是 JSON）；v1 不做文件 / 音频 multipart 透传。
- 不重试；客户端断开时响应 body 被 drop，上游流随之 drop，天然取消。

## 开发

### 任务入口

跨语言入口只用 mise：

```bash
mise run build    # scripts/build.py：先 apps/ui 再 native
mise run build-ui # 只构建控制台
mise run test     # 测试
mise run check    # test + fmt + clippy -D warnings
mise run package  # 打 tarball + sha256
```

Rust 侧没有 `install` 任务。

### workspace 结构

三个 crate，依赖单向 `core ← server ← cli`：

| crate | 职责 | 依赖 |
|---|---|---|
| `crates/core` | 配置解析、`ApiKind` / `ModelEntry`、路径白名单、`model` 校验、master key 校验；不依赖 axum / HTTP | — |
| `crates/server` | axum 路由、`/llmproxy/{provider}/v1/*`、字节流直通、静态资源、HTTP 错误信封 | core |
| `crates/cli` | 配置路径 / 参数、日志、信号、启动 server；**唯一二进制** | core + server |

新增 API 类型只改 `core`（`ApiKind` + 白名单表），新增路由只改 `server`。交付物只有一个可执行文件（`cli`）。

可选的控制台（`apps/ui`）与 npm 包壳（`npm/`）是独立产物，不塞进 Rust workspace；v1 不建。

### 测试分层

| 层 | 位置 | 覆盖 |
|---|---|---|
| L0 单元 | 各 crate `src` 内 | 配置校验、可写文档增删改、白名单、`model` 提取、常量时间比较、头过滤、URL 拼接、错误映射、参数解析 |
| L1 契约 | `crates/server/tests/{contract,console_write}.rs` | 路由面、错误码矩阵、`/models` 字段集合、body 逐字节透传、SSE 分块、上游 429 透传、控制台写接口与 `0600` 落盘 |
| L2 黑盒 | `crates/cli/tests/{cli,load,systemd_unit,console_crud}.rs` | spawn 真二进制：退出码、真进程冒烟、100 并发流 RSS、断开取消、systemd unit 可加载、控制台 URL 与状态接口、控制台增删改后热加载生效 |

`crates/cli/tests/load.rs` 对**真二进制**做两件事：

- 100 并发流（共 400 MiB）下采样 `/proc/<pid>/status` 的 `VmRSS`，断言增长 < 128 MiB —— 缓冲整段流会直接涨满 400 MiB。
- 客户端半途断开后，假上游响应体流的 `Drop` 标记必须在 5 秒内翻转 —— 证明上游连接被取消而不是继续拉取。

RSS 用例只在 Linux 上跑，其它平台自动跳过。

## 控制台

编译期内嵌的单页控制台，挂在 `/console/`，是**配置编辑器**（Provider 与模型的增删改）：

- 两个视图：**Provider**（配置编辑）与**关于**（版本、端口、路径、provider/模型数量、只读状态），顶部导航切换，切视图不刷新页面。
- 启动时生成 32 位随机 **launch token**，只打印一次：`console: http://127.0.0.1:4000/console/?token=<token>`。
- launch token 只做**一次性交换**：`GET /console/?token=<token>` 校验通过后 `303 → ./` 并下发
  `Set-Cookie: anybridge-console=<secret>; HttpOnly; SameSite=Strict; Path=/`（会话级，关浏览器失效）。
  地址栏里的 token 到此为止，之后浏览器只带 cookie。
- cookie 的签名密钥持久化在 `~/.config/anybridge-litellm/console-session.key`（`0600`），
  **所以重启服务不会让已打开的控制台掉登录态**。
- `/console/api/*` 认 cookie，也认 `X-Console-Token: <launch token>` 头（给 curl / 脚本用）。
- 只接受回环 `Host`（`127.0.0.1` / `localhost` / `[::1]`），`Origin` 若存在必须同源，否则 403。
- 写接口额外要求 `Content-Type: application/json`，挡住跨站简单表单。

**增删改**（Provider / 模型）直接改 `--config` 指向的 YAML，并**立即热加载**，不需要重启：

- 写盘是「同目录临时文件 + `fsync` + `rename`」，权限强制 `0600`；校验不过**不落盘**。
- 每次写之前重新从磁盘读一遍，因此外部手工编辑也会被吸收。
- 校验完全复用启动时的加载器：错误以 `{"error":{"details":[{"path","message"}]}}` 返回并展示在表单里。
- **`api_key` 只接受 `credential/<name>` 引用**。密钥值走独立的 `PUT /console/api/credentials/{name}`，
  写进凭据文件（`0600`，原子写盘）后立刻生效；配置响应里只有
  `{"kind":"credential","name":..,"configured":true,"source":"file","writable":true}`，永远不含值。
- 凭据未配置时，转发请求返回 `502 credential_not_configured`，不会带着空 key 打上游。
- **Provider 对话框**（版式参考 DSH Web 的「自定义模型 API」）：单列表单 + 顶部两个 tab。
  「第三方模型提供商」是预设（OpenAI / Anthropic / OpenAI Responses / 本地 Ollama），选中即填地址与协议；
  「自定义模型 API」填 Provider ID、显示名、API 地址、协议、API 密钥。
- **模型目录**在对话框内：`获取可用模型` 带着刚填的地址与密钥请求 `{base_url}/models`
  （新建时走 `POST /console/api/upstream-models` 探活，编辑时走 `GET .../providers/{provider}/upstream-models`），
  也可以 `+ 添加模型` 手填 ID；保存时用 `PUT /console/api/providers/{provider}/models` **整份同步**，
  只增删差异、保留已有条目的其它字段。
- 提交是三步串行：写 provider → 写凭据 → 同步模型目录；任何一步失败都停在对话框里并说明是哪一步。
- 已知限制：整份重写会丢 `#` 注释与空行（键顺序保留）；provider 不支持改名（改名 = 删除 + 新建）。
- 允许把 provider 删空（`providers: {}` 合法），控制台会展示空态；模型列表收在对应 provider 的卡片详情内。

界面统一用 **Tailwind CSS v4 + daisyUI 5**（`src/app.css` 是唯一入口，自定义 `anybridge` / `anybridge-dark` 两套 OKLCH 主题，跟随 `prefers-color-scheme`）。组件只复用 daisyUI 词汇（`card` / `btn` / `badge` / `table` / `modal` / `alert` / `input` / `select` / `checkbox` / `skeleton`），删除确认也用同一套 `<dialog>` 模态，不再调原生 `confirm()`。Tailwind/daisyUI 只在构建期生效，页面不加载任何组件库运行时脚本。

视觉验收依据见 [docs/ui-design.md](docs/ui-design.md)：四条硬约束（字号四档 / 每页一个主操作 / 正文对比度 ≥ 4.5:1 / 彩色元素 ≤ 2）、主题令牌与三层门禁。

### 凭据（`crates/core/src/credential.rs`）

密钥与配置彻底分开，参考 dsh 的 credential seam：

- 配置文件里只出现**引用** `credential/<name>`；`<name>` 匹配 `^[A-Za-z_][A-Za-z0-9_]*$`，最长 64 字节。
- 值由 `CredentialStore` 提供，**每次请求重新解析一次**（`resolve`），所以控制台轮换密钥后下一次请求就生效，不用重启。
- 本期实现只有 `YamlCredentialStore`：`credentials.yaml` 里的 `credentials` 映射，权限 `0600`，
  写盘是「同目录临时文件 + `fsync` + `rename`」；文件不存在等同于「全部未配置」。
- `CredentialStore` 是插拔点：以后接 keyring / Vault / 配置中心，只需新增实现，`server` 与 `console` 不动。
- 控制台接口：`GET /console/api/credentials` 列表、`PUT /console/api/credentials/{name}` 写入/轮换、
  `DELETE /console/api/credentials/{name}` 删除；三者都只回 `{name, reference, configured, source, writable}`。
- 只读模式（未显式 `--config`）下凭据存储也不可写，写接口返回 `409 read_only`。

构建链：`apps/ui`（Vite + TS + Tailwind + daisyUI）→ `dist/` → `rust-embed` 编进 `crates/server`。
`crates/server/build.rs` **只校验不编排**：release 构建缺产物或只有占位页直接失败；debug 构建会生成占位页，保证不装 Node 也能跑 Rust 测试。

## 部署

`deploy/anybridge-litellm.service` 是 systemd 用户级 unit：

```bash
install -Dm755 target/release/anybridge-litellm ~/.local/bin/anybridge-litellm
install -Dm644 deploy/anybridge-litellm.service ~/.config/systemd/user/
install -Dm600 /dev/null ~/.config/anybridge-litellm/config.yaml   # 再填入真实配置
systemctl --user daemon-reload
systemctl --user enable --now anybridge-litellm
```

unit 的关键约束：`ExecStart` 用绝对路径（`%h/.local/bin/`）并显式 `--bind 127.0.0.1:4000`；`Restart=on-failure`；`IPAddressDeny=any` + `IPAddressAllow=localhost` 从网络命名空间层面钉死回环；`ProtectSystem=strict` + `ProtectHome=read-only` + `MemoryDenyWriteExecute=true`，只用一个 `ReadWritePaths=%h/.config/anybridge-litellm` 例外放行控制台写配置。

密钥走 `EnvironmentFile=-%h/.config/anybridge-litellm/env`（`-` 表示可缺省），不要写进 unit 文件。

## 安全

- 默认只绑定 `127.0.0.1`，对外暴露需自行决定（反代 / 防火墙）。
- master key 使用**常量时间比较**。
- 上游 key 只来自配置或环境变量；**不回显、不进日志**。
- 仓库内不放真实凭据；提交前确认示例配置里全是假值（`sk-xxxx`、`credential/<name>`）。
- 凭据文件与配置文件分开：轮换密钥不必改配置，`credentials.yaml` 独立备份/权限管理。
