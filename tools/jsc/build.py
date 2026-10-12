#!/usr/bin/env python3
"""Build JavaScriptCore's ``jsc`` shell and its C++ proof for LazyOS.

Stages W1 and W2 of ``docs/webkit-port-plan.md`` (``tools/jsc/README.md``):

* ``cxxproof``: a C++ program with libc++, threads, exceptions, TLS,
  ``std::filesystem`` and ICU (``cxx.py``, ``cxxproof/main.cpp``);
* ``icu``: ICU 74.2, static, its data compiled in (``icu.py``);
* ``jsc``: WebKitGTK 2.54.1's ``JSCOnly`` port with the C-loop interpreter,
  patched by ``tools/jsc/patches/`` (``webkit.py``).

All three are static-PIE ``x86_64-linux-musl`` executables built by zig, in
``target/jsc/bin/``. The build needs cmake, ninja, perl, python, ruby, gperf
and bison, so by default it runs inside a Docker container (``Dockerfile``,
Debian 12 with zig from pip) with the repository mounted at ``/work``, on any
host with Docker. ``--no-docker`` runs the same steps on a Linux host that has
the tools. Sources are pinned by SHA-256 (``pins.py``).

Usage::

    python tools/jsc/build.py                  # everything, in Docker
    python tools/jsc/build.py --only cxxproof  # ICU and the proof program
    python tools/jsc/build.py --no-docker      # on this host (Linux, WSL, CI)
    python tools/jsc/build.py --require        # a missing download/Docker fails

Prints a JSON map ``{name: path or null}``. Without Docker (and without the
tools) the entries are null and the exit status is 0, 1 with ``--require``;
a compile error always exits 1.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import pins  # noqa: E402
from toolenv import BuildError  # noqa: E402

NAMES = ["cxxproof", "jsc"]
IMAGE = "lazyos-jsc-build:1"
VOLUME = "lazyos-jsc-work"


def in_container() -> bool:
    return os.environ.get("LAZYOS_JSC_IN_CONTAINER") == "1"


def run_steps(names: list[str], jobs: int, no_icu: bool) -> dict[str, str | None]:
    """The real build; runs on any Linux with the tools (the container's job)."""
    import cxx
    import icu
    built: dict[str, str | None] = {}
    if "cxxproof" in names:
        if not no_icu:
            icu.build(jobs)
        built["cxxproof"] = str(cxx.build(with_icu=not no_icu))
    if "jsc" in names:
        import webkit
        icu.build(jobs)
        built["jsc"] = str(webkit.build(jobs))
    chown_outputs()
    return built


def chown_outputs() -> None:
    """Files made by root in a bind mount belong to the invoking user."""
    uid, gid = os.environ.get("LAZYOS_JSC_UID"), os.environ.get("LAZYOS_JSC_GID")
    if uid is None or gid is None:
        return
    for path in [pins.OUT.parent, *pins.OUT.parent.rglob("*")]:
        os.chown(path, int(uid), int(gid))


def run_in_docker(argv: list[str]) -> int:
    """Run this script in the build container with the repository mounted."""
    if shutil.which("docker") is None:
        pins.log("docker not found (install Docker, or use --no-docker on a Linux host)")
        return 127
    built = subprocess.run(["docker", "build", "-q", "-t", IMAGE, str(HERE)])
    if built.returncode != 0:
        return built.returncode
    command = ["docker", "run", "--rm", "-v", f"{pins.ROOT}:/work", "-v", f"{VOLUME}:/build"]
    if hasattr(os, "getuid"):
        command += ["-e", f"LAZYOS_JSC_UID={os.getuid()}", "-e", f"LAZYOS_JSC_GID={os.getgid()}"]
    return subprocess.run([*command, IMAGE, *argv]).returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--only", nargs="+", choices=NAMES, metavar="NAME",
                        help=f"build only these ({', '.join(NAMES)})")
    parser.add_argument("--require", action="store_true",
                        help="fail (exit 1) when Docker or a download is unavailable")
    parser.add_argument("--no-docker", action="store_true",
                        help="build on this host instead of in the container")
    parser.add_argument("--jobs", type=int, default=os.cpu_count() or 4)
    parser.add_argument("--no-icu", action="store_true",
                        help="cxxproof without ICU (the toolchain-only check)")
    args = parser.parse_args()
    names = args.only or NAMES

    if not in_container() and not args.no_docker:
        forwarded = [a for a in sys.argv[1:] if a != "--no-docker"]
        code = run_in_docker(forwarded)
        if code == 127 and not args.require:
            print(json.dumps({name: None for name in names}, indent=2))
            return 0
        if code == 0:
            print(json.dumps({n: str(pins.OUT / n) if (pins.OUT / n).is_file() else None
                              for n in names}, indent=2))
        return code
    try:
        built = run_steps(names, args.jobs, args.no_icu)
    except BuildError as error:
        pins.log(str(error))
        return 1
    print(json.dumps(built, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
