# Linux ABI conformance bench

Tooling that measures how far LazyOS is from running prebuilt
`x86_64-unknown-linux-musl` static binaries. See the wiki **Linux ABI Plan** and
**Linux ABI Compatibility**.

## Pieces

| File | Purpose |
|------|---------|
| `fixtures/` | Small Rust programs (std) built static for `x86_64-unknown-linux-musl`, one per contract. |
| `build.py` | Builds the fixtures into `target/abi/fixtures/*.elf` (adds the musl target if missing; tolerant if the host can't link). |
| `run.py` | For each fixture: embeds it as `/system/bin/abi-init`, boots headless QEMU, parses the serial log, writes `docs/compat/matrix.md` + `compat.json`. |
| `coverage.py` | Scans the bench serial logs for `ENOSYS <nr> <name>` and writes `docs/compat/coverage.md` + `coverage.json`. |

## Fixtures

| Fixture | Contract |
|---------|----------|
| `hello` | Writing to stdout and exiting. |
| `alloc` | Heap allocation and deallocation. |
| `hashmap` | `std::collections` over the allocator. |
| `file` | Open/read/write/stat/rename/unlink through the VFS. |
| `time` | Clocks and sleeping: `Instant`/`SystemTime`, std's `thread::sleep`, and `clock_nanosleep` relative and with `TIMER_ABSTIME` on the monotonic and realtime clocks (each must last its 50 ms, issue #669), plus a past absolute deadline that must not block. |
| `thread` | `std::thread` spawn/join. |
| `syncstress` | Mutex/condvar/channel concurrency soak. |
| `fsstress` | Filesystem create/read/write churn. |
| `memstress` | Allocator growth, `brk`, `mmap`/`mprotect`/`munmap`, `mremap`. |
| `procstress` | `std::process` spawn with pipes, exit status, environment. |
| `sigstress` | Signal masks, handlers and delivery. |
| `fpsig` | XMM/MXCSR state survives asynchronous signals across sibling threads (#809). |
| `avx` | AVX state (#822): `CR4.OSXSAVE` and `XCR0` (x87, SSE, AVX; no AVX-512), VEX instructions, two threads holding 256-bit patterns in YMM0-7 under timer preemption and `tgkill` signals (a handler starts from the initial state and clobbers every YMM register), the signal frame's `XSAVE` area read as libgcc does (`FP_XSTATE_MAGIC1`/`2`, size, upper halves at offset 576, edited by the handler and restored), `fork` copies the upper halves, `execve` resets them. Needs a guest CPU with AVX (`--cpu`); otherwise prints `ABI:avx:INFO` and passes. |
| `threadsig` | Per-thread signal masks and thread-directed signals, JavaScriptCore's stop-the-world pattern (issue #772): 8 threads, each sent `SIGUSR1` with `pthread_kill`; every handler must run on the thread it was aimed at and park on a futex (all eight at once, so one handler's blocked signal must not hold it back from the others) until the collector releases it, 1000 rounds; then a process-directed `SIGUSR2` the main thread blocks must be handled by another thread. |
| `alarm` | `alarm`, `setitimer`/`getitimer` and POSIX timers (issue #535): `alarm(1)` + `pause`, a blocking `read` interrupted (`EINTR`) without and restarted with `SA_RESTART`, a cut-short `nanosleep` with its remainder, an `ITIMER_REAL` interval timer until disarmed, `ITIMER_PROF`/`ITIMER_VIRTUAL` on a busy loop, no alarm in a forked child, an alarm that survives `execve`, then `timer_create` (`SIGEV_SIGNAL` with `si_code == SI_TIMER` and `si_value`, `timer_gettime`/`getoverrun`/`delete`, `SIGEV_THREAD_ID` handled on the aimed thread). Each step prints `ABI:alarm:STEP:<name>`. |
| `epollstress` | `eventfd` + `epoll` level/edge readiness, timeouts, add/mod/del. |
| `msgpoll` | A Messenger endpoint (`ENDPOINT_FD`, `lazyos_sys::msg::Pollable`) and a `UnixStream` in one `epoll` set: each wakes it alone, a message sent by another thread wakes a blocked `epoll_wait(-1)`, a drained endpoint goes quiet, a closed peer reads `EPOLLIN \| EPOLLHUP` (issue #667). Each step prints `ABI:msgpoll:STEP:<name>`. |
| `unixstress` | `UnixStream` pair/EOF/shutdown, pathname bind/connect/accept, `SOCK_SEQPACKET` boundaries. |
| `threadreap` | A child belongs to its parent's process (issue #859): a child forked by the main thread reaped by another thread (status intact, a second `waitpid` is `ECHILD`), a child of a thread that has exited reaped by the main thread, a forked stranger gets `ECHILD`, and `prctl(PR_SET_PDEATHSIG, SIGKILL)` ends a grandchild when its parent exits (seen as EOF on a pipe only the grandchild holds). |
| `seqpollout` | `POLLOUT` on a `SOCK_SEQPACKET` socket (issue #863): WebKit's `sendmsg` -> `EAGAIN` -> `poll(POLLOUT)` -> `sendmsg` loop against a slow reader; every send after a `POLLOUT` must succeed (no spin). |
| `scmrights` | Descriptors and shared memory between processes (issue #771): `SCM_RIGHTS` over a `socketpair` with `sendmsg`/`recvmsg` (a pipe end and a `memfd_create` file to a forked child, `MSG_CMSG_CLOEXEC`, then `MSG_CTRUNC` with the extra descriptors closed), the memfd mapped `MAP_SHARED` on both sides, a `MAP_SHARED \| MAP_ANONYMOUS` mapping the child writes through, and end-of-file on the pipe once every copy is closed. |
| `persist` | A file on the persistent `/data` volume: write, `fsync`, `pwrite`/`pread`, `ftruncate`, append, then (second boot, same disk) the bytes are still there. Two boots, see below. |
| `statxio` | `statx` (path and `AT_EMPTY_PATH`), `preadv`/`pwritev` (positional, hostile count refused), the legacy `getdents`, and `/proc/mounts`, all on `/tmp`. |
| `cwd` | The per-task working directory: `chdir`/`getcwd`, `.`/`..` folding (clamped at `/`), relative create/list/rename/stat, `ENOENT`/`ENOTDIR`, and a child that inherits the directory across `fork` + `execve` without moving its parent. Works in `/tmp`, and in `/data` when a disk is attached (the row then requires both `ABI:cwd:ROUND:` lines). |
| `fsops` | What a file manager and an editor need from `std::fs`: `read_dir` + `file_type` (`d_type`), `symlink_metadata`, `create_dir`, create/write/truncate, `rename` (also over an existing file: the atomic save), `remove_file`, `remove_dir` (refused when non-empty), `remove_dir_all`. Runs on `/tmp`, the FAT root `/` and, with a disk, `/data`; each failing step prints `ABI:fsops:GAP:<base>:<step>:<error>`. |
| `statmiss` | A missing path stats as `ENOENT` through `statx`, `newfstatat`, `stat` and `std::fs::metadata`, absolute and relative, from `/` and from an app-shaped `.../bin` cwd, and a plain name can be created there. The image also carries BusyBox (`WITH_BUSYBOX` in `run.py`), since the bug was a missing name mistaken for a BusyBox applet alias. |
| `compat` | What real programs lean on, end to end through musl: a `signal()` (`SA_RESTART`) handler does not turn a blocked `read` into `EINTR` while `siginterrupt` does, `waitpid` of one specific child among several, `WIFSIGNALED`, a descriptor opened by one thread used and closed by another (`CLONE_FILES`), `select`, `MSG_PEEK`, `flock`, `isatty` on a socket, `getpwuid` (the fabricated `/etc/passwd`), `/proc/self/exe`, `getrlimit` and `sysinfo`. Each step prints `ABI:compat:STEP:<name>`. |
| `filemap` | File-backed `mmap` (issue #535): `MAP_PRIVATE` reads the file and keeps its stores; `MAP_SHARED` stores reach `read`, a second mapping, `msync` and the file; a `write` shows in the mapping; a forked child's shared stores are the parent's; growth reads zeros; `mprotect`/`PROT_NONE`; a read-only descriptor refuses a writable shared mapping; the mapping outlives `close` and `unlink`. |
| `dynlink` | Dynamic linking (issue #535, `dynlink/`, built by `dynlink.py` with zig): a non-PIE program linked against `libdynadd.so`, running on musl's `ld-musl-x86_64.so.1`, `dlopen`s `libplugin.so`, uses TLS of both libraries and a thread, and runs its PIE twin `dynpie` as a child. The image gets the interpreter and libraries at `/system/lib` (`LAZYOS_DYNLINK=1`; `/lib/<name>` is served from there). zig links against a stub `libc.so`, so the runnable loader is read from the `alpine:3.20` Docker image (or `--interpreter FILE`, `LAZYOS_LD_MUSL`); without zig or a source for it the row is `n/a`. |
| `tlsfix` | TLS on the Linux ABI (docs/tls-plan.md §8, its own workspace in `fixtures/tlsfix`, sharing `nettls/crypto`): reports the CPUID/XCR0 bits the RustCrypto SIMD dispatch (`cpufeatures`) reads (LazyOS saves YMM state since #822, so an AVX path may be chosen), then runs in-memory rustls client/server handshakes (TLS 1.3 and 1.2, AES-GCM and ChaCha20-Poly1305, X25519/P-256/P-384, a 256 KiB response each) against a test chain valid 2000..2099, and checks a wrong host name and an unknown CA are refused. No network peer: the networked client is `fetch` (`tools/net/run.py --tls`). |
| `dash`, `sqlite3`, `jq`, `rg` | Real, unmodified programs built from pinned sources by `tools/linuxapps/build.py`, on an image built with `LAZYOS_LINUXAPPS=1`. One boot of the `linuxapps` fixture runs each (dash: loops, functions, `$(...)`, a pipeline into an applet, `&` + `wait`, a script with arguments; sqlite3: create, index, reopen from a second process, `integrity_check`, a recursive CTE; jq: a filter over a pipe, `@csv`; rg: a sorted search of a tree, a 4-thread parallel count, stdin) and prints `ABI:<program>:PASS\|FAIL`; `run.py` judges the four rows from that one log. `n/a` when the programs are not built. |
| `busybox` | Pinned static BusyBox (`tools/abi/busybox.py`), the system shell: the kernel boots it with `sh -c "echo ABI:busybox:PASS; df; mount; ..."`; with a data disk attached the row also requires `/data` in the `df` and `mount` output, and that `cd /data` really moves the kernel's cwd (`pwd -P`, an exec'd `ls` and a relative redirection all see `/data`; after `cd ..` they see `/`). |

## Convention

- A fixture prints `ABI:<name>:PASS` or `ABI:<name>:FAIL:<reason>` and exits
  `0`/`1`.
- The kernel, when it sees an injected `/system/bin/abi-init` but cannot run Linux binaries
  yet, logs `ABI:INIT:SKIP:<reason>`.
- `run.py` classifies each fixture as `pass` / `fail` / `skip` / `not-run` /
  `unavailable` accordingly.

## Two-boot fixtures (`persist`)

A fixture listed in `run.py`'s `TWO_BOOT` runs against a freshly formatted ext2
data disk (`python -m tools.mkdisk`, attached with `qemu_shot.py --data-disk`),
booted twice on the same disk. Boot 1 must print `ABI:persist:WROTE`; boot 2
must print `ABI:persist:PASS`, and a boot 2 that prints `WROTE` again is reported
as "the file written by boot 1 was gone". The fixture tells the boots apart by
whether its file exists, so it takes no arguments. Without the mkdisk tooling
the row is `n/a` rather than failing.

## The `/system/bin/abi-init` hook

`build.rs` embeds the file named by the `LAZYOS_INIT` environment variable as
`/system/bin/abi-init` in the disk image. The kernel, if `/system/bin/abi-init` exists, runs *only* it
(instead of the demo programs) — so a bench run is deterministic. This is how the
runner isolates one fixture per boot.

## Local use

```bash
python tools/abi/build.py                 # build fixtures + fetch/build BusyBox (skips if no musl cc)
python tools/abi/run.py --only hello --at 8
python tools/abi/run.py --jobs 3          # boot three rows side by side (CI); images still build one at a time
python tools/abi/coverage.py
```

`build.py` fetches and builds a pinned static-musl BusyBox (issue #254) when
`musl-gcc` is available, or copies a binary dropped at `tools/abi/busybox`.
On a host that can do neither it reports BusyBox unavailable and still exits 0.

On Windows the fixtures may not link (`cc` missing); the bench then reports them
`unavailable`. CI (Linux) builds and runs them.

## CI

`.github/workflows/abi-compat.yml` builds the fixtures, runs the bench, uploads
`docs/compat/**` and `shots/abi/**`, and publishes `Linux-ABI-Matrix` and
`Linux-ABI-Coverage` to the wiki. Each PR gets a comment with the current matrix.
