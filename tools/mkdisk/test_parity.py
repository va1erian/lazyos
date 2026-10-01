#!/usr/bin/env python3
"""The Rust formatter (libs/ext2fs) and the Python one (tools/mkdisk) must agree.

Both write "the same" empty revision-1 ext2 volume: this builds one with each
from the same size, block size, label, uuid and timestamp and compares the
files. Run: ``python tools/mkdisk/test_parity.py`` (needs ``cargo``; the Rust
side is ``libs/ext2fs/examples/mkimage.rs``).

The comparison is of whole files, so it covers every byte the formatters write:
superblock and its backups (feature flags, uuid at 0x68, label at 0x78, counters),
descriptor tables, bitmaps, the reserved inodes, and the root and ``lost+found``
blocks. A mismatch names the first differing bytes and which structure they
belong to.

Intentional differences: none. (The Python seeded layouts add directories the
Rust ``bare`` mode does not, so the comparison uses ``layout.EMPTY``.) If one
ever becomes necessary, add it to ``ALLOWED`` below with the reason, so the
test documents it instead of weakening.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from mkdisk import ext2, geometry  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent.parent
UUID = bytes(range(1, 17))
NOW = 1_700_000_123
LABEL = "parity-check"
# (byte offset, length, why) ranges that may legitimately differ. Empty on purpose.
ALLOWED: list[tuple[int, int, str]] = []
CHUNK = 1 << 20

# Sizes cover: a single 4 KiB group, 1 KiB blocks across several groups (so
# sparse-super backups at groups 1 and 3, and a short last group), 2 KiB blocks,
# and two groups of 4 KiB blocks.
CASES = [
    (8 << 20, 4096),
    (40 << 20, 1024),
    (3 << 20, 2048),
    (130 << 20, 4096),
    (1 << 20, 1024),
]


def rust_command(out: Path, size: int, block_size: int) -> list[str]:
    return ["cargo", "run", "--quiet", "-p", "ext2fs", "--example", "mkimage", "--",
            str(out), "--size", str(size), "--block-size", str(block_size),
            "--label", LABEL, "--uuid", UUID.hex(), "--now", str(NOW), "--mode", "bare"]


def build_python(out: Path, size: int, block_size: int) -> None:
    """The same extents ``volume.format_image`` writes, with a pinned uuid and time."""
    geo = geometry.plan(size, block_size)
    extents = ext2.build_extents(geo, LABEL, UUID, NOW)
    with open(out, "wb") as handle:
        handle.truncate(geo.blocks_count * geo.block_size)
        for offset, data in extents:
            handle.seek(offset)
            handle.write(data)


def describe(offset: int, geo: geometry.Geometry) -> str:
    """Name the on-disk structure a byte offset falls in (for the failure message)."""
    bs = geo.block_size
    if 1024 <= offset < 2048:
        return f"primary superblock, field 0x{offset - 1024:02x}"
    block = offset // bs
    for index in range(geo.groups):
        g = geo.group(index)
        for name, first, count in (
            ("superblock and descriptor copy", g.start, g.block_bitmap - g.start),
            ("block bitmap", g.block_bitmap, 1),
            ("inode bitmap", g.inode_bitmap, 1),
            ("inode table", g.inode_table, geo.inode_table_blocks),
        ):
            if first <= block < first + count:
                return f"group {index} {name}, block {block} byte {offset % bs}"
    return f"data block {block} byte {offset % bs} (root or lost+found)"


def differences(left: Path, right: Path) -> list[int]:
    """Offsets of the first few differing bytes, ignoring ``ALLOWED`` ranges."""
    found: list[int] = []
    with open(left, "rb") as a, open(right, "rb") as b:
        base = 0
        while len(found) < 8:
            x, y = a.read(CHUNK), b.read(CHUNK)
            if x != y:
                for i in range(min(len(x), len(y))):
                    if x[i] != y[i] and not any(s <= base + i < s + n for s, n, _ in ALLOWED):
                        found.append(base + i)
                        if len(found) == 8:
                            break
                if len(x) != len(y):
                    found.append(base + min(len(x), len(y)))
            if not x and not y:
                break
            base += CHUNK
    return found


@unittest.skipUnless(shutil.which("cargo"), "cargo is not installed")
class FormatterParity(unittest.TestCase):
    def check(self, size: int, block_size: int) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            rust, python = Path(tmp, "rust.img"), Path(tmp, "python.img")
            subprocess.run(rust_command(rust, size, block_size), cwd=ROOT, check=True,
                           stdout=subprocess.DEVNULL, env={**os.environ, "CARGO_TERM_COLOR": "never"})
            build_python(python, size, block_size)
            geo = geometry.plan(size, block_size)
            self.assertEqual(rust.stat().st_size, python.stat().st_size,
                             "volume sizes differ (geometry disagrees)")
            diffs = differences(rust, python)
            report = "\n".join(f"  byte {d}: {describe(d, geo)}" for d in diffs)
            self.assertEqual(diffs, [], f"{size >> 20} MiB / {block_size}-byte blocks differ:\n{report}")

    def test_images_are_byte_identical(self) -> None:
        for size, block_size in CASES:
            with self.subTest(size=size, block_size=block_size):
                self.check(size, block_size)


if __name__ == "__main__":
    unittest.main()
