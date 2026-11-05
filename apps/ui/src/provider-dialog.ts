/**
 * 新增 / 编辑 provider 的对话框。
 *
 * 单列表单（无预设）+ 内嵌「模型目录」。目录条目是本地草稿：
 * 行内 icon 按钮做「配置（手风琴展开标准字段）/ 移除」，「获取可用模型」
 * 拉到上游列表后弹窗勾选导入，上游返回的标准字段直接预填进草稿。
 *
 * 提交是四步串行，任何一步失败都停在原地并回显原因：
 * 1. `POST|PATCH /console/api/providers`（id、展示名、地址、协议）
 * 2. `PUT /console/api/credentials/{name}`（只写密钥值；留空表示不修改）
 * 3. `PUT /console/api/providers/{id}/models`（整份 id 列表同步目录，新 provider
 *    由此创建 id-only 条目）
 * 4. 对每个带配置的草稿 `PATCH /console/api/providers/{id}/models`
 *    （`{original_id, model}`；同步后条目必存在）
 *
 * 凭据名由 provider id 派生（`upper(id)` + `_API_KEY`），表单里只展示、不可编辑。
 */

import {
  PROVIDERS_ENDPOINT,
  fetchUpstreamModels,
  modelPath,
  mutate,
  probeUpstreamModels,
  providerPath,
  setCredential,
  syncModels,
  type ModelDraft,
  type ProviderDraft,
  type ProviderEntry,
  type ProviderHeader,
  type UpstreamModel,
} from "./api";
import { collectValues, modalBackdrop, renderFieldGroups } from "./form";
import {
  draftFromValues,
  hasModelConfig,
  modelEntryToDraft,
  modelFieldSpecs,
} from "./model-fields";
import { el, iconButton } from "./render";

const API_OPTIONS = [
  { value: "completions", label: "OpenAI Chat Completions" },
  { value: "anthropic", label: "Anthropic Messages" },
  { value: "responses", label: "OpenAI Responses" },
];

const MODALITY_LABELS: Record<string, string> = {
  text: "文本",
  image: "图片",
};

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

