import "./app.css";
import {
  fetchState,
  modelPath,
  mutate,
  providerPath,
  type ModelDraft,
  type ModelEntry,
  type MutationResult,
  type ProviderEntry,
} from "./api";
import { confirmDialog, openForm, type FieldSpec, type FormValues } from "./form";
import { openProviderDialog } from "./provider-dialog";
import {
  clearError,
  el,
  mountShell,
  renderAbout,
  renderErrorState,
  renderLoading,
  renderState,
  setActiveView,
  setRefreshing,
  showError,
  type RowActions,
  type View,
} from "./render";

function text(value: string | string[] | undefined): string {
  return typeof value === "string" ? value.trim() : "";
}

function optionalText(value: string | string[] | undefined): string | undefined {
  const trimmed = text(value);
  return trimmed === "" ? undefined : trimmed;
}

function optionalNumber(value: string | string[] | undefined): number | undefined {
  const raw = text(value);
  if (raw === "") {
    return undefined;
  }
  const parsed = Number(raw);
  return Number.isFinite(parsed) ? parsed : undefined;
}

/** 逗号/空白分隔的列表；空列表等价于「未声明」。 */
function splitList(value: string | string[] | undefined): string[] | undefined {
  const raw = text(value);
  if (raw === "") {
    return undefined;
  }
  const items = raw
    .split(/[,，\s]+/)
    .map((item) => item.trim())
    .filter((item) => item !== "");
  return items.length === 0 ? undefined : items;
}

/** `null` = 成功；否则是可以直接展示的错误文本。 */
function failureMessage(result: MutationResult): string | null {
  if (result.kind === "ok") {
    return null;
  }
  if (result.kind === "unauthorized") {
    return "token 无效或已失效，请用启动日志里带 token 的地址重新打开。";
  }
  const lines = [result.message];
  for (const detail of result.details) {
    lines.push(`${detail.path}: ${detail.message}`);
  }
  return lines.join("\n");
}

const root = document.getElementById("app");
if (root === null) {
  throw new Error("缺少 #app 挂载点");
}

let requestId = 0;

/** 当前视图：Provider 列表 / 关于。切换不刷新页面，只重渲染。 */
let view: View = "providers";

const shell = mountShell(
  root,
  () => {
    void load();
  },
  (next) => {
    if (next === view) {
      return;
    }
    view = next;
    setActiveView(shell, view);
    void load();
  },
);
setActiveView(shell, view);

/** 「默认思考档位」输入框的候选来源，随模型对话框一起创建/销毁。 */
const THINKING_LEVELS_ID = "model-thinking-levels";

