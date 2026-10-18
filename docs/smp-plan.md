# LazyOS SMP plan (platform stage S8)

Multicore support for the kernel, staged so that **every intermediate state is
provably as correct as today's single-core kernel**. The organizing principle:
the kernel's current correctness rests on one implicit invariant — *only one
CPU is ever executing kernel code, because syscalls and handlers run with
interrupts off* — so we first make that invariant explicit and machine-checked,
then bring up the other CPUs **inside** it, and only afterwards relax it
subsystem by subsystem, each relaxation behind its own soak.

Status: plan only, nothing implemented. Prerequisites from
`docs/performance-plan.md` (LAPIC/I-O-APIC/MSI, per-class run queues) have
landed.

## 1. Goals and non-goals

Goals:

- G1. Boot N CPUs (from the MADT), all running the same kernel.
- G2. User tasks run in parallel on N CPUs from the first enabled milestone,
  with the kernel serialized — correct by construction, no data-race audit of
  the whole kernel required up front.
- G3. Incrementally remove the serialization (scheduler, memory, IPC, IRQ
  routing, VFS) only where a measured win justifies the risk, each step with
  lock-order proofs, stress soaks and a perf number.
- G4. A permanent race-finding safety net (lockdep, raw-lock ratchet, chaos
  preemption, deterministic multicore TCG runs, host TSAN for shared no_std
  libs) that outlives the port and catches regressions in future code.
- G5. One-switch disable: an image built without `LAZYOS_SMP=1` is
  byte-for-byte today's kernel; `smp.cpus=1` in `lazyos.cfg` boots one CPU on
  an SMP image.

Non-goals (this plan):

- NUMA, CPU hotplug, CPU parking for power, real-time multicore guarantees.
- Lock-free data structures; fine-grained locking is G3, done incrementally.
- Userspace threading changes (`clone`/futex semantics stay as they are; the
  Linux ABI's single-CPU answers in `process/linux/extra.rs` get real values at
  S8.6, nothing more).

## 2. Why staged, not "audit everything, then flip a switch"

The single-CPU assumption is load-bearing in ~20 places the code itself admits
(`arch/linux.rs` "single CPU" x9, `task/schedule.rs:214`, `mem/protect.rs:16`,
`task/fpu.rs:28`, `dev/irq.rs:4`, `ipc/inet/bell.rs:20`, `serial.rs:275`,
`arch/fault_storm.rs:10`, ...) and in the structural convention that the whole
kernel treats interrupts-off as mutual exclusion (`docs/architecture/tasks.md`
§"Syscalls run with interrupts off"; issue #382's heap rule; `WaitQueue::wait`'s
register-then-block atomicity; `YieldMutex`'s relax strategy, which parks a
contender for 200 µs *because on one CPU the holder must have been preempted
here*). A one-shot fine-grained rewrite would invalidate all of those
arguments simultaneously — exactly how "weird parallelism bugs that occur
later" are born. The entry-lock model (S8.3) keeps every existing argument
valid verbatim: with at most one CPU in the kernel, "interrupts off" and "no
other CPU is in here" mean the same thing, and the only new concurrency is
between user-mode tasks — which the kernel already treats as untrusted and
isolates via address spaces.

The one invariant the entry lock does **not** preserve is TLB coherence: a CPU
running user code holds translations the kernel (on the entry-lock CPU) may
invalidate for a *different* address space (reclaim, filemap eviction,
write-protect for fork, shared-buffer teardown). So TLB shootdown IPIs land in
S8.2, **before a second CPU is ever allowed to run user code** — not as a
later refinement. Same reasoning for the two other cross-CPU hardware facts:
per-CPU syscall-entry state (S8.1) and TSC sanity (S8.2).

## 3. Design overview

```
S8.0 audit + tooling        no kernel behaviour change; -smp N plumbing
S8.1 per-CPU foundations    %gs per-CPU areas; still 1 CPU, code converted
S8.2 AP bring-up + IPIs     trampoline, INIT-SIPI, LAPIC ICR, shootdown IPIs;
                            APs online but idle; TSC gate
S8.3 kernel entry lock      N CPUs run user tasks in parallel; kernel
                            serialized; ENABLED MILESTONE (LAZYOS_SMP=1)
S8.4 scheduler fine-grain   per-CPU runqs, entry lock off the switch path
S8.5 memory fine-grain      per-CPU frame/heap caches, real critical sections
S8.6 IPC + IRQ + VFS        finer channel/registry locks, IRQ steering,
                            sleeplocks for FS; Linux ABI reports real CPUs
S8.7 perf + docs            accept "speedup on N vCPUs", as-built docs
```

Each stage is independently mergeable, each keeps the suite green at `-smp 1`,
and S8.4+ keep it green at `-smp 2/4` with the stage's own stress cases.

## 4. Cross-cutting safety net (build first, in S8.0/S8.1)

These are the tools that make "no weird bugs later" a testable claim rather
than a hope. They exist before any second CPU runs.

