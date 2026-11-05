/**
 * 纯渲染层：只操作 DOM，不发请求。
 *
 * 样式全部来自 Tailwind 工具类与 daisyUI 组件（`card` / `table` / `badge` /
 * `alert` / `btn` / `skeleton`），不在这里维护自定义设计令牌。
 * 所有来自状态接口的字符串都通过 textContent 写入，避免拼接 HTML。
 */

import type { ConsoleState, ModelEntry, ProviderEntry } from "./api";

/** 控制台的两个视图；顶部导航切换，切视图不刷新页面。 */
export type View = "providers" | "about";

export interface Shell {
  content: HTMLElement;
  errorBar: HTMLElement;
  refreshButton: HTMLButtonElement;
  nav: Record<View, HTMLButtonElement>;
  /** 页面标题（服务名来自状态接口，不在前端写死）。 */
  title: HTMLElement;
}

/** 服务名只来自状态接口；拿不到时用这个占位，避免空白标题。 */
const FALLBACK_NAME = "anybridge-litellm";

function applyServiceName(shell: Shell, state: ConsoleState): void {
  const name =
    typeof state.name === "string" && state.name.trim() !== ""
      ? state.name.trim()
      : FALLBACK_NAME;
  shell.title.textContent = name;
  document.title = `${name} 控制台`;
}

/** 导航项：`aria-current` 跟着视图走。 */
export function setActiveView(shell: Shell, view: View): void {
  for (const [name, button] of Object.entries(shell.nav) as Array<[View, HTMLButtonElement]>) {
    const active = name === view;
    button.setAttribute("aria-current", active ? "page" : "false");
    button.classList.toggle("btn-active", active);
    button.classList.toggle("btn-ghost", !active);
  }
}

/** 控制台动作。`editable === false` 时所有写入口都不渲染。 */
export interface RowActions {
  editable: boolean;
  onCreateProvider(): void;
  onEditProvider(provider: ProviderEntry): void;
  onDeleteProvider(provider: ProviderEntry): void;
  onCreateModel(provider: ProviderEntry): void;
  onEditModel(provider: ProviderEntry, model: ModelEntry): void;
  onDeleteModel(provider: ProviderEntry, model: ModelEntry): void;
}

const numberFormat = new Intl.NumberFormat("en-US");

const MODALITY_LABELS: Record<string, string> = {
  text: "文本",
  image: "图片",
};

/** api → daisyUI 徽章配色；未知 api 退到中性色。 */
const API_BADGE_VARIANTS: Record<string, string> = {
  completions: "badge-primary",
  anthropic: "badge-secondary",
  responses: "badge-success",
};

export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className !== undefined && className !== "") {
    node.className = className;
  }
  if (text !== undefined) {
    node.textContent = text;
  }
  return node;
}

function textOrDash(value: string | null | undefined): string {
  if (typeof value !== "string") {
    return "-";
  }
  const trimmed = value.trim();
  return trimmed === "" ? "-" : value;
}

function formatNumber(value: number | undefined): string {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return "-";
  }
  return numberFormat.format(value);
}

async function writeClipboard(text: string): Promise<void> {
  if (typeof navigator.clipboard?.writeText === "function") {
    await navigator.clipboard.writeText(text);
    return;
  }
  const textarea = el("textarea", "clipboard-fallback");
  textarea.value = text;
  textarea.setAttribute("readonly", "readonly");
  document.body.appendChild(textarea);
  textarea.select();
  const copied = document.execCommand("copy");
  textarea.remove();
  if (!copied) {
    throw new Error("copy command rejected");
  }
}

function copyButton(text: string, label = text): HTMLButtonElement {
  const button = el(
    "button",
    "btn btn-xs btn-ghost shrink-0 font-normal text-subtle",
    "复制",
  );
  button.type = "button";
  button.title = `复制 ${label}`;
  // 20 多个按钮的可访问名都是「复制」，只有 title 对读屏不可靠。
  button.setAttribute("aria-label", `复制 ${label}`);
  button.addEventListener("click", () => {
    void (async () => {
      try {
        await writeClipboard(text);
        button.textContent = "已复制";
      } catch {
        button.textContent = "复制失败";
      }
      button.disabled = true;
      window.setTimeout(() => {
        button.textContent = "复制";
        button.disabled = false;
      }, 1200);
    })();
  });
  return button;
}

function actionButton(
  label: string,
  className: string,
  onClick: () => void,
): HTMLButtonElement {
  const button = el("button", className, label);
  button.type = "button";
  button.addEventListener("click", onClick);
  return button;
}

