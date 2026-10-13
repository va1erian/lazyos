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
| `kernel/src/arch/irqchip.rs`, `arch/ioapic.rs` | The legacy lines on the 8259 or, by default, the I/O APIC (issue #616): mask, EOI, spurious check ([interrupts.md](interrupts.md)) |
| `kernel/src/arch/msi_stubs.rs` | IDT stubs for the MSI vectors `0x40..0x60` (`dev::msi`) |
| `kernel/src/arch/timer.rs` | tick source choice: PIT unless it is frozen or its IRQ0 never arrives (or `LAZYOS_TIMER=lapic`), then the local APIC timer; prints `HW:TIMER:<pit\|lapic> <source> <hz>` |
| `kernel/src/arch/event_timer.rs` | deadline timer (P2): with the PIT as the tick, the APIC timer one-shot on vector `0x31` at the next sub-tick task deadline (`task::timerq`); none when the APIC is the tick or `LAZYOS_EVENT_TIMER=0` |
| `kernel/src/arch/timer_cal.rs` | pure decisions: PIT verdict, CPUID 0x15 crystal, APIC count |
| `kernel/src/arch/lapic.rs` | local APIC (x2APIC MSRs or uncached xAPIC MMIO), virtual wire (LINT0 ExtINT, LINT1 NMI), periodic timer |
| `kernel/src/arch/refclock.rs` | ACPI PM timer and HPET as reference clocks; PIT channel-0 reads |
| `kernel/src/arch/acpi_tables.rs` | `libs/acpi` over the physical map: RSDP -> XSDT/RSDT -> FADT, MADT, HPET, DSDT (`HW:ACPI:` line) |
| `kernel/src/arch/cpu.rs` | FPU/SSE enable (clear `CR0.EM`, set `CR4.OSFXSR`), and AVX state when the CPU has it (`CR4.OSXSAVE`, `XCR0`; below) |
| `kernel/src/task/fpu.rs` | per-task register areas and the `XSAVE`/`FXSAVE` calls the scheduler, spawn and signal code use |
| `kernel/src/arch/msr.rs` | `IA32_EFER/STAR/LSTAR/FMASK/FS_BASE` read/write wrappers |
| `kernel/src/arch/linux.rs` | `linux_syscall_entry`, `linux_dispatch` plumbing |
| `kernel/src/process/gate.rs` | Native `int 0x80` stub (`syscall_isr`) and dispatcher |
| `kernel/src/arch/irq_window.rs` | Interrupt windows: bounded interrupts-off stretches inside syscalls |
| `kernel/src/arch/irqoff.rs` | Worst interrupts-off stretch per syscall, `IRQOFF:MAX` reports |
| `kernel/src/arch/clock.rs` | TSC calibration, tick catch-up, `missed_ticks` |

**FPU, SSE and AVX state** (`cpu.rs`, `task/fpu.rs`, issues #373, #822).
`CPUID` reports AVX whatever the OS did, but a VEX instruction raises `#UD`
until `CR4.OSXSAVE` is set and `XCR0` enables the state, so `cpu::init` does
that when the CPU has `XSAVE` and `AVX` and its `XSAVE` layout is the standard
one (`CPUID.(0DH,2)`: `YMM_Hi128` at offset 576, 256 bytes): `XCR0` = x87 |
SSE | AVX (`0x7`), nothing else. AVX-512, MPX, PKRU and the rest stay off
because nothing saves their state, so `XGETBV` shows user code exactly what
the kernel handles. The boot prints `HW:CPU:fpu xsave_mask=<xcr0> area=<bytes>
mxcsr_mask=<mask>`. Without `XSAVE`/`AVX` (QEMU's default `qemu64` model) the
kernel keeps `FXSAVE` and `OSXSAVE` stays clear, as before.

Each task slot owns one static 64-byte-aligned area of `fpu::AREA_SIZE` = 832
bytes (`FXSAVE` image, `XSAVE` header, `YMM_Hi128`; the static array is 1024
slots, about 850 KiB); only the first 512 are used on an `FXSAVE` CPU. The
scheduler `XSAVE`s the outgoing task into its area and `XRSTOR`s the incoming
one's (plain `XSAVE`, with the full `XCR0` mask: not `XSAVEOPT`/`XSAVEC`, so the
header's reserved words stay zero in every area and a signal frame can carry
the image as is). A new program (`reset`, `execve`) starts from the power-on
image whose `XSTATE_BV` marks x87 and SSE in use, so the upper YMM halves are
initial (zero); `fork` and threads `XSAVE` the creator's live registers into
the child's area. Everything runs with interrupts off on the one CPU. Images
that come from user memory (a signal frame at `rt_sigreturn`) go through
`FpuImage::sanitize` before they reach `XRSTOR`: MXCSR is masked with the CPU's
own `MXCSR_MASK` (not the image's), `XSTATE_BV` is cut to `XCR0`, and the
compaction bit and reserved header words are cleared, since any of them would
raise `#GP` in ring 0.

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
| other PIC lines | device-core stubs (`irq_stubs.rs`) | delivered to claimed device lines (issue #240); the same vectors on the I/O APIC |
| `0x40..0x60` | MSI stubs (`msi_stubs.rs`) | one per MSI vector (issue #616); local APIC EOI |
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
  tenth of a tick (1 ms) has passed since the last one. On paths that reach
  poll points often enough, a pending interrupt therefore waits about 1 ms
  plus the work between two of them; a stretch without a poll point can last
  longer, and `irqoff` reports it (below). Poll points sit in
  the ext2 library's per-block loops (`BlockIo::pace`), the block drivers'
  waits, each zeroed frame (`alloc_zeroed_frame`), each page mapped or
  unmapped, page-table walks, user copies (64 KiB
  pieces), present blits and serial output.
- A window is safe wherever a poll point is reached, whatever locks the
  syscall holds, because the handlers it admits take only one lock, the
  i8042 FIFO's, which is held solely inside `ps2::service` with interrupts
  off and never across a poll point: IRQ0 goes to `window_tick` (count the
  periods, EOI, collect i8042 bytes) instead of `schedule` (`timer_isr`
  checks `IRQ_WINDOW_OPEN`), IRQ1/IRQ12 only collect bytes, and the other
  lines only latch (`dev::irq`, lock-free). Code holding the FIFO lock must
  never reach a poll point. Nothing switches tasks, so user
  memory a syscall validated stays valid. While a window is open, decoding,
  deadline expiry, CPU charging (`take_uncharged`) and task selection are
  deferred to the next scheduler entry outside a window: the next ordinary
  tick, or a voluntary yield if the syscall parks first.
- Windows open only while an `irqoff` span of the current task is open,
  with interrupts off: inside a syscall, or inside a kernel section
  (`irqoff::kernel_section`: the kernel task's periodic writeback and disk
  statistics, which run with interrupts off). Never in an interrupt handler,
  a `nap`, or before `irq_window::arm()` in `main`. Where a poll point is due
  but no span is open, it only drains the i8042 (`ps2::service`).
- Every voluntary switch (`task::switch::yield_now`, and the `YieldMutex`
  park or halt) runs outside the span and gives it back only if one was open
  (`irqoff::paused`), so no span outlives its task's syscall; `exit_cpu`
  ends it for good. `task::preempt_point` does nothing while a window is
  open: an interrupt there stopped code that may hold locks.
- The window's tick is acknowledged like `schedule`'s (`arch::timer`: the
  8259 or the local APIC timer, a stale APIC tick ignored).
- `irqoff` keeps the worst stretch per syscall (native and Linux numbers,
  one slot for kernel sections) and logs each new maximum of 2 ms or more as
  `IRQOFF:MAX abi=<native|linux|kernel> nr=<n> us=<n> over=<n>
  missed_ticks=<n> dropped=<n> from=<file:line> to=<file:line>`:
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
page-fault COW/demand-zero/`SIGSEGV`. No SMP. The legacy lines go through the
I/O APIC when the MADT names one (the 8259 and the local APIC's LINT0 are then
masked; `LAZYOS_IRQCHIP=pic` keeps the 8259 in virtual wire), and userspace
drivers take MSI/MSI-X vectors ([interrupts.md](interrupts.md), issue #616).
A PIT tick the local APIC accepted before line 0 was masked is ignored
(`timer::stale_tick`), as for the APIC timer. The CMOS RTC (`arch/rtc.rs`) is
read at boot for the wall clock.
