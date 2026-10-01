//! Directory listing.

use super::*;

impl Ext2 {
    /// The entries of the directory at `path`, without `.` and `..`.
    pub fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, Ext2Error> {
        let _guard = self.lock.lock();
        let ino = self.resolve(path)?;
        let inode = self.read_inode(ino)?;
        let blocks = self.dir_blocks(&inode)?;
        let size = self.block_size as usize;
        let mut entries = Vec::new();
        for block in blocks {
            let mut buf = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(block), &mut buf[..size])?;
            let mut offset = 0usize;
            for _ in 0..(size / DE_HEADER) {
                if offset + DE_HEADER > size {
                    break;
                }
                let entry_ino = le32(&buf, offset + DE_INO);
                let rec_len = le16(&buf, offset + DE_REC_LEN) as usize;
                let name_len = buf[offset + DE_NAME_LEN] as usize;
                if rec_len < DE_HEADER
                    || !rec_len.is_multiple_of(4)
                    || offset + rec_len > size
                    || name_len > rec_len - DE_HEADER
                {
                    return Err(Ext2Error::Invalid);
                }
                if entry_ino != 0 && name_len > 0 {
                    let name = &buf[offset + DE_HEADER..offset + DE_HEADER + name_len];
                    if name != b"." && name != b".." {
                        // Revision-0 entries carry no type byte: read the
                        // child inode instead. Types the VFS cannot hold
                        // (symlink, device, ...) are skipped, never guessed.
                        let file_type = buf[offset + DE_FILE_TYPE];
                        let kind = if self.has_file_type && file_type != 0 {
                            match file_type {
                                FT_REGULAR => Some(FileKind::File),
                                FT_DIRECTORY => Some(FileKind::Dir),
                                _ => None,
                            }
                        } else {
                            let child = self.read_inode(entry_ino)?;
                            kind_from_mode(le16(&child, INO_MODE))
                        };
                        if let Some(kind) = kind {
                            entries.push(DirEntry {
                                name: String::from_utf8_lossy(name).into_owned(),
                                ino: u64::from(entry_ino),
                                kind,
                            });
                        }
                    }
                }
                offset += rec_len;
                if offset == size {
                    break;
                }
            }
        }
        Ok(entries)
    }
}