function apiBadge(api: string | undefined): HTMLElement {
  const value = typeof api === "string" && api.trim() !== "" ? api : "unknown";
  const variant = API_BADGE_VARIANTS[value] ?? "badge-ghost";
  return el("span", `badge badge-sm badge-soft ${variant} font-mono`, value);
}

function modalityCell(modalities: string[] | undefined): HTMLElement {
  const cell = el("div", "flex flex-wrap gap-1");
  if (!Array.isArray(modalities) || modalities.length === 0) {
    cell.append(el("span", "text-subtle", "-"));
    return cell;
  }
  for (const modality of modalities) {
    cell.append(
      el(
        "span",
        // badge-soft + badge-neutral 在深色主题里文字色就是 --color-neutral（37% 亮度），
        // 叠在近黑底上只有 ~1.5:1，所以中性徽章用 ghost（base-content 文字，14:1）。
        "badge badge-xs badge-ghost",
        MODALITY_LABELS[modality] ?? modality,
      ),
    );
  }
  return cell;
}

function reasoningCell(model: ModelEntry): HTMLElement {
  const cell = el("div", "flex flex-col gap-1");
  const reasoning = model.reasoning;
  const levels = reasoning?.levels;

  if (!reasoning || !Array.isArray(levels) || levels.length === 0) {
    cell.append(el("span", "text-subtle", "-"));
    return cell;
  }

  const row = el("div", "flex flex-wrap gap-1");
  const defaultValue = reasoning.default;
  for (const level of levels) {
    const isDefault = typeof defaultValue === "string" && level === defaultValue;
    const badge = el(
      "span",
      isDefault
        ? "badge badge-xs badge-soft badge-primary font-mono font-semibold"
        : "badge badge-xs badge-ghost font-mono",
      level,
    );
    if (isDefault) {
      badge.title = "默认档位";
    }
    row.append(badge);
  }
  cell.append(row);

  if (typeof reasoning.param === "string" && reasoning.param.trim() !== "") {
    cell.append(
      el("span", "font-mono text-xs text-muted", reasoning.param),
    );
  }
  return cell;
}

function metaRow(
  label: string,
  value: string | null | undefined,
  copyable = true,
  mono = false,
): HTMLElement {
  const row = el(
    "div",
    "grid grid-cols-1 gap-0.5 sm:grid-cols-[5.5rem_minmax(0,1fr)] sm:items-center sm:gap-2",
  );
  row.append(el("dt", "font-mono text-xs text-muted", label));
  const dd = el("dd", "m-0 flex min-w-0 items-center gap-1.5");
  if (typeof value !== "string" || value.trim() === "") {
    dd.append(el("span", "text-subtle", "-"));
  } else {
    const code = el(
      "code",
      mono
        ? "truncate rounded-md bg-base-200 px-1.5 py-0.5 font-mono text-xs"
        : "truncate rounded-md bg-base-200 px-1.5 py-0.5 text-xs",
      value,
    );
    code.title = value;
    dd.append(code);
    if (copyable) {
      dd.append(copyButton(value, label));
    }
  }
  row.append(dd);
  return row;
}

function apiKeyRow(provider: ProviderEntry): HTMLElement {
  const row = el(
    "div",
    "grid grid-cols-1 gap-0.5 sm:grid-cols-[5.5rem_minmax(0,1fr)] sm:items-center sm:gap-2",
  );
  row.append(el("dt", "font-mono text-xs text-muted", "api_key"));
  const dd = el("dd", "m-0 flex min-w-0 items-center gap-1.5");
  const key = provider.api_key;
  const reference = typeof key?.reference === "string" ? key.reference : "";
  if (reference === "") {
    dd.append(el("span", "text-subtle", "-"));
    row.append(dd);
    return row;
  }

  // 只展示引用，值永远不回到前端。
  const code = el(
    "code",
    "truncate rounded-md bg-base-200 px-1.5 py-0.5 font-mono text-xs",
    reference,
  );
  code.title = reference;
  dd.append(code, copyButton(reference, "api_key"));
  dd.append(
    key?.configured === true
      ? el("span", "badge badge-xs badge-success", "已配置")
      : el("span", "badge badge-xs badge-warning", "未配置"),
  );
  if (typeof key?.source === "string" && key.source.trim() !== "") {
    dd.append(
      el("span", "whitespace-nowrap text-xs text-subtle", `源：${key.source}`),
    );
  }
  row.append(dd);
  return row;
}

