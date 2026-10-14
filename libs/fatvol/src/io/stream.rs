//! The byte stream `rust-fatfs` reads and writes: a shared handle on the
//! [`Cache`].

use alloc::rc::Rc;
use core::cell::RefCell;

use fatfs::{IoBase, Read, Seek, SeekFrom, Write};

use super::{BlockIo, Cache, Fault};

/// One [`Cache`] shared between the filesystem object (which owns its
/// storage) and the volume (which flushes it): `rust-fatfs` takes its
/// storage by value, so the stream is a handle.
pub struct Shared<D>(pub Rc<RefCell<Cache<D>>>);

impl<D> Clone for Shared<D> {
    fn clone(&self) -> Self {
        Shared(Rc::clone(&self.0))
    }
}

impl<D> IoBase for Shared<D> {
    type Error = Fault;
}

impl<D: BlockIo> Read for Shared<D> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Fault> {
        let mut cache = self.0.borrow_mut();
        let remaining = cache.len_bytes().saturating_sub(cache.pos);
        let take = usize::try_from(remaining).map_or(buf.len(), |r| r.min(buf.len()));
        let pos = cache.pos;
        cache.read_at(pos, &mut buf[..take])?;
        cache.pos += take as u64;
        Ok(take)
    }
}

impl<D: BlockIo> Write for Shared<D> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Fault> {
        let mut cache = self.0.borrow_mut();
        let remaining = cache.len_bytes().saturating_sub(cache.pos);
        let take = usize::try_from(remaining).map_or(buf.len(), |r| r.min(buf.len()));
        if take == 0 && !buf.is_empty() {
            return Ok(0);
        }
        let pos = cache.pos;
        cache.write_at(pos, &buf[..take])?;
        cache.pos += take as u64;
        Ok(take)
    }

    /// `rust-fatfs` flushes at its own moments; the volume decides when the
    /// device is written ([`Cache::flush_dirty`] at the end of an operation).
    fn flush(&mut self) -> Result<(), Fault> {
        Ok(())
    }
}

impl<D: BlockIo> Seek for Shared<D> {
    fn seek(&mut self, from: SeekFrom) -> Result<u64, Fault> {
        let mut cache = self.0.borrow_mut();
        let len = cache.len_bytes() as i64;
        let target = match from {
            SeekFrom::Start(at) => i64::try_from(at).map_err(|_| Fault::Eof)?,
            SeekFrom::End(delta) => len.checked_add(delta).ok_or(Fault::Eof)?,
            SeekFrom::Current(delta) => (cache.pos as i64).checked_add(delta).ok_or(Fault::Eof)?,
        };
        if target < 0 {
            return Err(Fault::Eof);
        }
        cache.pos = target as u64;
        Ok(cache.pos)
    }
}
