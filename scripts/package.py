#!/usr/bin/env python3
"""把 release 二进制打成 tarball + sha256。

产物：dist/{binary}-{version}-{platform}-{arch}.tar.gz
      dist/{binary}-{version}-{platform}-{arch}.tar.gz.sha256

包名、二进制名、版本号全部从 `cargo metadata` 读，脚本里不手写项目名。
只用标准库；幂等（先删旧产物）。
"""

from __future__ import annotations

import hashlib
import json
import platform
import subprocess
import sys
import tarfile
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DIST = ROOT / "dist"


@dataclass(frozen=True)
class Target:
    """一个产出二进制的 Cargo 目标。"""

    package: str
    binary: str
    version: str


def release_target() -> Target:
    """找出 workspace 里唯一的 bin 目标（本仓只有一个可执行文件）。"""
    output = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout

    binaries: list[Target] = []
    for package in json.loads(output)["packages"]:
        for target in package.get("targets", []):
            if "bin" in target.get("kind", []):
                binaries.append(
                    Target(
                        package=package["name"],
                        binary=target["name"],
                        version=package["version"],
                    )
                )

    if len(binaries) != 1:
        raise SystemExit(
            f"期望恰好一个 bin 目标，实际 {len(binaries)} 个："
            f"{[item.binary for item in binaries]}"
        )
    return binaries[0]


def release_binary(binary: str) -> Path:
    suffix = ".exe" if sys.platform.startswith("win") else ""
    path = ROOT / "target" / "release" / f"{binary}{suffix}"
    if not path.is_file():
        raise SystemExit(f"找不到 release 二进制：{path}（先运行 cargo build --release）")
    return path


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> int:
    target = release_target()
    binary = release_binary(target.binary)

    platform_name = sys.platform
    arch = platform.machine()
    stem = f"{target.binary}-{target.version}-{platform_name}-{arch}"
    tarball = DIST / f"{stem}.tar.gz"
    checksum = DIST / f"{stem}.tar.gz.sha256"

    DIST.mkdir(parents=True, exist_ok=True)
    # 幂等：先删同名与同前缀的旧产物。
    for stale in DIST.glob(f"{target.binary}-*"):
        if stale.is_file():
            stale.unlink()

    with tarfile.open(tarball, "w:gz") as archive:
        archive.add(binary, arcname=binary.name)

    digest = sha256_of(tarball)
    checksum.write_text(f"{digest}  {tarball.name}\n", encoding="utf-8")

    print(tarball.relative_to(ROOT))
    print(checksum.relative_to(ROOT))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