function emptyState(title: string, ...lines: string[]): HTMLElement {
  const box = el(
    "div",
    "mx-auto w-full max-w-md rounded-box border border-dashed border-base-300 bg-base-100 px-6 py-12 text-center",
  );
  box.append(el("h2", "text-sm font-semibold", title));
  for (const line of lines) {
    box.append(
      el("p", "mx-auto mt-1.5 max-w-xl break-words text-xs text-muted", line),
    );
  }
  return box;
}

function sectionHead(title: string, action?: HTMLButtonElement): HTMLElement {
  const head = el("div", "mb-3 flex flex-wrap items-center justify-between gap-3");
  head.append(
    el(
      "h2",
      "text-xs font-semibold uppercase tracking-[0.14em] text-muted",
      title,
    ),
  );
  if (action !== undefined) {
    head.append(action);
  }
  return head;
}

/** 「关于」页：服务的关键运行信息（版本、端口、路径）。 */
export function renderAbout(shell: Shell, state: ConsoleState): void {
  applyServiceName(shell, state);
  const content = shell.content;
  content.textContent = "";

  const providers = Array.isArray(state.providers) ? state.providers : [];
  const modelCount = providers.reduce(
    (total, provider) => total + (provider.models?.length ?? 0),
    0,
  );

  const section = el("section");
  section.append(sectionHead("关于"));

  const card = el(
    "div",
    "rounded-box border border-base-300 bg-base-100 p-4 shadow-sm sm:p-6",
  );
  const grid = el(
    "dl",
    "m-0 grid grid-cols-1 gap-x-8 gap-y-3 sm:grid-cols-2 xl:grid-cols-3",
  );

  const rows: Array<[string, string | null | undefined, boolean]> = [
    ["名称", state.name, false],
    ["版本", state.version, false],
    ["监听地址", state.bind, true],
    ["端口", portOf(state.bind), true],
    ["路径前缀", state.prefix, true],
    ["Provider 数量", formatNumber(providers.length), false],
    ["模型总数", formatNumber(modelCount), false],
    ["配置文件", state.config_file, true],
    ["凭据文件", state.credentials_file, true],
    ["控制台模式", state.editable === false ? "只读" : "可编辑", false],
  ];
  for (const [label, value, mono] of rows) {
    grid.append(metaRow(label, value, false, mono));
  }

  card.append(grid);
  section.append(card);
  content.append(section);
}

/** 从 `host:port` 里取端口；取不到就原样返回。 */
function portOf(bind: string | undefined): string | undefined {
  if (typeof bind !== "string") {
    return undefined;
  }
  const index = bind.lastIndexOf(":");
  return index >= 0 && index < bind.length - 1 ? bind.slice(index + 1) : bind;
}

function renderProviderCard(provider: ProviderEntry, actions: RowActions): HTMLElement {
  const models = provider.models ?? [];
  const card = el("article", "rounded-box border border-base-300 bg-base-100 shadow-sm");
  const body = el("div", "flex flex-col gap-4 p-4 sm:p-5");

  const head = el("div", "flex flex-wrap items-center gap-2");
  const displayName =
    typeof provider.display_name === "string" && provider.display_name.trim() !== ""
      ? provider.display_name.trim()
      : textOrDash(provider.name);
  head.append(el("h3", "text-sm font-semibold", displayName));
  // 有展示名时把 id 放到旁边（路径与配置都以 id 为准）。
  if (displayName !== provider.name) {
    head.append(el("code", "font-mono text-xs text-muted", textOrDash(provider.name)));
  }
  head.append(apiBadge(provider.api));
  head.append(
    el(
      "span",
      "text-xs text-muted",
      `${formatNumber(models.length)} 个模型`,
    ),
  );
  if (actions.editable) {
    const tools = el("div", "ml-auto flex gap-1.5");
    tools.append(
      actionButton("编辑", "btn btn-xs max-sm:btn-sm", () =>
        actions.onEditProvider(provider),
      ),
      actionButton("删除", "btn btn-xs btn-ghost text-error max-sm:btn-sm", () =>
        actions.onDeleteProvider(provider),
      ),
    );
    head.append(tools);
  }
  body.append(head);

  const meta = el(
    "dl",
    "m-0 grid grid-cols-1 gap-x-6 gap-y-2 sm:grid-cols-2 xl:grid-cols-3",
  );
  meta.append(metaRow("base_url", provider.base_url));
  meta.append(metaRow("base_path", provider.base_path));
  meta.append(apiKeyRow(provider));
  meta.append(
    metaRow(
      "timeout",
      typeof provider.timeout === "number" ? `${provider.timeout}s` : undefined,
      false,
    ),
  );
  meta.append(metaRow("api_version", provider.api_version, false));
  body.append(meta);

  // 模型列表收进 provider 详情，不再单独占一个页面区块。
  const modelsPanel = el("div", "border-t border-base-300/70 pt-3");
  const modelsHead = el(
    "div",
    "mb-2 flex flex-wrap items-center justify-between gap-2",
  );
  modelsHead.append(
    el(
      "h4",
      "text-xs font-semibold uppercase tracking-[0.14em] text-muted",
      `模型 (${formatNumber(models.length)})`,
    ),
  );
  if (actions.editable) {
    modelsHead.append(
      actionButton("新增模型", "btn btn-xs max-sm:btn-sm", () =>
        actions.onCreateModel(provider),
      ),
    );
  }
  modelsPanel.append(modelsHead);

  if (models.length === 0) {
    modelsPanel.append(
      el(
        "p",
        "m-0 rounded-lg border border-dashed border-base-300 px-4 py-6 text-center text-xs text-subtle",
        "该 provider 未配置模型。",
      ),
    );
  } else {
    modelsPanel.append(renderModelTable(provider, models, actions));
  }
  body.append(modelsPanel);

  card.append(body);
  return card;
}

