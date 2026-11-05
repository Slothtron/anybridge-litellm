# design-system/

UI/UX Pro Max 生成的设计系统产物，供 agent 在改 `apps/ui` 前读取。

## 文件

| 文件 | 来源 | 是否可直接照做 |
|---|---|---|
| `anybridge-litellm/MASTER.md` | `search.py --persist` 自动生成 | ⚠️ **部分不适用**，见下 |
| `anybridge-litellm/pages/console.md` | **手写裁决版** | ✅ 以本文件为准 |

## 阅读顺序

1. `pages/console.md` —— 裁决结果与令牌映射（**先读这个**）
2. `MASTER.md` —— 原始推荐，作为方法论参考

## 为什么需要裁决

`search.py` 是**关键词检索**，不理解「配置控制台」这个语义。它给出的：

- **采纳**：Data-Dense Dashboard 风格、4px 间距刻度、阴影层级、反模式清单、交付检查单、**主色 `#1E40AF` / 强调色 `#D97706`**（已换算为 OKLCH 写入 `app.css`）
- **拒绝**：Fira 字体（无中文字形）、手写 `.btn-primary` CSS（与 daisyUI 类名冲突）、落地页式 Page Pattern（这是应用界面，不是营销页）

> 强调色有一条硬约束：`#D97706` 叠在页面底色上只有 3.04:1，**只能作填充**，不能作正文前景。详见 `pages/console.md` §1。

## 令牌唯一来源

色值以 `apps/ui/src/app.css` 的 `@plugin "daisyui/theme"` 块为准：

- 浅色：`anybridge`
- 深色：`anybridge-dark`（`prefersdark`）

本目录**不复制**色值，只做角色映射。

## 重新生成

```bash
cd /home/lolioy/workspace/tools/anybridge/anybridge-litellm
python3 ~/.agents/skills/ui-ux-pro-max/scripts/search.py \
  "data tables KPI cards space-efficient maximum data visibility enterprise reporting" \
  --design-system -p "anybridge-litellm" --persist --output-dir .
```

> ⚠️ 会覆盖 `pages/console.md`。改之前先备份手写版。

## 相关

- **UI 设计规范（唯一真源）**：[`docs/ui-design.md`](../docs/ui-design.md)
- 整体设计方案：[`docs/design.md`](../docs/design.md) §3.5.2
