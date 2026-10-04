//! On-disk layout: magic, field offsets and little-endian helpers.

use super::*;

/// Superblock magic (`s_magic`) at byte 1024 of the volume.
pub(super) const EXT2_MAGIC: u16 = 0xEF53;
/// The superblock always starts at byte 1024 (block 0 for 2K/4K blocks, the
/// second block for 1K blocks).
pub(super) const SUPER_OFFSET: u64 = 1024;
/// Root directory inode, fixed by the format.
pub(super) const ROOT_INO: u32 = 2;
/// Inodes below `first_ino` are reserved (bad-blocks inode, root, ...).
pub(super) const DEFAULT_FIRST_INO: u32 = 11;
/// Twelve direct block slots; slot 12 is the single indirect, 13 the double,
/// and 14 the triple. Indirect slot `12 + n - 1` is a tree `n` tables deep.
pub(super) const DIRECT_BLOCKS: u32 = 12;
pub(super) const SINGLE_INDIRECT_SLOT: u32 = 12;
/// Every inode has fifteen block slots.
pub(super) const BLOCK_SLOTS: u32 = 15;
/// The deepest indirect tree an inode can hold (the triple-indirect slot).
pub(super) const MAX_DEPTH: usize = 3;
/// Largest file this driver will grow: 2 GiB - 1. Sizes stay in the 32-bit
/// `i_size`, so no length needs the large-file high word, and a signed-size
/// reader never sees a negative length.
pub const MAX_FILE_SIZE: u64 = 0x7FFF_FFFF;
/// The revision-1 core inode is 128 bytes; larger inode tails are preserved by
/// the read-modify-write in [`Ext2::write_inode`].
pub(super) const INODE_CORE_SIZE: usize = 128;
/// The largest block this driver handles; 4 KiB keeps stack buffers small.
pub(super) const MAX_BLOCK_SIZE: usize = 4096;
/// Bound on the block-group count so no per-group loop can run away.
pub(super) const MAX_GROUPS: u32 = 4096;
/// Directory entry header: inode, record length, name length, file type.
pub(super) const DE_HEADER: usize = 8;
pub(super) const DE_INO: usize = 0;
pub(super) const DE_REC_LEN: usize = 4;
pub(super) const DE_NAME_LEN: usize = 6;
pub(super) const DE_FILE_TYPE: usize = 7;
/// File-type byte values in a directory entry.
pub(super) const FT_REGULAR: u8 = 1;
pub(super) const FT_DIRECTORY: u8 = 2;
/// The longest ext2 name (and the VFS's own limit).
pub(super) const MAX_NAME: usize = 255;
/// Incompat feature: directory entries carry a file-type byte.
pub(super) const FEATURE_INCOMPAT_FILETYPE: u32 = 0x0002;
/// Read-only-compat features this driver understands: sparse superblocks (we
/// do not need the backups) and large files (the size high bits).
pub(super) const FEATURE_RO_SPARSE_SUPER: u32 = 0x0001;
pub(super) const FEATURE_RO_LARGE_FILE: u32 = 0x0002;
/// Compat feature: the volume has a journal (`s_journal_inum` names its inode).
pub(super) const FEATURE_COMPAT_HAS_JOURNAL: u32 = 0x0004;
/// Incompat feature: the journal holds committed work the next mount replays.
pub(super) const FEATURE_INCOMPAT_RECOVER: u32 = 0x0004;
/// The inode ext2/3/4 give an internal journal.
pub(super) const JOURNAL_INO: u32 = 8;

// Superblock byte offsets within the 1024-byte superblock.
pub(super) const SB_INODES_COUNT: usize = 0x00;
pub(super) const SB_BLOCKS_COUNT: usize = 0x04;
pub(super) const SB_FREE_BLOCKS: usize = 0x0C;
pub(super) const SB_FREE_INODES: usize = 0x10;
pub(super) const SB_FIRST_DATA_BLOCK: usize = 0x14;
pub(super) const SB_LOG_BLOCK_SIZE: usize = 0x18;
pub(super) const SB_BLOCKS_PER_GROUP: usize = 0x20;
pub(super) const SB_INODES_PER_GROUP: usize = 0x28;
pub(super) const SB_WTIME: usize = 0x30;
pub(super) const SB_MAGIC: usize = 0x38;
pub(super) const SB_STATE: usize = 0x3A;
pub(super) const SB_REV_LEVEL: usize = 0x4C;
pub(super) const SB_FIRST_INO: usize = 0x54;
pub(super) const SB_INODE_SIZE: usize = 0x58;
pub(super) const SB_FEATURE_COMPAT: usize = 0x5C;
pub(super) const SB_FEATURE_INCOMPAT: usize = 0x60;
pub(super) const SB_FEATURE_RO_COMPAT: usize = 0x64;
pub(super) const SB_UUID: usize = 0x68;
pub(super) const SB_VOLUME_NAME: usize = 0x78;
/// `s_journal_uuid` (external journals, unsupported), `s_journal_inum`,
/// `s_journal_dev`, and the backup of the journal inode's block map.
pub(super) const SB_JOURNAL_UUID: usize = 0xD0;
pub(super) const SB_JOURNAL_INUM: usize = 0xE0;
pub(super) const SB_JOURNAL_DEV: usize = 0xE4;
pub(super) const SB_JNL_BACKUP_TYPE: usize = 0xFD;
pub(super) const SB_JNL_BLOCKS: usize = 0x10C;

