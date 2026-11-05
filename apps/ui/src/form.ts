/**
 * 表单对话框：daisyUI `modal` + 原生 `<dialog>`，无运行时框架依赖。
 *
 * 前端只做「空值 → 省略字段」的整理；字段级的合法性（api 枚举、provider 名、
 * 上限关系、api_key 必须是环境变量引用）全部由服务端校验，错误以
 * `path: message` 的形式回显在对话框里。
 *
 * 确认框也走同一套 `modal`，避免原生 `confirm()` 的样式与主题脱节。
 */

import { el } from "./render";

export type FieldKind = "text" | "number" | "select" | "checkboxes";

export interface SelectOption {
  value: string;
  label: string;
}

export interface FieldSpec {
  name: string;
  label: string;
  kind: FieldKind;
  value?: string | number | readonly string[];
  placeholder?: string;
  hint?: string;
  required?: boolean;
  readOnly?: boolean;
  options?: readonly SelectOption[];
  /** 文本输入框的 `<datalist>` id，用于「从已有值里挑，也允许手填」。 */
  datalist?: string;
  min?: number;
  wide?: boolean;
}

export interface FormSpec {
  title: string;
  description?: string;
  fields: readonly FieldSpec[];
  submitLabel: string;
}

export type FormValues = Record<string, string | string[]>;

export interface FormHandle {
  /** 对话框根节点：调用方可以往里挂 `<datalist>`，随对话框一起销毁。 */
  root: HTMLElement;
  close(): void;
}

/** 返回错误消息（`\n` 分隔）表示失败，返回 `null` 表示成功并关闭。 */
export type FormSubmit = (values: FormValues) => Promise<string | null>;

const LABEL_CLASS = "text-xs font-medium text-muted";
const HINT_CLASS = "m-0 text-xs leading-relaxed text-subtle";

let dialogSeq = 0;

function fieldControl(field: FieldSpec): HTMLElement {
  const controlId = `field-${field.name}`;

  if (field.kind === "select") {
    const select = el("select", "select w-full");
    select.id = controlId;
    select.name = field.name;
    if (field.required !== true) {
      const empty = el("option", undefined, "未声明");
      empty.value = "";
      select.append(empty);
    }
    for (const option of field.options ?? []) {
      const node = el("option", undefined, option.label);
      node.value = option.value;
      select.append(node);
    }
    select.value = typeof field.value === "string" ? field.value : "";
    return select;
  }

  if (field.kind === "checkboxes") {
    const group = el("div", "flex flex-wrap gap-4 py-1");
    const selected = Array.isArray(field.value) ? field.value : [];
    for (const option of field.options ?? []) {
      const label = el("label", "flex cursor-pointer items-center gap-1.5 text-sm");
      const input = el("input", "checkbox checkbox-sm");
      input.type = "checkbox";
      input.name = field.name;
      input.value = option.value;
      input.checked = selected.includes(option.value);
      label.append(input, document.createTextNode(option.label));
      group.append(label);
    }
    return group;
  }

  const input = el("input", "input w-full");
  input.id = controlId;
  input.type = field.kind === "number" ? "number" : "text";
  input.name = field.name;
  if (field.value !== undefined && !Array.isArray(field.value)) {
    input.value = String(field.value);
  }
  if (field.placeholder !== undefined) {
    input.placeholder = field.placeholder;
  }
  if (field.required === true) {
    input.required = true;
  }
  if (field.readOnly === true) {
    input.readOnly = true;
    input.classList.add("bg-base-200", "text-muted");
  }
  if (field.min !== undefined) {
    input.min = String(field.min);
  }
  if (field.datalist !== undefined) {
    input.setAttribute("list", field.datalist);
  }
  return input;
}

/**
 * 从任意容器收集字段值（不要求是 `<form>`）：手风琴等非 openForm 场景复用。
 * checkboxes 取所有勾选项，其余控件按 `name` 取第一个。
 */