1. **`kernel/src/sync/` — the kernel lock type.** A spinlock wrapper
   (`sync::Lock<T>`) that all SMP-era critical sections use, with:
   - *Lockdep*: a per-CPU held-lock stack and a global order graph; acquiring
     in an order that completes a cycle panics immediately with both
     acquisition traces (`LOCKDEP:CYCLE a->b b->a at file:line`). Active in
     test images (`LAZYOS_TESTS=1`) and in `LAZYOS_SMP_DEBUG=1` builds;
     compiled out of release otherwise.
   - *Owner tracking*: holder CPU + task slot + `file:line` of acquisition,
     extending the existing `YieldMutex` `LockInfo`/`LOCK:WAIT` diagnostics
     (`task/yieldlock.rs`).
   - *Spin watchdog*: a spin longer than N ms panics with the owner's CPU,
     task and acquisition site — the SMP analogue of today's hang, and the
     replacement for the "holder was preempted on this CPU" reasoning the
     relax strategy loses.
   - *Assertions*: `debug_assert` that a `sync::Lock` is never held across
     `WaitQueue::wait`/park (today's `task/wait.rs:24-27` rule, machine-checked
     via a per-CPU `PARKING_WITH_LOCKS` counter).
2. **Documented global lock order** — a new `docs/architecture/locking.md`
   consolidating the scattered notes (`ipc/channels.rs:28-33`'s
   CHANNELS→REGISTRY, wait-queue-before-task-table, slab→list, heap→frames)
   into one ranked table. Lockdep is configured from the same table, so the
   doc and the panic agree.
3. **Raw-lock ratchet** — `tools/sync/check_locks.py`, modelled on
   `tools/trace/check_adhoc.py`: a baseline JSON of every file using
   `spin::Mutex` directly; new uses fail CI, migrated files must lower their
   entry. SMP-touched subsystems migrate to `sync::Lock` as they are converted;
   untouched single-CPU-only code (e.g. boot-time tables) may stay on the
   baseline forever.
4. **Deterministic multicore runs** — `-smp N` support in
   `tools/screenshot/qemu_qmp.py::build_qemu_command` (`--smp N` on every
   launcher: `run_demo.py --smp`, `qemu_session.py`/`qemu_shot.py`, GUI
   Simple+Advanced per the app-wiring convention) **and in
   `tools/test/run.py`**, which builds its own QEMU command (new `--smp N`
   and `--soak` flags). The SMP test suite runs under TCG with
   `-icount shift=0,sleep=off` so an interleaving that fails once fails
   reproducibly; KVM runs are the performance verdict, TCG the correctness one
   (mirrors the existing guidance in AGENTS.md "Interrupt latency"). Note:
   QEMU TCG **refuses `invtsc`** (`-cpu max,invtsc=on` fails with "TCG
   doesn't support requested feature"), which is why the S8.2 clock gate
   splits coherence from invariance.
5. **Chaos preemption** (`cfg(lazyos_tests)` only) — a deterministic-PRNG
   hook at every `irq_window::poll_point`, every lock acquire and every
   `preempt_point` that, on a seed-driven schedule, forces a yield or opens an
   IRQ window. Seed printed on failure (`TEST:<name>:FAIL:<detail> seed=0x..`)
   and replayable via the existing `FUZZ_SEED` convention. This shakes out
   latent order assumptions without real heisenbugs: the same seed gives the
   same interleaving under TCG.
6. **Host TSAN for shared libraries** — `ext2fs`, `wifimac`, `smbwire`,
   `framering` etc. are already host-tested; add a CI job running their test
   suites under `-Zsanitizer=thread` (nightly, host target) once the kernel
   can call them from two CPUs. The in-kernel code itself has no sanitizer
   story on `x86_64-unknown-none`; items 1-5 are its substitute.
7. **The SMP suite, split by harness** (per the testing requirement:
   correctness *and* stress). The `LAZYOS_TESTS=1` harness (`tests::run()`)
   diverges before `fs::init`/`arch::init` and runs pre-scheduler with
   interrupts disabled, so it can only host cases that need neither: per-CPU
   area layout, entry-stub CS tests (via `dispatch_for_test`), lockdep
   ordering, allocator churn from kthreads (`set_test_park`), and AP bring-up
   itself through a new **`smp::bring_up(n)` hook** the suite drives (the
   same hook the refused-SIPI test uses — a test image otherwise never
   reaches `arch::init`). The system-level cases need a booted, scheduled,
   multi-task system and so run as a **session harness** (the repo's usual
   pattern): a test program spawned at login, driven by `qemu_session.py`,
   judged by markers — parallel fork+exit generations, N-way IPC ping-pong
   with checksummed payloads, parallel ext2 writers (ext2 checker from the
   host), shootdown storm, entry-lock convoy (N tasks hammering syscalls with
   a deliberate parker inside). Every case must pass at `-smp 1` too
   (trivially serialized), so the suite is also the regression net for the
   conversion work itself.

## 5. Stages

### S8.0 — Audit and tooling (no behaviour change)

- Write `docs/architecture/smp-audit.md`: the table of every global static and
  every "single CPU" comment with its disposition — *per-CPU*, *entry-lock
  protected*, *needs real lock*, or *boot-time only (safe)*. Seed it from the
  survey list in §2 and the per-CPU inventory in §6; each later stage checks
  rows off. A new single-CPU assumption after this point is a review failure.
- `libs/acpi/src/madt.rs`: keep the **full list** of local/x2APIC entries
  (id, enabled, online-capable) instead of only `processors: u32` and
  `first_apic_id`. `arch/acpi_tables.rs` logs the list; the boot line grows
  `cpus=<n> apic_ids=...`.
- `--smp N` plumbing (§4.4) and `LAZYOS_SMP=0/1` build switch:
  `build.rs`/`build_support`, kernel `cfg(lazyos_smp)` (off ⇒ today's code
  paths, nothing compiled in), `tools/lazygui/catalog.py` +
  `test_catalog.py`, `limits.rs` key `cpu_max` (default from MADT count,
  clamped, `LAZYOS_LIMIT_CPU_MAX`), `smp.cpus=` in `lazyos.cfg`, and
  `tools/test/run.py` `--smp N`/`--soak`.
- Acceptance: `python tools/test/run.py --accel none` and the desktop
  harnesses pass unchanged with `--smp 1` **and** `--smp 4` (APs stay in
  firmware halt; the kernel does not see them yet). Ratchet script in CI with
  the current baseline.

### S8.1 — Per-CPU foundations (still one active CPU)

Mechanism: **per-CPU areas addressed via `%gs`**, `swapgs` at every
user→kernel boundary. `arch/linux.rs:8` avoided `swapgs` because *per-task* GS
desyncs when a syscall blocks; that objection does not apply to *per-CPU* GS:
the area belongs to the CPU, is installed by the CPU's own entry code, and a
task that blocks and resumes elsewhere simply re-enters through that CPU's
gate. The entry stubs must distinguish user-origin (swapgs) from
kernel-origin (no swapgs) entries — the standard CS-bit test in the stub.
Fallback considered and rejected: indexing by `lapic::id()` read on entry
(one MSR read in x2APIC, but an uncached MMIO read in xAPIC mode on the hot
syscall path, and no help for the IRQ-stack pointer the stub needs before any
read).

- New `kernel/src/arch/percpu.rs`: `struct Cpu { index, apic_id, online,
  tss, gdt, kstack (entry/IRQ), current: usize, preempt: PreemptState,
  irqoff: SpanState, irq_window: WindowState, syscall: SyscallEntryState,
  held_locks (lockdep), rng (chaos), stats }`, one static area per
  `MAX_CPUS` (compile-time 64; runtime count from `limits::cpu_max`), base
  address in `IA32_GS_BASE` (kernel-only; no `KERNEL_GS_BASE` swapping — user
  `%gs` is not used today, verified in the audit).
- Convert, one PR each, with the audit row checked off:
  - `arch/gdt.rs`: per-CPU `GDT`/`TSS`/`KERNEL_STACK`/`DOUBLE_FAULT_STACK`;
    `set_kernel_stack` writes *this CPU's* TSS (today it rewrites the one TSS
    on every switch, `task/schedule.rs:226`).
  - `arch/linux.rs`: `KERNEL_STACK`, `SAVED_USER_RSP`, `USER_CONTEXT` →
    per-CPU; the `syscall` stub reads them via `%gs` before touching rsp.
  - `task/preempt.rs`: `NEED_RESCHED`, `PREEMPTING`, `SELECTED*`,
    `SLICE_END`, `DEFERRED`, `HANDOFF_FROM/TO` → per-CPU (handoff becomes
    per-CPU: a handoff targets the CPU that will next schedule).
  - `arch/irqoff.rs` span accounting and `arch/irq_window.rs`
    (`IRQ_WINDOW_OPEN`, `LAST/OPENED/WINDOW_TICKS/UNCHARGED`) → per-CPU; the
    timer asm stub reads the window flag via `%gs` instead of the
    `#[no_mangle]` global.
  - `task/mod.rs` `CURRENT` → per-CPU `current`; `task::current()` reads
    `%gs`. `FOCUS`/`NEEDS_REDRAW`/`IDLE_TICKS` stay global (entry-lock era:
    single writer; audited).
  - `arch/fault_storm.rs`, `dev/peek.rs` claim-table note: convert or
    annotate per the audit.
