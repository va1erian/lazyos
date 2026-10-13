#!/usr/bin/env python3
"""Build the WebKit desktop app, ``os.lazy.webkit`` (stage W5).

``docs/webkit-port-plan.md`` section 5, W5. The app is one static-PIE musl program,
``webkit-app``, in two halves that meet through a small C ABI
(``webkit/port/app/lazyos_ui.h``):

1. the window, a Rust static library (``xui-app/crates/webkit``: the toolbar, the address
   field and the page view over ``xui-app``'s compositor client), built **on the host** with
   cargo for ``x86_64-unknown-linux-musl`` into ``target/webkit/lib/libxui_webkit.a``. A
   static library needs no linker, so any host that has the target can build it;
2. the UI process, C++ (``webkit/port/app/``): WebKit's run loop and C API on the main
   thread, the window on a second one, linked by zig with WebKit's own static libraries and
   the window's library, **in the build container** (``port.py --only webkit-app``), because
   WebKit's archives live in its work volume.

The result is ``target/webkit/bin/webkit-app``, copied to ``target/xui/xui-webkit.elf``, where
``tools/xui/core_packages.py`` finds it for the package (``xui-app/packages/webkit``).
Needs the port's other programs built first (``python tools/webkit/port.py``): the app starts
``WebProcess`` and ``NetworkProcess``, installed as ``/system/bin/webprocess`` and
``networkprocess`` by ``LAZYOS_WEBKIT=1`` images.

Usage::

    python tools/webkit/app.py                # window library, then the link in Docker
    python tools/webkit/app.py --lib-only     # just the Rust half (no Docker)
    python tools/webkit/app.py --require      # a missing Docker or target is an error
    python tools/webkit/app.py --no-docker    # link on this Linux host

Prints a JSON map of what it built. Without Docker or the musl target the entries are null and
the status is 0, or 1 with ``--require``; a failed build always exits 1.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import wkpins  # noqa: E402  (also puts tools/jsc first on sys.path)

ROOT = wkpins.ROOT
TARGET = "x86_64-unknown-linux-musl"
XUI_APP = ROOT / "xui-app"
LIB_NAME = "libxui_webkit.a"
LIB = wkpins.OUT / "lib" / LIB_NAME
PROGRAM = "webkit-app"
ELF = ROOT / "target" / "xui" / "xui-webkit.elf"


def ensure_target() -> bool:
    """True when the musl target is installed (adding it when it is not)."""
    installed = subprocess.run(["rustup", "target", "list", "--installed"], capture_output=True, text=True)
    if TARGET in installed.stdout:
        return True
    added = subprocess.run(["rustup", "target", "add", TARGET], capture_output=True, text=True)
    if added.returncode != 0:
        wkpins.log(f"cannot add {TARGET}: {added.stderr.strip()}")
        return False
    return True


def build_window_library() -> Path | None:
    """The window as a static library in ``target/webkit/lib``; None when cargo cannot build for musl."""
    if not ensure_target():
        return None
    wkpins.log("building the window (xui-webkit, static library, release)")
    built = subprocess.run(
        ["cargo", "build", "--manifest-path", str(XUI_APP / "Cargo.toml"), "-p", "xui-webkit",
         "--lib", "--target", TARGET, "--release"], cwd=ROOT)
    if built.returncode != 0:
        raise SystemExit("error: the window library did not build")
    source = XUI_APP / "target" / TARGET / "release" / LIB_NAME
    if not source.is_file():
        raise SystemExit(f"error: cargo finished but {source} is missing")
    LIB.parent.mkdir(parents=True, exist_ok=True)
    # Only a changed library may touch the file: CMake relinks the program when it is newer.
    if not LIB.is_file() or LIB.read_bytes() != source.read_bytes():
        shutil.copyfile(source, LIB)
    wkpins.log(f"{LIB} ({LIB.stat().st_size / 1048576:.1f} MiB)")
    return LIB


def link_program(no_docker: bool, require: bool) -> Path | None:
    """``webkit-app``, linked in the build container; None when Docker is not there."""
    import port
    argv = ["--only", PROGRAM]
    if no_docker:
        code = subprocess.run([sys.executable, str(HERE / "port.py"), "--no-docker", *argv]).returncode
    else:
        code = port.run_in_docker(argv)
    if code == 127 and not require:
        return None
    if code != 0:
        raise SystemExit(f"error: linking {PROGRAM} failed (exit {code})")
    program = wkpins.BIN / PROGRAM
    if not program.is_file():
        raise SystemExit(f"error: the build finished but {program} is missing")
    ELF.parent.mkdir(parents=True, exist_ok=True)
    if not ELF.is_file() or ELF.read_bytes() != program.read_bytes():
        shutil.copyfile(program, ELF)
    return ELF


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--lib-only", action="store_true", help="only the Rust half (no Docker needed)")
    parser.add_argument("--require", action="store_true", help="fail (exit 1) when Docker or the target is missing")
    parser.add_argument("--no-docker", action="store_true", help="link on this Linux host")
    args = parser.parse_args()

    library = build_window_library()
    built: dict[str, str | None] = {"window": str(library) if library else None}
    if not args.lib_only and library:
        program = link_program(args.no_docker, args.require)
        built["webkit-app"] = str(program) if program else None
    print(json.dumps(built, indent=2))
    missing = [name for name, path in built.items() if path is None]
    return 1 if args.require and missing else 0


if __name__ == "__main__":
    raise SystemExit(main())