/// `i_flags` bit: the directory carries an htree index (`EXT2_INDEX_FL`).
pub(super) const INDEX_FL: u32 = 0x1000;

/// `s_state` bits: the volume was cleanly unmounted / errors were recorded.
pub(super) const STATE_VALID: u16 = 0x0001;
pub(super) const STATE_ERROR: u16 = 0x0002;

// Inode byte offsets (the first 128 bytes of every inode). `i_dtime` is a
// full 32-bit field, so `i_gid` starts at 0x18, not 0x16.
pub(super) const INO_MODE: usize = 0x00;
pub(super) const INO_UID: usize = 0x02;
pub(super) const INO_SIZE: usize = 0x04;
pub(super) const INO_ATIME: usize = 0x08;
pub(super) const INO_CTIME: usize = 0x0C;
pub(super) const INO_MTIME: usize = 0x10;
pub(super) const INO_DTIME: usize = 0x14;
pub(super) const INO_GID: usize = 0x18;
pub(super) const INO_LINKS: usize = 0x1A;
pub(super) const INO_BLOCKS: usize = 0x1C;
pub(super) const INO_FLAGS: usize = 0x20;
pub(super) const INO_BLOCK: usize = 0x28;
pub(super) const INO_GENERATION: usize = 0x64;
pub(super) const INO_DIR_ACL: usize = 0x6C;

// Group descriptor byte offsets (32 bytes each in the descriptor table).
pub(super) const GD_BLOCK_BITMAP: usize = 0x00;
pub(super) const GD_INODE_BITMAP: usize = 0x04;
pub(super) const GD_INODE_TABLE: usize = 0x08;
pub(super) const GD_FREE_BLOCKS: usize = 0x0C;
pub(super) const GD_FREE_INODES: usize = 0x0E;
pub(super) const GD_USED_DIRS: usize = 0x10;
pub(super) const GD_SIZE: usize = 32;

pub(super) fn le16(buf: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buf[offset], buf[offset + 1]])
}

pub(super) fn le32(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

pub(super) fn put16(buf: &mut [u8], offset: usize, value: u16) {
    buf[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

pub(super) fn put32(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Map a block-layer failure onto the library's error space. A write to a
/// read-only device keeps its friendly `ReadOnly`; everything else is a corrupt
/// or missing backing store.
pub(super) fn io_error(error: IoError) -> Ext2Error {
    match error {
        IoError::ReadOnly => Ext2Error::ReadOnly,
        IoError::Failed => Ext2Error::Io,
    }
}

/// The node kind an ext2 `i_mode` denotes, or `None` for node types the VFS
/// cannot represent yet (symlinks, device nodes, sockets, FIFOs).
pub(super) fn kind_from_mode(mode: u16) -> Option<FileKind> {
    match mode & S_IFMT {
        S_IFREG => Some(FileKind::File),
        S_IFDIR => Some(FileKind::Dir),
        _ => None,
    }
}

/// Refuse an owner whose ids do not fit the 16-bit `i_uid`/`i_gid` fields.
/// Truncating would hand a file created by uid 65536 to uid 0 (root).
pub(super) fn check_owner(owner: Owner) -> Result<(), Ext2Error> {
    if owner.uid > u32::from(u16::MAX) || owner.gid > u32::from(u16::MAX) {
        return Err(Ext2Error::Invalid);
    }
    Ok(())
}

/// Stamp a change: both `ctime` and `mtime` move on content or tree changes.
pub(super) fn touch(inode: &mut [u8; INODE_CORE_SIZE], time: u32) {
    put32(inode, INO_CTIME, time);
    put32(inode, INO_MTIME, time);
}

/// Split `path` into its parent directory path and final component. The root
/// has no parent, so creating or removing it is [`Ext2Error::Exists`].
pub(super) fn split_parent(path: &str) -> Result<(&str, &str), Ext2Error> {
    let path = path.trim_matches('/');
    if path.is_empty() {
        return Err(Ext2Error::Exists);
    }
    let (parent, name) = match path.rsplit_once('/') {
        Some((parent, name)) => (parent, name),
        None => ("", path),
    };
    if name.is_empty() || name == "." || name == ".." {
        return Err(Ext2Error::Invalid);
    }
    if name.len() > MAX_NAME {
        return Err(Ext2Error::NameTooLong);
    }
    Ok((parent, name))
}

/// 16 bytes at `offset` (a UUID or a NUL-padded label).
pub(super) fn array16(buf: &[u8], offset: usize) -> [u8; 16] {
    let mut out = [0u8; 16];
    out.copy_from_slice(&buf[offset..offset + 16]);
    out
}
