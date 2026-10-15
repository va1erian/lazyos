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
