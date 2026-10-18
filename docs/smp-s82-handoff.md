# SMP S8.2 handoff (draft, work in progress)

**Read this before touching `smp-s8.2-ap`.** It is the exact state of the
S8.2 work (AP bring-up, IPIs, TLB shootdown), where it stopped, and what to
do next. The plan of record is [`smp-plan.md`](smp-plan.md) §S8.2; the
per-CPU inventory is [`architecture/smp-audit.md`](architecture/smp-audit.md).

## Branch and PR state

- Branch `smp-s8.2-ap`, head `a2d90db0f` ("SMP S8.2 (WIP)"), pushed to
  origin, **draft PR open against `smp-s8.0-groundwork`** (see the base
  note below).
- Base history: S8.0 (#901) and S8.1 (#902) are merged — but **#902 merged
  into its base branch `smp-s8.0-groundwork` (`ca2fadbb1`), not into
  `main`**, because it was a stacked PR. This branch carries everything
  (it merged `origin/main` at `0c90f6a8a` on top, resolving the two
  same-content-different-squash conflicts in `smp-plan.md` /
  `smp-audit.md` by keeping the branch's superset). **Action for a human:
  merge `smp-s8.0-groundwork` (`ca2fadbb1`) into `main`** — one PR, no
  code, so `main` gets S8.1 and future PRs can base on it. There will be a
  small AGENTS.md conflict (#900's "AMD NUC" → "Wi-Fi" renames happened on
  main after the branch diverged): take main's headings, keep the branch's
  `--smp` line in "Running the demo".

## What exists and builds green (kernel + tests, `LAZYOS_TESTS=1 cargo build`)

| File | Content |
|---|---|
| `kernel/src/arch/lapic.rs` | ICR support: `send()` (xAPIC MMIO with delivery-status poll / x2APIC single MSR write at `0x830`), `send_init`, `send_sipi(dest, vector_page)`, `send_fixed(dest, vector)`; `init_ap_local()` (the AP's own LVTs: timer masked, **LINT0 masked** — the 8259 is the BSP's alone — LINT1 NMI) |
| `kernel/src/arch/percpu.rs` | `Cpu.ipi: IpiState` — `tsc_entry` (the bring-up's coherence stamp), `sd_addr`/`sd_pages`/`sd_ack` (the shootdown mailbox); `online` is now an **`AtomicBool`** (Release publish / Acquire poll) |
| `kernel/src/arch/smp/trampoline.rs` | The 16→32→64-bit blob (`global_asm!`, `.code16/32/64`), position-independent by construction: fixed numeric offsets only, `ds = 0x0800` so every `[offset]` is `0x8000 + offset`, the two mode transitions `push` their `(offset, segment)` from **BSP-patched slots** and `retf` (LLVM IAS refuses symbol arithmetic in immediates — do not try to bring it back), a magic check (`0x5A5A_0001` at `0x0F00`) before anything runs. The temp GDT: 0x08 32-bit code, 0x10 data base `0x8000` (keeps the fixed offsets working in 32-bit mode), 0x18 data base 0 (stack SS), 0x20 64-bit code. Exports `smp_trampoline_start/prot/long_stub/end` |
| `kernel/src/arch/smp.rs` | `apply_config` (`smp.cpus=`, `smp.tsc=force` test-only), the TSC gates (invariance `CPUID.8000_0007.EDX[8]` + per-AP coherence via `Cpu.ipi.tsc_entry`), `prepare()` (page-table build, blob copy + patch), `bring_up(count)` / `bring_up_for_test` (the SDM INIT → 1 tick → SIPI → SIPI sequence, bounded poll on `online`), `ap_entry` (NXE/PAT/`cpu::init`/`linux::init`/`init_ap_local` on the AP, then idle with **interrupts on** — the AP must service IPI mailboxes), the three IPI handlers (window-safe contract), `shootdown_others` (mailbox + doorbell + bounded ack wait → panic on timeout) |
| `kernel/src/mem/tlb.rs` | `shootdown(va, pages)` (local flush + `shootdown_others` when >1 CPU) and `shootdown_all()` (address 0 = the "everything" case in the handler) |
| `kernel/src/mem/{mod,dma,frames}.rs` | The trampoline page (`0x8000`) is a **keep-out at `mem::init`**: out of the DMA pool (`DmaPool::keep_out`) when the pool owns low memory (it does: the pool is carved from the first usable region), else `RESERVED` in the general allocator; accounted in `frames.total`. `frames::reserve_frame` exists but is unused by smp now (the boot keep-out replaced it) |
| `kernel/src/arch/idt.rs` | The three IPI vectors registered: `0x60` RESCHED, `0x61` SHOOTDOWN, `0x62` STOP (audit-pinned free range 0x60..0x7F) |
| `kernel/src/fs/bootcfg.rs` | `smp.` prefix routed to `arch::smp::apply_config` |
| `kernel/src/main.rs` | `arch::smp::init()` right after `arch::init()` (test builds diverge at `tests::run()` before this, so the suite drives `bring_up_for_test` itself) |
| `kernel/src/tests/smp_suite.rs` | Four cases, registered after `percpu_suite`: the real bring-up (`-smp 4` expected: 4 online, per-AP TSC stamp + non-BSP APIC id), the resched self-IPI through the real ICR into an `irq_window_suite::in_syscall` window, the shootdown ack round-trip (vacuous at 1 CPU), idempotence |

## Where it stopped — the current blocker

The suite at `-smp 4` (WHPX) **dies silently at the first INIT-SIPI**: the
serial log ends right after the percpu tests (no panic line, no further
output, the runner reports "booted but never printed TEST:SUMMARY").
Diagnosis so far: an AP **triple fault resets the whole machine** in QEMU
(-no-reboot then exits), which matches the silence exactly — the BSP's
bounded poll never gets to print its `HW:SMP:CPU<n> ... FAIL` line.

A debug run was in flight when work stopped (background shell):
`python tools/test/run.py --accel auto --smp 4 --out shots/kernel-tests-s82d
--extra-arg=-D --extra-arg=shots/qemu-reset.log --extra-arg=-d
--extra-arg=cpu_reset --no-build` — **its first action is to read
`shots/qemu-reset.log`**: the triple-faulting RIP is in there (`CPU Reset`
blocks). Note `--no-build` assumes the image from the previous
`LAZYOS_TESTS=1 cargo build` — if `target/lazyos.img` has moved, rebuild
first (drop `--no-build`).

## What to check, in order (my suspects)

1. **The reset log's RIP.** If it is a low address (`0x8xxx`): the
   trampoline itself — most likely the retf patch slots (are `RETF1_*`'s
   u16s written? the debug line can be dropped from `ap_entry`'s sibling
   `serial_println!` at the blob's start: add one `serial_println!`-free
   check — see next point) or the magic/GDTR patch.
2. **Is the blob even running?** Cheap test: park a `0xB8`-style marker
   write at the very top of the 16-bit code — e.g. have the AP write a
   byte to QEMU's debug port: `mov dx, 0x402; mov al, 'A'; out dx, al`
   (QEMU logs `out` to the debug log with `-d ioport`? simpler: write to
   the `isa-debug-exit` port is destructive; use `-d ioport` and look for
   the write). If nothing arrives, the SIPI never started the AP — check
   the ICR path (x2APIC vs xAPIC: the boot log says XApic; verify
   `send()`'s MMIO writes land — dump the ICR's delivery-status bit).
3. **The AP page table.** `build_ap_page_table` copies PML4 entries
   256..512 from `mem::boot_table()` — verify the boot table's physical
   address is what `read_entry` reads (`phys_to_virt(boot + index*8)`),
   and that `alloc_zeroed_frame` frames are below 4 GiB (they are: the
   allocator's regions are). The huge page needs no CR4.PSE in long mode.
4. **WHPX INIT/SIPI.** If the reset log shows nothing at all, the AP never
   started: try `--accel none` (TCG) — WHPX's INIT-SIPI handling is the
   one part of this that has not been proven on this box; TCG is the
   correctness rig anyway.
5. **`mov esp, 0x7C00` + SS base 0**: the 0x7C00 stack is below the
   trampoline page and inside the identity map ✓. The mailbox stack value
   is a kernel-half VA read with a 64-bit `mov` ✓.

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
  window-safe contract, the TSC gates, the keep-out).
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
