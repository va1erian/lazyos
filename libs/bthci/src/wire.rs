//! Bounds-checked little-endian reader and writer for HCI parameters.
//!
//! Every HCI field a controller or a command sender hands us is untrusted, so
//! the reader never indexes: each accessor returns `Err(Error::Truncated)`
//! when the bytes are not there.

use alloc::vec::Vec;

use crate::addr::BdAddr;
use crate::Error;

pub(crate) struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len()
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.buf.len() < n {
            return Err(Error::Truncated);
        }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Ok(head)
    }

    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.array::<1>()?[0])
    }

    pub fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub fn u24(&mut self) -> Result<u32, Error> {
        let b = self.array::<3>()?;
        Ok(u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16)
    }

    pub fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    pub fn addr(&mut self) -> Result<BdAddr, Error> {
        Ok(BdAddr(self.array()?))
    }

    pub fn rest(&mut self) -> &'a [u8] {
        core::mem::take(&mut self.buf)
    }

    /// Fail when bytes are left over: a command whose length disagrees with
    /// its layout is malformed, not "extended".
    pub fn finish(&self) -> Result<(), Error> {
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(Error::BadLength)
        }
    }
}

pub(crate) fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub(crate) fn put_u24(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes()[..3]);
}

pub(crate) fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub(crate) fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}
