"""zig as the C and C++ compiler for CMake and autoconf, plus small helpers.

CMake and ICU's configure want a compiler that is one executable, not
``zig c++ -target x86_64-linux-musl``, so :func:`wrappers` writes four tiny
shell scripts (``zcc``, ``zc++``, ``zar``, ``zranlib``) into the work tree.
The target is always static musl, and libc++ with libunwind come with
``zig c++``; one libc++ for ICU and for WebKit keeps their ABIs the same.

Code is position independent because the programs are static-PIE, which is
what the other Linux programs are (``tools/linuxapps/toolchain.py``).
"""

from __future__ import annotations

import os
import shlex
import subprocess
import sys
from pathlib import Path

import pins

sys.path.insert(0, str(pins.ROOT / "tools" / "xui"))
import zig as xui_zig  # noqa: E402  (tools/xui/zig.py)

#: Common code generation flags; `-w` because upstream's warnings are upstream's.
CODEGEN = ["-O2", "-fPIC", "-w", "-ffunction-sections", "-fdata-sections", "-g0"]
LINK = ["-static-pie", "-Wl,--gc-sections"]


class BuildError(Exception):
    """A step failed; the message carries the tail of its output."""


def run(command: list[str], cwd: Path, what: str, env: dict | None = None,
        quiet: bool = False) -> None:
    """Run `command`; stream its output to the log file, raise on failure."""
    pins.log(f"{what}: {' '.join(shlex.quote(c) for c in command[:6])}"
             f"{' ...' if len(command) > 6 else ''}")
    merged = dict(os.environ)
    merged.update(env or {})
    logfile = pins.WORK / "logs" / (what.replace(" ", "_").replace("/", "_") + ".log")
    logfile.parent.mkdir(parents=True, exist_ok=True)
    with logfile.open("wb") as out:
        done = subprocess.run(command, cwd=cwd, env=merged, stdout=out,
                              stderr=subprocess.STDOUT)
    if done.returncode != 0:
        tail = logfile.read_bytes()[-6000:].decode("utf-8", "replace")
        raise BuildError(f"{what} failed (exit {done.returncode}); log {logfile}:\n{tail}")


def zig_command() -> list[str]:
    command = xui_zig.find_zig()
    if command is None:
        raise BuildError(f"zig not found: {xui_zig.INSTALL_HINT}")
    return command


def icf_wrapper(cxx: str, lld: str) -> str:
    """The `zc++` script. With `LAZYOS_LINK_ICF` set a link step runs twice: zig's `cc` driver
    refuses `-Wl,--icf=...` (lld folds identical functions and constants with it: 5 MB of a WebKit
    program), so the first link only reports (`ZIG_VERBOSE_LINK`) the exact `ld.lld` command line,
    and the second runs that line again, with `--icf=safe`, through zig's own lld."""
    return f"""#!/bin/sh
if [ -z "$LAZYOS_LINK_ICF" ]; then exec {cxx} "$@"; fi
case " $* " in *" -c "*|*" -E "*|*" -S "*) exec {cxx} "$@" ;; esac
log=$(mktemp)
ZIG_VERBOSE_LINK=1 {cxx} "$@" 2>"$log"
status=$?
grep -v '^\\(ld.lld\\|ar rcs\\) ' "$log" >&2
line=$(grep '^ld.lld ' "$log" | tail -1)
rm -f "$log"
if [ $status -eq 0 ] && [ -n "$line" ]; then
  out=$(mktemp)
  eval "{lld} --icf=safe ${{line#ld.lld }}" >"$out" 2>&1
  status=$?
  grep -v 'conservatively ignores SHT_LLVM_ADDRSIG' "$out" >&2
  rm -f "$out"
fi
exit $status
"""


def wrappers() -> dict[str, str]:
    """Write the compiler wrappers; the `CC`/`CXX`/`AR`/`RANLIB` paths."""
    base = pins.WORK / "wrap"
    base.mkdir(parents=True, exist_ok=True)
    zig = " ".join(shlex.quote(part) for part in zig_command())
    paths: dict[str, str] = {}
    for name, body in {
        "zcc": f"{zig} cc -target {xui_zig.ZIG_TARGET}",
        "zc++": f"{zig} c++ -target {xui_zig.ZIG_TARGET}",
        "zar": f"{zig} ar",
        "zranlib": f"{zig} ranlib",
    }.items():
        script = base / name
        script.write_text(icf_wrapper(body, f"{zig} ld.lld") if name == "zc++"
                          else f'#!/bin/sh\nexec {body} "$@"\n')
        script.chmod(0o755)
        paths[name] = str(script)
    return paths


def extract(archive: Path, source: pins.Source, into: Path) -> Path:
    """Unpack `archive` once; the directory holding `source.topdir`'s tree."""
    tree = into / source.topdir
    mark = tree / pins.EXTRACTED_MARK
    if mark.is_file():
        return tree
    if tree.exists():
        run(["rm", "-rf", str(tree)], into, f"removing partial {tree.name}")
    into.mkdir(parents=True, exist_ok=True)
    # `-m`: every file gets the extraction's time, not the archive's. A tree extracted again (a removed
    # or changed patch, `port_tree.py`) then rebuilds every object: with the archive's old times, a file a
    # removed patch had changed looked older than its object and kept the patched build.
    run(["tar", "-m", "-xf", str(archive), "-C", str(into)], into, f"extracting {archive.name}")
    mark.write_text("ok\n")
    return tree


def apply_patches(tree: Path, group: str) -> list[str]:
    """Apply `patches/<group>/*.patch` in name order, once; their names."""
    done = tree / ".lazyos-patched"
    applied = done.read_text().split() if done.is_file() else []
    patch_dir = pins.PATCHES / group
    names = sorted(p.name for p in patch_dir.glob("*.patch")) if patch_dir.is_dir() else []
    for name in names:
        if name in applied:
            continue
        run(["patch", "-p1", "--forward", "-i", str(patch_dir / name)], tree,
            f"patching {group} {name}")
        applied.append(name)
        done.write_text("\n".join(applied) + "\n")
    return names
