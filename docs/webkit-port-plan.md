# A WebKit port — exploration and plan

Status: **W0 to W4 built** (2026-10-12). The four kernel prerequisites
landed (issues #771 to #774, §4); the C++ toolchain is proven and WebKit's
`JSCOnly` shell runs on LazyOS both as the C-loop interpreter (`jsc`) and with
the JIT (`jsc-jit`: LLInt, baseline, DFG), each passing 254 of 255 files of a
fixed `JSTests/stress` subset, the dependency bundle (zlib to curl with
OpenSSL, FreeType, HarfBuzz, Fontconfig, Skia) is built and used by a program
on LazyOS, including an HTTPS fetch, and **WebCore and WebKit2 build as
`PORT=LazyOS` and run on LazyOS as three real processes**: a headless UI process
(`webkit-smoke`) launches a WebProcess and a NetworkProcess over the Unix IPC,
loads `about:blank` and a `data:` URL, reads the title and paints the page into a
bitmap that equals, pixel for pixel, the one the same programs paint on Linux
(§5 W4). Next is W5 (the window). Decisions taken: WebKit2 is
the engine (§2), the port is modeled on the PlayStation port with WebKit's C
API as the seam to a Rust xui window (§5), and the port files live out of
tree like `doom/` and `quake/` (§7).

**W1/W2 results** (`tools/jsc/`, [README](../tools/jsc/README.md); `python
tools/jsc/build.py` then `python tools/jsc/run.py`):

| | |
|---|---|
| C++ proof `cxxproof` | 30.8 MiB static-PIE (about 30 MiB of it ICU's data): libc++, 8 threads, exceptions, RTTI, TLS destructors, `std::filesystem`, `ucnv`/`uloc`/`Normalizer2`/`BreakIterator`; prints `JSC:W1:PASS` on LazyOS at the first run |
| `jsc` | WebKitGTK 2.54.1 `JSCOnly`, C-loop, ICU 74.2: **44.5 MiB** stripped, **no source patches** (flags only, see the README) |
| Build | clean, from nothing but the pinned Docker image: **4 min 20 s** on a 20-core host (downloads, ICU twice, 2098 WebKit objects, `jsc` link; zig cache cold), an identical 44.5 MiB result the second time |
| `jsc -e 'print(1+1)'` on LazyOS | 170 to 230 ms from `exec` to exit under WHPX (a 45 MB ELF read whole from ext2, ICU initialised, concurrent GC threads started) |
| 1.5 M allocations, 4 concurrent marker threads, `gc()`/`edenGC()` | 1.2 s under WHPX, correct result |
| Classes, private fields, Promises, async generators, `Intl.NumberFormat`, `toUpperCase` | `JSC:RUN:PASS:promise` |
| `jsc-jit` (`build.py --jit`) | `ENABLE_JIT=ON`, `ENABLE_C_LOOP=OFF`, LLInt in assembly, baseline and DFG; **FTL off** (B3 does not build with WebAssembly off at this revision); **52.0 MiB**; incremental build 7 minutes, two small source patches (below) |
| JIT speedup over `jsc` in the guest (WHPX, one CPU, `tier:dfg` confirmed) | `gc` script 1.8x to 2.1x, `hot` (fib(30), a 20 M-iteration loop, a 60 k sort) 2.6x to 2.7x, four SharedArrayBuffer agents 1.8x to 2.5x; `promise` (short, 130 ms) 0.8x and the timer-bound `suspend` script 1.0x: the JIT's start-up cost shows where there is nothing to compile. Runs on a loaded host vary by about 20%; native Linux gets 4x on the same `hot` code, so the guest loses part of it (one CPU: the compile thread competes with the mutator) |
| `JSTests/stress` subset (`tools/jsc/stress.py`, 255 files starting `g` or `n`, 262 KiB, WebKit commit of the `webkitgtk-2.54.1` tag) | **254/255 in `jsc` and 254/255 in `jsc-jit`** (99.6%), each file with a 10 s limit, about 58 s per shell. The one failure in both is `get-array-length-reuse.js` (`//@ memoryHog!`): it zero-fills a 2 GiB typed array in a 2 GiB guest, so the frame allocator runs out and the process dies with `SIGSEGV` (Linux would OOM-kill it). With `--memory 4G` both shells pass **255/255**. The judge requires 98% |
| Per-thread signal path (§4.2) with the real consumer | seen: the `suspend` script (main thread idle in the run loop on timers, collector working) makes the collector thread `tkill` the main thread with `SIGUSR1`, 32 sends in `jsc` and 24 in `jsc-jit`, each answered by the handler (`rt_sigsuspend`, `rt_sigreturn`); not by `agents` or `gc`, see §4.2 |
| `sched_setattr` (314) | implemented with `sched_getattr` (315); no `ENOSYS 314` line in the run |

Built in Docker (`tools/jsc/Dockerfile`, Debian 12 with zig from pip): a
WebKit build is thousands of small files and needs ruby, perl, cmake and
ninja, which a container and a named volume keep off a Windows bind mount;
the same `build.py` runs on a Linux host with `--no-docker`.

**W3 results** are in §5 W3: 17 libraries, **105.9 MiB** of archives, built in
**2 min 50 s**, `depsproof` passes on LazyOS; Skia (WebKit's own copy) is the
raster backend and a CPU path still exists.

Not done: the whole of `JSTests/stress` (a fixed 255-file subset runs once with
default options; the Ruby harness cannot run in the guest, so the harness's
option sets are not), FTL (B3), WebAssembly, and a kernel that enables AVX
state (the JIT shell is patched not to use AVX, see "W2 result" in section 5).

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

*Seen with JavaScriptCore (W2).* The collector suspends a thread with
`SIGUSR1` (`tkill`, musl's `pthread_kill`) and resumes it with a second one.
It does so for a mutator that is not at a safepoint, for example the main
thread parked in the run loop on a `setTimeout` while the collector thread
works; the guest log shows `tkill` from the collector thread to the main thread
and the handler running (`rt_sigsuspend`, `rt_sigreturn`). Four
`SharedArrayBuffer` agents allocating and parking in `Atomics.wait` while the
main thread calls `fullGC()` send **no** signal: each agent is a VM with a
heap of its own, so no collector has to stop another thread, and a script that
calls `fullGC()` itself stops at a safepoint. The earlier claim that a single
JS thread never uses the signal was wrong for the idle case. The evidence was a
temporary `serial_println!` in `sys_tkill` (not committed): 32 and 24 sends in
the `suspend` script for `jsc` and `jsc-jit`, 16 in two of the stress files
(`get-by-val-hoist-above-structure*.js`, which also wait on timers), none in
`gc`, `hot` or `agents`.

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

*W2 result.* Both shells run (table at the top). The JIT's first run died with
`SIGILL`, not with one of §4.3's `SIGSEGV` cases: the crash report (which now
prints the 16 bytes at `rip`) showed `c4 c1 7b 11 43 50`, `vmovsd [r11+0x50],
xmm0`, in the JIT code pool. JavaScriptCore enables AVX from `CPUID` alone, and
LazyOS sets neither `CR4.OSXSAVE` nor `XCR0`, so every VEX instruction is
`#UD`; a QEMU guest on WHPX or KVM reports AVX. The executable-memory side
(`mmap`/`mprotect` flips, thousands of them, with the compile thread running)
needed nothing. `tools/jsc/patches/webkit/0002` makes JSC check `OSXSAVE` and
`XGETBV` as Intel's manual prescribes; the kernel-side alternative is to
enable AVX state (`CR4.OSXSAVE`, `XSETBV`, `XSAVE` in the context switch and the
signal frame), which is not done. Without the patch a real PC with AVX would
hit the same `SIGILL`.

### W3. The dependency bundle (built)

`python tools/webkit/deps.py` builds the libraries below as static musl
archives with zig, in the `jsc` build's Docker container, into one prefix, from
pinned and hash-checked sources, and writes `target/webkit/deps.json`.
`depsproof` (`tools/webkit/depsproof/`) links all of them and uses each;
`python tools/webkit/run.py` runs it on LazyOS. Details:
[`tools/webkit/README.md`](../tools/webkit/README.md).

**What the revision needs.** Read in the WebKitGTK 2.54.1 tarball
(`Source/cmake/OptionsGTK.cmake`, `Source/WebCore/platform/*.cmake`,
`Source/ThirdParty/skia/CMakeLists.txt`) and, for the PlayStation port, in
WebKit's `main` (`OptionsPlayStation.cmake`, `PlatformPlayStation.cmake` of
WebCore and WebKit, read 2026-10-12). The release tarball **does not contain**
the PlayStation or WPE CMake files (`OptionsPlayStation.cmake`,
`OptionsWPE.cmake`, `Source/WebCore/PlatformPlayStation.cmake`,
`Source/WebKit/PlatformPlayStation.cmake`, `WebCore/platform/CoordinatedGraphics.cmake`,
and WebCore's `libwpe` directories are all absent; `WebKitFindPackage.cmake`
does not exist in either revision). It does contain every PlayStation
*source* file (`*/playstation/*`) and the shared `Curl`, `OpenSSL`, `FreeType`,
`Skia`, `TextureMapper`, `ImageDecoders` CMake fragments, so W4 takes the
glue from git at a matching revision (or writes it: it is about 250 lines) and
uses the tarball for everything else.

| Library | Version pinned | Why (who asks) | Licence |
|---|---|---|---|
| ICU (data, i18n, uc) | 74.2 (from W1; WebKit wants 70.1) | JSC, WTF, WebCore text; HarfBuzz's ICU glue; libxml2, libpsl | Unicode-3.0 |
| zlib | 1.3.2 | PNG, HTTP, WOFF; FreeType, curl, OpenSSL | Zlib |
| libpng | 1.6.59 | `ImageDecoders.cmake` (`PNG::PNG`), Skia's `SkPngCodec` | libpng-2.0 |
| libjpeg-turbo | 3.2.0 (WebKit wants 1.5.2; SIMD off) | `ImageDecoders.cmake`, Skia's `SkJpegCodec` | IJG, BSD-3-Clause, Zlib |
| libwebp (+demux) | 1.6.0 | `ImageDecoders.cmake` (`WebP::demux`) | BSD-3-Clause |
| FreeType | 2.14.3 (WebKit wants 2.9.0) | Skia's `SkFontHost_FreeType`; `WOFF2Checks` | FTL or GPL-2.0+ |
| brotli | 1.2.0 | WOFF2 inside FreeType (`HAVE_WOFF_SUPPORT`), `Content-Encoding: br` | MIT |
| HarfBuzz (+ICU) | 14.4.0 (WebKit wants 2.7.4) | `ComplexTextController`, `SkiaHarfBuzzFont`, Skia's SVG glyphs (`HarfBuzz::ICU` is required) | MIT |
| expat | 2.8.5 | Fontconfig's config parser; Skia's OpenType-SVG (`USE_SKIA_OPENTYPE_SVG`) | MIT |
| Fontconfig | 2.16.0 (WebKit wants 2.13.0) | **hard**: `FontCacheSkia.cpp` calls `SkFontMgr_New_FontConfig`, `SkiaSystemFallbackFontCache` includes `<fontconfig/fontconfig.h>`, Skia's CMake requires it everywhere but Windows and PlayStation | MIT-like (HPND) |
| libxml2 | 2.15.3 (WebKit wants 2.9.7 to 2.9.13) | `XMLDocumentParserLibxml2`, SVG; ENABLE_XSLT is off so libxslt is not needed | MIT |
| SQLite | 3.54.0 (WebKit wants 3.23.1) | storage, IndexedDB, cookie jar | public domain |
| libpsl | 0.23.3 (WebKit wants 0.20.2) | `PublicSuffixStoreCurl`, `CookieJarDB` (`PlatformPlayStation` lists it as required) | MIT; its built-in public-suffix list is MPL-2.0 |
| OpenSSL | 3.5.9 (LTS) | `Curl.cmake` links `OpenSSL::SSL`; `CurlSSLVerifier`, `OpenSSLHelper`, `crypto/openssl` (SubtleCrypto) call its API | Apache-2.0 |
| nghttp2 | 1.70.0 | HTTP/2 through curl (optional for WebKit; real sites want it) | MIT |
| curl | 8.22.0 (WebKit wants 7.85.0) | `platform/network/curl` (`USE_CURL`) | curl (MIT-like) |
| Skia | the copy inside the tarball (`Source/ThirdParty/skia`, 54 MB) | the raster backend, see below | BSD-3-Clause |

Not needed, and why: **Cairo** (the GTK and WPE ports set `USE_CAIRO` to
`OFF`; the Cairo files remain and `OptionsPlayStation.cmake` still has an
`if (NOT USE_SKIA)` branch for it, but nothing at this revision builds them);
**libwpe** and **WebKitRequirements** (the PlayStation port's input and
resource glue; a LazyOS port replaces about a dozen small files, §W4);
**libxslt**, **LCMS2**, **libavif**, **libjxl**, **libwoff2** (off, or
FreeType does WOFF2); **GLib, GTK, libsoup, libgcrypt, libtasn1, libepoxy,
libsecret, Enchant, Flite, GStreamer, ANGLE** (GTK-only or off); **libpsl's
neighbours** libidn2 and libunistring (ICU does the work). Build-time tools:
cmake, ninja, perl, python, ruby, gperf, bison, flex, pkg-config, all in the
`jsc` Docker image (the tarball carries `unifdef`).

**The raster backend is Skia, and a CPU path still exists.**
`OptionsGTK.cmake` sets `USE_SKIA` on and `USE_CAIRO` off; Skia is not a
download (it is `Source/ThirdParty/skia`, built by WebKit's own CMake as a
static library, GN not involved). It compiles its Ganesh GL backend
unconditionally (`SK_GL`, `SK_GANESH`), so it needs the Khronos `EGL` and
`GLES2` headers (WebKit ships them in `ThirdParty/ANGLE/include`) but no GL
library until something links `GrGLMakeNativeInterface_egl`. A non-composited
drawing area is still there: `DrawingAreaCoordinatedGraphics` creates a
`NonCompositedFrameRenderer`, which, when the surface does not use GL
(`AcceleratedSurface::usesGL()` is false for `RenderingPurpose::NonComposited`
with hardware acceleration off), paints with `GraphicsContextSkia` in
`RenderingMode::Unaccelerated` on a CPU `SkSurface` into a
`RenderTargetSHMImage`, a `ShareableBitmap` (shared memory) the UI process
reads. `grep` finds no `ENABLE_ACCELERATED_2D_CANVAS` or
`ENABLE_ACCELERATED_COMPOSITING` option at this revision; the 2D canvas
chooses `ImageBufferSkiaAcceleratedBackend` or `...UnacceleratedBackend` per
canvas. Three caveats for W4: that path is guarded by `PLATFORM(GTK) ||
ENABLE(WPE_PLATFORM)` (the PlayStation port's `AcceleratedSurfacePlayStation`
is GL only), so the port must select the SHM render target itself; the CPU
path still *compiles* `GLContext`, `TextureMapper` and `ThreadedCompositor`
(`USE_COORDINATED_GRAPHICS` is on in both ports), so linking needs an
`EGL`/`GLESv2` provider, a stub that reports no display is enough; and
`acceleratedCompositingEnabled` must stay false so a page never leaves the
non-composited renderer (read from the code, not yet run).

**Decisions.** *Raster:* Skia, because it is the only backend GTK and WPE still
build and the CPU path is a supported configuration, not a patch; Cairo plus
pixman would be a revival of code the ports dropped. Built from the tarball in
under a minute on 20 cores (53 s in the table below). *TLS:* OpenSSL 3.5 LTS, because WebKit's curl port and
SubtleCrypto are written against the OpenSSL API (`CurlSSLVerifier`,
`OpenSSLHelper`, `crypto/openssl`), so Mbed TLS or rustls would mean
rewriting those files. Apache-2.0 may be linked into LGPL-2.1 WebKit (LazyWeb's
GPLv2-only crate rule does not apply to it), but it is GPLv2-incompatible, so it
**must never be linked into LazyWeb or the `nettls` stack**:
`tools/webkit/licenses.py` enforces it (`wkpins.NEVER_IN_LAZYWEB`). It is built `no-asm`: the portable code
cannot execute an instruction LazyOS leaves disabled (AVX state), at the cost of
slower handshakes.

**W3 results** (`tools/webkit/`, [README](../tools/webkit/README.md)):

| Library | Version | Archives | Build |
|---|---|---|---|
| zlib | 1.3.2 | 0.2 MiB | 2 s |
| brotli | 1.2.0 | 1.0 MiB | 4 s |
| libpng | 1.6.59 | 0.4 MiB | 1 s |
| libjpeg-turbo | 3.2.0 | 0.9 MiB | 3 s |
| libwebp | 1.6.0 | 1.7 MiB | 3 s |
| icu | 74.2 | 44.9 MiB | 0 s |
| freetype | 2.14.3 | 1.3 MiB | 2 s |
| expat | 2.8.5 | 0.4 MiB | 2 s |
| harfbuzz | 14.4.0 | 8.5 MiB | 29 s |
| fontconfig | 2.16.0 | 0.6 MiB | 9 s |
| libxml2 | 2.15.3 | 2.5 MiB | 3 s |
| sqlite | 3.54.0 | 2.5 MiB | 15 s |
| libpsl | 0.23.3 | 0.1 MiB | 4 s |
| openssl | 3.5.9 | 14.2 MiB | 14 s |
| nghttp2 | 1.70.0 | 0.4 MiB | 3 s |
| curl | 8.22.0 | 1.7 MiB | 10 s |
| skia | WebKitGTK 2.54.1 ThirdParty/skia | 24.7 MiB | 53 s |

* **Build:** a clean prefix, with the sources downloaded, ICU already built by
  the `jsc` build and zig's global cache warm, takes **2 min 50 s** on a
  20-core host (`python tools/webkit/deps.py`, Docker); a second run only
  reads stamps. The archives total **105.9 MiB** (ICU's 31 MB data and
  Skia's 26 MB dominate); `target/webkit/deps.json` lists every installed file.
* **`depsproof`:** 43.5 MiB static-PIE (30 MB of it ICU's data). On LazyOS
  (WHPX, 2 GiB, a console image with networking) it prints
  `WEBKIT:W3:PASS:<lib>` for all 17 libraries and `WEBKIT:W3:PASS`.
  `python tools/webkit/run.py` also requires the decoded
  pixels (PNG, JPEG, WebP), the glyph bitmap, the HarfBuzz result and the Skia
  surface (a stroked line and a line of text through
  `SkFontMgr_New_FontConfig`) to equal what the same program printed on the build
  host (`target/webkit/depsproof.host.txt`), the page curl fetched over
  TLS 1.3 (3037 bytes, verified against the harness CA in the image's bundle,
  SNI `tls.test`) to be the one the host server holds, and the server to have seen
  a completed TLS 1.3 handshake with that SNI and a request for `/` from `depsproof`.
* **One deviation from upstream, found by running it:** on LazyOS
  `mmap(fd, PROT_READ, MAP_PRIVATE)` of a file fails (the file-backed `mmap`
  of §6). Skia's fontconfig font manager maps font files with
  `SkData::MakeFromFileName`, has no fallback, and so refused every font
  (`FontAccessible()`: `matchFamilyStyle` returned null although Fontconfig and
  FreeType alone worked). `skiabuild/lazyos_mmap.c` (linked into
  `libSkia.a`; `SkOSFile_posix.cpp` is compiled with `-Dmmap=lazyos_mmap`) copies the file
  into an anonymous mapping when the kernel cannot map it. No source file of any
  tarball is patched (`tools/webkit/patches/` is empty). The fix that removes the shim is the
  kernel's: a read-only `MAP_PRIVATE` file mapping, which also saves the RAM
  every WebKit process spends on its font files.
* **Not exercised:** WOFF2 through FreeType+brotli (no sample font; brotli and
  FreeType's `FT_REQUIRE_BROTLI` are built), Skia's OpenType-SVG path, curl's HTTP/2
  and `br` against a server (the harness speaks HTTP/1.1 only; `curl_version_info`
  confirms the features and nghttp2 is exercised alone), SubtleCrypto beyond
  SHA-256 and AES-GCM, and the GPU side (EGL/GLES2 are not in the bundle).
* **Gaps left for W4:** an `EGL`/`GLESv2` provider for linking (a stub), the
  PlayStation CMake glue (absent from the tarball), and the SHM render
  target selection for LazyOS (above); `Fontconfig` needs a `fonts.conf` in
  `/system/etc/fonts` (a sample is kept in the prefix at
  `share/fontconfig-etc`) and a writable `/tmp/fontconfig`.

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
compositing. The non-composited drawing area that paints a page into a shared
bitmap still exists at 2.54.1 (`NonCompositedFrameRenderer`, §5 W3), but only in
the GTK and WPE code path: the PlayStation `AcceleratedSurface` is GL only, and
the tarball lacks the PlayStation CMake files, so the port starts from git for
those and writes the render-target choice itself.

**W4 results** (`webkit/`, [README](../webkit/README.md); `python tools/webkit/deps.py`
then `python tools/webkit/port.py` and `python tools/webkit/port_run.py`):

| | |
|---|---|
| Configuration | `PORT=LazyOS`: curl and OpenSSL (the bundle's 3.5), Skia on the CPU, FreeType/Fontconfig/HarfBuzz, libwpe's types, the generic run loop, `USE_COORDINATED_GRAPHICS`, **JIT on** (LLInt, baseline, DFG; no FTL), static JSC, system malloc, no video, WebGL, WebRTC, Web Audio, WebAssembly, GPU process, sandbox, remote inspector |
| Compiled and linked | all of WTF, bmalloc, JavaScriptCore, PAL, Skia, WebCore and WebKit (8288 ninja steps, unified sources); `WebProcess` **107.4 MiB**, `NetworkProcess` **107.4 MiB**, `webkit-smoke` **104.4 MiB**, stripped static-PIE (ICU's 30 MB of data is in each) |
| Build | **9 min 19 s** from a fresh container state on a 20-core host with 15 GiB for Docker (extraction, 11 patches, 85 fetched files, configure, 8288 steps at `-j13`: 8 min 7 s, link, strip, the host run); zig's cache wiped first. A rebuild after editing one port file: seconds |
| On LazyOS (WHPX, 4 GiB, console image with networking) | `WEBKIT:W4:UP`, `WEBPROCESS:UP:<pid>`, `NETWORKPROCESS:UP:<pid>`, `LOAD:about:blank`, `LOAD:data:text/html,...`, `TITLE:hello lazyos`, `PAINT:800x600:6cf525bf`, `PASS`; from `exec` of `webkit-smoke` to the first finished load 3.7 s (two 107 MB ELFs read whole, a fork and exec each, JSC and Skia initialised), the data page 50 ms later |
| The same programs on the Linux build host | the same checksum, the same ink (42 pixels, `x` at 8,15 to 15,22): `port.py` runs `webkit-smoke` once in the container (`target/webkit/smoke.host.txt`) and `port_run.py` requires the guest to equal it, so layout, fonts, Skia and the IPC compute the same page on LazyOS as on Linux |

*What had to be written.* The tarball is GTK's (libsoup, GLib): it has every
`*/playstation/*` source and none of the PlayStation CMake, none of the libwpe
directories the PlayStation WebKit layer uses, and none of `platform/network/curl`
or the curl parts of WebKit (85 files, `webkit/port/upstream.txt`, fetched from
git by blob digest). The PlayStation port was also found to be unbuildable with
Skia at this revision as it stands (its Cairo-era code names `epoxy`, GLES3
enums, a `SkiaPaintingEngine::create()` that has since gained an argument, a
`hardwareAccelerationEnabled` setting that only GTK and WPE define), so the
port is the PlayStation WebCore and WebKit layer on a Skia build, patched
(`webkit/patches`, eleven patches including JSC's two; `webkit/README.md`
lists each with its reason) and given these LazyOS files (`webkit/port/tree`):
`OptionsLazyOS.cmake` and a `PlatformLazyOS.cmake` for WTF, bmalloc,
JavaScriptCore, PAL, WebCore and WebKit; `ProcessLauncherLazyOS.cpp` (fork and
`execv`, the socketpair's client end inherited; `posix_spawn` is not used);
`CurlSSLHandleLazyOS.cpp` (the system CA bundle instead of the console's);
`InjectedBundleLazyOS.cpp` (a static executable cannot load a bundle: refused
with a message); `WPELoaderLazyOS.cpp` (libwpe is linked static and asks for a
backend; there is none). Also: libwpe 1.16.2 (static, no xkbcommon) and a
do-nothing EGL/GLESv2 generated from the Khronos headers join the dependency
bundle (`tools/webkit/recipes_port.py`), `fonts.conf` is written into the image
(`/system/etc/fonts`) with the Liberation fonts, and W3's `lazyos_mmap` shim is
linked into the port's Skia. The platform macro is `PLATFORM(LAZYOS)`, not
`PLATFORM(PLAYSTATION)`, whose tests in WTF, JavaScriptCore and bmalloc choose
console code (patch `0003` renames the 44 tests in WebCore and WebKit only), so
those three stay exactly the proven `jsc-jit` configuration.

*Drawing.* `acceleratedCompositingEnabled` is false, the drawing area is the
PlayStation port's classic one: the WebProcess paints dirty rectangles into a
`ShareableBitmap` (shared memory, `memfd` + `SCM_RIGHTS`, W0) with
`GraphicsContextSkia` on a CPU surface and sends an `Update`; the UI process
keeps them in `BackingStoreSkia` and `WKPagePaint` copies them out. Nothing
touches EGL: the composited classes (`LayerTreeHost`, `ThreadedCompositor`,
`AcceleratedSurface`) are compiled and linked against the stand-ins and never
run. The chosen route is not the brief's `NonCompositedFrameRenderer` (GTK/WPE's
`DrawingAreaCoordinatedGraphicsGLib`): it includes libepoxy and needs the GTK
shared sources; the PlayStation drawing area gives the same CPU path with no
patch to select a render target.

*Gaps found by running it* (`SYSCALL_USED` and the guest's own errors; each is
fixed and has kernel tests, `compat_msg_*`):

| syscall | what WebKit does | what LazyOS did | now |
|---|---|---|---|
| `sendmsg` (46) and `writev` (20) on `AF_UNIX` `SOCK_SEQPACKET` | `ConnectionUnix` sends a header and a body as two `iovec` segments | `EOPNOTSUPP` (`sendmsg`) or two records (`writev`) | one record, as Linux builds it (`io::send_gathered`); a scattered datagram on `AF_INET` is still refused |
| `recvmsg` (47) with `MSG_NOSIGNAL` | `recvmsg(fd, &msg, MSG_NOSIGNAL)` | `EINVAL`: "Error receiving IPC message" on both sockets | accepted and ignored |
| `fadvise64` (221) | something in the three processes advises on a file | `ENOSYS` (once, ignored by the caller) | open, harmless |
| file-backed `mmap` (9) | Skia's font manager (`SkData::MakeFromFileName`, no fallback) | fails | the W3 shim copies the file; without it every font family silently fell back to one face (found because the first guest paint differed from the host's) |

Everything else the three processes use (`fork`, `execve`, `clone`, `futex`,
`socketpair`, `SCM_RIGHTS`, `memfd_create`, `sched_setattr`, `mremap`,
`madvise`, `prctl`, `sched_getaffinity`, `sysinfo`, `getdents64`, `select`)
was already there.

*Not verified.* HTTP or HTTPS loads through the NetworkProcess (the run loads
`about:blank` and a `data:` URL; the network stack is the bundle's, proven in
W3, but WebKit's curl path has not fetched a page on LazyOS), input events,
JavaScript in a page (the engine is `jsc-jit`'s, but no script ran), images,
the desktop (the run is a console image with the fonts added), crash recovery
of a WebProcess, memory per process (three static 105 to 107 MiB ELFs are
read whole into RAM; `--big-app`, 4 GiB), FTL, and the whole thing on real
hardware. `webkit-smoke` prints one `WEBKIT:W4:INFO:*` line each for what a
reader would otherwise have to guess (fonts Fontconfig sees, the glyphs as
text, timings).

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

* **`sched_setattr` (syscall 314): done.** WTF's `Thread::setThreadQoS`
  (`wtf/posix/ThreadingPOSIX.cpp`) calls it to lower a GC marker thread's
  priority; it was `ENOSYS` (eight lines in the gc script with four markers).
  Now the plain policies and a nice value are accepted and mapped onto the
  stride weight, with `sched_getattr` ([linux-abi-plan.md](linux-abi-plan.md));
  the run has no `ENOSYS 314` line.
* **ABI gaps JSC met while running its tests.** `mprotect(addr, 0, prot)` was
  `EINVAL` (Linux: 0), which aborted two growable-ArrayBuffer stress files;
  fixed. A process that exhausts the frame allocator in a page fault is killed
  with `SIGSEGV` where Linux would OOM-kill it (`get-array-length-reuse.js`,
  2 GiB in a 2 GiB guest); open.
* **No AVX state.** See W2 above: JIT code that uses VEX instructions needs the
  kernel to enable AVX or JSC to be told not to (done, by patch).
* **Binary size.** `jsc` is 44.5 MiB because ICU's data (about 30 MB: every
  locale, converter and break rule) is inside; there is no file-backed `mmap`
  to load `icudt74l.dat`. An ICU data filter (locales, converters) would cut
  most of it; not done. Each running `jsc` costs that much RAM for code alone.
* **Toolchain holes met** (all handled by flags, `tools/jsc/webkit.py`): the
  release tarball lacks `Tools/Scripts/hmaptool` (`-DUSE_HEADER_MAPS=OFF`);
  `ENABLE_SAMPLING_PROFILER` conflicts with the C-loop; simdutf's AVX-512
  paths do not compile under zig's clang without `evex512`, and LazyOS enables
  no AVX state anyway (`-DSIMDUTF_IMPLEMENTATION_{ICELAKE,HASWELL,WESTMERE}=0`);
  ICU's cross build needs its tools built for the host first, and
  `--disable-tools` silently leaves a 1 KB stub `libicudata.a`; the WebKitGTK
  tarball holds symlinks Windows cannot extract (hence the container).
* **Memory for code.** LazyOS loads a static ELF whole into RAM, with no
  demand paging and no sharing of text between processes. Three WebKit
  processes at 40 to 60 MB stripped each is roughly 150 MB resident for
  code before a page loads. File-backed `mmap` with shared text pages is
  the fix, and it is the entry on the missing list of
  [linux-abi-plan.md](linux-abi-plan.md) that now matters most. Until then
  the profile is `run_demo.py --big-app --memory 4G`.
* **File-backed `mmap` showed up in W3 and again in W4** (the port's Skia needs the same shim, and without it text came out in the wrong face with no error). Skia maps font files with `mmap(fd,
  PROT_READ, MAP_PRIVATE)`, which fails on LazyOS, and its fontconfig font manager
  has no fallback; the bundle carries a copying shim
  (`tools/webkit/skiabuild/lazyos_mmap.c`). WebKit maps files elsewhere too
  in WTF's `FileSystem` too (not yet exercised); expect more of the same
  until the kernel can map a file read-only and private.
* **Process spawning: done.** Fork and exec, `ProcessLauncherLazyOS.cpp`; both children came up on the first run.
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
