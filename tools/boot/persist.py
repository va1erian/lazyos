#!/usr/bin/env python3
"""`/home` on the boot stick survives a power-off (docs/usb-stick.md).

The stick image is the only disk: ``target/lazyos-usb.img`` on a
``qemu-xhci`` ``usb-storage`` device, no IDE, no virtio, no NIC. Per firmware
(OVMF and SeaBIOS by default), on a fresh copy of the image:

1. **Boot 1.** The OS runs from the ramdisk (``FS:ROOT:ram0p2``); ``usbd``
   serves the very stick it booted from as ``usb0`` and the kernel mounts its
   ``lazyhome`` partition late at ``/home``. The session logs in on the
   console as ``alice``, writes a nonce to ``/home/alice/usbnote``, reads it
   back and runs ``poweroff``.
2. **Boot 2.** Same copy: ``/home`` mounts clean, the nonce reads back, a
   second file is written, ``poweroff`` again.
3. **Host.** ``e2fsck -fn`` on the stick's home partition (MBR entry 3) is
   clean and ``debugfs`` finds both files.

The console steps are the USB storage harness's (``tools/storage/run.py``).
The image is the services profile with BusyBox for the console shell:
``LAZYOS_SERVICES=1 LAZYOS_USB=1 LAZYOS_USB_IMAGE=1 LAZYOS_USB_HOME_SIZE=64M``.

    python tools/boot/persist.py                      # build, both firmwares
    python tools/boot/persist.py --firmware bios --no-build
    python tools/boot/persist.py --accel none         # TCG: allow ~40 min

Exit status is non-zero on any failure.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import secrets
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))

import judge  # noqa: E402
from run import firmware_args  # noqa: E402

BUSYBOX = ROOT / "target/abi/busybox/busybox"
FAIL_ON = r"USBD:(FATAL|PANIC)|LazyOS PANIC"
#: The stick's home partition is MBR entry 3 (build_support/usb_stick.rs).
HOME_ENTRY = 3


def storage_steps():
    """`tools/storage/run.py`, loaded under its own name (ours is `run`)."""
    path = ROOT / "tools/storage/run.py"
    spec = importlib.util.spec_from_file_location("storage_run", path)
    module = importlib.util.module_from_spec(spec)
    # It imports its own `judge`; ours is already in sys.modules by that name.
    ours = sys.modules.pop("judge")
    sys.path.insert(0, str(path.parent))
    try:
        spec.loader.exec_module(module)
    finally:
        sys.path.remove(str(path.parent))
        sys.modules["judge"] = ours
    return module


def build() -> bool:
    busybox = os.environ.get("LAZYOS_BUSYBOX") or str(BUSYBOX)
    if not Path(busybox).is_file():
        print(f"missing {busybox}: run tools/abi/busybox.py or set LAZYOS_BUSYBOX")
        return False
    env = dict(os.environ, LAZYOS_SERVICES="1", LAZYOS_USB="1", LAZYOS_USB_IMAGE="1",
               LAZYOS_USB_HOME_SIZE="64M", LAZYOS_BUSYBOX=busybox)
    print("build: LAZYOS_SERVICES=1 LAZYOS_USB=1 LAZYOS_USB_IMAGE=1 "
          "LAZYOS_USB_HOME_SIZE=64M cargo build", flush=True)
    return subprocess.run(["cargo", "build"], cwd=ROOT, env=env).returncode == 0


def home_partition(image: Path) -> bytes:
    """The bytes of the stick's home partition, as its MBR describes them."""
    with image.open("rb") as file:
        mbr = file.read(512)
        at = 0x1BE + (HOME_ENTRY - 1) * 16
        start = int.from_bytes(mbr[at + 8:at + 12], "little")
        sectors = int.from_bytes(mbr[at + 12:at + 16], "little")
        file.seek(start * 512)
        return file.read(sectors * 512)


