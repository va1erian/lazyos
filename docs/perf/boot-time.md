# Boot time: baseline and improvements

Measured with `tools/bench/boot_time.py` on the desktop image
(`LAZYOS_XUID=1 cargo build`, dev profile: `xuid` + 2x `xdemo` + `dragdemo`),
256 MiB guest, headless QEMU, 5 measured runs after 1 warmup, median (p90 in the
JSON). Host: Windows 11, QEMU with WHPX; TCG for the software-emulated column.
The host was shared with other builds, so treat differences under ~0.05 s as
noise.

```bash
LAZYOS_XUID=1 cargo build
python tools/bench/boot_time.py --image target/lazyos.img --accel whpx,none --runs 5
```

The tool times, from `QEMU spawn`: the first non-black graphics-mode frame (QMP
`screendump` polling), serial milestones (kernel entered, memory, FAT mounted,
tasks spawned, scheduler, `XUID:UP:PASS` = **desktop ready**, `XDEMO:UP:PASS`
x2 = clients ready), and the kernel's `BOOT:PHASE:<name>:tsc=<n>` lines
(`kernel/src/boot_trace.rs`) as guest Mcycle deltas. Note `first_frame` is the
first frame in the kernel's 1280x720 mode, which the bootloader sets just
before the kernel runs, so under WHPX it coincides with `kernel_entered`.

## Result

| Time to | WHPX before | WHPX after | TCG before | TCG after |
|---|---:|---:|---:|---:|
| kernel entered | 1.20 s | 0.54 s | 3.38 s | 1.57 s |
| desktop ready (`XUID:UP:PASS`) | 4.59 s | **0.82 s** | 6.19 s | **4.02 s** |
| clients ready (2x `XDEMO:UP:PASS`) | 4.66 s | 0.90 s | 7.07 s | 4.92 s |

## Where the time went (WHPX, before)

| Phase | Time |
|---|---:|
| QEMU + BIOS + bootloader (reads the 5.1 MB kernel file) | 1.20 s |
| `mem::init` | 0.45 s |
| `fs::init` (block probe, PCI scan, FAT mount) | 0.17 s |
| Loading and spawning 6 ELFs (ATA PIO reads) | 2.63 s |
| xuid first frame | 0.11 s |

## Changes kept (each measured on WHPX, cumulative)

| # | Change | desktop ready | Why it helps |
|---|---|---:|---|
| 0 | baseline | 4.59 s | |
| 1 | Trim the kernel ELF embedded in the image to its loadable part (`build_support/elf_trim.rs`; the full ELF stays in `target/`) | 3.81 s | The bootloader reads the whole kernel over BIOS disk calls; 3.7 MB of it was DWARF. The bootloader finds `.bootloader-config` by *section name*, so the trimmed file keeps a rebuilt section table (dropping it entirely hangs stage 4). |
| 2 | ATA: one `READ SECTORS` per run (up to 128) with `rep insw` (`arch::io::insw_bytes`) | 1.57 s | Every port access is a VM exit. `in` per word was 256 exits per sector; `rep insw` is one string instruction per sector. |
| 3 | FAT: coalesce contiguous clusters into one read, one-entry FAT-sector cache, read each root-directory sector once per listing | 1.00 s | Removes per-cluster FAT sector reads and 16x redundant directory sector reads. |
| 4 | Frame allocator hands out never-used frames lazily (`mem/untouched.rs`) instead of threading all 63k frames onto the free list at boot | 0.82 s (4+5 together) | The old loop touched every page of RAM (host page fault each): 0.45 s -> 0.01 s. |
| 5 | PCI scan follows bridges from bus 0 instead of sweeping 256 buses, once instead of once per id | (see 4) | ~32k config port accesses (VM exits) -> ~100. |

Remaining WHPX floor: ~0.5 s is QEMU start, BIOS, and the bootloader; the rest
is ELF loading/spawn (0.13 s) and xuid's first composite (0.12 s).

## The full desktop: readiness instead of fixed delays (P7.3)

On the `LAZYOS_DESKTOP=1` image the desktop is not up at `XUID:UP:PASS`:
`init` opens LazyShell and the Terminal later. The bench times them too
(`shell_ready` = `SHELL:UP:PASS`, `terminal_ready` = `TERM:UP:PASS`):

```bash
LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=term cargo build   # the Terminal opens only on request
python tools/bench/boot_time.py --image target/lazyos.img --accel whpx --runs 5 \
    --no-clients --need shell_ready,terminal_ready
```

| Time to (WHPX, dev profile, median of 5) | before | after |
|---|---:|---:|
| compositor up (`XUID:UP:PASS`) | 0.53 s | 0.40 s |
| LazyShell up | 1.27 s | 0.87 s |
| Terminal up | 1.87 s | 0.86 s |

Before, `init` opened the shell 50 ticks after it started and the Terminal 40
ticks after that, and a service's dependents started when it was spawned.
Now services announce `init.Ready`, dependents wait for it, and the autostart
opens every built-in app as soon as all boot services are ready
(`user/src/bin/init/ready.rs`, `autostart.rs`). The compositor row differs by
host noise only (nothing before it changed); compare the gaps: compositor to
shell went from 0.74 s to 0.47 s, shell to Terminal from 0.60 s to 0.

Building the native user programs at `opt-level = 2` instead of `"s"` was
measured on the same host in the same hour (7 runs each): LazyShell up 0.91 s
against 0.84 s, the programs 9% larger (5.47 to 5.96 MB; `xuid` 398 to 439
KiB), and `tools/perf/run.py`'s `input_present` p50 111 µs against 117 µs,
within noise. Not a win, so `user` stays at `"s"`.

## Tried and dropped

- `strip = "debuginfo"` for the kernel package: works, but loses line tables
  for panic triage in `target/`; the build-script trim gets the same win.

## Kernel tests

`boot_trace_suite` (TSC ring, soak) and `boot_io_suite` (ATA runs vs single
reads, bounds, FAT over a hand-built fragmented volume plus whole-vs-windowed
reads of the real `HELLO.ELF` with random-range soak, PCI enumeration/priority,
lazy frame cursor and a 3000-frame allocate/free soak). `python tools/test/run.py
--accel none`: 204 pass, 0 fail.

## Known issues found while measuring

- Intermittent: in roughly 1 of 6 runs (both before and after these changes,
  TCG and WHPX) the desktop never reports `XUID:UP:PASS`, or only one
  `XDEMO:UP:PASS` arrives; the boot continues (the `dragdemo` markers appear).
  Likely a startup race between `xuid` and its clients; see
  `user/src/bin/xuid.rs` (UP marker emitted near line 828) and
  `user/src/bin/xdemo.rs` (client start near line 78). Not fixed here.
