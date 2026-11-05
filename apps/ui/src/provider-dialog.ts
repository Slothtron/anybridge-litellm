/**
 * 新增 / 编辑 provider 的对话框。
 *
 * 版式参考 DSH Web 的「自定义模型 API」：**单列表单** + 顶部两个 tab
 * （第三方模型提供商 / 自定义模型 API）+ 内嵌「模型目录」。
 *
 * 提交是三步串行，任何一步失败都停在原地并回显原因：
 * 1. `POST|PATCH /console/api/providers`（id、展示名、地址、协议）
 * 2. `PUT /console/api/credentials/{name}`（只写密钥值；留空表示不修改）
 * 3. `PUT /console/api/providers/{id}/models`（整份 id 列表同步目录）
 *
 * 凭据名由 provider id 派生（`upper(id)` + `_API_KEY`），表单里只展示、不可编辑。
 */

import {
  PROVIDERS_ENDPOINT,
  fetchUpstreamModels,
  mutate,
  probeUpstreamModels,
  providerPath,
  setCredential,
  syncModels,
  type ProviderDraft,
  type ProviderEntry,
} from "./api";
import { modalBackdrop } from "./form";
import { el } from "./render";

/** 预设：选中后只填地址与协议，id / 展示名仍由用户确认。 */
interface Preset {
  id: string;
  label: string;
  api: string;
  base_url: string;
  hint: string;
}

const PRESETS: readonly Preset[] = [
  {
    id: "openai",
    label: "OpenAI",
    api: "completions",
    base_url: "https://api.openai.com/v1",
    hint: "Chat Completions",
  },
  {
    id: "anthropic",
    label: "Anthropic",
    api: "anthropic",
    base_url: "https://api.anthropic.com/v1",
    hint: "Messages",
  },
  {
    id: "openai-responses",
    label: "OpenAI Responses",
    api: "responses",
    base_url: "https://api.openai.com/v1",
    hint: "Responses",
  },
  {
    id: "ollama",
    label: "本地 Ollama",
    api: "completions",
    base_url: "http://127.0.0.1:11434/v1",
    hint: "本机 11434",
  },
];

const API_OPTIONS = [
  { value: "completions", label: "OpenAI Chat Completions" },
  { value: "anthropic", label: "Anthropic Messages" },
  { value: "responses", label: "OpenAI Responses" },
];

/**
 * 凭据名由 provider id 派生：`upper(id)` + `_API_KEY`，非字母数字归一成 `_`。
 * 与服务端 `credential_ref_for` 保持一致。
 */
export function credentialNameFor(providerId: string): string {
  const normalized = providerId
    .trim()
    .toUpperCase()
    .replace(/[^A-Z0-9]/g, "_");
  return `${normalized}_API_KEY`;
}

export interface ProviderDialogOptions {
  /** `null` 表示新建。 */
  provider: ProviderEntry | null;
  /** 保存成功后的回调（通常是重新拉状态）。 */
  onSaved(): void | Promise<void>;
}

type Mode = "preset" | "custom";

