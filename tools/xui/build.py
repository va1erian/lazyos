#!/usr/bin/env python3
"""Build the xui app for LazyOS (issue #114).

The app is an ordinary Rust program built for ``x86_64-unknown-linux-musl``
(static): it is *not* part of the OS workspace. It is embedded in the disk
image by the root ``build.rs`` when ``LAZYOS_XUI_APP`` points at one of the
outputs, and booted by the kernel with ``LAZYOS_XUID=1``.

On Windows the musl target has no host linker, so the script points cargo at
the toolchain's bundled ``rust-lld`` with self-contained linking (the same
recipe the Linux-ABI fixtures need); on other hosts the default toolchain
linker is used. The environment is set only for the cargo subprocess, so a
later LazyOS workspace build is unaffected.

Usage::

    python tools/xui/build.py
    python tools/xui/build.py --debug

Output: target/xui/xui-m0.elf, target/xui/xui-counter.elf,
target/xui/xui-sysmon.elf, target/xui/xui-fabricmon.elf and
target/xui/xui-client.elf, plus a JSON map on stdout. If the musl target or
toolchain is unavailable the script reports what it could build and exits 0,
so a CI job can skip the visual run.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
APP = ROOT / "xui-app"
TARGET = "x86_64-unknown-linux-musl"
OUT_DIR = ROOT / "target" / "xui"
BINS = {
    "xui-m0": "xui-m0.elf",
    "xui-counter": "xui-counter.elf",
    "xui-sysmon": "xui-sysmon.elf",
    "xui-fabricmon": "xui-fabricmon.elf",
    "xui-client": "xui-client.elf",
}


def run(cmd: list[str], env: dict[str, str] | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, env=env)


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


def build_env() -> dict[str, str]:
    """The cargo environment, with the bundled lld on hosts without a musl cc."""
    env = dict(os.environ)
    if os.name != "nt":
        return env
    sysroot = run(["rustc", "--print", "sysroot"]).stdout.strip()
    version = run(["rustc", "-vV"]).stdout
    host = next(
        (line.split(":", 1)[1].strip() for line in version.splitlines() if line.startswith("host:")),
        "",
    )
    linker = Path(sysroot) / "lib" / "rustlib" / host / "bin" / "rust-lld.exe"
    if linker.is_file():
        env.setdefault(f"CARGO_TARGET_{TARGET.upper().replace('-', '_')}_LINKER", str(linker))
        env.setdefault("RUSTFLAGS", "-C link-self-contained=yes")
    return env


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--debug", action="store_true", help="build the debug profile")
    args = parser.parse_args()

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    built: dict[str, str] = {}

    if not ensure_target():
        print(json.dumps(built))
        return 0

    profile = "debug" if args.debug else "release"
    command = [
        "cargo",
        "build",
        "--manifest-path",
        str(APP / "Cargo.toml"),
        "--target",
        TARGET,
    ]
    if not args.debug:
        command.append("--release")
    build = run(command, env=build_env())
    if build.returncode != 0:
        # Only a missing toolchain/target is a skip (handled above); a real
        # compile error must fail CI instead of silently skipping the run.
        print("error: xui app build failed", file=sys.stderr)
        print(build.stderr[-2000:], file=sys.stderr)
        print(json.dumps(built))
        return 1

    release = APP / "target" / TARGET / profile
    for name, disk_name in BINS.items():
        source = release / name
        if not source.is_file():
            continue
        dest = OUT_DIR / disk_name
        dest.write_bytes(source.read_bytes())
        built[name] = str(dest)

    print(json.dumps(built, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
