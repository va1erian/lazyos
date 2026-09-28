# Arch, CPU tables & syscall gates

**What it is.** x86_64 bring-up and the two ring-3 entry gates: the native
`int 0x80` gate and the Linux `syscall`/`sysret` gate.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/arch/mod.rs` | `init()` order: `cpu` -> `gdt` -> `idt::init_hardware` -> `linux` -> mouse |
| `kernel/src/arch/gdt.rs` | GDT, TSS, ring-3 selectors, kernel stack / IST |
| `kernel/src/arch/idt.rs` | IDT, exception handlers, IRQs, page-fault dispatch, `TICKS` |
| `kernel/src/arch/pic.rs` | 8259 remap (IRQ 0-15 -> vectors 32-47), PIT at 100 Hz |
| `kernel/src/arch/cpu.rs` | FPU/SSE enable (clear `CR0.EM`, set `CR4.OSFXSR`) |
| `kernel/src/arch/msr.rs` | `IA32_EFER/STAR/LSTAR/FMASK/FS_BASE` read/write wrappers |
| `kernel/src/arch/linux.rs` | `linux_syscall_entry`, `linux_dispatch` plumbing |
| `kernel/src/process/mod.rs` | Native `int 0x80` stub (`syscall_isr`) and dispatcher |

**GDT layout** (`gdt.rs:39`) - order matters for `sysret`, which loads
`CS = base+16`, `SS = base+8`:

| Selector | Descriptor |
|---|---|
| `0x08` | kernel code |
| `0x10` | kernel data |
| `0x18` | user data |
| `0x20` | user code |
| next | TSS (kernel stack for ring transitions) |

- TSS `privilege_stack_table[0]` is updated per task by
  `gdt::set_kernel_stack` from the scheduler; IST index 0 is the double-fault
  stack (`DOUBLE_FAULT_IST`).

**IDT** (`idt.rs:16`) - vectors in use:

| Vector | Handler | Notes |
|---|---|---|
| 8 / 13-21 | exception stubs | log and `halt()` |
| #PF | `page_fault_isr` (naked) | pushes 15 GP regs; `page_fault_dispatch` tries COW, demand-zero, then `SIGSEGV` |
| 32 | `timer_isr` (naked, `task/switch.rs`) | preemption; bumps `TICKS` |
| 33 | keyboard IRQ | pushes scancodes, `on_key` routing |
| 44 | mouse IRQ | pushes bytes |
| `0x80` | `syscall_isr` (DPL 3) | native syscalls; saves `rdi/rsi/rdx/r8/r9/r10/rax` |

**MSRs** (`arch/linux.rs:175`): `STAR` encodes `CS=0x08/SS=0x10` on entry and
`CS=0x20/SS=0x18` on return; `LSTAR` = `linux_syscall_entry`; `FMASK` clears
IF/TF/DF; `EFER.SCE` enables the instructions. `IA32_FS_BASE` carries the
per-task thread pointer, restored on every context switch.

**Linux entry decisions** (module docs `arch/linux.rs`)

- `syscall` does not switch stacks, so the stub moves to the task's kernel stack
  (`KERNEL_STACK`, updated alongside TSS `RSP0`) and pushes user `rsp/rflags/rip`
  plus all callee-saved user registers.
- No `swapgs`: tasks may block inside a syscall (futex) and be switched out, so
  GS-based per-task state would desynchronize.
- `USER_CONTEXT` snapshots entry registers for `clone`; `set_user_return` and
  `set_saved_register` let `execve` and signal delivery rewrite the return path.
- `saved_user_rsp` reads the return stack, not the global snapshot, so a blocked
  task cannot observe another task's state.

**Native entry decisions** (`process/mod.rs:117`)

- `syscall_dispatch(regs)` sees `rax` = syscall number; `0` exits, `1-12` are
  dispatched (see [processes.md](processes.md), [ipc-fabric.md](ipc-fabric.md),
  [display.md](display.md)); unknown numbers return `u64::MAX`.
- The native ABI trusts user pointers (the gate runs on the caller's page
  table); the Messenger surface is the exception and validates ranges.

**Status.** Working: preemptive demo boot, Linux `syscall` shim, native gate,
page-fault COW/demand-zero/`SIGSEGV`. No SMP, no APIC (PIC only), no RTC.