function openModelForm(provider: ProviderEntry, model: ModelEntry | null): void {
  const current = model;
  const editing = current !== null;
  const levels = current?.reasoning?.levels?.join(", ") ?? "";
  const supported = current?.reasoning?.supported;

  const fields: FieldSpec[] = [
    {
      name: "id",
      label: "模型 id",
      kind: "text",
      value: current?.id ?? "",
      required: true,
      wide: true,
      hint: "即请求体里的 model，必须与上游完全一致",
    },
    { name: "name", label: "展示名（可选）", kind: "text", value: current?.name ?? "" },
    {
      name: "context_window",
      label: "context_window",
      kind: "number",
      value: current?.context_window ?? "",
      min: 1,
    },
    {
      name: "max_output_tokens",
      label: "max_output_tokens",
      kind: "number",
      value: current?.max_output_tokens ?? "",
      min: 1,
      hint: "不能大于 context_window",
    },
    {
      name: "input",
      label: "输入模态",
      kind: "checkboxes",
      value: current?.input_modalities ?? [],
      options: [
        { value: "text", label: "文本" },
        { value: "image", label: "图片" },
      ],
    },
    {
      name: "reasoning",
      label: "reasoning",
      kind: "select",
      value: supported === true ? "true" : supported === false ? "false" : "",
      options: [
        { value: "true", label: "支持" },
        { value: "false", label: "不支持" },
      ],
    },
    {
      name: "thinking_levels",
      label: "thinking_levels（逗号分隔）",
      kind: "text",
      value: levels,
      placeholder: "none, low, medium, high",
      wide: true,
    },
    {
      name: "default_thinking_level",
      label: "默认思考档位",
      kind: "text",
      value: current?.reasoning?.default ?? "",
      datalist: THINKING_LEVELS_ID,
      hint: "必须是 thinking_levels 里的一个值",
    },
  ];

  const handle = openForm(
    {
      title: editing ? `编辑模型 · ${current.id}` : `新增模型 · ${provider.name}`,
      fields,
      submitLabel: editing ? "保存" : "创建",
    },
    async (values: FormValues) => {
      const reasoning = text(values.reasoning);
      const input = values.input;
      const payload: ModelDraft = {
        id: text(values.id),
        name: optionalText(values.name),
        context_window: optionalNumber(values.context_window),
        max_output_tokens: optionalNumber(values.max_output_tokens),
        input: Array.isArray(input) && input.length > 0 ? input : undefined,
        reasoning: reasoning === "" ? undefined : reasoning === "true",
        thinking_levels: splitList(values.thinking_levels),
        default_thinking_level: optionalText(values.default_thinking_level),
      };
      const result =
        current !== null
          ? await mutate("PATCH", modelPath(provider.name), {
              original_id: current.id,
              model: payload,
            })
          : await mutate("POST", modelPath(provider.name), payload);
      const failure = failureMessage(result);
      if (failure !== null) {
        return failure;
      }
      await load();
      return null;
    },
  );

  // 默认思考档位从 thinking_levels 派生候选项，同时保留手填。
  const list = el("datalist");
  list.id = THINKING_LEVELS_ID;
  handle.root.append(list);
  const levelsInput = handle.root.querySelector<HTMLInputElement>(
    'input[name="thinking_levels"]',
  );
  const syncLevels = (): void => {
    list.textContent = "";
    for (const level of splitList(levelsInput?.value) ?? []) {
      const option = el("option");
      option.value = level;
      list.append(option);
    }
  };
  levelsInput?.addEventListener("input", syncLevels);
  syncLevels();
}

async function removeProvider(provider: ProviderEntry): Promise<void> {
  const confirmed = await confirmDialog({
    title: "删除 provider",
    message: `确定删除 provider \`${provider.name}\`？\n该配置会立即从网关移除，正在进行的请求不受影响。`,
    confirmLabel: "删除",
    danger: true,
  });
  if (!confirmed) {
    return;
  }
  const result = await mutate("DELETE", providerPath(provider.name));
  const failure = failureMessage(result);
  if (failure !== null) {
    showError(shell, failure);
    return;
  }
  clearError(shell);
  await load();
}

async function removeModel(provider: ProviderEntry, model: ModelEntry): Promise<void> {
  const confirmed = await confirmDialog({
    title: "删除模型",
    message: `确定删除 provider \`${provider.name}\` 下的模型 \`${model.id}\`？`,
    confirmLabel: "删除",
    danger: true,
  });
  if (!confirmed) {
    return;
  }
  const result = await mutate("DELETE", modelPath(provider.name, model.id));
  const failure = failureMessage(result);
  if (failure !== null) {
    showError(shell, failure);
    return;
  }
  clearError(shell);
  await load();
}

const actions: RowActions = {
  editable: true,
  onCreateProvider: () => {
    openProviderDialog({ provider: null, onSaved: load });
  },
  onEditProvider: (provider) => {
    openProviderDialog({ provider, onSaved: load });
  },
  onDeleteProvider: (provider) => {
    void removeProvider(provider);
  },
  onCreateModel: (provider) => openModelForm(provider, null),
  onEditModel: (provider, model) => openModelForm(provider, model),
  onDeleteModel: (provider, model) => {
    void removeModel(provider, model);
  },
};

async function load(): Promise<void> {
  const current = ++requestId;

  clearError(shell);
  setRefreshing(shell, true);
  renderLoading(shell);

  const result = await fetchState();
  if (current !== requestId) {
    return;
  }

  setRefreshing(shell, false);

  if (result.kind === "ok") {
    actions.editable = result.state.editable !== false;
    if (view === "about") {
      renderAbout(shell, result.state);
    } else {
      renderState(shell, result.state, actions);
    }
    return;
  }

  if (result.kind === "unauthorized") {
    renderErrorState(shell, "未授权", "当前 token 无法读取控制台状态。");
    showError(shell, "token 无效或已失效");
    return;
  }

  renderErrorState(shell, "加载失败", result.message);
  showError(shell, result.message);
}

void load();
