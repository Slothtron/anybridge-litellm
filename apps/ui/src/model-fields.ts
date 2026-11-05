/**
 * 模型配置字段的唯一来源：8 个标准字段（id / 展示名 / 上下文窗口 / 最大输出 /
 * 输入模态 / 推理 / 思考档位 / 默认档位）的 FieldSpec、值↔草稿转换。
 *
 * 两个消费方：
 * - `main.ts` 的「新增/编辑模型」对话框（含 id 字段，提交走模型接口）
 * - `provider-dialog.ts` 目录行的手风琴（不含 id，只改本地草稿）
 */

import type { ModelDraft, ModelEntry, ReasoningInfo } from "./api";
import { el } from "./render";
import type { FieldSpec, FormValues } from "./form";

/** 「默认思考档位」输入框的候选来源 id；每个表单实例自带一个 datalist。 */
export const THINKING_LEVELS_ID = "model-thinking-levels";

/** 模型配置的统一输入：状态接口的 `ModelEntry` 与提交用的 `ModelDraft` 都能装下。 */
export interface ModelConfigValues {
  id?: string;
  name?: string;
  context_window?: number;
  max_output_tokens?: number;
  /** 草稿形状。 */
  input?: string[];
  /** 状态接口形状（`input_modalities`）。 */
  input_modalities?: string[];
  /** 草稿是 bool；状态接口是 `ReasoningInfo`。 */
  reasoning?: boolean | ReasoningInfo;
  /** 草稿形状。 */
  thinking_levels?: string[];
  default_thinking_level?: string;
}

export function text(value: string | string[] | undefined): string {
  return typeof value === "string" ? value.trim() : "";
}

export function optionalText(value: string | string[] | undefined): string | undefined {
  const trimmed = text(value);
  return trimmed === "" ? undefined : trimmed;
}

export function optionalNumber(value: string | string[] | undefined): number | undefined {
  const raw = text(value);
  if (raw === "") {
    return undefined;
  }
  const parsed = Number(raw);
  return Number.isFinite(parsed) ? parsed : undefined;
}

/** 逗号/空白分隔的列表；空列表等价于「未声明」。 */
export function splitList(value: string | string[] | undefined): string[] | undefined {
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

/** 生成模型配置的 FieldSpec；`withId` 时附带可编辑的模型 id（完整表单用）。 */
export function modelFieldSpecs(
  current: ModelConfigValues,
  options: { withId?: boolean; datalistId?: string } = {},
): FieldSpec[] {
  const { withId = false, datalistId = THINKING_LEVELS_ID } = options;
  const modalities = current.input ?? current.input_modalities;
  const reasoning = current.reasoning;
  const supported = typeof reasoning === "object" ? reasoning?.supported : reasoning;
  const levels = typeof reasoning === "object" ? reasoning?.levels : current.thinking_levels;
  const defaultLevel =
    typeof reasoning === "object" ? reasoning?.default : current.default_thinking_level;

  const fields: FieldSpec[] = [];
  if (withId) {
    fields.push({
      name: "id",
      label: "模型 id",
      kind: "text",
      value: current.id ?? "",
      required: true,
      wide: true,
      hint: "即请求体里的 model，必须与上游完全一致",
    });
  }
  fields.push(
    { name: "name", label: "展示名（可选）", kind: "text", value: current.name ?? "" },
    {
      name: "context_window",
      label: "context_window",
      kind: "number",
      value: current.context_window ?? "",
      min: 1,
    },
    {
      name: "max_output_tokens",
      label: "max_output_tokens",
      kind: "number",
      value: current.max_output_tokens ?? "",
      min: 1,
      hint: "不能大于 context_window",
    },
    {
      name: "input",
      label: "输入模态",
      kind: "checkboxes",
      value: modalities ?? [],
      options: [
        { value: "text", label: "文本" },
        { value: "image", label: "图片" },
      ],
    },
    {
      name: "reasoning",
      label: "推理等级",
      kind: "toggle",
      value: supported === true ? "true" : "false",
      wide: true,
      hint: "打开后由网关注入思考参数；关闭表示该模型不支持推理",
    },
    {
      name: "thinking_levels",
      label: "思考档位",
      kind: "levels",
      value: levels ?? [],
      wide: true,
      dependsOn: "reasoning",
    },
    {
      name: "default_thinking_level",
      label: "默认思考档位",
      kind: "text",
      value: defaultLevel ?? "",
      datalist: datalistId,
      dependsOn: "reasoning",
      hint: "必须是思考档位里的一个值",
    },
  );
  return fields;
}

/** 表单值 → 提交草稿（不含 id；id 由调用方按场景补）。 */
export function draftFromValues(values: FormValues): Omit<ModelDraft, "id"> {
  // 开关关闭 = 明确「不支持推理」，档位与默认档位一并清空。
  const reasoning = text(values.reasoning) === "true";
  const input = values.input;
  const levels = reasoning && Array.isArray(values.thinking_levels) ? values.thinking_levels : [];
  return {
    name: optionalText(values.name),
    context_window: optionalNumber(values.context_window),
    max_output_tokens: optionalNumber(values.max_output_tokens),
    input: Array.isArray(input) && input.length > 0 ? input : undefined,
    reasoning,
    thinking_levels: levels.length > 0 ? levels : undefined,
    default_thinking_level: reasoning ? optionalText(values.default_thinking_level) : undefined,
  };
}

/** 状态接口的模型条目 → 可编辑草稿（thinking_levels 藏在 `reasoning` 里）。 */
export function modelEntryToDraft(model: ModelEntry): ModelDraft {
  const reasoning = model.reasoning;
  return {
    id: model.id,
    name: model.name,
    context_window: model.context_window,
    max_output_tokens: model.max_output_tokens,
    input: model.input_modalities,
    // 服务端加载时已把「有档位即支持」展开成 true，这里容忍缺省。
    reasoning: reasoning === undefined ? undefined : (reasoning.supported ?? true),
    thinking_levels: reasoning?.levels,
    default_thinking_level: reasoning?.default,
  };
}

/** 草稿是否带 id 之外的任何配置；没有就不必为它发 PATCH。 */
export function hasModelConfig(draft: ModelDraft): boolean {
  return (
    draft.name !== undefined ||
    draft.context_window !== undefined ||
    draft.max_output_tokens !== undefined ||
    (draft.input !== undefined && draft.input.length > 0) ||
    draft.reasoning !== undefined ||
    (draft.thinking_levels !== undefined && draft.thinking_levels.length > 0) ||
    draft.default_thinking_level !== undefined
  );
}

/**
 * 在 `root` 里挂「默认思考档位」的 datalist，并随档位行输入实时派生候选。
 * 与 `modelFieldSpecs` 的 `datalistId` 必须一致。
 */
export function attachThinkingLevelsDatalist(
  root: HTMLElement,
  datalistId: string = THINKING_LEVELS_ID,
): void {
  const list = el("datalist");
  list.id = datalistId;
  root.append(list);
  const sync = (): void => {
    list.textContent = "";
    for (const input of root.querySelectorAll<HTMLInputElement>(
      'input[name="thinking_levels"]',
    )) {
      const value = input.value.trim();
      if (value === "") {
        continue;
      }
      const option = el("option");
      option.value = value;
      list.append(option);
    }
  };
  // 档位行是动态增删的，用事件委托覆盖新增行。
  root.addEventListener("input", sync);
  sync();
}