function renderProviders(providers: ProviderEntry[], actions: RowActions): HTMLElement {
  const section = el("section", "mt-8");
  const create = actions.editable
    ? actionButton("新增 Provider", "btn btn-sm btn-primary", actions.onCreateProvider)
    : undefined;
  section.append(sectionHead("Provider", create));

  if (providers.length === 0) {
    section.append(
      emptyState(
        "还没有配置 provider",
        "新增一个 provider，或直接在配置文件里添加后点击右上角「刷新」。",
      ),
    );
    return section;
  }

  const grid = el("div", "grid grid-cols-1 gap-4");
  for (const provider of providers) {
    grid.append(renderProviderCard(provider, actions));
  }
  section.append(grid);
  return section;
}

function renderModelTable(
  provider: ProviderEntry,
  models: ModelEntry[],
  actions: RowActions,
): HTMLElement {
  // 横向滚动容器：窄视口下宁可出滚动条，也不把 id 挤成竖排单字。
  const scroll = el("div", "overflow-x-auto");
  const table = el("table", "table table-sm min-w-[760px]");
  const thead = el("thead");
  const headRow = el(
    "tr",
    "border-b border-base-300 text-xs uppercase tracking-wide text-muted",
  );
  const columns = ["id", "展示名", "上下文窗口", "输出上限", "输入模态", "思考档位"];
  if (actions.editable) {
    columns.push("操作");
  }
  for (const column of columns) {
    const isActions = actions.editable && column === "操作";
    headRow.append(
      el(
        "th",
        isActions
          ? "sticky right-0 z-10 border-l border-base-300 bg-base-100 whitespace-nowrap font-semibold"
          : "whitespace-nowrap font-semibold",
        column,
      ),
    );
  }
  thead.append(headRow);
  table.append(thead);

  const tbody = el("tbody");
  for (const model of models) {
    const row = el("tr", "group hover:bg-base-200/60");
    const idCell = el("td", "whitespace-nowrap");
    idCell.append(el("code", "font-mono text-xs", textOrDash(model.id)));
    row.append(idCell);

    row.append(el("td", "whitespace-nowrap text-base-content/80", textOrDash(model.name)));
    row.append(el("td", "whitespace-nowrap tabular-nums", formatNumber(model.context_window)));
    row.append(
      el("td", "whitespace-nowrap tabular-nums", formatNumber(model.max_output_tokens)),
    );

    const modality = el("td");
    modality.append(modalityCell(model.input_modalities));
    row.append(modality);

    const reasoning = el("td");
    reasoning.append(reasoningCell(model));
    row.append(reasoning);

    if (actions.editable) {
      // 操作列吸在右侧：窄视口下不必横滚整表才能改一条模型。
      const tools = el(
        "td",
        "sticky right-0 z-10 border-l border-base-300 bg-base-100 group-hover:bg-base-200",
      );
      const group = el("div", "flex gap-1");
      group.append(
        actionButton("编辑", "btn btn-xs max-sm:btn-sm", () =>
          actions.onEditModel(provider, model),
        ),
        actionButton("删除", "btn btn-xs btn-ghost text-error max-sm:btn-sm", () =>
          actions.onDeleteModel(provider, model),
        ),
      );
      tools.append(group);
      row.append(tools);
    }

    tbody.append(row);
  }
  table.append(tbody);
  scroll.append(table);
  return scroll;
}

