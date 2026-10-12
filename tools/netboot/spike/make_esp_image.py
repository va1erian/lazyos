#!/usr/bin/env python3
"""Build `esp.img`, the ESP-only disk image of the spike's `sanboot` fallback.

    python tools/netboot/spike/make_esp_image.py --payload target/netboot/payload

An MBR disk with one FAT16 partition (type 0x0E) holding what the stick's
FAT holds for the loader: `EFI/BOOT/BOOTX64.EFI` (from `bootx64.efi`),
`kernel-x86_64`, `boot.json` and `ramdisk`. `serve.py` serves it as
`/files/esp.img`, and `/sanboot.ipxe` boots it. The image is written by
`fat16_format.py` and read back here with the FAT reader of
`tools/boot/preflight.py`: every file's size and SHA-256 must match its source.
"""

from __future__ import annotations

import argparse
import hashlib
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))  # `python -I` drops the script dir

import common  # noqa: E402
import extract_payload  # noqa: E402
import fat16_format  # noqa: E402
import preflight  # noqa: E402


def layout(payload: Path) -> dict[str, Path]:
    """Path inside the volume -> source file."""
    files = {common.LOADER_FAT_PATH: payload / common.LOADER_NAME}
    files.update({name: payload / name for name in common.PAYLOAD_FILES})
    missing = [str(p) for p in files.values() if not p.is_file()]
    if missing:
        raise fat16_format.FormatError("missing: " + ", ".join(missing))
    return files


def read_back(image_path: Path, files: dict[str, Path]) -> list[str]:
    """Problems found reading `image_path` back; empty when every file matches its source."""
    problems = []
    image = preflight.Image(image_path)
    try:
        fat = extract_payload.boot_fat(image)
        for dest, source in files.items():
            entry = fat.find(dest)
            if entry is None:
                problems.append(f"{dest}: not found")
                continue
            digest, left = hashlib.sha256(), entry[3]
            for cluster in fat.chain(entry[2]):
                chunk = fat.cluster_bytes(cluster)[:left]
                digest.update(chunk)
                left -= len(chunk)
            if left or entry[3] != source.stat().st_size or digest.hexdigest() != common.sha256_file(source):
                problems.append(f"{dest}: contents differ from {source.name}")
        for directory in ("EFI", "EFI/BOOT"):
            if fat.first_entries(directory) != [".", ".."]:
                problems.append(f"{directory}: '.' and '..' are not the first entries")
    finally:
        image.f.close()
    return problems


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--payload", default=str(common.DEFAULT_PAYLOAD),
                    help="the directory extract_payload.py wrote (default %(default)s)")
    ap.add_argument("--out", help="the image (default PAYLOAD/esp.img)")
    ap.add_argument("--label", default="LAZYOSESP")
    args = ap.parse_args(argv)
    payload = Path(args.payload)
    out = Path(args.out) if args.out else payload / "esp.img"
    try:
        files = layout(payload)
        info = fat16_format.format_image(out, files, args.label)
        problems = read_back(out, files)
    except (fat16_format.FormatError, extract_payload.ExtractError, OSError) as exc:
        print(f"make_esp_image: {exc}", file=sys.stderr)
        return 1
    print(f"{out}: {common.mib(info['bytes'])}, FAT16 {info['clusters']} clusters of "
          f"{info['spc'] * 512} bytes, partition at LBA {info['part_start']}")
    for problem in problems:
        print(f"FAIL: {problem}", file=sys.stderr)
    if not problems:
        print("read back with preflight's FAT reader: every file matches")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