- The IDT stays one shared table; IST/double-fault stacks become per-CPU.
- **Entry paths get a naked wrapper + CS test each.** Today most entry paths
  are Rust `x86-interrupt` handlers with no stub where a swapgs/CS test could
  go (`process/gate.rs`'s `int 0x80`, the shared naked IRQ/MSI stubs, every
  fault in `arch/idt.rs`); each ring-3-origin path gains a naked asm wrapper
  that tests the interrupted CS and swaps GS. The audit counts them — they
  are the same paths S8.3's ring-3-origin entry-lock rule keys on.
- **NMI gets an IST + paranoid entry in this stage.** `nmi.rs` is a plain
  `x86-interrupt` handler today, and the kernel deliberately uses NMI as the
  hang reporter (it fires over IF=0 spins) — so it will land exactly at
  entry boundaries: in user mode (needs its own swapgs) or inside a stub
  after swapgs (must not swap again, must not un-swap on exit). The
  swap-tracking rule is recorded in `docs/architecture/smp.md`.
- `relax.rs`'s `TASKS.is_locked()` diagnostic check becomes per-CPU-aware
  here.
- Acceptance: everything above passes at `-smp 1` bit-identically (the
  per-CPU area of CPU 0 *is* the old globals' replacement; a `cfg`-gated
  assertion cross-checks old vs new during the transition PRs, then is
  removed). `tools/sync/check_locks.py` baseline only shrinks. New
  `LAZYOS_TEST_FILTER=percpu` cases (area layout, swapgs paths via
  `dispatch_for_test`, entry from user vs kernel).

### S8.2 — AP bring-up, IPIs, TLB shootdown (APs online but idle)

- **Trampoline**: a position-independent real-mode blob (16-bit: set up
  segments, switch to protected/long mode using tables the kernel prepares,
  jump to a per-CPU Rust entry) assembled with **`global_asm!` and LLVM's
  integrated assembler (`.code16`)** from `kernel/src/arch/smp/trampoline.S`
  — the kernel already builds naked asm this way (`linux.rs`, `switch.rs`,
  `idt.rs`), and this keeps zig out of every kernel build and the
  kernel-tests CI job (zig stays Mail-only per AGENTS.md). Two boot facts the
  first draft got wrong: the bootloader (0.11.17) maps physical memory at a
  **dynamic offset and identity-maps nothing low**, so the kernel builds a
  small dedicated AP page table (identity-mapped low page + the shared kernel
  half) at boot, and reserves the trampoline page and the per-AP mailbox
  **before `mem::init` makes the frame allocator live**. Host test: the blob
  links at two addresses to identical bytes (position independence).
