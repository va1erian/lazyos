#!/usr/bin/env python3
"""Build LazyOS and boot the interactive CLI demo in QEMU — one command.

By default uses the **dev** profile (kernel at O2, dependencies at O3). Measured
in QEMU, that is the fastest configuration: the fully optimized **release**
profile (O3 + fat LTO) is *slower* under QEMU's TCG emulation for the
floating-point rasterizer, though it should win on real hardware. Pass
``--release`` to build it anyway.

Examples
--------
    python tools/run_demo.py                 # dev build (fast in QEMU) + boot
    python tools/run_demo.py --release       # optimized build for real hardware
    python tools/run_demo.py --no-build      # boot the existing target/lazyos.img
    python tools/run_demo.py -- --cpu max    # pass extra args to QEMU
    python tools/run_demo.py --reset-data    # wipe the persistent data disk first
    python tools/run_demo.py --no-data-disk  # boot with only the boot disk

A persistent ext2 data disk (default ``target/data.img``, 64 MiB) is attached as
a second virtio-blk device. It is created on first use and never regenerated
unless you pass ``--reset-data``. A fresh volume is seeded with ``/data/home/<user>``
for the demo accounts (owned by them) and a sticky ``/data/tmp``; everything else
under ``/data`` is root-only, so log in as ``alice`` to write to your own home.

In the demo: two windows run concurrently (a demo program and the `sh`
interpreter). Press Tab to move focus (green border); typed input goes to the
focused program.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "screenshot"))
from qemu_qmp import accel_args, data_disk_args, find_qemu  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent))
import mkdisk  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_IMAGE = ROOT / "target" / "lazyos.img"


def confirm(question: str) -> bool:
    """Ask on the terminal; anything but an explicit yes (or no TTY) is a no."""
    if not sys.stdin.isatty():
        return False
    try:
        return input(f"{question} [y/N] ").strip().lower() in ("y", "yes")
    except EOFError:  # e.g. stdin redirected from the null device
        return False


def prepare_data_disk(path: Path, reset: bool, assume_yes: bool) -> bool:
    """Make sure the data volume exists, resetting it only when asked to.

    Returns ``False`` when the user declined an explicit reset. Never
    regenerates an existing volume implicitly: that would destroy user data.
    """
    if reset and path.exists():
        plan = mkdisk.seeded()
        question = (f"Erase {path} and format a fresh volume containing:\n"
                    f"{mkdisk.describe(plan)}\nProceed?")
        if not assume_yes and not confirm(question):
            print("data disk left untouched; aborting.", file=sys.stderr)
            return False
        mkdisk.format_image(path, layout=plan)
        print(f"reset data disk: {mkdisk.status(path).describe()}", flush=True)
    elif mkdisk.ensure_volume(path):
        print(f"created data disk: {mkdisk.status(path).describe()}", flush=True)
    return True


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--no-build", action="store_true", help="skip `cargo build`")
    parser.add_argument("--release", action="store_true",
                        help="build the optimized release profile (slower in QEMU)")
    parser.add_argument("--headless", action="store_true", help="no display window")
    parser.add_argument("--image", default=str(DEFAULT_IMAGE), help="disk image to boot")
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--memory", default="256M", help="guest RAM (default: 256M)")
    parser.add_argument("--accel", default="auto",
                        choices=["auto", "none", "tcg", "whpx", "kvm"],
                        help="QEMU accelerator; auto uses whpx/kvm when available "
                             "(many times faster than TCG)")
    parser.add_argument("--disk", default="virtio", choices=["virtio", "ata"],
                        help="boot disk bus: virtio-blk (DMA, fast) or legacy IDE/ATA PIO")
    parser.add_argument("--data-disk", default=str(mkdisk.DEFAULT_PATH), metavar="PATH",
                        help="persistent ext2 data volume, attached as a second virtio-blk "
                             "device and created if missing (default: %(default)s)")
    parser.add_argument("--no-data-disk", action="store_true",
                        help="do not attach a data volume")
    parser.add_argument("--reset-data", action="store_true",
                        help="regenerate the data volume with the seeded layout "
                             "(asks first unless --yes)")
    parser.add_argument("--yes", "-y", action="store_true",
                        help="answer yes to the --reset-data confirmation")
    parser.add_argument("qemu_args", nargs=argparse.REMAINDER,
                        help="extra QEMU args (after `--`)")
    args = parser.parse_args(argv)
    if args.no_data_disk and args.reset_data:
        parser.error("--reset-data conflicts with --no-data-disk")

    if not args.no_build:
        cargo = ["cargo", "build"]
        profile = "dev (optimized deps; fastest in QEMU)"
        if args.release:
            cargo.append("--release")
            profile = "release (optimized for real hardware)"
        print(f"building LazyOS [{profile}]…", flush=True)
        result = subprocess.run(cargo, cwd=ROOT)
        if result.returncode != 0:
            return result.returncode

    image = Path(args.image)
    if not image.is_file():
        print(f"disk image not found: {image}\nRun without --no-build to build it.", file=sys.stderr)
        return 1

    data_disk = None if args.no_data_disk else Path(args.data_disk)
    if data_disk and not prepare_data_disk(data_disk, args.reset_data, args.yes):
        return 1

    qemu = find_qemu(args.qemu)
    command = [
        qemu,
        "-m", args.memory,
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
        "-serial", "mon:stdio",
    ]
    # virtio-blk is DMA-based; the IDE/PIO path costs a VM exit per 16 bits read,
    # which made loading the ~2.7 MB desktop ELFs take tens of seconds.
    if args.disk == "virtio":
        command += ["-drive", f"format=raw,file={image},if=none,id=boot",
                    "-device", "virtio-blk-pci,drive=boot"]
    else:
        command += ["-drive", f"format=raw,file={image}"]
    if data_disk:
        command += data_disk_args(data_disk)
    command += accel_args(args.accel, qemu)
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
