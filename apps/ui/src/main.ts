import "./app.css";
import {
  PROVIDERS_ENDPOINT,
  TOKEN_STORAGE_KEY,
  fetchState,
  modelPath,
  mutate,
  providerPath,
  type ModelDraft,
  type ModelEntry,
  type MutationResult,
  type ProviderDraft,
  type ProviderEntry,
} from "./api";
import { confirmDialog, openForm, type FieldSpec, type FormValues } from "./form";
import {
  clearError,
  mountShell,
  renderErrorState,
  renderLoading,
  renderMissingToken,
  renderState,
  setRefreshing,
  showError,
  type RowActions,
} from "./render";

/**
 * 读取 token：优先 URL 查询参数，其次 sessionStorage。
 * URL 中的 token 会被写入 sessionStorage 并从地址栏移除，避免落进浏览历史。
 * 不使用 localStorage。
 */
function readToken(): string | null {
  const location = window.location;
  let token: string | null = null;

  try {
    const params = new URLSearchParams(location.search);
    const fromUrl = params.get("token");
    if (fromUrl !== null && fromUrl.trim() !== "") {
      token = fromUrl.trim();
      try {
        sessionStorage.setItem(TOKEN_STORAGE_KEY, token);
      } catch {
        // 隐私模式等场景下 sessionStorage 可能不可用，忽略即可。
      }
      params.delete("token");
      const search = params.toString();
      const cleaned = `${location.pathname}${search === "" ? "" : `?${search}`}${location.hash}`;
      window.history.replaceState(null, "", cleaned);
    }
  } catch {
    // URLSearchParams 构造失败时退回 sessionStorage。
  }

  if (token === null) {
    try {
      token = sessionStorage.getItem(TOKEN_STORAGE_KEY);
    } catch {
      token = null;
    }
  }

  if (token === null) {
    return null;
  }
  const trimmed = token.trim();
  return trimmed === "" ? null : trimmed;
}

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

const token = readToken();
let requestId = 0;

const shell = mountShell(root, () => {
  void load();
});

function openProviderForm(provider: ProviderEntry | null): void {
  const current = provider;
  const editing = current !== null;
  const keyHint =
    current?.api_key?.kind === "env" && typeof current.api_key.env === "string"
      ? `当前：os.environ/${current.api_key.env}（留空保持原值）`
      : "只接受 os.environ/VAR（或裸变量名）；明文密钥不会写进配置文件";

  const fields: FieldSpec[] = [
    {
      name: "name",
      label: "provider 名",
      kind: "text",
      value: current?.name ?? "",
      required: !editing,
      readOnly: editing,
      hint: editing
        ? "改名请删除后重建"
        : "小写字母开头，只含小写字母 / 数字 / 连字符，最长 32 字符",
    },
    {
      name: "api",
      label: "api",
      kind: "select",
      value: current?.api ?? "completions",
      required: true,
      options: [
        { value: "completions", label: "completions（OpenAI Chat）" },
        { value: "anthropic", label: "anthropic（Messages）" },
        { value: "responses", label: "responses（OpenAI Responses）" },
      ],
    },
    {
      name: "base_url",
      label: "base_url",
      kind: "text",
      value: current?.base_url ?? "",
      required: true,
      placeholder: "https://api.openai.com/v1",
      wide: true,
    },
    {
      name: "api_key",
      label: "api_key（环境变量）",
      kind: "text",
      placeholder: "os.environ/OPENAI_API_KEY",
      hint: keyHint,
      wide: true,
    },
    {
      name: "timeout",
      label: "timeout（秒）",
      kind: "number",
      value: current?.timeout ?? "",
      min: 1,
      placeholder: "600",
    },
    {
      name: "api_version",
      label: "api_version（可选）",
      kind: "text",
      value: current?.api_version ?? "",
      placeholder: "2023-06-01",
    },
  ];

  openForm(
    {
      title: editing ? `编辑 provider · ${current.name}` : "新增 provider",
      fields,
      submitLabel: editing ? "保存" : "创建",
    },
    async (values: FormValues) => {
      const payload: ProviderDraft = {
        name: text(values.name),
        api: text(values.api),
        base_url: text(values.base_url),
        api_key: optionalText(values.api_key),
        timeout: optionalNumber(values.timeout),
        api_version: optionalText(values.api_version),
      };
      const result =
        current !== null
          ? await mutate(token ?? "", "PATCH", providerPath(current.name), payload)
          : await mutate(token ?? "", "POST", PROVIDERS_ENDPOINT, payload);
      const failure = failureMessage(result);
      if (failure !== null) {
        return failure;
      }
      await load();
      return null;
    },
  );
}

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
    },
  ];

  openForm(
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
          ? await mutate(token ?? "", "PATCH", modelPath(provider.name), {
              original_id: current.id,
              model: payload,
            })
          : await mutate(token ?? "", "POST", modelPath(provider.name), payload);
      const failure = failureMessage(result);
      if (failure !== null) {
        return failure;
      }
      await load();
      return null;
    },
  );
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
  const result = await mutate(token ?? "", "DELETE", providerPath(provider.name));
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
  const result = await mutate(token ?? "", "DELETE", modelPath(provider.name, model.id));
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
  onCreateProvider: () => openProviderForm(null),
  onEditProvider: (provider) => openProviderForm(provider),
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

  if (token === null) {
    clearError(shell);
    renderMissingToken(shell);
    setRefreshing(shell, false);
    return;
  }

  clearError(shell);
  setRefreshing(shell, true);
  renderLoading(shell);

  const result = await fetchState(token);
  if (current !== requestId) {
    return;
  }

  setRefreshing(shell, false);

  if (result.kind === "ok") {
    actions.editable = result.state.editable !== false;
    renderState(shell, result.state, actions);
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
