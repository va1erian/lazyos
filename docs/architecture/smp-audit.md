# SMP audit (S8.0)

**What it is.** The inventory every other SMP stage works from
([`../smp-plan.md`](../smp-plan.md), stage S8.0): every place the kernel
assumes it runs on one CPU, with its **disposition** — how that assumption is
supposed to be discharged. Each stage checks its rows off; a row may only
change disposition in the PR that implements it. After S8.0, a new
single-CPU assumption landing without a row here is a review failure.

**Why the kernel is single-CPU-correct today.** One invariant carries
everything: at most one CPU executes kernel code, because syscalls and
handlers run with interrupts off (an interrupt gate for `int 0x80`,
`IA32_FMASK=0x700` for the Linux `syscall`, `without_interrupts` around driver
critical sections) and the scheduler only ever runs on the CPU that took the
tick. The plan's stages discharge this invariant row by row instead of
invalidating it all at once.

**Dispositions**

| Mark | Meaning |
|---|---|
| per-CPU | Becomes a member of the per-CPU area (`arch::percpu`, S8.1) |
| entry lock | Stays global; correct because at most one CPU is in the kernel (`sync::Lock`, S8.3) |
| shootdown | Must go through `mem::tlb::shootdown` before a second CPU runs user code (S8.2) |
| real lock | Becomes a `sync::Lock` (or a sleeplock, where it parks) at the stage named |
| boot-only | Only runs before any AP is online; safe by construction, pinned by a test |
| done | Landed |

## The rows

### Boot, CPU and entry (S8.1) — **done**

**Design note (recorded with the rows below).** The plan first sketched
`swapgs` at every user→kernel boundary, with a naked CS-test wrapper per
entry path and paranoid NMI entry. The build went simpler: **the kernel runs
every privilege level with `%gs`'s base = the CPU's area, and `swapgs` is
never executed** — user code never touches `%gs` (`ARCH_SET_GS` is a stub,
no vDSO, nothing writes `IA32_KERNEL_GS_BASE`), and per-CPU state must be
block-invariant anyway (a task can park inside a syscall and resume
elsewhere, which is exactly what killed the old per-task-GS idea). The
consequence: every entry path (syscall, gate, fault, IRQ, NMI) reads
per-CPU state directly, no wrappers, and a hostile `mov gs:[x]` in ring 3
faults on the kernel-only page without leaking anything. If user `%gs`
support is ever added, revisit with real `swapgs` discipline.

| Site | What | Disposition |
|---|---|---|
| `arch/linux.rs` | `KERNEL_STACK`, `SAVED_USER_RSP`, `USER_CONTEXT` — the Linux `syscall` stub's stack switch globals | **done** (S8.1): `percpu::Cpu::syscall`, read through `%gs`; the stub's writes are `gs:`-relative (`OFF_USER_CONTEXT`/`OFF_SAVED_USER_RSP`/`OFF_SYSCALL_STACK`, pinned by `percpu_entry_asm_offsets_match_the_layout`) |
| `arch/linux.rs:8` | Deliberate `swapgs` avoidance (per-task GS desyncs on block) | **done** (S8.1), superseded by the no-`swapgs` design note above |
| `arch/gdt.rs` | `GDT`, `TSS`, `KERNEL_STACK`, `DOUBLE_FAULT_STACK`; `set_kernel_stack` rewrites the one TSS on every switch (`schedule.rs:226`) | **done** (S8.1): per-CPU in the area (`gdt::init_cpu`); `set_kernel_stack` writes this CPU's TSS |
| `arch/idt.rs` | One shared IDT (stays shared); IST/double-fault stacks | **done** (S8.1): the double-fault IST stack is per-CPU in the area; the IDT stays shared and reads the *current* TSS at fault time |
| `task/mod.rs:187` | `CURRENT` — the one current task | **done** (S8.1): `percpu::Cpu::current` via `task::current()`/`set_current` |
| `arch/irqoff.rs` | `SPAN_START/SPAN_NR/SPAN_FROM/SPAN_TASK` — one open interrupts-off span | **done** (S8.1): `percpu::Cpu::irqoff`; the per-syscall maxima and the per-task `TASK_NR` stay machine-wide |
| `arch/irq_window.rs` | `IRQ_WINDOW_OPEN`, `LAST`, `OPENED`, `WINDOW_TICKS`, `UNCHARGED`; the timer asm reads the flag as a `#[no_mangle]` global | **done** (S8.1): `percpu::Cpu::window`; `timer_isr` reads the flag `gs:`-relative (`OFF_WINDOW_OPEN`); `ARMED` stays machine-wide (one boot) |
| `task/preempt.rs` | `NEED_RESCHED`, `PREEMPTING`, `SELECTED`, `SELECTED_AT`, `SLICE_END`, `DEFERRED`, `HANDOFF_FROM/TO` — global | **done** (S8.1): `percpu::Cpu::preempt` (atomic fields, for the S8.3 wake IPI); `NAPPING` stays per-task |
| `process/gate.rs`, `arch/irq_stubs.rs`, `arch/msi_stubs.rs`, `arch/idt.rs` faults, `arch/event_timer.rs` | Rust `x86-interrupt` handlers with no stub where a CS test could go | **not needed**: the no-`swapgs` design has no CS test to place (design note above) |
| `arch/nmi.rs` | Plain `x86-interrupt` NMI handler | **not needed**: with no `swapgs`, an NMI sees a valid `%gs` at every privilege level; the handler needs no IST for GS reasons (its stack behaviour is unchanged from single-core) |
| `task/fpu.rs:28` | `AREAS` — per-task FPU images, unsynchronized ("interrupts off on the single CPU") | entry lock (S8.3; the switch path runs under the inherited hold) |
| `task/relax.rs` | `Yield` relax parks 200 µs — sound because the holder was preempted on this CPU | the `TASKS.is_locked()` spin guard stays correct for N CPUs (spinning while a remote holder runs is safe); the strategy itself dies with its premise (sleeplocks, S8.6) |
| `arch/fault_storm.rs:10` | "handled with interrupts off on a single CPU, so plain atomics" | entry lock (S8.3), audited |

