# SMP S8.2 handoff (draft, work in progress)

**Read this before touching `smp-s8.2-ap`.** It is the exact state of the
S8.2 work (AP bring-up, IPIs, TLB shootdown), where it stopped, and what to
do next. The plan of record is [`smp-plan.md`](smp-plan.md) §S8.2; the
per-CPU inventory is [`architecture/smp-audit.md`](architecture/smp-audit.md).

## Branch and PR state

- **`claude/smp-s8.2-ap-fixes`** (2026-10-21): S8.1 (#902's squash) and the
  S8.2 commits replayed on `main` (the S8.0 duplicates and the old merge of
  `main` dropped), plus the bring-up fixes below. It supersedes
  `smp-s8.2-ap` / draft #905, whose base `smp-s8.0-groundwork` holds S8.1
  only because #902 merged there after #901 had been squashed into `main`.
  A PR of this branch against `main` lands S8.1 and S8.2 together.

## What exists and builds green (kernel + tests, `LAZYOS_TESTS=1 cargo build`)

| File | Content |
|---|---|
| `kernel/src/arch/lapic.rs` | ICR support: `send()` (xAPIC MMIO with delivery-status poll / x2APIC single MSR write at `0x830`), `send_init`, `send_sipi(dest, vector_page)`, `send_fixed(dest, vector)`; `init_ap_local()` (the AP's own LVTs: timer masked, **LINT0 masked** — the 8259 is the BSP's alone — LINT1 NMI) |
| `kernel/src/arch/percpu.rs` | `Cpu.ipi: IpiState` — `tsc_entry` (stamped just before `online`; the bring-up logs the latency, not a skew), `sd_addr`/`sd_pages`/`sd_ack` (the shootdown mailbox); `online` is now an **`AtomicBool`** (Release publish / Acquire poll) |
| `kernel/src/arch/smp/trampoline.rs` | The 16→32→64-bit blob (`global_asm!`, `.code16/32/64`), position-independent by construction: fixed numeric offsets only, `ds = 0x0800` so every `[offset]` is `0x8000 + offset`, the two mode transitions `push` their `(offset, segment)` from **BSP-patched slots** and `retf` (LLVM IAS refuses symbol arithmetic in immediates — do not try to bring it back), a magic check (`0x5A5A_0001` at `0x0F00`) before anything runs. The temp GDT: 0x08 32-bit code, 0x10 data base `0x8000` (keeps the fixed offsets working in 32-bit mode), 0x18 data base 0 (stack SS), 0x20 64-bit code. Exports `smp_trampoline_start/prot/long_stub/end` |
| `kernel/src/arch/smp.rs` | `apply_config` (`smp.cpus=`, `smp.tsc=force` test-only), the TSC invariance gate (`CPUID.8000_0007.EDX[8]`; the plan's per-AP skew gate is not built yet), `prepare()` (page-table build, blob copy + patch), `bring_up(count)` / `bring_up_for_test` (the SDM INIT → 1 tick → SIPI → SIPI sequence, bounded poll on `online`), `ap_entry` (NXE/PAT/`cpu::init`/`linux::init`/`init_ap_local` on the AP, then idle with **interrupts on** — the AP must service IPI mailboxes), the three IPI handlers (window-safe contract), `shootdown_others` (mailbox + doorbell + bounded ack wait → panic on timeout) |
| `kernel/src/mem/tlb.rs` | `shootdown(va, pages)` (local flush + `shootdown_others` when >1 CPU) and `shootdown_all()` (address 0 = the "everything" case in the handler) |
| `kernel/src/mem/mod.rs` | The trampoline page (`0x8000`): the frame allocator never hands out anything below 1 MiB (`LOWEST_FRAME`), so nothing competes for it; `mem::init` records what the boot memory map says of it (`trampoline_page_kind`) and `smp::prepare` refuses SMP unless it is wholly usable RAM. The real-mode stack lives inside the page (`0x8D00`), so no other low page is touched |
| `kernel/src/arch/idt.rs` | The three IPI vectors registered: `0x60` RESCHED, `0x61` SHOOTDOWN, `0x62` STOP (audit-pinned free range 0x60..0x7F) |
| `kernel/src/fs/bootcfg.rs` | `smp.` prefix routed to `arch::smp::apply_config` |
| `kernel/src/main.rs` | `arch::smp::init()` right after `arch::init()` (test builds diverge at `tests::run()` before this, so the suite drives `bring_up_for_test` itself) |
| `kernel/src/tests/smp_suite.rs` | Four cases, registered after `percpu_suite`: the real bring-up (`-smp 4` expected: 4 online, per-AP TSC stamp + non-BSP APIC id), the resched self-IPI through the real ICR into an `irq_window_suite::in_syscall` window, the shootdown ack round-trip (vacuous at 1 CPU), idempotence |

## The bring-up blocker — fixed (2026-10-21)

The APs now come up: `LAZYOS_TEST_FILTER=smp_ python tools/test/run.py
--accel none --smp 4` prints `HW:SMP:CPU1..3 ... online` and passes all four
`smp_` cases, with no exception logged on any CPU (`-d int,cpu_reset`). There
was not one bug but nine, each found from QEMU's `-d int` log in turn:

1. **xAPIC ICR destination** (`lapic::send`): the APIC id went into
   `ICR_HI` bits 0..8 instead of 24..32, so every INIT targeted APIC 0 and
   reset the **BSP** (the "triple fault" was the BSP restarting firmware).
2. **GDTR base** patched to `0x8000 + GDTR_OFF` (the GDTR itself) instead of
   `GDT_OFF`.
3. **The 16-bit `retf`**: LLVM's integrated assembler emits `retf` under
   `.code16` as `66 CB` (a 32-bit far return), which pops two dwords and loads
   a junk CS. Now `.byte 0xCB`.
4. **The 32-bit `retf` operands** were pushed in the wrong order (CS on top).
5. **The 64-bit stub's mailbox reads** used the ds-relative `[0x0F..]`
   offsets; long mode ignores DS's base, so they read the BDA. Now `[0x8F..]`.
6. **`EFER.NXE`** was off when paging came on (the kernel half's NX bits are
   reserved then: #PF on the first push). Set with LME in the trampoline; the
   old `ap_entry` constant also named bit 63 (reserved) instead of bit 11.
7. **Stack alignment**: `jmp` into `ap_entry` left RSP 16-aligned, so the
   compiler's 16-aligned slots were off by 8 (`fxsave64` #GP in `cpu::init`).
   Now `and rsp, -16; call`.
8. **The AP's area** never had its `self_ptr`/`index` (`percpu::prepare_ap`
   now writes them before the SIPI), and **`ap_entry` never loaded its GDT,
   TSS or IDT** (now `gdt::init_cpu`, `percpu::set_gs_base` — the selector
   loads zero the GS base — and `idt::load_on_this_cpu`).
9. **Idempotence**: `bring_up` re-sent INIT to APIC ids already online
   (resetting a running AP). It now skips them, and parks (INITs) an AP that
   times out so its area can be reused safely. The INIT-to-SIPI wait is now
   the SDM's 10 ms (it was 100 us: `cycles_per_tick` is a 10 ms tick).

The resched case now stages its window through `irq_window_suite::in_syscall`
(a bare `irq_window::open()` outside a syscall span opens nothing).

## The `%gs` hole — fixed (2026-10-21)

S8.1's "no `swapgs`" design was exploitable: ring 3 can load any DPL-3
selector into `%gs` (base 0), and the `syscall` stub then wrote the user
context to `gs:`-relative *user* addresses and loaded RSP from there. The
pre-fix kernel, given `gs_suite`'s ring-3 task, takes a ring-0 page fault
writing `0xb0` with its frame on the user's own stack page. Now
`arch/gsgate.rs` holds the `swapgs` discipline and its invariant: maskable
interrupts and `int` gates swap on a ring-3 `CS` (entry, and the resumed
frame at exit), exceptions/NMI/`#DF` take a paranoid entry (`IA32_GS_BASE`'s
sign bit), and `syscall` swaps first and right before `sysretq`/`iretq`.
Every `x86-interrupt` handler became an `extern "C"` function behind a
generated naked gate (the compiler's `iretq` has no room for the exit swap);
the `abi_x86_interrupt` feature is gone. **A new ring-3-reachable vector must
use `irq_gate!`/`paranoid_gate!`** (or splice the same tests into a naked
stub), and **`CR4.FSGSBASE` must stay off.**

## Pre-PR review (2026-10-21) — fixed, and what was declined

Two fresh-context reviewers read the branch (one on the entry gates, one on
the per-CPU areas and the bring-up). Fixed:

- **NMI on its own IST** (`gdt::NMI_IST`, `Cpu::nmi_stack`): an NMI could
  land between `syscall`'s `swapgs` and the stack switch, or between
  `pop rsp` and `sysretq`, and run ring 0 on a user-chosen RSP (pre-existing,
  but the new docs claimed the window covered).
- **Exception stubs halt on a swap-window fault** (`gs_entry_paranoid!`,
  `gs_window_fault`): their entry (GS base) and exit (resumed `CS`) rules
  disagree exactly there, and resuming would leave ring 3 on the kernel's base.
- **`cli` before every exit `swapgs`**, so "IF is clear in every swap window"
  holds by construction; **`CR4.FSGSBASE` forced off** on every CPU.
- **AP CR0**: the trampoline loads `PE|MP|ET|NE` outright and `cpu::init`
  clears `CD`/`NW` and sets `NE` (INIT leaves the caches off; TCG hid it).
- **Bring-up timeout** (`bound` double-counted `started`), a refusal without a
  calibrated TSC, only MADT entries with the *enabled* flag
  (`Madt::startable`; online-capable-only sockets would each wait out a
  timeout), xAPIC ids above 0xFF skipped.
- **AP local APIC** put in the BSP's mode (`LAZYOS_X2APIC=1` would #GP).
- **Trampoline page** checked against the boot memory map (the old keep-out
  was dead code: nothing is allocated below 1 MiB anyway); stack moved into it.
- **Timed-out areas retired**, `gdt::init_cpu` starts from a fresh table, the
  AP's and the STOP handler's lines go through `RawSerial` (no lock).
- **Shootdowns never truncate**: above 64 pages a target flushes everything;
  the ack timeout is a calibrated second.
- **`AREAS` back in `.bss`** (the `GlobalDescriptorTable::new()` initializer
  had moved 3.3 MiB into `.data`).
- **Tests**: `gs_suite` now checks the GS *base* from ring 3 every round,
  crosses `int3`'s paranoid gate, proves the timer interrupted the task, runs
  six tasks at once, and tests the paranoid in-window branch from ring 0;
  `smp_ipi_soak_shootdowns_and_rescheds` is 20000 shootdown round trips
  (short and full-flush ranges) with reschedule IPIs.

Declined for this PR, recorded as **must-fix before S8.3**:

- `percpu::current()`/`area()` return `&'static mut Cpu`: two live `&mut` to
  one area exist today (formally UB, no miscompile seen), and S8.3's remote
  wakes would make it a cross-CPU alias. Return `&'static Cpu` and reach the
  few non-atomic fields (TSS, GDT, syscall entry) through raw accessors.
- One shootdown sender at a time: two CPUs writing one target's mailbox, or a
  sender spinning with IF clear while it is itself a target, deadlock. Not
  reachable while APs never send; S8.3 needs a sender lock or an ack spin
  that services its own mailbox. The two-write xAPIC ICR send needs the same.
- Nested NMIs on the IST stack (an exception inside the NMI handler whose
  `iretq` unblocks NMIs would let a second NMI reuse the stack), and an NMI
  between SIPI and `ap_entry`'s IDT load triple-faults. Neither happens in
  normal operation; Linux-style nesting detection is future work.
- The plan's TSC skew gate (a two-way exchange) and the S8.2 acceptance items
  still open: a test that another CPU really faults after a shootdown, the
  STOP vector, failed-AP tolerance, the kernel-half tripwire, and the caller
  conversions above.
- `limit.cpu_max` stays 1 by default (APs parked on a normal boot); S8.3
  decides whether the MADT count becomes the default.

## After the APs come up

- The two failing suite cases should flip green (the resched one was
  already re-staged through `irq_window_suite::in_syscall` but never
  reached while the bring-up died).
- **Not done yet (the plan's S8.2 list):** the shootdown **caller
  conversions** — the mechanical list in `smp-audit.md`'s S8.2 row
  (`grep -rn "tlb::flush" kernel/src` + the flush-by-CR3-write sites:
  `mem/cow.rs` fork!, `protect.rs`'s `filemap::write_protect`/`unmap_from`,
  `reclaim.rs`, `filemap.rs` + page-cache eviction, `uspace.rs`
  unmap/remap) routed through `mem::tlb::shootdown`; the fault paths stay
  exempt (recorded). With the APs idle this is behaviour-neutral (the IPIs
  are a few µs), so do it in this PR once the bring-up is green.
- The docs: mark the S8.2 rows done in `smp-audit.md`, the §S8.2 status in
  `smp-plan.md`, and write the as-built `docs/architecture/smp.md` (the
  trampoline page layout, the mailbox, the IPI vectors and their
  window-safe contract, the TSC gate, the low-page check).
- The end-to-end gate: a desktop `core_apps` session at `-smp 4` with the
  APs online-idle (build with `LAZYOS_DESKTOP=1 LAZYOS_UI_PROBE=1
  LAZYOS_XUI_AUTOSTART=term LAZYOS_RESET_OS=1`; BusyBox and rhai must be
  built first — this worktree starts without them:
  `python tools/abi/busybox.py && python tools/rhai/build.py`; the boot
  log should show `HW:SMP:CPU<n> ... online` lines).
- Then: fresh-context review (per AGENTS.md's pre-PR rule), un-draft, and
  the CI green set.

## Evidence so far

- Suite at `-smp 4` **before** the smp module existed: 1568/0 (twice).
- Suite with the smp module: dies at the bring-up (above); the three
  non-bring-up cases passed vacuously in the first attempt.
- The desktop `core_apps` at `-smp 4` (APs never online — S8.1 code):
  green (`TERM:UP:PASS` 3.4 s, all eight `INIT:LAUNCH:PASS`).
- Boot facts (this box): `lapic: XApic id=0x0 virtual wire`,
  `MADT ... cpus=4 apic_ids=[0,1,2,3]`, WHPX accel, PIT tick.