- **LAPIC ICR** (`arch/lapic.rs`): `send_init`, `send_sipi`, `send_fixed`,
  `send_nmi` for xAPIC MMIO (0x300/0x310, delivery-status polling) and x2APIC
  MSR (0x83F/0x830... per SDM; one write in x2APIC). Bring-up sequence per AP:
  INIT, 10 ms wait, SIPI x2 at the trampoline page, poll `Cpu.online` with a
  bounded timeout; a failed AP is logged (`HW:SMP:CPU<n> apic=<id> FAIL`) and
  left offline — boot continues with the CPUs that came up (stability: a
  flaky core is degraded performance, never a dead machine).
- **AP init** (`arch/smp/ap_entry.rs`): EFER.NXE+SCE, CR0/CR4/XCR0 exactly as
  `cpu::init` sets them on the BSP, `IA32_PAT`, per-CPU GDT/IDT/TSS load,
  syscall MSRs (`STAR/LSTAR/FMASK` — note `FMASK` clears IF, keeping the
  entry-serialized model intact), GS base → its `Cpu`, LAPIC SVR/LVTs/timer
  calibrated, then `hlt` in the idle loop awaiting work (S8.3 gives it real
  work).
- **Clock gate, split in two** — a single invariance gate would make every
  TCG-based SMP test vacuous (QEMU TCG refuses `invtsc`, and TCG is the
  correctness rig): (a) **coherence, always required** — QEMU's TSC is one
  clock shared by the vCPUs; on real hardware measure pairwise skew with the
  existing `arch/clock.rs` calibration and refuse SMP on skew
  (`HW:SMP:TSC:SKEW`, stay single-CPU); (b) **invariance, required only for
  wall-clock sanity** (CPUID.8000_0007H EDX[8]), reported
  (`HW:SMP:TSC:NONINV`) with a test-only override (`smp.tsc=force` in
  `lazyos.cfg`, compile-gated to `cfg(lazyos_tests)`) so the TCG rig can
  bring APs up. On real boxes (NUC/N150 plans) non-invariant TSC is the
  first thing that goes wrong; refusing there is the stable answer.
  `monotonic_ns` keeps its single TSC stream: one designated **timekeeper
  CPU** owns `TICKS`/catch-up (`arch/clock.rs`); AP timers drive preemption
  and accounting only. `window_tick` also calls `periods_since_last()`, but
  windows only open inside a syscall on the entry-lock holder, so the stream
  stays single-writer; `clock.rs`'s "this single CPU cannot run it in
  between" comment gets its audit row.
- **IPI vectors** (new, registered in `arch/idt.rs`): `RESCHED_VECTOR`
  (per-CPU: set `NEED_RESCHED`, window-safe), `SHOOTDOWN_VECTOR`,
  `STOP_VECTOR` (stop-the-world for PAT changes and panic). Allocated in
  **0x60–0x7F** — the free range between the MSI block (0x40–0x5F) and the
  software gates (0x80/0x81) — pinned in the audit so a future MSI expansion
  cannot collide. IPI handlers obey
  the `irq_window` contract exactly: no locks but the per-CPU area, no
  allocation, no task switch — they are the SMP generalization of today's
  window-safe handlers, and the audit records that.
- **TLB shootdown** (`mem/tlb.rs`): `shootdown(cpus, addr, pages)` — sender
  writes the range into each target's per-CPU mailbox, sends the IPI, targets
  `invlpg` in the handler and ack with an atomic counter, sender spins with
  the watchdog. The caller list is **derived mechanically** from
  `grep -rn "tlb::flush" kernel/src` (one audit row each), not from memory;
  today that surfaces, in rough order of danger: **`mem/cow.rs`
  `clone_user_table` (fork)** — the sharpest, because fork marks the
  *parent's* leaves read-only/COW in place and flushes only locally; with
  parent threads on two CPUs a stale writable entry on the non-forking CPU
  writes straight through to the shared frame after fork, silently;
  `mem/protect.rs` (`filemap::write_protect`, `unmap_from` — which unmaps in
  *every* address space), `mem/reclaim.rs`, `mem/filemap.rs` + page-cache
  eviction, `mem/uspace.rs` unmap/remap (`remap_page` in mremap is not
  teardown but needs the same treatment), and the boot-time-only rows
  `fbwindow.rs` and `mmio.rs` (safe: before any AP is online, MMIO windows
  handed out monotonically). The fault paths (`pagefault.rs` COW/demand
  resolve) deliberately get **no** shootdown: a resolve can only make an
  entry more restrictive or install a new one, so remote stale entries are
  safe — the audit records that reasoning so a reviewer does not flag it.
  Rule recorded in `docs/architecture/virtual-memory.md`: *any
  unmap/write-protect of a page table that a running task might be using
  goes through `shootdown`, never bare `invlpg`*. Kernel-half-only changes
  are **not** exempt by "a CR3 write flushes all" — a local flush never
  cleans another CPU's TLB, PCID or not (there is no PCID today either).
  They are safe now only because every kernel-half PTE change is
  boot-time-only and monotonic in address; the audit pins *that* argument,
  with a tripwire test so a runtime kernel-half unmap added later fails the
  suite until it shoots down.
