#!/usr/bin/env python3
"""Tests for the ext2 formatter (issue #332). Run: python tools/mkdisk/test_mkdisk.py.

The formatter is checked three ways: by re-implementing the kernel driver's
mount-time validation (``Ext2::open`` in ``kernel/src/fs/ext2.rs``), by a
miniature fsck that walks the tree and cross-checks bitmaps and free counts,
and by the real ``e2fsck -n`` when the host has one.
"""

from __future__ import annotations

import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from mkdisk import ext2, geometry, volume  # noqa: E402

MIB = 1 << 20
ROOT_INO, LOST_FOUND_INO = geometry.ROOT_INO, geometry.LOST_FOUND_INO
# Layout limits copied from kernel/src/fs/ext2/layout.rs.
KERNEL_MAX_GROUPS = 4096
KERNEL_FEATURE_INCOMPAT = 0x0002
KERNEL_FEATURE_RO = 0x0001 | 0x0002


def format_bytes(size: int, block_size: int = 4096, **kwargs) -> bytes:
    """Materialise the extents of a fresh volume into one in-memory image."""
    geo = geometry.plan(size, block_size)
    image = bytearray(geo.blocks_count * block_size)
    for offset, data in ext2.build_extents(geo, **kwargs):
        image[offset:offset + len(data)] = data
    return bytes(image)


