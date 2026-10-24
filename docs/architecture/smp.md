# SMP, as built (S8.0–S8.3b)

What multicore support the kernel has today, how it works, and the rules a
change must keep. The plan and its staging are [`../smp-plan.md`](../smp-plan.md);
the inventory of every single-CPU assumption is [`smp-audit.md`](smp-audit.md).

**State.** The application processors (APs) come up, check their clock, and
idle with interrupts on, serving three IPIs. **No AP runs a task**: the
scheduler, every syscall and every interrupt handler other than the three IPIs
still run on the boot CPU (the BSP), so the kernel's central invariant ("only
one CPU executes kernel code") still holds. A normal boot keeps the APs parked
(`limit.cpu_max` defaults to 1); an image opts in with
`LAZYOS_LIMIT_CPU_MAX=N` (`run_demo.py --limit cpu_max=N`) and QEMU `--smp N`.
S8.3's entry lock is what lets an AP run user tasks; everything below exists
so that step is safe. The lock itself is built (S8.3a) and in every gate of a
`LAZYOS_SMP=1` kernel, where it checks the hold discipline on every entry;
with only the boot CPU running tasks it is never contended outside the
suite. S8.3b gave each CPU an idle context, made every halt drop the hold
and marks which CPU each task is current on ("Idle, halts and on-CPU
marks").

## Bring-up

`arch::smp::init` (after `arch::init`) decides the CPU count — the MADT's
enabled processors, capped by `smp.cpus=` in `lazyos.cfg`, `limit.cpu_max` and
`percpu::MAX_CPUS` (64) — and refuses, staying single-CPU with a `HW:SMP:CPUS=1
(reason)` line, when: there is no usable MADT, the TSC is not calibrated, the
TSC is not invariant (`HW:SMP:TSC:NONINV`; `smp.tsc=force` overrides it in
test kernels only, because QEMU TCG refuses `invtsc`), the trampoline page
(`0x8000`) is not usable RAM in the boot memory map, or the AP page table
cannot be built below 4 GiB.

For each startable APIC id (enabled MADT entries only; xAPIC ids above 0xFF are
skipped) that is neither the BSP nor already online, `bring_up_one` writes the
mailbox, sends INIT, waits the SDM's 10 ms, sends SIPI twice 200 µs apart, and
waits a second for the AP. A CPU that does not start is parked with INIT and
its area is **retired** (an AP that started late may still be using it); a
CPU whose clock is refused is parked and its area stays reusable. Either way
boot continues with the CPUs that came up.

The online count (`online_cpus()`) is raised **before** the BSP accepts the
AP's clock, so it never trails an area whose `online` is set. The accept is a
hand-off with one winner: the AP moves the exchange state from `ACCEPT` to
`CLAIMED` (compare-exchange) and only then publishes `online`; a BSP that
reaches its deadline first moves it to `WITHDRAWN`, parks the CPU and lowers
the count. A BSP that loses that race waits for the claimed CPU's `online`
instead of parking a CPU that is about to run. An AP flushes its whole TLB right
after it publishes `online`, behind a full fence that pairs with the
shootdown sender's fence before its scan: a sender that did not see the CPU
online changed its tables before that flush.

### The trampoline page (physical `0x8000`)

`arch/smp/trampoline.rs` is a position-independent real → protected → long mode
blob assembled with `global_asm!`; the BSP copies it to `0x8000` and patches
the few absolute values (GDT base, the two `retf` targets). The frame
allocator never hands out anything below 1 MiB, so nothing competes for the
page.

| Offset | Content |
|---|---|
| `0x0000` | the code |
| `..0x0D00` | the early stack (grows down) |
| `0x0E00` | temporary GDT (5 entries) |
| `0x0E30` | the GDTR; `0x0E40` the `retf` patch slots |
| `0x0E50` | the early `IDTR`; `0x0E60` the early NMI table; `0x0E9C` its NMI count |
| `0x0F00` | the mailbox: magic `0x5A5A_0001`, `CR3`, `ap_entry`, the AP's `Cpu` area, its stack top |

The AP runs on a dedicated page table (identity-mapped low 2 MiB over the
machine-wide kernel half) until `ap_entry` switches to the boot table.
Traps for the next person: `EFER.NXE` must be on before paging (the kernel
half uses NX); the xAPIC ICR destination is `ICR_HI` bits 24..32 (a bare id
there is destination 0 and an INIT resets the BSP); long mode ignores `DS`'s
base; LLVM's `retf` under `.code16` is a 32-bit return (`.byte 0xCB`); the AP
starts with `CR0.CD/NW` as INIT left them.

The temp GDT is 0x08 64-bit code, 0x10 flat data (the stack), 0x18 data based
at `0x8000` (the fixed offsets), 0x20 32-bit code: code and stack selectors
match the kernel GDT's (0x08, 0x10), so a frame the early NMI stub pushed is
still loadable after `ap_entry` loads the kernel GDT. `ap_entry` builds its
GDT/TSS and loads the kernel IDT **before** it leaves the trampoline's page
table (the early NMI table lives in its identity-mapped page).

### Per-CPU areas

`arch/percpu.rs`: one `Cpu` per slot, addressed through `%gs` (`gs:[0]` is the
area's own address). In ring 0 `IA32_GS_BASE` is the area; in ring 3
`IA32_KERNEL_GS_BASE` holds it and every ring-3 boundary executes one `swapgs`
(`arch/gsgate.rs`; user `%gs` is attacker-controlled, so nothing may trust it).
A new ring-3-reachable vector must use `irq_gate!`/`paranoid_gate!`, and
`CR4.FSGSBASE` must stay off. Each area holds its own GDT/TSS, kernel,
double-fault and NMI stacks, the preemption/irq-window state, the IPI mailboxes
(`IpiState`) and, in test kernels only, the job mailbox.

`percpu::current()` and `percpu::area(i)` return **`&'static Cpu`**, never
`&mut`: the owning CPU, an interrupt handler re-entering it and (S8.3) a
remote waker hold one at the same time. Every member is an atomic or an
`UnsafeCell` (same layout, so the asm's `gs:` offsets and the
`percpu_entry_asm_offsets_match_the_layout` pin are unchanged); the
non-atomic ones have one accessor each that states who may touch them:
`set_rsp0` (the TSS, its own CPU), `user_context`/`set_user_context` (by
value, its own CPU), `gdt::init_cpu` (TSS and GDT, before the CPU runs on
them). The stacks are cells too, reached only by their top addresses.

### TSC gates

Two, deliberately separate (the invariance one alone would have disarmed every
TCG-based test):

1. **Invariance** (`CPUID.8000_0007H:EDX[8]`), reported and required for
   wall-clock sanity.
2. **Coherence** (`arch/smp/tscsync.rs`), required always. Before an AP may go
   online, AP and BSP exchange 32 rounds of `t1 (AP) → tb (BSP) → t2 (AP)`.
   For coherent clocks `t1 <= tb <= t2`; each round's violation is a lower
   bound on the offset (short by that round's hand-off latency), so the
   largest over the rounds is the estimate and one delayed round cannot hide a
   skew. More than 50 µs is
   refused: `HW:SMP:TSC:SKEW cpuN apic=.. <cycles> cycles, refused`, the AP is
   parked. The AP does not publish `online` itself after its last round — the
   BSP's INIT for a refused CPU can land after the AP finished, and an AP
   that had marked itself online by then would be counted while parked — it
   waits for the BSP's explicit accept.

The boot path logs the invariance bit (`HW:SMP:TSC:INVARIANT=`; the suite
logs it once, marked forced), and each accepted AP's online line carries the
measured offset (`skew=<cycles>`). Under WHPX on the development box the bit
is set and every AP measures 0 cycles.

`monotonic_ns` keeps its single TSC stream; there is no per-AP timer yet.

## NMIs

An NMI cannot be masked; the platform may raise one on any CPU at any time
(QEMU's `inject-nmi`, which the harness sends to a hung boot, reaches every
CPU; a board's NMI button; LINT1 on an AP stays masked from INIT until
`init_ap_local`).

**During bring-up.** INIT leaves `IDTR` at base 0, where an NMI
triple-faults the machine (the whole machine: QEMU resets). The trampoline's
**first instruction** loads the early NMI table, laid out so one table serves
all three modes: the real-mode IVT's vector 2 at `+8`, the 32-bit IDT's at
`+16` and the 64-bit IDT's at `+32` do not overlap. Each points at a stub
that counts the NMI and returns. The 32-bit stub also covers the three
instructions between `CR0.PE` and the far return, where `CS` still holds the
real-mode `0x0800` that `iretd` could not reload: it resumes at `smp_prot`
(everything those instructions did is redone there). Then the kernel IDT is
loaded in `ap_entry` before the page table switch. What is left, and why it
is accepted: an NMI delivered before the first instruction (latched while
the AP waited for its SIPI) takes INIT's `IDTR`, the real-mode IVT at
physical 0 (the BIOS's, left intact on a BIOS boot; unknown on UEFI); and a
real-mode NMI before `mov ss/sp` pushes three words at `0000:FFFA` (low
conventional memory nothing uses). Both are a few instructions wide on a
path that runs once per AP per boot.

**Nested NMIs.** The handler runs on its own IST stack, and the CPU blocks
further NMIs only until the next `iretq` — any exception inside the handler
(a `#PF` on a stack word it dumps) ends with one. A second NMI would then
land on the same IST top, overwriting the first one's frame. The gate
(`arch/nmi.rs`) first copies the hardware frame off the IST top onto the rest
of the NMI stack and runs there; an NMI whose interrupted `rsp` lies inside
this CPU's NMI stack has interrupted the handler and is **dropped** (counted,
`nmi::nested_dropped`) with an `iretq` straight from the IST top, which the
outer handler no longer uses. The test is stateless (no flag to set or clear
in a window), and a dropped NMI costs a second hang report only. Linux
re-runs the handler for a nested NMI instead; nothing here needs that. Only a
ring-0 interrupted context is tested (ring 3 may aim its `rsp` anywhere); the
`syscall` entry's swap window, ring 0 on a user-chosen `rsp`, can still make
one NMI look nested, which only drops its report.

## IPIs

Vectors `0x60..0x62`, in the free range between MSI (`0x40..0x5F`) and the
software gates (`0x80`, `0x81`); `0x60..0x7F` is reserved for kernel IPIs.

| Vector | Name | Handler |
|---|---|---|
| `0x60` | `RESCHED` | sets this CPU's `need_resched` |
| `0x61` | `SHOOTDOWN` | serves this CPU's mailbox (if a wait loop has not), acks |
| `0x62` | `STOP` | marks the CPU `stopped` and halts with interrupts off |

The handlers obey the `irq_window` contract: no lock but the per-CPU area, no
allocation, no task switch. They sit behind the same naked gates as every other
interrupt (`arch/gsgate.rs`).

The ICR is each CPU's own register, so two CPUs' sends never interfere; the
xAPIC send's two writes (`ICR_HI`, then `ICR_LO`) run with interrupts off so
an interrupt on the same CPU cannot send in between. An NMI is not held off:
none sends, and the panic path that may run in one never returns to the send
it interrupted.

## TLB shootdown

A local `invlpg` or CR3 write cleans **this CPU's** TLB only. A mapping torn
down or tightened while another CPU may be running that address space must be
invalidated there too, before the change can be observed or the frame
reused; a stale writable entry over a freed frame is a store into someone
else's memory that faults no one.

**Rule: any unmap, write-protect or permission tightening of a mapping a
running task might use goes through `mem::tlb`, never a bare `invlpg`.**
`python tools/sync/check_tlb.py` (CI) fails on a new bare
`instructions::tlb::flush` outside the audited list.

API (`mem/tlb.rs`):

- `shootdown(va, pages)` — local flush, then every other online CPU. More than
  64 pages flush everything (never truncate).
- `shootdown_all()` — everything (fork, a PAT change).
- `Batch` — for loops: `note(va)` flushes locally as it goes (exactly the old
  per-page `invlpg`) and remembers the span; `finish()` sends **one** round
  trip for the span to the other CPUs. Call it before a frame the pages named
  is freed (`unmap_range` frees at most 32 frames per round trip) and before
  returning to a caller that relies on the new rights. A pending `Batch` that
  is dropped is a bug (debug assertion).

Converted callers: `unmap_range` and `remap_page` (`mem/unmap.rs`),
`protect_range`, `filemap::write_protect` (and through `unmap_from`),
`mem/reclaim.rs` table frees, `mmio::unmap_mmio` and `uncache_phys_map`,
`fbwindow::map`, `wc` (PAT), fork (`clone_user_table`: it downgrades the
parent's leaves in place, so every CPU running a parent thread must drop its
writable entries), and the **COW copy** in `pagefault.rs`.

What stays local, and why. A fault resolve that only installs or *loosens* an
entry (demand-zero, a COW page made writable in place, a shared page made
writable, a read-only parent made writable) leaves a remote CPU at worst with a
stale read-only entry: its next store faults, the CPU drops the entry, and
`arch::spurious_fault` retries the instruction. The COW **copy** is not in that
class — the space drops its reference to the old frame, and a sibling thread
on another CPU with a stale read-only entry would keep *reading* that frame,
unfaulted, after its other owner writes or frees it — so it shoots down before
it releases the frame. (The first draft of the plan exempted all fault paths;
`smp_tlb_cow_copy_reaches_remote_tlbs` is the case that found the difference.)

Sender rules (`arch/smp/shootdown.rs`). Any CPU may send, from any context
but an NMI. A sender takes the machine's **sender lock** (a ticket lock, so
turns are first come, first served) with interrupts off, posts the range to
every other online CPU, rings each doorbell, and waits for every ack (one
calibrated second for all of them, then a panic naming a CPU); the lock is
released after the last ack. A waiter gives up after a second per online
CPU. A mailbox therefore has one writer, and a
request is a generation (`sd_req`, served when `sd_ack` reaches it) that is
never overwritten before it is served (the sender asserts it). A CPU waiting
for the lock waits with interrupts off — the doorbell of the holder's request
to it cannot land — so its wait loop **serves its own mailbox**; the holder
waits only for acks, which every online CPU produces from its handler or
such a loop. Serving is idempotent: a request served from a loop leaves its
doorbell pending, and the handler then finds nothing to do.

**Rule for S8.3 on:** every loop that spins with interrupts off on an online
CPU, waiting for another CPU, calls `smp::serve_pending()` (the entry lock's
spin is the first), or a shootdown sent to that CPU never completes. The
bring-up's waits on the BSP and the suite's rig waits already do.
`serve_pending` also halts the CPU when another one is panicking (the `STOP`
it was sent cannot land either).

## Dying address spaces

`free_user_table` frees page-table frames with no shootdown. That is sound
only while no other CPU has the table in `CR3`: without PCID, a `CR3` write
drops every cached walk of the old table, so a CPU that left a table holds
nothing of it. The rule: **an idle CPU sits on the boot table** (`ap_entry`
idles there, and so does each CPU's idle context, `task::idle`), and a task's
table is freed
only when no task of it runs. `mem::switch_to` publishes each CPU's table in
its area (`Cpu::cr3`, after the write), and `free_user_table` panics if
another online CPU's published table is the one it frees
(`smp::others_on_table`; nothing is read with one CPU online). A CPU switching
*onto* a dying table is a lifecycle bug the check cannot order against; it
catches the one S8.3 can make, a CPU left behind on a table whose tasks
exited.

## The entry lock

`sync::entry` (S8.3a): one ticket lock whose holder is a **CPU**, held while
that CPU executes kernel code, so "only one CPU executes kernel code" stays
true once several run user tasks. `LAZYOS_SMP=1` (`cfg(lazyos_smp)`) builds
it; without it every hook is an empty inline. The boot log says which:
`HW:SMP:ENTRY_LOCK=true|false`.

The rules, applied by every gate through `entry::enter(cs)` (the
interrupted `CS`) and `entry::leave(cs)` (the resumed one):

| Event | This CPU | Effect |
|---|---|---|
| entry from ring 3 | not holding | takes the lock (spins if another CPU has it) |
| entry from ring 0 | holding | inherits the hold |
| entry from ring 0 | not holding, halt mark set | takes the lock, clears the mark |
| resume of ring 3 | holding | drops the lock |
| resume of ring 0 | holding | keeps it |
| anything else | | panic: a gate's discipline is broken |

A context switch carries the hold: the scheduler runs under it, and the task
it resumes, kernel code by construction (every switch is kernel to kernel),
runs under it too until it resumes ring 3. Nothing is released inside the
switch, so a leaving task's stack is never live on this CPU while its slot is
selectable by another. The boot CPU takes the hold in `kernel_main`'s first
lines (the boot is kernel code); it rides into the first task. Kernel
threads, the mux included, run held.

Where the hooks are: `task::schedule` (timer and yield gates: the entry
frame's `CS`, then the returned frame's), `process::gate::syscall_dispatch`
(`int 0x80`), `linux_dispatch_gate` and `linux_syscall_return` (the
`syscall` pair, always ring 3), `exception_dispatch` and
`page_fault_dispatch` (after the suite's fault probes, which an AP takes
holding nothing), and the `irq_gate!`/`paranoid_gate!` macros, which wrap
their target in a generated `locked` function. Gates that never take it:
`irq_gate!(.., window_safe)` (the three IPIs, the spurious vector),
`paranoid_gate!(.., err, unlocked)` (`#DF`), the NMI, and the
interrupt-window tick (`window_tick`, which runs inside a syscall's window,
i.e. on the holder, and is window-safe anyway). An AP that only idles never
holds it.

The fault gates (`exception_dispatch`, `page_fault_dispatch`, the
`paranoid_gate!` wrappers) go through `entry::fault_gate`, which differs in
one case: a ring-0 fault on a CPU that does not hold the lock runs its
handler first, so a fatal fault keeps its own report (error code, `CR2`, the
frame dump) rather than a bare panic, and a fault that is resolved anyway
panics before it resumes kernel code that ran unheld.

`python tools/sync/check_entry.py` (CI) pins all of this: every
`iretq`/`sysretq` outside the suite sits in an audited file, each naked stub
calls only its listed callees and each of those calls the hooks, and every
`window_safe`/`unlocked` gate is listed with its reason. A new stub or
opt-out fails it until it is hooked or justified.

A release masks interrupts first: the stub's code between the callee's
return and its `iretq`/`sysretq` runs unheld, and an interrupt there would
enter as if from held kernel code (the `iretq`/`sysretq` restores the resumed
context's own `IF`). A broken rule's panic names the interrupted RIP.

The lock is a `sync::ticket::TicketLock`, the same interrupts-off ticket lock
the shootdown sender lock uses. A waiter spins with interrupts off, so it
serves its shootdown mailbox (`smp::serve_pending`) and halts if another CPU
panics; after 2 s it panics itself, naming the holder CPU and the gate
(`file:line`) it took the lock in. The lock counts its contended
acquisitions and the longest wait (read by the suite).

Without `LAZYOS_SMP=1` the gate macros emit the bare target and the
callees' hooks are empty inlines: the kernel behaves exactly as before, and
its boot log says `HW:SMP:ENTRY_LOCK=false`.

## Idle, halts and on-CPU marks

S8.3b, still with only the boot CPU running tasks.

**Halting drops the hold.** Kernel code that waits for an interrupt with
interrupts on and holds no lock goes through `sync::entry::halt`: `nap` (so
every wait loop, the idle context and `poll_until`), `exit_cpu`'s halt and
the yielding lock's last resort (`relax::yield_and_halt`). It sets the CPU's
halt mark (`Cpu::entry_halt`), drops the lock, executes `sti; hlt`, and after
`cli` takes the lock back if it does not hold it. The mark is set before the
release, and the release masks interrupts, so nothing interrupts the CPU
between "not holding" and "marked"; `sti`'s shadow lands the first
interrupt on the `hlt`. That interrupt's gate finds the CPU unheld and
marked (the one exception to "a ring-0 entry finds the lock held"), takes
the lock, clears the mark and may switch tasks under that hold. So a halting
task usually resumes already holding (after its interrupt, or picked again
after a switch); an NMI, which never takes the lock, is the case that ends
the `hlt` unheld. A halt on a CPU that does not hold the lock panics: its
release would hand out a turn it never had.

**The idle context.** When nothing is runnable the scheduler resumes its
CPU's idle context (`task::idle`), which loops on `nap` and a preemption
point. Before, it resumed the task that had just parked and let it halt in
its own wait loop: the CPU stayed on that task's stack and page table, so no
other CPU could run it once woken, its slot could not be reclaimed once
dead, and its table could not be freed. The idle context is a slot number,
not a task: the top `MAX_CPUS` slots (`FIRST_IDLE_SLOT..MAX_TASKS`) are
reserved, one per CPU, `claim_slot` stops below them and `limit.task_max` is
capped at `TASK_SLOTS` (960). The table stays empty there, so `current()`
always indexes the per-slot arrays (FPU areas, `NAPPING`, kernel stack tops)
and every path that reads the current task finds an empty slot, which it
already treats as idle: a tick is `IDLE_TICKS`, a wake preempts, no sweep or
interval timer sees it, nothing that walks the table lists it. It runs on
the boot table (`install` loads `mem::boot_table`), on a kernel stack that is
never freed, and is created for the boot CPU by `register_kernel`. Until a
CPU has one (`sched::fallback`) the old fallback, the interrupted task,
still applies.

An interrupt that stops the idle context never switches from inside its
handler: `preempt_point` does nothing there, and the idle loop switches
right after the handler returns. The `iretq` back to the halt turns
interrupts on for an instant, which lets a pending interrupt of lower
priority in first; under the I/O APIC the tick (vector `0x20`) ranks below
every device line, and a device that re-asserts as soon as its driver acks
starved the clock when each of its interrupts switched to the driver with
interrupts still off (`dev_irq_prompt_storm_slow_claimant` found it). A
tick still switches directly: its scheduler entry selects.

**On-CPU marks** (`task::oncpu`). Per slot, the CPU it is current on: set by
`set_current` (`install`, the harness) for the new task and cleared for the
previous one, both under the lock. A task current on another CPU is
running there (ring 3, a syscall, or a nap without the hold), its saved
frame is stale and its stack live, so selection (`pick_next_best`), the
direct handoff (`take_handoff`) and reaping (`family::reapable`, which
`reap_matching` and the reclaim path share) all skip it. With one global
run queue that is the whole answer to "two CPUs on one stack": a parked or
preempted task is current nowhere and may run anywhere, a running or
napping one is current exactly where it runs.

What S8.3b leaves to S8.3c: a task that dies (or whose group is killed)
while current on another CPU is not reapable until that CPU switches away
from it, and nothing yet tells that CPU to (the `RESCHED` IPI) or tells a
parent waiting in `wait4` to look again once it has.

## Panic

`panic` calls `smp::stop_others()` first: `STOP` to every other online CPU,
then a bounded wait (200 ms in total) for each to report `stopped`. It takes no lock and
allocates nothing, so it works from any context; a second CPU that panics while
the first is stopping the world just halts, and so does a CPU spinning with
interrupts off in a shootdown wait (it cannot take the `STOP`). The serial
port's force-unlock and the on-screen report both assume the machine is one
CPU again, which this makes true. Whether any AP is online is read from the
areas themselves, not the online count (which trailed an AP's own `online`
store before the count moved ahead of it) and not `%gs` (unset early in
boot): with no AP online it does nothing, exactly as on a single-CPU machine.

## Tests

`LAZYOS_TEST_FILTER=smp_ python tools/test/run.py --accel none --smp 4`
(vacuous at `-smp 1`, but for the nested NMI): bring-up, resched and
shootdown IPI round trips, a 20000-round shootdown soak, idempotence, `STOP`
plus restart, the clock-skew refusal (a deliberate 1 ms offset), the TLB
cases (`tests/smp_tlb_suite.rs`, `smp_space_suite.rs`) and the robustness
cases (`smp_ipi_suite.rs`): the BSP and two APs sending shootdowns at once
(3000 rounds each, 200000 with `--soak`; every ack counted), nested NMIs
dropped (300 rounds, 20000 with `--soak`), an NMI to every AP, an NMI storm
during an AP's start (from the trampoline's `lidt` on: the instruction before
it is the accepted window) plus two deterministic stops in `ap_entry` before
its IDT load where it must take an NMI through the early table, a CPU that is
never sent its SIPI (timeout, park, retired area, then the area handed back
and the CPU started), the panic path's stop with the online count hidden,
the bring-up's deadline forced right after its accept (both outcomes of the
claim/withdraw race seen, each consistent with the count), a
sparse 201-page span (one round trip, a full flush on the target), a
`protect_range` that must make an AP's next store fault (a fault-recovering
probe, `arch/probe.rs`, test kernels only), and the dying-table check over
100 fresh tables (5000 with `--soak`) with a frame-leak count. Each of the
storm, nested-NMI and concurrent-sender cases was checked to fail without
its fix (the machine resets, the outer NMI never returns, a sender panics
waiting for an ack).
The TLB cases make an AP cache a translation, change the mapping through the
real kernel path, and have the AP read again — a read of the old frame is a
stale entry. The AP runs the probes as jobs in its reschedule IPI
(`smp::run_on`, test kernels only) and stays on the scratch table, because a CR3
write would flush the entry under test. A control case changes a PTE with no
flush and logs how many APs kept the stale translation, so the others can fail.

The entry lock's cases (`tests/smp_entry_suite.rs`, built with
`LAZYOS_SMP=1`; without it one case checks the lock is compiled out): the
hold rules on the boot CPU; every online CPU, the boot CPU included, running
5000 critical sections each (200000 with `--soak`) around a deliberately
non-atomic update, with no update lost, no two CPUs inside at once, and the
contended count proving they really waited; an AP waiting for the lock while
the boot CPU, holding it, sends a shootdown that the waiter must serve from
its spin; and two queued APs getting their turns in queue order, over eight
rounds that alternate which queues first (an unfair lock passes one round
half the time). The cases bring the APs up themselves, so a filtered run is
not vacuous, and every failure path drops and retakes the boot CPU's hold
around the APs it started. Both halves were checked to fail without their
mechanism: with the ticket wait removed a second CPU overwrites the holder
and the release assertion fires, and with `serve_pending` removed from the
spin the boot CPU's shootdown panics waiting for the waiter's ack. The whole
suite running under the lock (`LAZYOS_SMP=1 python tools/test/run.py --accel
none --smp 4`) is the check of the gates' discipline: a gate that enters or
leaves wrongly panics the run.

