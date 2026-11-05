import "./app.css";
import {
  deleteKey,
  fetchState,
  modelPath,
  mutate,
  providerPath,
  rotateKey,
  type ApiKeyEntry,
  type ModelDraft,
  type ModelEntry,
  type MutationResult,
  type ProviderEntry,
} from "./api";
import { confirmDialog, openForm, type FormValues } from "./form";
import { openKeyDialog, openSecretDialog } from "./key-dialog";
import { draftFromValues, modelFieldSpecs, text } from "./model-fields";
import { openProviderDialog } from "./provider-dialog";
import {
  clearError,
  mountShell,
  renderAbout,
  renderApiKeys,
  renderErrorState,
  renderLoading,
  renderProviderDetail,
  renderState,
  setActiveView,
  setRefreshing,
  showError,
  type RowActions,
} from "./render";

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

/**
 * hash 路由：`#/` provider 列表、`#/provider/{id}` 详情、`#/keys` API 密钥、`#/about` 关于。
 * 刷新 / 前进后退都能落回原视图；未知 id 回落列表。
 */
type Route =
  | { view: "providers" | "keys" | "about" }
  | { view: "provider"; name: string };

function parseRoute(): Route {
  const match = /^#\/provider\/([^/?]+)$/.exec(location.hash);
  if (match !== null) {
    return { view: "provider", name: decodeURIComponent(match[1]) };
  }
  if (location.hash === "#/keys") {
    return { view: "keys" };
  }
  return { view: location.hash === "#/about" ? "about" : "providers" };
}

/** 路由 → 顶部导航高亮项。详情页仍归属 Provider。 */
function navView(view: Route["view"]): "providers" | "keys" | "about" {
  return view === "provider" ? "providers" : view;
}

let route: Route = parseRoute();

const shell = mountShell(
  root,
  () => {
    void load();
  },
  (next) => {
    // 导航按钮只改 hash，真正的重渲染统一由 hashchange 驱动。
    const target = next === "about" ? "#/about" : next === "keys" ? "#/keys" : "#/";
    if (location.hash === target) {
      void load();
      return;
    }
    location.hash = target;
  },
);
setActiveView(shell, navView(route.view));

window.addEventListener("hashchange", () => {
  route = parseRoute();
  setActiveView(shell, navView(route.view));
  void load();
});

function openModelForm(provider: ProviderEntry, model: ModelEntry | null): void {
  const current = model;
  const editing = current !== null;

  openForm(
    {
      title: editing ? `编辑模型 · ${current.id}` : `新增模型 · ${provider.name}`,
      fields: modelFieldSpecs(current ?? {}, { withId: true }),
      submitLabel: editing ? "保存" : "创建",
    },
    async (values: FormValues) => {
      const payload: ModelDraft = { id: text(values.id), ...draftFromValues(values) };
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

/** 批量删除：先确认一次，再逐条删除；任一条失败停在原地并回显。 */
async function removeModels(
  provider: ProviderEntry,
  models: ModelEntry[],
): Promise<void> {
  const confirmed = await confirmDialog({
    title: `删除 ${models.length} 个模型`,
    message: `确定删除 provider \`${provider.name}\` 下的 ${models.length} 个模型？\n${models
      .map((model) => `· ${model.id}`)
      .join("\n")}`,
    confirmLabel: "删除",
    danger: true,
  });
  if (!confirmed) {
    return;
  }
  for (const model of models) {
    const result = await mutate("DELETE", modelPath(provider.name, model.id));
    const failure = failureMessage(result);
    if (failure !== null) {
      showError(shell, failure);
      await load();
      return;
    }
  }
  clearError(shell);
  await load();
}

async function removeApiKey(key: ApiKeyEntry): Promise<void> {
  const confirmed = await confirmDialog({
    title: "删除 API 密钥",
    message: `确定删除 API 密钥 \`${key.name}\`？\n调用方持有的明文会立即失效。`,
    confirmLabel: "删除",
    danger: true,
  });
  if (!confirmed) {
    return;
  }
  const result = await deleteKey(key.name);
  if (result.kind !== "ok") {
    showError(
      shell,
      result.kind === "unauthorized" ? "登录态已失效，请刷新页面" : result.message,
    );
    return;
  }
  clearError(shell);
  await load();
}

async function rotateApiKey(key: ApiKeyEntry): Promise<void> {
  const confirmed = await confirmDialog({
    title: "轮换 API 密钥",
    message: `确定轮换 API 密钥 \`${key.name}\`？\n旧明文会立刻失效，新明文只显示一次。`,
    confirmLabel: "轮换",
    danger: true,
  });
  if (!confirmed) {
    return;
  }
  const result = await rotateKey(key.name);
  if (result.kind !== "ok") {
    showError(
      shell,
      result.kind === "unauthorized" ? "登录态已失效，请刷新页面" : result.message,
    );
    return;
  }
  clearError(shell);
  if (result.secret !== undefined) {
    openSecretDialog(result.secret);
  }
  await load();
}

/** provider id 列表：API 密钥对话框的 scope 多选数据源。 */
let providerNames: string[] = [];

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
  onBatchDeleteModels: (provider, models) => {
    void removeModels(provider, models);
  },
  onCreateKey: () => {
    openKeyDialog({ key: null, providers: providerNames, onSaved: load });
  },
  onEditKey: (key) => {
    openKeyDialog({ key, providers: providerNames, onSaved: load });
  },
  onRotateKey: (key) => {
    void rotateApiKey(key);
  },
  onDeleteKey: (key) => {
    void removeApiKey(key);
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

  if (result.kind !== "ok") {
    if (result.kind === "unauthorized") {
      renderErrorState(shell, "未授权", "当前 token 无法读取控制台状态。");
      showError(shell, "token 无效或已失效");
      return;
    }
    renderErrorState(shell, "加载失败", result.message);
    showError(shell, result.message);
    return;
  }

  actions.editable = result.state.editable !== false;
  providerNames = (result.state.providers ?? []).map((provider) => provider.name);
  // 删除 / 重命名可能让 hash 里的 provider 失效，每次渲染前重新解析。
  route = parseRoute();
  if (route.view === "about") {
    renderAbout(shell, result.state);
    return;
  }
  if (route.view === "keys") {
    renderApiKeys(shell, result.state, actions);
    return;
  }
  if (route.view === "provider") {
    const name = route.name;
    const provider = (result.state.providers ?? []).find(
      (item) => item.name === name,
    );
    if (provider !== undefined) {
      renderProviderDetail(shell, result.state, provider, actions);
      return;
    }
    // provider 已不存在：回落列表，且不产生新的历史记录。
    history.replaceState(null, "", "#/");
  }
  renderState(shell, result.state, actions);
}

void load();
