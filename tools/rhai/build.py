#!/usr/bin/env python3
"""Build the `rhai` command for LazyOS (issue #319).

`rhai` is an ordinary Rust program built for ``x86_64-unknown-linux-musl``
(static, ``std``): it is *not* part of the OS workspace. The root ``build.rs``
embeds it in the disk image as ``/system/bin/rhai`` whenever ``target/rhai/rhai.elf``
exists (or ``LAZYOS_RHAI`` points at another build), and the kernel's Linux
loader resolves ``rhai`` typed at the ``sh`` prompt to it.

The recipe mirrors ``tools/xui/build.py``: on Windows the musl target has no
host linker, so cargo is pointed at the toolchain's bundled ``rust-lld`` with
self-contained linking; elsewhere the default linker is used. The environment
is set only for the cargo subprocess. Rhai is pure Rust, so no C compiler
(``musl-gcc``) is needed.

Usage::

    python tools/rhai/build.py
    python tools/rhai/build.py --debug

Output: ``target/rhai/rhai.elf`` plus a JSON map on stdout. If the musl target
cannot be installed the script says so and exits 0 with an empty map, so a
host without it (and CI's optional jobs) skips the run instead of failing; a
real compile error exits 1.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
APP = ROOT / "rhai-host"
TARGET = "x86_64-unknown-linux-musl"
OUT_DIR = ROOT / "target" / "rhai"
BIN = "rhai"
OUT_NAME = "rhai.elf"


def run(cmd: list[str], env: dict[str, str] | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, env=env)


def ensure_target() -> bool:
    """Return True if the musl target is installed (adding it if needed)."""
    try:
        installed = run(["rustup", "target", "list", "--installed"])
    except FileNotFoundError:
        print("warning: rustup not found; cannot check for the musl target", file=sys.stderr)
        return False
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


def no_linker(stderr: str) -> bool:
    """True when cargo failed only because no linker could be run."""
    lowered = stderr.lower()
    return "linker" in lowered and (
        "not found" in lowered or "could not exec" in lowered or "no such file" in lowered
    )


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--debug", action="store_true", help="build the debug profile")
    args = parser.parse_args()

    built: dict[str, str] = {}
    if not ensure_target():
        print(json.dumps(built))
        return 0

    command = ["cargo", "build", "--manifest-path", str(APP / "Cargo.toml"), "--target", TARGET]
    if not args.debug:
        command.append("--release")
    build = run(command, env=build_env())
    if build.returncode != 0:
        if no_linker(build.stderr):
            # Rust cannot link for musl on this host (no `cc`, no rust-lld):
            # the command is optional, so report it unavailable like a
            # missing target instead of failing the whole build.
            print("warning: no linker for the musl target; rhai unavailable", file=sys.stderr)
            print(json.dumps(built))
            return 0
        # A real compile error must fail CI instead of silently skipping.
        print("error: rhai build failed", file=sys.stderr)
        print(build.stderr[-2000:], file=sys.stderr)
        print(json.dumps(built))
        return 1

    source = APP / "target" / TARGET / ("debug" if args.debug else "release") / BIN
    if not source.is_file():
        print(f"error: {source} was not produced", file=sys.stderr)
        print(json.dumps(built))
        return 1
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    dest = OUT_DIR / OUT_NAME
    dest.write_bytes(source.read_bytes())
    built[BIN] = str(dest)
    print(json.dumps(built, indent=2))
    print(f"rhai: {dest} ({dest.stat().st_size} bytes)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
