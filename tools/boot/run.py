#!/usr/bin/env python3
"""Boot the USB stick image headless and judge it (docs/usb-stick.md).

Builds ``target/lazyos-usb.img`` (``LAZYOS_DESKTOP=1 LAZYOS_USB=1 LAZYOS_USB_IMAGE=1 cargo
build``) unless ``--no-build``, boots it under the chosen firmware from the
chosen medium, waits for the desktop, takes a screenshot over QMP and judges
the serial markers and the pixels (``judge.py``):

    python tools/boot/run.py                              # OVMF, USB stick only
    python tools/boot/run.py --firmware bios              # SeaBIOS, USB stick only
    python tools/boot/run.py --firmware uefi --media ide  # the stick as an IDE disk
    python tools/boot/run.py --media virtio --no-build    # legacy virtio-blk
    python tools/boot/run.py --image target/lazyos.img --root nvme0p3 \\
        --media nvme --firmware bios                      # the dev image on NVMe
    python tools/boot/run.py --image target/lazyos.img --root 'virtio0p3|ata0p3' \\
        --media virtio --firmware bios                    # the dev image, same judge

``--media usb`` attaches the image as ``usb-storage`` on a ``qemu-xhci``
controller and no other disk: no IDE or virtio disk, no NIC, so the kernel
cannot read the medium and must run from the ramdisk (``FS:ROOT:ram0p2``).
Every run also puts a USB keyboard and mouse on that controller, and the judge
requires ``usbd`` to bind the keyboard (``USBD:HID:KBD``): the target PC may
have no PS/2 port. ``usbd`` claims the controller once it starts, which is
fine: the kernel never touches the stick. The drive is opened with ``snapshot=on``
unless ``--persist``, so a run never changes the image.

``--media ahci`` does the same on QEMU's AHCI controller (docs/ahci-plan.md
A3; ``--root ahci0p3``).

``--media nvme`` attaches the image to QEMU's NVMe controller as the only
disk (docs/nvme-install-plan.md N1); with the dev image and ``--root nvme0p3``
the kernel must mount its root from the NVMe disk. ``--serial-only`` judges a
non-desktop build by its serial markers alone.

Writes ``<out>/serial.log``, ``<out>/screen.png`` and ``<out>/report.json``
(markers, the seconds from QEMU start to the kernel, the root and the
desktop, and the pngstats numbers). Read the PNG to see the desktop. Exit
status is non-zero on any failure.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "screenshot"))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import judge  # noqa: E402
import pngstats  # noqa: E402
import qemu_qmp  # noqa: E402

OVMF_CODE = [Path("/usr/share/OVMF/OVMF_CODE_4M.fd"), Path("/usr/share/OVMF/OVMF_CODE.fd"),
             Path("/usr/share/edk2/ovmf/OVMF_CODE.fd"), Path("/usr/share/qemu/OVMF_CODE.fd")]
OVMF_VARS = [Path("/usr/share/OVMF/OVMF_VARS_4M.fd"), Path("/usr/share/OVMF/OVMF_VARS.fd"),
             Path("/usr/share/edk2/ovmf/OVMF_VARS.fd"), Path("/usr/share/qemu/OVMF_VARS.fd")]


def find_first(paths: list[Path], what: str, explicit: str | None) -> Path:
    if explicit:
        return Path(explicit)
    for path in paths:
        if path.is_file():
            return path
    raise SystemExit(f"no {what} found (tried {', '.join(map(str, paths))}); "
                     f"install OVMF (Debian/Ubuntu: apt install ovmf) or pass --ovmf-code/--ovmf-vars")


def firmware_args(args, scratch: Path) -> list[str]:
    """SeaBIOS is QEMU's default; UEFI is OVMF in two pflash units (a private
    copy of the variable store, so runs never share or dirty it)."""
    if args.firmware == "bios":
        return []
    code = find_first(OVMF_CODE, "OVMF code image", args.ovmf_code)
    vars_copy = scratch / "OVMF_VARS.fd"
    shutil.copyfile(find_first(OVMF_VARS, "OVMF variable store", args.ovmf_vars), vars_copy)
    return ["-drive", f"if=pflash,format=raw,unit=0,readonly=on,file={code.as_posix()}",
            "-drive", f"if=pflash,format=raw,unit=1,file={vars_copy.as_posix()}"]


def media_args(media: str, image: Path, persist: bool) -> list[str]:
    """The image as the only disk, on the requested bus, first in boot order."""
    snapshot = "" if persist else ",snapshot=on"
    drive = f"if=none,id=stick,format=raw,file={image.resolve().as_posix()}{snapshot}"
    if media == "usb":
        return ["-drive", drive,
                "-device", "usb-storage,bus=xhci.0,drive=stick,removable=on,bootindex=0"]
    if media == "ide":
        return ["-drive", drive, "-device", "ide-hd,drive=stick,bus=ide.0,bootindex=0"]
    if media == "nvme":
        # QEMU's NVMe controller (1b36:0010), the target PC's internal disk
        # (docs/nvme-install-plan.md N1). SeaBIOS and OVMF both boot from it.
        return ["-drive", drive,
                "-device", "nvme,serial=lazyos-nvme0,drive=stick,bootindex=0"]
    if media == "ahci":
        # An AHCI controller of its own (docs/ahci-plan.md A3), so it works on
        # every machine type; the disk is the only one on it.
        return ["-device", "ahci,id=ahcib", "-drive", drive,
                "-device", "ide-hd,drive=stick,bus=ahcib.0,bootindex=0"]
    return ["-drive", drive,
            "-device", "virtio-blk-pci,drive=stick,disable-modern=on,bootindex=0"]


def input_args() -> list[str]:
    """An xHCI controller with a USB keyboard and mouse and no PS/2 use: the
    stick's input path on a PC without PS/2 (``usbd``). The USB medium hangs
    off the same controller."""
    return ["-device", "qemu-xhci,id=xhci",
            "-device", "usb-kbd,bus=xhci.0", "-device", "usb-mouse,bus=xhci.0"]


def build(env_extra: dict[str, str]) -> None:
    # The stick boots straight into the desktop session (`user`, issue #623).
    env = dict(os.environ, LAZYOS_DESKTOP="1", LAZYOS_USB="1", LAZYOS_USB_IMAGE="1",
               LAZYOS_AUTOLOGIN="user", **env_extra)
    print("build: LAZYOS_DESKTOP=1 LAZYOS_USB=1 LAZYOS_USB_IMAGE=1 cargo build", flush=True)
    subprocess.run(["cargo", "build"], cwd=ROOT, env=env, check=True)


def watch(serial: Path, started: float, ready: str | None, timeout: float,
          process: subprocess.Popen) -> list[tuple[float, str]]:
    """Follow the serial log, stamping each new line with the seconds since
    QEMU started, until the ready marker, a panic, QEMU exiting or the timeout."""
    stamped: list[tuple[float, str]] = []
    seen = 0
    while time.time() - started < timeout:
        text = serial.read_text(errors="replace") if serial.exists() else ""
        lines = text.splitlines()
        now = time.time() - started
        stamped += [(now, line) for line in lines[seen:]]
        seen = len(lines)
        if (ready and ready in text) or judge.PANIC.search(text) or process.poll() is not None:
            break
        time.sleep(0.5)
    return stamped


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--firmware", choices=["uefi", "bios"], default="uefi")
    parser.add_argument("--media", choices=["usb", "ide", "virtio", "nvme", "ahci"], default="usb")
    parser.add_argument("--image", default=str(ROOT / "target" / "lazyos-usb.img"))
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--out", help="output directory (default shots/boot/<firmware>-<media>)")
    parser.add_argument("--root", default=judge.RAM_ROOT,
                        help="regex the FS:ROOT device must match (default %(default)s)")
    parser.add_argument("--ready", default=judge.READY,
                        help="serial marker that means booted (default %(default)s; '' = none)")
    parser.add_argument("--settle", type=float, default=8.0,
                        help="seconds to wait after the ready marker before the screenshot")
    parser.add_argument("--timeout", type=float, default=900.0)
    parser.add_argument("--memory", default="2G")
    parser.add_argument("--accel", default="auto", help="auto, kvm, whpx or none (TCG)")
    parser.add_argument("--persist", action="store_true",
                        help="let the guest write the image (default: snapshot=on)")
    parser.add_argument("--qemu", default=None)
    parser.add_argument("--ovmf-code", default=None)
    parser.add_argument("--ovmf-vars", default=None)
    parser.add_argument("--serial-only", action="store_true",
                        help="judge the serial markers only: no USB keyboard and no "
                             "desktop pixels required (a non-desktop image)")
    parser.add_argument("--extra-arg", action="append", default=[],
                        help="extra QEMU argument (repeatable)")
    args = parser.parse_args(argv)
    ready = args.ready or None

    if not args.no_build:
        build({})
    image = Path(args.image)
    if not image.is_file():
        raise SystemExit(f"{image} not found; build it with "
                         f"LAZYOS_DESKTOP=1 LAZYOS_USB=1 LAZYOS_USB_IMAGE=1 cargo build")
    out = Path(args.out or ROOT / "shots" / "boot" / f"{args.firmware}-{args.media}")
    out.mkdir(parents=True, exist_ok=True)
    serial = out / "serial.log"
    serial.unlink(missing_ok=True)
    qemu = qemu_qmp.find_qemu(args.qemu)
    port = qemu_qmp.free_port()

    with tempfile.TemporaryDirectory(prefix="lazyos-boot-") as scratch:
        command = [qemu, "-display", "none", "-no-reboot", "-nic", "none",
                   "-qmp", f"tcp:127.0.0.1:{port},server=on,wait=off",
                   "-serial", f"file:{serial.as_posix()}", "-m", args.memory]
        command += qemu_qmp.accel_args(args.accel, qemu)
        command += firmware_args(args, Path(scratch))
        command += input_args() + media_args(args.media, image, args.persist)
        command += args.extra_arg
        print("qemu:", " ".join(command), flush=True)
        started = time.time()
        process = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        stamped: list[tuple[float, str]] = []
        shot = out / "screen.png"
        try:
            qmp = qemu_qmp.Qmp("127.0.0.1", port, timeout=30)
            stamped = watch(serial, started, ready, args.timeout, process)
            if process.poll() is None:
                time.sleep(args.settle)
                shot = qmp.screenshot(out / "screen")
            qmp.close()
        finally:
            if process.poll() is None:
                process.kill()
            stderr = process.communicate(timeout=30)[1].decode(errors="replace")
    if stderr.strip():
        print("qemu stderr:", stderr.strip(), file=sys.stderr)

    log = serial.read_text(errors="replace") if serial.exists() else ""
    failures = judge.judge_serial(log, args.firmware, args.root, ready,
                                  usb_input=not args.serial_only)
    stats: dict = {"error": "no screenshot"}
    if shot.exists():
        _, stats, _ = pngstats.analyse_file(str(shot), None, None, None, None, None)
    if not args.serial_only:
        failures += judge.judge_pixels(stats)
    report = {
        "firmware": args.firmware,
        "media": args.media,
        "image": str(image),
        "image_mib": image.stat().st_size >> 20,
        "seconds": judge.milestones(stamped, ready),
        "media_marker": judge.MEDIA.findall(log),
        "root": judge.ROOT.findall(log),
        "screen": stats,
        "failures": failures,
    }
    (out / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    print(("FAIL: " + "; ".join(failures)) if failures else "PASS", flush=True)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
