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
target/xui/xui-sysmon.elf, target/xui/xui-fabricmon.elf,
target/xui/xui-client.elf and target/xui/xui-term.elf, plus xui-editor.elf,
xui-paint.elf and xui-files.elf (the migrated document apps), and a JSON map
on stdout. If the musl target or toolchain is unavailable the script reports
what it could build and exits 0, so a CI job can skip the visual run.

The Docs app (``xui-docs.elf``, Markdown rendered by litehtml) is built last, in
its own cargo invocation and target directory, with the zig toolchain
(``tools/xui/zig.py``): litehtml is C++, and only that package pulls it in.
Without zig it is skipped with a warning and every other app still builds.

``xui-core``, ``xui-canvas`` and ``xui-icons`` are git dependencies on
``va1erian/xui`` at a single pinned revision; ``xui-canvas`` is built with
``default-features = false`` so its software painter core (in-memory font
registration, natural-width alignment, borrowed pixels, ``OffscreenBackend``) is
used without ``winit``/``softbuffer``/``glutin``/``glow``/``arboard``/``xui-gpu``.
No vendored copy or ``[patch]`` is involved.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import zig  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent.parent
APP = ROOT / "xui-app"
TARGET = "x86_64-unknown-linux-musl"
OUT_DIR = ROOT / "target" / "xui"
# The Docs app has its own cargo target directory: its zig-linked build uses a
# different RUSTFLAGS environment, which would otherwise invalidate (and
# alternately rebuild) every dependency the other apps share.
DOCS_TARGET_DIR = ROOT / "target" / "xui-zig"
DOCS_PACKAGE = "xui-docs"
BINS = {
    "xui-m0": "xui-m0.elf",
    "xui-counter": "xui-counter.elf",
    "xui-sysmon": "xui-sysmon.elf",
    "xui-fabricmon": "xui-fabricmon.elf",
    "xui-widget": "xui-widget.elf",
    "xui-client": "xui-client.elf",
    "xui-term": "xui-term.elf",
    # The migrated portable apps (issues #162/#159); every desktop image ships
    # them (build.rs `DESKTOP_XUI_APPS`).
    "xui-editor": "xui-editor.elf",
    "xui-paint": "xui-paint.elf",
    "xui-files": "xui-files.elf",
    # The Settings app (confd-backed configuration panel).
    "xui-settings": "xui-settings.elf",
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
        # Target-specific variables so the host build scripts and proc macros
        # keep the default linker; a bare RUSTFLAGS would leak into them.
        prefix = f"CARGO_TARGET_{TARGET.upper().replace('-', '_')}"
        env.setdefault(f"{prefix}_LINKER", str(linker))
        env.setdefault(f"{prefix}_RUSTFLAGS", "-C linker-flavor=ld.lld")
    return env


def build_docs(debug: bool) -> str | None:
    """Build the Docs app with zig; return its ELF path, or None when skipped.

    A missing zig is a skip (warning), like a missing musl target; a compile
    error is fatal so CI cannot silently ship an image without the app.
    """
    command = zig.find_zig()
    if command is None:
        print(
            f"warning: zig not found, skipping {DOCS_PACKAGE} "
            f"(install: {zig.INSTALL_HINT})",
            file=sys.stderr,
        )
        return None
    found = zig.version(command)
    if found != zig.ZIG_VERSION:
        print(f"warning: zig {found} found, {zig.ZIG_VERSION} is the tested version", file=sys.stderr)
    wrappers = zig.write_wrappers(command, OUT_DIR / "zig")
    env = dict(os.environ)
    env.update(zig.cargo_env(TARGET, wrappers))
    cargo = [
        "cargo",
        "build",
        "--manifest-path",
        str(APP / "Cargo.toml"),
        "-p",
        DOCS_PACKAGE,
        "--target",
        TARGET,
        "--target-dir",
        str(DOCS_TARGET_DIR),
    ]
    if not debug:
        cargo.append("--release")
    build = run(cargo, env=env)
    if build.returncode != 0:
        print(f"error: {DOCS_PACKAGE} build failed", file=sys.stderr)
        print(build.stderr[-2000:], file=sys.stderr)
        raise SystemExit(1)
    source = DOCS_TARGET_DIR / TARGET / ("debug" if debug else "release") / DOCS_PACKAGE
    return str(source) if source.is_file() else None


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

    docs = build_docs(args.debug)
    if docs:
        dest = OUT_DIR / f"{DOCS_PACKAGE}.elf"
        dest.write_bytes(Path(docs).read_bytes())
        built[DOCS_PACKAGE] = str(dest)

    print(json.dumps(built, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
