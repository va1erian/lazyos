//! Ring-3 execution: the `int 0x80` syscall gate and a static ELF64 loader.
//!
//! The loader maps a program into a given address space ([`load_image`]); the
//! scheduler (`crate::task`) then runs it in ring 3. Syscalls reach the kernel
//! through the gate installed at vector `0x80`.

use alloc::vec::Vec;
use core::arch::global_asm;
use x86_64::structures::idt::HandlerFunc;
use x86_64::{PhysAddr, VirtAddr};
use xmas_elf::program::{SegmentData, Type as ProgramType};
use xmas_elf::ElfFile;

use crate::mem::vma::{Kind, Prot};
use crate::task;
use crate::{fs, input::keyboard, mem};

pub mod linux;

/// Base of the user heap (grows up toward the stack).
pub const USER_HEAP_BASE: u64 = 0x60_0000;
/// Top of the user stack (grows down).
pub const USER_STACK_TOP: u64 = 0x80_0000;
/// User stack size.
pub const USER_STACK_SIZE: u64 = 0x2_0000;

/// Saved general-purpose registers, laid out to match the syscall stub's pushes.
#[repr(C)]
struct Regs {
    rax: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rdx: u64,
    rsi: u64,
    rdi: u64,
}

// Syscall entry stub: save argument registers, dispatch, restore, iretq.
global_asm!(
    r#"
    .global syscall_isr
    syscall_isr:
        push rdi
        push rsi
        push rdx
        push r8
        push r9
        push r10
        push rax
        mov rdi, rsp
        call syscall_dispatch
        pop rax
        pop r10
        pop r9
        pop r8
        pop rdx
        pop rsi
        pop rdi
        iretq
    "#
);

extern "C" {
    fn syscall_isr();
}

/// The handler to install at vector `0x80` (DPL 3).
pub fn syscall_gate() -> HandlerFunc {
    // Safety: `syscall_isr` is a naked ISR with a compatible (no ABI) signature.
    unsafe { core::mem::transmute::<*const (), HandlerFunc>(syscall_isr as *const ()) }
}

#[no_mangle]
extern "C" fn syscall_dispatch(regs: *mut Regs) {
    // Safety: the stub passes a valid pointer to saved registers.
    let regs = unsafe { &mut *regs };
    if regs.rax == 0 {
        exit(regs.rdi as u32);
    }
    regs.rax = match regs.rax {
        1 => sys_write(regs.rdi, regs.rsi),
        2 => sys_read_char(),
        3 => sys_read_file(regs.rdi, regs.rsi, regs.rdx),
        4 => sys_sbrk(regs.rdi),
        // 5: the native Messenger surface (issue #69): `rdi` is the op code,
        // `rsi` points at a `MsgArgs` block and `rdx` at a `MsgResult` block.
        5 => crate::ipc::syscalls::dispatch(regs.rdi, regs.rsi, regs.rdx),
        _ => u64::MAX,
    };
}

/// Test-harness entry into the native syscall surface (issue #62 pattern):
/// drive one syscall exactly as the `int 0x80` gate would, without the ring
/// transition. Compiled only for the in-kernel suite.
#[cfg(laZYOS_TESTS)]
pub fn dispatch_for_test(nr: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    match nr {
        5 => crate::ipc::syscalls::dispatch(a1, a2, a3),
        _ => u64::MAX,
    }
}

/// syscall 1: write bytes to the task's terminal (and the serial log).
fn sys_write(ptr: u64, len: u64) -> u64 {
    // Safety: syscalls only pass pointers into the (mapped) user address space.
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    task::write_output(bytes);
    crate::serial::write_bytes(bytes);
    len
}

/// syscall 2: block until a key is routed to this task, then return its code.
fn sys_read_char() -> u64 {
    loop {
        if let Some(key) = task::take_key() {
            return match key {
                keyboard::Key::Char(c) => c as u64,
                keyboard::Key::Enter => b'\n' as u64,
                keyboard::Key::Space => b' ' as u64,
                keyboard::Key::Backspace => 8,
                keyboard::Key::Tab => b'\t' as u64,
                keyboard::Key::Escape => 27,
                _ => 0,
            };
        }
        // Interrupts are disabled inside the gate; enable them so the timer can
        // preempt us (letting other tasks run) and the keyboard can deliver keys.
        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
    }
}

/// Read a NUL-terminated string from user memory.
fn user_cstr(ptr: u64) -> &'static str {
    let mut len = 0usize;
    // Safety: the caller must pass a valid, NUL-terminated user pointer.
    unsafe {
        while len < 4096 && core::ptr::read_volatile((ptr as *const u8).add(len)) != 0 {
            len += 1;
        }
        let bytes = core::slice::from_raw_parts(ptr as *const u8, len);
        core::str::from_utf8(bytes).unwrap_or("")
    }
}

/// syscall 3: read a file into a user buffer. Returns the count, or `u64::MAX`.
fn sys_read_file(name_ptr: u64, buf_ptr: u64, buf_len: u64) -> u64 {
    let name = user_cstr(name_ptr);
    match fs::read(name) {
        Some(bytes) => {
            let count = bytes.len().min(buf_len as usize);
            // Safety: the destination is a valid user buffer of `buf_len` bytes.
            unsafe {
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf_ptr as *mut u8, count);
            }
            count as u64
        }
        None => u64::MAX,
    }
}

