#!/usr/bin/env python3
"""Package a LazyRAD project as a LazyOS ``.lzp`` on the host.

The packages LazyOS ships for its LazyRAD demo apps are built here, the way the
IDE's *File -> Make LazyOS App* builds them on LazyOS: the project is checked,
the player (``target/lazyrad/lrplay.elf``, from ``tools/lazyrad/build.py``) is
copied in, and the manifest declares what the scripts use (the LazyOS
platform's derivation, so a song-playing app gets ``os.lazy.audio.v1``). The
work is ``lazyrad-os/examples/lzpack.rs``; this script picks the inputs.

Usage::

    python tools/lazyrad/package.py                    # the MOD player -> target/pkg/MODPLAY.LZP
    python tools/lazyrad/package.py --app modplayer --no-build
    python tools/lazyrad/package.py --project <dir> --out x.lzp --system-name user.me.x

``--require`` makes a missing musl toolchain an error instead of a skip (the
image build asked for the package, so it must exist).
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
MANIFEST = ROOT / "lazyrad-os" / "Cargo.toml"
PLAYER = ROOT / "target" / "lazyrad" / "lrplay.elf"
PKG_DIR = ROOT / "target" / "pkg"

#: The demo apps shipped as packages: name -> (project, package, id, description).
APPS = {
    "modplayer": (
        ROOT / "lazyrad-os" / "samples" / "modplayer",
        PKG_DIR / "MODPLAY.LZP",
        "org.lazy.modplayer",
        "A ProTracker MOD player made with LazyRAD",
    ),
}


def lzpack_command(project: Path, out: Path, system_name: str | None,
                   description: str | None) -> list[str]:
    """The cargo command that runs the packager example."""
    command = ["cargo", "run", "--quiet", "--release", "--manifest-path", str(MANIFEST),
               "--example", "lzpack", "--", str(project), "--player", str(PLAYER),
               "--out", str(out)]
    if system_name:
        command += ["--system-name", system_name]
    if description:
        command += ["--description", description]
    return command


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--app", choices=sorted(APPS), default="modplayer",
                        help="a demo app to package (default: modplayer)")
    parser.add_argument("--project", type=Path, help="package this project instead")
    parser.add_argument("--out", type=Path, help="the .lzp to write")
    parser.add_argument("--system-name", help="the app id (reverse DNS)")
    parser.add_argument("--description", help="a one-line description")
    parser.add_argument("--no-build", action="store_true",
                        help="use the existing target/lazyrad/lrplay.elf")
    parser.add_argument("--require", action="store_true",
                        help="fail (not skip) when the player cannot be built")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    project, out, system_name, description = APPS[args.app]
    if args.project:
        project, system_name, description = args.project, None, None
        if not args.out:
            print("error: --project needs --out", file=sys.stderr)
            return 2
    out = args.out or out
    system_name = args.system_name or system_name
    description = args.description or description

    if not args.no_build:
        build = subprocess.run([sys.executable, str(ROOT / "tools" / "lazyrad" / "build.py"),
                                "--bin", "lrplay"], cwd=ROOT)
        if build.returncode != 0:
            return build.returncode
    if not PLAYER.is_file():
        level = "error" if args.require else "warning"
        print(f"{level}: {PLAYER} is missing (no musl toolchain?); no package built",
              file=sys.stderr)
        return 1 if args.require else 0
    packed = subprocess.run(lzpack_command(project, out, system_name, description), cwd=ROOT)
    return packed.returncode


if __name__ == "__main__":
    raise SystemExit(main())
