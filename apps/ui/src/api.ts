/**
 * 控制台数据访问层。
 *
 * - 读：`GET /console/api/state`
 * - 写：`POST|PATCH|DELETE /console/api/providers[/...]`，统一返回最新状态。
 *
 * 所有写请求都带 `Content-Type: application/json`（服务端据此挡跨站简单表单），
 * 响应里永远没有上游 `api_key` 的值，只有 `{kind, env?}` 描述符。
 * 契约见仓库设计方案 §3.5。
 */

/** 状态接口路径；相对于站点根，挂在 `/console/` 下。 */
const STATE_ENDPOINT = "/console/api/state";
/** provider 集合路径；新建 provider 时直接 POST。 */
export const PROVIDERS_ENDPOINT = "/console/api/providers";
/** 凭据集合路径；`PUT /{name}` 写入，`DELETE /{name}` 删除。 */
export const CREDENTIALS_ENDPOINT = "/console/api/credentials";

export interface ReasoningInfo {
  supported?: boolean;
  /** 由 provider 的 api 推导：reasoning_effort / thinking / reasoning.effort */
  param?: string;
  levels?: string[];
  default?: string;
}

export interface ModelEntry {
  id: string;
  name?: string;
  context_window?: number;
  max_output_tokens?: number;
  /** ["text"] / ["text", "image"] */
  input_modalities?: string[];
  reasoning?: ReasoningInfo;
}

/**
 * `api_key` 的凭据描述符。值本身永远不会离开服务端。
 *
 * 配置里只存 `credential/<name>` 引用，密钥值由后端的凭据存储解析；
 * 这里只给「是否已配置 / 来自哪一层 / 能不能写」。
 */
export interface ApiKeyInfo {
  kind: "credential";
  /** 凭据名（不带前缀）。 */
  name?: string;
  /** 完整引用，形如 `credential/WPSAI_API_KEY`。 */
  reference?: string;
  configured?: boolean;
  /** 提供值的来源层；本地实现固定是 `file`。 */
  source?: string | null;
  writable?: boolean;
}

/** 凭据写入结果。 */
export interface CredentialState {
  name: string;
  reference: string;
  configured: boolean;
  source?: string | null;
  writable: boolean;
}

export interface ProviderEntry {
  /** provider id（配置键、路径段）。 */
  name: string;
  /** 可选展示名；为空时界面回退显示 id。 */
  display_name?: string | null;
  api?: string;
  base_path?: string;
  base_url?: string;
  timeout?: number;
  /** 服务端 `None` 会序列化成 `null`，两者都要容忍。 */
  api_version?: string | null;
  api_key?: ApiKeyInfo;
  models?: ModelEntry[];
}

export interface ConsoleState {
  name?: string;
  version?: string;
  bind?: string;
  prefix?: string;
  /** 服务是否以 `--config` 启动；false 时写接口返回 409。 */
  editable?: boolean;
  /** 配置文件路径；只读模式下为 null。 */
  config_file?: string | null;
  /** 凭据文件路径；只读模式下为 null。 */
  credentials_file?: string | null;
  providers?: ProviderEntry[];
}

/** 写接口载荷。`undefined` 字段会被 JSON 序列化丢弃。 */
export interface ProviderDraft {
  /** provider id（配置键、路径段）。 */
  name: string;
  /** 可选展示名；为空时界面回退显示 id。 */
  display_name?: string;
  /** 凭据引用；服务端按 `upper(id)_API_KEY` 派生，这里传了也只会被校验语法。 */
  api: string;
  base_url: string;
  /** 只接受 `os.environ/VAR`；空表示更新时保留原值。 */
  api_key?: string;
  timeout?: number;
  api_version?: string;
}

export interface ModelDraft {
  id: string;
  name?: string;
  context_window?: number;
  max_output_tokens?: number;
  input?: string[];
  reasoning?: boolean;
  thinking_levels?: string[];
  default_thinking_level?: string;
}

export interface ErrorDetail {
  path: string;
  message: string;
}

export type FetchStateResult =
  | { kind: "ok"; state: ConsoleState }
  | { kind: "unauthorized" }
  | { kind: "error"; message: string };

export type MutationResult =
  | { kind: "ok"; state: ConsoleState }
  | { kind: "unauthorized" }
  | { kind: "error"; message: string; details: ErrorDetail[] };

