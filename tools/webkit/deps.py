#!/usr/bin/env python3
"""Build the WebKit dependency bundle and its proof program for LazyOS (stage W3).

``docs/webkit-port-plan.md`` section 5, W3: every library WebCore and the
WebKit layer need, as static ``x86_64-linux-musl`` archives built by zig from
pinned, hash-checked sources (``wkpins.py``), installed into one prefix inside
the ``jsc`` build's Docker volume, with ``target/webkit/deps.json`` listing
each library's version, archive digest and installed files. ``depsproof`` is
a C++ program that links all of them and uses each one (``depsproof/``).

The build runs in the same container as the ``jsc`` build
(``tools/jsc/Dockerfile``, ``tools/jsc/README.md``): the repository mounted at
``/work``, the volume ``lazyos-jsc-work`` at ``/build``. ``--no-docker`` runs
the same steps on a Linux host with the tools.

Usage::

    python tools/webkit/deps.py                       # everything, in Docker
    python tools/webkit/deps.py --only zlib curl      # these (and what they need)
    python tools/webkit/deps.py --require             # a missing Docker/download fails
    python tools/webkit/deps.py --no-docker           # on this Linux host

Prints a JSON map ``{name: path or null}`` of the outputs
(``target/webkit/bin/depsproof``, ``target/webkit/deps.json``). Without Docker
the entries are null and the status is 0, or 1 with ``--require``; a failed
build always exits 1.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import wkpins  # noqa: E402  (also puts tools/jsc first on sys.path)
from toolenv import BuildError  # noqa: E402

OUTPUTS = ["depsproof", "deps.json"]


def library_names() -> list[str]:
    import recipes
    return recipes.ORDER


def in_container() -> bool:
    return os.environ.get("LAZYOS_JSC_IN_CONTAINER") == "1"


def run_steps(only: list[str] | None, jobs: int) -> dict[str, str | None]:
    """The real build; runs on any Linux with the tools (the container's job)."""
    import manifest
    import proof
    import recipes
    names = only or recipes.ORDER
    began = time.monotonic()
    timings: dict[str, float] = {}
    done: set[str] = set()
    for name in names:
        if name == "depsproof":
            continue
        step = time.monotonic()
        recipes.ensure(name, jobs, done)
        timings[name] = round(time.monotonic() - step, 1)
    built: dict[str, str | None] = {}
    if "depsproof" in names:
        built["depsproof"] = str(proof.build(jobs))
    manifest.write(timings, round(time.monotonic() - began, 1))
    built["deps.json"] = str(wkpins.MANIFEST)
    chown_outputs()
    return built


def chown_outputs() -> None:
    """Files made by root in a bind mount belong to the invoking user."""
    uid, gid = os.environ.get("LAZYOS_JSC_UID"), os.environ.get("LAZYOS_JSC_GID")
    if uid is None or gid is None:
        return
    for path in [wkpins.OUT, *wkpins.OUT.rglob("*")]:
        os.chown(path, int(uid), int(gid))


def load_jsc_build():
    """tools/jsc/build.py by path (a bare ``import build`` finds tools/doom's)."""
    import importlib.util
    path = wkpins.ROOT / "tools" / "jsc" / "build.py"
    spec = importlib.util.spec_from_file_location("jsc_build", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def run_in_docker(argv: list[str]) -> int:
    """Run this script in the ``jsc`` build container, repository at /work."""
    jsc_build = load_jsc_build()
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
    return subprocess.run([*command, jsc_build.IMAGE, "-I", "/work/tools/webkit/deps.py", *argv]).returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--only", nargs="+", metavar="NAME",
                        help="build only these (and what they need); see --list")
    parser.add_argument("--list", action="store_true", help="print the recipe names and exit")
    parser.add_argument("--require", action="store_true",
                        help="fail (exit 1) when Docker or a download is unavailable")
    parser.add_argument("--no-docker", action="store_true",
                        help="build on this host instead of in the container")
    parser.add_argument("--jobs", type=int, default=os.cpu_count() or 4)
    args = parser.parse_args()
    if args.list:
        print("\n".join(library_names()))
        return 0
    if not in_container() and not args.no_docker:
        forwarded = [a for a in sys.argv[1:] if a != "--no-docker"]
        code = run_in_docker(forwarded)
        shown = {n: str(wkpins.OUT / sub) if (wkpins.OUT / sub).is_file() else None
                 for n, sub in (("depsproof", "bin/depsproof"), ("deps.json", "deps.json"))}
        if code == 127 and not args.require:
            print(json.dumps({n: None for n in OUTPUTS}, indent=2))
            return 0
        if code == 0:
            print(json.dumps(shown, indent=2))
        return code
    try:
        built = run_steps(args.only, args.jobs)
    except BuildError as error:
        wkpins.log(str(error))
        return 1
    print(json.dumps(built, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
