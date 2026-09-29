"""Command line: ``python -m tools.mkdisk [PATH] [--size 64M] [--label NAME]``."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from . import volume
from .geometry import BLOCK_SIZES, DEFAULT_BLOCK_SIZE


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m tools.mkdisk",
        description="Write an empty ext2 volume (pure Python; no mkfs.ext2 needed).")
    parser.add_argument("path", nargs="?", type=Path, default=volume.DEFAULT_PATH,
                        help=f"image to write (default: {volume.DEFAULT_PATH})")
    parser.add_argument("--size", default=volume.format_size(volume.DEFAULT_SIZE),
                        help="volume size, e.g. 64M, 512K, 1G (default: %(default)s)")
    parser.add_argument("--label", default=volume.DEFAULT_LABEL,
                        help="volume label, at most 16 ASCII bytes (default: %(default)s)")
    parser.add_argument("--block-size", type=int, default=DEFAULT_BLOCK_SIZE,
                        choices=BLOCK_SIZES, help="ext2 block size (default: %(default)s)")
    parser.add_argument("--force", action="store_true",
                        help="replace PATH if it already exists")
    args = parser.parse_args(argv)

    if args.path.exists() and not args.force:
        print(f"{args.path} already exists; pass --force to replace it.", file=sys.stderr)
        return 1
    try:
        size = volume.format_image(args.path, volume.parse_size(args.size), args.label,
                                   args.block_size)
    except (ValueError, OSError) as exc:
        print(f"mkdisk: {exc}", file=sys.stderr)
        return 1
    print(f"wrote {args.path} ({volume.format_size(size)}, ext2, label {args.label!r})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
