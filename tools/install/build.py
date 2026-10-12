#!/usr/bin/env python3
"""Build `lazyos-install` for Linux (docs/grub-install-plan.md I2).

`lazyos-install` installs LazyOS beside Linux, behind its GRUB, from the USB
stick. It runs on the person's Linux, so it is a static musl program (no
glibc version to match) built from the OS workspace (`tools/install`), with
the toolchain's ``rust-lld`` on hosts without a C compiler, the recipe of
``tools/nettls/build.py``. ``LAZYOS_INSTALLER=1`` (``run_demo.py
--installer``) puts the result on the stick's FAT partition as
``install/lazyos-install`` with ``install/40_lazyos.in`` and
``install/README.txt``.

Usage::

    python tools/install/build.py             # target/install/lazyos-install
    python tools/install/build.py --require   # an unavailable target or a failed build exits 1
    python tools/install/build.py --debug

Prints a JSON map of what it built on stdout. Without the musl target it
warns and exits 0 (unless ``--require``); a compile error always exits 1.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
TARGET = "x86_64-unknown-linux-musl"
OUT_DIR = ROOT / "target" / "install"
BIN = "lazyos-install"
OUTPUT = OUT_DIR / BIN


def log(message: str) -> None:
    print(f"install: {message}", file=sys.stderr)


def run(cmd: list[str], env: dict[str, str] | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, env=env)


def ensure_target() -> bool:
    """True when the musl target is installed (adding it if needed)."""
    try:
        installed = run(["rustup", "target", "list", "--installed"])
    except FileNotFoundError:
        log("rustup not found; cannot check for the musl target")
        return False
    if TARGET in installed.stdout:
        return True
    added = run(["rustup", "target", "add", TARGET])
    if added.returncode != 0:
        log(f"cannot add {TARGET}: {added.stderr.strip()}")
        return False
    return True


def build_env() -> dict[str, str]:
    """The cargo environment: the bundled lld where there is no `cc`."""
    env = dict(os.environ)
    if shutil.which("cc") is None:
        env[f"CARGO_TARGET_{TARGET.upper().replace('-', '_')}_LINKER"] = "rust-lld"
        flags = "-C linker-flavor=ld.lld -C link-self-contained=yes"
        env["RUSTFLAGS"] = (env.get("RUSTFLAGS", "") + " " + flags).strip()
    return env


def no_linker(stderr: str) -> bool:
    """True when cargo failed only because no linker could be run."""
    lowered = stderr.lower()
    return "linker" in lowered and (
        "not found" in lowered or "could not exec" in lowered or "no such file" in lowered
    )


def build(debug: bool) -> Path | None:
    """`target/install/lazyos-install`, or None when the toolchain is
    unavailable. A compile error is fatal: a stick must never ship a stale
    installer."""
    if not ensure_target():
        return None
    cargo = ["cargo", "build", "-p", "lazyos-install", "--target", TARGET]
    if not debug:
        cargo.append("--release")
    log("building lazyos-install (static musl)")
    built = run(cargo, env=build_env())
    if built.returncode != 0:
        if no_linker(built.stderr):
            log("no linker for the musl target; lazyos-install unavailable")
            return None
        log("lazyos-install build failed")
        print(built.stderr[-4000:], file=sys.stderr)
        raise SystemExit(1)
    output = ROOT / "target" / TARGET / ("debug" if debug else "release") / BIN
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    shutil.copy(output, OUTPUT)  # keeps the executable bit for host runs
    log(f"{OUTPUT} ({OUTPUT.stat().st_size} bytes)")
    return OUTPUT


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--debug", action="store_true", help="build the debug profile")
    parser.add_argument("--require", action="store_true",
                        help="fail (exit 1) when the musl target or a linker is unavailable")
    args = parser.parse_args()
    built: dict[str, str] = {}
    output = build(args.debug)
    if output is not None:
        built[BIN] = str(output)
    print(json.dumps(built, indent=2))
    if args.require and output is None:
        log("unavailable: lazyos-install")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
