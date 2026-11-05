# Console 页面规则（人工裁决版）

> **PROJECT:** anybridge-litellm
> **类型:** Dashboard / Data View
> **状态:** 手写。`MASTER.md` 是 `search.py --persist` 自动生成的原始推荐，本文件是它与现有 daisyUI 主题的**裁决结果**。
> **优先级:** 本文件 > `MASTER.md`。重新生成 MASTER 时**不要覆盖本文件**。

---

## 0. 裁决总览

| 维度 | MASTER.md 建议 | 裁决 | 理由 |
|---|---|---|---|
| 风格 | Data-Dense Dashboard | ✅ **采纳** | 与「配置编辑器」定位一致 |
| 布局 | 12 栏网格、max-width 1400px | ⚠️ 部分采纳 | 用 `max-w-7xl`(1280) + 单页表格，不做 12 栏栅格系统 |
| 间距 | 4 / 8 / 16 / 24 / 32 / 48 / 64 | ✅ **采纳** | 4 的倍数，与 Tailwind 默认刻度一致 |
| 阴影 | sm / md / lg / xl | ✅ **采纳** | 优先用 daisyUI 组件内置 `--depth`，不手写 box-shadow |
| 主色 | `#1E40AF` | ✅ **采纳**（2026-11-05） | 已改为 `--color-primary: oklch(42.44% 0.1809 265.64)`；白字 8.72:1 |
| 强调色 | `#D97706`（琥珀） | ✅ **采纳**（2026-11-05） | 已改为 `--color-accent: oklch(66.58% 0.1574 58.32)`；黑字 6.59:1。**仅可作填充**，见 §1 警告 |
| 字体 | Fira Code + Fira Sans | ❌ **拒绝** | 控制台是中文界面，system-ui + Noto Sans SC/PingFang 更稳；Fira 无中文字形，回退后中英混排会跳 |
| 组件 CSS | 手写 `.btn-primary` 等 | ❌ **拒绝** | 与 daisyUI 类名冲突（daisyUI 是 `btn btn-primary`），会双写样式 |
| 反模式 | 无装饰、必须有筛选 | ✅ **采纳** | 见 §4 |
| 交付检查 | 9 条 | ✅ **采纳** | 见 §5 |

**一句话**：MASTER 的**方法论**（密度、间距、反模式、检查单）采纳；**具体色值和字体**以 `apps/ui/src/app.css` 的 `anybridge` / `anybridge-dark` 主题为准。

---

## 1. 令牌映射（MASTER → 现有 daisyUI）

| MASTER 角色 | 现有令牌 | 说明 |
|---|---|---|
| Background `#F8FAFC` | `--color-base-200` | 页面底色 |
| Card `#FFFFFF` | `--color-base-100` | 卡片/表格底 |
| Border `#DBEAFE` | `--color-base-300` | 分隔线 |
| Foreground `#1E3A8A` | `--color-base-content` | 正文 |
| Muted Foreground `#475569` | `.text-muted` / `.text-subtle` | **不要**低于 65%，45%/50% 实测 3.1–3.2:1 不过 AA |
| Primary `#1E40AF` | `--color-primary` = `oklch(42.44% 0.1809 265.64)` | 主操作、焦点环、`btn-primary` |
| Accent `#D97706` | `--color-accent` = `oklch(66.58% 0.1574 58.32)` | **仅作填充**（`bg-accent` + 黑字），见 §1 警告 |
| Destructive `#DC2626` | `--color-error`（oklch 50% 0.17 27） | 仅确认弹窗内使用 |
| Warning（MASTER 无） | `--color-warning` = `oklch(78% 0.15 92)` | 推到黄色，与 accent 拉开 ΔE 0.145；content 改深色（9.0:1） |
| Ring `#1E40AF` | `--color-primary` | 已在 `app.css` 的 `:focus` 里覆盖 |

