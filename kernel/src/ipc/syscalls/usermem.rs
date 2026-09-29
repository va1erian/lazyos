//! User pointer validation and copying.

use super::*;

// Page-table entry bits and the raw entry reader live in `mem::pte`.
use pte::{
    ADDR as PTE_ADDR, HUGE as PTE_HUGE, PRESENT as PTE_PRESENT, USER as PTE_USER,
    WRITABLE as PTE_WRITABLE,
};

/// Highest address a user pointer may name: the canonical lower half. Anything
/// above is kernel memory and never a valid syscall buffer.
pub(super) const USER_MAX: u64 = 0x0000_8000_0000_0000;

/// Read one 64-bit page-table entry at `phys`.
pub(super) fn entry_at(phys: u64, index: usize) -> u64 {
    // Safety: `phys` names a live page table reachable through the physical
    // memory map, and `index` is masked to 9 bits by every caller.
    unsafe { pte::read(PhysAddr::new(phys), index) }
}

/// Validate that `[ptr, ptr + len)` is a user range the caller may access.
///
/// `write` also requires writability (privatizing a COW page when needed).
/// Not-present pages inside an `Anon`/`Heap` VMA are materialized exactly as a
/// page fault would. Fails fast with `-EFAULT`; allocates nothing for a range
/// that is not the caller's.
pub(crate) fn access_range(ptr: u64, len: usize, write: bool) -> Result<(), i64> {
    if len == 0 {
        return Ok(());
    }
    let end = ptr.checked_add(len as u64).ok_or(errno::EFAULT)?;
    let table = mem::kernel_table();
    let mut va = ptr & !0xfff;
    while va < end {
        translate(table, va, write)?;
        va += 4096;
    }
    Ok(())
}

/// Translate one user virtual address in `table` to a kernel-accessible
/// physical address (page base plus the in-page offset). `-EFAULT` when the
/// range is not present/accessible to user mode.
pub(super) fn translate(table: PhysAddr, va: u64, write: bool) -> Result<u64, i64> {
    if va >= USER_MAX {
        return Err(errno::EFAULT);
    }
    let l4 = entry_at(table.as_u64(), ((va >> 39) & 0x1ff) as usize);
    if l4 & PTE_PRESENT == 0 {
        return materialize(table, va, write);
    }
    if l4 & PTE_USER == 0 {
        return Err(errno::EFAULT);
    }
    let l3 = entry_at(l4 & PTE_ADDR, ((va >> 30) & 0x1ff) as usize);
    if l3 & PTE_PRESENT == 0 {
        return materialize(table, va, write);
    }
    if l3 & PTE_USER == 0 {
        return Err(errno::EFAULT);
    }
    if l3 & PTE_HUGE != 0 {
        if write && l3 & PTE_WRITABLE == 0 {
            return Err(errno::EFAULT);
        }
        return Ok((l3 & PTE_ADDR) + (va & ((1 << 30) - 1)));
    }
    let l2 = entry_at(l3 & PTE_ADDR, ((va >> 21) & 0x1ff) as usize);
    if l2 & PTE_PRESENT == 0 {
        return materialize(table, va, write);
    }
    if l2 & PTE_USER == 0 {
        return Err(errno::EFAULT);
    }
    if l2 & PTE_HUGE != 0 {
        if write && l2 & PTE_WRITABLE == 0 {
            return Err(errno::EFAULT);
        }
        return Ok((l2 & PTE_ADDR) + (va & ((1 << 21) - 1)));
    }
    let l1 = entry_at(l2 & PTE_ADDR, ((va >> 12) & 0x1ff) as usize);
    if l1 & PTE_PRESENT == 0 {
        return materialize(table, va, write);
    }
    // Device MMIO is never a syscall buffer: it is not RAM (issue #240).
    if l1 & PTE_USER == 0 || l1 & pte::MMIO != 0 {
        return Err(errno::EFAULT);
    }
    if write && l1 & PTE_WRITABLE == 0 {
        // A shared COW page: privatize it, then re-walk for the new frame.
        if !mem::cow_fault(table, va & !0xfff) {
            return Err(errno::EFAULT);
        }
        return translate(table, va, write);
    }
    Ok((l1 & PTE_ADDR) + (va & 0xfff))
}

/// Resolve a not-present page through the demand-zero path, then translate.
///
/// Only `Anon`/`Heap` VMAs materialize (see `mem::demand_fault`); anything else
/// is `-EFAULT`. The kernel already resolved faults through this path for user
/// tasks, so the copy helper uses the same rule instead of relying on a fault.
pub(super) fn materialize(table: PhysAddr, va: u64, write: bool) -> Result<u64, i64> {
    let mut error = PageFaultErrorCode::empty();
    if write {
        error.insert(PageFaultErrorCode::CAUSED_BY_WRITE);
    }
    if !mem::demand_fault(table, va, error) {
        return Err(errno::EFAULT);
    }
    translate(table, va, write)
}

/// Copy `len` bytes from a validated user range into a fresh `Vec`.
pub(super) fn copy_in(ptr: u64, len: usize) -> Result<Vec<u8>, i64> {
    access_range(ptr, len, false)?;
    let table = mem::kernel_table();
    let mut out: Vec<u8> = Vec::with_capacity(len);
    let mut done = 0usize;
    while done < len {
        // `access_range` proved `ptr + len` does not overflow, so this add is
        // safe for every `done < len`.
        let va = ptr + done as u64;
        let phys = translate(table, va, false)?;
        let chunk = (4096 - (va & 0xfff) as usize).min(len - done);
        let src = mem::phys_to_virt(PhysAddr::new(phys)).as_ptr::<u8>();
        // Safety: `phys` is a live user frame mapped through the kernel's
        // physical map, and `chunk` stays inside the page it points into.
        unsafe {
            core::ptr::copy_nonoverlapping(src, out.as_mut_ptr().add(done), chunk);
        }
        done += chunk;
    }
    // Safety: the loop wrote exactly `len` initialized bytes.
    unsafe { out.set_len(len) };
    Ok(out)
}

/// Copy `bytes` into a validated, writable user range.
///
/// Shared with the read-only monitor syscalls (`sysinfo`, `sys_tasks`) so an
/// unprivileged caller can never aim a kernel write at a kernel or unmapped
/// address: the whole range is checked before a byte is written.
pub(crate) fn copy_out(ptr: u64, bytes: &[u8]) -> Result<(), i64> {
    access_range(ptr, bytes.len(), true)?;
    let table = mem::kernel_table();
    let mut done = 0usize;
    while done < bytes.len() {
        let va = ptr + done as u64;
        let phys = translate(table, va, true)?;
        let chunk = (4096 - (va & 0xfff) as usize).min(bytes.len() - done);
        let dst = mem::phys_to_virt(PhysAddr::new(phys)).as_mut_ptr::<u8>();
        // Safety: `phys` is a live writable user frame mapped through the
        // kernel's physical map, and `chunk` stays inside the page.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr().add(done), dst, chunk);
        }
        done += chunk;
    }
    Ok(())
}
