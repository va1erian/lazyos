# Kernel, filesystem and conformance

Companion to [`AGENTS.md`](../../AGENTS.md); covers: Resource limits, the kernel test harness, ext2 and its journal, the Linux ABI bench and real Linux programs.

## Resource limits and guest memory

Every launcher boots QEMU with **1 GiB** of RAM by default (`DEFAULT_MEMORY`
in `tools/screenshot/qemu_qmp.py`; `--memory 4G` on any of them, including
the abi/usb/shutdown/rhai runners and the GUI's Memory field). The kernel's
tunable ceilings live in one module, `kernel/src/limits.rs`
([`docs/architecture/limits.md`](../architecture/limits.md)): defaults are
derived from usable RAM and the screen size, and `limit.<key>=<value>` lines
in `/boot/lazyos.cfg` override them at boot (clamped, logged, never fatal).
The build writes them from `LAZYOS_LIMIT_<KEY>` variables:

```bash
LAZYOS_LIMIT_HEAP_MAX=768M LAZYOS_LIMIT_FD_MAX=4096 cargo build
python tools/run_demo.py --limit heap_max=768M --limit stack_size=16M
```

Keys: `heap_max` (kernel heap ceiling; the heap grows on demand), `fd_max`
(descriptors per task, 8192), `stack_size` (Linux main stack, 8 MiB,
demand-zero), `quota_user_memory`, `quota_kernel_memory`, `shared_buffer_max`,
`scratch_max` (`/transient` and `/tmp`, one ramfs in the kernel heap; 1 GiB, at most half of
`heap_max`; a LazyRAD `.lzp` staged there is several MiB), `task_max` (task
slots in use, RAM in MiB between 256 and the table's 1024). The boot log prints
the table (`limits: ...`). `task::MAX_TASKS` (1024, the table's capacity) stays a
compile-time constant; `python tools/run_demo.py --big-app` is the profile for
one browser-class program ([`docs/architecture/limits.md`](../architecture/limits.md)). The kernel image runs at `0xffff_8000_0000_0000`
(`mem::layout`): symbolize with `addr2line -e <kernel> <rip - 0xffff800000000000>`.

## Linux ABI conformance bench

Compatibility with Linux (`x86_64-unknown-linux-musl`) binaries is tracked by a
bench that runs from day one, before any ABI support exists:

```bash
python tools/abi/build.py            # build the static musl fixtures
python tools/abi/run.py --at 8       # run each fixture in headless QEMU, write the matrix
python tools/abi/coverage.py         # summarise ENOSYS syscalls from the logs
```

`run.py` embeds one fixture as `/system/bin/abi-init` (via `LAZYOS_INIT`), boots, and
classifies it from the serial log (`ABI:<name>:PASS|FAIL`, or `ABI:INIT:SKIP`).
It writes `docs/compat/matrix.md` (+ `compat.json`). CI
(`.github/workflows/abi-compat.yml`) runs the bench, uploads the matrix and
coverage (the docs site publishes them, see "Documentation site"), and comments
them on PRs. See `tools/abi/README.md` and `docs/linux-abi-plan.md`.

The Linux loader runs dynamically linked musl programs (`PT_INTERP`; issue
#535): the interpreter is looked up as stored, else in `/system/lib`
(`fhs::SYSTEM_LIB`, which also serves `/lib/<name>` and `/usr/lib/<name>`),
and `mmap(fd)` of a regular file is demand-faulted from a per-inode page
cache (`kernel/src/fs/openfile/pagecache.rs`, `kernel/src/mem/filemap.rs`).
`python tools/abi/dynlink.py` builds the `dynlink` row's program, PIE twin and
libraries with zig and reads musl's `ld-musl-x86_64.so.1` out of the
`alpine:3.20` Docker image (`--interpreter FILE` or `LAZYOS_LD_MUSL` name
another); `LAZYOS_DYNLINK=1` embeds them (`tools/abi/run.py` does).

## Kernel test harness

The in-kernel unit/soak suite (issue #62) runs instead of the normal boot when
the image is built with `LAZYOS_TESTS=1`, and prints one machine-parseable line
per test over serial (`TEST:<name>:PASS|FAIL:<detail>`, ending with
`TEST:SUMMARY:PASS=<n> FAIL=<n>`). One command builds, boots headless, parses
and reports:

```bash
python tools/test/run.py                 # build + run; writes docs/test/report.md
python tools/test/run.py --accel none    # force TCG (CI uses auto: KVM when usable)
python tools/test/run.py --no-build      # re-run the current image
python tools/test/run.py --ide-disk      # boot from IDE (ATA) instead of the default virtio-blk
```

While working on one subsystem, `LAZYOS_TEST_FILTER=<text> python tools/test/run.py
--accel none` runs only the tests whose name contains the text (touch
`kernel/src/main.rs` after changing the filter: the build does not notice an
environment variable on its own). Run the whole suite before you finish.
The runner exits non-zero on any failure, a missing summary, or a stale
non-test image. Normal boots are unaffected: without `LAZYOS_TESTS=1` the suite
is not compiled. Test-only hooks live behind `cfg(lazyos_tests)`; add new tests
under `kernel/src/tests/` (`mem_suite` is where allocator-specific tests go). CI
is `.github/workflows/kernel-tests.yml`; see `tools/test/README.md`.

## ext2 library

The ext2 driver is `libs/ext2fs` (`no_std` + `alloc`, depends only on `spin`), shared by
the kernel (`kernel/src/fs/ext2.rs` and `ext2/fsimpl.rs` are the adapter) and the host
image build. It also holds the formatter and populator (`format`, `mkdir_p`,
`write_file`, `remove_tree`). It is tested on the host over an in-memory `BlockIo`,
with an independent fsck-style checker (`src/check.rs`), a 1k-cycle soak and a seeded
fuzz entry point shared with the cargo-fuzz target (`fuzz/fuzz_targets/ext2fs.rs`):

```bash
cargo test -p ext2fs
FUZZ_CASES=30000 cargo test -p ext2fs seeded     # a longer seeded soak
FUZZ_SEED=0x<seed> cargo test -p ext2fs <test>   # replay a printed failing seed
python fuzz/gen_corpus.py --check                # the checked-in seeds are current
```

The kernel's `ext2_suite` still runs unchanged against the adapter, and
`mount_library_formatted_root` mounts a library-made image at `/` through `lazyos.cfg`.

Real mounts (and the host build) go through the library's write-back block cache
([`docs/architecture/block-cache.md`](../architecture/block-cache.md)): writes reach
the disk at a commit (sync, fsync, every 5 s, memory pressure), in a crash-safe phase
order. `Ext2::open` stays uncached for tests that judge the disk after every write;
anything that needs one step on disk before the next inside an operation calls
`Ext2::barrier`. `cargo test -p ext2fs cache` covers it (crash prefixes, byte-identical
images), the kernel side is `LAZYOS_TEST_FILTER=bcache`, and
`cargo test -p ext2fs --release bench -- --ignored --nocapture` prints the I/O cost of a
30 MB tree. `LAZYOS_BLOCK_CACHE_KB=0` builds a kernel that mounts uncached.
A session's `quit` kills QEMU without a sync, so a session whose files a later
boot reads (`lazyrad_home.json` before `lazyrad_home_project.json`) waits a few
seconds past the flusher's 5 s before it quits.

## ext2 journal (`LAZYOS_JOURNAL=1`)

The OS volume can carry an internal JBD2 journal ([`docs/architecture/journal.md`](../architecture/journal.md)):
metadata commits are atomic transactions, replayed at mount, so a power cut
needs no repair. `python tools/run_demo.py --journal` (or the launcher's
Advanced tab) builds it in; an existing image gets one on the next in-place
update. Host tests: `cargo test -p ext2fs journal` (a power cut at every write,
then the independent checker must find nothing) and
`cargo test -p build-support-tests journal`.

## Real Linux programs (`LAZYOS_LINUXAPPS=1`)

`python tools/linuxapps/build.py` builds unmodified dash, sqlite3, jq and
ripgrep from pinned, hash-checked sources (zig for C) into
`target/linuxapps/bin`; `LAZYOS_LINUXAPPS=1` (`run_demo.py --linuxapps`) puts
them in `/system/bin`. The ABI bench runs them (`tools/abi/run.py --only
dash,sqlite3,jq,rg`), and `linuxapps_console.json`/`linuxapps_desktop.json`
drive them interactively (the desktop Terminal runs its shell on a pty, so
`vi`, `less`, `^C` and cooked-mode REPLs work). See `tools/linuxapps/README.md`.
