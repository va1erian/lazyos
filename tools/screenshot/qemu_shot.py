#!/usr/bin/env python3
"""Boot a disk image (or bare firmware) in headless QEMU and capture PNG
screenshots through QMP.

For scripted interaction (typing, mouse, timed captures), see
``qemu_session.py``.

Usage
-----
    python tools/screenshot/qemu_shot.py --image target/lazyos.img --out shots --at 2,5,10
    python tools/screenshot/qemu_shot.py --out shots --at 2,5   # firmware smoke test

Outputs
-------
    <out>/shot_<t>s.png   one screenshot per requested time
    <out>/serial.log      serial console output (COM1)
    <out>/summary.json    machine-readable manifest
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

from qemu_qmp import (Qmp, accel_args, add_data_disk_option, add_home_disk_option,
                      build_qemu_command, existing_data_disk, existing_home_disk,
                      find_qemu, free_port)


def parse_times(raw: str) -> list[float]:
    times = []
    for piece in raw.split(","):
        piece = piece.strip()
        if not piece:
            continue
        value = float(piece)
        if value < 0:
            sys.exit(f"--at values must be >= 0 (got {value})")
        times.append(value)
    if not times:
        sys.exit("--at must contain at least one timestamp")
    return times


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--image", help="raw disk image to boot (omit to boot firmware only)")
    parser.add_argument("--out", default="shots", help="output directory (default: shots)")
    parser.add_argument("--at", default="3,6,10", help="comma-separated capture times in seconds")
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--timeout", type=float, default=180.0, help="QMP/overall timeout in seconds")
    parser.add_argument("--memory", default="256M", help="guest RAM (default: 256M)")
    parser.add_argument("--accel", default="auto",
                        choices=["auto", "none", "tcg", "whpx", "kvm"],
                        help="QEMU accelerator (auto: whpx/kvm if available)")
    parser.add_argument("--extra-arg", action="append", default=[], metavar="ARG",
                        help="extra QEMU argument; repeat for multiple")
    add_data_disk_option(parser)
    add_home_disk_option(parser)
    args = parser.parse_args()
    data_disk = existing_data_disk(args.data_disk)
    home_disk = existing_home_disk(args.home_disk)

    qemu = find_qemu(args.qemu)
    out_dir = Path(args.out).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    times = parse_times(args.at)
    serial_log = out_dir / "serial.log"

    image = None
    if args.image:
        image_path = Path(args.image).resolve()
        if not image_path.is_file():
            sys.exit(f"--image not found: {image_path}")
        image = str(image_path)

    port = free_port()
    extra = list(args.extra_arg) + accel_args(args.accel, qemu)
    command = build_qemu_command(qemu, image, port, serial_log, args.memory, extra,
                                 data_disk, home_disk=home_disk)
    print(f"launching: {' '.join(command)}", flush=True)
    proc = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

    screenshots: list[str] = []
    qmp: Qmp | None = None
    try:
        qmp = Qmp("127.0.0.1", port, args.timeout)
        started = time.time()
        for moment in times:
            remaining = moment - (time.time() - started)
            if remaining > 0:
                time.sleep(remaining)
            shot = qmp.screenshot(out_dir / f"shot_{moment:g}s")
            screenshots.append(shot.name)
            print(f"captured {shot}", flush=True)
        try:
            qmp.execute("quit")
        except Exception:
            pass
    finally:
        if qmp is not None:
            qmp.close()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()

    summary = {
        "qemu": qemu,
        "image": image,
        "screenshots": screenshots,
        "serial_log": serial_log.name if serial_log.exists() else None,
        "exit_code": proc.returncode,
    }
    (out_dir / "summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
