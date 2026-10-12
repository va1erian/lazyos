//! `memfd_create`, its file operations, and `mmap` of it (and of anonymous
//! memory) `MAP_SHARED`: two address spaces see the same frames, `fork`
//! keeps them shared, a private mapping copies on write, and the frames and
//! the user-memory charge come back with the last of the mapping, the
//! descriptor and the address space.

use super::*;
use crate::mem::memfd::{live_pages, Memfd};
use crate::quota::{self, Resource};
use alloc::sync::Arc;

mod accounting;
mod files;
mod sharing;

pub use accounting::*;
pub use files::*;
pub use sharing::*;

const MEMFD_CREATE: u64 = 319;
const MFD_CLOEXEC: u64 = 1;
const MFD_ALLOW_SEALING: u64 = 2;
const PAGE: u64 = 4096;
const PROT_READ: u64 = 1;
const PROT_WRITE: u64 = 2;
const PROT_NONE: u64 = 0;
const MAP_SHARED: u64 = 0x1;
const MAP_PRIVATE: u64 = 0x2;
const MAP_FIXED: u64 = 0x10;
const MAP_ANONYMOUS: u64 = 0x20;
const SEEK_SET: u64 = 0;
const SEEK_CUR: u64 = 1;
const SEEK_END: u64 = 2;
const PTE_SHARED: u64 = 1 << 52;
const ENODEV: u64 = neg(19);
const EFBIG: u64 = neg(27);
const ENOMEM: u64 = neg(12);

/// Where the cases map things: well inside the mmap region, apart from the
/// kernel task's own bump pointer.
const BASE: u64 = process::linux::MMAP_BASE + 0x400_0000;

fn memfd_create(name: &str, flags: u64) -> u64 {
    let name = cpath(name);
    sys(MEMFD_CREATE, &[name.as_ptr() as u64, flags])
}

fn mmap(addr: u64, len: u64, prot: u64, flags: u64, fd: u64, offset: u64) -> u64 {
    sys(9, &[addr, len, prot, flags, fd, offset])
}

fn munmap(addr: u64, len: u64) -> u64 {
    sys(11, &[addr, len])
}

fn mprotect(addr: u64, len: u64, prot: u64) -> u64 {
    sys(10, &[addr, len, prot])
}

fn write_fd(fd: u64, data: &[u8]) -> u64 {
    sys(1, &[fd, data.as_ptr() as u64, data.len() as u64])
}

fn read_fd(fd: u64, len: usize) -> Vec<u8> {
    let mut buf = alloc::vec![0xEEu8; len];
    let n = sys(0, &[fd, buf.as_mut_ptr() as u64, len as u64]);
    buf.truncate(if (n as i64) > 0 { n as usize } else { 0 });
    buf
}

fn pread_fd(fd: u64, len: usize, offset: u64) -> Vec<u8> {
    let mut buf = alloc::vec![0xEEu8; len];
    let n = sys(17, &[fd, buf.as_mut_ptr() as u64, len as u64, offset]);
    buf.truncate(if (n as i64) > 0 { n as usize } else { 0 });
    buf
}

/// The memory file behind `fd`.
fn memfd_of(fd: u64) -> Result<Arc<Memfd>, String> {
    match task::fd_clone(fd as usize) {
        Some(task::Fd::Memfd { ref mem }) => Ok(Arc::clone(mem)),
        _ => Err(format!("descriptor {fd} is not a memfd")),
    }
}

/// An address space the cases map into, entered with [`Space::enter`]; the
/// boot table comes back, and the space is freed, when it drops.
struct Space {
    table: PhysAddr,
    boot: PhysAddr,
}

impl Space {
    fn new() -> Result<Space, String> {
        let table = mem::new_user_table().ok_or("new_user_table failed")?;
        Ok(Space {
            table,
            boot: mem::kernel_table(),
        })
    }

    /// A space made from this one (a `fork`), returning to the same boot table.
    fn child(&self, table: PhysAddr) -> Space {
        Space {
            table,
            boot: self.boot,
        }
    }

    fn enter(&self) {
        mem::switch_to(self.table);
    }

    /// Store `bytes` at `va` as the process would: a read-only (COW) page
    /// takes the write fault first.
    fn store(&self, va: u64, bytes: &[u8]) -> Result<(), String> {
        for (index, byte) in bytes.iter().enumerate() {
            let at = va + index as u64;
            let page = at & !(PAGE - 1);
            let entry = raw_entry(self.table, page).ok_or(format!("{page:#x} unmapped"))?;
            if entry & PTE_WRITABLE == 0 {
                check!(
                    mem::cow_fault(self.table, page),
                    "write fault refused at {at:#x}"
                );
            }
            let frame = frame_of(self.table, page)?;
            let ptr = mem::phys_to_virt(PhysAddr::new(frame)).as_mut_ptr::<u8>();
            // Safety: the frame is mapped by the physical-memory window and
            // `at & 0xfff` stays inside it.
            unsafe { ptr.add((at & (PAGE - 1)) as usize).write_volatile(*byte) };
        }
        Ok(())
    }

    /// Load `len` bytes at `va`.
    fn load(&self, va: u64, len: usize) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        for index in 0..len {
            let at = va + index as u64;
            let frame = frame_of(self.table, at & !(PAGE - 1))?;
            let ptr = mem::phys_to_virt(PhysAddr::new(frame)).as_ptr::<u8>();
            // Safety: as in `store`.
            out.push(unsafe { ptr.add((at & (PAGE - 1)) as usize).read_volatile() });
        }
        Ok(out)
    }

    fn frame(&self, va: u64) -> Result<PhysAddr, String> {
        Ok(PhysAddr::new(frame_of(self.table, va)?))
    }

    fn entry(&self, va: u64) -> Result<u64, String> {
        raw_entry(self.table, va).ok_or(format!("{va:#x} unmapped"))
    }
}

impl Drop for Space {
    fn drop(&mut self) {
        mem::switch_to(self.boot);
        mem::free_user_table(self.table);
    }
}

fn refs(frame: PhysAddr) -> u32 {
    mem::frame_refcount(frame)
}

fn close_fd(fd: u64) {
    sys(3, &[fd]);
}
