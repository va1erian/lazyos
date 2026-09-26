#!/usr/bin/env python3
"""Build LazyOS and boot the interactive window demo in QEMU — one command.

Examples
--------
    python tools/run_demo.py                 # build (incremental) + boot windowed
    python tools/run_demo.py --no-build      # boot the existing target/lazyos.img
    python tools/run_demo.py -- --cpu max    # pass extra args to QEMU

In the demo: arrow keys move the window, Page Up / Page Down scroll the text,
Home / End jump to the top / bottom.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "screenshot"))
from qemu_qmp import find_qemu  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_IMAGE = ROOT / "target" / "lazyos.img"


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--no-build", action="store_true", help="skip `cargo build`")
    parser.add_argument("--headless", action="store_true", help="no display window")
    parser.add_argument("--image", default=str(DEFAULT_IMAGE), help="disk image to boot")
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--memory", default="256M", help="guest RAM (default: 256M)")
    parser.add_argument("qemu_args", nargs=argparse.REMAINDER,
                        help="extra QEMU args (after `--`)")
    args = parser.parse_args(argv)

    if not args.no_build:
        print("building LazyOS (incremental)…", flush=True)
        result = subprocess.run(["cargo", "build"], cwd=ROOT)
        if result.returncode != 0:
            return result.returncode

    image = Path(args.image)
    if not image.is_file():
        print(f"disk image not found: {image}\nRun without --no-build to build it.", file=sys.stderr)
        return 1

    qemu = find_qemu(args.qemu)
    command = [
        qemu,
        "-drive", f"format=raw,file={image}",
        "-m", args.memory,
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
        "-serial", "mon:stdio",
    ]
    if args.headless:
        command += ["-display", "none"]

    extra = args.qemu_args
    if extra and extra[0] == "--":
        extra = extra[1:]
    command += extra

    print("running:", " ".join(command), flush=True)
    return subprocess.call(command)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
