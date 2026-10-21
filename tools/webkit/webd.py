#!/usr/bin/env python3
"""Build webd (``webd-app``), the program that owns WebKit for the session and serves light views.

``docs/webd.md`` and ``docs/webd-plan.md`` (P2 slice 2). Like LazyWeb+ (``app.py``, which this
reuses) the program is one static-PIE musl program in two halves:

1. the service, a Rust static library built **on the host** with cargo for
   ``x86_64-unknown-linux-musl`` into ``target/webkit/lib``: ``xui-app/webd`` (``xui-webd-host``,
   ``libxui_webd_host.a``: the real Messenger binding around ``xui-app/crates/webd``);
2. the C++ half (``webkit/port/webd/main.cpp`` over the engine of ``webkit/port/app`` and its C ABI,
   ``abi.cpp``), linked by zig with WebKit's own static libraries and the service's library **in the
   build container** (``port.py --only webd-app``).

The result is ``target/webkit/bin/webd-app``, copied to ``target/xui/xui-webd.elf``, where
``tools/xui/core_packages.py`` finds it for the package (``xui-app/packages/webd``). Needs the port's other
programs built first (``python tools/webkit/port.py``): webd starts ``WebProcess`` and ``NetworkProcess``,
installed as ``/system/bin/webprocess`` and ``networkprocess`` by ``LAZYOS_WEBD=1`` images.

Usage::

    python tools/webkit/webd.py                # service library, then the link in Docker
    python tools/webkit/webd.py --lib-only     # just the Rust half (no Docker)
    python tools/webkit/webd.py --require      # a missing Docker or target is an error
    python tools/webkit/webd.py --no-docker    # link on this Linux host

Prints a JSON map of what it built. Without Docker or the musl target the entries are null and the
status is 0, or 1 with ``--require``; a failed build always exits 1. ``LAZYOS_JSC_VOLUME`` names the
build container's work volume, as for every WebKit step.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import app  # noqa: E402  (tools/webkit/app.py: the build steps every desktop WebKit program shares)

APP = app.App("xui-webd-host", "libxui_webd_host.a", "webd-app", "xui-webd.elf")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--lib-only", action="store_true", help="only the Rust half (no Docker needed)")
    parser.add_argument("--require", action="store_true", help="fail (exit 1) when Docker or the target is missing")
    parser.add_argument("--no-docker", action="store_true", help="link on this Linux host")
    args = parser.parse_args()

    library = app.build_window_library(APP)
    built: dict[str, str | None] = {"service": str(library) if library else None}
    if not args.lib_only and library:
        program = app.link_program(APP, args.no_docker, args.require)
        built[APP.program] = str(program) if program else None
    print(json.dumps(built, indent=2))
    missing = [name for name, path in built.items() if path is None]
    return 1 if args.require and missing else 0


if __name__ == "__main__":
    raise SystemExit(main())
