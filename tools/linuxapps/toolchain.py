"""zig as the C compiler: static musl programs for LazyOS, plus host helpers.

``tools/xui/zig.py`` finds zig (``LAZYOS_ZIG``, ``zig`` on ``PATH``, or the
``ziglang`` pip wheel) and names the musl target; this module only adds the
two compile steps the recipes need:

* :meth:`Zig.program` compiles C sources straight into a static-PIE
  ``x86_64-linux-musl`` executable. Not a fixed-address one: zig links those
  at 16 MiB, inside the window LazyOS's Linux loader keeps for `brk`/`mmap`,
  while a position-independent image is placed where the loader chooses;
* :meth:`Zig.host_program` compiles a generator (dash's ``mkinit`` and
  friends) for the machine running the build, so it can be run here.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "xui"))
import zig as xui_zig  # noqa: E402  (tools/xui/zig.py)

#: Size-optimised, static-PIE, and quiet about upstream's warnings (the code
#: is unmodified; its warnings are upstream's business, errors still stop us).
TARGET_FLAGS = ["-Os", "-fPIE", "-w", "-ffunction-sections", "-fdata-sections"]
LINK_FLAGS = ["-static-pie", "-Wl,--gc-sections", "-s"]


class BuildError(Exception):
    """A compiler or generator failed; the message carries its output."""


def run(command: list[str], cwd: Path, what: str) -> subprocess.CompletedProcess:
    done = subprocess.run(command, cwd=cwd, capture_output=True, text=True)
    if done.returncode != 0:
        output = (done.stdout + done.stderr)[-4000:]
        raise BuildError(f"{what} failed (exit {done.returncode}):\n{output}")
    return done


class Zig:
    INSTALL_HINT = xui_zig.INSTALL_HINT
    TESTED_VERSION = xui_zig.ZIG_VERSION

    def __init__(self, command: list[str]):
        self.command = command

    @classmethod
    def find(cls) -> "Zig | None":
        command = xui_zig.find_zig()
        return None if command is None else cls(command)

    def version(self) -> str:
        return xui_zig.version(self.command)

    def program(self, sources: list[str], out: Path, cwd: Path,
                flags: list[str] = (), libs: list[str] = ()) -> Path:
        """Compile `sources` (relative to `cwd`) and link a static musl `out`."""
        out.parent.mkdir(parents=True, exist_ok=True)
        command = [*self.command, "cc", "-target", xui_zig.ZIG_TARGET,
                   *TARGET_FLAGS, *flags, *sources, *LINK_FLAGS, *libs, "-o", str(out)]
        run(command, cwd, f"compiling {out.name}")
        return out

    def preprocess(self, source: Path, cwd: Path, flags: list[str] = ()) -> str:
        """`source` run through the target's C preprocessor, without line markers."""
        command = [*self.command, "cc", "-target", xui_zig.ZIG_TARGET, "-E", "-P",
                   "-x", "c", *flags, str(source)]
        return run(command, cwd, f"preprocessing {source.name}").stdout

    def host_program(self, source: str, cwd: Path, flags: list[str] = ()) -> Path:
        """Compile `source` (relative to `cwd`) for this machine; its path."""
        stem = Path(source).stem
        out = cwd / (stem + (".exe" if os.name == "nt" else ""))
        command = [*self.command, "cc", "-O1", "-w", *flags, source, "-o", str(out)]
        run(command, cwd, f"compiling the host tool {stem}")
        return out