S8.3b's cases (`tests/smp_idle_suite.rs`): the idle slots are reserved and
empty in the table; the only task sleeping, the CPU runs its idle context on
the boot table with the kernel task current nowhere, and the sleep ends on
time; a task marked current on another CPU is never selected (the idle
context is picked instead), never handed the CPU and not reaped once
finished, and is all three once the mark is cleared; 500 rounds (20000 with
`--soak`) of a ping-pong thread and a 200 µs sleep, each switching into the
idle context and out. With `LAZYOS_SMP=1`: an AP queued for the lock gets it
inside one halt of the boot CPU (its turn's timestamp falls between the
halt's start and end); a ring-0 entry into a marked halt takes the lock and
clears the mark; and every AP taking the lock around a section until told
to stop while the boot CPU, holding it except inside its halts, makes 300
halts (20000 with `--soak`), naps and 100 µs sleeps in turn, each after a
section of its own: no update lost, no overlap, every AP got in.

## Known gaps (carried into S8.3c)

- `limit.cpu_max` stays 1 by default; S8.3c decides whether the MADT count
  becomes the default.
- A task that dies while current on another CPU waits for that CPU to
  switch away before it can be reaped, and a parent in `wait4` is not told
  when it does (S8.3c, with the `RESCHED` IPI on a kill).
- Today no AP takes the entry lock (outside the suite): every vector that
  can reach an AP is window-safe (LAPIC timer masked, I/O APIC and MSI aimed
  at the boot CPU). That is what makes the boot CPU's waits for an AP
  (`finish_on`, a shootdown) safe while it holds the lock; routing a locked
  vector to an AP before S8.3c would deadlock them, caught only by the 2 s
  watchdog. The NMI's `HANG:LOCKS` report does not list the entry lock yet.
- The rules S8.3 must keep, recorded above: every interrupts-off spin calls
  `smp::serve_pending`; the idle task runs on the boot table; nothing sends
  an IPI from an NMI but the panic path.
- The NMI windows accepted above: an NMI latched before the trampoline's
  first instruction, and a dropped nested NMI's report.
