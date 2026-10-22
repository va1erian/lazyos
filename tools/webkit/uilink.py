"""The build steps every desktop WebKit program shares (``tools/webkit/webd.py``).

A program of the WebKit port that has a Rust half is one static-PIE musl program in two parts:

1. the Rust half, a static library built **on the host** with cargo for
   ``x86_64-unknown-linux-musl`` into ``target/webkit/lib`` (``build_window_library``). A static
   library needs no linker, so any host that has the target can build it;
2. the C++ half (``webkit/port/``), linked by zig with WebKit's own static libraries and the Rust
   library **in the build container** (``link_program``: ``port.py --only <program>``), because
   WebKit's archives live in its work volume.

This is a library; ``webd.py`` is its only user. (LazyWeb+ used to be linked this way as
``lazywebplus-app``; it is now a plain xui app, a client of webd: ``tools/xui/build.py``.)
"""

from __future__ import annotations

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
