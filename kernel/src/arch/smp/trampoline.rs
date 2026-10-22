//! The AP trampoline: 16-bit real-mode entry code the BSP copies to physical
//! `0x8000` and a SIPI jumps into (docs/smp-plan.md S8.2).
//!
//! Position independence without a linker: the blob references its data only
//! through **fixed numeric offsets inside its page** (the GDT, the GDTR, the
//! retf patch slots and the mailbox live at fixed offsets; `ds = 0x0800`
//! makes every 16/32-bit `[offset]` resolve to `0x8000 + offset`). The two
//! mode transitions `push` their segment, then their offset, from the patch slots
//! and `retf` — the offsets are **written into the copy by the BSP**
//! (`smp.rs` computes them from the exported labels), so no symbol
//! arithmetic survives in the assembled blob at all.
//!
//! Flow: real mode → 32-bit protected (own GDT: PE on) → PAE + `EFER.LME` +
//! `CR0.PG` with the BSP-built AP page table (identity low 2 MiB + the
//! kernel half) → 64-bit stub → `%gs` base from the mailbox → jump to the
//! kernel-half `ap_entry`. Interrupts stay off throughout; the AP's idle
//! loop turns them on, nothing here does.
//!
//! Page layout (offsets from `0x8000`):
//!
//! | Offset | Content |
//! |---|---|
//! | `0x0000` | the code (real → protected → long mode) |
//! | `..0x0D00` | the real/protected-mode stack, growing down (the code stays below) |
//! | `0x0E00` | the temp GDT, five 8-byte entries |
//! | `0x0E30` | the GDTR (`u16` limit, `u32` base patched by the BSP) |
//! | `0x0E40` | retf patch slots: `u16` CS₁, `u16` offset₁, `u32` CS₂, `u32` offset₂ |
//! | `0x0F00` | the mailbox: magic `u32`, then `CR3`, `ap_entry`, `Cpu` area, stack top |
//!
//! Mailbox fields: magic `0x5A5A_0001` (checked before the blob trusts the
//! rest), the AP page table's physical address (`CR3`), `ap_entry`'s virtual
//! address (kernel half), this AP's `Cpu` area virtual address
//! (`IA32_GS_BASE`), and the top of this AP's kernel stack (`RSP`).

use core::arch::global_asm;

// The blob's own symbols: the copy bounds and the two retf targets, whose
// offsets the BSP computes (`label as usize - start as usize + 0x8000`).
extern "C" {
    pub static smp_trampoline_start: u8;
    pub static smp_prot: u8;
    pub static smp_long_stub: u8;
    pub static smp_trampoline_end: u8;
}

/// The temp GDT's page offset (five 8-byte entries, base 0):
/// `0x00` null · `0x08` 32-bit code (`0x00CF9A00`) · `0x10` data, base
/// `0x8000`, limit `0xFFFF` bytes (`0x8000FFFF` / `0x00009200`) — so the
/// same fixed offsets keep addressing the page in 32-bit mode · `0x18`
/// data, base 0, 4 GiB (`0x0000FFFF` / `0x00CF9200`, the stack's SS) ·
/// `0x20` 64-bit code (`0x0000FFFF` / `0x00AF9B00`).
pub const GDT_OFF: u32 = 0x0E00;
/// The GDTR's page offset: a `u16` limit, then the `u32` base the BSP
/// patches to `0x8000 + GDT_OFF`.
pub const GDTR_OFF: u32 = 0x0E30;
/// The GDTR's base field (the limit sits at [`GDTR_OFF`]).
pub const GDTR_BASE_OFF: u32 = GDTR_OFF + 2;
/// The retf patch slots: `CS₁` (`u16`), `offset₁` (`u16`), `CS₂` (`u32`),
/// `offset₂` (`u32`).
pub const RETF1_CS_OFF: u32 = 0x0E40;
pub const RETF1_OFF_OFF: u32 = 0x0E42;
pub const RETF2_CS_OFF: u32 = 0x0E44;
pub const RETF2_OFF_OFF: u32 = 0x0E48;
/// The mailbox's fields (see the page layout above).
pub const MB_MAGIC_OFF: u32 = 0x0F00;
pub const MB_CR3_OFF: u32 = 0x0F08;
pub const MB_ENTRY_OFF: u32 = 0x0F10;
pub const MB_CPU_OFF: u32 = 0x0F18;
pub const MB_STACK_OFF: u32 = 0x0F20;
/// The mailbox magic the AP checks before trusting the rest.
pub const MB_MAGIC: u32 = 0x5A5A_0001;

