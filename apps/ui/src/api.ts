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

/** sessionStorage 中保存 console token 的键。 */
export const TOKEN_STORAGE_KEY = "anybridge.console.token";

/** 状态接口路径；相对于站点根，挂在 `/console/` 下。 */
const STATE_ENDPOINT = "/console/api/state";
/** provider 集合路径；新建 provider 时直接 POST。 */
export const PROVIDERS_ENDPOINT = "/console/api/providers";

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

/** `api_key` 的形态描述符。值本身永远不会离开服务端。 */
export interface ApiKeyInfo {
  kind: "env" | "literal" | "missing";
  /** `kind === "env"` 时的环境变量名。 */
  env?: string;
}

export interface ProviderEntry {
  name: string;
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
  providers?: ProviderEntry[];
}

/** 写接口载荷。`undefined` 字段会被 JSON 序列化丢弃。 */
export interface ProviderDraft {
  name: string;
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
export async function fetchState(token: string): Promise<FetchStateResult> {
  let response: Response;
  try {
    response = await fetch(STATE_ENDPOINT, {
      method: "GET",
      headers: {
        Accept: "application/json",
        "X-Console-Token": token,
      },
      // 控制台读的是磁盘上的最新配置，禁止任何缓存层返回旧状态。
      cache: "no-store",
      credentials: "omit",
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
  token: string,
  method: "POST" | "PATCH" | "DELETE",
  path: string,
  body?: unknown,
): Promise<MutationResult> {
  const headers: Record<string, string> = {
    Accept: "application/json",
    "X-Console-Token": token,
  };
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
      credentials: "omit",
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

export function providerPath(name: string): string {
  return `${PROVIDERS_ENDPOINT}/${encodeURIComponent(name)}`;
}

export function modelPath(provider: string, model?: string): string {
  const base = `${providerPath(provider)}/models`;
  return model === undefined ? base : `${base}?model=${encodeURIComponent(model)}`;
}
