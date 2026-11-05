# Changelog

本文件格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Added

- provider 级 `headers`：自定义请求头映射，值可为 `credential/<name>` 引用或字面量；
  转发时最后写入，可覆盖 `Authorization` / `x-api-key` / `anthropic-version`（适配 Cookie 鉴权的上游）。
- provider 的 `api_key` 改为可选：省略时不注入任何鉴权头，完全由 `headers` 承担。
- 对外 API key（`crates/core/src/client_key.rs`）：存在同一份 `credentials.yaml` 的 `keys` 段，
  只存 `sha256` 摘要，明文只在创建 / 轮换时返回一次；每把 key 带 provider `scope`（id 列表或 `*`）。
- 控制台 API key 接口：`GET|POST /console/api/keys`、`PATCH|DELETE /console/api/keys/{name}`、
  `POST /console/api/keys/{name}/rotate`；控制台新增「API 密钥」视图。
- 新增错误类型 `scope_denied`（403）：对外 API key 越权访问 provider。
- 凭据模块（`crates/core/src/credential.rs`）：配置文件只存 `credential/<name>` 引用，密钥值独立落在
  `credentials.yaml`（默认 `$XDG_CONFIG_HOME/anybridge-litellm/credentials.yaml`，`0600`，原子写盘）。
  `CredentialStore` 是插拔点，本期只有 YAML 文件实现；请求时按操作解析，轮换后下一次请求即生效。
- 控制台凭据接口：`GET /console/api/credentials`、`PUT|DELETE /console/api/credentials/{name}`；
  provider 状态只回 `{kind:"credential", name, reference, configured, source, writable}`，值永不回显。
- `credentials.file` 配置项；未配置时落 XDG 默认路径。
- provider 级可选 `name`（展示名）：为空时界面回退显示 provider id。
- Provider 对话框重做（对齐 DSH Web「自定义模型 API」）：单列表单 + 「第三方模型提供商 / 自定义模型 API」两个 tab，
  预设选中即填地址与协议；模型目录内嵌（获取可用模型 / 手动添加 / 逐条移除）。
- 新增 `POST /console/api/upstream-models`（未建 provider 时用表单里的地址与密钥探活）与
  `PUT /console/api/providers/{provider}/models`（整份 id 列表同步目录，只增删差异、保留已有条目的字段）；
  卡片上的「导入模型」入口移除，模型目录统一在对话框里。
- 控制台新增「关于」视图：版本、监听地址、端口、路径前缀、配置文件、凭据文件、provider/模型数量、只读状态。
- 新增错误类型 `credential_not_configured`（502）：凭据未配置时不带空 key 打上游。

### Changed

- 项目名/版本号不再手写：二进制名取 `env!("CARGO_BIN_NAME")`，服务名只在 `core::GATEWAY_NAME`
  定义一处（`binary_name_matches_gateway_name` 兜住改名漂移），版本走 `env!("CARGO_PKG_VERSION")`；
  `scripts/package.py` 从 `cargo metadata` 读包名、二进制名与版本；控制台标题与 h1 取状态接口的 `name`。
- **破坏性**：`api_key` 只接受 `credential/<name>`，旧的 `os.environ/VAR` 与裸变量名会在加载时报错并提示迁移。
- **破坏性**：`credentials.yaml` 顶层键 `credentials` 重命名为 `refs`（对齐 dsh）；旧布局加载即报错并提示迁移，不做静默回退。
- 控制台 provider 表单新增「使用 API 密钥」开关与「自定义请求头」编辑区；详情页展示 API 密钥状态与请求头。
- 控制台 provider 表单拆成「凭据引用（自动派生、只读）」+「密钥值（只写）」，保存后单独写入凭据文件。
- 凭据命名约定改为 `upper(provider id)` + `_API_KEY`；示例配置同步为 `credential/OPENAI_API_KEY` 等。
- 首页概览统计卡移除，内容并入「关于」视图。
- 只读模式（未显式 `--config`）下凭据存储同样不可写，写接口返回 409。

