#!/usr/bin/env python3
"""Build LazyWeb+ (``lazywebplus-app``), WebKit's desktop UI process with its Rust window.

``docs/lazywebplus.md`` and ``docs/webkit-port-plan.md``. The app is one static-PIE musl program in
two halves that meet through a small C ABI (``webkit/port/app/lazyos_ui.h``):

1. the window, a Rust static library, built **on the host** with cargo for
   ``x86_64-unknown-linux-musl`` into ``target/webkit/lib``: ``xui-app/webplus`` (the browser's chrome
   on the seam of ``xui-app/crates/webkit``: ``libxui_webplus.a``). A static library needs no
   linker, so any host that has the target can build it;
2. the UI process, C++ (``webkit/port/app/``): WebKit's run loop and C API on the main thread, the
   window on a second one, linked by zig with WebKit's own static libraries and the window's
   library, **in the build container** (``port.py --only lazywebplus-app``), because WebKit's
   archives live in its work volume.

The result is ``target/webkit/bin/lazywebplus-app``, copied to ``target/xui/xui-lazywebplus.elf``,
where ``tools/xui/core_packages.py`` finds it for the package (``xui-app/packages/lazywebplus``).
Needs the port's other programs built first (``python tools/webkit/port.py``): the app starts
``WebProcess`` and ``NetworkProcess``, installed as ``/system/bin/webprocess`` and
``networkprocess`` by ``LAZYOS_LAZYWEBPLUS=1`` images.

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
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import wkpins  # noqa: E402  (also puts tools/jsc first on sys.path)

ROOT = wkpins.ROOT
TARGET = "x86_64-unknown-linux-musl"
XUI_APP = ROOT / "xui-app"


@dataclass(frozen=True)
class App:
    """One desktop app: the crate that is its window, the program that links it, its package ELF."""
    package: str
    library: str
    program: str
    elf: str

    @property
    def lib(self) -> Path:
        return wkpins.OUT / "lib" / self.library

    @property
    def packaged(self) -> Path:
        return ROOT / "target" / "xui" / self.elf


APP = App("xui-webplus", "libxui_webplus.a", "lazywebplus-app", "xui-lazywebplus.elf")

# Kept for the code that names them.
LIB = APP.lib
PROGRAM = APP.program
ELF = APP.packaged


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


def build_window_library(app: App = APP) -> Path | None:
    """The window as a static library in ``target/webkit/lib``; None when cargo cannot build for musl."""
    if not ensure_target():
        return None
    wkpins.log(f"building the window ({app.package}, static library, release)")
    built = subprocess.run(
        ["cargo", "build", "--manifest-path", str(XUI_APP / "Cargo.toml"), "-p", app.package,
         "--lib", "--target", TARGET, "--release"], cwd=ROOT)
    if built.returncode != 0:
        raise SystemExit("error: the window library did not build")
    source = XUI_APP / "target" / TARGET / "release" / app.library
    if not source.is_file():
        raise SystemExit(f"error: cargo finished but {source} is missing")
    app.lib.parent.mkdir(parents=True, exist_ok=True)
    # Only a changed library may touch the file: CMake relinks the program when it is newer.
    if not app.lib.is_file() or app.lib.read_bytes() != source.read_bytes():
        shutil.copyfile(source, app.lib)
    wkpins.log(f"{app.lib} ({app.lib.stat().st_size / 1048576:.1f} MiB)")
    return app.lib


def link_program(app: App, no_docker: bool, require: bool) -> Path | None:
    """The app's program, linked in the build container; None when Docker is not there."""
    import port
    argv = ["--only", app.program]
    if no_docker:
        code = subprocess.run([sys.executable, str(HERE / "port.py"), "--no-docker", *argv]).returncode
    else:
        code = port.run_in_docker(argv)
    if code == 127 and not require:
        return None
    if code != 0:
        raise SystemExit(f"error: linking {app.program} failed (exit {code})")
    program = wkpins.BIN / app.program
    if not program.is_file():
        raise SystemExit(f"error: the build finished but {program} is missing")
    app.packaged.parent.mkdir(parents=True, exist_ok=True)
    if not app.packaged.is_file() or app.packaged.read_bytes() != program.read_bytes():
        shutil.copyfile(program, app.packaged)
    return app.packaged


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--lib-only", action="store_true", help="only the Rust half (no Docker needed)")
    parser.add_argument("--require", action="store_true", help="fail (exit 1) when Docker or the target is missing")
    parser.add_argument("--no-docker", action="store_true", help="link on this Linux host")
    args = parser.parse_args()

    app = APP
    library = build_window_library(app)
    built: dict[str, str | None] = {"window": str(library) if library else None}
    if not args.lib_only and library:
        program = link_program(app, args.no_docker, args.require)
        built[app.program] = str(program) if program else None
    print(json.dumps(built, indent=2))
    missing = [name for name, path in built.items() if path is None]
    return 1 if args.require and missing else 0


if __name__ == "__main__":
    raise SystemExit(main())
