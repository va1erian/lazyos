//! File-backed `mmap` of regular files (docs: `fs::openfile::pagecache`,
//! `mem::filemap`), on the ext2 `/data` volume (a node-backed file) and on
//! the `/tmp` ramfs (a path-backed one): lazy demand-faulting, private
//! copy-on-write, shared coherence with `read`/`write` and between address
//! spaces, `fork`, truncation, write-back at `msync`/`munmap`/exit and
//! across a remount, plus a soak.

use super::*;
use crate::mem::vma::Kind;
use crate::quota::{self, Resource};

mod basic;
mod coherence;
mod killed;
mod lifecycle;
mod parents;
mod refresh;

pub(super) const PAGE: u64 = 4096;
pub(super) const PROT_NONE: u64 = 0;
pub(super) const PROT_READ: u64 = 1;
pub(super) const PROT_WRITE: u64 = 2;
pub(super) const RW: u64 = PROT_READ | PROT_WRITE;
pub(super) const MAP_SHARED: u64 = 0x1;
pub(super) const MAP_PRIVATE: u64 = 0x2;
pub(super) const MAP_FIXED: u64 = 0x10;
pub(super) const MS_SYNC: u64 = 4;
pub(super) const SYS_MMAP: u64 = 9;
pub(super) const SYS_MPROTECT: u64 = 10;
pub(super) const SYS_MUNMAP: u64 = 11;
pub(super) const SYS_MREMAP: u64 = 25;
pub(super) const SYS_MSYNC: u64 = 26;
pub(super) const MREMAP_MAYMOVE: u64 = 1;
pub(super) const MREMAP_FIXED: u64 = 2;
pub(super) const ENODEV_: u64 = 19;

/// Where the cases map: well inside the mmap region.
pub(super) const BASE: u64 = process::linux::MMAP_BASE + 0x800_0000;

pub(super) fn sys6(nr: u64, args: [u64; 6]) -> u64 {
    process::linux::dispatch_args6_for_test(nr, args)
}

pub(super) fn mmap(addr: u64, len: u64, prot: u64, flags: u64, fd: u64, offset: u64) -> u64 {
    sys6(SYS_MMAP, [addr, len, prot, flags, fd, offset])
}

pub(super) fn munmap(addr: u64, len: u64) -> u64 {
    syscall(SYS_MUNMAP, addr, len, 0, 0)
}

pub(super) fn mprotect(addr: u64, len: u64, prot: u64) -> u64 {
    syscall(SYS_MPROTECT, addr, len, prot, 0)
}

pub(super) fn msync(addr: u64, len: u64, flags: u64) -> u64 {
    syscall(SYS_MSYNC, addr, len, flags, 0)
}

pub(super) fn ftruncate(fd: u64, len: u64) -> u64 {
    syscall(SYS_FTRUNCATE, fd, len, 0, 0)
}

/// `len` bytes that differ per page and per `seed`.
pub(super) fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| {
            (i as u8)
                .wrapping_mul(7)
                .wrapping_add(seed)
                .wrapping_add((i / 4096) as u8)
        })
        .collect()
}

/// An address space the cases map into.
pub(super) struct Space {
    pub(super) table: PhysAddr,
    boot: PhysAddr,
}

impl Space {
    pub(super) fn new() -> Result<Space, String> {
        let table = mem::new_user_table().ok_or("new_user_table failed")?;
        Ok(Space {
            table,
            boot: mem::kernel_table(),
        })
    }

    pub(super) fn child(&self, table: PhysAddr) -> Space {
        Space {
            table,
            boot: self.boot,
        }
    }

    pub(super) fn enter(&self) {
        mem::switch_to(self.table);
    }

    /// Touch the page at `va` the way the CPU would: a missing page takes the
    /// demand fault, a store to a read-only one the copy-on-write fault.
    pub(super) fn touch(&self, va: u64, write: bool) -> Result<(), String> {
        let page = va & !(PAGE - 1);
        for _ in 0..3 {
            match raw_entry(self.table, page) {
                None => {
                    let mut error = PageFaultErrorCode::empty();
                    if write {
                        error |= PageFaultErrorCode::CAUSED_BY_WRITE;
                    }
                    check!(
                        mem::demand_fault(self.table, va, error),
                        "demand fault refused at {va:#x}"
                    );
                }
                Some(entry) if write && entry & PTE_WRITABLE == 0 => {
                    check!(
                        mem::cow_fault(self.table, page),
                        "write fault refused at {va:#x}"
                    );
                }
                Some(_) => return Ok(()),
            }
        }
        Err(format!("{va:#x} never settled"))
    }

    pub(super) fn can_touch(&self, va: u64, write: bool) -> bool {
        self.touch(va, write).is_ok()
    }

