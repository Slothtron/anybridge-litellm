/**
 * 对外 API key 的新建 / 编辑对话框，以及「只显示一次」的密钥展示。
 *
 * 提交走 `POST /console/api/keys` 或 `PATCH /console/api/keys/{name}`；
 * 创建 / 轮换的响应里带一次性明文，展示后不再回显（服务端只存 sha256 摘要）。
 */

import {
  createKey,
  updateKey,
  type ApiKeyEntry,
  type ApiKeyDraft,
} from "./api";
import { modalBackdrop } from "./form";
import { copyButton, el } from "./render";

export interface KeyDialogOptions {
  /** `null` 表示新建。 */
  key: ApiKeyEntry | null;
  /** 可选的 provider id（scope 多选）。 */
  providers: string[];
  /** 保存成功后的回调（通常是重新拉状态）。 */
  onSaved(secret?: string): void | Promise<void>;
}

/** 打开「新建 / 编辑 API 密钥」对话框；关掉即销毁。 */
export function openKeyDialog(options: KeyDialogOptions): void {
  const editing = options.key !== null;
  const current = options.key;
  let busy = false;

  const dialog = el("dialog", "modal");
  const titleId = "key-dialog-title";
  dialog.setAttribute("aria-labelledby", titleId);

  const box = el("div", "modal-box w-11/12 max-w-xl p-0");
  const form = el("form", "flex max-h-[85vh] flex-col");
  form.noValidate = true;

  const header = el("header", "border-b border-base-300 px-6 py-4");
  const heading = el(
    "h2",
    "text-lg font-semibold",
    editing ? `编辑 API 密钥 · ${current?.name ?? ""}` : "新增 API 密钥",
  );
  heading.id = titleId;
  header.append(heading);

  const body = el("div", "flex flex-col gap-4 overflow-y-auto px-6 py-5");
  body.append(
    el(
      "p",
      "m-0 text-xs leading-relaxed text-muted",
      "对外调用方出示的密钥：只能访问 scope 内的 provider。明文只在创建 / 轮换时显示一次。",
    ),
  );

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

  const nameInput = el("input", "input w-full font-mono");
  nameInput.type = "text";
  nameInput.id = "key-name";
  nameInput.placeholder = "alice";
  nameInput.value = current?.name ?? "";
  nameInput.readOnly = editing;
  if (editing) {
    nameInput.classList.add("bg-base-200", "text-muted");
  }

  const descriptionInput = el("input", "input w-full");
  descriptionInput.type = "text";
  descriptionInput.id = "key-description";
  descriptionInput.placeholder = "可选说明";
  descriptionInput.value = current?.description ?? "";

  // ---- scope ----
  const scopeField = el("fieldset", "flex flex-col gap-2");
  const scopeLegend = el("legend", "p-0 text-xs font-medium text-muted", "可访问的 provider");
  const allRow = el("label", "flex items-center gap-2 text-xs");
  const allToggle = el("input", "checkbox checkbox-sm");
  allToggle.type = "checkbox";
  allToggle.id = "key-scope-all";
  allRow.append(allToggle, document.createTextNode("全部 provider（*）"));

  const scopeList = el("div", "grid grid-cols-1 gap-1.5 pl-6 sm:grid-cols-2");
  const scopeBoxes = new Map<string, HTMLInputElement>();
  for (const provider of options.providers) {
    const row = el("label", "flex items-center gap-2 font-mono text-xs");
    const box = el("input", "checkbox checkbox-xs");
    box.type = "checkbox";
    box.value = provider;
    box.checked = current?.scope.includes(provider) === true;
    scopeBoxes.set(provider, box);
    row.append(box, document.createTextNode(provider));
    scopeList.append(row);
  }
  if (options.providers.length === 0) {
    scopeList.append(
      el("p", "m-0 text-xs text-subtle", "还没有 provider：先创建 provider 再配置 scope。"),
    );
  }
  allToggle.checked = current?.scope.includes("*") === true;
  allToggle.addEventListener("change", () => {
    for (const box of scopeBoxes.values()) {
      box.disabled = allToggle.checked;
    }
  });
  for (const box of scopeBoxes.values()) {
    box.disabled = allToggle.checked;
  }
  scopeField.append(scopeLegend, allRow, scopeList);

  // ---- 启停 ----
  const enabledRow = el("label", "flex items-center gap-2 text-xs");
  const enabledToggle = el("input", "toggle toggle-sm");
  enabledToggle.type = "checkbox";
  enabledToggle.id = "key-enabled";
  enabledToggle.checked = current?.enabled ?? true;
  enabledRow.append(enabledToggle, document.createTextNode("启用"));

  body.append(
    field("名称", nameInput, "用于识别调用方；创建后不可改名。"),
    field("说明（可选）", descriptionInput),
    scopeField,
    enabledRow,
  );

  const error = el("p", "alert alert-error mx-6 mb-4 whitespace-pre-line text-xs");
  error.setAttribute("role", "alert");
  error.hidden = true;

  const actions = el("div", "modal-action m-0 items-center border-t border-base-300 px-6 py-4");
  const cancel = el("button", "btn btn-sm btn-ghost", "取消");
  cancel.type = "button";
  const submit = el("button", "btn btn-sm btn-primary", editing ? "保存" : "创建");
  submit.type = "submit";
  actions.append(cancel, submit);

  form.append(header, body, error, actions);
  box.append(form);
  dialog.append(box, modalBackdrop());
  document.body.append(dialog);

  cancel.addEventListener("click", () => dialog.close());
  dialog.addEventListener("close", () => dialog.remove());

  function collectScope(): string[] {
    if (allToggle.checked) {
      return ["*"];
    }
    const scope: string[] = [];
    for (const [provider, box] of scopeBoxes) {
      if (box.checked) {
        scope.push(provider);
      }
    }
    return scope;
  }

  form.addEventListener("submit", (event) => {
    event.preventDefault();
    if (busy) {
      return;
    }
    const name = nameInput.value.trim();
    if (name === "") {
      error.textContent = "名称：必填";
      error.hidden = false;
      nameInput.focus();
      return;
    }
    const scope = collectScope();
    if (scope.length === 0) {
      error.textContent = "scope：至少选择一个 provider（或「全部 provider」）";
      error.hidden = false;
      return;
    }

    busy = true;
    error.hidden = true;
    submit.disabled = true;
    cancel.disabled = true;

    const draft: Omit<ApiKeyDraft, "name"> = {
      scope,
      description:
        descriptionInput.value.trim() === "" ? undefined : descriptionInput.value.trim(),
      enabled: enabledToggle.checked,
    };

    void (async () => {
      const result = editing
        ? await updateKey(current?.name ?? name, draft)
        : await createKey({ ...draft, name });
      busy = false;
      submit.disabled = false;
      cancel.disabled = false;
      if (result.kind !== "ok") {
        error.textContent =
          result.kind === "unauthorized"
            ? "登录态已失效，请刷新页面"
            : result.message;
        error.hidden = false;
        return;
      }
      dialog.close();
      if (result.secret !== undefined) {
        openSecretDialog(result.secret);
      }
      await options.onSaved(result.secret);
    })();
  });

  dialog.showModal();
}