export function collectValues(
  root: ParentNode,
  fields: readonly FieldSpec[],
): FormValues {
  const values: FormValues = {};
  for (const field of fields) {
    if (field.kind === "checkboxes") {
      values[field.name] = Array.from(
        root.querySelectorAll<HTMLInputElement>(`input[name="${field.name}"]:checked`),
      ).map((input) => input.value);
      continue;
    }
    const control = root.querySelector<HTMLInputElement | HTMLSelectElement>(
      `[name="${field.name}"]`,
    );
    if (control !== null) {
      values[field.name] = control.value;
    }
  }
  return values;
}

/**
 * 把一个字段渲染成「标签 + 控件 + 提示」的组。
 * 控件 id 固定为 `field-{name}`：同一容器内不要出现同名字段。
 */
export function renderFieldGroup(field: FieldSpec): HTMLElement {
  const wrapper = el(
    "div",
    field.wide === true
      ? "flex min-w-0 flex-col gap-1.5 sm:col-span-2"
      : "flex min-w-0 flex-col gap-1.5",
  );
  const label = el("label", LABEL_CLASS, field.label);
  if (field.kind !== "checkboxes") {
    label.htmlFor = `field-${field.name}`;
  }
  wrapper.append(label, fieldControl(field));
  if (field.hint !== undefined && field.hint !== "") {
    wrapper.append(el("p", HINT_CLASS, field.hint));
  }
  return wrapper;
}

function collect(form: HTMLFormElement, fields: readonly FieldSpec[]): FormValues {
  return collectValues(form, fields);
}

/** 生成「点击遮罩关闭」的 daisyUI 背景层。 */
export function modalBackdrop(): HTMLFormElement {
  const form = el("form", "modal-backdrop");
  form.method = "dialog";
  const close = el("button");
  // 遮罩按钮会铺满整屏，且不能成为 Tab 停靠点；读屏名统一成中文。
  close.textContent = "close";
  close.setAttribute("aria-label", "关闭");
  close.tabIndex = -1;
  form.append(close);
  return form;
}

