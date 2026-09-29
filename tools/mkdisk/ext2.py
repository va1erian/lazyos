"""ext2 revision-1 encoder: turns a :class:`~mkdisk.geometry.Geometry` into bytes.

The output is a list of *extents* ``(byte offset, data)`` rather than one big
buffer: a freshly formatted volume is almost entirely zeros, so only the
metadata is ever materialised and the rest of the file is left to the host
(see :mod:`mkdisk.volume`). Field offsets match ``kernel/src/fs/ext2/layout.rs``.
"""

from __future__ import annotations

import struct
import time
import uuid as uuidlib

from .geometry import (FIRST_INO, GD_SIZE, INODE_SIZE, LOST_FOUND_INO, ROOT_INO,
                       Geometry, GroupLayout)

Extent = tuple[int, bytes]

EXT2_MAGIC = 0xEF53
SUPER_OFFSET = 1024
SUPER_SIZE = 1024
LABEL_MAX = 16

STATE_VALID = 1
ERRORS_CONTINUE = 1
# Incompat FILETYPE: dirents carry a type byte. RO_COMPAT: sparse superblock
# backups (see ``geometry.has_backup``) and >2 GiB files. These are the only
# features the driver accepts, and it accepts them all.
FEATURE_INCOMPAT_FILETYPE = 0x0002
FEATURE_RO_SPARSE_SUPER = 0x0001
FEATURE_RO_LARGE_FILE = 0x0002

S_IFDIR = 0o040000
FT_DIRECTORY = 2
DE_HEADER = 8
# `.` and `..` need a 12-byte record (8-byte header + name padded to 4).
DOT_REC_LEN = 12


def build_extents(geometry: Geometry, label: str = "", volume_uuid: bytes | None = None,
                  now: int | None = None) -> list[Extent]:
    """Every non-zero byte range of an empty volume with a ``lost+found``."""
    stamp = int(time.time()) if now is None else now
    ident = volume_uuid if volume_uuid is not None else uuidlib.uuid4().bytes
    if len(ident) != 16:
        raise ValueError("uuid must be 16 bytes")
    name = encode_label(label)

    layouts = [geometry.group(g) for g in range(geometry.groups)]
    root_block = layouts[0].first_free
    lost_found = [root_block + 1 + i for i in range(geometry.lost_found_blocks)]
    used = [layout.metadata_blocks for layout in layouts]
    used[0] += 1 + len(lost_found)  # root directory block + lost+found
    free_blocks = [layout.blocks - count for layout, count in zip(layouts, used)]
    if free_blocks[0] < 0:
        raise ValueError("volume too small for its own metadata")
    free_inodes = [geometry.inodes_per_group] * geometry.groups
    free_inodes[0] -= FIRST_INO  # inodes 1..11 are reserved / in use

    descriptors = b"".join(
        group_descriptor(layout, free_blocks[i], free_inodes[i], 2 if i == 0 else 0)
        for i, layout in enumerate(layouts))
    descriptors += bytes(-len(descriptors) % geometry.block_size)  # pad to whole blocks

    bs = geometry.block_size
    extents: list[Extent] = []
    for index, layout in enumerate(layouts):
        sb = superblock(geometry, name, ident, stamp, index, sum(free_blocks), sum(free_inodes))
        if index == 0:
            extents.append((SUPER_OFFSET, sb))
            extents.append(((geometry.first_data_block + 1) * bs, descriptors))
        elif layout.has_backup:
            extents.append((layout.start * bs, sb))
            extents.append(((layout.start + 1) * bs, descriptors))
        extents.append((layout.block_bitmap * bs, block_bitmap(geometry, layout, used[index])))
        extents.append((layout.inode_bitmap * bs, inode_bitmap(geometry, index)))

    extents += root_and_lost_found(geometry, layouts[0], root_block, lost_found, stamp)
    return extents


def encode_label(label: str) -> bytes:
    """A volume name padded to the 16-byte superblock field."""
    raw = label.encode("ascii", errors="strict")
    if len(raw) > LABEL_MAX:
        raise ValueError(f"label is {len(raw)} bytes; the limit is {LABEL_MAX}")
    return raw.ljust(LABEL_MAX, b"\0")


