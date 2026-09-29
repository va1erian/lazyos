//! Inode attributes: mode, owner and the three timestamps (`setattr`).
//!
//! An attribute change rewrites exactly one inode, which is 128 bytes inside
//! one inode-table block and never straddles a sector, so it lands whole or
//! not at all. It goes through [`Ext2::write_inode`] like every other change,
//! and so through the block write that marks the volume dirty first
//! (`state.rs`): a stop after it leaves either the old or the new attributes
//! behind a dirty flag, never a torn inode behind a clean one.

use super::*;
use crate::fs::vfs::{SetAttr, Times};

impl Ext2 {
    /// Apply `attr` to the inode at `path` and return its new metadata. The
    /// change is validated in full before anything is written, so a refused
    /// field (an id past 16 bits) leaves the inode untouched.
    pub(super) fn set_attributes(&self, path: &str, attr: &SetAttr) -> Result<Meta, FsError> {
        let _guard = self.lock.lock();
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        let ino = self.resolve(path)?;
        let mut inode = self.read_inode(ino)?;
        apply(&mut inode, attr)?;
        self.write_inode(ino, &inode)?;
        self.meta_of(ino)
    }
}

/// Write the selected fields of `attr` into an in-memory inode.
fn apply(inode: &mut [u8; INODE_CORE_SIZE], attr: &SetAttr) -> Result<(), FsError> {
    let uid = attr.uid.map(narrow_id).transpose()?;
    let gid = attr.gid.map(narrow_id).transpose()?;
    if let Some(mode) = attr.mode {
        let kind = le16(inode, INO_MODE) & S_IFMT;
        put16(inode, INO_MODE, kind | (mode & 0o7777));
    }
    if let Some(uid) = uid {
        put16(inode, INO_UID, uid);
    }
    if let Some(gid) = gid {
        put16(inode, INO_GID, gid);
    }
    for (field, time) in [
        (INO_ATIME, attr.atime),
        (INO_MTIME, attr.mtime),
        (INO_CTIME, attr.ctime),
    ] {
        if let Some(time) = time {
            put32(inode, field, disk_time(time));
        }
    }
    Ok(())
}

/// Only the low 16 bits of an owner are stored (`i_uid`/`i_gid`; the Linux
/// high halves in `osd2` are not used by this driver), so a larger id is
/// refused rather than truncated: truncating uid 65536 would give the file to
/// root. The same rule [`check_owner`] applies at creation.
fn narrow_id(id: u32) -> Result<u16, FsError> {
    u16::try_from(id).map_err(|_| FsError::Invalid)
}

/// A timestamp as the 32-bit inode field holds it. Linux reads these fields
/// as signed seconds, and this driver's own clock is unsigned, so only
/// `0..=i32::MAX` (1970 to 2038) means the same to both; anything outside is
/// clamped to that range, as Linux clamps a time a filesystem cannot hold.
pub(super) fn disk_time(time: i64) -> u32 {
    time.clamp(0, i64::from(i32::MAX)) as u32
}

/// The three timestamps of an inode.
pub(super) fn times_of(inode: &[u8; INODE_CORE_SIZE]) -> Times {
    Times {
        atime: i64::from(le32(inode, INO_ATIME)),
        mtime: i64::from(le32(inode, INO_MTIME)),
        ctime: i64::from(le32(inode, INO_CTIME)),
    }
}
