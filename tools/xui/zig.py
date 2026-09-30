"""Zig as the C/C++ toolchain for the Docs app (`xui-docs`).

litehtml, the HTML engine behind Docs, is C++. The other xui apps are pure Rust
and link with the toolchain's bundled lld, but a static
``x86_64-unknown-linux-musl`` binary that contains C++ needs a musl C++
compiler, a C++ standard library and a linker for that target. Windows has none
of them and a stock Linux runner only has glibc ones. ``zig c++`` is a clang
driver that ships all three (musl, libc++, libunwind), so the same recipe
builds the app on a Windows host and on a Linux CI runner without a sysroot.

Rust still compiles every Rust crate; zig only compiles the C/C++ objects
(``cc-rs``) and performs the final link. This module finds zig, writes the tiny
compiler wrappers ``cc-rs`` and rustc need (they take a single executable, not
``zig cc -target ...``), and returns the cargo environment.

The pinned version is :data:`ZIG_VERSION`; install it with
``pip install ziglang==0.16.0`` (a wheel bundling zig, on both Windows and
Linux), put ``zig`` on ``PATH``, or point ``LAZYOS_ZIG`` at an executable.
"""

from __future__ import annotations

import os
import shlex
import shutil
import stat
import subprocess
import sys
from pathlib import Path

ZIG_VERSION = "0.16.0"
INSTALL_HINT = f"pip install ziglang=={ZIG_VERSION}  (or put zig on PATH, or set LAZYOS_ZIG)"

# The zig spelling of the Rust target `x86_64-unknown-linux-musl`.
ZIG_TARGET = "x86_64-linux-musl"

# Code generation flags for the C/C++ objects. `CRATE_CC_NO_DEFAULTS=1` (set in
# `cargo_env`) stops cc-rs adding its own `--target=` (which zig rejects) and
# `-O` flags, so they are stated here: `-fPIC` because the binary is
# static-PIE (LazyOS's loader takes position-independent executables linked at
# address 0), `-Os` to keep the image small, `-w` because libc++'s headers warn
# under clang's nullability checks.
C_FLAGS = "-Os -w -fPIC -ffunction-sections -fdata-sections"

# rustc must not add its own musl start files (`link-self-contained=no`): zig
# supplies crt1/libc, and both together define `_start` twice. `-pie` makes zig
# emit a static-PIE, which it does not do from rustc's `-static-pie` alone.
RUSTFLAGS = "-C link-self-contained=no -C link-arg=-pie"


def _probe(command: list[str]) -> str | None:
    """The version `command version` prints, or None when it does not run."""
    try:
        done = subprocess.run(
            [*command, "version"], capture_output=True, text=True, timeout=60
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return done.stdout.strip() if done.returncode == 0 else None


def find_zig() -> list[str] | None:
    """The command that runs zig, or None when it is not installed.

    Order: ``LAZYOS_ZIG``, ``zig`` on ``PATH``, then the ``ziglang`` pip wheel
    (``python -m ziglang``).
    """
    candidates: list[list[str]] = []
    explicit = os.environ.get("LAZYOS_ZIG")
    if explicit:
        candidates.append(shlex.split(explicit, posix=os.name != "nt"))
    on_path = shutil.which("zig")
    if on_path:
        candidates.append([on_path])
    candidates.append([sys.executable, "-m", "ziglang"])
    for command in candidates:
        if _probe(command) is not None:
            return command
    return None


def version(zig: list[str]) -> str:
    """The version string of the zig at `zig`."""
    return _probe(zig) or "unknown"


def _script(zig: list[str], subcommand: str, windows: bool) -> str:
    """The wrapper script text running `zig <subcommand> [-target ...]`."""
    target = ["-target", ZIG_TARGET] if subcommand in ("cc", "c++") else []
    words = [*zig, subcommand, *target]
    if windows:
        quoted = " ".join(f'"{word}"' for word in words)
        return f"@{quoted} %*\r\n"
    quoted = " ".join(shlex.quote(word) for word in words)
    return f'#!/bin/sh\nexec {quoted} "$@"\n'


def write_wrappers(zig: list[str], directory: Path, windows: bool | None = None) -> dict[str, Path]:
    """Write the ``zcc``/``zcxx``/``zar`` wrappers into `directory`.

    cc-rs and rustc each take one executable, so ``zig c++ -target ...`` needs a
    wrapper. Returns the map from role (``cc``, ``cxx``, ``ar``) to path; the
    paths are absolute because cargo runs build scripts from other directories.
    """
    if windows is None:
        windows = os.name == "nt"
    directory = directory.resolve()
    directory.mkdir(parents=True, exist_ok=True)
    suffix = ".cmd" if windows else ""
    paths: dict[str, Path] = {}
    for role, name, subcommand in (
        ("cc", "zcc", "cc"),
        ("cxx", "zcxx", "c++"),
        ("ar", "zar", "ar"),
    ):
        path = directory / f"{name}{suffix}"
        # Newline handling is explicit so a script is never rewritten with the
        # host's line endings (a CRLF `#!/bin/sh` line does not run).
        with open(path, "w", newline="") as handle:
            handle.write(_script(zig, subcommand, windows))
        path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        paths[role] = path
    return paths


def cargo_env(rust_target: str, wrappers: dict[str, Path]) -> dict[str, str]:
    """The environment that makes cargo build C/C++ and link with zig.

    Set only for the cargo subprocess that builds the Docs app, so the other
    apps and the OS workspace keep their own toolchain.
    """
    triple = rust_target.replace("-", "_")
    upper = rust_target.upper().replace("-", "_")
    return {
        "CRATE_CC_NO_DEFAULTS": "1",
        f"CC_{triple}": str(wrappers["cc"]),
        f"CXX_{triple}": str(wrappers["cxx"]),
        f"AR_{triple}": str(wrappers["ar"]),
        f"CFLAGS_{triple}": C_FLAGS,
        f"CXXFLAGS_{triple}": C_FLAGS,
        f"CARGO_TARGET_{upper}_LINKER": str(wrappers["cxx"]),
        f"CARGO_TARGET_{upper}_RUSTFLAGS": RUSTFLAGS,
    }
