//! Whole-file reads and writes by path, and `lookup`.

use super::*;

impl Ext2 {
    /// Metadata of the node at `path`.
    pub fn lookup(&self, path: &str) -> Result<InodeMeta, Ext2Error> {
        let _guard = self.lock.lock();
        let ino = self.resolve(path)?;
        self.meta_of(ino)
    }

    /// Read up to `buf.len()` bytes at `offset`; sparse holes read as zeros and a read
    /// past the end returns 0.
    pub fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, Ext2Error> {
        let _guard = self.lock.lock();
        let ino = self.resolve(path)?;
        let inode = self.read_inode(ino)?;
        if kind_from_mode(le16(&inode, INO_MODE)) != Some(FileKind::File) {
            return Err(Ext2Error::IsDir);
        }
        let size = self.file_size(&inode);
        if offset >= size || buf.is_empty() {
            return Ok(0);
        }
        let count = min(size - offset, buf.len() as u64) as usize;
        let block_size = u64::from(self.block_size);
        let size_usize = self.block_size as usize;
        let mut done = 0usize;
        while done < count {
            let position = offset + done as u64;
            let index = self.block_index(position)?;
            let inner = (position % block_size) as usize;
            let chunk = min(size_usize - inner, count - done);
            let block = self.block_map(&inode, index)?;
            if block == 0 {
                buf[done..done + chunk].fill(0); // a sparse hole reads as zero
            } else {
                let mut tmp = [0u8; MAX_BLOCK_SIZE];
                self.read_block(u64::from(block), &mut tmp[..size_usize])?;
                buf[done..done + chunk].copy_from_slice(&tmp[inner..inner + chunk]);
            }
            done += chunk;
        }
        Ok(done)
    }

    /// Write `data` at `offset`, growing the file. A write that fails part-way reports
    /// the bytes that landed.
    pub fn write(&self, path: &str, offset: u64, data: &[u8]) -> Result<usize, Ext2Error> {
        let _guard = self.lock.lock();
        let ino = self.resolve(path)?;
        let mut inode = self.read_inode(ino)?;
        if kind_from_mode(le16(&inode, INO_MODE)) != Some(FileKind::File) {
            return Err(Ext2Error::IsDir);
        }
        if data.is_empty() {
            return Ok(0);
        }
        // Sizes are 32-bit, so a write past the cap is short (or refused); it
        // must never wrap a huge offset onto a low block.
        let data = &data[..indirect::writable_len(offset, data.len())?];
        let block_size = u64::from(self.block_size);
        let size_usize = self.block_size as usize;
        let mut done = 0usize;
        let mut failure = None;
        while done < data.len() {
            let position = offset + done as u64;
            let index = self.block_index(position)?;
            let inner = (position % block_size) as usize;
            let chunk = min(size_usize - inner, data.len() - done);
            let (block, fresh) = match self.ensure_block(&mut inode, index) {
                Ok(mapped) => mapped,
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            };
            let mut tmp = [0u8; MAX_BLOCK_SIZE];
            if !fresh {
                if let Err(error) = self.read_block(u64::from(block), &mut tmp[..size_usize]) {
                    failure = Some(error);
                    break;
                }
            }
            // A fresh block is written from zeros, so a short write can never
            // expose stale bytes from the block's previous owner.
            tmp[inner..inner + chunk].copy_from_slice(&data[done..done + chunk]);
            if let Err(error) = self.write_block(u64::from(block), &tmp[..size_usize]) {
                failure = Some(error);
                break;
            }
            done += chunk;
        }
        // `ensure_block` allocated blocks and edited the in-memory inode as it
        // went. Persist the inode whatever happened, or every block allocated
        // before a failure (out of space, an I/O error) stays marked used in
        // the bitmap while no inode owns it: a permanent leak, and the bytes
        // already written vanish. The size covers exactly what landed.
        let landed = offset + done as u64;
        if done > 0 && landed > self.file_size(&inode) {
            put32(&mut inode, INO_SIZE, landed as u32);
        }
        touch(&mut inode, self.now());
        let persisted = self.write_inode(ino, &inode);
        match failure {
            // A short write reports the bytes that landed; the caller's next
            // write sees the failure again with nothing written.
            Some(error) if done == 0 => Err(error),
            _ => persisted.map(|()| done),
        }
    }
}