def superblock(geometry: Geometry, label: bytes, ident: bytes, stamp: int, group: int,
               free_blocks: int, free_inodes: int) -> bytes:
    """The 1 KiB superblock; backups differ only in ``s_block_group_nr``."""
    sb = bytearray(SUPER_SIZE)
    log = geometry.block_size.bit_length() - 11  # 1024 -> 0, 2048 -> 1, 4096 -> 2
    struct.pack_into("<IIIIIIII", sb, 0x00,
                     geometry.inodes_count, geometry.blocks_count, 0,  # no root reserve
                     free_blocks, free_inodes, geometry.first_data_block, log, log)
    struct.pack_into("<III", sb, 0x20, geometry.blocks_per_group, geometry.blocks_per_group,
                     geometry.inodes_per_group)
    struct.pack_into("<II", sb, 0x2C, 0, stamp)  # never mounted; written now
    # mnt_count 0, max_mnt_count -1 (no forced fsck), magic, state, errors.
    struct.pack_into("<HHHHH", sb, 0x34, 0, 0xFFFF, EXT2_MAGIC, STATE_VALID, ERRORS_CONTINUE)
    struct.pack_into("<II", sb, 0x40, stamp, 0)  # last check, no interval
    struct.pack_into("<II", sb, 0x48, 0, 1)  # creator OS Linux, revision 1
    struct.pack_into("<IHH", sb, 0x54, FIRST_INO, INODE_SIZE, group)
    struct.pack_into("<III", sb, 0x5C, 0, FEATURE_INCOMPAT_FILETYPE,
                     FEATURE_RO_SPARSE_SUPER | FEATURE_RO_LARGE_FILE)
    sb[0x68:0x78] = ident
    sb[0x78:0x88] = label
    return bytes(sb)


def group_descriptor(layout: GroupLayout, free_blocks: int, free_inodes: int,
                     used_dirs: int) -> bytes:
    """One 32-byte descriptor-table entry."""
    return struct.pack("<IIIHHH14x", layout.block_bitmap, layout.inode_bitmap,
                       layout.inode_table, free_blocks, free_inodes, used_dirs)


def block_bitmap(geometry: Geometry, layout: GroupLayout, used: int) -> bytes:
    """Used blocks are a prefix of the group (metadata, then group-0 data)."""
    bitmap = bytearray(geometry.block_size)
    set_bits(bitmap, 0, used)
    # A short last group leaves bits past the volume end; mke2fs marks them
    # used so they can never be allocated.
    set_bits(bitmap, layout.blocks, geometry.blocks_per_group - layout.blocks)
    return bytes(bitmap)


def inode_bitmap(geometry: Geometry, group: int) -> bytes:
    """Group 0 starts with inodes 1..11 in use; padding bits are set."""
    bitmap = bytearray(geometry.block_size)
    if group == 0:
        set_bits(bitmap, 0, FIRST_INO)
    set_bits(bitmap, geometry.inodes_per_group,
             geometry.block_size * 8 - geometry.inodes_per_group)
    return bytes(bitmap)


def set_bits(bitmap: bytearray, start: int, count: int) -> None:
    """Set ``count`` bits from bit index ``start`` (LSB-first, as ext2 does)."""
    for bit in range(start, start + count):
        bitmap[bit >> 3] |= 1 << (bit & 7)


def inode(mode: int, size: int, links: int, blocks: list[int], block_size: int,
          stamp: int) -> bytes:
    """A 128-byte directory inode whose data lives in direct ``blocks``."""
    raw = bytearray(INODE_SIZE)
    # i_dtime is a full 32-bit field, so gid/links/i_blocks start at 0x18/0x1A/0x1C.
    struct.pack_into("<HHIIIII", raw, 0x00, mode, 0, size, stamp, stamp, stamp, 0)
    struct.pack_into("<HHI", raw, 0x18, 0, links, len(blocks) * block_size // 512)
    struct.pack_into(f"<{len(blocks)}I", raw, 0x28, *blocks)
    return bytes(raw)


def dirent(ino: int, rec_len: int, name: bytes, file_type: int = FT_DIRECTORY) -> bytes:
    """A directory entry padded out to ``rec_len``."""
    header = struct.pack("<IHBB", ino, rec_len, len(name), file_type)
    return (header + name).ljust(rec_len, b"\0")


def root_and_lost_found(geometry: Geometry, group0: GroupLayout, root_block: int,
                        lost_found: list[int], stamp: int) -> list[Extent]:
    """Inodes 2 and 11 plus the directory blocks they own."""
    bs = geometry.block_size
    table = group0.inode_table * bs
    # `lost+found` is one 16 KiB directory: `.`/`..` first, then empty blocks
    # (inode 0, spanning the whole block) that fsck can fill later.
    lf_first = (dirent(LOST_FOUND_INO, DOT_REC_LEN, b".")
                + dirent(ROOT_INO, bs - DOT_REC_LEN, b".."))
    extents: list[Extent] = [
        (table + (ROOT_INO - 1) * INODE_SIZE,
         inode(S_IFDIR | 0o755, bs, 3, [root_block], bs, stamp)),  # ., .., lost+found/..
        (table + (LOST_FOUND_INO - 1) * INODE_SIZE,
         inode(S_IFDIR | 0o700, len(lost_found) * bs, 2, lost_found, bs, stamp)),
        (root_block * bs,
         dirent(ROOT_INO, DOT_REC_LEN, b".") + dirent(ROOT_INO, DOT_REC_LEN, b"..")
         + dirent(LOST_FOUND_INO, bs - 2 * DOT_REC_LEN, b"lost+found")),
        (lost_found[0] * bs, lf_first),
    ]
    extents += [(block * bs, dirent(0, bs, b"", 0)) for block in lost_found[1:]]
    return extents
