"""Command line: ``python -m tools.mkdisk [PATH] [--size 64M] [--label NAME] [--no-seed | --home-volume]``."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from . import layout, volume
from .geometry import BLOCK_SIZES, DEFAULT_BLOCK_SIZE


def octal_mode(text: str) -> int:
    """Parse a permission mode such as ``755``, ``0755`` or ``1777`` (always octal)."""
    try:
        return int(text.removeprefix("0o"), 8)
    except ValueError:
        raise argparse.ArgumentTypeError(f"{text!r} is not an octal mode (try 755 or 1777)")


def build_layout(args: argparse.Namespace) -> layout.Layout:
    """The directory layout the flags ask for."""
    if args.home_volume:
        return layout.home_volume(args.root_mode, args.root_uid, args.root_gid)
    if args.seed:
        return layout.seeded(args.root_mode, args.root_uid, args.root_gid)
    return layout.Layout(args.root_mode, args.root_uid, args.root_gid)


def make_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="python -m tools.mkdisk",
        description="Write an ext2 volume (pure Python; no mkfs.ext2 needed).")
    parser.add_argument("path", nargs="?", type=Path, default=volume.DEFAULT_PATH,
                        help=f"image to write (default: {volume.DEFAULT_PATH})")
    parser.add_argument("--size", default=volume.format_size(volume.DEFAULT_SIZE),
                        help="volume size, e.g. 64M, 512K, 1G (default: %(default)s)")
    parser.add_argument("--label", default=None,
                        help="volume label, at most 16 ASCII bytes (default: "
                             f"{volume.DEFAULT_LABEL}, or {volume.HOME_LABEL} with --home-volume)")
    parser.add_argument("--block-size", type=int, default=DEFAULT_BLOCK_SIZE,
                        choices=BLOCK_SIZES, help="ext2 block size (default: %(default)s)")
    parser.add_argument("--root-mode", type=octal_mode, default=0o755, metavar="MODE",
                        help="octal mode of the volume root, i.e. /data (default: 755)")
    parser.add_argument("--root-uid", type=int, default=0,
                        help="owner uid of the volume root (default: %(default)s)")
    parser.add_argument("--root-gid", type=int, default=0,
                        help="owner gid of the volume root (default: %(default)s)")
    parser.add_argument("--seed", action=argparse.BooleanOptionalAction, default=True,
                        help="create /home/<user> for the demo accounts and a sticky /tmp "
                             "(default: on; --no-seed formats a bare volume; "
                             "--home-volume overrides it)")
    parser.add_argument("--home-volume", action="store_true",
                        help="seed <user>/ at the volume root for the demo accounts and no "
                             "/home or /tmp, label lazyhome: the volume LazyOS mounts at /home")
    parser.add_argument("--force", action="store_true",
                        help="replace PATH if it already exists")
    return parser


def main(argv: list[str]) -> int:
    args = make_parser().parse_args(argv)
    if args.path.exists() and not args.force:
        print(f"{args.path} already exists; pass --force to replace it.", file=sys.stderr)
        return 1
    label = args.label or (volume.HOME_LABEL if args.home_volume else volume.DEFAULT_LABEL)
    try:
        plan = build_layout(args)
        size = volume.format_image(args.path, volume.parse_size(args.size), label,
                                   args.block_size, plan)
    except (ValueError, OSError) as exc:
        print(f"mkdisk: {exc}", file=sys.stderr)
        return 1
    print(f"wrote {args.path} ({volume.format_size(size)}, ext2, label {label!r})")
    print(layout.describe(plan))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
