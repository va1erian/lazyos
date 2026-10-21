# LazyWeb+ performance plan: make mainstream sites usable

Status: **draft, 2026-10-13** (rebased on main at #851/#852). Scope: `os.lazy.lazywebplus` ([`lazywebplus.md`](lazywebplus.md)) on the WebKit
port ([`webkit-port-plan.md`](webkit-port-plan.md)). Out of scope: LazyWeb (Blitz), a GPU or video path,
WebAssembly, SMP. Method follows [`performance-plan.md`](performance-plan.md): measure first, and keep what was
*measured* apart from what was only *read from the code*.

The goal is **usable, not fast**: a heavy page (Ars Technica, a modern Google property) must not freeze the window or
the desktop, must not keep the CPU busy once it has settled, and must scroll and take clicks. Everything below is
chosen for that.

## 0. Budgets (proposed; the person who owns the product should confirm them)

| # | Budget | How it is judged |
|---|---|---|
| U1 | Input is never ignored: pointer and keys reach the page and the chrome within 100 ms, also while a heavy page loads or animates (no `UI:STALL` over 250 ms from the browser, the shell or `xuid`) | `UI:STALL` lines + a scripted click while loading |
| U2 | A page that has settled costs nothing: under 3% of the CPU over any 5 s window, 5 s after load, with no pointer motion | per-process CPU from the kernel, no `WEBPLUS:INFO:PAINT` line |
| U3 | Ars Technica front page: first paint under 5 s, readable and scrollable (10 fps) at 1280x720 and (5 fps) at 2560x1440 | `WEBPLUS:INFO:TIME`, wheel session, frame counter |
| U4 | No engine process dies and no load ends in "internal error" across a list of 20 mainstream sites, cold and warm cache | harness verdict |
| U5 | Anything that cannot meet U1-U3 says so and offers a way out (stop the page, reload without JavaScript) instead of spinning | a visible bar, tested |

## 1. What was measured (2026-10-13) and what was not

**Setup.** A harness-built LazyWeb+ image (binaries of 2026-10-13 13:57, so *before* #847), QEMU/WHPX, 4 GiB,
1280x720 desktop, the 1000x450 default window, fresh profile, QEMU user networking to the real internet, driven by
`qemu_session.py` (Ctrl+L, type the address, Enter, wait, screenshots; then a click on the consent button and wheel
turns). Two boots of the same script. No new code was built.

| # | Measured | Note |
|---|---|---|
| M1 | `https://www.google.com/` loads in 1.2 s and is the **basic HTML page** (no script, "Rechercher" button) | the UA is `Mozilla/5.0 (PlayStation; PlayStation/0.00) ... Version/17.0 Safari/605.1.15` (`UserAgentPlayStation.cpp`), so sites treat the port as a console. "Modern Google" was therefore **not exercised** |
| M2 | `https://arstechnica.com/`: navigation start to commit 1.5-1.6 s, to `didFinishNavigation` **3.8 s and 4.8 s** (two boots), free RAM -170 to -220 MiB; a 228-partner consent dialog is on top | first paint 2.0-2.4 s after the request |
| M3 | After the click on *I accept* and three wheel bursts the article grid draws with web fonts and pictures; with nothing happening, screenshots 30 s apart are pixel-identical | the page is usable in this configuration |
| M4 | The WebProcess is demoted by the CPU guard (`SCHED:GUARD:DEMOTED`, six of ten ticks) **17 times by the end of a 70 s visit** (the counter is cumulative per task) | it is CPU-bound for most of the visit |
| M5 | 30 and 77 `ENOSYS 285 fallocate` per Ars visit, from the NetworkProcess | `WTF::FileSystem::createMappedFileData` (`FileSystem.cpp:353`, the cache's body writer) treats any errno except `EOPNOTSUPP` as a failure and returns no file: large cache bodies are probably never stored (to be confirmed by the cache directory's size) |
| M6 | A second visit to a page the first boot cached killed the NetworkProcess (`rip` in `cc` padding, internal error 300) | the known pre-#847 bug (patch 0017); the point is that a warm profile is a different test from a cold one |
| M7 | After the browser had run, `SYSMOND:SNAPSHOT:FAIL -7` repeats every second; a task named `fork` at slot 65 (apparently a second child process) appeared after the consent click; LazyShell logged `UI:STALL` of 663 ms at boot | unexplained, see P0 |
| M9 | Two of four launches of LazyWeb+ died before `UP:PASS` (status 139), once with the shipped program and once with a rebuilt one | a write fault in `memcpy` of the first resized frame, at a 2 MiB boundary, into a block `mremap` had moved; issue #853, section 8 |
| M8 | The main checkout's own image (HiDPI 2560x1440, both browsers and every app on a 1 GiB OS volume) booted with `probe write failed: no space left` for `confd` and `logd` | a full volume also fails the browser's cache and cookie writes. #852 (main, 2026-10-13) fixes the build side: the default volume now adds SRB2Kart and WebKit, and an interrupted update no longer leaves unlisted files behind |

**Not reproduced: "hangs immediately and spins".** At 1x on WHPX with a fresh profile Ars is usable. So the report
depends on something this run did not have. The candidates to rule in or out first (P0 does this before any fix):
HiDPI (the same path moves 4x the pixels), no WHPX (TCG) or a real single-core PC, a real pointer (the PS/2 mouse
runs at 200 Hz and every move is forwarded one by one), a long-lived profile (cache, cookies, a full volume), and a
JavaScript-heavy variant of Google (signed in, results page) that the console UA never receives.

## 2. Where the time can go (hypotheses, read from the code; none is measured yet)

| # | Hypothesis | Evidence in the code | What confirms or refutes it |
|---|---|---|---|
| H1 | **The paint path costs about eight full-frame passes per repaint (to be counted in P0) and runs unpaced on WebKit's main thread** (the UI process's half is fixed by the change of section 8) | `paint.cpp` as it was: `viewNeedsDisplay` drops the rect; `paintNow` does `memset` 0xff of the whole bitmap, `WKPagePaint` of the whole rect, a scalar BGRA to RGBA loop over every byte, then `lzw_ui_frame`; `mailbox.rs::store_frame` copies it again; `PageView::show` builds a new `Image` (a new id per frame; backends cache the decoded form per id, so each frame may be decoded and cached again), and `paint` fills the bounds white before `draw_image`. The same thread runs the IPC that feeds it | per-stage time counters at 1x and 2x; one repaint at 2560x1440 is 8-13 MB per pass |
| H2 | **Nothing paces rendering**, so any animation is a 100% duty-cycle pipeline | WebKit's classic drawing area re-arms a 0 s timer (`scheduleDisplay`: `m_displayTimer.startOneShot(0_s)`), paints dirty rects and waits only for the UI process's `DidUpdate`; the window asks `WKViewSetVisible(true)` forever; `schedulePaint` runs on the next loop turn. Carousels, GIFs, ad refresh, `requestAnimationFrame` loops and hover effects drive it | frames/s on a static page, an animated page and a minimised window |
| H3 | **One CPU with per-thread fairness**: a WebProcess with a collector, compiler and worker threads outweighs the UI process's two | `arch.md`: "No SMP"; `sched_getaffinity` answers one CPU; the scheduler's classes are strict and the CPU guard counts **per task**, so N helper threads that are each below six ticks of ten are never demoted (`tasks.md`); `ProcessLauncherLazyOS.cpp` sets no priority | per-thread CPU split during Ars; `UI:STALL` of the window thread |
| H4 | **Input is forwarded event by event** | `view.rs` sends every `MouseMove` to `lzw_engine_mouse`, which queues one `std::function` per event; the wheel makes one call per tick, up to 8 per event. At 200 Hz over a page full of `mouseover` handlers this is a second source of continuous work | count events per second reaching `WKPageHandleMouseEvent`; CPU while the pointer moves over Ars |
| H5 | **Memory and start-up cost of the engine as built** | three static 107 MiB ELFs, each carrying 30 MB of ICU data, copied **eagerly** into frames at `exec` (`processes.md`, "Pages holding file bytes are mapped eagerly"; 4.3 s cold start in W5); `USE_SYSTEM_MALLOC` is on, so WTF's allocator is musl's (zig's bundled musl: mallocng, one global lock, an `mmap` per large block; to be confirmed in the pinned tree); no demand paging although file-backed `mmap` and a page cache now exist (#535) | `exec` time, RSS per process, `mmap`/`munmap`/fault counts per second |
| H6 | **Build and CPU features**: `-O2`, no LTO or PGO, baseline x86-64; OpenSSL built `no-asm`; libjpeg-turbo SIMD off; QEMU started with no `-cpu` (#810) | `tools/jsc/toolenv.py` `CODEGEN`; W3 table; the launch line of `qemu_qmp.py`. (Under WHPX the guest did report AVX state, `xsave_mask=0x7`, so this is a TCG and a real-PC question) | CPUID line at boot; per-library microbench; A/B builds |
| H7 | **Content the port asks for**: the console UA, 228 consent partners, ad and tracker scripts | M1, M2; the 3rd-party share of CPU is unknown | CPU by origin (a request log with bytes and script time) |
| H8 | **The HTTP cache cannot store large responses** (M5), so every revisit refetches and re-decodes | `fallocate` is `ENOSYS` | the cache directory size after a visit; request count on revisit |

H1 to H4 decide whether the page "spins"; H5 to H8 decide how long it takes to become usable. H1/H2 are the
cheapest to fix and the ones whose effect is the same at any CPU speed.

## 3. The plan

Phases are ordered by risk-adjusted value. Each ends with a number from the harness, not a feeling.

### P0. Make it measurable and pin the repro (3 days)

1. `python tools/webplus/perf.py [--live] [--hidpi] [--accel none] [--site NAME]`: builds like `tools/webplus/run.py`,
   boots once, loads each site, scripts the same interaction (settle, accept consent, five wheel bursts, one
   pointer sweep), prints a table and writes `docs/perf/webplus.md`. Sites: fixtures committed in
   `tools/web/fixtures/theoldnet.com/webplus/perf/` (an animation page: CSS + `requestAnimationFrame` + GIF; a
   script-busy page: 2 MB JSON, 20k DOM nodes; 200 pictures; WOFF2 fonts; a tall page; iframes with timers) so a
   run is repeatable offline; `--live` reads `tools/webplus/perf_sites.txt` (ars, google search, wikipedia, github,
   hn, bbc, nytimes, reddit) and is manual (no copies of third-party pages are kept).
2. UI-process counters. **Done in part** (`WEBPLUS:INFO:PAINT:` once a second, a plain `say` of the UI process;
   section 8): paints, reports, dirty pixels, time in clear, `WKPagePaint`, conversion and the hand-over, the longest
   report-to-paint wait, the wheel and mouse events the window sent, wheel-to-first-report. **Still to do:**
   `postState` calls, events actually forwarded to WebKit, the window thread's `show_ms`. `WEBPROCESS:JSC:cores=<n>:markers=<n>:compilers=<n>` from the WebProcess
   (`JSC::Options` after `recomputeDependentOptions`).
3. Kernel: `PERF:CPU` once a second, one line per task with more than 1% (name, slot, thread, pct, class, guard
   demotions), and a **tick sampler**: on each 100 Hz tick record (slot, user `rip`, user or kernel) into a ring,
   print the top 20 per 5 s as `PERF:PROF`; the host script symbolizes with `addr2line` against **unstripped**
   copies (`port_cmake.py` strips with `--strip-all` today; keep `target/webkit/bin/*.sym`). Counts of syscalls
   per number for a chosen task and an `ENOSYS` counter per number.
4. Record the configurations that might reproduce the report (1x/2x, WHPX/TCG, cold/warm profile, pointer sweep on
   and off) and say which one hangs. Until one does, nothing in P1 to P5 is "the fix", only a measured gain.

*Done when:* `docs/perf/webplus.md` holds a baseline for the fixtures and Ars in all configurations, and M7's
`SYSMOND:SNAPSHOT:FAIL -7`, the second engine process and the 663 ms stall have an explanation.

### P1. A cheap paint path (4 days; H1, part of H4)

*Status:* items 1, 3 (the UI process's half), 4 and 6 are in the change of section 8 (damage rectangle, a
persistent BGRA target, no per-frame clear, a 32-bit conversion, the window's white fill skipped under a covering
frame; items 3 and 6 are not finished: the mailbox still copies the frame, and `postState` is not throttled). Item 5
shipped as a pacer thread in the UI process, not the window-owned pull described below, which would need an ABI
addition (`lzw_engine_paint`); at the kernel's 100 Hz tick a 33 ms wait is about 40 ms.

Files: `webkit/port/app/paint.cpp`, `engine.cpp`, `xui-app/crates/webkit/src/{mailbox,view,ffi}.rs`,
`xui-app/webplus/src/app.rs`; the xui item needs a change in `va1erian/xui` (the pinned `xui-core` revision).

1. **Damage.** Keep the rect `viewNeedsDisplay` gets, union them, paint only that rect into a persistent bitmap; the
   white background is written when the view is created or resized, never per paint.
2. **No swizzle.** Skia's N32 here is BGRA; give the window a BGRA-capable surface (xui `Image::from_bgra` or, better,
   a `Surface` the window updates in place: `update(rect, pixels)` with one stable id) instead of converting
   every byte; if xui cannot, swizzle only the dirty rect with SSSE3 `pshufb`.
3. **No copies.** The UI process paints into a buffer the window lends it (double buffer swap through the mailbox);
   the mailbox carries the buffer and the dirty rect, not `extend_from_slice` of the frame.
4. **No white fill under an opaque image** in `PageView::paint` (clip to the part the picture does not cover).
5. **Pace.** The window owns the clock: its xui timer asks the engine for a paint at most every 33 ms and only while
   damage is pending (`lzw_engine_paint`, pulled, not pushed; `WKRunLoop` has no timer in the C API, and the window
   thread already has one), and no paint is requested while the window is minimised or the page area is hidden
   (`WKViewSetVisible(false)`; see P2).
6. **State posts** (`postState`: three string copies and a mailbox clone per progress callback): at most 10 Hz, and only
   when a field changed.

*Done when:* the P0 counters show one pass over the dirty rect per repaint, a full-window repaint at 2560x1440 costs
under 10 ms of UI-process CPU, and U2 holds on the animation fixture with the window minimised.

### P2. Stop the spin and keep the desktop responsive (3 days; H2, H3, H4)

1. **Throttle rendering in the engine.** A LazyOS patch (`0022`; `0018` and `0019` are the content rule list patches, `0020` and `0021` the unresponsive-page ones) to the classic drawing area so `scheduleDisplay` waits
   a frame interval (33 ms by default; the interval is a preference the window can raise, up to 100 ms, while the
   page is hidden or the machine is loaded) instead of a 0 s timer; then check what `requestAnimationFrame`, CSS
   animation and `DOMTimer` do with it (the page-level knobs that exist in the C API: `WKPreferencesSetDOMTimersThrottlingEnabled`,
   `...RequestAnimationFrameEnabled`, `...AnimatedImageAsyncDecodingEnabled`, `...LoadsImagesAutomatically`,
   `...PageVisibilityBasedProcessSuppressionEnabled`, and `WKViewSetVisible`/`WKViewSetActive`).
2. **Visibility.** `WKViewSetVisible(false)` and `WKViewSetActive(false)` when the window is minimised, on another
   desktop or unfocused for a while (the window already sees focus and size events): a hidden page stops animating.
3. **Coalesce input.** Keep only the newest `MouseMove` per paint interval (and per page-event turn); fold wheel
   notches into one `WKPageHandleWheelEvent` with a summed delta.
4. **Priorities.** Start the WebProcess and NetworkProcess with a lower weight than the UI process
   (`sched_setattr`, which lowering is allowed unprivileged; `ProcessLauncherLazyOS.cpp`), so the window, `xuid`
   and the shell are served first. In the kernel, make the CPU guard count **per address space**, not per task
   (`kernel/src/task/guard.rs`), so a process with eight busy helper threads is demoted as one.
5. **Thread counts for one CPU** through the environment the children inherit (names to be checked against
   `Options.h` of the pinned tree): `JSC_numberOfGCMarkers=1`, `JSC_numberOfDFGCompilerThreads=1`, and measure
   `JSC_useConcurrentGC=0` and `JSC_useConcurrentJIT=0` (a concurrent compiler on one CPU buys nothing and
   costs two context switches per compile). The P0 line tells what JSC picked on its own.
6. **A way out (U5).** WebKit's responsiveness ping (`processDidBecomeUnresponsive`; verify it reaches the C API)
   or a CPU watchdog in the window (the WebProcess above 90% for 8 s with no `didFinish`) raises a bar:
   *This page is slow: Wait / Stop / Reload without JavaScript*. A per-site "JavaScript off" is one
   `WKPreferencesSetJavaScriptEnabled(false)` and a reload, and Google's basic page (M1) shows how much of the
   web works that way.

*Done when:* the animation fixture is under 15% CPU at 30 fps and 0% hidden, a scripted click during the Ars load is
answered within 250 ms, and the window thread logs no `UI:STALL` over 250 ms.

### P3. The kernel and ABI surface a browser exercises (1 to 2 weeks; H5, H8)

1. **`fallocate` (285).** Implement mode 0 on ext2 (reserve blocks, or just extend) or return `EOPNOTSUPP` (what
   Linux says for a file system without it, and what WTF accepts). Then make a five-site run print **no**
   `ENOSYS`, and keep that as a harness check (`tools/webplus/perf.py` fails on any). Half a day, big for H8.
   *Found (issue #864): not yet.* Answering `EOPNOTSUPP` (WTF accepts it) makes the cache store bodies as blobs,
   and blobs need **hard links**: `BlobStorage::add` links each blob under two names and `synchronize()` deletes
   every blob with `st_nlink == 1` at the next start. `link` is `EPERM` and `st_nlink` is always 1 here, so
   the blobs vanish and a revisit refetches what the inline path used to serve (the LazyWeb+ harness's
   cache-hit check fails). The shared-mmap body path itself is proven (`linux_filemap_cache_body_*`: create,
   `ftruncate`, `MAP_SHARED` write, `mprotect(PROT_READ)`, `msync`, unmap, read back, remount, a soak). So the
   order is: hard links on ext2 (issue #871: an `ext2fs` link operation, VFS `link`, open-file and page-cache
   identity by inode rather than path, `unlink_open` parking when `nlink > 1`, a real `st_nlink`; unlink's
   decrement is in `ext2fs` already), then `fallocate` (`EOPNOTSUPP`).
2. **Demand-paged, shared executables.** Map `PT_LOAD` segments from the page cache (the machinery of #535) instead of
   copying them into frames: three 107 MiB programs stop costing 320 MiB at launch, ICU's 30 MB data is paged
   in only where used, and a second WebProcess shares the first's text. Expected: exec in well under a second.
3. **Cheap clocks.** `clock_gettime` is a syscall with interrupts off; JSC and WebCore call it thousands of
   times a second. Count them in P0; if they matter, a read-only time page (vDSO-like) is the answer.
4. **Fault and map costs.** From the P0 syscall and fault counters: `MAP_POPULATE`/fault-around for the JSC heap and
   Skia, bulk zeroing, merging adjacent anonymous VMAs, and `madvise(DONTNEED)` returning frames promptly.
5. **Yield loops.** WTF's lock spins with `sched_yield`; on one CPU every spin is a context switch to the thread
   that holds the lock. Count `sched_yield` per second in P0 and, if it is high, make `sched_yield` park until
   a wake or a tick when no other task is runnable.
6. **Volume headroom.** M8: one volume holds two browsers (each with its own 113 MiB UI program, plus the shared
   engine programs, in `/system/bin` and again as packages). Share the engine programs between the two browsers,
   or retire `os.lazy.webkit` (#843 item 1), and cap the HTTP cache (a size limit tied to the volume's free space).

*Done when:* H5 and H8 numbers in P0 change as predicted, `exec` of `lazywebplus-app` plus both children is under
1.5 s warm, and the revisit of a site in the fixtures issues no request that the cache could have answered.

### P4. The engine as built (1 to 2 weeks; H5, H6; each item is an A/B on the P0 table)

1. **Allocator A/B** in the WebProcess and NetworkProcess: musl malloc (today), mimalloc (MIT, thread-local heaps,
   no global lock), and WebKit's own bmalloc/libpas (`USE_SYSTEM_MALLOC=OFF`; libpas is already linked for the JIT
   heap). Keep the winner behind `LAZYOS_WEBKIT_MALLOC=`.
2. **Compiler flags:** `-O3` for JavaScriptCore, WebCore and Skia, ThinLTO if the 15 GiB container allows it; then a
   baseline of `x86-64-v2` (SSE4.2, POPCNT) only once the launchers guarantee a CPU model (#810), with runtime
   dispatch kept for the AVX2 kernels Skia and simdutf carry. PGO is a later, separate decision.
3. **ICU data filter** (locales and converters a browser UI needs): 30 MB to about 8 MB in each of three programs.
4. **OpenSSL with assembly** now that AVX state is on (#822) and AES-NI/PCLMUL are used when CPUID reports them
   (verify the dispatch under `qemu64`); libjpeg-turbo with SIMD; zlib's SIMD paths. TLS handshakes are about
   half the CPU of a cold ad-heavy page.
5. **JSC tiers for short-lived pages:** measure `JSC_thresholdForJITSoon` and the baseline/DFG thresholds on the
   script fixture. FTL stays off (B3 needs WebAssembly at this revision, W2).
6. **User agent** decision, deliberately: a LazyOS UA with a standard `Safari/`/`Version/` tail
   (`WKPageSetCustomUserAgent`) gets the real pages from Google and others, which are heavier than M1's. Do this
   *after* P1 and P2, and measure both UAs in P0 so the cost is known.

### P5. Do less work (ongoing; H7)

1. **Content blocking** (#845, landed as #851: EasyList content rule lists, per-site switch, blocked count in the status
   bar). It is the largest *per-page* lever (228 consent partners and their scripts); what is left is to measure
   it in P0 (CPU and requests with and without) and to keep its rule-list compile off the load path.
2. **Resource policy knobs**: no autoplay of animated images beyond the first frames, `loading="lazy"` honoured,
   a cap on concurrent connections per host and in total, a lower image decode budget on small machines.
3. **A "lite" fallback** per site (JavaScript off, images on demand) remembered like the notification decisions
   (`notify.tsv` pattern), reachable from the P2 bar.

## 4. Order and the first week

| Day | Work | Why first |
|---|---|---|
| 1-3 | P0 items 1-3 (counters, kernel `PERF:CPU`, fixtures); P3.1 (`fallocate`) in parallel | measurement; the cheapest real bug |
| 3-4 | P0 item 4: run the matrix, find the configuration that hangs | decides whether P1/P2 or P3/P4 leads |
| 5-8 | P1 (paint path), P2.1-P2.3 (pacing, visibility, input) | removes the spin at any CPU speed |
| 9-11 | P2.4-P2.6 (priority, per-process guard, JSC threads, the bar) | keeps the desktop alive |
| 3 weeks | P3.2, P4.1, P4.3-4, P5.1 | load time and memory |

Critical path: P0 -> P1 -> P2. P3.1, P3.3 and P4 are independent and parallelisable.

## 5. Issue board

Existing: **#845** (ad blocker: done in #851, P5.1 measures it), **#843** (retire `os.lazy.webkit`: P3.6; drop `SQLITE_OMIT_WAL`: fewer journal rewrites for the cookie jar and
storage, once the mapped `-shm` is proven), **#810** (CPU model: H6, P4.2), **#811** (XSAVE/AVX: apparently superseded by #822,
close or narrow it), **#812** and **#813** (FSGSBASE, PCID: small and only worth it if P0's syscall and switch counts
say so), **#535** (file-backed `mmap`: P3.2), **#725** and **#720** (blocking Messenger calls on UI paths: the window
thread calls the shell for toasts and `mimed`; keep them off the paint thread), **#842** (toast identity from a
thread; costs a run-loop round trip per toast today).

New issues this plan would open: *LazyWeb+: performance baseline harness and counters (P0)*; *LazyWeb+: damage-based,
paced paint path (P1)*; *WebKit port: frame-interval throttle and visibility (P2.1-2.2, patch 0020)*; *kernel: CPU guard
per address space, engine process priority (P2.4)*; *Linux ABI: `fallocate`* (P3.1); *exec: demand-paged shared
executables (P3.2)*; *WebKit port: allocator and compiler-flag A/B (P4)*; *LazyWeb+: unresponsive-page bar and
per-site JavaScript off (P2.6, P5.3)*; *LazyWeb+: user agent (P4.6)*.

## 6. Risks and non-goals

* **Throttling changes behaviour.** 30 fps animation and capped timers are visible; the interval must be a setting
  (`about:lazyweb`) and the page's `requestAnimationFrame` must still run, only less often.
* **Priorities can starve a page** while the desktop is busy; the guard's window-based demotion already shows the
  pattern works, but U3 must be judged with `xuid` compositing a moving window.
* **A bigger UA means heavier pages**, which is why P4.6 is last among the engine items.
* **No JIT or GC change makes a 1-CPU guest fast.** An SMP kernel would be the largest single gain and is a separate
  project (`arch.md`, `virtual-memory.md`: "the design assumes one CPU").
* The numbers in section 1 come from two boots of one scripted session on WHPX; they are a baseline, not a
  benchmark. The kernel sampler of P0 is the first tool that can say *where* the CPU goes.

## 7. Questions only the user can answer

1. Which setup reproduced the hang: HiDPI (`--hidpi`), WHPX or TCG, QEMU or a real PC, which window size, with or
   without a pointer moving, which page (Google signed in? a results page?), and was it a fresh or a long-lived
   profile? That picks the lead of section 4.
2. May the UA change (P4.6) so sites send their real pages, knowing they are heavier?
3. Is dropping the 1 GiB volume's headroom (two browsers) acceptable, or should `os.lazy.webkit` go now (P3.6, #843)?

## 8. Scroll jank: first measurements and the quick wins (2026-10-13, after rebasing on #851/#852)

**Method.** The UI process now prints `WEBPLUS:INFO:PAINT:` once a second while it paints (`webkit/port/app/paint.cpp`):
paints, damage reports, pixels, microseconds spent clearing, in `WKPagePaint`, converting and handing the frame to the
window, the longest paint, and `w2d`, the time from a wheel turn to the first damage report that followed (the
WebProcess's share of a scroll step). `<profile>/paint-mode` (`legacy` or `fast:<ms>`, read only by an image built with
the UI probe, because the sandboxed NetworkProcess may write that folder) switches the old whole-view path and the new
one in the same image. Session: login, LazyWeb+ from the menu, Ars Technica, accept the consent, three bursts
of 15 wheel notches 100 ms apart and a 30-notch flick. WHPX, 4 GiB, 1280x720 desktop, window 1000x450 (0.45 Mpx).

| per paint, microseconds | legacy (as before) | fast (damage rect, 32-bit conversion, paced at 33 ms) |
|---|---|---|
| clear | 144 | 0 |
| `WKPagePaint` | 966 | 581 |
| convert | 236 | 76 |
| hand the frame to the window (the mailbox copy) | 85 | 530 |
| total | 1431 | 1186 |
| paints for the 41-42 wheel notches | 60 | 47 (58 reports coalesced) |
| UI-process paint CPU per notch | 2.0 ms | 1.4 ms |
| wheel to first damage report (`w2d`), average / worst per burst | 9-15 ms / 14-42 ms | 9-19 ms / 17-36 ms |

(The committed design, one run of each path in one image with `paint-mode`, three bursts of 13-15 notches. The
mailbox copy is a 1.8 MB `memcpy` and varies run to run; `wk` and the conversion are what the change moves.)

**What it says.**

1. **At 1000x450 the UI process is not the jank.** A notch costs about 15-20 ms in the WebProcess (`w2d`) and 1-2 ms per
   paint here, one or two paints per notch. The fast path takes about a third off the UI process's share (1.4 ms
   against 2.0 ms a notch); it does not change what a person sees at this size.