/// syscall 4: grow this task's heap; returns the previous break or `u64::MAX`.
///
/// The new range is recorded as a `Heap` VMA and populated on first touch
/// (demand-zero), so a large `sbrk` costs no frames until the program uses
/// them. Shrinking releases the pages under the new break.
fn sys_sbrk(increment: u64) -> u64 {
    let current = task::heap_break();
    if increment == 0 {
        return current;
    }
    let page = 4096u64;
    let Some(target) = current.checked_add(increment) else {
        return u64::MAX;
    };
    let new_break = (target + page - 1) & !(page - 1);
    if new_break > USER_STACK_TOP - USER_STACK_SIZE {
        return u64::MAX;
    }
    let table = mem::kernel_table();
    if new_break > current {
        mem::vma::insert(
            table,
            current,
            new_break,
            Prot::READ | Prot::WRITE,
            Kind::Heap,
        );
    } else if new_break < current {
        mem::vma::remove(table, new_break, current);
        mem::unmap_range(table, new_break, current);
    }
    task::set_heap_break(new_break);
    current
}

/// syscall 0: terminate the current task.
fn exit(_code: u32) -> ! {
    serial_println!("user: task exited");
    task::finish_current(0);
    // Wait for the scheduler to switch to another task.
    loop {
        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
    }
}

/// Map a program's `PT_LOAD` segments into `table` and return its entry point.
///
/// Segments are mapped eagerly (their contents must exist before the program
/// runs) with the protection the ELF header asks for: read always, write only
/// for `PF_W`, execute only for `PF_X`. Each segment is recorded as a `File`
/// VMA so `munmap`/`mprotect` and diagnostics see the same layout the hardware
/// does.
pub fn load_segments(table: PhysAddr, elf_bytes: &[u8]) -> Result<u64, &'static str> {
    let elf = ElfFile::new(elf_bytes).map_err(|_| "not a valid ELF")?;
    let entry = elf.header.pt2.entry_point();

    let mut pages: Vec<(u64, u64)> = Vec::new();
    for program_header in elf.program_iter() {
        if program_header.get_type() != Ok(ProgramType::Load) {
            continue;
        }
        let vaddr = program_header.virtual_addr();
        let mem_size = program_header.mem_size();
        let start = vaddr & !0xFFF;
        let end = (vaddr + mem_size + 0xFFF) & !0xFFF;

        let flags = program_header.flags();
        let mut prot = Prot::READ;
        if flags.is_write() {
            prot = prot | Prot::WRITE;
        }
        if flags.is_execute() {
            prot = prot | Prot::EXEC;
        }

        let mut va = start;
        while va < end {
            if phys_for(&pages, va).is_none() {
                let phys = mem::alloc_zeroed_frame().ok_or("out of memory")?;
                if !mem::map_page_in(table, VirtAddr::new(va), phys, mem::prot_flags(prot)) {
                    return Err("failed to map segment");
                }
                pages.push((va, phys.as_u64()));
            }
            va += 4096;
        }

        let data = program_header
            .get_data(&elf)
            .map_err(|_| "bad segment data")?;
        if let SegmentData::Undefined(file_bytes) = data {
            for (i, &byte) in file_bytes.iter().enumerate() {
                let va = vaddr + i as u64;
                if let Some(phys) = phys_for(&pages, va) {
                    let dst = mem::phys_to_virt(PhysAddr::new(phys)) + (va & 0xFFF);
                    // Safety: within the freshly-mapped user page.
                    unsafe { core::ptr::write_volatile(dst.as_mut_ptr::<u8>(), byte) };
                }
            }
        }

        mem::vma::insert(table, start, end, prot, Kind::File);
    }

    Ok(entry)
}

/// Load a static ELF64 image and map the native user stack.
pub fn load_image(table: PhysAddr, elf_bytes: &[u8]) -> Result<u64, &'static str> {
    let entry = load_segments(table, elf_bytes)?;
    map_range_kind(
        table,
        USER_STACK_TOP - USER_STACK_SIZE,
        USER_STACK_TOP,
        Prot::READ | Prot::WRITE,
        Kind::Stack,
    )?;
    Ok(entry)
}

/// Map `[start, end)` as zeroed anonymous user pages into `table` (eager), for
/// callers that must have the pages present immediately. Linux `mmap`/`brk`
/// prefer the lazy VMA path; the kernel test suite uses this to build scratch
/// address spaces.
#[allow(dead_code)]
pub fn map_range(table: PhysAddr, start: u64, end: u64) -> Result<Vec<(u64, u64)>, &'static str> {
    map_range_kind(table, start, end, Prot::READ | Prot::WRITE, Kind::Anon)
}

/// [`map_range`] with an explicit protection and VMA kind.
pub fn map_range_kind(
    table: PhysAddr,
    start: u64,
    end: u64,
    prot: Prot,
    kind: Kind,
) -> Result<Vec<(u64, u64)>, &'static str> {
    let mut pages = Vec::new();
    let mut va = start & !0xFFF;
    while va < end {
        let phys = mem::alloc_zeroed_frame().ok_or("out of memory")?;
        if !mem::map_page_in(table, VirtAddr::new(va), phys, mem::prot_flags(prot)) {
            return Err("failed to map user page");
        }
        pages.push((va, phys.as_u64()));
        va += 4096;
    }
    mem::vma::insert(table, start, end, prot, kind);
    Ok(pages)
}

/// Physical frame backing a page recorded by [`map_range`].
pub fn page_phys(pages: &[(u64, u64)], va: u64) -> Option<u64> {
    phys_for(pages, va)
}

fn phys_for(mappings: &[(u64, u64)], va: u64) -> Option<u64> {
    let page = va & !0xFFF;
    mappings
        .iter()
        .find(|(page_vaddr, _)| *page_vaddr == page)
        .map(|(_, phys)| *phys)
}
