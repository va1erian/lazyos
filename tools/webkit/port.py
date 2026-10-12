#!/usr/bin/env python3
"""Build WebKit's LazyOS port: WebProcess, NetworkProcess and ``webkit-smoke`` (stage W4).

``docs/webkit-port-plan.md`` section 5, W4: WebCore and the WebKit (WebKit2)
layer built with ``PORT=LazyOS`` (``webkit/``, the port's own files, laid over
the pinned WebKitGTK 2.54.1 tarball by ``port_tree.py``) against the W3
dependency bundle, as static-PIE ``x86_64-linux-musl`` programs by zig:

* ``WebProcess`` and ``NetworkProcess``: what the UI process launches;
* ``webkit-smoke``: a headless UI process driven through WebKit's C API
  (``webkit/port/smoke/``).

Runs inside the ``jsc`` build's Docker container (``tools/jsc/Dockerfile``, the
volume ``lazyos-jsc-work``) like ``deps.py``; ``--no-docker`` runs the same
steps on a Linux host that has the tools. Needs the bundle: ``python
tools/webkit/deps.py`` first (it includes the EGL stand-in and libwpe).

Usage::

    python tools/webkit/port.py                 # configure + build all three, in Docker
    python tools/webkit/port.py --configure     # only the CMake configure step
    python tools/webkit/port.py --only WebProcess
    python tools/webkit/port.py --require       # a missing Docker/download is an error
    python tools/webkit/port.py --no-docker     # on this Linux host

Prints a JSON map ``{name: path or null}`` (``target/webkit/bin/``). Without
Docker the entries are null and the status is 0, or 1 with ``--require``; a
failed build always exits 1.
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
import wkpins  # noqa: E402  (also puts tools/jsc first on sys.path)
from toolenv import BuildError  # noqa: E402

NAMES = ["WebProcess", "NetworkProcess", "webkit-smoke"]


def in_container() -> bool:
    return os.environ.get("LAZYOS_JSC_IN_CONTAINER") == "1"


def run_steps(only: list[str] | None, jobs: int, configure_only: bool) -> dict[str, str | None]:
    """The real build; runs on any Linux with the tools (the container's job)."""
    import port_cmake
    import port_tree
    tree = port_tree.prepare()
    if configure_only:
        port_cmake.configure(tree)
        return {}
    built = port_cmake.build(tree, jobs, only or NAMES)
    if not only:
        port_cmake.host_reference()
    port_cmake.chown_outputs()
    return built


def default_jobs() -> int:
    """Twenty cores, but a WebCore translation unit can take a gigabyte: leave memory for them."""
    cpus = os.cpu_count() or 4
    try:
        with open("/proc/meminfo") as meminfo:
            kib = int(next(line for line in meminfo if line.startswith("MemTotal")).split()[1])
        return max(2, min(cpus, int(kib / (1024 * 1024) / 1.1)))
    except (OSError, StopIteration, ValueError):
        return cpus


def run_in_docker(argv: list[str]) -> int:
    """Run this script in the ``jsc`` build container, repository at /work."""
    import deps
    jsc_build = deps.load_jsc_build()
    if shutil.which("docker") is None:
        wkpins.log("docker not found (install Docker, or use --no-docker on a Linux host)")
        return 127
    built = subprocess.run(["docker", "build", "-q", "-t", jsc_build.IMAGE, str(jsc_build.HERE)])
    if built.returncode != 0:
        return built.returncode
    command = ["docker", "run", "--rm", "--entrypoint", "python3",
               "-v", f"{wkpins.ROOT}:/work", "-v", f"{jsc_build.VOLUME}:/build"]
    if hasattr(os, "getuid"):
        command += ["-e", f"LAZYOS_JSC_UID={os.getuid()}", "-e", f"LAZYOS_JSC_GID={os.getgid()}"]
    return subprocess.run([*command, jsc_build.IMAGE, "-I", "/work/tools/webkit/port.py", *argv]).returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--only", nargs="+", choices=NAMES, metavar="NAME",
                        help=f"build only these ({', '.join(NAMES)})")
    parser.add_argument("--configure", action="store_true", help="only configure the CMake tree")
    parser.add_argument("--require", action="store_true",
                        help="fail (exit 1) when Docker or a download is unavailable")
    parser.add_argument("--no-docker", action="store_true",
                        help="build on this host instead of in the container")
    parser.add_argument("--jobs", type=int, default=0, help="parallel compiles (default: by memory)")
    args = parser.parse_args()
    names = args.only or NAMES

    if not in_container() and not args.no_docker:
        forwarded = [a for a in sys.argv[1:] if a != "--no-docker"]
        code = run_in_docker(forwarded)
        if code == 127 and not args.require:
            print(json.dumps({name: None for name in names}, indent=2))
            return 0
        if code == 0:
            print(json.dumps({n: str(wkpins.BIN / n) if (wkpins.BIN / n).is_file() else None
                              for n in names}, indent=2))
        return code
    try:
        built = run_steps(args.only, args.jobs or default_jobs(), args.configure)
    except BuildError as error:
        wkpins.log(str(error))
        return 1
    print(json.dumps(built, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
