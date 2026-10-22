# SMP, as built (S8.0–S8.2)

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
so that step is safe.

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
idles there; S8.3's per-CPU idle task must too), and a task's table is freed
only when no task of it runs. `mem::switch_to` publishes each CPU's table in
its area (`Cpu::cr3`, after the write), and `free_user_table` panics if
another online CPU's published table is the one it frees
(`smp::others_on_table`; nothing is read with one CPU online). A CPU switching
*onto* a dying table is a lifecycle bug the check cannot order against; it
catches the one S8.3 can make, a CPU left behind on a table whose tasks
exited.

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

## Known gaps (carried into S8.3)

- `limit.cpu_max` stays 1 by default; S8.3 decides whether the MADT count
  becomes the default.
- The rules S8.3 must keep, recorded above: every interrupts-off spin calls
  `smp::serve_pending`; the idle task runs on the boot table; nothing sends
  an IPI from an NMI but the panic path.
- The NMI windows accepted above: an NMI latched before the trampoline's
  first instruction, and a dropped nested NMI's report.
