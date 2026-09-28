//! `memstress` — allocator growth (`Vec`/`Box`, brk) and raw anonymous
//! `mmap`/`mprotect`/`munmap` through the musl symbols.

mod common;

use core::ffi::c_void;
use std::alloc::{alloc, dealloc, Layout};

unsafe extern "C" {
    fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: i32,
        flags: i32,
        fd: i32,
        offset: i64,
    ) -> *mut c_void;
    fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32;
    fn munmap(addr: *mut c_void, len: usize) -> i32;
}

const PROT_READ: i32 = 1;
const PROT_WRITE: i32 = 2;
const MAP_PRIVATE: i32 = 0x02;
const MAP_ANONYMOUS: i32 = 0x20;

const SYS_BRK: u64 = 12;

/// One-argument raw syscall (`brk`), avoiding a libc dependency for this probe.
unsafe fn syscall1(nr: u64, a1: u64) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") nr as i64 => ret,
        in("rdi") a1,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack)
    );
    ret
}

fn note(first: &mut Option<String>, reason: String) {
    if first.is_none() {
        *first = Some(reason);
    }
}

fn main() {
    let mut first: Option<String> = None;

    // Large Vec growth: pushes force the allocator to grow repeatedly.
    let mut values: Vec<u32> = Vec::new();
    for i in 0..1_000_000u32 {
        values.push(i);
    }
    let sum: u64 = values.iter().map(|v| *v as u64).sum();
    if values.len() != 1_000_000 {
        note(&mut first, format!("Vec length {}", values.len()));
    }
    if sum != 499_999_500_000 {
        note(&mut first, format!("Vec sum {sum}"));
    }

    // A large heap allocation with a written and verified pattern.
    let mut boxed = vec![0u64; 131_072].into_boxed_slice();
    for (i, slot) in boxed.iter_mut().enumerate() {
        *slot = i as u64;
    }
    if boxed.iter().enumerate().any(|(i, v)| *v != i as u64) {
        note(&mut first, "large Box pattern mismatch".to_string());
    }

    // Over-aligned allocations must honour the requested alignment.
    for align in [16usize, 64, 256, 4096] {
        let layout = Layout::from_size_align(4096, align).unwrap();
        let ptr = unsafe { alloc(layout) };
        if ptr.is_null() {
            note(&mut first, format!("alloc align {align} failed"));
            continue;
        }
        if ptr as usize % align != 0 {
            note(&mut first, format!("alloc align {align} misaligned"));
        }
        unsafe {
            core::ptr::write_bytes(ptr, 0xA5, 4096);
            dealloc(ptr, layout);
        }
    }

    // Raw brk: read the break, grow it, touch the new page, shrink back.
    let current = unsafe { syscall1(SYS_BRK, 0) };
    if current <= 0 {
        note(&mut first, format!("brk(0) returned {current}"));
    } else {
        let grown = current + 0x1_0000;
        let result = unsafe { syscall1(SYS_BRK, grown as u64) };
        if result != grown {
            note(&mut first, format!("brk grow returned {result} != {grown}"));
        } else {
            unsafe { core::ptr::write_volatile((result as u64 - 8) as *mut u64, 0x5A5A_5A5A) };
            let read = unsafe { core::ptr::read_volatile((result as u64 - 8) as *const u64) };
            if read != 0x5A5A_5A5A {
                note(&mut first, "brk memory did not round-trip".to_string());
            }
        }
        let restored = unsafe { syscall1(SYS_BRK, current as u64) };
        if restored != current {
            note(
                &mut first,
                format!("brk restore returned {restored} != {current}"),
            );
        }
    }

    // Raw anonymous mmap: write, flip protections, read again, unmap.
    let len = 64 * 1024;
    let base = unsafe {
        mmap(
            core::ptr::null_mut(),
            len,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if base as isize == -1 {
        note(&mut first, "anonymous mmap failed".to_string());
    } else {
        unsafe { core::ptr::write_bytes(base as *mut u8, 0x3C, len) };
        let intact = (0..len)
            .step_by(4096)
            .all(|off| unsafe { core::ptr::read_volatile((base as *const u8).add(off)) } == 0x3C);
        if !intact {
            note(&mut first, "mmap write/read pattern mismatch".to_string());
        }
        if unsafe { mprotect(base, len, PROT_READ) } != 0 {
            note(&mut first, "mprotect read-only failed".to_string());
        }
        if unsafe { mprotect(base, len, PROT_READ | PROT_WRITE) } != 0 {
            note(&mut first, "mprotect read-write failed".to_string());
        }
        if unsafe { munmap(base, len) } != 0 {
            note(&mut first, "munmap failed".to_string());
        }
    }

    match first {
        Some(reason) => common::fail("memstress", &reason),
        None => common::pass("memstress"),
    }
}
