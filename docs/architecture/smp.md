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
| `0x0F00` | the mailbox: magic `0x5A5A_0001`, `CR3`, `ap_entry`, the AP's `Cpu` area, its stack top |

The AP runs on a dedicated page table (identity-mapped low 2 MiB over the
machine-wide kernel half) until `ap_entry` switches to the boot table.
Traps for the next person: `EFER.NXE` must be on before paging (the kernel
half uses NX); the xAPIC ICR destination is `ICR_HI` bits 24..32 (a bare id
there is destination 0 and an INIT resets the BSP); long mode ignores `DS`'s
base; LLVM's `retf` under `.code16` is a 32-bit return (`.byte 0xCB`); the AP
starts with `CR0.CD/NW` as INIT left them.

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

## IPIs

Vectors `0x60..0x62`, in the free range between MSI (`0x40..0x5F`) and the
software gates (`0x80`, `0x81`); `0x60..0x7F` is reserved for kernel IPIs.

| Vector | Name | Handler |
|---|---|---|
| `0x60` | `RESCHED` | sets this CPU's `need_resched` |
| `0x61` | `SHOOTDOWN` | invalidates the range in this CPU's mailbox, acks |
| `0x62` | `STOP` | marks the CPU `stopped` and halts with interrupts off |

The handlers obey the `irq_window` contract: no lock but the per-CPU area, no
allocation, no task switch. They sit behind the same naked gates as every other
interrupt (`arch/gsgate.rs`).

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

Sender rules. Today only the BSP sends (the APs run nothing but the three
handlers), so one mailbox per target is never contended. S8.3 must add a
sender lock or an ack-wait that services its own mailbox: two senders, or a
sender spinning with interrupts off while it is itself a target, deadlock. The
ack wait is a calibrated second, then a panic naming the CPU.

## Panic

`panic` calls `smp::stop_others()` first: `STOP` to every other online CPU,
then a bounded wait (200 ms in total) for each to report `stopped`. It takes no lock and
allocates nothing, so it works from any context; a second CPU that panics while
the first is stopping the world just halts. The serial port's force-unlock and
the on-screen report both assume the machine is one CPU again, which this makes
true.

## Tests

`LAZYOS_TEST_FILTER=smp_ python tools/test/run.py --accel none --smp 4`
(vacuous at `-smp 1`): bring-up, resched and shootdown IPI round trips, a
20000-round shootdown soak, idempotence, `STOP` plus restart, the clock-skew
refusal (a deliberate 1 ms offset), and the TLB cases (`tests/smp_tlb_suite.rs`).
The TLB cases make an AP cache a translation, change the mapping through the
real kernel path, and have the AP read again — a read of the old frame is a
stale entry. The AP runs the probes as jobs in its reschedule IPI
(`smp::run_on`, test kernels only) and stays on the scratch table, because a CR3
write would flush the entry under test. A control case changes a PTE with no
flush and logs how many APs kept the stale translation, so the others can fail.

## Known gaps (must close before S8.3)

- One shootdown sender at a time (above); the two-write xAPIC ICR send needs
  the same serialization.
- Nested NMIs on the IST stack, and an NMI between SIPI and `ap_entry`'s IDT
  load, are not handled.
- An idle AP must never sit on a dying address space's CR3 while its page
  tables are freed: S8.3's per-CPU idle task runs on the kernel table. `exit`
  frees page-table frames without a shootdown, which is sound only because no
  other CPU is on that table.
- `limit.cpu_max` stays 1 by default; S8.3 decides whether the MADT count
  becomes the default.