2. **Per-pixel costs scale with the window.** Every stage that touches the frame (`memset`, `WKPagePaint`, conversion,
   the mailbox copy, xui's premultiply and opaque scan of each new `Image`, its copy to the canvas, the compositor)
   grows with the page area: x1.6 for a maximised window at 1280x720, x3.4 for an unmaximised one at 2560x1440 and
   x7 maximised. The legacy path's 1 ms of conversion is about 7 ms per frame maximised at 2560x1440; the window
   side is not yet measured.
3. **Wheel input is 40 px per notch** and WebKit reads only the sign of an axis event (`WebEventFactory.cpp`), so a
   notch is one line step and one repaint of a scrolled view. Sending two events per notch (80 px) was tried and
   taken out again: it changes the feel, breaks the harnesses' 200 px scroll judge, and nothing measured needs it.
   A larger step belongs in the engine (a precise-delta axis event, `WKPageHandleWheelEvent`) with the judges.
4. **Page repaints are the rest.** Pages with a fixed or sticky header cannot use the blit scroll and repaint the
   whole view through Skia for every step (`scrollContentsSlowPath`); nothing in the UI process changes that. Only
   the tick sampler of P0 can say how much of `w2d` is raster, layout or script.

**Same pixels, much less work when little changes (the committed version, a test page with a text field, hover and
click effects, a grid and a fixed header; `legacy` and `fast:33` in two boots of one image).** The nine screenshots of
a session (typing, a click, hovering a box and a cell, wheel down and up) differ in no page pixel (only the taskbar
clock). A blinking caret, idle, painted 900,000 pixels a second in `legacy` (the whole view twice) and 364 in `fast`
(two 182-pixel rectangles); the hover and click paints were 364-450,182 pixels against 900,000.

**In this change:** the paint rewrite and counters (`paint.cpp`, `engine.h`, `engine.cpp`) and `PageView::paint` no
longer filling the view white under a frame that covers it (`xui-app/crates/webkit/src/view.rs`, with
`tests/page_fill.rs`). **Proposed for xui** (`va1erian/xui`,
`xui-canvas/src/image_cache.rs`): `premultiplied()` is a scalar loop with three multiply-divides per pixel and
`pixmap()` then scans every pixel again for `opaque`, for each new image id. A frame is opaque: copy a row whole
when its alphas are all 255 (one pass, auto-vectorised) and take `opaque` from the same pass; at 2560x1250 that
pass is the largest controllable cost left on the window thread.

### The frame at 2560x1440, with the window and the compositor counted (2026-10-17)

The paint thread's counters above leave out the window thread, `xuid` and the kernel. `python tools/webplus/frame_run.py [--perf]` (scroll page `webplus/scroll.html`, 2560x1440 at scale 2, a 2000x900 window, WHPX) adds them from `PERF:CPU` and the kernel's `PERF:present_mpx`: **about 28 ms of CPU per frame at 23 paints a second, and every paint is the whole window** (1.80 Mpx), for a scroll that exposes a few hundred pixels. The UI process is 44 to 46% of the core, of which the counted paint stages are only 8 to 10 ms a paint: the rest is the window thread, still uncounted. This is the case for P1 item 1 (damage) applied to scrolling (a blit-scroll of the persistent bitmap and a paint of the exposed rows), ahead of any GPU stage. The numbers and what they mean for the GPU plan are in [`kabylake-gpu-plan.md`](kabylake-gpu-plan.md) §11.

### Scroll as a delta (2026-10-17): what was built, and what is not yet measured

**Cause, read from WebKit's source (`DrawingAreaProxyCoordinatedGraphics::incorporateUpdate`).** The web process
already paints only what a scroll exposed, and the UI process already scrolls its backing store itself. But an update
that scrolled is reported to the page as *the whole view needs display*, so `paint.cpp` copied (`WKPagePaint`),
converted and handed on 1.8 Mpx per step; the window then built a new image from them, which xui premultiplies and
scans in full (an image never changes, and a frame stream never hits its cache); `xuid` composed and presented the
whole window. `BackingStore::scroll` also allocated, read and wrote back a bitmap the size of the scrolled area for
every step (megabytes, with the page faults, on a kernel that maps every large block).

**What changed.**

| Where | Change |
|---|---|
| `webkit/patches/0024` | `WKPageSetUpdateCallback`: each update's scrolled rectangle, offset and repainted rectangles, with no "whole view" report while it is set; `BackingStore::scroll` moves the pixels in place |
| `xui-app/crates/webkit/src/damage.rs` | `Pending`: the updates between two paints as one scroll and at most eight rectangles, or a whole frame (two different scrolled areas, a scroll further than the area, rectangles covering four fifths); pending rectangles move with a scroll, in both places (a superset, never a subset) |
| `.../mailbox.rs`, `ffi_paint.rs` | `lzw_ui_frame_delta` and `lzw_paint_*`; deltas queue behind the picture they belong to under one lock; a window too far behind, or without a picture, is answered "send a whole frame" |
| `.../parts.rs`, `view.rs` | the picture is the first whole frame plus the rectangles painted since, each an image with an offset and a clip; a scroll moves offsets and clips, so xui draws the images it has already prepared and only the exposed rows are new; past 32 parts a whole frame replaces them |
| `webkit/port/app/paint.cpp` | asks for the plan, paints only its rectangles, sends a delta; `legacy` mode (`<profile>/paint-mode`) keeps the old whole-frame path for comparison |
| `xui-app/webplus/src/frameclock.rs` | `INFO:WINDOW` once a second: updates, apply and draw time on the window thread, parts |

**Verified (host).** `cargo test -p xui-webkit` includes `tests/scroll_model.rs`: a model of WebKit's backing store (move
the area, then paint the update's rectangles, exactly as `BackingStoreSkia.cpp` does) drives the real accumulator, the
mailbox and the parts through 400 seeded random sequences (scrolls of a whole view, of part of it, sideways, further
than the area, loose rectangles, the engine and the window running at different paces) and compares the picture with the
store pixel for pixel; breaking the accumulator's move of pending rectangles, or the sign of a part's offset, fails it
at the first seed. `tests/scroll_view.rs` renders the real `Browser` offscreen and reads pixels back. What the model
takes from WebKit (the direction of the offset, that the exposed rows are in the update's rectangles) is read from the
source above, not run: the engine run (below) is what checks it.

**Measured (QEMU, WHPX, 2560x1440 at scale 2, the 2000x900 window scrolled on `webplus/scroll.html` for 25 s; two runs,
the same numbers within 1%).** `python tools/webplus/frame_run.py --perf`:

| | before (whole frames) | after (deltas) |
|---|---|---|
| CPU per painted frame | **28.7 ms** (66% of the core at 22.9 paints a second) | **17.0 ms** (45% at 26.6 a second) |
| sustainable frames a second on this core | about 35 | about 59 |
| paints that were whole frames | all | 1 or 2 of about 585 to 665 (26.6 a second for 25 s) |
| pixels sent per paint | 1.80 Mpx | 0.17 to 0.18 Mpx |
| window thread, per update | about 10 ms (inferred) | apply 47 us, draw 2.1 ms (`INFO:WINDOW`), at most 22 parts |
| the engine's check against a whole paint | not run | 21 to 106 checks, 0 differed |

**How the CPU figure is made, and what it leaves out.** `frame_run.py` adds up the per-process averages of `PERF:CPU`
lines, which the kernel prints only for processes at or above 20% of the core and only the busiest five, and each
average is over the looks where that process appeared. It is a thresholded estimate: a process that is busy only
now and then counts for more than it used, and one below 20% (after the change the page's process and `xuid` sit
near it) not at all. The before/after pair is made the same way, so the *difference* is meaningful, the absolute
17.0 ms is not a clean budget, and the split in the paragraph above it is from the stage counters, not from this sum.
The probe image also runs the engine check below (a whole `WKPagePaint`, a conversion and a compare on the first delta
of every second and every 30th after: about 1% of the core), which `PERF:CPU` counts and `INFO:PAINT` stages do not.

The check is the one that matters for trust: in a probe image the UI process keeps the picture the window should have
and, every 30th delta, compares it with a whole paint from WebKit's backing store (`paint.cpp`, "the check"; a
difference is `FAIL:PAINT:MISMATCH` and the harness fails). It held what the host model had to take from WebKit's source
(the direction of the offset, that the exposed rows are among the update's rectangles) against the real engine.

**A trap in the stage times.** `INFO:PAINT`'s `wk`, `swz` and `post` are wall time around each call, and on one CPU
the scheduler runs other tasks (the window, the compositor, the page's process) inside them. A one-pixel `WKPagePaint`
read 3 ms there, while counters inside WebKit (patches `0025`, `0026`, `INFO:WKPAINT`) put the call, destructors
included, at about 64 us. Read the cost of a change from CPU per frame (`PERF:CPU` with the paint rate, which
`frame_run.py` prints), not from the stages; a first reading of this work as "the paint got slower" was this.

**What is left per frame (17 ms, on a fast host).** `xuid` about 6.8 ms (compose and present of a window that still
changes whole on screen, of which the kernel's copy is about 0.8), the UI process about 8 ms (the window's 2.2, the
paint thread's 2 to 3, the main thread's `incorporateUpdate`: the IPC, the in-place scroll and the strip's paint, which
nothing counts yet), the page's process about 2.3 ms. The compositor is now the largest single piece.
### A build trap: two checkouts, one Docker volume (found when this change was first tried)

`webkit/README.md` says two checkouts must not build WebKit in the same Docker volume. The quiet failure is not a
rebuild of everything but **no rebuild of a C++ file**: ninja decides by modification time, the volume held
`paint.cpp.o` compiled from the main checkout at 20:17, and the same path in another checkout had a source of 20:11,
older, so the object was linked as it was. The program built from this change's tree had none of its strings
(`grep -a INFO:PAINT lazywebplus-app`: nothing). Use `LAZYOS_JSC_VOLUME=<name>` per checkout (copy a finished one with
`cp -a`), or `touch webkit/port/app/*.cpp` before the first build in a checkout, and look for `INFO:PAINT` in the
serial log to know which program is running.

### The NetworkProcess spin (a log from a HiDPI session, not yet reproduced)

A log of a HiDPI desktop (home volume on a second disk, a profile that had run before) shows, after Ars Technica
finished loading at +352 s, `SCHED:GUARD:DEMOTED task=fork slot=41` every second for 28 s (33 demotions); slot 41 is
`NETWORKPROCESS:UP:41`. The WebProcess (slot 39) was demoted once. There was no disk traffic in those seconds
(`block:` counters unchanged), so it is a pure user-mode spin of the NetworkProcess's first thread, with `xuid` and
`inputd` also demoted while the window was being resized (`RESIZE:1664x900`, `860x900`, `2061x900`, `988x900`,
`2375x900`). Before the load finished the NetworkProcess logged 45 `ENOSYS 285 fallocate` in 6 s. A fresh profile at
1280x720 (this plan's sessions) does not show it. `python tools/webplus/sample.py` (a second QMP port and
`info registers` at 30 Hz: CR3 names the process, RIP symbolizes against the unstripped program) is how to see
where it is; `tools/webplus/README.md` has the commands.

### The startup crash (M9, section 1): about half of the launches (issue #853)

Two of four launches of LazyWeb+ in this harness died before `UP:PASS` (status 139, `INIT:APP:FAILED
startup=1`), one with the program the image shipped (built before this change) and one with a rebuilt one. It is the
crash `docs/lazywebplus.md` lists under *Known limits* as seen once and not reproduced. Both traces:

* a write fault, `PROTECTION_VIOLATION | CAUSED_BY_WRITE | USER_MODE`, in `memcpy` of a 1,800,000-byte frame (the
  window's first resize, 1000x386 to 1000x450), at the first byte of a 2 MiB-aligned region
  (`0x100042000000`, `0x100042a00000`);
* the destination is the block `mremap` returned a few syscalls earlier (`trace syscall 25 a1=0x1000417b4000 ->
  0x10004290e000`; the destination register is that block plus 0x10), so the Vec grew by `realloc` and
  `mremap`, which `sys_mremap` serves by relocating to `first_free_range` (the lowest hole, not the fresh bump of
  `mmap`).

Hypothesis, to be proven by a kernel test that grows a mapping across a 2 MiB boundary after a `fork`:
`remap_page` and `try_map_page_in` create page-table levels with `map_to`, whose parent entries take only the
leaf's own `PRESENT|WRITABLE|USER` bits, so a copy-on-write (read-only) leaf creates a non-writable PD or PT entry,
and `cow_fault` later makes the leaf writable in place without touching its parents; the CPU keeps faulting and
the handler (leaf present and writable, "not COW") ends in SIGSEGV. The UI process forks the engine's children,
which is when its pages become copy-on-write. The fix would be in `kernel/src/mem/uspace.rs`: map with
`map_to_with_table_flags(..., PRESENT | WRITABLE | USER_ACCESSIBLE)` for user tables, and repair existing parents.
A user-space mitigation (allocating the mailbox Vec afresh instead of growing it) avoids `mremap` but not the cause,
and the WebProcess (JavaScriptCore reallocates large buffers all the time) is exposed to it too.

## Scroll latency on a real page (Ars Technica, 2560x1440, one CPU)

Measured with `tools/webplus/live_run.py --build --scroll 40 --sample-port N` (the real site over the host's network)
and `tools/webplus/cpu_profile.py` (the 30 Hz sampler's recording by function). QEMU/WHPX, one host, one guest CPU.

* **The UI side is not the limit.** A paint costs about 5 ms in the UI process and a window draw about 9 ms;
  `INFO:PAINT` stage times are wall time on one CPU, so judge by CPU. What a wheel notch waits for is the
  WebProcess: wheel-to-first-damage averages 88-92 ms and reaches 450 ms (`w2d`), and a notch is one paint,
  so scrolling runs at 5-12 paints a second however fast the rest is.
* **The WebProcess uses 25-50% of the CPU with no input at all** (the page's own timers and ads), and about twice
  that while scrolling. By function it is JavaScript and GC first (about half when idle, a third to a half while
  scrolling: LLInt, DFG compiles, marking), then WebCore layout and paint, then Skia (`bilerp_clamp_8888`: images
  scaled on every paint). There is no single hot function.
* **Whole frames:** 340 of 430 scrolling paints were whole frames (the page has fixed elements, so
  `LocalFrameView::scrollContentsFastPath` repaints); a page without them (`webplus/scroll.html`) is all deltas.
  Open: keep the scroll delta and repaint only the fixed elements' rects.
* **Fixed, but not the limiter:** zig's `malloc` (`std.heap.SmpAllocator`) has one slot per CPU behind a spin mutex
  that never yields, and the guest reports one CPU. A thread preempted inside `malloc` made every other thread
  that allocated spin for a whole time slice: 17-19% of the WebProcess's user samples were in `Thread.lock`. The
  build now patches the toolchain to at least 16 slots (`tools/webkit/zigfix.py`, applied by `port_cmake.build`,
  which links the programs again once). The samples in `Thread.lock` went to zero; wheel-to-damage did not change
  (88 ms against 92 ms), because the spinners were using time nothing else wanted.
* **Fixed:** the I/O APIC redirection entry was read back (four VM exits) every time the timer interrupt asked
  whether a line was masked; 20% of wall-clock samples. `arch/ioapic.rs` keeps a table of what it wrote.

## Scrolling ahead of the page (checkerboard)

**Off by default since issue #940 (2026-10-21).** On the Alder Lake box with `/home` on the USB stick the
page answered late often enough that the picture shown ahead had to jump back two to thirteen times a
minute, which the user found worse than a view that waits; `xui_webkit::speculate::set_ahead(true)` turns
it back on (its tests do). Off, a notch moves the view only once the page has scrolled, gliding there when
the page's move comes within `ANSWER_MS` of the notch.

The page answers a wheel notch 70 ms later on average on Ars Technica, 480 ms at worst, so a notch used to
be seen that late. The window now moves the picture it has at once, as the first smartphone browsers did, and
leaves what that uncovers as a checkerboard (64 px squares, light grey, fixed to the page's rows) until the
page paints it. `xui-app/crates/webkit/src/speculate.rs` (the rules, pure numbers), `picture.rs` (drawing),
`view.rs` (wheel in, timer, update in).

* **What "ahead" is.** A plain vertical wheel adds 40 px a notch (WebKit's line step) to `pending`. Each update
  carries the main frame's scroll offset its pixels were painted at (patch 0028, `WKPageSetScrollOffsetCallback`:
  `PageDidScroll` arrives just before the update that repaints for it, on one connection; `paint.cpp` notes the
  offset at each update and `lzw_ui_scroll_offset` sends it with the post, staged in the mailbox under the frame's
  lock so it always goes with its pixels). A change of that offset is the page having done that much: `pending`
  gives it back. Never more than 480 px ahead, and never above the page's top.
* **When the page does not follow** (the end of the page, a box that scrolls by itself, a handler that cancels
  the wheel): a burst the page has not moved for 900 ms is dropped and that direction is not shown ahead until the
  page moves; whatever is left 500 ms after the last notch or update is dropped (500: the page's slowest answer
  seen was 450 ms, and a shorter settle made the picture jump back and then forward). Only an update that moved the
  page's offset counts as the page answering: a video or an ad that keeps repainting must not keep a shift alive. The
  100 ms timer that does the dropping runs only while the picture is ahead (an idle page costs no wakeups). Every
  number the web process sends (offsets, scrolls, rectangles) is clamped at the C ABI and combined with saturating or
  64-bit arithmetic (`tests/speculate.rs`, "hostile offsets"); patch 0029 does the same for `BackingStore::scroll`.
* **The scroll bar stays put.** WebKit paints its 7 px scroll bar into the page's picture, so shifting the picture
  dragged the thumb ahead and back on every answer. The picture's last 7 columns (`SCROLLBAR` in `picture.rs`) are
  drawn unshifted: the thumb moves only when the page does.
* **Known costs.** What is fixed to the window (a sticky header) moves with the picture until the page answers,
  then jumps back. The first burst at the end of a page shows a short checkerboard before it is dropped. A click
  during the shift lands a few tens of pixels off for as long as it lasts.
* **Measured** (`live_run.py --scroll 40` on real Ars Technica, 2560x1440, QEMU/WHPX, `INFO:SPEC`): a notch is
  drawn 7.9 ms after it arrives on average (142 ms worst, a busy window thread) against the page's 69 ms average
  (483 ms worst); 353 shifts, no jump back; on the synthetic page 5 ms against 10 ms. The page still paints at
  the same 5-12 paints a second, so the checkerboard is what shows between: this is responsiveness, not more
  frames, and not less work for the page.
* **Tests.** `tests/speculate.rs` (rules, a 400-seed simulation of a late, bursty page with limits, mutation-checked),
  `tests/scroll_ahead.rs` (the pixels: shifted picture with a checkerboard strip, exact again after the answer,
  nothing for a modified wheel or up from the top; `XUI_SNAPSHOT=<dir>` keeps the picture).
  `INFO:SPEC:shifts:snaps:snap_px:blocked:w2s=<avg>/<max us>:ahead` once a second while in use.

## 9. Merged pointer moves on Ars Technica (2026-10-17, issue #890)

**Method.** `python tools/webplus/pointer_bench.py`: one image (LazyWeb+, the UI probe, the real Mozilla roots), five
boots on WHPX, 4 GiB, 1280x720, window 1000x540. Each boot writes `<profile>/pointer-mode` (`each`: every move
forwarded as before #890; `merged`: one slot, one queued task), opens https://arstechnica.com/ (the 228-partner
consent dialog on top), reloads it with F5 while the pointer sweeps the page in one-packet steps at about 200 Hz, and
sweeps it again 10 s after the load. Boot 0 warms the cache and the ad blocker's lists and is not counted; the others
alternate. The numbers are the browser's own: `INFO:PAINT` (`mouse=`: events WebKit handled; it now also prints
for a second with only pointer events), `INFO:MAINLAG`/`STALL:main`, `INFO:CPU`, `UI:STALL`.

| phase | mode | boots | events handled/s | paints/s | `WKPagePaint` ms/s | UI CPU % | web CPU % | main-thread lags >= 250 ms | `UI:STALL` |
|---|---|---|---|---|---|---|---|---|---|
| reload (3-5 s) | each | 2 | 104, 52 | 1.8 | 5.0 | 42 | 31 | 0 | 0.5 |
| reload (3-5 s) | merged | 2 | 69, 48 | 1.2 | 8.1 | 41 | 29 | 0 | 0 |
| settled (15 s) | each | 2 | 150 | 0.1 | 0.0 | 0 | 0 | 0 | 0 |
| settled (15 s) | merged | 2 | 150 | 0.1 | 0.1 | 0 | 0 | 0 | 0 |

**What it says.** In this configuration the change is not measurable. WebKit's main thread never fell 250 ms behind in
either mode, so moves did not pile up: settled, both modes handed WebKit every move the sweep made (about 150 a
second), as merging intends when the thread keeps up. The warm-cache reload lasts only 3-5 s and varies more between
boots than between modes. Merging pays off only where the main thread stalls while the pointer moves (H4): HiDPI,
TCG or a slow real PC, a cold profile, a page busier than Ars behind its consent dialog. Those are the next runs
(`--accel none`, a `LAZYOS_DISPLAY_MODE=2560x1440` image).

## 10. Would SMP help, and what to do first (2026-10-17, corrected the same day)

**What the ledger said.** `PERF:runwait` (new, `LAZYOS_PERF=1`, `kernel/src/perf/runwait.rs`) keeps, per task, the
time between becoming runnable (woken, or preempted with work left) and being run. Live Ars Technica at 2560x1440,
38 s of wheel scrolling, one guest CPU, no sampler (`live_run.py --build --scroll 40`, read with
`tools/webplus/scroll_stats.py`):

| task | waited runnable | share of the window | waits | mean wait |
|---|---|---|---|---|
| WebProcess main thread (slot 36) | 12.5 s | 33% | 24 967 | 0.5 ms |
| the WebProcess's other threads (JIT, GC, workers) | 5-12 s each | 10-32% | 5 000-25 000 | 0.2-1 ms |
| `lazyshell` (the desktop shell) | 7.8 s | 20% | 25 703 (670 a second) | 0.3 ms |
| `lazywebplus.elf` (the window) | 3.9 s | 10% | 17 080 | 0.2 ms |
| `netdrv` | 17.8 s | 46% | 3 366 | 5 ms |

The WebProcess used 44-50% of the CPU, about half of it the main thread: it ran for about 10 s and waited for 12.5 s.
The first reading of that was "the page's answer is that thread's serial work, so a CPU of its own would cut
wheel-to-damage (70 ms on average) by up to 2x, and SMP is the lever". **The experiments below show that reading is
wrong.**

**Experiments** (same page, same scroll, `LAZYOS_PERF=1`, each a throwaway kernel change, none kept; live Ars Technica
varies from run to run, w2d by about 20 ms between identical builds, so read only differences far outside that):

| change | main thread waited runnable | page answer (w2d), average / worst | paints a second |
|---|---|---|---|
| none (baseline) | 12.5 s, 33% | 70 / 446 ms | 11.2 |
| the CPU guard can no longer demote a `Normal` process (`NORMAL_BUDGET_TICKS` 10 of 10): 0 demotions | 12.5 s, 33% | 89 / 491 ms | 11.1 |
| every thread but a process's first at the minimum weight (helpers at half the main thread's share) | 12.3 s, 32% | 89 / 477 ms | 11.5 |
| the same-class minimum slice 1 ms -> 200 us (twice the switches: 44 000 waits) | 12.9 s, 34% | 76 / 426 ms | 10.8 |
| **the WebProcess's main thread in the `Interactive` class** (run 1, run 2) | **1.6 s, 4%** / 1.8 s, 5% | **68 / 491 ms** / 67 / 491 ms | 11.6 / 11.9 |

Three different policies leave the main thread's wait where it was (it behaves like a conserved quantity on a CPU
that is busy: whenever the thread is not running something else is), and the one that does take it away, strict
priority, takes it from 33% to 4% **without moving the page's answer at all**. So the main thread's wait for the CPU
is not on the critical path of a wheel notch. What is: the thread's own work for that notch and whatever else sits in
front of it in its run loop (the page's timers, scripts and layout; an idle page already costs 25-50% of the CPU), the
hops around it (the UI process's own queue, the IPC through the kernel), and WebKit's flow control: one wheel event is
in flight and the rest are coalesced, so the paint rate is about the reciprocal of the answer time (11 a second at
70-90 ms). Cutting the answer time is what raises both.

**What that does to the SMP verdict.** The "up to 2x" came from reading wait as lost latency. It is not: priority
removed 90% of the wait and the latency did not change. A second CPU would help what is *throughput* (the window's
draws, the compositor, the JIT and GC helpers would run beside the main thread, and four serial stages in four
processes would overlap), and it would stop the idle burn of one process from slowing another, but nothing measured here
says it would shorten wheel-to-damage, and the experiment that could have shown it did not. SMP stays what it was, the
plan's XL, high-risk stage (AP bring-up, per-CPU run queues, TLB shootdown on every PTE change, the page cache's
single-CPU arguments and the fault epoch, 154 interrupts-off sections and 264 `static mut`/`Mutex` uses to review in
856 kernel files), and it is **not** justified by the scroll latency. What would justify it is a workload that is
saturating the CPU with independent work; this is one thread's serial path.

### Follow-ups, ranked (revised after the experiments)

1. **Decompose the page's answer time** (days; a few timestamps). 70 ms is wheel -> UI main thread -> IPC -> the web
   process's main thread -> its scroll, layout and repaint -> IPC -> the UI process. Stamp each hop (the UI process
   has `lzw_engine_wheel` and the update callback; the web process can log when it handles the event and when it
   sends the update) and see which one is the 70. The main thread's CPU per paint is about 24 ms (an estimate: half of
   the process's 47% of the core, at 11 paints a second), so perhaps half of the answer is not CPU time on that thread: it is queueing behind the
   page's own events, or blocking (a lock, a worker, a synchronous IPC), or the transit. Nothing below is worth doing
   before this says where the time is.
2. **The page's idle burn and its run loop.** Whatever is in the web process's main thread when the wheel arrives
   delays it: timers, scripts, animation. Throttle what a visible but idle page may do (`requestAnimationFrame` and
   timer clamping when nothing is interacting, fewer JIT tiers), and see whether the answer time falls with it.
3. **Keep the scroll delta when the page has fixed elements** (days; WebKit patch + `damage.rs`). 340 of 430 scrolling
   paints on Ars were whole frames (1.5 Mpx each), which adds work to the very thread of item 1.
4. **Wake storms.** `lazyshell` is woken about 670 times a second and `netdrv` polls (46% runnable) while the page
   scrolls. They do not slow the answer (priority showed that), but they are a standing cost on an idle desktop:
   `PERF:sched` counts wakes per task; make them event-driven.
5. **The window thread's draw** (9-12 ms per draw at 2560x1440, `INFO:WINDOW`), `xuid`'s 8-11% and the kernel present
   copy (`PERF:present_mpx`, 0.4 ms a megapixel on the host; the G6 decision wants the box's number).
6. **Cheaper memory primitives in the kernel** (`compiler_builtins` `set_bytes`/`copy_forward` were 5-7% of kernel-mode
   samples while scrolling): `rep stosb`/`movsb` with ERMS, or AVX for page zeroing and copies.
7. **Shown-ahead polish**: the fixed header moves with the picture until the page answers; a horizontal scroll bar
   moves too; a click during the shift lands off.
8. **Box measurements.** Nothing here was run on the Kaby Lake box: `PERF:present_mpx`, `PERF:CPU`, `PERF:runwait`,
   `INFO:PAINT`, `INFO:WINDOW`, `INFO:SPEC` during 20 s of scrolling decide G6 (`kabylake-gpu-plan.md` section 11).
9. **SMP**: only if a workload turns out to be CPU-saturated with independent work (see above).

Closed by the experiments: raising the page's priority, the CPU guard's budget, helper-thread weights and the
scheduler's minimum slice. None of them changes how fast a wheel notch is answered. The `Interactive` class for the
main thread does remove its wait, and may still be worth having for another reason (a busy desktop starving the
page), but it is not a scrolling fix and was not kept.
## 11. Smooth scrolling at 2560x1440 on live pages (2026-10-19)

**Goal and how it is measured.** 60 frames a second while scrolling Ars Technica and webkit.org in a maximized
window at 2560x1440, 30 accepted if it has no jank. `python tools/webplus/scroll_bench.py --build` boots a
`LAZYOS_PERF=1` image at that mode, maximizes the window, opens each live page, accepts Ars Technica's consent
dialog, lets the page settle 25 s and turns the wheel for 20 s (3 notches a step, 50 ms between steps, 60 notches down then 60 up).
The window reports what the user sees (`INFO:SMOOTH`, `xui-app/crates/webkit/src/smooth.rs`): frames drawn a second
while the wheel is active, the longest gap between two, gaps over 33 ms, and how many of the rows shown were
checkerboard. Scrolling is under way from a notch that moved the picture (a notch against the end of the page draws
nothing and is not jank) until 250 ms after the last one, and the first gap of a burst counts from its first notch.
Live pages change between runs: compare runs made close together. One guest CPU, WHPX. Earlier rows were measured
before these two rules and count gaps against the ends of webkit.org, a short page the bench reaches the bottom of.

| run | Ars: frames/s | Ars: gaps > 33 ms | Ars: checkerboard | webkit.org: frames/s | webkit.org: gaps > 33 ms |
|---|---|---|---|---|---|
| before (the work of sections 8 to 10) | 21 | 150 in 10 s | 18.6% | 19 | 144 in 10 s |
| bands, glide, direct painter, display rate, fixed layers, cookie jar, alt-svc | 74 | 28 in 18 s | 2.8% | 49 | 104 in 17 s |
| plus the epoll fix (the window woken by the engine) | 80 | 25 in 18 s | 3.1% | 55 | 117 in 17 s |
| all of the above, the end of the page, and the meter's two rules | 81 (longest gap 75 ms) | 26 in 17 s | 1.4% | 50 (longest gap 86 ms) | 13 in 17 s |

**What was built.**

* **Overscan bands** (WebKit patch `0030`, `bands.rs`, `paint.cpp`): the web process paints a view's height above
  and below the view after each update, in 512K-pixel chunks, and the window keeps them. Scrolling ahead of the page
  (section 8) now shows real rows instead of a checkerboard as far as the bands reach (`Speculation::set_reach`, up
  to three views).
* **Glide** (`glide.rs`, `view_input.rs`): a wheel notch moves what is shown towards the target with a 45 ms time
  constant on a 16 ms timer, instead of jumping 40 px per notch; the page's own moves are followed at once.
* **Direct painter** (`xui-app/src/backend/direct.rs`, `raster.rs`): the page node writes its pixels straight into
  the window's frame when nothing is drawn over it. xui's canvas path premultiplied and scanned every new image and
  copied the damage twice; at 3 to 4 megapixels a frame that was most of a frame's cost. `raster.rs` is checked
  pixel for pixel against the canvas painter (`tests_raster.rs`). The display client's blit swizzles a row in one
  pass (`user/src/messenger/display/canvas.rs`).
* **The page's display rate and fixed layers** (patch `0031`): Ars Technica's consent overlay and ad slots are
  full-view fixed layers that paint nothing, and every scroll step repainted the whole view for them; only what
  they paint is repainted now. An animation (webkit.org's header, 46% of the view) is displayed at most every
  33 ms, a scroll never waits.
* **Synchronous cookie reads** (patch `0032`): `document.cookie` is a synchronous message to the NetworkProcess,
  whose main thread also wrote the cookie jar with an fsync per statement, and on LazyOS an fsync flushes the whole
  volume (`LOCK:WAIT ... held_in=` names the holder's syscall now). The page stopped for 200-700 ms at a time.
* **curl's alt-svc file** (patch `0033`): a temporary file and a rename after every request; an ext2 rename
  without a journal orders itself with two flushes of every dirty block under the VFS lock (500-750 ms seen).
* **JavaScriptCore's collector** on one CPU runs stop-the-world (`engine.cpp`, `sysconf(_SC_NPROCESSORS_ONLN)`):
  the concurrent marker had no core to itself and throttled the page's main thread while it marked.
* **The window was not woken by the engine** (kernel, `task::fd_close`): the window parks on an epoll set holding
  an eventfd doorbell that the engine's thread rings (`xui-app/src/backend/wakeup.rs`). `close` dropped every
  interest registered under the closed number from every epoll instance in the closer's table, and an instance
  inherited across `fork` is the same object: the first engine process's `exec` (which closes its close-on-exec
  copies) took the doorbell out of the window's set. From then on the window woke only on compositor messages and
  its 1 s idle timer: on webkit.org the engine posted 20 updates a second and the window took one, so the mailbox
  overflowed into whole frames. An interest now leaves only when no live task holds both the instance and that
  file under that number, as the exit path already did, and closing an instance (or the exit of a task holding
  one) drops the interests nobody owns any more, so one the parent gave up while a child still held it does not
  keep its file open for good (`pipe_epoll_child_close_keeps_parent_interest`, `pipe_epoll_unowned_interest_leaves`,
  `pipe_soak_epoll_fork_close`). This affected every xui app that starts a process after its first window.
* **The end of the page** (`Speculation::set_at_bottom`, `paint.cpp`): the window scrolled ahead past the bottom
  of a short page (webkit.org) into rows that do not exist and showed a checkerboard until it gave up 900 ms later.
  The UI process now sends `WKPageIsPinnedToBottomSide` with each update's offset, and nothing further is shown
  ahead downward from a picture at the bottom. That was the first version, and it did not hold: it let the
  notches through when `reach_down` was not zero, and it is not zero at the end of webkit.org (29 rows: the engine
  paints a few rows past the view's edge even at the document's end), so the notches against the end took the
  picture 480 rows ahead and 450 of them were checkerboard, the hole in the screenshots of the bench's
  `site1_scrolled`. A diagnostic line per draw with checkerboard (offset, ahead, `at_bottom`, `reach_down`) showed
  `at_bottom=true down=29 pending=480`. Pinned with no more than `END_SLACK` (64) rows of bands below the view now
  means the end: nothing further is shown ahead, and what is ahead is dropped when the page says so. Bands that
  reach further say the page goes on (WebKit tells the pinned state again when the page scrolls, not when it grows,
  and it is `true` from the first load until the first scroll). Tests: `speculate.rs`
  (`at_the_end_a_margin_of_bands_below_the_view_is_not_a_page_that_goes_on`) and the pixel test
  `tests/scroll_end.rs`, its own binary because the bands wait in one process-wide queue. webkit.org's checkerboard
  went from 7.3% of the rows shown to 0.0%. The cost: the first notch after a load, with no bands yet, is not
  shown ahead (the pinned state is stale-true until the first update), and a notch at the end of a short page
  shows nothing. The bench takes a second shot three seconds after the scroll (`site<n>_settled`), which is what
  shows a hole that stays.

**A build trap found on the way.** Removing a WebKit patch re-extracts the tree, but `tar` gave every file the
archive's time: a file the removed patch had changed looked older than its object, ninja kept the patched object, and
the "clean" build still printed the removed patch's lines. `tools/jsc/toolenv.py` extracts with `tar -m` now, so a
re-extracted tree rebuilds everything, as `port_tree.py` already assumed.

**Diagnostics kept.** `PERF:BLOCKED` (`kernel/src/task/blocksample.rs`, `LAZYOS_PERF=1` only): every 5 s, where the
browser's threads were when they were not running (syscall and kind of wait, 50 samples), which is how the cookie
and alt-svc waits were found. `LOCK:WAIT` gained `held_in=`. `INFO:SMOOTH` and `scroll_bench.py`.

**Left to do** (each its own change):

1. **ext2 metadata under the global VFS lock.** A file rename flushes every dirty block of the volume twice and an
   fsync flushes the volume, both while holding `abi-vfs`: one program's fsync stalls every other program's file
   calls for hundreds of milliseconds. The two WebCore patches avoid the browser's own; the kernel should flush only
   the file (and order a rename with a journal or a narrower barrier).
2. **Whole frames on webkit.org.** 3 to 4 a second still reach the window while scrolling (the animation and the
   scroll in one update), and drawing one costs 50-80 ms at this size: the long gaps that remain. Keep the delta
   when an update both scrolls and repaints, or composite the animated element apart.
3. **Two hangs seen once each.** A WebProcess waiting for good in `GetNetworkProcessConnection` at startup was the
   kernel refusing WebKit's hand-off of a new NetworkProcess connection whose queue already held a handle
   (`SCM_RIGHTS`, `ETOOMANYREFS`, issue #865). The cycle check is now exact (`kernel/src/ipc/pipe/reach.rs`), and
   `python tools/webplus/relaunch_run.py` closes and starts the browser until it would show again. Still open:
   input stopped reaching the window after Ars Technica loaded (the input services alive), not reproduced.

## 12. Frame pacing and where a scrolling frame's CPU goes (2026-10-19)

**Measured first.** `python tools/webplus/frame_run.py --perf --sample-port 4499 --sample-stacks` (the long text page,
2560x1440, the wheel for 20 s, WHPX, one CPU). Two new meters: `XUID:COMPOSE` (always on, every 5 s: the compositor's
repaints, megapixels, and the time composing and presenting them, `user/src/bin/xuid/meter.rs`) and the sampler's
stacks (`tools/webplus/sample.py --stacks` keeps the user stack of each sample; `cpu_profile.py --callers memcpy`
names who called a hot leaf).

**What it showed.** While the page scrolled, the LazyWeb+ window presented **about 100 whole frames a second**
(`INFO:SMOOTH frames=101..108`): its 16 ms glide timer, plus each of the engine's 25 updates a second drawn at once.
The screen refreshes at 60 Hz, so four frames in ten were never seen, yet `xuid` composed and copied 1.8 megapixels
for each (`XUID:COMPOSE repaints=512 kpx=921600` per 5 s) and the window rastered and copied each into its slot.
Most of the UI process's time was those copies (`memcpy` 58% of its samples), and `xuid`'s was its blit.

**Frame pacing** (`xui-app/src/backend/pacing.rs`). A client window presents at most once per screen refresh;
damage that arrives sooner waits in the damage map and the loop parks until the slot (`park_client`), so the next
present carries all of it. Presents keep their cadence (a late wake does not push later frames back). The interval
is `GetOutput.refresh_millihz` (new field, `idl/display.midl`): `xuid` answers with `gpud`'s measured rate when
`gpud` drives the display (`GetBeam.measured_millihz`, `user/src/bin/xuid/refresh.rs`, a bounded call asked again
at a few later `GetOutput`s, since `gpud` may start after the compositor), else 60 Hz. Every xui app is paced.

| same harness, two runs | before | paced |
|---|---|---|
| window frames a second while scrolling | 101-108 | 60-62 |
| longest gap between two frames | 17-23 ms | 21-28 ms (none over 33) |
| `xuid` repaints / Mpx per 5 s | 512 / 922 | 301 / 542 |
| `xuid` composing + presenting, ms a second | 138 | 72 |
| UI process, `xuid` (`PERF:CPU`) | 24%, 14% | both under the 20% the line reports |

**Smaller changes on the way.**

* `xuid`'s blit (`user/src/messenger/display/canvas.rs`) swaps two pixels per 64-bit word, compiled for speed
  (`#[optimize(speed)]`; user programs build for size and cannot use SSE: the soft-float target refuses
  `#[target_feature]`). A first version chose the conversion through a function pointer that was not inlined, a
  call per two pixels: check the disassembly of such loops. The composing cost is now about 370 us per megapixel,
  the kernel's present copy about 300.
* **The probe image's picture check was in the paint stages.** `paint.cpp` keeps a shadow of the window's picture in
  probe images and moves it with every scroll (a whole-view `memmove`) and compares it with a whole paint every 30
  deltas; that was counted in `post` (2 ms of its 2.3). It is reported apart now (`INFO:PAINT ... ver=`, the
  `check` line of `frame_run.py`); a production image does none of it.
* **QEMU-only:** under WHPX each `inb` from the i8042 is a VM exit to QEMU, and `ps2::service` reads the status port
  every tick and at interrupt-window poll points: about 6% of the guest's CPU in these runs. A real PC pays about a
  microsecond a read. Left as it is; remember it when reading QEMU numbers.

**Left to do** (ranked by the profile):

1. **Paint into the slot** (done, section 12.1).
2. **WebKit's UI-side backing store** moves the whole view in place for every update that scrolled
   (`BackingStore::scroll`, patch `0024`), 25 times a second, although `paint.cpp` only reads the rectangles it
   paints. A ring of rows (scrolling by moving an origin) would make that free.
3. **Pace to the beam on the box.** `GetBeam` gives the frame counter and scan line; presenting right after vblank
   (G5 flips later) would also keep a copy out of the beam's way.

### 12.1 Buffer age: composing straight into the presented slot

A client window used to compose every frame into a private `frame` and then copy each slot's stale box (the
damage it missed plus the new damage) from it into the shared slot it presented. For the page view, which a
scroll repaints whole, that was two whole-view passes a frame. Now client mode composes into the slot itself
(`xui-app/src/client_window/slots.rs`, `Slots::prepare`; `render.rs`, `Dest::Slot`): the acquired slot first
copies the part of its stale box that the new damage does not cover from the other slot (current there: the one
composed last), then the damage is composed into it, the canvas path copying its painting surface's damage in
and a direct painter (`direct.rs`) writing the shared buffer itself. A slot that no other slot can complete (the
first frames, a resize) composes its whole stale box instead. Owner mode keeps `frame`.

Two contracts changed with it. A direct painter returns `false` when it cannot draw now, and the damage then goes
through the canvas: a slot's old pixels there may be frames old, so writing nothing is no longer an option. A
window closed while a direct painter writes its slot keeps its buffers mapped until the painter returns.

| `frame_run.py --perf`, 2560x1440, WHPX, one run each | before | buffer age |
|---|---|---|
| window thread, `INFO:WINDOW` draw | 678, 679 us | 631, 622, 646 us |
| UI process `PERF:CPU` (printed only above 20%) | 27% and 24% in one run, none in the other | none in three runs |
| UI process user-mode samples (`--sample-port`, 30 Hz) | 66 | 55 |
| of which `memcpy`/`memmove` | 47 | 27 |

The numbers are noisy (one CPU under WHPX, the host busy with other work); the samples say where the change acts:
the copies in the UI process roughly halved. The screenshots match the old build's: `shot_page` pixel for pixel
(but the clock), `shot_scrolled` once the 4 rows the timing-driven scroll differs by are aligned (but the pointer,
which does not scroll). Host tests: `client_window::slots::tests` (with a seeded model of frames that fail part
way) and `backend::render::tests` (a slot composite equals the frame path's picture, a declining direct painter
leaves the damage to the canvas).
