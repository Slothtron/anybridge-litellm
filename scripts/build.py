#!/usr/bin/env python3
"""构建编排：**先 UI 再 native**。

`build.rs` 只校验 `apps/ui/dist`，不跑 npm；真正的顺序由这里保证。
两步都显式打印，任何一步失败都直接非零退出，绝不静默跳过。

用法：
    python3 scripts/build.py            # release 构建（要求 UI 产物非占位）
    python3 scripts/build.py --debug    # debug 构建（允许占位页）
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
UI = ROOT / "apps" / "ui"
UI_DIST = UI / "dist"


def run(cmd: list[str], cwd: Path) -> None:
    print(f"[build] $ {' '.join(cmd)}  (cwd={cwd.relative_to(ROOT)})", flush=True)
    subprocess.run(cmd, cwd=cwd, check=True)


def package_manager() -> list[str]:
    if shutil.which("pnpm") and (UI / "pnpm-lock.yaml").exists():
        return ["pnpm"]
    return ["npm"]


def build_ui() -> None:
    if not (UI / "package.json").exists():
        print("[build] 跳过 UI：apps/ui/package.json 不存在", flush=True)
        return

    pm = package_manager()
    if not (UI / "node_modules").exists():
        install = [*pm, "install"]
        if pm == ["npm"]:
            install += ["--no-audit", "--no-fund"]
        run(install, UI)
    run([*pm, "run", "build"], UI)

    index = UI_DIST / "index.html"
    if not index.is_file():
        raise SystemExit(f"[build] UI 构建后仍缺少 {index}，检查 vite 的 outDir 配置")
    print(f"[build] UI 产物就绪：{index.relative_to(ROOT)}", flush=True)


def build_native(debug: bool) -> None:
    cmd = ["cargo", "build"]
    if not debug:
        cmd.append("--release")
    run(cmd, ROOT)


def main(argv: list[str]) -> int:
    debug = "--debug" in argv[1:]
    build_ui()
    build_native(debug)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