export function openForm(spec: FormSpec, submit: FormSubmit): FormHandle {
  const dialog = el("dialog", "modal");
  // 没有 aria-labelledby 时，<dialog> 的可访问名会退化成整段正文。
  dialogSeq += 1;
  const titleId = `modal-title-${dialogSeq}`;
  dialog.setAttribute("aria-labelledby", titleId);

  const box = el("div", "modal-box w-11/12 max-w-2xl p-0");
  // 禁用原生校验气泡：错误统一回到对话框内的 alert，避免两套提示并存。
  const form = el("form", "flex max-h-[85vh] flex-col");
  form.noValidate = true;

  const header = el("header", "border-b border-base-300 px-6 py-4");
  const heading = el("h2", "text-lg font-semibold", spec.title);
  heading.id = titleId;
  header.append(heading);
  if (spec.description !== undefined && spec.description !== "") {
    header.append(el("p", "mt-1 text-xs text-muted", spec.description));
  }

  const grid = el(
    "div",
    "grid grid-cols-1 gap-x-4 gap-y-3.5 overflow-y-auto px-6 py-5 sm:grid-cols-2",
  );
  for (const field of spec.fields) {
    const wrapper = el(
      "div",
      field.wide === true
        ? "flex min-w-0 flex-col gap-1.5 sm:col-span-2"
        : "flex min-w-0 flex-col gap-1.5",
    );
    const label = el("label", LABEL_CLASS, field.label);
    if (field.kind !== "checkboxes") {
      label.htmlFor = `field-${field.name}`;
    }
    wrapper.append(label, fieldControl(field));
    if (field.hint !== undefined && field.hint !== "") {
      wrapper.append(el("p", HINT_CLASS, field.hint));
    }
    grid.append(wrapper);
  }

  const error = el(
    "p",
    "alert alert-error mx-6 mb-4 whitespace-pre-line text-xs",
  );
  error.setAttribute("role", "alert");
  error.hidden = true;

  const actions = el("div", "modal-action m-0 border-t border-base-300 px-6 py-4");
  const cancel = el("button", "btn btn-sm btn-ghost", "取消");
  cancel.type = "button";
  const confirm = el("button", "btn btn-sm btn-primary", spec.submitLabel);
  confirm.type = "submit";
  actions.append(cancel, confirm);

  form.append(header, grid, error, actions);
  box.append(form);
  dialog.append(box, modalBackdrop());
  document.body.append(dialog);
  dialog.showModal();

  const first = form.querySelector<HTMLElement>(
    "input:not([type=checkbox]):not([readonly]), select:not([disabled])",
  );
  first?.focus();

  let busy = false;
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    if (busy) {
      return;
    }
    const values = collect(form, spec.fields);

    // 客户端只拦「必填为空」，语义错误仍然交给服务端逐条回显。
    const missing: string[] = [];
    for (const field of spec.fields) {
      if (field.required !== true) {
        continue;
      }
      const value = values[field.name];
      const empty = Array.isArray(value) ? value.length === 0 : value.trim() === "";
      if (empty) {
        missing.push(`${field.label}：必填`);
      }
    }
    if (missing.length > 0) {
      error.textContent = missing.join("\n");
      error.hidden = false;
      const name = spec.fields.find((field) => {
        const value = values[field.name];
        return (
          field.required === true &&
          (Array.isArray(value) ? value.length === 0 : value.trim() === "")
        );
      })?.name;
      const target = name === undefined ? null : form.elements.namedItem(name);
      if (target instanceof HTMLElement) {
        target.focus();
      }
      return;
    }

    busy = true;
    error.hidden = true;
    confirm.disabled = true;
    cancel.disabled = true;

    void submit(values)
      .then((message) => {
        if (message === null) {
          dialog.close();
          return;
        }
        error.textContent = message;
        error.hidden = false;
      })
      .catch((cause: unknown) => {
        error.textContent = cause instanceof Error ? cause.message : String(cause);
        error.hidden = false;
      })
      .finally(() => {
        busy = false;
        confirm.disabled = false;
        cancel.disabled = false;
      });
  });

  cancel.addEventListener("click", () => dialog.close());
  dialog.addEventListener("close", () => dialog.remove());

  return {
    root: dialog,
    close: () => dialog.close(),
  };
}

export interface ConfirmSpec {
  title: string;
  message: string;
  confirmLabel?: string;
  /** 删除等不可逆操作使用 `btn-error`。 */
  danger?: boolean;
}

/** 主题化的确认框；返回 `true` 表示用户确认。 */
export function confirmDialog(spec: ConfirmSpec): Promise<boolean> {
  return new Promise<boolean>((resolve) => {
    const dialog = el("dialog", "modal");
    dialogSeq += 1;
    const titleId = `modal-title-${dialogSeq}`;
    dialog.setAttribute("aria-labelledby", titleId);
    const box = el("div", "modal-box max-w-md p-0");

    const body = el("div", "px-6 py-5");
    const heading = el("h2", "text-lg font-semibold", spec.title);
    heading.id = titleId;
    body.append(heading);
    body.append(
      el("p", "mt-2 whitespace-pre-line text-sm text-muted", spec.message),
    );

    const actions = el("div", "modal-action m-0 border-t border-base-300 px-6 py-4");
    const cancel = el("button", "btn btn-sm btn-ghost", "取消");
    cancel.type = "button";
    const confirm = el(
      "button",
      spec.danger === true ? "btn btn-sm btn-error" : "btn btn-sm btn-primary",
      spec.confirmLabel ?? "确认",
    );
    confirm.type = "button";
    actions.append(cancel, confirm);

    box.append(body, actions);
    dialog.append(box, modalBackdrop());
    document.body.append(dialog);

    let confirmed = false;
    cancel.addEventListener("click", () => dialog.close());
    confirm.addEventListener("click", () => {
      confirmed = true;
      dialog.close();
    });
    dialog.addEventListener("close", () => {
      dialog.remove();
      resolve(confirmed);
    });

    dialog.showModal();
    cancel.focus();
  });
}
