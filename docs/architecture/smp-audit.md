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

**Design note (recorded with the rows below).** Per-CPU state lives in
`arch/percpu.rs`'s areas, addressed through `%gs`, with the **`swapgs`
discipline** of `arch/gsgate.rs`: in ring 0 `IA32_GS_BASE` is the CPU's
area, in ring 3 `IA32_KERNEL_GS_BASE` holds it, and one `swapgs` crosses
each boundary. The first S8.1 build ran every privilege level on one GS base
and never swapped ("user code never touches `%gs`"); that was wrong in a way
that mattered: ring 3 can load any DPL-3 selector into `%gs`, zeroing its
base, and the next `syscall` then saved the user context through `gs:` into
low *user* memory and loaded its stack pointer from there (found in the
2026-10-21 review, regression test `gs_suite`). Two rules keep the
discipline sound: **`CR4.FSGSBASE` is never enabled** (ring 3's
`wrgsbase`/`rdgsbase` would forge or read the base — every user GS base is
then a descriptor's, 0, which is what lets the paranoid gates recognize the
kernel's by its sign bit), and **every new ring-3-reachable vector goes
through an `irq_gate!` or `paranoid_gate!`** (or splices the same tests into
its naked stub), never an `x86-interrupt` handler.

| Site | What | Disposition |
|---|---|---|
| `arch/linux.rs` | `KERNEL_STACK`, `SAVED_USER_RSP`, `USER_CONTEXT` — the Linux `syscall` stub's stack switch globals | **done** (S8.1): `percpu::Cpu::syscall`, read through `%gs`; the stub's writes are `gs:`-relative (`OFF_USER_CONTEXT`/`OFF_SAVED_USER_RSP`/`OFF_SYSCALL_STACK`, pinned by `percpu_entry_asm_offsets_match_the_layout`) |
| `arch/linux.rs:8` | Deliberate `swapgs` avoidance (per-task GS desyncs on block) | **done** (S8.1): GS state is per-CPU, which survives a block; the stub swaps first and before `sysretq`/`iretq` (design note above) |
| `arch/gdt.rs` | `GDT`, `TSS`, `KERNEL_STACK`, `DOUBLE_FAULT_STACK`; `set_kernel_stack` rewrites the one TSS on every switch (`schedule.rs:226`) | **done** (S8.1): per-CPU in the area (`gdt::init_cpu`); `set_kernel_stack` writes this CPU's TSS |
| `arch/idt.rs` | One shared IDT (stays shared); IST/double-fault stacks | **done** (S8.1): the double-fault IST stack is per-CPU in the area; the IDT stays shared and reads the *current* TSS at fault time |
| `task/mod.rs:187` | `CURRENT` — the one current task | **done** (S8.1): `percpu::Cpu::current` via `task::current()`/`set_current` |
| `arch/irqoff.rs` | `SPAN_START/SPAN_NR/SPAN_FROM/SPAN_TASK` — one open interrupts-off span | **done** (S8.1): `percpu::Cpu::irqoff`; the per-syscall maxima and the per-task `TASK_NR` stay machine-wide |
| `arch/irq_window.rs` | `IRQ_WINDOW_OPEN`, `LAST`, `OPENED`, `WINDOW_TICKS`, `UNCHARGED`; the timer asm reads the flag as a `#[no_mangle]` global | **done** (S8.1): `percpu::Cpu::window`; `timer_isr` reads the flag `gs:`-relative (`OFF_WINDOW_OPEN`); `ARMED` stays machine-wide (one boot) |
| `task/preempt.rs` | `NEED_RESCHED`, `PREEMPTING`, `SELECTED`, `SELECTED_AT`, `SLICE_END`, `DEFERRED`, `HANDOFF_FROM/TO` — global | **done** (S8.1): `percpu::Cpu::preempt` (atomic fields, for the S8.3 wake IPI); `NAPPING` stays per-task |
| `process/gate.rs`, `arch/irq_stubs.rs`, `arch/msi_stubs.rs`, `arch/idt.rs` faults, `arch/event_timer.rs` | Rust `x86-interrupt` handlers with no stub where a CS test could go | **done** (S8 review): `extern "C"` handlers behind `arch/gsgate.rs`'s generated gates — IRQ/MSI/IPI/event-timer/keyboard/mouse/spurious through `irq_gate!` (CS test both ends), the faults through `paranoid_gate!`; the naked stubs (`int 0x80`, timer/yield, `#DE`/`#UD`/`#GP`/`#PF`) splice the same tests |
| `arch/nmi.rs` | Plain `x86-interrupt` NMI handler | **done** (S8 review): paranoid gate — reads `IA32_GS_BASE`, swaps iff it is not a kernel-half address, swaps back iff it swapped (an NMI may land inside a swap window); no IST needed for GS reasons |
| `task/fpu.rs:28` | `AREAS` — per-task FPU images, unsynchronized ("interrupts off on the single CPU") | entry lock (S8.3; the switch path runs under the inherited hold) |
| `task/relax.rs` | `Yield` relax parks 200 µs — sound because the holder was preempted on this CPU | the `TASKS.is_locked()` spin guard stays correct for N CPUs (spinning while a remote holder runs is safe); the strategy itself dies with its premise (sleeplocks, S8.6) |
| `arch/fault_storm.rs:10` | "handled with interrupts off on a single CPU, so plain atomics" | entry lock (S8.3), audited |

### Bring-up, clocks, IPIs (S8.2)

| Site | What | Disposition |
|---|---|---|
| `libs/acpi/src/madt.rs` | Only a processor count and `first_apic_id` were kept | **done** (S8.0): `apic_ids` list, logged as `apic_ids=[..]` at boot |
| `vendor/bootloader` | Hands over on the BSP only; physical memory at a dynamic offset, nothing identity-mapped low | trampoline (`global_asm!`, `.code16`) + a dedicated AP page table; the trampoline page (mailbox and stack inside) is below the allocator's 1 MiB floor, and the bring-up refuses SMP unless the memory map calls it usable RAM (S8.2) |
| `arch/lapic.rs` | Timer, EOI, x2APIC; no ICR | **done** (S8.2): `send_init`/`send_sipi`/`send_fixed` (no `send_nmi` yet); the xAPIC two-write ICR send needs a sender lock before S8.3 ([`smp.md`](smp.md), known gaps) |
| `arch/irqchip.rs:102-104`, `dev/msi.rs:96-99` | I/O APIC and MSI destinations hardwired to the BSP's APIC ID | entry lock keeps them sound (all IRQs on the entry-lock CPU's APIC); steering per device (S8.6) |
| IPI vectors | none exist | `RESCHED`/`SHOOTDOWN`/`STOP` in 0x60–0x7F, pinned here so MSI (0x40–0x5F) cannot collide (S8.2) |
| `arch/clock.rs` | `LAST_TSC`, `MISSED`, `CYCLES_PER_TICK` — one TSC catch-up stream; `clock.rs:153`'s "this single CPU cannot run it in between" | timekeeper CPU only (no AP runs the tick yet; `window_tick`'s `periods_since_last` runs only on the entry-lock holder, so the stream stays single-writer); the AP clock-coherence gate (`tscsync`, 50 µs) is **done** (S8.2) |
| `mem/wc.rs` | PAT change, SDM single-CPU sequence | boot-only; every AP installs the same WC entry at `ap_entry` (`pat_this_cpu`) before it is online, and the leaf flush is `tlb::shootdown_all`; a runtime PAT change would need `STOP_VECTOR` stop-the-world (S8.2, **done**) |
| `fbwindow.rs`, `mem/mmio.rs` | Kernel-half PTE changes flushed locally | **done** (S8.2): they flush through `mem::tlb::shootdown` (a local flush never cleans another CPU's TLB, CR3 write or not), and the `check_tlb.py` ratchet is the tripwire for a new bare flush; the runtime `fbwindow::map` re-point (display mode switch) is covered by it |

### The entry-lock era (S8.3)

| Site | What | Disposition |
|---|---|---|
| whole kernel | "interrupts off ⇒ no other kernel code runs" | replaced by the entry lock, keyed on the interrupted context's ring: ring-3 entries acquire, ring-0-origin interrupts inherit the hold; released at user return, park **and preemptive switch** (S8.3) |
| `task/preempt.rs:72-76` | No idle task; the parked task `hlt`s | per-CPU idle task, so a CPU with nothing runnable never holds the lock (S8.3) |
| `task/preempt.rs` | `NEED_RESCHED`, `PREEMPTING`, `SELECTED`, `SELECTED_AT`, `SLICE_END`, `DEFERRED`, `HANDOFF_FROM/TO` — global | per-CPU (S8.1); remote wakes send `RESCHED_VECTOR` (S8.3) |
| `arch/event_timer.rs` | `ARMED`, `RATE` — the one-shot deadline timer programs the arming CPU's LAPIC timer | per-CPU **in S8.3** (a deadline armed before a migration would otherwise fire late; the 10 ms sweep is the backstop, never the answer) |
| `task/lifecycle.rs:370` | "Revisit this unwrap if/when SMP" | entry lock (S8.3), audited |
| `ipc/channels/reclaim.rs` | The reclaimer reads its roots (handle tables, names) before it takes `CHANNELS`: sound only because no syscall interleaves with it | entry lock (S8.3); with it gone, take the roots under `CHANNELS` or re-check each end before closing it |
| `kernel/src/quota.rs`, `IDLE_TICKS` | Global accounting | entry lock (S8.3), per-CPU accumulation + global fold |
| `serial.rs:274-286` | Panic path force-unlocks the serial port, "justified by one CPU" | **done** (S8.2): `panic` calls `smp::stop_others()` before any dump |

### Fine-graining (S8.4–S8.6)

| Site | What | Disposition |
|---|---|---|
| `task/runq.rs` | `RUNNABLE`, `DONE` global masks | entry lock (S8.3) → per-CPU (S8.4) |
| `task/sched.rs` | `VIRTUAL_CLOCK` | global under the scheduler lock (S8.4) |
| `task/schedule.rs:212-217` | `install`'s slot-pinning invariant ("on today's single-CPU scheduler") | real proof: selection under the scheduler lock, teardown takes it (S8.4) |
| `mem/frames.rs` | `FRAMES: Mutex<Option<Frames>>`, one global lock | entry lock (S8.3) → per-CPU magazines + central lock (S8.5) |
| `mem/heap.rs:33-42` | The #382 rule: the heap lock is only taken with interrupts off | `sync::Lock` + per-CPU slab caches (S8.5) |
| `mem/protect.rs:15-17` | "an SMP kernel needs a shootdown here" | shootdown (S8.2); callers derived from `grep -rn "tlb::flush" kernel/src` **plus the flush-by-CR3-write sites** (`switch_to(` — `mem/cow.rs` fork flushes that way): **`mem/cow.rs` (fork — the sharpest: it write-protects the parent in place)**, `protect.rs` (`filemap::write_protect`, `unmap_from` unmapping every address space), `mem/reclaim.rs`, `mem/filemap.rs` + page-cache eviction, `mem/uspace.rs` unmap/remap (`remap_page`), the page-cache generation bumps | **done** (S8.2): every caller above goes through `mem::tlb::{shootdown, shootdown_all, Batch}` (also `mmio`, `fbwindow`, `wc`); `python tools/sync/check_tlb.py` (CI) fails on a new bare `invlpg`; as built in [`smp.md`](smp.md) |
| `mem/` fault paths | COW/demand resolve flushes locally | exempt **only where the entry loosens or is installed** (demand-zero, COW made writable in place, shared page made writable): a remote stale read-only entry faults and is retried (`arch::spurious_fault`). The COW **copy** is not exempt — the space drops its reference to the old frame while a sibling thread's stale entry keeps reading it — and shoots down before it frees (**done**, S8.2; `smp_tlb_cow_copy_reaches_remote_tlbs`) |
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