- **Panic path**: a panic on any CPU sends `STOP_VECTOR` to all others (they
  halt, no further damage), then the existing serial/console panic dump runs
  on the panicking CPU with per-CPU register dumps appended. `serial.rs`'s
  force-unlock keeps working because stop-the-world makes "one CPU" true
  again.
- Acceptance: boot log `HW:SMP:CPU<n> apic=<id> online` for every AP;
  `LAZYOS_TEST_FILTER=smp` cases **driven through a `smp::bring_up(n)`
  hook** (the `LAZYOS_TESTS=1` image never reaches `arch::init`, so
  bring-up is exposed to the suite explicitly): bring-up with 1/2/4 vCPUs,
  IPI round trip, shootdown correctness — unmap a page mapped in another
  CPU's table and prove the other CPU faults (including the fork case from
  `cow.rs`), STOP vector, failed-AP tolerance by refusing one SIPI via the
  hook; the whole existing
  suite at `-smp 4` with APs idle (they must not perturb anything);
  `pngstats` desktop run at `--smp 4`. Under TCG the invariance half of the
  clock gate is overridden (`smp.tsc=force`); a KVM/WHPX run re-verifies the
  real gate.

### S8.3 — The kernel entry lock (ENABLED MILESTONE)

The keystone. One global `sync::Lock` — the **entry lock** — held by a CPU
whenever it executes kernel code on behalf of ring 3. **Acquisition is keyed
on the interrupted context, not on the event**: an entry whose interrupted
CS is ring 3 (native `int 0x80`, Linux `syscall`, a device IRQ or exception
arriving from user mode) acquires the lock; an entry arriving from ring 0
runs **under the interrupted context's hold** (historic-BKL / paranoid-entry
semantics) — the tick's `schedule()`, an IRQ bottom half, a page fault
inside the kernel simply run while the hold is inherited. This distinction
is what keeps the kernel's most common idle paths from deadlocking on
themselves: a napping task (`task/wait.rs` naps in a loop with IRQs on) that
had reacquired the lock on resume, then took its own tick interrupt, would
otherwise spin forever on itself — same for `exit_cpu`'s `enable_and_hlt`
loop and for the mux task, whose whole loop runs with IRQs on while holding
the heap lock (`preempt.rs` documents that an interrupt hitting that loop
must not spin on it). The lock is released when the CPU is about to leave
kernel context — return to user mode, park the interrupted task, enter the
idle loop — and, just as importantly, at a **preemptive switch** (IRQ
return → `preempt_point` → `yield_now`), not only at a blocking park:

- Park/switch = release as part of the switch (after the scheduler has chosen
  the next task on this CPU, before `install`), reacquire on resume. This
  mirrors today precisely: a blocked syscall stops executing kernel code and
  the scheduler runs something else; under SMP "something else" may be on
  another CPU.
- IPI handlers and the timer tick's window-safe portion never take it (they
  hold no locks by contract); a tick that decides to `schedule()` runs under
  the inherited hold.
- IRQ bottom halves (`dev::intx::service`) run under the inherited hold in
  IRQ context, exactly as today they run with interrupts off; the
  quiet-context rule (`preempt.rs`) composes unchanged.
- With `cfg(lazyos_smp)` off, the lock is a zero-cost `without_interrupts`
  wrapper — one code shape, two behaviours, no fork.

Scheduler changes (minimal in this stage): run queue and `decide` stay global
under the entry lock; `CURRENT`/preempt state are already per-CPU (S8.1);
each CPU gets an **idle task** (the parked-kernel-task-per-CPU that `hlt`s,
replacing the "no idle task" note in `preempt.rs:72-76`) so a CPU with nothing
runnable never spins holding the lock; wakeups that should preempt a *remote*
CPU's current task send `RESCHED_VECTOR` (today `note_wake` only sets the
local flag — the audit's most subtle row: a wake on CPU A of a task that
belongs on CPU B must IPI B).

- `arch/event_timer.rs`'s `ARMED`/`RATE` become per-CPU **here**, not in
  S8.4: the one-shot deadline timer programs the *arming CPU's* LAPIC timer,
  so with N CPUs a deadline armed by a task that resumes on another CPU
  would fire late — correctness holds (the 10 ms tick sweep is the backstop,
  `schedule.rs:243-249`), but sleep latency regresses silently; per-CPU
  arming keeps it exact. AP timer drift itself breaks nothing structural:
  slice/deferral bookkeeping only wobbles fairness, and deadlines are
  expiry-swept at every scheduler entry from the timekeeper's clock.

Why this is stable: every correctness argument in the kernel that says "with
interrupts off, nothing else mutates this" becomes "under the entry lock, no
other CPU is in the kernel" — same guarantee, new mechanism. The only new
concurrency is user-vs-user (isolated by address spaces, as today) and
user-vs-kernel-elsewhere, which is exactly what the S8.2 shootdown work made
sound. Everything userspace-visible already works: syscalls, IPC handoffs
(`preempt::hand_off` becomes per-CPU), signals, the mux.

Also in this stage:

- `arch/irq_window.rs`: windows remain per-CPU; the contract sentence "a
  window is safe under any other lock" is re-derived in the audit doc for the
  entry lock (a window may **not** open while a *contended* entry-lock spin is
  in progress on that CPU — the window flag gates it).
- Quota accounting (`kernel/src/quota.rs`) and `IDLE_TICKS`: per-CPU
  accumulation, global fold, under the entry lock — no new races.
