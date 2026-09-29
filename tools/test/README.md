# Kernel test harness

A first-class, CI-able unit/soak test suite that runs **inside the kernel**
(issue #62). It complements the end-to-end checks (`tools/screenshot`,
`tools/abi`): it boots LazyOS in headless QEMU, but instead of the demo it runs
a suite in kernel context and classifies the result from serial output.

## Quick start

```bash
# Build the test image (LAZYOS_TESTS=1), boot it headless, parse the results
python tools/test/run.py

# Force TCG (no hardware acceleration)
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
switch on, `kernel_main` calls `kernel::tests::run()` (`kernel/src/tests/mod.rs`)
after memory setup and halts; the normal boot path is compiled out. Without the
switch nothing in the suite is compiled, so normal boots are byte-for-byte
unchanged.

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

189 tests as of 2026-09-28 (21 of them soak/stress tests), grouped by name
prefix; the authoritative list is `SUITE` in `kernel/src/tests/mod.rs`, which
assembles each suite's own `CASES` table from `kernel/src/tests/<suite>.rs` (or
`<suite>/` when a suite outgrew one file), and the runner's report
(`docs/test/report.md`) lists every result:

| Prefix | Count | Area |
|------|------|-----------------|
| `ipc_*` | 51 | handles and rights, channels, transactions, shared buffers and fences, ACL, audit ring, registry, topics, the `messenger` syscall surface |
| `task_*` | 35 | task table, fork/reap and thread reclaim, futex, fd table, process tree and sessions, signals, scheduler classes, table fill and slot recycling (#204) |
| `hardening_*` | 19 | user-pointer validation on every native and Linux syscall (#182) |
| `linux_*`, `pipe_*` | 25 | Linux shim: `mremap`, epoll/eventfd, `AF_UNIX` sockets, pipes, `dup`/`FD_CLOEXEC`, vfork-style `clone` |
| `fs_*`, `block_*` | 21 | VFS caches and permissions, FAT `EROFS`, overlay copy-up/whiteouts/limits, ext2 block sizes and corruption, ATA reads |
| `mem_*`, `slab_*`, `heap_*` | 15 | frames, COW/VMA, `mprotect`, slab classes and owner accounting, kernel heap |
| `quota_*` | 7 | per-uid charges at the handle, buffer, queue and memory choke points |
| `display_*`, `sysinfo_*`, `service_*`, `keyd_*` | 16 | display grant and modifier tracking, syscall 14 snapshots, credential transitions, `SHARE_ONLY` buffers |

Test-only hooks are behind `cfg(lazyos_tests)` (`task::harness`,
`process::linux::dispatch_for_test`), so the production kernel carries none of
these APIs.

## Adding tests

Add a `fn() -> Result<(), String>` to the relevant suite file under
`kernel/src/tests/` and register it in that suite's `CASES` table (it is picked
up automatically through `SUITE` in `kernel/src/tests/mod.rs`). Keep the output
protocol exact. Every kernel component needs both a correctness test and a
stress/soak test (see `AGENTS.md`); put them next to the existing names of the
same prefix.

## CI

`.github/workflows/kernel-tests.yml` installs QEMU, enables KVM on the runner
(`tools/ci/enable_kvm.sh`, best effort), runs `python tools/test/run.py`
(`--accel auto`: KVM when usable, else TCG), appends `docs/test/report.md` to the
job summary, and uploads `docs/test/**` + `shots/kernel-tests/**` as an
artifact.