### Fixed

- 重启服务不再踢掉已打开的控制台：launch token 改为一次性交换，会话落到
  `HttpOnly; SameSite=Strict` 的 cookie；签名密钥持久化在 `console-session.key`（`0600`），
  跨重启有效（对齐 DSH Web 的 browser-session 做法）。`X-Console-Token` 头保留给脚本。

- 输入框里的下划线字符（如 `WPSAI_API_KEY` 的 `_`）被行盒裁掉：`.input` 的 `line-height` 改为控件高度。

- 单二进制 Rust 代理服务：只做上游 provider 的代理，`/llmproxy/{provider}/v1/*` 原样转发，不做协议转换、不做模型别名替换。
- provider 分组配置：一个 provider 一种 `api`，模型级不提供 `api` / `alias`。
- 三种上游 API 声明：`completions` / `anthropic` / `responses`，只决定路径白名单、上游鉴权头与 `extra.reasoning.param`。
- `GET /llmproxy/{provider}/v1/models`：由配置生成，标准 OpenAI 字段 + `extra` 能力声明；不提供 `/models/{id}`。
- 内嵌 Web 控制台：Vite + TS 单页，`rust-embed` 编进二进制，挂在 `/console/`；回环 `Host` + 同源 `Origin` + 启动随机 token 三重校验，只暴露 `GET /console/api/state`（不含上游 key）。
- 控制台配置编辑：`POST|PATCH|DELETE /console/api/providers[/{provider}[/models]]` 增删改 Provider 与模型，写回 `--config` 指向的 YAML 并**立即热加载**，无需重启。
  - 原子写盘（同目录临时文件 + `fsync` + `rename`），权限强制 `0600`；校验失败不落盘。
  - `api_key` 只接受 `os.environ/VAR`，响应只给 `{kind, env}` 描述符，明文密钥既不写盘也不回显。
  - 写接口额外要求 `Content-Type: application/json`；无 `--config` 时返回 409。
  - systemd unit 增加 `ReadWritePaths=%h/.config/anybridge-litellm` 例外。
- UI/UX 统一：控制台接入 **Tailwind CSS v4 + daisyUI 5**，`src/app.css` 单入口定义 `anybridge` / `anybridge-dark` 两套 OKLCH 主题（跟随 `prefers-color-scheme`）。
  - 全部组件复用 daisyUI 词汇（`card` / `btn` / `badge` / `table` / `modal` / `alert` / `input` / `select` / `checkbox` / `skeleton`），删除确认改为同款模态，不再用原生 `confirm()`。
  - 焦点环统一主色；模型表 `min-w-[760px]` + 横向滚动，窄视口不挤压 id / 展示名。
  - Tailwind/daisyUI 只参与构建，页面零运行时 JS 依赖；产物仍是单个哈希 CSS。
- 允许 `providers` 为空：`providers: {}` 是合法配置，控制台可把 provider 删到 0 并展示空态。
- 模型列表收进 provider 卡片详情，页面不再单独渲染「模型」区块。
- master key 鉴权，常量时间比较。
- `GET /health` 与 `GET /` 发现端点。
- 单二进制交付 + systemd 运维形态。
- `deploy/anybridge-litellm.service`：绝对路径 `ExecStart`、`Restart=on-failure`、`IPAddressDeny=any` + `IPAddressAllow=localhost` 钉死回环。
- `mise run package`：tarball + `.sha256`。
- 构建链 `scripts/build.py`：先 `apps/ui` 再 native；`crates/server/build.rs` 只校验产物，release 构建拒绝占位页。
- 测试：L0 单测 92、L1 契约 32、L2 黑盒 18；其中 100 并发流 RSS 断言、「客户端断开即取消上游」与控制台增删改热加载用例直接对真二进制跑。