def host_checks(image: Path, nonce: str) -> list[str]:
    """`e2fsck -fn` and the two files, read with `debugfs` from the host."""
    failures = []
    with tempfile.TemporaryDirectory() as scratch:
        volume = Path(scratch) / "home.ext2"
        volume.write_bytes(home_partition(image))
        fsck = subprocess.run(["e2fsck", "-fn", str(volume)], capture_output=True, text=True)
        if fsck.returncode != 0:
            failures.append("e2fsck -fn is not clean:\n" + fsck.stdout.strip())
        for name, want in (("/alice/usbnote", nonce), ("/alice/second", "again")):
            cat = subprocess.run(["debugfs", "-R", f"cat {name}", str(volume)],
                                 capture_output=True, text=True)
            if cat.stdout.strip() != want:
                failures.append(f"{name} on the stick is {cat.stdout.strip()!r}, not {want!r}")
    return failures


def boot(name: str, steps: list[dict], args, firmware: list[str], stick: Path,
         out: Path) -> str:
    """One QEMU session with `stick` as the only disk; returns the serial log."""
    run_dir = out / name
    run_dir.mkdir(parents=True, exist_ok=True)
    script = out / f"{name}.json"
    script.write_text(json.dumps(steps, indent=1), encoding="utf-8")
    extra = firmware + [
        "-nic", "none", "-no-shutdown",
        "-device", "qemu-xhci,id=xhci",
        "-drive", f"if=none,id=stick,format=raw,file={stick.resolve().as_posix()}",
        "-device", "usb-storage,bus=xhci.0,drive=stick,id=usbstick0,removable=on,bootindex=0",
    ]
    command = [sys.executable, str(ROOT / "tools/screenshot/qemu_session.py"),
               "--accel", args.accel, "--memory", args.memory,
               "--timeout", str(args.timeout), "--wait-timeout", str(args.timeout),
               "--out", str(run_dir), "--script", str(script), "--fail-on", FAIL_ON,
               ] + [f"--extra-arg={arg}" for arg in extra]
    started = time.time()
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    print(f"{out.name}/{name}: session {'ok' if result.returncode == 0 else 'FAILED'} "
          f"in {time.time() - started:.0f} s", flush=True)
    if result.returncode != 0:
        print(result.stdout[-2000:] + result.stderr[-2000:])
    log = run_dir / "serial.log"
    return log.read_text(encoding="utf-8", errors="replace") if log.exists() else ""


def verdict(name: str, failures: list[str]) -> bool:
    for failure in failures:
        print(f"FAIL: {name}: {failure}")
    print(f"{name}: {'FAIL' if failures else 'PASS'}", flush=True)
    return not failures


def one_firmware(kind: str, args, storage) -> bool:
    out = args.out / kind
    out.mkdir(parents=True, exist_ok=True)
    stick = out / "stick.img"
    shutil.copyfile(args.image, stick)
    args.firmware = kind
    firmware = firmware_args(args, out)
    slow = args.accel in ("none", "tcg")
    nonce = secrets.token_hex(6)
    log = boot("boot1", storage.session(nonce, False, slow), args, firmware, stick, out)
    ok = verdict(f"{kind}/boot1", judge.judge_persist(log, kind, nonce, False))
    if ok:
        log = boot("boot2", storage.session(nonce, True, slow), args, firmware, stick, out)
        ok &= verdict(f"{kind}/boot2", judge.judge_persist(log, kind, nonce, True))
    if ok:
        ok &= verdict(f"{kind}/host", host_checks(stick, nonce))
    return ok


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--firmware", choices=["uefi", "bios", "both"], default="both")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--image", type=Path, default=ROOT / "target/lazyos-usb.img")
    parser.add_argument("--out", type=Path, default=ROOT / "shots/boot/persist")
    parser.add_argument("--accel", default="auto")
    parser.add_argument("--memory", default="2G")
    parser.add_argument("--timeout", type=float, default=2400.0)
    parser.add_argument("--ovmf-code", default=None)
    parser.add_argument("--ovmf-vars", default=None)
    args = parser.parse_args()
    if not args.no_build and not build():
        print("persist: FAIL (build)")
        return 1
    if not args.image.is_file():
        print(f"persist: FAIL ({args.image} missing)")
        return 1
    storage = storage_steps()
    kinds = ["uefi", "bios"] if args.firmware == "both" else [args.firmware]
    ok = True
    for kind in kinds:
        ok &= one_firmware(kind, args, storage)
    print("persist: " + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
