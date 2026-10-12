# A WebKit port — exploration and plan

Status: **W0 built** (2026-10-12): the four kernel prerequisites landed
(issues #771 to #774, this plan's §4), so nothing in the kernel stops a
WebKit2 port any more; W1 (the toolchain proof) is not started. Decisions
taken: WebKit2 is the engine (§2), the port is modeled on the PlayStation
port with WebKit's C API as the seam to a Rust xui window (§5), and the port
files live out of tree like `doom/` and `quake/` (§7).

Scope: what it would take to run a mainstream browser engine on LazyOS, and
the shortest credible path to a page rendered by WebKit in a desktop window.
LazyWeb ([lazyweb.md](lazyweb.md)), the Blitz browser with QuickJS, stays the
shipped browser throughout; this is the long road beside it, not a
replacement. Gecko and Blink are out of scope for the reasons in §2.

Related: [lazyweb.md](lazyweb.md), [linux-abi-plan.md](linux-abi-plan.md),
[architecture/limits.md](architecture/limits.md),
[doom-port-plan.md](doom-port-plan.md), [quake-port-plan.md](quake-port-plan.md),
[tls-plan.md](tls-plan.md).

## 1. Why ask

A browser engine is the largest piece of ordinary software there is, and it
exercises everything an OS offers: processes and threads, shared memory,
signals, a JIT's memory tricks, fonts, images, sockets and TLS, a window.
Asking what a WebKit port needs is a way of asking what LazyOS still lacks
for *any* big Linux program, and the answer (§4) turned out to be short and
worth doing on its own.

## 2. Which engine

**WebKit** is the only realistic candidate. It is built as a core plus a
port layer per platform, and one-person or hobby-OS ports exist and work:
Haiku has carried its own for over a decade, MorphOS has Wayfarer from a
single developer, AmigaOS has Odyssey, and Sony's PlayStation port runs on a
non-Linux OS with a generic run loop and curl networking. Those ports are
the template; the PlayStation one is the closest (no GLib, no X, no
Wayland).

**Gecko** has no port layer in that sense: it is welded to XPCOM, NSPR, the
`mach` build and its Tier-1 platforms, embedding was dropped years ago, and
the last hobby-OS port (BeZilla on BeOS) died around Firefox 2. **Blink** is
30 million lines around Mojo IPC and a sandbox that assume Linux. Neither is
a few months of work for anyone outside the project that owns it.

The engine is **WebKit2**, the multi-process architecture every current port
uses (a UI process, a WebProcess per page group, a NetworkProcess). There is
no single-process mode left in the Linux ports, so the IPC layer is the
first thing a port has to satisfy (§4.1).

## 3. What LazyOS already has

* **A Linux ABI for static musl binaries** with threads, futexes, fork and
  exec, `mmap`/`mprotect`, epoll, Unix sockets, signals
  ([linux-abi-plan.md](linux-abi-plan.md)). Real programs (dash, sqlite3,
  jq, ripgrep, BusyBox) run unmodified.
* **A C and C++ toolchain**: zig builds static musl programs for the Linux
  apps, Doom's C and SQLite; `zig c++` ships libc++.
* **A pattern for a foreign engine in a desktop window**: Doom and Quake
  link a C engine, compiled by zig, into a Rust `xui-app` client that owns
  the window, input and sound ([doom-port-plan.md](doom-port-plan.md)).
* **HTTPS**: rustls over an in-tree pure-Rust provider, the Mozilla roots in
  the image ([tls-plan.md](tls-plan.md)); fonts: the Liberation family under
  `/system/share`, registered by every Blitz app.
* **Memory and tasks sized by boot-time limits**
  ([architecture/limits.md](architecture/limits.md)).

## 4. The prerequisites (W0, built)

The assessment found four gaps. All four are kernel work, all four unblock
ordinary Linux software beyond a browser, and all four landed together
(PR for issues #771 to #774).

### 4.1 Descriptor passing and shared memory (#771)

WebKit's Unix connection sends shared-memory descriptors over `SCM_RIGHTS`,
and `memfd_create` is where those descriptors come from. Both were missing.
Now: `sendmsg`/`recvmsg` carry descriptors on `AF_UNIX` stream and seqpacket
sockets with Linux's rules (install on receive, `MSG_CMSG_CLOEXEC`,
`MSG_CTRUNC`, 253 a message, in-flight descriptors charged to the sender and
capped globally, a read never mixes bytes of a message that carried
descriptors with others); `memfd_create` is a sparse file in frames of its
own; `mmap(MAP_SHARED)` of a memfd, and `MAP_SHARED | MAP_ANONYMOUS`, map the
same frames into every process and survive `fork`. Still missing, and
listed: memfd seals, `MAP_SHARED` of a regular file, `SIGBUS` on a truncated
mapping. WebKit's `ConnectionUnix.cpp` and `SharedMemory` should run
unchanged.

### 4.2 Per-thread signals (#772)

JavaScriptCore's garbage collector suspends every mutator thread by sending
a signal to *that thread* and waiting for the handler to park; its sampling
profiler does the same. Signal state used to be per address space, so one
thread's mask was every thread's and `tgkill` reached the process. Now the
blocked mask and a private pending set are per thread, process-directed
signals go to one eligible thread as Linux's `complete_signal` picks it,
dispositions stay per process, a fatal default ends the whole group, and a
call interrupted on a thread where no handler ran is restarted
transparently. The `threadsig` fixture does the collector's pattern end to
end: eight threads, `pthread_kill` each, handlers park on a futex, a
thousand rounds.

### 4.3 Executable memory (#773)

A JIT maps memory, writes code, flips it executable, and flips it back to
patch, thousands of times, with other threads executing from the page. This
already worked (NX on by default, `PROT_EXEC` honoured, `invlpg` on every
`mprotect`), but was untested, and the tests found four bugs: the `si_code`
of a `SIGSEGV` followed the CPU's write bit instead of Linux's rule, a forked
child could write through a read-only or RX page, `mprotect` copied
still-shared pages eagerly, and a write-only mapping was refused. The
`jitmem` fixture is a tiny JIT in ring 3 with real fault recovery. The TLB
flush is local: LazyOS runs on one CPU, and an SMP kernel needs a shootdown
here ([architecture/virtual-memory.md](architecture/virtual-memory.md),
"Executable memory").

### 4.4 Headroom (#774)

A WebKit2 browser is three or four processes and several dozen threads, and
a modern page sits at a few hundred MB. Measured, not guessed, with the
`bigapp` fixture (threads until `clone` fails, memory until `ENOMEM`,
descriptors until `EMFILE`, then four children at once; the thread count
moves by a few with what else the boot runs):

| Guest RAM | Threads before | after | Memory | Descriptors before | after |
|---|---|---|---|---|---|
| 1 GiB | 254 | 1016 | 760 MiB | 1024 | 8192 |
| 4 GiB | 254 | 1022 | 3064 MiB | 1024 | 8192 |

The task table is 1024 slots, kernel stacks come from the heap while a task
exists (a `limit.task_max` key, one slot per MiB of RAM; a flood of `clone`s
ends in `ENOMEM` while the kernel keeps a heap reserve), `RLIMIT_NOFILE` is
a real per-process limit under `fd_max`, and `run_demo.py --big-app` is the
profile ([architecture/limits.md](architecture/limits.md)). Open: a heap
kernel stack has no guard page, and a user quota set above physical RAM
ends in a segmentation fault at the frame allocator instead of `ENOMEM`.

## 5. The path (W1 to W5)

Effort is for one experienced person, full time, and is the part of this
document most likely to be wrong.

### W1. Prove the toolchain (about a week)

Build a C++ program with `zig c++ -target x86_64-linux-musl -static` and
libc++ that uses threads, exceptions, thread-local storage and
`std::filesystem`, and run it through the ABI shim like the Linux apps
(`tools/linuxapps/build.py`). Then build ICU statically with its data
compiled into the library, since there is no file-backed `mmap` to load a
data file from. Done when both run on LazyOS; nothing below is then blocked
by the compiler.

### W2. The `jsc` shell (two to four weeks)

WebKit's `JSCOnly` port exists for exactly this: CMake with
`PORT=JSCOnly`, `ENABLE_STATIC_JSC=ON`, `ENABLE_JIT=OFF`, the C-loop
interpreter; its only dependency is ICU. Run `jsc` on LazyOS as a static
ELF and run JavaScriptCore's own test suite from the ABI runner. This is
where §4.2 and §4.3 are proven against the real consumer. The JIT is tried
afterwards by flipping it on: on Linux it uses plain `mprotect` flips, with
none of the Apple-only write-protect calls. A `jsc` fixture in the bench is
the regression test for all of it.

### W3. The dependency bundle (one to two weeks)

A script beside the Linux apps' that builds zlib, libpng, libjpeg-turbo,
libxml2, sqlite, FreeType, HarfBuzz, curl with a TLS library, and the raster
backend WebKit uses at the pinned revision, all static musl, from pinned and
hash-checked sources. The host side needs perl, ruby, python, gperf, bison,
cmake and ninja, so this runs under WSL like the `xui-app` lib tests.
Licensing is easier than LazyWeb's: WebKit is LGPL 2.1 and BSD, so the
GPLv2-only crate rule of [lazyweb.md](lazyweb.md) does not apply here.

### W4. The port skeleton (one to two months)

Copy the PlayStation port's CMake options and platform files into a
`PORT=LazyOS` tree. Nearly all of it is choosing generic implementations
WebKit already has: the generic run loop, the POSIX file system, pthread
threading, curl networking, the built-in image decoders, FreeType fonts
pointed at the Liberation files in the image. What must be written is
small:

* a process launcher that forks and execs the WebProcess and
  NetworkProcess with the socketpair end inherited (fork and exec, not
  `posix_spawn`: musl's uses a vfork-style `clone` the shim refuses);
* a font database that scans a directory instead of calling fontconfig;
* cursor, pasteboard (over `clipboardd`), screen, keyboard and mouse
  mapping.

Everything optional is off: video, WebGL, WebRTC, the sandbox, accelerated
compositing. The one thing to check before pinning a revision is that it
still has the non-composited drawing area that paints a page into a shared
bitmap; WebKitGTK has discussed removing it.

### W5. The window (a few weeks)

The UI process is a Rust `xui-app` client, as Doom and Quake are, with the
WebKit UI library statically linked in and driven through WebKit's C API:
create a context and a page, hand it the shared bitmap the WebProcess
paints, blit that into the client window, forward xui input as page events.
Doom already links C compiled by zig into a Rust musl binary, so the recipe
exists. Ctrl+L, history and downloads come later, and can borrow LazyWeb's
chrome.

**First render:** a static page with CSS and images, JavaScript
interpreted, no video, no GPU, two to three months in.

## 6. The problems that will surface

* **Memory for code.** LazyOS loads a static ELF whole into RAM, with no
  demand paging and no sharing of text between processes. Three WebKit
  processes at 40 to 60 MB stripped each is roughly 150 MB resident for
  code before a page loads. File-backed `mmap` with shared text pages is
  the fix, and it is the entry on the missing list of
  [linux-abi-plan.md](linux-abi-plan.md) that now matters most. Until then
  the profile is `run_demo.py --big-app --memory 4G`.
* **Process spawning.** Fork and exec, as above.
* **Threads per process.** JSC alone starts a dozen; `task_max` is per
  machine, and a desktop session already holds a few dozen slots. The
  `bigapp` numbers are for a console image; measure a logged-in desktop.
* **The treadmill.** WebKit moves fast; a port no upstream maintainer runs
  rots within a few releases (Haiku's is perpetually behind). Pin a
  revision, keep the port files out of tree (§7), and rebase on upstream
  only when something is needed.

## 7. Where it would live

`webkit/` beside `doom/` and `quake/`: the port layer, the dependency and
build scripts, the Rust window, and the package `org.lazy.webkit` built
only by `LAZYOS_WEBKIT=1` (`run_demo.py --webkit`, a GUI checkbox), with
WebKit fetched at a pinned revision at build time and never vendored. The
PR for this plan ships none of that.

## 8. Alternatives considered

* **Keep extending Blitz plus QuickJS** (LazyWeb): the shipped browser, and
  the right default for anything that does not need a full engine.
* **Ladybird's LibWeb**: a few hundred thousand lines, multi-process over
  the same Unix primitives as WebKit, written for a hobby OS. Worth a look
  before W4 if the WebKit dependency bundle (W3) turns out heavier than
  expected; §4 serves it just as well.
* **Servo**: Rust, but its renderer needs OpenGL and its JavaScript is
  SpiderMonkey built through Mozilla's own build; not smaller than WebKit
  for this purpose.
