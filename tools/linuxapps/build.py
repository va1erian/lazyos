#!/usr/bin/env python3
"""Build real Linux command-line programs as static musl executables.

LazyOS runs static ``x86_64-linux-musl`` binaries; these are unmodified
upstream programs to exercise that with (``tools/linuxapps/README.md``):

* ``lua``: Lua 5.4.7, the interpreter's sources with zig cc (``recipes.py``);
* ``sqlite3``: SQLite 3.46.1, the amalgamation and its shell (``recipes.py``);
* ``jq``: jq 1.7.1, with configure's results written out (``recipes.py``);
* ``dash``: dash 0.5.12 and its source generators (``dash.py``, ``dashgen.py``);
* ``rg``: ripgrep 14.1.1 with cargo, jemalloc removed (``rg.py``).

Every source archive is pinned by SHA-256 (``sources.py``) and cached in
``target/linuxapps/src/``; outputs go to ``target/linuxapps/bin/<name>``, and
each is checked to be an ELF64 x86-64 executable without an interpreter.

Usage::

    python tools/linuxapps/build.py                 # everything
    python tools/linuxapps/build.py --only lua jq   # some
    python tools/linuxapps/build.py --require       # anything unavailable fails

Prints a JSON map ``{name: path or null}`` on stdout. Without zig, Rust's musl
target or network access the affected programs are null, the reason is on
stderr, and the exit status is 0 unless ``--require``. A compile error, or an
output that is not a static executable, always exits 1.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Callable

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import dash  # noqa: E402
import elfcheck  # noqa: E402
import recipes  # noqa: E402
import rg  # noqa: E402
import sources  # noqa: E402
from toolchain import BuildError, Zig  # noqa: E402

BIN = sources.WORK / "bin"

#: name -> recipe taking (zig, output path); zig-less recipes ignore it.
C_RECIPES: dict[str, Callable[[Zig, Path], Path | None]] = {
    "lua": recipes.build_lua,
    "sqlite3": recipes.build_sqlite3,
    "jq": recipes.build_jq,
    "dash": dash.build_dash,
}
NAMES = [*C_RECIPES, "rg"]


def build_one(name: str, cc: Zig | None) -> Path | None:
    """Build `name`; None when a prerequisite is missing (logged)."""
    out = BIN / name
    if name == "rg":
        return rg.build_rg(out)
    if cc is None:
        sources.log(f"{name}: zig not found (install: {Zig.INSTALL_HINT})")
        return None
    return C_RECIPES[name](cc, out)


def verify(path: Path) -> None:
    """Fail loudly when `path` is not a static ELF64 x86-64 executable."""
    problem = elfcheck.check(path)
    if problem is not None:
        raise BuildError(f"{path} is not a static x86-64 executable: {problem}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--only", nargs="+", choices=NAMES, metavar="NAME",
                        help=f"build only these ({', '.join(NAMES)})")
    parser.add_argument("--require", action="store_true",
                        help="fail (exit 1) when a toolchain or download is unavailable")
    args = parser.parse_args()
    wanted = args.only or NAMES

    cc = Zig.find() if any(name in C_RECIPES for name in wanted) else None
    if cc is not None and cc.version() != Zig.TESTED_VERSION:
        sources.log(f"zig {cc.version()} found, {Zig.TESTED_VERSION} is the tested version")
    built: dict[str, str | None] = {}
    try:
        for name in wanted:
            path = build_one(name, cc)
            if path is not None:
                verify(path)
                sources.log(f"{name}: {path} ({path.stat().st_size // 1024} KiB)")
            built[name] = None if path is None else str(path)
    except BuildError as error:
        sources.log(str(error))
        print(json.dumps(built, indent=2))
        return 1
    print(json.dumps(built, indent=2))
    missing = [name for name, path in built.items() if path is None]
    if missing:
        sources.log(f"unavailable: {', '.join(missing)}")
        if args.require:
            return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
