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
//! **NMIs.** An NMI cannot be masked, and the platform can raise one at any
//! CPU at any time (QEMU's `inject-nmi`, a board's NMI button; LINT1 is
//! masked by the INIT until `ap_entry` programs it). With INIT's `IDTR` (base
//! 0) it would triple-fault the machine. So the blob's **first instruction**
//! loads `IDTR` with the early NMI table at `0x0E60`, laid out so one table
//! serves all three modes: the real-mode IVT's vector 2 at `+8`, the 32-bit
//! IDT's at `+16` and the 64-bit IDT's at `+32` do not overlap. Each points at
//! a stub that counts the NMI (`0x0E9C`) and returns. The 32-bit stub also
//! covers the three instructions between `CR0.PE` and the far return, when
//! `CS` still holds the real-mode `0x0800`, which `iretd` could not reload:
//! it resumes at `smp_prot` instead (everything those instructions did is
//! redone there). The selectors are chosen so the 64-bit gate stays valid
//! once `ap_entry` loads the kernel's GDT (0x08 code, 0x10 data in both
//! tables), until it loads the kernel's IDT before leaving this page's
//! identity mapping. What is left is the instruction before the `lidt`.
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
//! | `0x0E50` | the early `IDTR` (`u16` limit 47, `u32` base `0x8E60`) |
//! | `0x0E60` | the early NMI table (IVT/IDT32/IDT64 vector 2, BSP-patched) |
//! | `0x0E9C` | NMIs the early stubs took (`u32`) |
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
    pub static smp_nmi16: u8;
    pub static smp_nmi32: u8;
    pub static smp_nmi64: u8;
}

/// The temp GDT's page offset (five 8-byte entries, base 0):
/// `0x00` null · `0x08` 64-bit code (`0x0000FFFF` / `0x00AF9B00`) · `0x10`
/// data, base 0, 4 GiB (`0x0000FFFF` / `0x00CF9200`, the stack's SS) ·
/// `0x18` data, base `0x8000`, limit `0xFFFF` bytes (`0x8000FFFF` /
/// `0x00009200`) — so the same fixed offsets keep addressing the page in
/// 32-bit mode · `0x20` 32-bit code (`0x00CF9A00`). Code at 0x08 and the
/// stack's data at 0x10 match the kernel GDT's, so a frame the early NMI
/// stub resumes stays loadable after `ap_entry` switches tables.
pub const GDT_OFF: u32 = 0x0E00;
/// The selectors above.
pub const SEL_CODE64: u16 = 0x08;
pub const SEL_CODE32: u16 = 0x20;
/// The early `IDTR` (limit, then base) and the table it names.
pub const EARLY_IDTR_OFF: u32 = 0x0E50;
pub const EARLY_IDT_OFF: u32 = 0x0E60;
/// The table's limit: it ends with the 64-bit vector 2 (`+32..+48`).
pub const EARLY_IDT_LIMIT: u16 = 47;
/// NMIs the early stubs took (a `u32` the suite reads).
pub const EARLY_NMIS_OFF: u32 = 0x0E9C;
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
    .global smp_nmi16
    .global smp_nmi32
    .global smp_nmi64

smp_trampoline_start:
    .code16
    lidt cs:[0x0E50]                /* first: the early NMI table (above) */
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
    mov ax, 0x0018                  /* ds: base 0x8000, the fixed offsets live */
    mov ds, ax
    mov ax, 0x0010                  /* ss: base 0, the 0x8D00 stack */
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

    /* The early NMI stubs (see the module doc): count, return. */
    .code16
smp_nmi16:
    lock inc dword ptr cs:[0x0E9C]
    .byte 0xCF                      /* a 16-bit iret (spelled out, like the
                                       retf above) */

    .code32
smp_nmi32:
    lock inc dword ptr ss:[0x8E9C]  /* ss: base 0 in every window */
    cmp word ptr [esp + 4], 0x0020
    je 1f
    /* CS is the real-mode 0x0800 (between CR0.PE and the far return): it
       cannot be reloaded, so resume where that return goes. */
    push eax
    movzx eax, word ptr ss:[0x8E42] /* smp_prot's patched offset */
    mov [esp + 4], eax
    mov dword ptr [esp + 8], 0x0020
    pop eax
1:  iretd

    .code64
smp_nmi64:
    lock inc dword ptr [0x8E9C]     /* the identity-mapped page */
    iretq

    /* The temp GDT, the GDTR and the patch slots: data at the fixed offsets
       the code references. The mailbox (0x0F00) needs no source bytes — the
       BSP fills it in the copied page. */
    .org 0x0E00
    .quad 0x0000000000000000        /* 0x00 null */
    .quad 0x00AF9B000000FFFF        /* 0x08 64-bit code */
    .quad 0x00CF92000000FFFF        /* 0x10 data, base 0, 4 GiB */
    .quad 0x000092008000FFFF        /* 0x18 data, base 0x8000, 64 KiB */
    .quad 0x00CF9A000000FFFF        /* 0x20 32-bit code */
    .org 0x0E30
    .word 5 * 8 - 1                 /* GDTR limit */
    .long 0                         /* GDTR base: patched to 0x8000+0x0E00 */
    .org 0x0E40
    .long 0                         /* the retf slots: all BSP-patched */
    .long 0
    .long 0
    .org 0x0E50
    .word 47                        /* the early IDTR: limit ... */
    .long 0x8E60                    /* ... and base (0x8000 + 0x0E60) */
    .org 0x0E60
    .fill 48, 1, 0                  /* the early NMI table: BSP-patched */
    .org 0x0E9C
    .long 0                         /* NMIs the early stubs took */
smp_trampoline_end:

    .org 0x1000                     /* the blob claims its whole page */
    "#
);