class Volume:
    """A read-only decoder that shares no code with the encoder."""

    def __init__(self, image: bytes) -> None:
        self.image = image
        self.sb = image[1024:2048]
        self.block_size = 1024 << self.u32(0x18)
        self.inodes_count, self.blocks_count = self.u32(0x00), self.u32(0x04)
        self.free_blocks, self.free_inodes = self.u32(0x0C), self.u32(0x10)
        self.first_data = self.u32(0x14)
        self.bpg, self.ipg = self.u32(0x20), self.u32(0x28)
        self.inode_size = struct.unpack_from("<H", self.sb, 0x58)[0]
        self.groups = -(-(self.blocks_count - self.first_data) // self.bpg)

    def u32(self, offset: int) -> int:
        return struct.unpack_from("<I", self.sb, offset)[0]

    def block(self, number: int) -> bytes:
        return self.image[number * self.block_size:(number + 1) * self.block_size]

    def descriptor(self, group: int) -> dict:
        table = (self.first_data + 1) * self.block_size
        bb, ib, it, fb, fi, ud = struct.unpack_from("<IIIHHH", self.image, table + 32 * group)
        return dict(block_bitmap=bb, inode_bitmap=ib, inode_table=it, free_blocks=fb,
                    free_inodes=fi, used_dirs=ud)

    def inode(self, ino: int) -> dict:
        group, index = divmod(ino - 1, self.ipg)
        table = self.descriptor(group)["inode_table"]
        raw = self.image[table * self.block_size + index * self.inode_size:][:128]
        mode, _, size, _, _, _, dtime, _, links, i_blocks = struct.unpack_from("<HHIIIIIHHI", raw)
        return dict(mode=mode, size=size, links=links, i_blocks=i_blocks, dtime=dtime,
                    blocks=list(struct.unpack_from("<15I", raw, 0x28)))

    def entries(self, ino: int) -> list[tuple[int, int, bytes, int]]:
        """``(inode, rec_len, name, type)`` for every entry of a directory."""
        found = []
        for number in self.inode(ino)["blocks"][:12]:
            if not number:
                continue
            block, pos = self.block(number), 0
            while pos < self.block_size:
                child, rec_len, name_len, kind = struct.unpack_from("<IHBB", block, pos)
                assert rec_len >= 8 and rec_len % 4 == 0 and pos + rec_len <= self.block_size
                found.append((child, rec_len, block[pos + 8:pos + 8 + name_len], kind))
                pos += rec_len
        return found

    def bitmap_bits(self, number: int, count: int) -> list[bool]:
        raw = self.block(number)
        return [bool(raw[i >> 3] >> (i & 7) & 1) for i in range(count)]


def kernel_open_errors(image: bytes) -> list[str]:
    """Every ``Ext2::open`` rejection the image would trigger (none = mounts)."""
    v, errors = Volume(image), []
    if struct.unpack_from("<H", v.sb, 0x38)[0] != 0xEF53:
        errors.append("bad magic")
    if v.u32(0x18) > 2:
        errors.append("block size above 4 KiB")
    if v.inodes_count < ROOT_INO or v.blocks_count <= v.first_data:
        errors.append("degenerate counts")
    if v.free_blocks > v.blocks_count or v.free_inodes > v.inodes_count:
        errors.append("free counts exceed totals")
    if v.first_data > 1:
        errors.append("first data block")
    if not v.bpg or not v.ipg:
        errors.append("zero group size")
    rev, first_ino = v.u32(0x4C), v.u32(0x54)
    if rev < 1:
        errors.append("not revision 1")
    if not 128 <= v.inode_size <= v.block_size or v.block_size % v.inode_size:
        errors.append("inode size")
    if first_ino == 0 or first_ino > v.inodes_count:
        errors.append("first_ino")
    inode_groups = -(-v.inodes_count // v.ipg)
    if not 0 < v.groups <= KERNEL_MAX_GROUPS or inode_groups > v.groups:
        errors.append("group count")
    gdt_blocks = -(-v.groups * 32 // v.block_size)
    if v.first_data + 1 + gdt_blocks > v.blocks_count:
        errors.append("descriptor table past the volume")
    if v.blocks_count * v.block_size > len(image):
        errors.append("volume larger than device")
    if v.u32(0x60) & ~KERNEL_FEATURE_INCOMPAT:
        errors.append("unsupported incompat features")
    if v.u32(0x64) & ~KERNEL_FEATURE_RO:
        errors.append("unsupported ro-compat features")
    if v.ipg % (v.block_size // v.inode_size):
        errors.append("ragged inode table")
    return errors


class GeometryTests(unittest.TestCase):
    def test_backup_groups_follow_sparse_super(self) -> None:
        backups = [g for g in range(30) if geometry.has_backup(g)]
        self.assertEqual(backups, [0, 1, 3, 5, 7, 9, 25, 27])

    def test_rejects_bad_parameters(self) -> None:
        with self.assertRaises(ValueError):
            geometry.plan(4096, 4096)  # too small
        with self.assertRaises(ValueError):
            geometry.plan(64 * MIB, 8192)  # driver stops at 4 KiB
        with self.assertRaises(ValueError):
            geometry.plan(600 * 1024 * MIB, 4096)  # beyond MAX_GROUPS

    def test_runt_trailing_group_is_dropped(self) -> None:
        # 128 MiB + 10 blocks: the 10-block tail cannot hold its own metadata.
        geo = geometry.plan(128 * MIB + 10 * 4096, 4096)
        self.assertEqual((geo.groups, geo.blocks_count), (1, 32768))


class KernelExpectationTests(unittest.TestCase):
    """The image must mount under the driver's ``Ext2::open`` rules."""

    CASES = [(1 * MIB, 1024), (1 * MIB, 4096), (8 * MIB, 2048), (64 * MIB, 4096),
             (40 * MIB, 1024), (200 * MIB, 4096)]  # last two span several groups

    def test_open_rules_hold_for_all_shapes(self) -> None:
        for size, block_size in self.CASES:
            with self.subTest(size=size, block_size=block_size):
                self.assertEqual(kernel_open_errors(format_bytes(size, block_size)), [])

    def test_default_volume_matches_documented_shape(self) -> None:
        v = Volume(format_bytes(64 * MIB))
        self.assertEqual((v.block_size, v.blocks_count, v.groups, v.inodes_count),
                         (4096, 16384, 1, 4096))

    def test_kernel_mirror_actually_rejects_damage(self) -> None:
        image = bytearray(format_bytes(1 * MIB))
        image[1024 + 0x38] ^= 0xFF  # corrupt the magic
        self.assertIn("bad magic", kernel_open_errors(bytes(image)))


class ContentTests(unittest.TestCase):
    def setUp(self) -> None:
        self.image = format_bytes(64 * MIB, label="scratch", volume_uuid=bytes(range(16)), now=1234)
        self.v = Volume(self.image)

    def test_superblock_identity_fields(self) -> None:
        self.assertEqual(self.v.sb[0x68:0x78], bytes(range(16)))
        self.assertEqual(self.v.sb[0x78:0x88].rstrip(b"\0"), b"scratch")
        self.assertEqual(struct.unpack_from("<H", self.v.sb, 0x3A)[0], 1)  # cleanly unmounted
        self.assertEqual(struct.unpack_from("<I", self.v.sb, 0x30)[0], 1234)

    def test_root_directory(self) -> None:
        root = self.v.inode(ROOT_INO)
        self.assertEqual((root["mode"], root["links"], root["size"]), (0o040755, 3, 4096))
        names = [(e[0], e[2]) for e in self.v.entries(ROOT_INO)]
        self.assertEqual(names, [(2, b"."), (2, b".."), (11, b"lost+found")])
        self.assertTrue(all(e[3] == 2 for e in self.v.entries(ROOT_INO)))  # FILETYPE dirs

    def test_lost_found_is_sixteen_kib_of_empty_blocks(self) -> None:
        lf = self.v.inode(LOST_FOUND_INO)
        self.assertEqual((lf["mode"], lf["links"], lf["size"]), (0o040700, 2, 16384))
        self.assertEqual(lf["i_blocks"], 16384 // 512)
        entries = self.v.entries(LOST_FOUND_INO)
        self.assertEqual([e[2] for e in entries[:2]], [b".", b".."])
        self.assertEqual(entries[1][0], ROOT_INO)
        self.assertTrue(all(e[0] == 0 for e in entries[2:]))

    def test_label_limits(self) -> None:
        with self.assertRaises(ValueError):
            ext2.encode_label("x" * 17)
        with self.assertRaises(UnicodeEncodeError):
            ext2.encode_label("café")


class ConsistencyTests(unittest.TestCase):
    """A miniature ``e2fsck``: bitmaps, free counts and reachability agree."""

    SHAPES = [(1 * MIB, 1024), (64 * MIB, 4096), (40 * MIB, 1024), (200 * MIB, 4096)]

    def owned_blocks(self, v: Volume) -> set[int]:
        """Every block the tree and the group metadata claim to own."""
        owned: set[int] = set()
        for group in range(v.groups):
            geo = geometry.plan(v.blocks_count * v.block_size, v.block_size)
            layout = geo.group(group)
            gd = v.descriptor(group)
            self.assertEqual(gd["block_bitmap"], layout.block_bitmap)
            self.assertEqual(gd["inode_table"], layout.inode_table)
            owned.update(range(layout.start, layout.first_free))
        for ino in (ROOT_INO, LOST_FOUND_INO):
            owned.update(b for b in v.inode(ino)["blocks"] if b)
        return owned

    def test_bitmaps_and_counts(self) -> None:
        for size, block_size in self.SHAPES:
            with self.subTest(size=size, block_size=block_size):
                v = Volume(format_bytes(size, block_size))
                owned = self.owned_blocks(v)
                total_free_blocks = total_free_inodes = 0
                for group in range(v.groups):
                    gd = v.descriptor(group)
                    start = v.first_data + group * v.bpg
                    in_group = min(v.bpg, v.blocks_count - start)
                    bits = v.bitmap_bits(gd["block_bitmap"], v.bpg)
                    for bit, used in enumerate(bits):
                        expected = bit >= in_group or (start + bit) in owned
                        self.assertEqual(used, expected, f"group {group} block bit {bit}")
                    self.assertEqual(gd["free_blocks"], bits[:in_group].count(False))
                    ibits = v.bitmap_bits(gd["inode_bitmap"], v.block_size * 8)
                    self.assertTrue(all(ibits[v.ipg:]))  # padding is marked used
                    self.assertEqual(gd["free_inodes"], ibits[:v.ipg].count(False))
                    self.assertEqual(gd["used_dirs"], 2 if group == 0 else 0)
                    total_free_blocks += gd["free_blocks"]
                    total_free_inodes += gd["free_inodes"]
                self.assertEqual(total_free_blocks, v.free_blocks)
                self.assertEqual(total_free_inodes, v.free_inodes)
                self.assertEqual(v.free_inodes, v.inodes_count - geometry.FIRST_INO)

    def test_backup_superblocks_and_descriptors(self) -> None:
        v = Volume(format_bytes(200 * MIB, 4096))  # two groups, both carry backups
        self.assertEqual(v.groups, 2)
        backup = v.block(v.bpg)
        self.assertEqual(backup[:1024][0x38:0x3A], b"\x53\xef")
        self.assertEqual(struct.unpack_from("<H", backup, 0x5A)[0], 1)  # s_block_group_nr
        self.assertEqual(v.block(v.bpg + 1)[:64], v.block(1)[:64])  # descriptor table copy

    def test_no_reserved_inode_is_populated_except_root_and_lost_found(self) -> None:
        v = Volume(format_bytes(64 * MIB))
        for ino in range(1, geometry.FIRST_INO + 1):
            expected_used = ino in (ROOT_INO, LOST_FOUND_INO)
            self.assertEqual(v.inode(ino)["mode"] != 0, expected_used, f"inode {ino}")


@unittest.skipUnless(shutil.which("e2fsck"), "e2fsck not installed on this host")
class E2fsckTests(unittest.TestCase):
    def test_e2fsck_reports_clean(self) -> None:
        for size, block_size in ((64 * MIB, 4096), (40 * MIB, 1024), (200 * MIB, 4096)):
            with self.subTest(size=size, block_size=block_size), \
                    tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / "data.img"
                path.write_bytes(format_bytes(size, block_size))
                done = subprocess.run(["e2fsck", "-fn", str(path)], capture_output=True, text=True)
                self.assertEqual(done.returncode, 0, done.stdout + done.stderr)


class VolumeFileTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / "sub" / "data.img"

    def test_parse_size(self) -> None:
        self.assertEqual(volume.parse_size("64M"), 64 * MIB)
        self.assertEqual(volume.parse_size("512k"), 512 * 1024)
        self.assertEqual(volume.parse_size("1GiB"), 1 << 30)
        self.assertEqual(volume.parse_size("2048"), 2048)
        with self.assertRaises(ValueError):
            volume.parse_size("lots")

    def test_ensure_creates_once_and_never_overwrites(self) -> None:
        self.assertTrue(volume.ensure_volume(self.path, 1 * MIB))
        self.path.write_bytes(b"user data")
        self.assertFalse(volume.ensure_volume(self.path, 1 * MIB))
        self.assertEqual(self.path.read_bytes(), b"user data")

    def test_format_image_replaces_and_matches_in_memory_build(self) -> None:
        self.path.parent.mkdir()
        self.path.write_bytes(b"junk")
        volume.format_image(self.path, 1 * MIB, "lbl")
        image = self.path.read_bytes()
        self.assertEqual(len(image), 1 * MIB)
        self.assertEqual(kernel_open_errors(image), [])
        self.assertEqual(list(self.path.parent.glob("*.partial")), [])

    def test_failed_format_leaves_existing_volume_intact(self) -> None:
        volume.format_image(self.path, 1 * MIB)
        before = self.path.read_bytes()
        with self.assertRaises(ValueError):
            volume.format_image(self.path, 1 * MIB, "x" * 17)
        self.assertEqual(self.path.read_bytes(), before)
        self.assertEqual(list(self.path.parent.glob("*.partial")), [])

    def test_status(self) -> None:
        self.assertFalse(volume.status(self.path).exists)
        volume.format_image(self.path, 1 * MIB)
        state = volume.status(self.path)
        self.assertEqual((state.exists, state.size), (True, 1 * MIB))
        self.assertIn("1 MiB", state.describe())


if __name__ == "__main__":
    unittest.main()
