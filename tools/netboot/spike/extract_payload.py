#!/usr/bin/env python3
"""Copy the loader's four files out of a stick image's FAT partition.

    python tools/netboot/spike/extract_payload.py --image target/lazyos-usb.img \\
        --out target/netboot/payload

Writes `kernel-x86_64`, `ramdisk`, `boot.json` and `bootx64.efi` (the stick's
`EFI/BOOT/BOOTX64.EFI`) into the output directory, prints each file's size and
SHA-256, and records them in `manifest.json` (`name`, `size`, `sha256`). The
image is only read. It reuses the FAT reader of `tools/boot/preflight.py`
(the same code that judges the stick before it is written), so what comes out
is what the firmware would read from the stick.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))  # `python -I` drops the script dir

import common  # noqa: E402
import preflight  # noqa: E402

#: MBR partition types that can hold the boot FAT (FAT12/16/32, CHS and LBA).
FAT_TYPES = (0x01, 0x04, 0x06, 0x0B, 0x0C, 0x0E)
#: What to extract: output name -> path inside the FAT.
SOURCES = {
    "kernel-x86_64": "kernel-x86_64",
    "boot.json": "boot.json",
    "ramdisk": "ramdisk",
    common.LOADER_NAME: common.LOADER_FAT_PATH,
}


class ExtractError(Exception):
    """The image is not what the stick build writes."""


def boot_fat(image: preflight.Image) -> preflight.Fat:
    """The FAT of the first MBR partition of a FAT type (the stick's `/boot`)."""
    parts = preflight.mbr_entries(image.read(0, preflight.SECTOR))
    boot = next((p for p in parts if p["type"] in FAT_TYPES), None)
    if boot is None:
        raise ExtractError("no FAT partition in the MBR (types "
                           + ", ".join(f"{p['type']:#04x}" for p in parts) + ")")
    try:
        return preflight.Fat(image, boot["start"] * preflight.SECTOR)
    except (ValueError, ArithmeticError, IndexError) as exc:
        raise ExtractError(f"unreadable FAT at LBA {boot['start']}: {exc}") from exc


def copy_file(fat: preflight.Fat, entry: tuple, dest: Path) -> tuple[int, str]:
    """Stream the cluster chain of `entry` to `dest`; `(size, sha256)`."""
    size = entry[3]
    left = size
    digest = hashlib.sha256()
    with open(dest, "wb") as out:
        for cluster in fat.chain(entry[2]):
            if left <= 0:
                break
            chunk = fat.cluster_bytes(cluster)[:left]
            out.write(chunk)
            digest.update(chunk)
            left -= len(chunk)
    if left:
        raise ExtractError(f"{dest.name}: the cluster chain ends {left} bytes short of {size}")
    return size, digest.hexdigest()


def extract(image_path: Path, out_dir: Path) -> list[dict]:
    """Extract the four files; the manifest rows, in `SOURCES` order."""
    image = preflight.Image(image_path)
    try:
        fat = boot_fat(image)
        out_dir.mkdir(parents=True, exist_ok=True)
        rows = []
        for name, fat_path in SOURCES.items():
            entry = fat.find(fat_path)
            if entry is None or entry[1] & 0x10:
                raise ExtractError(f"{fat_path} is not a file on the boot partition")
            size, sha = copy_file(fat, entry, out_dir / name)
            rows.append({"name": name, "size": size, "sha256": sha})
    finally:
        image.f.close()
    return rows


def check_loader(path: Path) -> list[dict]:
    """preflight's PE32+ x86-64 EFI application check on the extracted loader."""
    report = preflight.Report()
    preflight.check_pe(path.read_bytes(), report)
    return report.rows


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--image", default=str(common.ROOT / "target" / "lazyos-usb.img"))
    ap.add_argument("--out", default=str(common.DEFAULT_PAYLOAD))
    args = ap.parse_args(argv)
    image = Path(args.image)
    if not image.is_file():
        print(f"extract_payload: {image} not found", file=sys.stderr)
        return 2
    out = Path(args.out)
    try:
        rows = extract(image, out)
    except (ExtractError, OSError) as exc:
        print(f"extract_payload: {exc}", file=sys.stderr)
        return 1
    for row in rows:
        print(f"{row['name']:<14} {row['size']:>12} bytes  {row['sha256']}")
    (out / "manifest.json").write_text(json.dumps(rows, indent=2) + "\n")
    check = check_loader(out / common.LOADER_NAME)[0]
    print(f"{'ok' if check['ok'] else 'FAIL'}: bootx64.efi: {check['check']} ({check['detail']})")
    print(f"wrote {out / 'manifest.json'}")
    return 0 if check["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
