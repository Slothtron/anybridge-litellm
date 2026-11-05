/**
 * Heroicons（outline，24px 视图）内联 SVG 工厂。
 *
 * 只用 `currentColor` 描边，颜色完全交给宿主按钮/文字；
 * 一律 `aria-hidden`，可访问名由宿主按钮的 `aria-label` 提供。
 */

const SVG_NS = "http://www.w3.org/2000/svg";

export type IconName =
  | "trash"
  | "chevron-down"
  | "clipboard"
  | "check"
  | "arrow-left";

/** Heroicons v2 outline 的 `d` 属性（MIT）。 */
const ICON_PATHS: Record<IconName, string> = {
  trash:
    "M14.74 9l-.346 9m-4.788 0L9.26 9m9.968-3.21c.342.052.682.107 1.022.166m-1.022-.165L18.16 19.673a2.25 2.25 0 01-2.244 2.077H8.084a2.25 2.25 0 01-2.244-2.077L4.772 5.79m14.456 0a48.108 48.108 0 00-3.478-.397m-12 .562c.34-.059.68-.114 1.022-.165m0 0a48.11 48.11 0 013.478-.397m7.5 0v-.916c0-1.18-.91-2.164-2.09-2.201a51.964 51.964 0 00-3.32 0c-1.18.037-2.09 1.022-2.09 2.201v.916m7.5 0a48.667 48.667 0 00-7.5 0",
  "chevron-down": "M19.5 8.25l-7.5 7.5-7.5-7.5",
  clipboard:
    "M15.666 3.888A2.25 2.25 0 0013.5 2.25h-3c-1.03 0-1.9.693-2.166 1.638m7.332 0c.055.194.084.4.084.612v0a.75.75 0 01-.75.75H9a.75.75 0 01-.75-.75v0c0-.212.03-.418.084-.612m7.332 0c.646.049 1.288.11 1.927.184 1.1.128 1.907 1.077 1.907 2.185V19.5a2.25 2.25 0 01-2.25 2.25H6.75A2.25 2.25 0 014.5 19.5V6.257c0-1.108.806-2.057 1.907-2.185a48.208 48.208 0 011.927-.184",
  check: "M4.5 12.75l6 6 9-13.5",
  "arrow-left": "M10.5 19.5L3 12m0 0l7.5-7.5M3 12h18",
};

export function icon(name: IconName, className = "h-4 w-4"): SVGSVGElement {
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("fill", "none");
  svg.setAttribute("stroke", "currentColor");
  svg.setAttribute("stroke-width", "1.5");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("class", className);
  const path = document.createElementNS(SVG_NS, "path");
  path.setAttribute("stroke-linecap", "round");
  path.setAttribute("stroke-linejoin", "round");
  path.setAttribute("d", ICON_PATHS[name]);
  svg.append(path);
  return svg;
}
