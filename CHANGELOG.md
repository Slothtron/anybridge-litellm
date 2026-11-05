# Changelog

本文件格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Added

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
