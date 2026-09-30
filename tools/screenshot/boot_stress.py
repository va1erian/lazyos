#!/usr/bin/env python3
"""Boot one image many times and count the boots that never become ready.

An intermittent boot hang (issue #382: about one desktop boot in sixteen on
the CI runner under KVM) only shows up across many boots, so this runs
``qemu_session.py`` ``--boots`` times, ``--parallel`` at a time, each waiting
for ``--marker`` on serial. Every boot runs with ``-snapshot``, so parallel
instances share the image read-only.

A boot that times out goes through ``qemu_session.py``'s failure path, which
captures ``info registers`` over QMP and injects an NMI so the kernel prints
its ``HANG:`` report (``kernel/src/arch/nmi.rs``) before QEMU quits. Failed
boots keep their whole output directory; passing boots keep only their serial
log, so a long run stays small.

    python tools/screenshot/boot_stress.py --image target/lazyos.img \
        --boots 40 --parallel 4 --marker TERM:UP:PASS --out shots/stress

Prints ``BOOTSTRESS: boots=N ready=R hung=H`` and exits non-zero when any
boot hung, so CI can use it as a gate.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

SESSION = Path(__file__).with_name("qemu_session.py")


def write_script(out: Path, marker: str, timeout: float) -> Path:
    """The one-gate session every boot runs: wait for readiness, then quit."""
    script = out / "boot_stress.json"
    steps = [{"wait_for": marker, "timeout": timeout}, {"quit": True}]
    script.write_text(json.dumps(steps), encoding="utf-8")
    return script


def boot_once(index: int, args: argparse.Namespace, script: Path) -> tuple[int, bool]:
    """Run one boot; returns ``(index, ready)``."""
    out = Path(args.out) / f"boot_{index:03d}"
    command = [
        sys.executable, str(SESSION), "--image", args.image, "--out", str(out),
        "--script", str(script), "--accel", args.accel,
        "--wait-timeout", str(args.timeout), "--extra-arg=-snapshot",
    ]
    command += [f"--extra-arg={arg}" for arg in args.extra_arg]
    log = out.with_suffix(".log")
    out.parent.mkdir(parents=True, exist_ok=True)
    with log.open("w", encoding="utf-8") as sink:
        result = subprocess.run(command, stdout=sink, stderr=subprocess.STDOUT)
    ready = result.returncode == 0
    if ready:
        prune(out, log)
    print(f"boot {index:03d}: {'ready' if ready else 'HUNG'}", flush=True)
    return index, ready


def prune(out: Path, log: Path) -> None:
    """Keep only a passing boot's serial log."""
    serial = out / "serial.log"
    if serial.exists():
        shutil.copyfile(serial, out.with_suffix(".serial.log"))
    shutil.rmtree(out, ignore_errors=True)
    log.unlink(missing_ok=True)


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--image", required=True, help="raw disk image to boot")
    parser.add_argument("--boots", type=int, default=16, help="number of boots")
    parser.add_argument("--parallel", type=int, default=4, help="concurrent boots")
    parser.add_argument("--marker", default="TERM:UP:PASS",
                        help="serial marker that means the boot is ready")
    parser.add_argument("--timeout", type=float, default=90.0,
                        help="seconds to wait for the marker per boot")
    parser.add_argument("--accel", default="auto",
                        choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--extra-arg", action="append", default=[], metavar="ARG",
                        help="extra QEMU argument; repeat for multiple")
    parser.add_argument("--out", default="shots/boot_stress", help="output directory")
    args = parser.parse_args()

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    script = write_script(out, args.marker, args.timeout)
    started = time.time()
    with ThreadPoolExecutor(max_workers=max(1, args.parallel)) as pool:
        results = list(pool.map(lambda i: boot_once(i, args, script), range(args.boots)))
    hung = sorted(index for index, ready in results if not ready)
    ready = len(results) - len(hung)
    print(f"BOOTSTRESS: boots={len(results)} ready={ready} hung={len(hung)} "
          f"seconds={time.time() - started:.0f}", flush=True)
    for index in hung:
        print(f"BOOTSTRESS: hung boot {index:03d}: {out / f'boot_{index:03d}'}")
    return 1 if hung else 0


if __name__ == "__main__":
    raise SystemExit(main())