- Tracing: the recurring evidence lines go through the trace API, not
  `serial_println!` — a new `Smp` row in `libs/tracefmt`'s catalog
  (`python tools/trace/catalog.py`) for the entry-lock contention counters,
  the existing `Lock` channel for lockdep cycles and `LOCK:WAIT`-style lines
  (never a stray `LOCKDEP:` prefix); boot-time facts (`HW:SMP:CPU<n>
  online`, `HW:SMP:TSC:*`) stay sanctioned `HW:` serial diagnostics
  (`kernel/build.rs`). Per-CPU `PERF:` lines (the existing
  latency harness grows `--smp N`); contention on the entry lock becomes a
  first-class perf metric (holder CPU + site histogram), because it is the
  number that motivates each S8.4+ step.

Acceptance (the milestone gate):

- Whole in-kernel suite at `-smp 1/2/4`, TCG (deterministic) and KVM.
- New `smp_suite` stress cases (§4.7) green, including the 10-minute soak
  (`python tools/test/run.py --smp 4 --soak`) with post-run invariant checks:
  frame/heap counters balanced, no leaked slots, ext2 checker clean on the OS
  volume, log ring uncorrupted.
- Desktop harnesses (`core_apps.json`, `shutdown_shell.json`, the accounts
  attack harness, `tools/net/run.py --netd`) at `--smp 4` — the account
  attacks are concurrency attacks for free (a policy probe racing a service
  restart).
- Chaos-preemption seed sweep as a CI job (50 seeds × the stress cases —
  under TCG this is hours of wall time, not a step): zero failures, every
  seed replayable.
- Perf: parallel user-space benchmark (e.g. N compute tasks via `rhai`)
  scales ~linearly to N; a kernel-heavy benchmark (syscall ping-pong) does not
  regress vs `-smp 1` beyond the lock's own cost (measured, recorded in
  `docs/perf/history.md` with `--label smp-entry-lock`).
- Real hardware: one `dbgd` session on the NUC box with `smp.cpus=2`
  (`lazyos-dbg` skill), 24 h soak, `HW:SMP` lines clean.

**This is the release point.** `LAZYOS_SMP=1` desktops are supported from
here; S8.4+ are performance work that may always be declined.

### S8.4 — Scheduler fine-graining

Replace "global runq under the entry lock" with per-CPU run queues + a
`sched::Lock`-protected migration path:

- Per-CPU `RUNNABLE` masks (today `task/runq.rs` globals); `select_next`
  becomes per-CPU; stride scheduler keeps its global `VIRTUAL_CLOCK` under a
  short scheduler lock (or per-CPU clocks with periodic rebalance — decided by
  measurement, default global: simpler to reason about, the lock is taken once
  per switch).
- Wakeup placement policy: prefer the waker's CPU, then the idlest; migration
  only of Runnable (never Running) tasks; `fs_base` already travels with the
  `Task` (written at `schedule.rs:230`), and the audit confirms nothing else
  per-task must (there is no per-task TSC state today; none is added).
- The entry lock is **removed from the scheduler and timer paths first** (they
  are the hottest), while syscalls/IRQs keep it. This splits the invariant:
  scheduler code must now be reviewed as genuinely concurrent; it is small,
  and its lockdep order (sched → TASKS, never TASKS → sched) goes into
  `locking.md` before the PR.
- `install`'s CR3/TSS/GS writes are already per-CPU; the invariant comment at
  `schedule.rs:212-217` gets its real SMP proof (selection under the scheduler
  lock pins the slot; slot teardown takes the same lock).
- Acceptance: suite + smp_suite at `-smp 4`; new migration stress (N tasks,
  M<N CPUs, forced wake storms); fairness check (stride shares within 5% under
  parallel load, judged from `PERF:` accounting); no regression at `-smp 1`;
  perf: context-switch rate and wake-to-run latency (`tools/perf/run.py --smp 4`)
  improve measurably vs the entry-lock baseline or the step is reverted.

### S8.5 — Memory fine-graining

- Frame allocator: keep the central `FRAMES` lock but add per-CPU free-frame
  magazines (batch of 32) so the hot path never takes it; the magazine
  discipline goes in `docs/architecture/physical-memory.md`. Heap: per-CPU
  size-class slab caches in front of `IrqSafeHeap` (the #382 rule — heap lock
  only with interrupts off — is replaced by: heap lock is a `sync::Lock`, and
  the per-CPU caches make contention rare). `mem/heap.rs` growth path
  (heap→frames order) recorded in `locking.md`.
