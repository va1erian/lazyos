# Arch, CPU tables & syscall gates

**What it is.** x86_64 bring-up and the two ring-3 entry gates: the native
`int 0x80` gate and the Linux `syscall`/`sysret` gate.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/arch/mod.rs` | `init()` order: `cpu` -> `gdt` -> `idt::init_hardware` -> `linux` -> mouse |
| `kernel/src/arch/gdt.rs` | GDT, TSS, ring-3 selectors, kernel stack / IST |
| `kernel/src/arch/idt.rs` | IDT, exception handlers, IRQs, page-fault dispatch, `TICKS` |
| `kernel/src/arch/pic.rs` | 8259 remap (IRQ 0-15 -> vectors 32-47), PIT at 100 Hz; line 0 masks whichever source is the tick |
| `kernel/src/arch/timer.rs` | tick source choice: PIT unless it is frozen or its IRQ0 never arrives (or `LAZYOS_TIMER=lapic`), then the local APIC timer; prints `HW:TIMER:<pit\|lapic> <source> <hz>` |
| `kernel/src/arch/timer_cal.rs` | pure decisions: PIT verdict, CPUID 0x15 crystal, APIC count |
| `kernel/src/arch/lapic.rs` | local APIC (x2APIC MSRs or uncached xAPIC MMIO), virtual wire (LINT0 ExtINT, LINT1 NMI), periodic timer |
| `kernel/src/arch/refclock.rs` | ACPI PM timer and HPET as reference clocks; PIT channel-0 reads |
| `kernel/src/arch/acpi_tables.rs` | `libs/acpi` over the physical map: RSDP -> XSDT/RSDT -> FADT, MADT, HPET, DSDT (`HW:ACPI:` line) |
| `kernel/src/arch/cpu.rs` | FPU/SSE enable (clear `CR0.EM`, set `CR4.OSFXSR`) |
| `kernel/src/arch/msr.rs` | `IA32_EFER/STAR/LSTAR/FMASK/FS_BASE` read/write wrappers |
| `kernel/src/arch/linux.rs` | `linux_syscall_entry`, `linux_dispatch` plumbing |
| `kernel/src/process/gate.rs` | Native `int 0x80` stub (`syscall_isr`) and dispatcher |
| `kernel/src/arch/irq_window.rs` | Interrupt windows: bounded interrupts-off stretches inside syscalls |
| `kernel/src/arch/irqoff.rs` | Worst interrupts-off stretch per syscall, `IRQOFF:MAX` reports |
| `kernel/src/arch/clock.rs` | TSC calibration, tick catch-up, `missed_ticks` |

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
| 8 / 13-21 | exception stubs | a ring-3 fault ends only the faulting process (`user_fault`, `arch/fault.rs`); a ring-0 fault logs and `halt()`s |
| #PF | `page_fault_isr` (naked) | pushes 15 GP regs; `page_fault_dispatch` tries COW, demand-zero, then `SIGSEGV` |
| 32 | `timer_isr` (naked, `task/switch.rs`) | preemption; bumps `TICKS` |
| 33 | keyboard IRQ | pushes scancodes, `on_key` routing |
| 44 | mouse IRQ | pushes bytes |
| other PIC lines | device-core stubs (`irq_stubs.rs`) | delivered to claimed device lines (issue #240) |
| `0x81` | `yield_isr` (naked, DPL 0, `task/switch.rs`) | voluntary reschedule from `WaitQueue::wait`: no tick, no EOI (issue #338) |
| `0x30` | `timer_isr` again | the local APIC timer when it is the tick; `timer::end_of_tick` sends the APIC EOI instead of the 8259 one |
| `0x80` | `syscall_isr` (DPL 3) | native syscalls; saves `rdi/rsi/rdx/r8/r9/r10/rax` |
| `0xFF` | APIC spurious | no EOI |

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

**Native entry decisions** (`process/gate.rs`)

- `syscall_dispatch(regs)` sees `rax` = syscall number; `0` exits, 1-5 and 7-32 are
  dispatched (6, the old command-line spawn, is retired) (see [processes.md](processes.md), [ipc-fabric.md](ipc-fabric.md),
  [display.md](display.md)); unknown numbers return `u64::MAX`.
- Every user pointer, native or Linux, is validated against the caller's page
  tables before the kernel touches it (`user_ptr::try_*`, backed by
  `ipc::syscalls::access_range`): a kernel address, an unmapped range or a
  read-only page is `-EFAULT`. Native syscalls report it; the Linux shim's
  infallible call sites degrade safely (a read yields zero, a write is dropped)
  and are being converted to `-EFAULT` one by one.

**Interrupt windows** (`irq_window.rs`, `irqoff.rs`)

- Both gates run their syscall with interrupts off. Long kernel loops call
  `irq_window::poll_point()`, which opens a window (`sti; nop; cli`) once a
  tenth of a tick (1 ms) has passed since the last one, so a pending interrupt
  waits at most about that long whatever the syscall does. Poll points sit in
  the ext2 library's per-block loops (`BlockIo::pace`), the block drivers'
  waits, each zeroed frame (`alloc_zeroed_frame`), each page mapped or
  unmapped, page-table walks, user copies (64 KiB
  pieces), present blits and serial output.
- A window is safe wherever it opens, whatever locks the syscall holds,
  because the handlers it admits take none: IRQ0 goes to `window_tick`
  (count the periods, EOI, collect i8042 bytes) instead of `schedule`
  (`timer_isr` checks `IRQ_WINDOW_OPEN`), IRQ1/IRQ12 only collect bytes, and
  the other lines only latch (`dev::irq`). Nothing switches tasks, so user
  memory a syscall validated stays valid. Decoding, deadline expiry, CPU
  charging (`take_uncharged`) and task selection wait for the next ordinary
  tick.
- Windows open only inside a syscall (an `irqoff` span is open) with
  interrupts off: never in a handler that interrupted user mode or a `nap`,
  never in the kernel task, never before `irq_window::arm()` in `main`.
- `irqoff` keeps the worst stretch per syscall (native and Linux numbers) and
  logs each new maximum of 2 ms or more as `IRQOFF:MAX abi=<native|linux>
  nr=<n> us=<n> over=<n> missed_ticks=<n> from=<file:line> to=<file:line>`:
  the stretch lacking a poll point lies between those two lines. Under a
  hypervisor a stretch also holds any time the host did not run the vCPU;
  for numbers free of host noise, boot under TCG with `-icount
  shift=0,sleep=off` (1 ns per instruction, slower than hardware), e.g.
  `qemu_session.py --accel tcg --extra-arg=-icount
  --extra-arg=shift=0,sleep=off` with the session's timeouts scaled up.
- Tests: `irq_window_suite` (windows shut outside a syscall, ticks through
  windows exactly once, handlers with the task table/console/serial locked,
  per-syscall charging, `nap`, a one-second soak) and
  `bcache_soak_irq_latency_large_writes` (1 MiB writes and fsyncs through a
  cache half that size: no stretch over 2 ms, no missed tick).

**Status.** Working: preemptive demo boot, Linux `syscall` shim, native gate,
page-fault COW/demand-zero/`SIGSEGV`. No SMP. Device interrupts go through
the 8259 only; the local APIC is enabled (virtual wire) only when its timer is
the tick, and the I/O APIC is recorded from the MADT but not used. The CMOS RTC
(`arch/rtc.rs`) is read at boot for the wall clock.
