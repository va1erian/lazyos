"""Volume geometry: how blocks, groups and inodes are laid out.

Everything here is derived from the requested size and block size alone, so
the encoder (:mod:`mkdisk.ext2`) never has to make a layout decision. The
constants mirror what the kernel driver (``libs/ext2fs/src/layout.rs`` and
``Ext2::open``) accepts; ``test_mkdisk.py`` re-checks them against the
driver's own validation rules.
"""

from __future__ import annotations

from dataclasses import dataclass

# The driver handles 1/2/4 KiB blocks only (``MAX_BLOCK_SIZE`` = 4096).
BLOCK_SIZES = (1024, 2048, 4096)
DEFAULT_BLOCK_SIZE = 4096
# Revision-1 core inode; the driver refuses anything smaller and tolerates larger.
INODE_SIZE = 128
GD_SIZE = 32
ROOT_INO = 2
# First non-reserved inode. ``lost+found`` takes it, exactly like mke2fs.
FIRST_INO = 11
LOST_FOUND_INO = FIRST_INO
# One inode per 16 KiB is mke2fs's "small" ratio and keeps the inode table
# (and so the wasted space) small on a tens-of-megabytes volume.
BYTES_PER_INODE = 16 * 1024
# mke2fs pre-allocates ``lost+found`` to 16 KiB, but never past the twelve
# direct slots, which keeps the freshly formatted inode free of indirection.
LOST_FOUND_BYTES = 16 * 1024
DIRECT_BLOCKS = 12
# ``MAX_GROUPS`` in the driver bounds every per-group loop; a bigger volume
# would be refused at mount time, so refuse to build it.
MAX_GROUPS = 4096
# A trailing group smaller than its own metadata plus this many blocks is
# dropped instead of formatted (mke2fs uses the same 50-block floor).
MIN_GROUP_DATA_BLOCKS = 50
MIN_SIZE = 1024 * 1024


@dataclass(frozen=True)
class GroupLayout:
    """Where one group keeps its metadata (absolute block numbers)."""

    start: int
    blocks: int
    has_backup: bool
    block_bitmap: int
    inode_bitmap: int
    inode_table: int
    first_free: int

    @property
    def metadata_blocks(self) -> int:
        """Blocks from the group start up to the first data block."""
        return self.first_free - self.start


def has_backup(group: int) -> bool:
    """Sparse-super rule: groups 0, 1 and powers of 3, 5 and 7 carry backups."""
    if group <= 1:
        return True
    for base in (3, 5, 7):
        power = base
        while power < group:
            power *= base
        if power == group:
            return True
    return False


@dataclass(frozen=True)
class Geometry:
    """Sizes of a formatted volume, plus per-group block placement."""

    block_size: int
    blocks_count: int
    first_data_block: int
    blocks_per_group: int
    inodes_per_group: int
    groups: int
    gdt_blocks: int
    inode_table_blocks: int

    @property
    def inodes_count(self) -> int:
        """Total inodes across all groups."""
        return self.inodes_per_group * self.groups

    @property
    def lost_found_blocks(self) -> int:
        """Blocks pre-allocated to ``lost+found`` (2..12)."""
        wanted = -(-LOST_FOUND_BYTES // self.block_size)
        return max(2, min(DIRECT_BLOCKS, wanted))

    def group(self, index: int) -> GroupLayout:
        """The metadata placement of group ``index``."""
        start = self.first_data_block + index * self.blocks_per_group
        blocks = min(self.blocks_per_group, self.blocks_count - start)
        backup = has_backup(index)
        # Superblock + descriptor table copy sit at the group start.
        cursor = start + (1 + self.gdt_blocks if backup else 0)
        return GroupLayout(
            start=start,
            blocks=blocks,
            has_backup=backup,
            block_bitmap=cursor,
            inode_bitmap=cursor + 1,
            inode_table=cursor + 2,
            first_free=cursor + 2 + self.inode_table_blocks,
        )


def plan(size_bytes: int, block_size: int = DEFAULT_BLOCK_SIZE) -> Geometry:
    """Choose a geometry for a volume of at most ``size_bytes``.

    Raises :class:`ValueError` for sizes the format or the driver cannot take.
    """
    if block_size not in BLOCK_SIZES:
        raise ValueError(f"block size must be one of {BLOCK_SIZES}, got {block_size}")
    if size_bytes < MIN_SIZE:
        raise ValueError(f"volume must be at least {MIN_SIZE} bytes")
    first_data = 1 if block_size == 1024 else 0
    blocks = size_bytes // block_size
    while True:
        groups = -(-(blocks - first_data) // (block_size * 8))
        if groups > MAX_GROUPS:
            raise ValueError(f"volume needs {groups} groups; the driver allows {MAX_GROUPS}")
        geometry = _with_groups(blocks, block_size, first_data, groups)
        last = geometry.group(groups - 1)
        if groups == 1 or last.blocks >= last.metadata_blocks + MIN_GROUP_DATA_BLOCKS:
            return geometry
        blocks -= last.blocks  # drop the runt group and re-plan


def _with_groups(blocks: int, block_size: int, first_data: int, groups: int) -> Geometry:
    """Fill in the inode and descriptor-table sizes for a fixed group count."""
    inodes_per_block = block_size // INODE_SIZE
    wanted = blocks * block_size // BYTES_PER_INODE
    per_group = -(-max(wanted, FIRST_INO) // groups)
    # Whole inode-table blocks per group; the driver rejects a ragged tail.
    per_group = -(-per_group // inodes_per_block) * inodes_per_block
    per_group = min(per_group, block_size * 8)  # one inode-bitmap block
    return Geometry(
        block_size=block_size,
        blocks_count=blocks,
        first_data_block=first_data,
        blocks_per_group=block_size * 8,
        inodes_per_group=per_group,
        groups=groups,
        gdt_blocks=-(-groups * GD_SIZE // block_size),
        inode_table_blocks=per_group // inodes_per_block,
    )
