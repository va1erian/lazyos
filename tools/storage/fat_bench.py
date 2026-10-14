#!/usr/bin/env python3
"""How fast is a FAT stick through the whole path? (docs/usb-hotplug-plan.md
P6: the FUSE round trip is the cost to measure.)

Boots the `fat_run.py` image with a FAT32 stick plugged in, then writes and
reads a file of SIZE MiB under `/media/MY_STICK` with BusyBox `dd` (64 KiB
blocks, as the kernel splits FUSE requests) and prints the guest-side rates
from the serial log's timestamps of the markers the commands echo. QEMU's
`usb-storage` is a host file behind a USB 2 era emulation, so the numbers
bound the software path (VFS, FUSE, `fatfuse`, `libs/fatvol`, `usbd`), not a
real stick.

    python tools/storage/fat_bench.py [--size 16] [--no-build] [--accel auto]
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))

import fat_run  # noqa: E402
import run as storage  # noqa: E402
import stick  # noqa: E402


def session(mib: int, slow: bool) -> list[dict]:
    step = 600 if slow else 240
    blocks = mib * 16
    steps = storage.login(slow)
    steps += [{"wait": 1.0}, fat_run.plug("fat0", "usbfat0"),
              {"wait_for": f"FATFUSE:UP {fat_run.MOUNT}", "timeout": 600 if slow else 90}]
    # `date +%s` before and after, printed on one line each so the host can
    # subtract them; the data goes to the stick, then is read back.
    big = f"{fat_run.MOUNT}/bench.bin"
    steps += storage.command(f"date +%s; dd if=/dev/zero of={big} bs=65536 count={blocks} 2>&1; "
                             "date +%s; echo W-DONE", r"(?m)^W-DONE", step)
    steps += storage.command(f"date +%s; dd if={big} of=/dev/null bs=65536 2>&1; "
                             "date +%s; echo R-DONE", r"(?m)^R-DONE", step)
    steps += storage.command("poweroff", "INIT:SHUTDOWN:BEGIN", step)
    steps += [{"wait_for": "power: filesystems synced", "timeout": 600 if slow else 120},
              {"wait": 1.0}, {"quit": True}]
    return steps


def rates(log: str, mib: int) -> list[str]:
    out = []
    for label, marker in (("write", "W-DONE"), ("read", "R-DONE")):
        text = log.split(marker)[0] if marker in log else ""
        stamps = re.findall(r"(?m)^(\d{10})\r?$", text)
        if len(stamps) >= 2:
            seconds = max(int(stamps[-1]) - int(stamps[-2]), 1)
            out.append(f"{label}: {mib} MiB in {seconds} s = {mib / seconds:.1f} MiB/s "
                       "(whole-second resolution)")
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--size", type=int, default=16, help="MiB to write and read")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--image", type=Path, default=ROOT / "target/lazyos.img")
    parser.add_argument("--out", type=Path, default=ROOT / "shots/storage_bench")
    parser.add_argument("--accel", default="auto")
    parser.add_argument("--timeout", type=float, default=2400.0)
    args = parser.parse_args()
    if not args.no_build and not storage.build():
        return 1
    args.out.mkdir(parents=True, exist_ok=True)
    home = args.out / "stick.img"
    stick.make_stick(home)
    fat = args.out / "fat.img"
    # Room for the file: a FAT32 stick of twice the size, at least 64 MiB.
    size = f"{max(64, args.size * 2 + 16)}M"
    subprocess.run(fat_run.fatstick() + ["make", str(fat), "--size", size,
                                         "--label", fat_run.LABEL], cwd=ROOT, check=True)
    slow = args.accel in ("none", "tcg")
    steps = session(args.size, slow)
    ns = argparse.Namespace(out=args.out, image=args.image, accel=args.accel, timeout=args.timeout)
    started = time.time()
    log = fat_run.boot(ns, home, fat, steps)
    print(f"session took {time.time() - started:.0f} s")
    for line in rates(log, args.size) or ["no rates found in the log"]:
        print(line)
    return 0


if __name__ == "__main__":
    sys.exit(main())