/** 打开对话框；不返回句柄，关掉即销毁。 */
export function openProviderDialog(options: ProviderDialogOptions): void {
  const editing = options.provider !== null;
  const current = options.provider;
  /** 目录草稿：本地编辑，保存时按 §头部注释的 3→4 步落盘。 */
  let drafts: ModelDraft[] = (current?.models ?? []).map(modelEntryToDraft);
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

  // ---- 主体：单列表单 ----
  const body = el("div", "flex flex-col gap-4 overflow-y-auto px-6 py-5");

  const description = el(
    "p",
    "m-0 text-xs leading-relaxed text-muted",
    "连接中转站、自部署服务或其他兼容 OpenAI / Anthropic 协议的接口，需填写 API 地址、协议和模型。",
  );

  const panel = el("div", "flex flex-col gap-4");

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

  // 「使用 API 密钥」开关：关闭时提交 `api_key: ""`，由自定义请求头鉴权。
  let useApiKey = current?.api_key !== null;
  const apiKeyToggle = el("input", "toggle toggle-sm");
  apiKeyToggle.type = "checkbox";
  apiKeyToggle.id = "provider-use-api-key";
  apiKeyToggle.checked = useApiKey;
  const apiKeyControl = el("div", "flex flex-col gap-2");
  const apiKeyToggleRow = el("label", "flex items-center gap-2 text-xs text-muted");
  apiKeyToggleRow.append(apiKeyToggle, document.createTextNode("使用 API 密钥"));
  apiKeyControl.append(apiKeyToggleRow, secretInput);
  const apiKeyField = field("API 密钥", apiKeyControl, "");

  // 自定义请求头：有序行（名称 + 值），值可为 `credential/<name>` 引用。
  const headersSection = el("section", "flex flex-col gap-2");
  headersSection.append(
    el("span", "text-xs font-medium text-base-content", "自定义请求头"),
    el(
      "p",
      "m-0 text-xs leading-relaxed text-subtle",
      "值以 credential/<name> 开头时从凭据文件解析（适合 Cookie 等机密），否则按字面量发送。",
    ),
  );
  const headerRows = el("div", "flex flex-col gap-1.5");
  const addHeader = el("button", "btn btn-xs btn-ghost self-start", "+ 添加请求头");
  addHeader.type = "button";
  headersSection.append(headerRows, addHeader);

  let headerDrafts: ProviderHeader[] = (current?.headers ?? []).map((header) => ({
    name: header.name,
    value: header.value,
  }));

  function renderHeaders(): void {
    headerRows.textContent = "";
    headerDrafts.forEach((header, index) => {
      const row = el("div", "flex items-center gap-1.5");
      const name = el("input", "input input-sm w-40 shrink-0 font-mono");
      name.placeholder = "Header-Name";
      name.value = header.name;
      name.setAttribute("aria-label", `请求头 ${index + 1} 名称`);
      name.addEventListener("input", () => {
        headerDrafts[index].name = name.value;
      });
      const value = el("input", "input input-sm w-full font-mono");
      value.placeholder = "credential/WPS_SID 或字面量";
      value.value = header.value;
      value.setAttribute("aria-label", `请求头 ${index + 1} 的值`);
      value.addEventListener("input", () => {
        headerDrafts[index].value = value.value;
      });
      row.append(
        name,
        value,
        iconButton(
          "trash",
          `移除请求头 ${index + 1}`,
          () => {
            headerDrafts.splice(index, 1);
            renderHeaders();
          },
          "text-error",
        ),
      );
      headerRows.append(row);
    });
  }

  function collectHeaders(): ProviderHeader[] {
    return headerDrafts
      .map((header) => ({ name: header.name.trim(), value: header.value.trim() }))
      .filter((header) => header.name !== "" && header.value !== "");
  }

  addHeader.addEventListener("click", () => {
    headerDrafts = [...headerDrafts, { name: "", value: "" }];
    renderHeaders();
  });
  renderHeaders();

  panel.append(
    field("Provider ID", idInput, "以小写字母开头的标识，在请求中唯一标识该提供商，并用于派生凭据名。"),
    field("显示名称", displayNameInput),
    field("API 地址", baseUrlInput),
    field("API 协议", apiSelect),
    apiKeyField,
    credentialHint,
    headersSection,
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

  // 批量管理：勾选行 → 全选/清空/批量移除。
  const catalogTools = el("div", "flex flex-wrap items-center gap-1.5");
  const batchCount = el("span", "text-xs text-muted", "已选 0 项");
  const selectAll = el("button", "btn btn-xs btn-ghost", "全选");
  selectAll.type = "button";
  const clearSelection = el("button", "btn btn-xs btn-ghost", "清空");
  clearSelection.type = "button";
  const batchRemove = el("button", "btn btn-xs btn-ghost text-error", "批量移除");
  batchRemove.type = "button";
  catalogTools.append(batchCount, selectAll, clearSelection, batchRemove);

  const addRow = el("div", "flex gap-1.5");
  const addInput = el("input", "input input-sm w-full");
  addInput.type = "text";
  addInput.placeholder = "手动添加模型 ID";
  addInput.setAttribute("aria-label", "手动添加模型 ID");
  const addButton = el("button", "btn btn-sm btn-ghost shrink-0", "+ 添加模型");
  addButton.type = "button";
  addRow.append(addInput, addButton);

  catalog.append(catalogHead, catalogTools, catalogList, catalogEmpty, addRow);

  body.append(description, panel, catalog);

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

  function syncCredentialHint(): void {
    if (!apiKeyToggle.checked) {
      credentialHint.textContent =
        "未使用 API 密钥：请通过「自定义请求头」完成鉴权（例如 Cookie: credential/WPS_SID）。";
      return;
    }
    const name = credentialNameFor(idInput.value);
    const reference = `credential/${name}`;
    credentialHint.textContent =
      current?.api_key?.configured === true
        ? `当前凭据已配置（${reference}，源：${current.api_key.source ?? "file"}）；留空表示不修改`
        : `配置里只写引用 ${reference}；值写入独立的凭据文件（0600），保存后不再回显`;
  }

  secretInput.hidden = !useApiKey;
  apiKeyToggle.addEventListener("change", () => {
    secretInput.hidden = !apiKeyToggle.checked;
    syncCredentialHint();
  });

  // ---- 目录行：icon 按钮 + 手风琴 ----
  let openIndex: number | null = null;
  let openPanel: HTMLElement | null = null;

  /** 1000 以上折叠成 K/M，徽章里放得下；精确值在手风琴里可见。 */
  function compactNumber(value: number): string {
    const trim = (n: number): string => String(Math.round(n * 10) / 10);
    if (value >= 1_000_000) {
      return `${trim(value / 1_000_000)}M`;
    }
    if (value >= 1_000) {
      return `${trim(value / 1_000)}K`;
    }
    return String(value);
  }

  function summaryBadges(draft: ModelDraft): HTMLElement {
    const wrap = el("div", "flex flex-wrap items-center gap-1");
    const badge = (text: string, title: string): HTMLElement => {
      const node = el("span", "badge badge-xs badge-ghost font-mono", text);
      node.title = title;
      return node;
    };
    if (typeof draft.context_window === "number") {
      wrap.append(badge(compactNumber(draft.context_window), "上下文窗口"));
    }
    if (typeof draft.max_output_tokens === "number") {
      wrap.append(badge(compactNumber(draft.max_output_tokens), "最大输出"));
    }
    for (const modality of draft.input ?? []) {
      wrap.append(badge(MODALITY_LABELS[modality] ?? modality, "输入模态"));
    }
    if (draft.reasoning === true) {
      wrap.append(badge("推理", "支持推理"));
    }
    return wrap;
  }

  /** 把当前展开的手风琴值合回草稿；提交前与收起前都调用。 */
  function syncOpenAccordion(): void {
    if (openIndex === null || openPanel === null) {
      return;
    }
    const index = openIndex;
    const fields = modelFieldSpecs(drafts[index]);
    drafts[index] = { ...drafts[index], ...draftFromValues(collectValues(openPanel, fields)) };
  }

  function closeAccordion(): void {
    if (openIndex === null || openPanel === null) {
      return;
    }
    syncOpenAccordion();
    openPanel.remove();
    openPanel = null;
    openIndex = null;
    renderModels();
  }

  function toggleAccordion(index: number): void {
    if (openIndex === index) {
      closeAccordion();
      return;
    }
    closeAccordion();
    const fields = modelFieldSpecs(drafts[index]);
    const panelNode = el(
      "div",
      "mt-2 grid grid-cols-1 gap-x-4 gap-y-3 border-t border-base-300 pt-3 sm:grid-cols-2",
    );
    renderFieldGroups(panelNode, fields);
    // 输入即合回草稿：即使不收起直接保存，值也不会丢。
    const liveSync = (): void => {
      if (openIndex === index && openPanel === panelNode) {
        drafts[index] = { ...drafts[index], ...draftFromValues(collectValues(panelNode, fields)) };
        refreshBadges(index);
      }
    };
    panelNode.addEventListener("input", liveSync);
    panelNode.addEventListener("change", liveSync);

    const container = catalogList.children[index];
    if (container instanceof HTMLElement) {
      container.append(panelNode);
    }
    openIndex = index;
    openPanel = panelNode;

    const button = container?.querySelector<HTMLButtonElement>('button[aria-label^="配置模型"]');
    button?.setAttribute("aria-expanded", "true");
    button?.querySelector("svg")?.classList.add("rotate-180");
  }

  function refreshBadges(index: number): void {
    const container = catalogList.children[index];
    const badges = container?.querySelector('[data-role="summary"]');
    if (badges instanceof HTMLElement) {
      badges.replaceChildren(...summaryBadges(drafts[index]).childNodes);
    }
  }

  /** 目录勾选：批量移除用。 */
  const selected = new Set<string>();

  function renderModels(): void {
    openIndex = null;
    openPanel = null;
    catalogList.textContent = "";
    for (const id of [...selected]) {
      if (!drafts.some((draft) => draft.id === id)) {
        selected.delete(id);
      }
    }
    drafts.forEach((draft, index) => {
      const container = el("div", "flex flex-col rounded-md bg-base-100 px-2 py-1.5");
      const line = el("div", "flex items-center gap-2");
      const checkbox = el("input", "checkbox checkbox-xs shrink-0");
      checkbox.type = "checkbox";
      checkbox.checked = selected.has(draft.id);
      checkbox.setAttribute("aria-label", `选择模型 ${draft.id}`);
      checkbox.addEventListener("change", () => {
        if (checkbox.checked) {
          selected.add(draft.id);
        } else {
          selected.delete(draft.id);
        }
        syncBatch();
      });
      line.append(checkbox);
      line.append(el("code", "min-w-0 flex-1 truncate font-mono text-xs", draft.id));
      const badges = summaryBadges(draft);
      badges.setAttribute("data-role", "summary");
      line.append(badges);
      line.append(
        iconButton("chevron-down", `配置模型 ${draft.id}`, () => toggleAccordion(index)),
      );
      line.append(
        iconButton("trash", `移除模型 ${draft.id}`, () => {
          selected.delete(draft.id);
          drafts.splice(index, 1);
          renderModels();
        }, "text-error"),
      );
      container.append(line);
      catalogList.append(container);
    });
    catalogEmpty.hidden = drafts.length > 0;
    syncBatch();
  }

  function syncBatch(): void {
    batchCount.textContent = `已选 ${selected.size} 项`;
    batchRemove.disabled = selected.size === 0;
    selectAll.disabled = drafts.length === 0 || selected.size === drafts.length;
    clearSelection.disabled = selected.size === 0;
  }

  selectAll.addEventListener("click", () => {
    for (const draft of drafts) {
      selected.add(draft.id);
    }
    renderModels();
  });
  clearSelection.addEventListener("click", () => {
    selected.clear();
    renderModels();
  });
  batchRemove.addEventListener("click", () => {
    drafts = drafts.filter((draft) => !selected.has(draft.id));
    selected.clear();
    renderModels();
  });

  function addDraft(draft: ModelDraft): void {
    if (draft.id.trim() === "" || drafts.some((item) => item.id === draft.id)) {
      return;
    }
    drafts = [...drafts, draft];
    renderModels();
  }

  addButton.addEventListener("click", () => {
    addDraft({ id: addInput.value.trim() });
    addInput.value = "";
    addInput.focus();
  });
  addInput.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      addDraft({ id: addInput.value.trim() });
      addInput.value = "";
    }
  });

  // ---- 获取可用模型：弹窗勾选导入 ----
  /** 上游条目 → 目录草稿（标准字段直接预填）。 */
  function upstreamToDraft(model: UpstreamModel): ModelDraft {
    return {
      id: model.id,
      name: model.name ?? undefined,
      context_window: model.context_window,
      max_output_tokens: model.max_output_tokens,
      input: model.input_modalities,
      // 上游给了档位即支持推理（服务端归一化已保证 reasoning 与档位一致）。
      reasoning: model.reasoning ?? (model.thinking_levels?.length ? true : undefined),
      thinking_levels: model.thinking_levels,
    };
  }

  /** 用上游字段刷新已有草稿：上游给了值就覆盖，没给就保留原配置。 */
  function mergeUpstreamDraft(existing: ModelDraft, incoming: ModelDraft): ModelDraft {
    const merged: ModelDraft = { ...existing };
    for (const key of [
      "name",
      "context_window",
      "max_output_tokens",
      "input",
      "reasoning",
      "thinking_levels",
    ] as const) {
      if (incoming[key] !== undefined) {
        Object.assign(merged, { [key]: incoming[key] });
      }
    }
    return merged;
  }

  function openImportPicker(
    models: readonly UpstreamModel[],
    onConfirm: (selected: UpstreamModel[]) => void,
  ): void {
    const picker = el("dialog", "modal");
    const pickerTitleId = "import-picker-title";
    picker.setAttribute("aria-labelledby", pickerTitleId);
    const pickerBox = el("div", "modal-box w-11/12 max-w-xl p-0");
    const pickerForm = el("form", "flex max-h-[70vh] flex-col");
    pickerForm.noValidate = true;

    const pickerHeader = el("header", "border-b border-base-300 px-6 py-4");
    const pickerHeading = el("h2", "text-lg font-semibold", "选择要导入的模型");
    pickerHeading.id = pickerTitleId;
    const pickerToolbar = el("div", "mt-2 flex items-center gap-2");
    const counter = el(
      "span",
      "ml-auto text-xs text-muted",
      "",
    );
    pickerHeader.append(pickerHeading, pickerToolbar);
    pickerToolbar.append(counter);

    const pickerList = el("div", "flex flex-col gap-1 overflow-y-auto px-6 py-4");
    const checkboxes: HTMLInputElement[] = [];
    for (const model of models) {
      const row = el("label", "flex cursor-pointer items-center gap-2.5 rounded-md px-2 py-1.5 hover:bg-base-200");
      const checkbox = el("input", "checkbox checkbox-sm shrink-0");
      checkbox.type = "checkbox";
      checkbox.name = "import-model";
      checkbox.value = model.id;
      // 默认全选：确认即导入新条目并按上游刷新已有条目的标准字段。
      checkbox.checked = true;
      const id = el("code", "min-w-0 truncate font-mono text-xs", model.id);
      row.append(checkbox, id);
      if (typeof model.name === "string" && model.name !== "") {
        row.append(el("span", "min-w-0 truncate text-xs text-muted", model.name));
      }
      const preview = el("span", "ml-auto flex shrink-0 items-center gap-1");
      if (typeof model.context_window === "number") {
        const node = el("span", "badge badge-xs badge-ghost font-mono", compactNumber(model.context_window));
        node.title = "上下文窗口";
        preview.append(node);
      }
      if (model.reasoning === true) {
        const node = el("span", "badge badge-xs badge-ghost", "推理");
        node.title = "支持推理";
        preview.append(node);
      }
      if (preview.childElementCount > 0) {
        row.append(preview);
      }
      checkboxes.push(checkbox);
      pickerList.append(row);
    }

    const syncCounter = (): void => {
      const selected = checkboxes.filter((box) => box.checked).length;
      counter.textContent = `已选 ${selected} / ${checkboxes.length}`;
      confirmButton.disabled = selected === 0;
    };

    const selectAll = el("button", "btn btn-xs btn-ghost", "全选");
    selectAll.type = "button";
    selectAll.addEventListener("click", () => {
      for (const box of checkboxes) {
        box.checked = true;
      }
      syncCounter();
    });
    const clearAll = el("button", "btn btn-xs btn-ghost", "清空");
    clearAll.type = "button";
    clearAll.addEventListener("click", () => {
      for (const box of checkboxes) {
        box.checked = false;
      }
      syncCounter();
    });
    pickerToolbar.append(selectAll, clearAll);

    const pickerActions = el("div", "modal-action m-0 items-center border-t border-base-300 px-6 py-4");
    const pickerCancel = el("button", "btn btn-sm btn-ghost", "取消");
    pickerCancel.type = "button";
    const confirmButton = el("button", "btn btn-sm btn-primary", "导入所选");
    confirmButton.type = "button";
    pickerActions.append(pickerCancel, confirmButton);

    pickerForm.append(pickerHeader, pickerList, pickerActions);
    pickerBox.append(pickerForm);
    picker.append(pickerBox, modalBackdrop());
    document.body.append(picker);

    pickerCancel.addEventListener("click", () => picker.close());
    confirmButton.addEventListener("click", () => {
      const selected = models.filter(
        (_, index) => checkboxes[index]?.checked === true,
      );
      picker.close();
      onConfirm(selected);
    });
    picker.addEventListener("close", () => picker.remove());
    pickerList.addEventListener("change", syncCounter);

    syncCounter();
    picker.showModal();
  }

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
              api_key: apiKeyToggle.checked ? secretInput.value.trim() : "",
              headers: collectHeaders(),
            });
        if (result.kind === "error") {
          error.textContent = result.message;
          error.hidden = false;
          return;
        }
        error.hidden = true;
        if (result.models.length === 0) {
          error.textContent = "上游没有返回任何模型";
          error.hidden = false;
          return;
        }
        openImportPicker(result.models, (selected) => {
          for (const model of selected) {
            const incoming = upstreamToDraft(model);
            const index = drafts.findIndex((draft) => draft.id === incoming.id);
            if (index === -1) {
              drafts = [...drafts, incoming];
            } else {
              // 已存在的条目用上游字段刷新（上游给了什么就覆盖什么），
              // 这样「获取可用模型」也能给早期导入的 id-only 条目补上标准字段。
              drafts[index] = mergeUpstreamDraft(drafts[index], incoming);
            }
          }
          renderModels();
        });
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
        // 手风琴还开着时先把值收回来。
        syncOpenAccordion();

        const payload: ProviderDraft = {
          name: id,
          display_name:
            displayNameInput.value.trim() === "" ? undefined : displayNameInput.value.trim(),
          api: apiSelect.value,
          base_url: baseUrl,
          // 关闭开关 = 明确「不使用 API 密钥」，服务端会删除该键。
          api_key: apiKeyToggle.checked ? undefined : "",
          headers: collectHeaders(),
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

        const secret = apiKeyToggle.checked ? secretInput.value.trim() : "";
        if (secret !== "") {
          const credential = await setCredential(credentialNameFor(id), secret);
          if (credential.kind !== "ok") {
            error.textContent = `provider 已保存，但凭据写入失败：\n${describeFailure(credential)}`;
            error.hidden = false;
            return;
          }
        }

        const synced = await syncModels(id, drafts.map((draft) => draft.id));
        if (synced.kind !== "ok") {
          error.textContent = `provider 已保存，但模型目录同步失败：\n${describeFailure(synced)}`;
          error.hidden = false;
          return;
        }

        for (const draft of drafts) {
          if (!hasModelConfig(draft)) {
            continue;
          }
          // 空数组等价「未声明」，避免服务端把空 input 当显式配置。
          const payload: ModelDraft = {
            ...draft,
            input: draft.input && draft.input.length > 0 ? draft.input : undefined,
          };
          const patched = await mutate("PATCH", modelPath(id), {
            original_id: draft.id,
            model: payload,
          });
          if (patched.kind !== "ok") {
            error.textContent = `模型目录已同步，但模型 ${draft.id} 的配置写入失败：\n${describeFailure(patched)}`;
            error.hidden = false;
            return;
          }
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