/** 建立页面骨架；只调用一次。 */
export function mountShell(
  root: HTMLElement,
  onRefresh: () => void,
  onView: (view: View) => void,
): Shell {
  root.textContent = "";

  const app = el("div", "min-h-dvh");

  const header = el(
    "header",
    "sticky top-0 z-30 border-b border-base-300 bg-base-100/90 backdrop-blur",
  );
  const bar = el(
    "div",
    "mx-auto flex max-w-7xl items-center gap-3 px-4 py-3 sm:px-6",
  );

  const brand = el("div", "flex min-w-0 flex-1 items-center gap-3");
  brand.append(
    el(
      "div",
      "flex size-9 shrink-0 items-center justify-center rounded-lg bg-primary text-xs font-bold text-primary-content shadow-sm",
      "AB",
    ),
  );
  const titles = el("div", "min-w-0");
  const title = el("h1", "truncate text-2xl font-semibold tracking-tight", "");
  titles.append(title);
  titles.append(
    el(
      "p",
      "truncate text-xs text-muted",
      "本地控制台 · 编辑本机配置文件 · 不存数据、不回显密钥",
    ),
  );
  brand.append(titles);
  bar.append(brand);

  const refreshButton = el(
    "button",
    "btn btn-sm btn-ghost shrink-0 gap-1.5",
    "刷新",
  );
  refreshButton.type = "button";
  refreshButton.addEventListener("click", onRefresh);
  bar.append(refreshButton);

  const nav = el(
    "nav",
    "mx-auto flex max-w-7xl items-center gap-1 px-4 pb-2 sm:px-6",
  );
  nav.setAttribute("aria-label", "控制台视图");
  const navButtons = {
    providers: el("button", "btn btn-xs btn-ghost", "Provider"),
    about: el("button", "btn btn-xs btn-ghost", "关于"),
  } as Record<View, HTMLButtonElement>;
  for (const [view, button] of Object.entries(navButtons) as Array<[View, HTMLButtonElement]>) {
    button.type = "button";
    button.addEventListener("click", () => onView(view));
    nav.append(button);
  }

  header.append(bar, nav);

  // 可见性只能靠 `hidden` 工具类切换：daisyUI 的 `.alert{display:grid}` 会盖掉
  // UA 的 `[hidden]{display:none}`，只改 `hidden` 属性错误条依然可见。
  const errorBar = el("div", "alert alert-error mx-auto mt-4 hidden max-w-7xl text-sm");
  errorBar.setAttribute("role", "alert");
  errorBar.hidden = true;

  const content = el("main", "mx-auto w-full max-w-7xl px-4 pb-16 pt-6 sm:px-6");

  app.append(header, errorBar, content);
  root.append(app);

  return { content, errorBar, refreshButton, nav: navButtons, title };
}

export function setRefreshing(shell: Shell, busy: boolean): void {
  shell.refreshButton.disabled = busy;
  shell.refreshButton.textContent = busy ? "刷新中…" : "刷新";
}

export function showError(shell: Shell, message: string): void {
  shell.errorBar.textContent = message;
  shell.errorBar.hidden = false;
  shell.errorBar.classList.remove("hidden");
}

export function clearError(shell: Shell): void {
  shell.errorBar.textContent = "";
  shell.errorBar.hidden = true;
  shell.errorBar.classList.add("hidden");
}

export function renderLoading(shell: Shell): void {
  const content = shell.content;
  content.textContent = "";
  const box = el("div", "flex flex-col gap-3 py-2");
  for (let index = 0; index < 3; index += 1) {
    box.append(el("div", "skeleton h-16 w-full rounded-box"));
  }
  box.append(
    el("p", "mt-1 text-center text-xs text-muted", "正在加载控制台状态…"),
  );
  content.append(box);
}

export function renderErrorState(shell: Shell, title: string, message: string): void {
  const content = shell.content;
  content.textContent = "";
  content.append(emptyState(title, message));
}

export function renderState(
  shell: Shell,
  state: ConsoleState,
  actions: RowActions,
): void {
  applyServiceName(shell, state);
  const content = shell.content;
  content.textContent = "";
  const providers = Array.isArray(state.providers) ? state.providers : [];

  if (!actions.editable) {
    const note = el("div", "alert alert-warning mb-6 text-sm");
    note.append(
      document.createTextNode(
        "当前以只读方式启动（未指定 --config），控制台不会修改任何配置。",
      ),
    );
    content.append(note);
  }

  content.append(renderProviders(providers, actions));
}
