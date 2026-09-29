"""Directory inodes and blocks for the root and the seeded tree.

Every directory here is one block in group 0, so the used blocks and inodes
stay a contiguous prefix of their bitmaps (see :mod:`mkdisk.ext2`), and inode
numbers are handed out in layout order: the first seeded directory is inode 12,
right after ``lost+found``.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass, field

from .geometry import FIRST_INO, INODE_SIZE, LOST_FOUND_INO, ROOT_INO, GroupLayout
from .layout import LOST_FOUND, Layout

Extent = tuple[int, bytes]

S_IFDIR = 0o040000
FT_DIRECTORY = 2
DE_HEADER = 8
# `.` and `..` need a 12-byte record (8-byte header + name padded to 4).
DOT_REC_LEN = 12


@dataclass
class Dir:
    """A directory to write: identity, attributes and its named children."""

    ino: int
    parent: int
    block: int
    mode: int
    uid: int
    gid: int
    children: list[tuple[str, int]] = field(default_factory=list)

    @property
    def links(self) -> int:
        """``.`` and the parent's entry, plus one ``..`` per child directory."""
        return 2 + len(self.children)


def plan_tree(layout: Layout, root_block: int, first_block: int) -> list[Dir]:
    """Root first, then the layout's directories (inodes 12.., blocks ``first_block``..).

    ``lost+found`` is a child of the root but is written by :mod:`mkdisk.ext2`
    (it spans many blocks), so only its name and inode appear here.
    """
    root = Dir(ROOT_INO, ROOT_INO, root_block, layout.root_mode, layout.root_uid,
               layout.root_gid, [(LOST_FOUND.lstrip("/"), LOST_FOUND_INO)])
    by_path = {"/": root}
    dirs = [root]
    for index, spec in enumerate(layout.dirs):
        parent = by_path[spec.parent]
        node = Dir(FIRST_INO + 1 + index, parent.ino, first_block + index, spec.mode,
                   spec.uid, spec.gid)
        parent.children.append((spec.name, node.ino))
        by_path[spec.path] = node
        dirs.append(node)
    return dirs


def inode(mode: int, uid: int, gid: int, size: int, links: int, blocks: list[int],
          block_size: int, stamp: int) -> bytes:
    """A 128-byte directory inode whose data lives in direct ``blocks``."""
    raw = bytearray(INODE_SIZE)
    # i_dtime is a full 32-bit field, so gid/links/i_blocks start at 0x18/0x1A/0x1C.
    struct.pack_into("<HHIIIII", raw, 0x00, S_IFDIR | mode, uid, size, stamp, stamp, stamp, 0)
    struct.pack_into("<HHI", raw, 0x18, gid, links, len(blocks) * block_size // 512)
    struct.pack_into(f"<{len(blocks)}I", raw, 0x28, *blocks)
    return bytes(raw)


def dirent(ino: int, rec_len: int, name: bytes, file_type: int = FT_DIRECTORY) -> bytes:
    """A directory entry padded out to ``rec_len``."""
    header = struct.pack("<IHBB", ino, rec_len, len(name), file_type)
    return (header + name).ljust(rec_len, b"\0")


def directory_block(node: Dir, block_size: int) -> bytes:
    """``.``, ``..`` and the children, the last record stretching to the block end."""
    entries = [(node.ino, b"."), (node.parent, b"..")]
    entries += [(ino, name.encode()) for name, ino in node.children]
    lengths = [-(-(DE_HEADER + len(name)) // 4) * 4 for _, name in entries]
    if sum(lengths) > block_size:
        raise ValueError(f"directory inode {node.ino} has too many entries for one block")
    lengths[-1] += block_size - sum(lengths)
    return b"".join(dirent(ino, rec, name) for (ino, name), rec in zip(entries, lengths))


def tree_extents(dirs: list[Dir], group0: GroupLayout, block_size: int,
                 stamp: int) -> list[Extent]:
    """Inode-table slots and directory blocks for every planned directory."""
    table = group0.inode_table * block_size
    extents: list[Extent] = []
    for node in dirs:
        raw = inode(node.mode, node.uid, node.gid, block_size, node.links, [node.block],
                    block_size, stamp)
        extents.append((table + (node.ino - 1) * INODE_SIZE, raw))
        extents.append((node.block * block_size, directory_block(node, block_size)))
    return extents
