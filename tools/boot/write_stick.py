#!/usr/bin/env python3
"""Write the USB stick image to a removable disk and verify it (docs/usb-stick.md).

    python tools/boot/write_stick.py --list
    sudo python3 tools/boot/write_stick.py --device /dev/sdX          # Linux
    python tools\\boot\\write_stick.py --device \\\\.\\PhysicalDrive2    # Windows, as Administrator

Only removable or USB disks are offered or accepted. On Linux a disk with a
mounted partition is refused (unmount it first); on Windows the system and
boot disks are refused and the chosen disk is taken offline for the write
(which dismounts its volumes) and brought back online after. The tool shows
the disk's model and size and asks twice, the second time for the device name
typed back, then writes the whole image and reads it back to compare SHA-256
digests. Everything on the disk is lost.

`tools/boot/stick_gui.py` does the same, and the build, from a window.
Rufus (choose "DD Image" mode when asked) or balenaEtcher do the same job on
Windows and macOS.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
CHUNK = 4 << 20


@dataclass
class Disk:
    path: str
    size: int
    model: str
    removable: bool
    usb: bool
    mounted: list[str]
    system: bool = False
    number: int | None = None  # Windows disk number

    def describe(self) -> str:
        kind = "USB" if self.usb else "removable"
        where = f" mounted: {', '.join(self.mounted)}" if self.mounted else ""
        return f"{self.path}  {self.size / 1e9:.1f} GB  {self.model or '?'}  ({kind}){where}"


# ----- Linux -------------------------------------------------------------

def _read(path: Path) -> str:
    try:
        return path.read_text().strip()
    except OSError:
        return ""


def linux_disks() -> list[Disk]:
    mounts = [line.split()[0] for line in _read(Path("/proc/mounts")).splitlines() if line]
    swaps = [line.split()[0] for line in _read(Path("/proc/swaps")).splitlines()[1:] if line]
    disks = []
    for block in sorted(Path("/sys/block").iterdir()):
        name = block.name
        if name.startswith(("loop", "ram", "zram", "dm-", "md", "sr")):
            continue
        removable = _read(block / "removable") == "1"
        usb = "/usb" in os.path.realpath(block / "device")
        if not (removable or usb):
            continue
        sectors = int(_read(block / "size") or 0)
        model = " ".join(filter(None, (_read(block / "device" / "vendor"),
                                       _read(block / "device" / "model"))))
        dev = f"/dev/{name}"
        used = sorted({m for m in mounts + swaps
                       if m == dev or (m.startswith(dev) and m[len(dev):].lstrip("p").isdigit())})
        disks.append(Disk(dev, sectors * 512, model, removable, usb, used))
    return disks


# ----- Windows -----------------------------------------------------------

POWERSHELL_LIST = (
    "Get-Disk | ForEach-Object { $d = $_; "
    "$letters = @(Get-Partition -DiskNumber $d.Number -ErrorAction SilentlyContinue | "
    "Where-Object DriveLetter | ForEach-Object { \"$($_.DriveLetter):\" }); "
    "[pscustomobject]@{Number=$d.Number; Size=$d.Size; Model=$d.FriendlyName; "
    "BusType=\"$($d.BusType)\"; IsBoot=$d.IsBoot; IsSystem=$d.IsSystem; Letters=$letters} } "
    "| ConvertTo-Json -Depth 3"
)


def powershell(command: str) -> str:
    result = subprocess.run(["powershell", "-NoProfile", "-Command", command],
                            capture_output=True, text=True, check=True)
    return result.stdout


def windows_disks() -> list[Disk]:
    raw = json.loads(powershell(POWERSHELL_LIST) or "[]")
    disks = []
    for entry in raw if isinstance(raw, list) else [raw]:
        usb = entry.get("BusType") in ("USB", "7")
        removable = usb or entry.get("BusType") in ("SD", "MMC")
        if not removable:
            continue
        disks.append(Disk(rf"\\.\PhysicalDrive{entry['Number']}", int(entry["Size"]),
                          entry.get("Model") or "", removable, usb,
                          list(entry.get("Letters") or []),
                          system=bool(entry.get("IsBoot") or entry.get("IsSystem")),
                          number=int(entry["Number"])))
    return disks


def windows_offline(disk: Disk, offline: bool) -> None:
    state = "$true" if offline else "$false"
    powershell(f"Set-Disk -Number {disk.number} -IsOffline {state}")


# ----- Common ------------------------------------------------------------

def list_disks() -> list[Disk]:
    return windows_disks() if os.name == "nt" else linux_disks()


def refusal(disk: Disk, image_size: int) -> str | None:
    """Why `disk` must not be written, or None."""
    if not (disk.removable or disk.usb):
        return "it is not a removable or USB disk"
    if disk.system:
        return "it is the system or boot disk"
    if disk.mounted and os.name != "nt":
        return f"it is in use ({', '.join(disk.mounted)}); unmount it first"
    if disk.size < image_size:
        return f"it holds {disk.size} bytes, the image needs {image_size}"
    return None


def confirm(disk: Disk, image: Path) -> bool:
    print(f"\nAbout to write {image} ({image.stat().st_size >> 20} MiB) to:\n  {disk.describe()}")
    print("EVERYTHING on this disk will be destroyed.")
    if os.name == "nt" and disk.mounted:
        print(f"Its volumes ({', '.join(disk.mounted)}) are dismounted: the disk goes offline "
              "for the write.")
    if input("Continue? [y/N] ").strip().lower() not in ("y", "yes"):
        return False
    typed = input(f"Type the device name ({disk.path}) to confirm: ").strip()
    return typed == disk.path


def write_and_verify(disk: Disk, image: Path) -> None:
    size = image.stat().st_size
    written = hashlib.sha256()
    flags = os.O_WRONLY | getattr(os, "O_BINARY", 0)
    fd = os.open(disk.path, flags)
    try:
        with image.open("rb") as source:
            done = 0
            while chunk := source.read(CHUNK):
                if len(chunk) % 512:
                    chunk += bytes(512 - len(chunk) % 512)  # raw devices take whole sectors
                os.write(fd, chunk)
                written.update(chunk)
                done += len(chunk)
                print(f"\rwrote {done >> 20} / {size >> 20} MiB", end="", flush=True)
        os.fsync(fd)
    finally:
        os.close(fd)
    print()
    fd = os.open(disk.path, os.O_RDONLY | getattr(os, "O_BINARY", 0))
    try:
        if hasattr(os, "posix_fadvise"):
            os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)  # read the device, not the cache
        back = hashlib.sha256()
        left = done
        while left:
            chunk = os.read(fd, min(CHUNK, left))
            if not chunk:
                break
            back.update(chunk)
            left -= len(chunk)
            print(f"\rverified {(done - left) >> 20} / {done >> 20} MiB", end="", flush=True)
    finally:
        os.close(fd)
    print()
    if back.hexdigest() != written.hexdigest():
        raise SystemExit("VERIFY FAILED: the disk does not read back what was written")
    print(f"verified: sha256 {written.hexdigest()}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--image", default=str(ROOT / "target" / "lazyos-usb.img"))
    parser.add_argument("--device", help="the disk to write (/dev/sdX or \\\\.\\PhysicalDriveN)")
    parser.add_argument("--list", action="store_true", help="list removable disks and exit")
    parser.add_argument("--yes", action="store_true",
                        help="skip the two questions (the GUI, stick_gui.py, asks them itself)")
    args = parser.parse_args(argv)

    image = Path(args.image)
    disks = list_disks()
    if args.list or not args.device:
        if not disks:
            print("no removable or USB disks found")
        for disk in disks:
            reason = refusal(disk, image.stat().st_size if image.is_file() else 0)
            print(disk.describe() + (f"   [refused: {reason}]" if reason else ""))
        if not args.device:
            print("\npass --device <path> to write one")
        return 0
    if not image.is_file():
        raise SystemExit(f"{image} not found; build it with "
                         "LAZYOS_DESKTOP=1 LAZYOS_USB_IMAGE=1 cargo build")
    disk = next((d for d in disks if d.path.lower() == args.device.lower()), None)
    if disk is None:
        raise SystemExit(f"{args.device} is not a removable or USB disk (see --list)")
    reason = refusal(disk, image.stat().st_size)
    if reason:
        raise SystemExit(f"refusing {disk.path}: {reason}")
    if not args.yes and not confirm(disk, image):
        print("nothing written")
        return 1
    if os.name == "nt":
        windows_offline(disk, True)
    try:
        write_and_verify(disk, image)
    finally:
        if os.name == "nt":
            windows_offline(disk, False)
    print("done: unplug the stick, plug it into the PC and pick it in the boot menu "
          "(F8 on ASUS boards); see docs/usb-stick.md")
    return 0


if __name__ == "__main__":
    sys.exit(main())