/** 打开对话框；不返回句柄，关掉即销毁。 */
export function openProviderDialog(options: ProviderDialogOptions): void {
  const editing = options.provider !== null;
  const current = options.provider;
  let mode: Mode = editing ? "custom" : "preset";
  let models: string[] = (current?.models ?? []).map((model) => model.id);
  let busy = false;

  const dialog = el("dialog", "modal");
  const titleId = "provider-dialog-title";
  dialog.setAttribute("aria-labelledby", titleId);

  const box = el("div", "modal-box w-11/12 max-w-2xl p-0");
  const form = el("form", "flex max-h-[85vh] flex-col");
  form.noValidate = true;

  // ---- 头部 ----
  const header = el("header", "border-b border-base-300 px-6 py-4");
  const heading = el(
    "h2",
    "text-lg font-semibold",
    current !== null ? `编辑 provider · ${current.name}` : "新增 provider",
  );
  heading.id = titleId;
  header.append(heading);

  const tabs = el("div", "mt-3 flex gap-1 border-b border-base-300", "");
  tabs.setAttribute("role", "tablist");
  const tabButtons: Record<Mode, HTMLButtonElement> = {
    preset: el("button", "btn btn-sm btn-ghost rounded-b-none", "第三方模型提供商"),
    custom: el("button", "btn btn-sm btn-ghost rounded-b-none", "自定义模型 API"),
  };
  for (const [name, button] of Object.entries(tabButtons) as Array<[Mode, HTMLButtonElement]>) {
    button.type = "button";
    button.setAttribute("role", "tab");
    button.addEventListener("click", () => {
      mode = name;
      syncTabs();
    });
    tabs.append(button);
  }
  if (!editing) {
    header.append(tabs);
  }

  // ---- 主体 ----
  const body = el("div", "flex flex-col gap-4 overflow-y-auto px-6 py-5");

  const description = el(
    "p",
    "m-0 text-xs leading-relaxed text-muted",
    "连接中转站、自部署服务或其他兼容 OpenAI / Anthropic 协议的接口，需填写 API 地址、协议和模型。",
  );

  // 预设面板
  const presetPanel = el("div", "grid grid-cols-1 gap-2 sm:grid-cols-2");
  for (const preset of PRESETS) {
    const card = el(
      "button",
      "flex flex-col items-start gap-0.5 rounded-box border border-base-300 bg-base-100 px-3 py-2.5 text-left hover:border-primary",
    );
    card.type = "button";
    card.append(
      el("span", "text-sm font-medium", preset.label),
      el("span", "font-mono text-xs text-subtle", `${preset.hint} · ${preset.base_url}`),
    );
    card.addEventListener("click", () => {
      idInput.value = preset.id;
      baseUrlInput.value = preset.base_url;
      apiSelect.value = preset.api;
      syncCredentialHint();
      mode = "custom";
      syncTabs();
    });
    presetPanel.append(card);
  }

  // 自定义面板（单列）
  const customPanel = el("div", "flex flex-col gap-4");

  const idInput = el("input", "input w-full");
  idInput.type = "text";
  idInput.id = "provider-id";
  idInput.name = "provider-id";
  idInput.placeholder = "acme-gateway";
  idInput.required = true;
  idInput.readOnly = editing;
  if (current !== null) {
    idInput.value = current.name;
    idInput.classList.add("bg-base-200", "text-muted");
  }

  const displayNameInput = el("input", "input w-full");
  displayNameInput.type = "text";
  displayNameInput.id = "provider-display-name";
  displayNameInput.placeholder = "显示名称";
  displayNameInput.value = current?.display_name ?? "";

  const baseUrlInput = el("input", "input w-full");
  baseUrlInput.type = "url";
  baseUrlInput.id = "provider-base-url";
  baseUrlInput.placeholder = "https://gateway.example/v1";
  baseUrlInput.value = current?.base_url ?? "";
  baseUrlInput.required = true;

  const apiSelect = el("select", "select w-full");
  apiSelect.id = "provider-api";
  for (const option of API_OPTIONS) {
    const node = el("option", undefined, option.label);
    node.value = option.value;
    apiSelect.append(node);
  }
  apiSelect.value = current?.api ?? "completions";

  const secretInput = el("input", "input w-full");
  secretInput.type = "password";
  secretInput.id = "provider-api-key";
  secretInput.placeholder = "输入 API 密钥";
  secretInput.autocomplete = "off";

  const credentialHint = el("p", "m-0 text-xs leading-relaxed text-subtle", "");

  customPanel.append(
    field("Provider ID", idInput, "以小写字母开头的标识，在请求中唯一标识该提供商，并用于派生凭据名。"),
    field("显示名称", displayNameInput),
    field("API 地址", baseUrlInput),
    field("API 协议", apiSelect),
    field("API 密钥", secretInput),
    credentialHint,
  );

  // ---- 模型目录 ----
  const catalog = el("section", "flex flex-col gap-2 rounded-box bg-base-200/60 p-3");
  const catalogHead = el("div", "flex items-center justify-between gap-2");
  catalogHead.append(
    el("span", "text-xs font-medium text-base-content", "模型目录"),
  );
  const fetchButton = el("button", "btn btn-xs btn-ghost", "获取可用模型");
  fetchButton.type = "button";
  catalogHead.append(fetchButton);

  const catalogList = el("div", "flex flex-col gap-1");
  const catalogEmpty = el(
    "p",
    "m-0 rounded-lg border border-dashed border-base-300 px-3 py-4 text-center text-xs text-subtle",
    "模型选择器中将不显示任何模型；目录外 ID 仍可直接发送。",
  );

  const addRow = el("div", "flex gap-1.5");
  const addInput = el("input", "input input-sm w-full");
  addInput.type = "text";
  addInput.placeholder = "手动添加模型 ID";
  addInput.setAttribute("aria-label", "手动添加模型 ID");
  const addButton = el("button", "btn btn-sm btn-ghost shrink-0", "+ 添加模型");
  addButton.type = "button";
  addRow.append(addInput, addButton);

  catalog.append(catalogHead, catalogList, catalogEmpty, addRow);

  body.append(description, presetPanel, customPanel, catalog);

  // ---- 错误 ----
  const error = el("p", "alert alert-error mx-6 mb-4 whitespace-pre-line text-xs");
  error.setAttribute("role", "alert");
  error.hidden = true;

  // ---- 动作 ----
  const actions = el("div", "modal-action m-0 items-center border-t border-base-300 px-6 py-4");
  const cancel = el("button", "btn btn-sm btn-ghost", "取消");
  cancel.type = "button";
  const submit = el("button", "btn btn-sm btn-primary", editing ? "保存" : "创建提供商");
  submit.type = "submit";
  actions.append(cancel, submit);

  form.append(header, body, error, actions);
  box.append(form);
  dialog.append(box, modalBackdrop());
  document.body.append(dialog);

  // ---- 行为 ----
  function field(label: string, control: HTMLElement, hint?: string): HTMLElement {
    const wrap = el("div", "flex flex-col gap-1.5");
    const text = el("label", "text-xs font-medium text-muted", label);
    if (control.id !== "") {
      text.htmlFor = control.id;
    }
    wrap.append(text, control);
    if (hint !== undefined) {
      wrap.append(el("p", "m-0 text-xs leading-relaxed text-subtle", hint));
    }
    return wrap;
  }

  function syncTabs(): void {
    for (const [name, button] of Object.entries(tabButtons) as Array<[Mode, HTMLButtonElement]>) {
      const active = name === mode;
      button.classList.toggle("btn-active", active);
      button.classList.toggle("btn-ghost", !active);
      button.setAttribute("aria-selected", active ? "true" : "false");
    }
    presetPanel.hidden = mode !== "preset";
    customPanel.hidden = mode !== "custom";
    // 模型目录属于「自定义」这一步；预设 tab 下不展示。
    catalog.hidden = mode !== "custom";
  }

  function syncCredentialHint(): void {
    const name = credentialNameFor(idInput.value);
    const reference = `credential/${name}`;
    credentialHint.textContent =
      current?.api_key?.configured === true
        ? `当前凭据已配置（${reference}，源：${current.api_key.source ?? "file"}）；留空表示不修改`
        : `配置里只写引用 ${reference}；值写入独立的凭据文件（0600），保存后不再回显`;
  }

  function renderModels(): void {
    catalogList.textContent = "";
    for (const id of models) {
      const row = el(
        "div",
        "flex items-center gap-2 rounded-md bg-base-100 px-2 py-1.5",
      );
      row.append(el("code", "min-w-0 flex-1 truncate font-mono text-xs", id));
      const remove = el("button", "btn btn-xs btn-ghost text-error", "移除");
      remove.type = "button";
      remove.addEventListener("click", () => {
        models = models.filter((item) => item !== id);
        renderModels();
      });
      row.append(remove);
      catalogList.append(row);
    }
    catalogEmpty.hidden = models.length > 0;
  }

  function addModel(id: string): void {
    const value = id.trim();
    if (value === "" || models.includes(value)) {
      return;
    }
    models = [...models, value];
    renderModels();
  }

  addButton.addEventListener("click", () => {
    addModel(addInput.value);
    addInput.value = "";
    addInput.focus();
  });
  addInput.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      addModel(addInput.value);
      addInput.value = "";
    }
  });

  fetchButton.addEventListener("click", () => {
    void (async () => {
      fetchButton.disabled = true;
      const original = fetchButton.textContent;
      fetchButton.textContent = "获取中…";
      try {
        const result = current
          ? await fetchUpstreamModels(current.name)
          : await probeUpstreamModels({
              base_url: baseUrlInput.value.trim(),
              api: apiSelect.value,
              api_key: secretInput.value,
            });
        if (result.kind === "error") {
          error.textContent = result.message;
          error.hidden = false;
          return;
        }
        error.hidden = true;
        for (const model of result.models) {
          addModel(model.id);
        }
        if (result.models.length === 0) {
          error.textContent = "上游没有返回任何模型";
          error.hidden = false;
        }
      } finally {
        fetchButton.disabled = false;
        fetchButton.textContent = original;
      }
    })();
  });

  idInput.addEventListener("input", () => {
    syncCredentialHint();
  });

  cancel.addEventListener("click", () => dialog.close());
  dialog.addEventListener("close", () => dialog.remove());

  form.addEventListener("submit", (event) => {
    event.preventDefault();
    if (busy) {
      return;
    }
    const id = idInput.value.trim();
    const baseUrl = baseUrlInput.value.trim();
    if (id === "") {
      error.textContent = "Provider ID：必填";
      error.hidden = false;
      idInput.focus();
      return;
    }
    if (baseUrl === "") {
      error.textContent = "API 地址：必填";
      error.hidden = false;
      baseUrlInput.focus();
      return;
    }

    busy = true;
    error.hidden = true;
    submit.disabled = true;
    cancel.disabled = true;

    void (async () => {
      try {
        const payload: ProviderDraft = {
          name: id,
          display_name:
            displayNameInput.value.trim() === "" ? undefined : displayNameInput.value.trim(),
          api: apiSelect.value,
          base_url: baseUrl,
        };
        const saved =
          current === null
            ? await mutate("POST", PROVIDERS_ENDPOINT, payload)
            : await mutate("PATCH", providerPath(current.name), payload);
        if (saved.kind !== "ok") {
          error.textContent = describeFailure(saved);
          error.hidden = false;
          return;
        }

        const secret = secretInput.value.trim();
        if (secret !== "") {
          const credential = await setCredential(credentialNameFor(id), secret);
          if (credential.kind !== "ok") {
            error.textContent = `provider 已保存，但凭据写入失败：\n${describeFailure(credential)}`;
            error.hidden = false;
            return;
          }
        }

        const synced = await syncModels(id, models);
        if (synced.kind !== "ok") {
          error.textContent = `provider 已保存，但模型目录同步失败：\n${describeFailure(synced)}`;
          error.hidden = false;
          return;
        }

        dialog.close();
        await options.onSaved();
      } catch (cause) {
        error.textContent = cause instanceof Error ? cause.message : String(cause);
        error.hidden = false;
      } finally {
        busy = false;
        submit.disabled = false;
        cancel.disabled = false;
      }
    })();
  });

  syncTabs();
  syncCredentialHint();
  renderModels();
  dialog.showModal();
  (editing ? displayNameInput : idInput).focus();
}

/** 把写接口的失败结果拍平成可读文本。 */
function describeFailure(result: {
  kind: string;
  message?: string;
  details?: ReadonlyArray<{ path: string; message: string }>;
}): string {
  if (result.kind === "unauthorized") {
    return "token 无效或已失效，请用启动日志里带 token 的地址重新打开。";
  }
  const lines = [result.message ?? "保存失败"];
  for (const detail of result.details ?? []) {
    lines.push(`${detail.path}: ${detail.message}`);
  }
  return lines.join("\n");
}
