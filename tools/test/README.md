# Kernel test harness

A first-class, CI-able unit/soak test suite that runs **inside the kernel**
(issue #62). It complements the end-to-end checks (`tools/screenshot`,
`tools/abi`): it boots LazyOS in headless QEMU, but instead of the demo it runs
a suite in kernel context and classifies the result from serial output.

## Quick start

```bash
# Build the test image (LAZYOS_TESTS=1), boot it headless, parse the results
python tools/test/run.py

# Deterministic CI-style run (no hardware acceleration)
python tools/test/run.py --accel none

# Re-run an image that was already built in test mode
python tools/test/run.py --no-build

# Custom image / output directory / timeout
python tools/test/run.py --image target/lazyos.img --out shots/kernel-tests --timeout 240
```

QEMU is discovered exactly like the screenshot tools (`--qemu`, then `PATH`,
then `C:\Program Files\qemu\...` on Windows).

Outputs:

| Path | Meaning |
|------|---------|
| `docs/test/report.md` | human-readable report (tests, details, soak timing) |
| `docs/test/report.json` | machine-readable report |
| `shots/kernel-tests/serial.log` | raw serial log |

`docs/test/` and `shots/` are generated and git-ignored. The runner exits
non-zero if any test fails, `TEST:SUMMARY` is missing (panic, hang, or a stale
non-test image), or the summary disagrees with the parsed results.

## Test mode

`kernel/build.rs` turns `LAZYOS_TESTS=1` into `cfg(lazyos_tests)`. With the
switch on, `kernel_main` calls `kernel/src/tests.rs::run()` after memory setup
and halts; the normal boot path is compiled out. Without the switch nothing in
the suite is compiled, so normal boots are byte-for-byte unchanged.

Each test prints one line over COM1:

```
TEST:<name>:PASS
TEST:<name>:FAIL:<detail>
TEST:<name>:INFO:<detail>        informational (e.g. soak cycles)
TEST:<name>:PROGRESS:<detail>    informational progress
TEST:SUMMARY:PASS=<n> FAIL=<n>
```

`<detail>` never contains newlines (failure details are flattened).

## What the suite covers

48 tests as of 2026-09-28, grouped by prefix (the authoritative list is `SUITE`
in `kernel/src/tests.rs`; soak tests print `PROGRESS` lines and a cycle-budget
verdict):

| Prefix | Count | What it asserts |
|------|------|-----------------|
| `mem_*` | 3 | zeroed frames round-trip through `phys_to_virt`; COW clone + `mprotect` privatisation; `mem_soak_cow_fork_churn` (500 map/write/clone/fault rounds with bounded live frames) |
| `heap_*`, `slab_*` | 6 | kernel heap integrity; slab reuse after free, live/peak stats, oversized fallback, per-owner accounting, bounded-live soak |
| `task_*` | 10 | kernel task registration; fork + reap churn; thread-exit slot reclaim (#133); futex mismatch; fd table; process tree, pgid/sid inheritance, `setsid`, re-parenting on death; `SIGSTOP`/`SIGCONT` |
| `pipe_*` | 4 | ring wrap round-trips; EOF/`EPIPE`/`O_NONBLOCK`; `dup` + fork + `FD_CLOEXEC`; vfork-style `clone` child |
| `linux_*` | 3 | `mremap` soak; `eventfd` semantics; `AF_UNIX` pathname bind/connect soak |
| `ipc_*` | 13 | handle open/duplicate/close and rights; per-uid handle and buffer quotas; channel cancel wakeups and peer death; ACL default-deny, allow and explicit-deny rules; audit ring wrap; the native `messenger` syscall echo; topic ACL through the syscall gate |
| `block_*`, `fs_*` | 9 | ATA reads the FAT root; VFS cache invalidation; FAT `EROFS`; overlay rename/replace and `ENOSPC` limits; ABI `mkdir`/`rename`/`rmdir` and unlink-while-open; ext2 1/2/4 KiB block sizes over a `FakeDisk`; ext2 rejects corrupt images |

Test-only hooks are behind `cfg(lazyos_tests)` (`task::harness`,
`process::linux::dispatch_for_test`), so the production kernel carries none of
these APIs.

## Adding tests

Add a `fn() -> Result<(), String>` to `kernel/src/tests.rs` and register it in
`SUITE`. Keep the output protocol exact. Every kernel component needs both a
correctness test and a stress/soak test (see `AGENTS.md`); put them next to the
existing names of the same prefix.

## CI

`.github/workflows/kernel-tests.yml` installs QEMU, runs
`python tools/test/run.py --accel none`, appends `docs/test/report.md` to the
job summary, and uploads `docs/test/**` + `shots/kernel-tests/**` as an
artifact.