    pub(super) fn load(&self, va: u64, len: usize) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(len);
        for index in 0..len {
            let at = va + index as u64;
            self.touch(at, false)?;
            let frame = frame_of(self.table, at & !(PAGE - 1))?;
            let ptr = mem::phys_to_virt(PhysAddr::new(frame)).as_ptr::<u8>();
            // SAFETY: the frame is mapped by the physical-memory window and
            // the offset stays inside it.
            out.push(unsafe { ptr.add((at & (PAGE - 1)) as usize).read_volatile() });
        }
        Ok(out)
    }

    pub(super) fn store(&self, va: u64, bytes: &[u8]) -> Result<(), String> {
        for (index, byte) in bytes.iter().enumerate() {
            let at = va + index as u64;
            self.touch(at, true)?;
            let frame = frame_of(self.table, at & !(PAGE - 1))?;
            let ptr = mem::phys_to_virt(PhysAddr::new(frame)).as_mut_ptr::<u8>();
            // SAFETY: as in `load`.
            unsafe { ptr.add((at & (PAGE - 1)) as usize).write_volatile(*byte) };
        }
        Ok(())
    }

    pub(super) fn entry(&self, va: u64) -> Option<u64> {
        raw_entry(self.table, va)
    }
}

impl Drop for Space {
    fn drop(&mut self) {
        mem::switch_to(self.boot);
        mem::free_user_table(self.table);
    }
}

/// Run `case` once per backing: a node-backed ext2 file and a path-backed
/// ramfs one.
pub(super) fn on_both(case: fn(&str) -> Result<(), String>) -> Result<(), String> {
    let data = Data::new(0)?;
    case("/data")?;
    case("/tmp")?;
    data.check_clean()
}

pub(super) fn open_rw(path: &str) -> Result<u64, String> {
    let fd = open(path, O_RDWR);
    check!(fd < 64, "open({path}) returned {fd:#x}");
    Ok(fd)
}

pub(super) fn open_ro(path: &str) -> Result<u64, String> {
    let fd = open(path, O_RDONLY);
    check!(fd < 64, "open({path}) returned {fd:#x}");
    Ok(fd)
}

pub(super) fn pread_ok(fd: u64, len: usize, offset: u64) -> Result<Vec<u8>, String> {
    pread(fd, len, offset).map_err(|code| format!("pread returned {code:#x}"))
}

pub(super) fn bytes_of(path: &str) -> Result<Vec<u8>, String> {
    slurp(path)
}

pub(in crate::tests) const CASES: &[(&str, Test)] = &[
    (
        "linux_filemap_read_then_write_keeps_writable_parents",
        parents::read_then_write_keeps_writable_parents,
    ),
    ("linux_filemap_private", basic::private_mapping),
    ("linux_filemap_shared", basic::shared_mapping),
    (
        "linux_filemap_shared_survives_exit_and_remount",
        basic::shared_survives_remount,
    ),
    (
        "linux_filemap_refusals_and_protection",
        basic::refusals_and_protection,
    ),
    ("linux_filemap_fork", lifecycle::fork_keeps_mappings),
    (
        "linux_filemap_truncate_and_unlink",
        lifecycle::truncation_and_unlink,
    ),
    (
        "linux_filemap_partial_unmap_and_mremap",
        lifecycle::partial_unmap_and_remap,
    ),
    ("linux_filemap_soak", lifecycle::leaks_nothing),
    (
        "linux_filemap_dirty_tracking_and_flusher",
        coherence::dirty_tracking_and_flusher,
    ),
    (
        "linux_filemap_parked_fault_stale_translation",
        coherence::parked_fault_cannot_leave_a_stale_translation,
    ),
    (
        "linux_filemap_lost_fault_race_no_extra_reference",
        coherence::lost_fault_race_keeps_no_extra_reference,
    ),
    (
        "linux_filemap_store_during_writeback",
        coherence::store_during_writeback_is_not_lost,
    ),
    (
        "linux_filemap_killed_writeback_stops_waiting",
        killed::killed_writeback_stops_waiting,
    ),
    (
        "linux_filemap_fork_write_protect",
        coherence::fork_and_write_protect,
    ),
    (
        "linux_filemap_reaped_dirty_page_defers_the_write",
        coherence::reaped_with_a_dirty_page_defers_the_write,
    ),
    (
        "linux_filemap_truncate_down_and_up",
        coherence::truncate_down_and_up_unmaps_dropped_pages,
    ),
    (
        "linux_filemap_parked_fault_soak",
        coherence::parked_fault_soak,
    ),
    (
        "linux_filemap_refresh_keeps_a_stored_page",
        refresh::refresh_keeps_a_stored_page,
    ),
    (
        "linux_filemap_change_behind_the_cache",
        coherence::change_behind_the_cache_is_refreshed,
    ),
];