global_asm!(
    r#"
    .section .text.smp_trampoline, "ax"
    .global smp_trampoline_start
    .global smp_prot
    .global smp_long_stub
    .global smp_trampoline_end

smp_trampoline_start:
    .code16
    cli
    xor ax, ax
    mov ds, ax                      /* IVT-safe for the first writes */
    mov es, ax
    mov ss, ax
    mov sp, 0x8D00                  /* a stack inside this page (below the GDT) */
    mov ax, 0x0800                  /* ds: every [offset] = 0x8000 + offset */
    mov ds, ax

    cmp dword ptr [0x0F00], 0x5A5A0001   /* the copy is whole and patched */
    jne 9f

    lgdt [0x0E30]

    /* CR0 set outright, not ORed: INIT leaves CD and NW set (caches off)
       and NE clear. PE | MP | ET | NE: protected mode, caches on. */
    mov eax, 0x00000033
    mov cr0, eax
    push word ptr [0x0E40]          /* the BSP-patched CS first ... */
    push word ptr [0x0E42]          /* ... then the offset, which retf pops first */
    .byte 0xCB                      /* a 16-bit retf: LLVM assembles `retf`
                                       here as 66 CB (32-bit), which pops two
                                       dwords and loads a junk CS */

    .code32
smp_prot:
    mov ax, 0x0010                  /* ds: base 0x8000, the fixed offsets live */
    mov ds, ax
    mov ax, 0x0018                  /* ss: base 0, the 0x8D00 stack */
    mov ss, ax
    mov esp, 0x00008D00

    mov eax, cr4
    or eax, 1 << 5                  /* CR4.PAE */
    mov cr4, eax
    mov ecx, 0xC0000080             /* IA32_EFER */
    rdmsr
    or eax, 1 << 8                  /* EFER.LME: long mode enable */
    or eax, 1 << 11                 /* EFER.NXE: the kernel half's NX bits
                                       are reserved without it, so the
                                       first stack touch would #PF */
    wrmsr
    mov eax, [0x0F08]               /* the AP page table (below 4 GiB) */
    mov cr3, eax
    mov eax, cr0
    or eax, 0x80010000              /* CR0.PG | CR0.WP: IA-32e mode on */
    mov cr0, eax
    push dword ptr [0x0E44]         /* CS first: retf pops EIP, then CS */
    push dword ptr [0x0E48]         /* ... so the offset goes on top */
    retf

    .code64
smp_long_stub:
    /* Long mode, still at the low identity-mapped page. Segment bases other
       than FS/GS are ignored, so [disp32] is the absolute linear address:
       the mailbox is at 0x8000 + 0x0F00 here, not at the ds-relative
       0x0F00 the 16/32-bit code used. */
    mov rsp, [0x8F20]               /* this AP's kernel stack top */
    mov ecx, 0xC0000101             /* IA32_GS_BASE */
    mov eax, [0x8F18]               /* the Cpu area's virtual address */
    mov edx, [0x8F1C]
    wrmsr
    mov rax, [0x8F10]               /* ap_entry (kernel half, shared) */
    and rsp, -16                    /* the SysV entry alignment: `call`
                                       leaves RSP = 8 mod 16, which the
                                       compiler's aligned slots (fxsave)
                                       rely on; ap_entry never returns */
    call rax

9:  hlt
    jmp 9b

    /* The temp GDT, the GDTR and the patch slots: data at the fixed offsets
       the code references. The mailbox (0x0F00) needs no source bytes — the
       BSP fills it in the copied page. */
    .org 0x0E00
    .quad 0x0000000000000000        /* 0x00 null */
    .quad 0x00CF9A000000FFFF        /* 0x08 32-bit code */
    .quad 0x000092008000FFFF        /* 0x10 data, base 0x8000, 64 KiB */
    .quad 0x00CF92000000FFFF        /* 0x18 data, base 0, 4 GiB */
    .quad 0x00AF9B000000FFFF        /* 0x20 64-bit code */
    .org 0x0E30
    .word 5 * 8 - 1                 /* GDTR limit */
    .long 0                         /* GDTR base: patched to 0x8000+0x0E00 */
    .org 0x0E40
    .long 0                         /* the retf slots: all BSP-patched */
    .long 0
smp_trampoline_end:
    .org 0x1000                     /* the blob claims its whole page */
    "#
);