function extractErrorMessage(body: unknown, status: number): string {
  if (typeof body === "object" && body !== null) {
    const error = (body as { error?: unknown }).error;
    if (typeof error === "object" && error !== null) {
      const message = (error as { message?: unknown }).message;
      if (typeof message === "string" && message.trim() !== "") {
        return message;
      }
    }
    const message = (body as { message?: unknown }).message;
    if (typeof message === "string" && message.trim() !== "") {
      return message;
    }
  }
  return `请求失败（HTTP ${status}）`;
}

function extractDetails(body: unknown): ErrorDetail[] {
  if (typeof body !== "object" || body === null) {
    return [];
  }
  const error = (body as { error?: unknown }).error;
  if (typeof error !== "object" || error === null) {
    return [];
  }
  const details = (error as { details?: unknown }).details;
  if (!Array.isArray(details)) {
    return [];
  }
  return details.flatMap((detail): ErrorDetail[] => {
    if (typeof detail !== "object" || detail === null) {
      return [];
    }
    const path = (detail as { path?: unknown }).path;
    const message = (detail as { message?: unknown }).message;
    if (typeof path !== "string" || typeof message !== "string") {
      return [];
    }
    return [{ path, message }];
  });
}

async function readJson(response: Response): Promise<unknown> {
  try {
    return (await response.json()) as unknown;
  } catch {
    return null;
  }
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * 拉取控制台状态。
 *
 * - 200 且是对象 → `{ kind: "ok", state }`
 * - 401 → `{ kind: "unauthorized" }`
 * - 其它状态码 / 网络错误 / 非对象响应 → `{ kind: "error", message }`
 */
export async function fetchState(): Promise<FetchStateResult> {
  let response: Response;
  try {
    response = await fetch(STATE_ENDPOINT, {
      method: "GET",
      headers: { Accept: "application/json" },
      // 控制台读的是磁盘上的最新配置，禁止任何缓存层返回旧状态。
      cache: "no-store",
      // 会话 cookie 由浏览器自动带上（HttpOnly，JS 读不到）。
      credentials: "same-origin",
    });
  } catch (cause) {
    const detail = cause instanceof Error ? cause.message : String(cause);
    return { kind: "error", message: `无法连接网关：${detail}` };
  }

  if (response.status === 401) {
    return { kind: "unauthorized" };
  }

  if (response.status !== 200) {
    const body = await readJson(response);
    return { kind: "error", message: extractErrorMessage(body, response.status) };
  }

  const body = await readJson(response);
  if (!isObject(body)) {
    return { kind: "error", message: "响应格式不正确：期望 JSON 对象" };
  }

  return { kind: "ok", state: body as ConsoleState };
}

/**
 * 发一次写请求。所有写接口都返回最新状态，调用方无需再 GET。
 *
 * `body` 为 `undefined` 时不发请求体（DELETE）。
 */
export async function mutate(
  method: "POST" | "PUT" | "PATCH" | "DELETE",
  path: string,
  body?: unknown,
): Promise<MutationResult> {
  const headers: Record<string, string> = { Accept: "application/json" };
  if (body !== undefined) {
    headers["Content-Type"] = "application/json";
  }

  let response: Response;
  try {
    response = await fetch(path, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      cache: "no-store",
      credentials: "same-origin",
    });
  } catch (cause) {
    const detail = cause instanceof Error ? cause.message : String(cause);
    return { kind: "error", message: `无法连接网关：${detail}`, details: [] };
  }

  if (response.status === 401) {
    return { kind: "unauthorized" };
  }

  const payload = await readJson(response);
  if (response.status !== 200) {
    return {
      kind: "error",
      message: extractErrorMessage(payload, response.status),
      details: extractDetails(payload),
    };
  }
  if (!isObject(payload)) {
    return { kind: "error", message: "响应格式不正确：期望 JSON 对象", details: [] };
  }
  return { kind: "ok", state: payload as ConsoleState };
}

/**
 * 上游 `/models` 的一个条目（已归一化）。服务端会按别名映射标准字段
 * （context_window / max_output_tokens / input_modalities / reasoning），
 * 取不到的字段直接省略，前端不做推断。
 */
export interface UpstreamModel {
  id: string;
  name?: string | null;
  context_window?: number;
  max_output_tokens?: number;
  input_modalities?: string[];
  reasoning?: boolean;
}

export type UpstreamModelsResult =
  | { kind: "ok"; models: UpstreamModel[] }
  | { kind: "error"; message: string };

