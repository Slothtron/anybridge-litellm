#!/usr/bin/env python3
"""把 release 二进制打成 tarball + sha256。

产物：dist/anybridge-litellm-{version}-{platform}-{arch}.tar.gz
      dist/anybridge-litellm-{version}-{platform}-{arch}.tar.gz.sha256

只用标准库；幂等（先删旧产物）。
"""

from __future__ import annotations

import hashlib
import json
import platform
import subprocess
import sys
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DIST = ROOT / "dist"
PACKAGE = "anybridge-litellm-cli"
BINARY = "anybridge-litellm"


def workspace_version() -> str:
    output = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    for package in json.loads(output)["packages"]:
        if package["name"] == PACKAGE:
            return package["version"]
    raise SystemExit(f"cargo metadata 里找不到 {PACKAGE}")


def release_binary() -> Path:
    suffix = ".exe" if sys.platform.startswith("win") else ""
    path = ROOT / "target" / "release" / f"{BINARY}{suffix}"
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
    version = workspace_version()
    binary = release_binary()

    platform_name = sys.platform
    arch = platform.machine()
    stem = f"{BINARY}-{version}-{platform_name}-{arch}"
    tarball = DIST / f"{stem}.tar.gz"
    checksum = DIST / f"{stem}.tar.gz.sha256"

    DIST.mkdir(parents=True, exist_ok=True)
    # 幂等：先删同名与同前缀的旧产物。
    for stale in DIST.glob(f"{BINARY}-*"):
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
