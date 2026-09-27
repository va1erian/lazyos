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

`kernel/build.rs` turns `LAZYOS_TESTS=1` into `cfg(laZYOS_TESTS)`. With the
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

| Test | What it asserts |
|------|-----------------|
| `mem_frames_distinct_aligned` | `alloc_frame` returns distinct, 4 KiB-aligned frames outside low memory (public API only; #54 can add counter tests in `mem_suite`) |
| `mem_zeroed_frame_clear` | `alloc_zeroed_frame` is really zeroed and round-trips through `phys_to_virt` |
| `mem_user_table_shares_kernel_half` | `new_user_table` copies PML4 entries 1..512 and leaves entry 0 empty |
| `mem_cow_clone_copies_on_write` | `clone_user_table` marks both sides COW read-only; `cow_fault` gives each side a private, intact copy |
| `mem_soak_cow_fork_churn` | soak: 500 iterations of map/write/clone/fault-both-sides, with progress and a cycle-budget verdict (~8.8e9 cycles under `--accel none`) |
| `heap_vec_integrity` | kernel heap allocation/free/reuse keeps data intact |
| `task_kernel_registered` | kernel task slot, snapshot, no children, nothing to reap |
| `task_block_wake_roundtrip` | the futex park/wake primitives (`set_blocked`/`wake_task`) |
| `task_fork_reap_churn` | `spawn_fork` + exit + `reap_child` bookkeeping across rounds |
| `task_futex_wait_mismatch` | `futex(FUTEX_WAIT)` returns EAGAIN on a mismatched word; `FUTEX_WAKE` with no waiters returns 0 |
| `task_fd_table` | fd open/size/read/seek/dup/close bookkeeping |

Test-only hooks are behind `cfg(laZYOS_TESTS)` (`task::harness`,
`process::linux::dispatch_for_test`), so the production kernel carries none of
these APIs.

## Adding tests

Add a `fn() -> Result<(), String>` to `kernel/src/tests.rs` and register it in
`SUITE`. Keep the output protocol exact. Allocator-specific tests that only
make sense after #54 belong in `mem_suite`, next to the existing names.

## CI

`.github/workflows/kernel-tests.yml` installs QEMU, runs
`python tools/test/run.py --accel none`, appends `docs/test/report.md` to the
job summary, and uploads `docs/test/**` + `shots/kernel-tests/**` as an
artifact.
