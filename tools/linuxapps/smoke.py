#!/usr/bin/env python3
"""Smoke-test the built Linux apps on real Linux, in an Alpine container.

LazyOS is the real target, but a run on Linux first separates "the binary is
broken" from "LazyOS lacks a syscall". Each program runs once in
``alpine:3.20`` with ``target/linuxapps/bin`` mounted at ``/w``; the output is
compared with what the command must print.

Usage::

    python tools/linuxapps/smoke.py [--only NAME ...] [--require]

Without a running Docker daemon it says so and exits 0 (1 with ``--require``).
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
BIN = ROOT / "target" / "linuxapps" / "bin"
IMAGE = "alpine:3.20"
PLATFORM = "linux/amd64"

#: name -> (shell command run inside the container, the line it must print).
CHECKS: dict[str, tuple[str, str]] = {
    "lua": ("/w/lua -e 'print(1+1)'", "2"),
    "sqlite3": ("/w/sqlite3 :memory: 'select 6*7'", "42"),
    "jq": ("echo '{\"a\":1}' | /w/jq .a", "1"),
    "dash": ("/w/dash -c 'echo $((2+3))'", "5"),
    "rg": ("/w/rg --version | head -n 1", "ripgrep 14.1.1"),
}


def docker_running() -> bool:
    try:
        done = subprocess.run(["docker", "info", "--format", "{{.ServerVersion}}"],
                              capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.SubprocessError):
        return False
    return done.returncode == 0 and done.stdout.strip() != ""


def run_check(name: str) -> tuple[bool, str]:
    """Run `name`'s check; (passed, what it printed)."""
    command, expected = CHECKS[name]
    if not (BIN / name).is_file():
        return False, "not built"
    docker = ["docker", "run", "--rm", "--platform", PLATFORM,
              "-v", f"{BIN}:/w:ro", IMAGE, "sh", "-c", command]
    done = subprocess.run(docker, capture_output=True, text=True, timeout=300)
    output = (done.stdout + done.stderr).strip()
    first = output.splitlines()[0].strip() if output else ""
    passed = done.returncode == 0 and first.startswith(expected)
    return passed, output


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--only", nargs="+", choices=sorted(CHECKS), metavar="NAME")
    parser.add_argument("--require", action="store_true",
                        help="fail when Docker is unavailable")
    args = parser.parse_args()
    if not docker_running():
        print("smoke: no running Docker daemon; nothing tested", file=sys.stderr)
        return 1 if args.require else 0
    failed = 0
    for name in args.only or list(CHECKS):
        passed, output = run_check(name)
        failed += not passed
        print(f"{'PASS' if passed else 'FAIL'} {name}: {output}")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