/** 带上该 provider 的凭据向上游要一份模型列表；不落盘。 */
export async function fetchUpstreamModels(
  provider: string,
): Promise<UpstreamModelsResult> {
  let response: Response;
  try {
    response = await fetch(
      `${PROVIDERS_ENDPOINT}/${encodeURIComponent(provider)}/upstream-models`,
      {
        method: "GET",
        headers: { Accept: "application/json" },
        cache: "no-store",
        credentials: "same-origin",
      },
    );
  } catch (cause) {
    const detail = cause instanceof Error ? cause.message : String(cause);
    return { kind: "error", message: `无法连接网关：${detail}` };
  }
  return parseUpstreamModels(response);
}

async function postUpstreamModels(
  path: string,
  payload: unknown,
): Promise<UpstreamModelsResult> {
  let response: Response;
  try {
    response = await fetch(path, {
      method: "POST",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
      },
      body: JSON.stringify(payload),
      cache: "no-store",
      credentials: "same-origin",
    });
  } catch (cause) {
    const detail = cause instanceof Error ? cause.message : String(cause);
    return { kind: "error", message: `无法连接网关：${detail}` };
  }
  return parseUpstreamModels(response);
}

async function parseUpstreamModels(
  response: Response,
): Promise<UpstreamModelsResult> {
  const body = await readJson(response);
  if (response.status !== 200) {
    return {
      kind: "error",
      message: extractErrorMessage(body, response.status),
    };
  }
  if (!isObject(body) || !Array.isArray(body.models)) {
    return { kind: "error", message: "响应格式不正确：期望 { models: [] }" };
  }
  const models: UpstreamModel[] = [];
  for (const item of body.models) {
    if (!isObject(item)) {
      continue;
    }
    const id = item.id;
    if (typeof id !== "string" || id.trim() === "") {
      continue;
    }
    const model: UpstreamModel = {
      id,
      name: typeof item.name === "string" ? item.name : null,
    };
    if (typeof item.context_window === "number") {
      model.context_window = item.context_window;
    }
    if (typeof item.max_output_tokens === "number") {
      model.max_output_tokens = item.max_output_tokens;
    }
    if (
      Array.isArray(item.input_modalities) &&
      item.input_modalities.every((kind) => typeof kind === "string")
    ) {
      model.input_modalities = item.input_modalities as string[];
    }
    if (typeof item.reasoning === "boolean") {
      model.reasoning = item.reasoning;
    }
    models.push(model);
  }
  return { kind: "ok", models };
}

/** 还没建 provider 时，用表单里的地址与密钥直接探一次上游模型列表。 */
export async function probeUpstreamModels(probe: {
    base_url: string;
    api: string;
    api_version?: string;
    api_key: string;
  },
): Promise<UpstreamModelsResult> {
  return postUpstreamModels("/console/api/upstream-models", probe);
}

/** 用整份 id 列表同步模型目录（只增删差异，保留已有条目的字段）。 */
export async function syncModels(
  provider: string,
  ids: readonly string[],
): Promise<MutationResult> {
  return mutate(
    "PUT",
    `${PROVIDERS_ENDPOINT}/${encodeURIComponent(provider)}/models`,
    { ids },
  );
}

/** 批量导入上游模型（已存在的 id 由服务端跳过）。 */
export async function importModels(
  provider: string,
  ids: readonly string[],
): Promise<MutationResult> {
  return mutate(
    "POST",
    `${PROVIDERS_ENDPOINT}/${encodeURIComponent(provider)}/models/import`,
    { ids },
  );
}

/** 写入/轮换一条凭据。值是单向下行：响应里永远不会回显。 */
export async function setCredential(
  name: string,
  value: string,
): Promise<MutationResult> {
  return mutate("PUT", `${CREDENTIALS_ENDPOINT}/${encodeURIComponent(name)}`, {
    value,
  });
}

/** 删除一条凭据。 */
export async function deleteCredential(
  name: string,
): Promise<MutationResult> {
  return mutate("DELETE", `${CREDENTIALS_ENDPOINT}/${encodeURIComponent(name)}`);
}

export function providerPath(name: string): string {
  return `${PROVIDERS_ENDPOINT}/${encodeURIComponent(name)}`;
}

export function modelPath(provider: string, model?: string): string {
  const base = `${providerPath(provider)}/models`;
  return model === undefined ? base : `${base}?model=${encodeURIComponent(model)}`;
}
