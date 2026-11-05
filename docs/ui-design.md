# 控制台 UI 设计规范

> **状态**：v1.1（2026-11-05 落地）
> **适用**：`apps/ui`（Tailwind CSS v4 + daisyUI 5）
> **性质**：本文是**验收依据**，不是概览。改 UI 前先读本文；与代码冲突时以本文为准，改代码而不是改文档。

---

## 0. 定位

控制台是**配置编辑器**，不是数据看板。这决定了两件事：

1. **一个页面只回答一个问题**——「现在配了哪些 Provider / 模型」。
2. **密度优先，克制装饰**——风格取 Data-Dense Dashboard（多表格、KPI 卡、最小内边距、网格布局），不做营销页式的 Hero / 渐变 / 玻璃拟态。

---

## 1. 四条硬约束

不满足即视为未完成，不接受「后续再补」。

| # | 规则 | 量化标准 | 反例 |
|---|---|---|---|
| 1 | **字号只用四档** | `text-xs`(12) / `text-sm`(14) / `text-lg`(18) / `text-2xl`(24)；相邻步进 ≥ 1.25 | `text-[10.5px]` / `text-[11px]` / `text-[11.5px]` / 裸 `text-base`(16) |
| 2 | **每页 1 个主操作** | 页面级 `btn-primary` 计数 = 1；`btn-error` 只出现在确认弹窗内 | 行内常驻 `btn-error` 删除按钮 |
| 3 | **正文对比度 ≥ 4.5:1** | 次要文字用 `.text-muted`(78%) / `.text-subtle`(70%)；禁止 `text-base-content/45`、`/50` | 实测 3.1:1 / 3.2:1，不过 WCAG AA |
| 4 | **彩色元素一屏 ≤ 2 个** | 语义色只用于状态点 / 徽章，不铺背景 | 每张卡片一个彩色底，强调失效 |

**依据不是审美**：11 / 11.5 / 12 三档的相邻步进只有 1.04–1.09，等于「写了 5 档只得到 3 个可感知层级」，属于无效复杂度。

> 弹窗是独立表面：`modal-box` 内的 `btn-primary`（提交）与 `btn-error`（危险确认）不计入页面级计数。

---

## 2. 主题令牌

色值以 `apps/ui/src/app.css` 的 `@plugin "daisyui/theme"` 块为准，两套主题一一对应：

| 令牌 | 浅色 `anybridge` | 深色 `anybridge-dark` | 对比度 |
|---|---|---|---|
| `--color-primary` | `oklch(42.44% 0.1809 265.64)` | `oklch(72.5% 0.135 265.64)` | 白字 8.72:1 / 7.36:1 |
| `--color-accent` | `oklch(66.58% 0.1574 58.32)` | `oklch(87% 0.1 58.32)` | 黑字 6.59:1 / 10.43:1 |
| `--color-warning` | `oklch(78% 0.15 92)` | `oklch(86% 0.13 92)` | 深色 content 9.0:1 / 11.27:1 |
| `--color-error` | `oklch(50% 0.17 27)` | `oklch(78% 0.13 25)` | 白字 / 深色 content |

### 两条使用约束

- **`accent` 只可作填充**（`bg-accent` + `text-accent-content`）。作正文前景叠在 `base-200` 上只有 **3.04:1**，正文强调一律用 `text-primary`（8.34:1）或 `text-base-content`。
- **`warning` 必须是黄色**。与 `accent`（58.32°）拉开 33.7°、OKLab ΔE 0.145 / 0.073；退回琥珀会让深色主题下「警告徽章」与「强调按钮」几乎同色。黄色是浅色，`warning-content` 必须用深色（白字只有 2.01:1）。

---

## 3. 组件词汇

只复用 daisyUI 5 的语义类，**不写一次性样式**：

`card` / `btn` / `badge` / `table` / `modal` / `alert` / `skeleton` / `input` / `select` / `checkbox`

| 场景 | 约定 |
|---|---|
| 对话框 | 原生 `<dialog class="modal">` + `modal-box`；表单与删除确认共用，不用原生 `confirm()` |
| 行内删除 | `btn-ghost text-error`（低权重），危险确认才用 `btn-error` |
| 焦点 | `input` / `select` 的 `--input-color` 指向 `--color-primary`，不跟 daisyUI 默认近黑色 |
| 宽表格 | `min-w-[760px]` + 外层 `overflow-x-auto`，窄视口横向滚动而不是把 id 挤成竖排 |
| 输入行高 | `.input` 的 `line-height` 等于控件高度，避免下划线被行盒裁掉 |

---

## 4. 排版与间距

| 维度 | 规则 |
|---|---|
| 字号 | 四档：12 / 14 / 18 / 24（§1 契约 1） |
| 行高 | 14px 正文 `leading-relaxed`(1.625)；12px 说明文字 `leading-snug`(1.375) |
| 间距 | 4 的倍数；区块间 24，组内 8 |
| 字重 | 正文 400，标签 500，标题 600；不用 700 以上 |
| 等宽 | 仅用于 id / 字段名 / 代码片段（`font-mono`） |

---

## 5. 门禁

| 层 | 命令 | 通过标准 |
|---|---|---|
| 构建 | `cd apps/ui && npm run build` | `tsc --noEmit` 通过；单 CSS 产物；无运行时组件库脚本 |
| 全量 | `mise run check` | fmt + clippy `-D warnings` + workspace 测试全绿 |
| 视觉 | 1440 / 420px 视口实机走查 | §1 四条硬约束逐条核对 |

```bash
# 看渲染结果
cd apps/ui && npm run dev -- --host 127.0.0.1
# → http://127.0.0.1:5199/console/
```

> 视觉层目前**没有自动化门禁**：字号看 `getComputedStyle`，对比度用 DevTools 取色器读数，主操作/彩色元素靠人工清点。想加自动化需在 `crates/cli/tests` 里补一个 DOM 断言，不引入外部检测器。

---

## 6. 反模式

- ❌ 装饰性渐变、玻璃拟态、发光阴影
- ❌ 无筛选的宽表格（必须有横向滚动容器）
- ❌ Emoji 当图标（用 SVG：Heroicons / Lucide）
- ❌ 布局位移型 hover（`scale` / `translate` 推挤相邻元素）
- ❌ 无 `cursor-pointer` 的可点击元素
- ❌ 不可见的焦点态
- ❌ 常驻的 `btn-error`（破坏性操作收进弹窗）

---

## 7. 与 `design-system/` 的关系

| 文件 | 角色 |
|---|---|
| **`docs/ui-design.md`**（本文） | **唯一真源**，验收依据 |
| `design-system/anybridge-litellm/MASTER.md` | ui-ux-pro-max 生成的原始推荐，方法论参考 |
| `design-system/anybridge-litellm/pages/console.md` | 人工裁决版：MASTER 与现有主题的映射与取舍 |
| `docs/design.md` §3.5.2 | 整体设计方案（架构 / 契约 / 里程碑），UI 部分指向本文 |

---

## 8. 变更记录

| 日期 | 变更 |
|---|---|
| 2026-11-05 | v1：四条硬约束、主色/强调色切 `#1E40AF` + `#D97706`、warning 推黄色、门禁（构建 / 全量 / 视觉走查） |
| 2026-11-05 | v1.1 落地：15 处任意字号（`text-[11px]` ×14、`text-[11.5px]` ×1）统一为 `text-xs`；4 处裸 `text-base` 归入四档（h1 → `text-2xl`，弹窗 h2 → `text-lg`）；2 处行内删除按钮 `btn-error btn-soft` → `btn-ghost text-error` |
