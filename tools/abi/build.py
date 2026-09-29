#!/usr/bin/env python3
"""Build the Linux-ABI conformance fixtures.

Fixtures are ordinary Rust programs built for `x86_64-unknown-linux-musl`
(static). They are *not* part of the OS build. If the musl target/toolchain is
unavailable the script reports what it could build and exits 0 — the bench then
marks those fixtures "unavailable" rather than failing CI on tooling.

Output: target/abi/fixtures/<name>.elf and a JSON map on stdout.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import busybox

ROOT = Path(__file__).resolve().parent.parent.parent
FIXTURES = ROOT / "tools" / "abi" / "fixtures"
TARGET = "x86_64-unknown-linux-musl"
OUT_DIR = ROOT / "target" / "abi" / "fixtures"
NAMES = [
    "hello",
    "alloc",
    "hashmap",
    "file",
    "time",
    "thread",
    "syncstress",
    "fsstress",
    "memstress",
    "procstress",
    "sigstress",
    "epollstress",
    "unixstress",
    "persist",
    "statxio",
]


def run(cmd: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True)


def ensure_target() -> bool:
    """Return True if the musl target is installed (adding it if needed)."""
    installed = run(["rustup", "target", "list", "--installed"])
    if TARGET in installed.stdout:
        return True
    added = run(["rustup", "target", "add", TARGET])
    if added.returncode != 0:
        print(f"warning: cannot add {TARGET}: {added.stderr.strip()}", file=sys.stderr)
        return False
    return True


def build_fixtures() -> dict[str, str]:
    """Build the Rust fixtures; an unbuildable host yields an empty map."""
    if not ensure_target():
        return {}
    build = run(
        [
            "cargo",
            "build",
            "--manifest-path",
            str(FIXTURES / "Cargo.toml"),
            "--target",
            TARGET,
            "--release",
        ]
    )
    if build.returncode != 0:
        print("warning: fixture build failed", file=sys.stderr)
        print(build.stderr[-2000:], file=sys.stderr)
        return {}
    release = FIXTURES / "target" / TARGET / "release"
    built: dict[str, str] = {}
    for name in NAMES:
        source = release / name
        if not source.is_file():
            continue
        dest = OUT_DIR / f"{name}.elf"
        dest.write_bytes(source.read_bytes())
        built[name] = str(dest)
    return built


def build_busybox() -> dict[str, str]:
    """Fetch/build the pinned BusyBox, or report it unavailable."""
    shell = busybox.ensure_busybox()
    if shell is None:
        print("warning: busybox unavailable", file=sys.stderr)
        return {}
    dest = OUT_DIR / "busybox.elf"
    dest.write_bytes(shell.read_bytes())
    return {"busybox": str(dest)}


def main() -> int:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    built = build_fixtures()
    built.update(build_busybox())
    print(json.dumps(built, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
