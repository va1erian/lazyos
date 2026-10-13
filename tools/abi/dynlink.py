#!/usr/bin/env python3
"""Build the dynamic-linking fixture (issue #535, gap 1) with zig.

LazyOS runs dynamically linked musl programs: the kernel maps the program and
the interpreter its `PT_INTERP` names, and musl's own dynamic linker does the
rest. This builds, as real dynamic objects for `x86_64-linux-musl`:

* `dynlink`: the test program (non-PIE), linked against `libdynadd.so`;
* `dynpie`: the same source as a PIE, which the kernel moves to its PIE base;
* `libdynadd.so` and `libplugin.so`: a library linked at build time and one
  `dlopen`ed at run time (with thread-local storage and a constructor).

and fetches the interpreter, `ld-musl-x86_64.so.1`. zig links against a stub
`libc.so` (symbols only; it assumes the target has musl), so the runnable
loader has to come from a musl distribution: Alpine's `musl` package, read out
of the `alpine:3.20` Docker image (`docker run --rm alpine:3.20 cat
/lib/ld-musl-x86_64.so.1`), or any musl `ld-musl-x86_64.so.1` you name with
`--interpreter FILE` or `LAZYOS_LD_MUSL`. musl is MIT licensed.

Output goes to `target/dynlink/`: `bin/` (`dynlink`, `dynpie`) and `lib/` (the
interpreter and the two libraries). `LAZYOS_DYNLINK=1` puts them at
`/system/bin` and `/system/lib`; `tools/abi/run.py` does that for its
`dynlink` row.

Without zig, or without a source for the interpreter, the build is skipped
(exit 0, an empty map), like the other fixtures; a compile error exits 1.

    python tools/abi/dynlink.py [--out DIR] [--interpreter FILE]
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
SOURCES = ROOT / "tools" / "abi" / "dynlink"
OUT = ROOT / "target" / "dynlink"
TARGET = "x86_64-linux-musl"
#: The interpreter's name: what `PT_INTERP` of a musl program says.
INTERPRETER = "ld-musl-x86_64.so.1"
#: The first bytes of every ELF file.
ELF_MAGIC = bytes([0x7F, 0x45, 0x4C, 0x46])
#: The image the interpreter is read from when no file is named.
ALPINE_IMAGE = "alpine:3.20"

sys.path.insert(0, str(ROOT / "tools" / "xui"))
import zig as xui_zig  # noqa: E402  (tools/xui/zig.py)


class BuildError(Exception):
    pass


class Skipped(Exception):
    """A prerequisite is missing: the fixture is unavailable, not broken."""


def run(command: list[str], what: str) -> str:
    done = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    if done.returncode != 0:
        raise BuildError(f"{what} failed:\n{(done.stdout + done.stderr)[-3000:]}")
    return done.stdout + done.stderr


def fetch_interpreter(dest: Path, explicit: Path | None) -> bool:
    """Put a runnable musl `ld-musl-x86_64.so.1` at `dest`; False if there is none."""
    named = explicit or (Path(os.environ["LAZYOS_LD_MUSL"]) if os.environ.get("LAZYOS_LD_MUSL") else None)
    if named is not None:
        if not named.is_file():
            raise BuildError(f"{named} is not a file")
        shutil.copyfile(named, dest)
        return True
    docker = shutil.which("docker")
    if docker is None:
        return False
    done = subprocess.run(
        [docker, "run", "--rm", ALPINE_IMAGE, "cat", f"/lib/{INTERPRETER}"],
        capture_output=True,
    )
    if done.returncode != 0 or done.stdout[:4] != ELF_MAGIC:
        sys.stderr.write(done.stderr.decode(errors="replace")[-500:])
        return False
    dest.write_bytes(done.stdout)
    return True


def build(zig: list[str], out: Path, interpreter: Path | None) -> dict[str, str]:
    bin_dir, lib_dir = out / "bin", out / "lib"
    shutil.rmtree(out, ignore_errors=True)
    bin_dir.mkdir(parents=True)
    lib_dir.mkdir(parents=True)
    if not fetch_interpreter(lib_dir / INTERPRETER, interpreter):
        raise Skipped(
            f"no musl interpreter: docker with {ALPINE_IMAGE}, --interpreter or LAZYOS_LD_MUSL"
        )
    common = [*zig, "cc", "-target", TARGET, "-Os", "-w"]
    for name in ("dynadd", "plugin"):
        soname = f"lib{name}.so"
        run(
            [*common, "-shared", "-fPIC", f"-Wl,-soname,{soname}",
             str(SOURCES / f"lib{name}.c"), "-o", str(lib_dir / soname)],
            f"building {soname}",
        )
    program = str(SOURCES / "dynlink.c")
    libs = [f"-L{lib_dir}", "-ldynadd"]
    run([*common, "-dynamic", program, *libs, "-o", str(bin_dir / "dynlink")],
        "building dynlink")
    run([*common, "-dynamic", "-fPIE", "-pie", program, *libs, "-o", str(bin_dir / "dynpie")],
        "building dynpie")
    return {
        "dynlink": str(bin_dir / "dynlink"),
        "dynpie": str(bin_dir / "dynpie"),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--out", type=Path, default=OUT)
    parser.add_argument("--interpreter", type=Path, help="a musl ld-musl-x86_64.so.1 to use")
    args = parser.parse_args()
    zig = xui_zig.find_zig()
    if zig is None:
        print(f"warning: zig not found ({xui_zig.INSTALL_HINT}); dynlink not built",
              file=sys.stderr)
        print(json.dumps({}))
        return 0
    try:
        built = build(zig, args.out, args.interpreter)
    except Skipped as error:
        print(f"warning: dynlink not built: {error}", file=sys.stderr)
        print(json.dumps({}))
        return 0
    except BuildError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(json.dumps(built, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