### Bring-up, clocks, IPIs (S8.2)

| Site | What | Disposition |
|---|---|---|
| `libs/acpi/src/madt.rs` | Only a processor count and `first_apic_id` were kept | **done** (S8.0): `apic_ids` list, logged as `apic_ids=[..]` at boot |
| `vendor/bootloader` | Hands over on the BSP only; physical memory at a dynamic offset, nothing identity-mapped low | trampoline (`global_asm!`, `.code16`) + a dedicated AP page table; trampoline page and mailbox reserved before `mem::init` (S8.2) |
| `arch/lapic.rs` | Timer, EOI, x2APIC; no ICR | `send_init`/`send_sipi`/`send_fixed`/`send_nmi` (S8.2) |
| `arch/irqchip.rs:102-104`, `dev/msi.rs:96-99` | I/O APIC and MSI destinations hardwired to the BSP's APIC ID | entry lock keeps them sound (all IRQs on the entry-lock CPU's APIC); steering per device (S8.6) |
| IPI vectors | none exist | `RESCHED`/`SHOOTDOWN`/`STOP` in 0x60–0x7F, pinned here so MSI (0x40–0x5F) cannot collide (S8.2) |
| `arch/clock.rs` | `LAST_TSC`, `MISSED`, `CYCLES_PER_TICK` — one TSC catch-up stream; `clock.rs:153`'s "this single CPU cannot run it in between" | timekeeper CPU only (S8.2); `window_tick`'s `periods_since_last` runs only on the entry-lock holder, so the stream stays single-writer |
| `mem/wc.rs` | PAT change, SDM single-CPU sequence | boot-only; if ever runtime, behind `STOP_VECTOR` stop-the-world (S8.2) |
| `fbwindow.rs`, `mem/mmio.rs` | Kernel-half PTE changes flushed locally | boot-only + monotonic MMIO window addresses, pinned by a tripwire test — **not** exempt "because a CR3 write flushes all" (a local flush never cleans another CPU's TLB) (S8.2) |

### The entry-lock era (S8.3)

| Site | What | Disposition |
|---|---|---|
| whole kernel | "interrupts off ⇒ no other kernel code runs" | replaced by the entry lock, keyed on the interrupted context's ring: ring-3 entries acquire, ring-0-origin interrupts inherit the hold; released at user return, park **and preemptive switch** (S8.3) |
| `task/preempt.rs:72-76` | No idle task; the parked task `hlt`s | per-CPU idle task, so a CPU with nothing runnable never holds the lock (S8.3) |
| `task/preempt.rs` | `NEED_RESCHED`, `PREEMPTING`, `SELECTED`, `SELECTED_AT`, `SLICE_END`, `DEFERRED`, `HANDOFF_FROM/TO` — global | per-CPU (S8.1); remote wakes send `RESCHED_VECTOR` (S8.3) |
| `arch/event_timer.rs` | `ARMED`, `RATE` — the one-shot deadline timer programs the arming CPU's LAPIC timer | per-CPU **in S8.3** (a deadline armed before a migration would otherwise fire late; the 10 ms sweep is the backstop, never the answer) |
| `task/lifecycle.rs:370` | "Revisit this unwrap if/when SMP" | entry lock (S8.3), audited |
| `kernel/src/quota.rs`, `IDLE_TICKS` | Global accounting | entry lock (S8.3), per-CPU accumulation + global fold |
| `serial.rs:274-286` | Panic path force-unlocks the serial port, "justified by one CPU" | sound under `STOP_VECTOR` stop-the-world before any dump (S8.2) |

### Fine-graining (S8.4–S8.6)

| Site | What | Disposition |
|---|---|---|
| `task/runq.rs` | `RUNNABLE`, `DONE` global masks | entry lock (S8.3) → per-CPU (S8.4) |
| `task/sched.rs` | `VIRTUAL_CLOCK` | global under the scheduler lock (S8.4) |
| `task/schedule.rs:212-217` | `install`'s slot-pinning invariant ("on today's single-CPU scheduler") | real proof: selection under the scheduler lock, teardown takes it (S8.4) |
| `mem/frames.rs` | `FRAMES: Mutex<Option<Frames>>`, one global lock | entry lock (S8.3) → per-CPU magazines + central lock (S8.5) |
| `mem/heap.rs:33-42` | The #382 rule: the heap lock is only taken with interrupts off | `sync::Lock` + per-CPU slab caches (S8.5) |
| `mem/protect.rs:15-17` | "an SMP kernel needs a shootdown here" | shootdown (S8.2); callers derived from `grep -rn "tlb::flush" kernel/src` **plus the flush-by-CR3-write sites** (`switch_to(` — `mem/cow.rs` fork flushes that way): **`mem/cow.rs` (fork — the sharpest: it write-protects the parent in place)**, `protect.rs` (`filemap::write_protect`, `unmap_from` unmapping every address space), `mem/reclaim.rs`, `mem/filemap.rs` + page-cache eviction, `mem/uspace.rs` unmap/remap (`remap_page`), the page-cache generation bumps |
| `mem/` fault paths | COW/demand resolve flushes locally | deliberately exempt: a resolve only tightens or installs an entry, so a remote stale entry is safe (recorded here so a reviewer does not flag it) |
| `fs/openfile/pagecache.rs` | "no store can land while the kernel holds the cache lock" | real critical sections; the epoch checked under the lock (S8.5) |
| `ipc/channels.rs:28-33` | `CHANNELS` lock order note; `WaitQueue` rules at `task/wait.rs:24-27` rely on interrupts-off for register-then-block | entry lock (S8.3) → finer channel/endpoint locks (S8.6); the wait rule inverts (lock-held-across-state-set) and the lock-order table records both eras |
| `task/yieldlock.rs` | `YieldMutex` (the two VFS tables, ext2 volumes) parks inside its critical section | sleeplock (S8.6) |
| `ipc/inet/bell.rs:20` | Single-waiter atomics, "rung from syscall context (one CPU)" | audited: CAS claim or documented single-waiter (S8.6) |
| `dev/peek.rs:44` | "A port to SMP must make the claim table and the BAR decode a real critical section" | real lock (S8.6) |
| `dev/irq.rs:4` | "runs in interrupt context on a single CPU, so it may not take a lock" | the lock-free `RAISED` rule survives as the IPI-handler contract; quiet-context test per-CPU (S8.6) |
| `process/linux/extra.rs:80`, `process/linux/misc.rs` | `getcpu`, `sched_getaffinity` answer one CPU | real values (S8.6) |
| `process/linux/futex.rs:137` | single-CPU note | futex wakes go through the ordinary wake path (IPI-carrying) (S8.6) |
| `task/pollwait.rs:24` | single-CPU note | entry lock → ordinary lock (S8.6) |

### Harness (S8.2/S8.3)

| Site | What | Disposition |
|---|---|---|
| `kernel/src/tests/mod.rs` | The suite diverges before `fs::init`/`arch::init`, runs pre-scheduler with interrupts disabled | pre-scheduler SMP cases stay here; AP bring-up exposed as `smp::bring_up(n)`; system-level cases move to a `qemu_session.py` harness (smp-plan §4.7) |
| `tools/test/run.py`, `tools/screenshot/qemu_qmp.py` | No `-smp` anywhere | **done** (S8.0): `--smp N` on every launcher, `--soak` on `run.py` |

## How to use this file

1. Before a stage's PR: re-verify its rows against the tree (line numbers
   drift; symbols do not) and state the disposition in the PR.
2. When a row is discharged, mark it **done** with the PR number, and run
   `python tools/sync/check_locks.py --ratchet` — a migration lowers counts.
3. When writing new kernel code that would once have relied on "interrupts
   off", add its assumption here instead of a comment that says "single CPU".
   The lock-order table lives in [`locking.md`](locking.md) (written in S8.1);
   the scattered orders it consolidates are quoted there verbatim.
