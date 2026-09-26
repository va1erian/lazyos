//! Ring-3 user programs: the `int 0x80` syscall gate and a static ELF64 loader.

use alloc::vec::Vec;
use core::arch::{asm, global_asm};
use x86_64::structures::idt::HandlerFunc;
use x86_64::structures::paging::PageTableFlags;
use x86_64::{PhysAddr, VirtAddr};
use xmas_elf::program::{SegmentData, Type as ProgramType};
use xmas_elf::ElfFile;

use crate::arch::gdt;
use crate::console;
use crate::input::keyboard;
use crate::mem;

/// Fixed virtual base for user programs.
pub const USER_CODE_BASE: u64 = 0x40_0000;
/// Top of the user stack (grows down).
pub const USER_STACK_TOP: u64 = 0x80_0000;
/// User stack size.
pub const USER_STACK_SIZE: u64 = 0x2_0000;

/// Kernel stack pointer saved when entering user mode; restored by `exit`.
#[no_mangle]
pub static mut SAVED_KERNEL_RSP: u64 = 0;

/// Saved general-purpose registers, laid out to match the push order below.
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

// Syscall entry stub: save the argument registers, dispatch, restore, iretq.
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

// Ring-3 entry trampoline. Captures `rsp` at its very entry (before any Rust
// prologue) so `exit` can restore the caller's stack and `ret` back to it.
// Arguments (System V): rdi = entry, rsi = stack, rdx = user CS, rcx = user SS.
global_asm!(
    r#"
    .global enter_user_asm
    enter_user_asm:
        mov [rip + SAVED_KERNEL_RSP], rsp
        mov rax, rcx
        mov ds, ax
        mov es, ax
        push rcx
        push rsi
        push 0x202
        push rdx
        push rdi
        iretq
    "#
);

extern "C" {
    fn enter_user_asm(entry: u64, stack: u64, code: u64, data: u64);
}

/// The handler to install at vector `0x80` (DPL 3).
pub fn syscall_gate() -> HandlerFunc {
    // Safety: `syscall_isr` is a naked ISR with a compatible (no ABI) signature.
    unsafe { core::mem::transmute(syscall_isr as usize) }
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
        _ => u64::MAX,
    };
}

fn sys_write(ptr: u64, len: u64) -> u64 {
    // Safety: syscalls only pass pointers into the (mapped) user address space.
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    match core::str::from_utf8(bytes) {
        Ok(text) => {
            console::_write_str(text);
            crate::serial::_write_str(text);
        }
        Err(_) => {
            for &byte in bytes {
                let buf = [byte];
                let s = unsafe { core::str::from_utf8_unchecked(&buf) };
                console::_write_str(s);
                crate::serial::_write_str(s);
            }
        }
    }
    len
}

fn sys_read_char() -> u64 {
    // Interrupts are disabled while inside the syscall gate; re-enable them so
    // the keyboard IRQ can arrive and wake the blocking read.
    let was_enabled = x86_64::instructions::interrupts::are_enabled();
    x86_64::instructions::interrupts::enable();
    let key = keyboard::read_key();
    if !was_enabled {
        x86_64::instructions::interrupts::disable();
    }
    match key {
        keyboard::Key::Char(c) => c as u64,
        keyboard::Key::Enter => b'\n' as u64,
        keyboard::Key::Space => b' ' as u64,
        keyboard::Key::Backspace => 8,
        keyboard::Key::Tab => b'\t' as u64,
        keyboard::Key::Escape => 27,
        _ => 0,
    }
}

/// Terminate the current user program and return control to the kernel.
fn exit(_code: u32) -> ! {
    crate::serial_println!("user: program exited");
    // Safety: restores the kernel stack saved by `enter_user_asm`, then returns.
    unsafe {
        asm!(
            "mov rsp, [rip + {saved}]",
            "ret",
            saved = sym SAVED_KERNEL_RSP,
            options(noreturn),
        );
    }
}

/// Load a static ELF64 image and run it in ring 3. Returns on load error or
/// after the program exits.
pub fn run(elf_bytes: &[u8]) -> Result<(), &'static str> {
    let elf = ElfFile::new(elf_bytes).map_err(|_| "not a valid ELF")?;
    let entry = elf.header.pt2.entry_point();

    let user_flags =
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;

    // Map each page once (segments can share pages) and remember its frame.
    let mut pages: Vec<(u64, u64)> = Vec::new();
    for ph in elf.program_iter() {
        if ph.get_type() != Ok(ProgramType::Load) {
            continue;
        }
        let vaddr = ph.virtual_addr();
        let mem_size = ph.mem_size();
        let start = vaddr & !0xFFF;
        let end = (vaddr + mem_size + 0xFFF) & !0xFFF;

        let mut va = start;
        while va < end {
            if phys_for(&pages, va).is_none() {
                let phys = mem::alloc_zeroed_frame().ok_or("out of memory")?;
                if !mem::map_page(VirtAddr::new(va), phys, user_flags) {
                    return Err("failed to map segment");
                }
                pages.push((va, phys.as_u64()));
            }
            va += 4096;
        }

        let data = ph.get_data(&elf).map_err(|_| "bad segment data")?;
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
    }

    // Map the user stack.
    let mut va = USER_STACK_TOP - USER_STACK_SIZE;
    while va < USER_STACK_TOP {
        let phys = mem::alloc_zeroed_frame().ok_or("out of memory (stack)")?;
        if !mem::map_page(VirtAddr::new(va), phys, user_flags) {
            return Err("failed to map stack");
        }
        va += 4096;
    }

    let selectors = gdt::selectors();
    crate::serial_println!("user: entering ring 3 at {:#x}", entry);
    // Safety: the trampoline builds an `iretq` frame and drops to ring 3.
    unsafe {
        enter_user_asm(
            entry,
            USER_STACK_TOP - 16,
            selectors.user_code as u64,
            selectors.user_data as u64,
        );
    }
    // `enter_user_asm` never returns; keep the call non-tail so the return
    // address stays on the kernel stack for `exit` to use.
    core::hint::black_box(());
    Ok(())
}

fn phys_for(mappings: &[(u64, u64)], va: u64) -> Option<u64> {
    let page = va & !0xFFF;
    mappings
        .iter()
        .find(|(page_vaddr, _)| *page_vaddr == page)
        .map(|(_, phys)| *phys)
}