> 深色主题一一对应，见 `app.css` 的 `anybridge-dark` 块。**不要为 MASTER 的 hex 单独写 fallback**。

### ⚠️ 强调色对比度警告

`--color-accent`（`#D97706`）叠在页面底色 `--color-base-200`（`#F8FAFC`）上只有 **3.04:1**：

- ✅ 可作**填充**（`bg-accent` + `text-accent-content` 黑字，6.59:1）
- ✅ 可作**大字号**（≥ 18.66px bold / 24px regular，3:1 门槛）
- ❌ **不可作正文前景**（`text-accent` 小字不过 4.5:1）

正文强调一律用 `text-primary`（8.34:1）或 `text-base-content`。

---

## 2. 排版契约

| 用途 | 类名 | 字号 / 行高 | 备注 |
|---|---|---|---|
| 页面标题 | `text-2xl` | 24 / 32 | `h1` 已从 `text-base` 提档 |
| 弹窗标题 | `text-lg` | 18 / 28 | `h2` 已从 `text-base` 提档 |
| 正文 | `text-sm` | 14 / 20 | 表格单元格、表单值 |
| 说明/标签 | `text-xs` | 12 / 16 | 15 处 `text-[11px]` / `[11.5px]` 已统一 |

**禁止**：`text-[10.5px]`、`text-[11px]`、`text-[11.5px]`、裸 `text-base`。
**原因**：11 / 11.5 / 12 三档差 1px，人眼分辨不出，却让相邻步进只有 1.04–1.09，等于「写了 5 档只得到 3 个可感知层级」。

---

## 3. 强调契约

| 规则 | 目标 | 现状 |
|---|---|---|
| 每页主操作数 | **1× `btn-primary`** | ✅ 页面级仅「新增 Provider」 |
| 破坏性操作 | 行内 `btn-ghost text-error`，确认弹窗内才 `btn-error` | ✅ 2 处行内删除已降级 |
| 彩色元素上限 | 一屏 ≤ 2 个 | ✅ 品牌块 + 错误条 |

**原因**：破坏性操作比主操作还多时，页面会被红色主导，用户找不到「下一步该点什么」。

---

## 4. 反模式（继承 MASTER）

- ❌ 装饰性渐变、玻璃拟态、发光阴影
- ❌ 无筛选的宽表格（必须有 `overflow-x-auto` + `min-w-[760px]`）
- ❌ Emoji 当图标（用 Heroicons / Lucide）
- ❌ 布局位移型 hover（`scale` / `translate` 会推挤相邻元素）
- ❌ 无 `cursor-pointer` 的可点击元素
- ❌ 不可见的焦点态
- ❌ 对比度 < 4.5:1 的正文

---

## 5. 交付检查单

- [ ] 无任意字号 `text-[Npx]`、无裸 `text-base`（全部走 §2 四档）
- [ ] 每页 1 个 `btn-primary`，`btn-error` 仅出现在弹窗
- [ ] 正文对比度 ≥ 4.5:1（`.text-muted` / `.text-subtle` 起步）
- [ ] 表格窄视口横向滚动，不挤成竖排
- [ ] 焦点态可见（`focus-visible` 走主色）
- [ ] `prefers-reduced-motion` 下无动效
- [ ] 375 / 768 / 1024 / 1440 四档无溢出
- [ ] `cd apps/ui && npm run build` 通过，`mise run check` 全绿

---

## 6. 重新生成

```bash
cd /home/lolioy/workspace/tools/anybridge/anybridge-litellm
python3 ~/.agents/skills/ui-ux-pro-max/scripts/search.py \
  "data tables KPI cards space-efficient maximum data visibility enterprise reporting" \
  --design-system -p "anybridge-litellm" --persist --output-dir .
```

> 该命令会**覆盖** `MASTER.md` 与 `pages/console.md`。本文件是手写裁决版，重新生成前先 `cp pages/console.md /tmp/console.reconciled.md` 备份。