/** 「只显示一次」的密钥展示；关闭后无法再次查看。 */
export function openSecretDialog(secret: string): void {
  const dialog = el("dialog", "modal");
  dialog.setAttribute("aria-labelledby", "secret-dialog-title");
  const box = el("div", "modal-box max-w-lg");

  const heading = el("h2", "text-lg font-semibold", "API 密钥（只显示一次）");
  heading.id = "secret-dialog-title";
  box.append(
    heading,
    el(
      "p",
      "mt-2 text-xs leading-relaxed text-muted",
      "请立刻复制保存。关闭后无法再次查看；遗失只能轮换。",
    ),
  );

  const codeRow = el("div", "mt-3 flex items-center gap-2 rounded-md bg-base-200 px-3 py-2");
  codeRow.append(
    el("code", "min-w-0 flex-1 break-all font-mono text-xs", secret),
    copyButton(secret, "API 密钥"),
  );
  box.append(codeRow);

  const actions = el("div", "modal-action");
  const close = el("button", "btn btn-sm btn-primary", "我已保存");
  close.type = "button";
  close.addEventListener("click", () => dialog.close());
  actions.append(close);
  box.append(actions);

  dialog.append(box, modalBackdrop());
  document.body.append(dialog);
  dialog.addEventListener("close", () => dialog.remove());
  dialog.showModal();
}