- The page-cache/`filemap` arguments flagged in
  `docs/architecture/virtual-memory.md:124-130` (fault epoch, "no store lands
  while the cache lock is held") get real critical sections: per-inode
  `PageCache` locks become `sync::Lock` held across the fault fixup, with the
  epoch checked under the lock; shootdowns (S8.2) make the unmap side sound.
- COW: `page_fault_dispatch` runs per-CPU concurrently; the fault path takes
  the address-space lock (new, per-PML4) around COW resolution — two CPUs
  faulting the same COW page is the classic SMP corruption, and the audit
  gives it a named test (`smp_suite::cow_race`, chaos-seeded).
- Acceptance: allocator soak ×4 CPUs (the existing million-alloc soak, now
  parallel), `cow_race`, reclaim-under-parallel-pressure (N tasks touching
  >RAM with the flusher running), `LAZYOS_TEST_FILTER=bcache` unchanged,
  ext2 checker clean after every soak, frame/heap counters balanced.

### S8.6 — IPC, interrupts, VFS

- **IPC**: split `CHANNELS: Mutex<Registry>` (the single hottest lock under
  the entry lock's contention histogram) into per-transaction and
  per-endpoint locks with the documented order; `WaitQueue` park/wake
  re-derived without the interrupts-off crutch (register-then-block becomes
  lock-held-across-state-set — the `task/wait.rs` rule inverts, and
  `locking.md` records both eras so a reader of old comments is not misled);
  targeted wakeups already exist (`channels/recv/waitset.rs`) and get the
  remote-IPI treatment from S8.3. `inet/bell.rs`'s single-waiter atomics get
  their audit row closed (they were written "rung from syscall context (one
  CPU)"; now genuinely atomic multi-waiter or documented single-waiter with a
  CAS claim).
- **IRQ steering**: I/O APIC RTE destinations and MSI address APIC IDs become
  per-device choices (`dev/msi.rs:96-99`, `arch/irqchip.rs:102-104`): a
  device's IRQ goes to the CPU its driver's task last ran on, reprogrammed on
  migration (bounded: at most one reprogram per device per second,
  `HW:IRQ:STEER` traced). `dev::irq::dispatch`'s "no locks" rule survives —
  the `RAISED` bits are already atomic; the quiet-context test becomes
  per-CPU.
- **VFS**: `YieldMutex` (whose relax strategy assumed one CPU) is replaced by
  a proper sleeplock — park on a wait queue, wake the next waiter — for `FS`/
  `ABI_FS` and the ext2 `volio::Gate`; the park-inside-critical-section
  semantics (waiting for `usbd`/virtio providers) are exactly what a sleeplock
  is for, and the 200 µs park hack dies with the single-CPU assumption it
  encoded. Page-cache coherence deadline (`fuse::ATTR_TICKS`) unchanged.
- **Linux ABI**: `getcpu` (vDSO-less rdtscp+APIC or per-CPU `%gs` value),
  `sched_getaffinity` report the real mask; `process/linux/futex.rs`'s
  single-CPU note closed (futex wake becomes an IPI-carrying wake like every
  other).
- Acceptance per subsystem: its existing harness green at `-smp 4`
  (`tools/usb/run.py --hotplug`, `tools/smb/run.py`, `tools/net/run.py
  --netd`, the fat_run replug race), plus a parallel-IPC throughput number in
  `docs/perf/report.md`; messengerd round trip under 4-way contention within
  2x of the single-pair latency.

### S8.7 — Performance and as-built docs

- Entry lock fully retired (syscalls/IRQs take only their subsystem locks);
  the retirement PR includes the final `smp-audit.md` pass — every row closed
  or explicitly exempted with a reason.
- Platform-plan S8 acceptance: "linear speedup on N vCPUs" measured with a
  fixed workload set (parallel compute, parallel build-like fork/exec churn,
  parallel file I/O, parallel IPC) at `-smp 1/2/4/8` KVM, recorded in
  `docs/perf/history.md`.
- Docs: new `docs/architecture/smp.md` (as-built: entry model history, per-CPU
  layout, IPI vectors, lock order summary); update `tasks.md` (SMP/per-CPU
  runqs no longer "not yet"), `virtual-memory.md` (shootdown as-built),
  `interrupts.md` (destinations), `arch.md` (irq_window ↔ IPI contract),
  `limits.md` (`cpu_max`), AGENTS.md (a "Multicore (SMP)" section: build
  switch, `--smp`, the suite commands, the "never bare-invlpg a shared
  mapping" rule).

## 6. Per-CPU data inventory (S8.1 checklist)

| Global today | File | Disposition |
|---|---|---|
| `CURRENT` | `task/mod.rs:187` | per-CPU (S8.1) |
| `SCHEDULING` | `task/mod.rs` | boot-phase flag; stays global (audit) |
| `KERNEL_STACK`, `SAVED_USER_RSP`, `USER_CONTEXT` | `arch/linux.rs` | per-CPU (S8.1) |
| `GDT`, `TSS`, `KERNEL_STACK`, `DOUBLE_FAULT_STACK` | `arch/gdt.rs` | per-CPU (S8.1) |
| `NEED_RESCHED`, `PREEMPTING`, `SELECTED*`, `SLICE_END`, `DEFERRED`, `HANDOFF_*` | `task/preempt.rs` | per-CPU (S8.1); `NAPPING` stays per-task |
| `SPAN_*` | `arch/irqoff.rs` | per-CPU (S8.1) |
| `IRQ_WINDOW_OPEN`, `LAST/OPENED/WINDOW_TICKS/UNCHARGED` | `arch/irq_window.rs` | per-CPU (S8.1) |
| `LAST_TSC`, `MISSED` catch-up state | `arch/clock.rs` | timekeeper CPU only (S8.2) |
| `ARMED`, `RATE` event timer | `arch/event_timer.rs` | per-CPU (S8.4) |
| `RUNNABLE`, `DONE` | `task/runq.rs` | per-CPU (S8.4) |
| `VIRTUAL_CLOCK` | `task/sched.rs` | global under sched lock (S8.4) |
| `FOCUS`, `NEEDS_REDRAW`, `IDLE_TICKS` | `task/mod.rs` | global, entry-lock era; revisit S8.4 |
| entry-lock spin histogram, lockdep stack, chaos PRNG | new `arch/percpu.rs` | per-CPU (S8.1) |

Everything else in the audit (registries, allocator, IPC tables, VFS) is
*lock-protected global*, not per-CPU, and is handled by its stage.

## 7. Known hazards and standing answers

- **TSC skew / invariance** — the gate splits: coherence (skew) is always
  required, invariance only for wall-clock sanity with a test-only override —
  QEMU TCG refuses `invtsc`, so a single gate would disarm the correctness
  rig. On real boxes, refusal (`HW:SMP:TSC:SKEW`) is a supported outcome.
- **NMI at an entry boundary** — NMI is the hang reporter and fires exactly
  where per-CPU GS is being swapped; it gets an IST + paranoid-entry rule in
  S8.1, before any AP is online.
- **Trampoline bugs are triple-faults** — bounded SIPI retry, failed AP left
  offline, boot continues; QEMU `-d int` reproduction steps in
  `docs/architecture/smp.md`.
- **Deadlock: entry lock held across park** — machine-checked (§4.1); the
  park path releases by construction, and any new sleep mechanism must go
  through `WaitQueue`/sleeplock, which the assert covers.
- **Spin while the owner is descheduled** — watchdog panic with owner dump
  (§4.1); `YieldMutex`'s 200 µs park strategy is *not* ported to
  `sync::Lock` (its premise dies with one CPU) — S8.6 replaces the two
  park-inside locks with sleeplocks instead.
- **Stale TLB on a remote CPU** — shootdown before the unmap is observable
  (S8.2 rule); PCID introduction is a flagged future review point.
- **IRQ storms + entry lock = starvation of user CPUs** — IRQ bottom halves
  already defer to quiet contexts; the deferral test becomes per-CPU, and the
  S8.6 steering spreads the load.
- **Two CPUs faulting one COW page** — named test (`cow_race`) + per-PML4
  lock (S8.5).
- **Panic with N CPUs live** — STOP-vector stop-the-world before any dump or
  reboot path; the serial force-unlock stays sound (§S8.2).
- **Determinism loss in CI** — TCG+icount is the correctness verdict, KVM the
  perf verdict; a failure that only reproduces on KVM gets its seed/trace
  captured via dbgd before any fix claim.
- **Regression risk to the shipped single-core product** — `LAZYOS_SMP=0`
  builds compile the lock out entirely; CI keeps a `-smp 1` no-SMP lane
  forever.

## 8. Effort and ordering summary

| Stage | Size | Gate to next stage |
|---|---|---|
| S8.0 audit + tooling | M | audit doc merged, ratchet in CI |
| S8.1 per-CPU foundations | L | suite green at -smp 1, percpu tests |
| S8.2 AP bring-up + shootdown | L | APs online, smp tests, TSC gate |
| S8.3 entry lock | M | **milestone**: suite+soaks+chaos green at -smp 4 |
| S8.4 scheduler | M | migration stress + fairness + perf |
| S8.5 memory | L | cow_race + parallel soaks + checker clean |
| S8.6 IPC/IRQ/VFS | XL | per-subsystem harnesses at -smp 4 |
| S8.7 perf + docs | M | platform-plan S8 acceptance |

S8.0–S8.3 are the stability core and are ordered so that no stage requires
trusting an unaudited invariant: S8.1 and S8.2 make the hardware per-CPU
facts true, S8.3 makes the software invariant explicit, and only then does any
second CPU execute a kernel-adjacent instruction path. Every later stage
shrinks the serialized region by one measured, lockdep-covered, soak-covered
subsystem at a time, and each can stop there permanently without leaving the
kernel in a half-parallel state.

## 9. Review record

The plan was reviewed before adoption by a fresh-context agent that did not
write it: it verified every file:line claim against the tree, probed the
build machine's QEMU live, and walked the entry-lock rule against the
park/nap/mux/handoff paths. Findings and what changed in response:

- **Blockers.** (1) The entry-lock rule is now keyed on the *interrupted
  context's ring*: ring-0-origin interrupts inherit the hold instead of
  acquiring, and release also happens at preemptive switches — the original
  "every IRQ acquires" wording deadlocked on the kernel's own idle paths
  (napping tasks, `exit_cpu`'s hlt loop, the mux with IRQs on). (2) The TSC
  gate is split into coherence (always) vs invariance (wall-clock only, with
  a `cfg(lazyos_tests)` override) — QEMU TCG refuses `invtsc`, so the
  original gate would have silently disarmed every TCG-based SMP test. (3)
  `mem/cow.rs` fork downgrades joined the S8.2 shootdown list, derived
  mechanically from a `tlb::flush` grep rather than from memory.
- **Majors.** NMI gets an IST + paranoid entry and every ring-3 entry path a
  naked CS-test wrapper in S8.1 (most handlers are Rust `x86-interrupt`
  functions with no stub today); the trampoline builds with `global_asm!` /
  `.code16` instead of zig (zig must stay Mail-only) and needs a dedicated AP
  page table because the bootloader maps physical memory at a dynamic offset
  with nothing identity-mapped low; the SMP suite is split by harness
  (`tests::run()` pre-scheduler cases vs a `qemu_session.py`-driven system
  harness) with a `smp::bring_up(n)` hook; `event_timer` per-CPU arming
  moved up to S8.3; the "kernel-half changes stay local" claim was replaced
  by the boot-time-only + monotonic-MMIO argument with a tripwire test;
  `tools/test/run.py` gains `--smp`/`--soak`; trace-channel decisions are
  pinned (a new `Smp` row, the existing `Lock` channel, no stray `LOCKDEP:`
  prefix).
- **Verdict recorded:** the staged entry-lock approach is the right
  stability-first strategy for this kernel; strict single-CPU remains a
  legitimate permanent alternative, which the plan's "S8.4+ may always be
  declined" framing preserves.
