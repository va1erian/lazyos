# WebKit on LazyOS (`webkit/`)

Stages W4 to W6 of [`docs/webkit-port-plan.md`](../docs/webkit-port-plan.md): the port
layer of a `PORT=LazyOS` build of WebCore and the WebKit (WebKit2) layer, and the desktop
browser on top of it (LazyWeb+). WebKit
itself is never vendored: `tools/webkit/port.py` takes the pinned WebKitGTK
2.54.1 tarball (the one the `jsc` build uses), patches it, adds the files below
and builds `WebProcess`, `NetworkProcess` and a headless UI process,
`webkit-smoke`, as static-PIE musl programs with zig, in the `jsc` build's Docker
container, against the W3 dependency bundle (`tools/webkit/README.md`).
`tools/webkit/webd.py` builds webd (below), the program that owns WebKit for the session.

```bash
python tools/webkit/deps.py          # the bundle, libwpe and the EGL stand-in first
python tools/webkit/port.py          # -> target/webkit/bin/{WebProcess,NetworkProcess,webkit-smoke}
python tools/webkit/port.py --configure   # CMake only
python tools/webkit/port_run.py      # console image with LAZYOS_WEBKIT_PORT=1, boot, judge (QEMU)
python tools/webkit/test_port_judge.py
python tools/run_demo.py --webkit-port   # an interactive console proof image (networking, big-app profile)
python tools/webkit/webd.py          # the service library (host) and webd-app (Docker)
python tools/xui/build.py            # the apps, LazyWeb+ (os.lazy.lazywebplus) among them
python tools/webplus/run.py          # desktop image, browse, judge (QEMU; tools/webplus/README.md)
python tools/run_demo.py --lazywebplus   # the desktop with the browser (networking, big-app profile, 1 GiB volume)
```

**Resources.** The build needs Docker with about 15 GiB of RAM (a WebCore translation unit can take
a gigabyte; `-j` is chosen by memory), about 10 minutes from a fresh container state (an
incremental rebuild after one port file takes seconds), and about 7 GiB of disk in the
`lazyos-jsc-work` volume (sources, the dependency bundle, build trees) plus three programs of
about 75 MiB each in `target/webkit/bin` (107 MiB before "Size of the programs"). An image with them needs a 1 GiB OS volume (the
build, `run_demo.py` and the launcher size it; `tools/lazygui/osvolume.py`), and the guest 4 GiB
of RAM (`port_run.py` passes it).

Two checkouts that build WebKit at once must not share the Docker work volume (each would
re-extract the tree and rebuild everything): `LAZYOS_JSC_VOLUME=<name>` gives a checkout its own
(copy `src`, `webkit-deps`, `prefix`, `wrap` and `zig-cache` from `lazyos-jsc-work` to keep the caches).

In the image the programs are `/system/bin/webprocess`, `/system/bin/networkprocess` and, with
`LAZYOS_WEBKIT_PORT=1`, `/system/bin/webkit-smoke` (program names there are lowercase, `libs/fhs`);
the launcher's default paths are those. webd (`LAZYOS_WEBD=1`, every desktop) ships the two processes;
`LAZYOS_LAZYWEBPLUS=1` adds the browser's core package, a client of webd (below).

`port.py` also runs `webkit-smoke` once on the build host (the programs are static
musl executables), in a scratch directory of the work volume, and keeps its output in
`target/webkit/smoke.host.txt`; `port_run.py` requires that file and the guest's painted bitmap
(checksum and ink) to equal it. A host run that fails or times out fails the build, and a boot
that painted nothing, or has no host run to compare with, fails the judge. `webkit-smoke` names no
profile directory, so its NetworkProcess keeps its cookie jar (`cookie.jar.db`) in the working directory
it inherits (the scratch directory there, the launcher's directory on LazyOS); webd gives its children a profile directory (below). `webkit-smoke` honours `WEBKIT_SMOKE_TIMEOUT` (seconds, default 90; `port_run.py` raises it under
`--accel none`). Both runs set `WEBKIT_SMOKE_FDS=1100` (`port/smoke/monitor.cpp`, issue #867): the program
holds that many descriptors first, so the UI process's IPC sockets are above 1024, and so is each engine
process's connection to the UI process (inherited at the same number; the WebProcess-NetworkProcess
connection is made in the engines and stays low), and tests the IPC socket monitor's loop on such a socket (`WEBKIT:W4:FDS:...`, `WEBKIT:W4:MONITOR:PASS:...`,
both required by the judge).

LazyOS has no file-backed `mmap` yet, and the port's Skia compiles W3's
`lazyos_mmap` shim (`tools/webkit/skiabuild/lazyos_mmap.c`, linked from
`WebCore/PlatformLazyOS.cmake`): without it Skia's font manager refuses every font and
all families draw in one face. Remove it with the kernel limitation.

## Size of the programs

LazyOS loads a static ELF whole into RAM and shares nothing between processes (docs/webkit-port-plan.md
section 6, "Memory for code"), and there is one WebProcess per view, so every byte of `WebProcess` is
paid per view. What was done, measured on the stripped programs (`WebProcess` / `NetworkProcess`):

| step | `WebProcess` | `NetworkProcess` |
|---|---|---|
| as built for W5 | 112,846,192 | 112,899,440 |
| ICU data cut (`tools/jsc/icudata.py`: 30.8 MB to 9.9 MB, see `tools/jsc/README.md`) | 91,977,072 | 92,030,320 |
| `-fno-asynchronous-unwind-tables -fno-unwind-tables` (`port_cmake.py`: `.eh_frame`, its index and `.gcc_except_table` 8.3 MB to 1.1 MB, the rest is the bundle's libraries) | 82,363,760 | |
| `-Os` for the cold targets (`OptionsLazyOS.cmake`, below) | (about 2.4 MB of that step) | |
| lld `--icf=safe` (`toolenv.icf_wrapper`) | 78,054,768 | 78,447,984 |
| `WebKitUIProcess` and `WebKitWebProcess` back at `-O2` (the scroll and paint path; the final state) | 78,534,000 | 78,517,616 |

`-Os` covers `WebCoreJSBindings` (generated, 9 MB of text), `WebCoreInspector` and the
`WebKitShared` and `WebKitNetworkProcess` object libraries (IPC and process management); `WebKitUIProcess` and
`WebKitWebProcess` stay at `-O2` because they hold the per-frame scroll and paint path (the first measurement had
them at `-Os`; the rows above were taken that way, see the note after the table); a sample of objects came out 19 to 25% smaller than at `-O2`. JavaScriptCore, WTF, WebCore's layout, style
and painting, and Skia stay at `-O2` (their share of text is another 45 MB: the next 10 MB if speed may be
traded). ICF is on for the WebKit programs only: zig's `cc` driver rejects `-Wl,--icf`, so the `zc++` wrapper runs the
link a second time through `zig ld.lld --icf=safe` (`LAZYOS_LINK_ICF=1`, set by `port_cmake.py`); `safe` folds only
functions whose address is never taken (the `-faddrsig` table), `all` would have saved 0.9 MB more.

Already in effect before (checked, not changed): `-ffunction-sections -fdata-sections`, `--gc-sections`, `strip
--strip-all` (no `.symtab`, no debug), no LTO (ThinLTO of 80 MB of text does not fit the 15 GiB container; not tried).
`-fno-pic` saves 1% of text and was not taken; the feature switches (video, WebGL, WebRTC, Web Audio, WebAssembly,
GPU process, inspector, MathML, XSLT) were already off, and the others WebCore still has (IndexedDB 0.56 MB,
service workers 0.62 MB, WebCrypto 0.26 MB, streams 0.9 MB) are runtime preferences in this release, not build options.

**Why `NetworkProcess` is as big as `WebProcess`** (they differ by 0.4 MB): both link every object of WebKit and
`--gc-sections` can only drop what nothing reaches, and from `NetworkProcess` main the references reach all of
JavaScriptCore (with the JIT) and WebCore's DOM, style and rendering through several unrelated, legitimate
paths: `InitializeWebKit2` -> `JSC::initialize` -> LLInt -> `operationDefaultCall` -> baseline and DFG compilers;
content rule lists -> `ContentExtension::compileGlobalDisplayNoneStyleSheet` -> CSS parser -> `CachedResourceLoader` ->
`Document`, `Page`, rendering; a regex-substitution redirect action -> `JSObjectCallAsFunction` -> `JSC::VM`; a CSP
violation report -> `Inspector::createScriptCallStack` -> `StackVisitor`; `upgradeInsecureRequestIfNeeded` ->
`Document::removedLastRef`. Cutting one leaves the others (measured on the link map: with the first four cut the
reachable code is still 90.0 of 90.2 MB), so a lean `NetworkProcess` (WebCore's loader and curl without the DOM) needs a split of WebCore's
libraries, not a link flag. The analysis scripts are not kept in the tree; `--Map` is not available through
`zig cc`, but `ZIG_VERBOSE_LINK=1` prints the `ld.lld` line and `zig ld.lld -Map=...` takes it.

## The glue

The release tarball is GTK's. It has every `*/playstation/*` *source* file of
the PlayStation port but none of its CMake (`OptionsPlayStation.cmake` and the
`PlatformPlayStation.cmake` of WTF, bmalloc, JavaScriptCore, PAL, WebCore and
WebKit), nor the libwpe directories those sources use, nor the whole of
`platform/network/curl` and the curl parts of `WebKit` (the tarball's network
stack is libsoup). The glue is therefore:

| where | what |
|---|---|
| `port/tree/Source/cmake/OptionsLazyOS.cmake` | `PORT=LazyOS`: derived from `OptionsPlayStation.cmake` at the commit the `webkitgtk-2.54.1` tag points to (`628d6ce1`); the header comment lists every difference |
| `port/tree/Source/{WTF/wtf,bmalloc,JavaScriptCore,WebCore/PAL/pal,WebCore,WebKit}/PlatformLazyOS.cmake` | the same for each project |
| `port/tree/Source/WebKit/UIProcess/Launcher/ProcessLauncherLazyOS.cpp` | the process launcher: `socketpair(SOCK_SEQPACKET)`, `fork`, `execv` of `WebProcess`/`NetworkProcess` with the client end inherited by that child alone (both ends are close-on-exec, the forked child clears the flag, issue #889) and `<program> <process id> <fd>` as the command line the generic unix `AuxiliaryProcessMain` parses; one reaper thread per child, asleep in `waitid(WEXITED | WNOWAIT)` until it ends and collecting it under the launcher's lock (issue #887; `INFO:REAPER` counts the wakeups). Replaces the PlayStation launcher (a closed library) and the GLib one (GSubprocess, `posix_spawn`) |
| `port/tree/Source/WebCore/platform/network/lazyos/CurlSSLHandleLazyOS.cpp` | the curl port's CA trust: the system PEM bundle (`/etc/ssl/certs/ca-certificates.crt`, the Mozilla roots of `docs/tls-plan.md`) instead of the console's CertificateStore |
| `port/upstream.txt` | 77 files of WebKit's git that the tarball lacks (libwpe glue, the curl network port), each fetched from `raw.githubusercontent.com` at the pinned commit and checked against its git blob digest (`tools/webkit/port_tree.py`) |
| `port/glstub/gen_stub.py` | writes `libEGL.a`/`libGLESv2.a` from the Khronos headers: every entry point does nothing (`tools/webkit/recipes_port.py`, built with the bundle) |
| `port/tree/Source/WebKit/WebProcess/InjectedBundle/lazyos/InjectedBundleLazyOS.cpp` | injected bundles are shared libraries; the programs are static executables: refused with a message |
| `port/tree/Source/WebKit/UIProcess/API/C/lazyos/WPELoaderLazyOS.cpp` | the `_wpe_loader_interface` that statically linked libwpe asks its backend for: no backend, so no objects (libwpe then uses its in-process pasteboard) |
| `port/app/*.cpp`, `engine.h`, `lazyos_ui.h` | the engine and the constants it shares with webd (below) |
| `port/tree/Source/WebKit/Platform/IPC/unix/SocketMonitorLazyOS.h` | the IPC connection's socket monitor loop (`poll()`, patch `0022`), header-only so `port/smoke/monitor.cpp` tests the same code |
| `port/smoke/main.cpp` | `webkit-smoke`, the headless UI process: C API only, one `WEBKIT:W4:*` line per step (`tools/webkit/portjudge.py` lists them), plus `INFO` lines for the fonts Fontconfig sees, the glyphs as text and timings |
| `port/smoke/spawn.cpp` | the spawn probe (issue #889): `webkit-smoke` is the launcher's embedder (`lzw_launcher_event`) and at each engine launch starts a copy of itself (`/proc/self/exe --fd-probe`) that must hold no socket (first shown to see a deliberately inherited one, `SPAWN:CONTROL:PASS`): `WEBKIT:W4:SPAWN:PASS:launches=<n>` |
| `port/fonts.conf.in` | Fontconfig's configuration in the image (`/system/etc/fonts/fonts.conf`), a template: `port_cmake.py` fills in the font and cache directories from `libs/fhs` and writes `target/webkit/fonts.conf`, which the image build embeds. The program paths and the CA bundle path reach the C++ as `-D` macros from the same source (`tools/webkit/fhspaths.py`) |
| `patches/` | changes to the tarball, applied in name order after `tools/jsc/patches/webkit` (below) |

libwpe 1.16.2 is built static without libxkbcommon and without a backend
(`tools/webkit/recipes_port.py`): the PlayStation WebKit layer is written
against its input-event, pasteboard and key-code types, and replacing them
would have meant rewriting the event conversion. Its view backend and renderer
(`USE_WPE_BACKEND_PLAYSTATION`, `USE_WPE_RENDERER`) are off; the UI API is
`WKViewCreate`, the PlayStation port's own non-backend path.

## Platform macro

`PLATFORM(PLAYSTATION)` selects allocator, assertion and file system code in WTF,
JavaScriptCore and bmalloc that is wrong on Linux. The port defines
`WTF_PLATFORM_LAZYOS` and patch `0003` renames every test of
`PLATFORM(PLAYSTATION)` in `Source/WebCore` and `Source/WebKit` (44 places in 27
files, `tools/webkit/gen_platform_patch.py` regenerates the patch), so WTF, JSC
and bmalloc stay exactly the proven `jsc-jit` configuration.

## Patches

See the header of each file; all are small. `0003`, `0004`, `0009` and `0010` are
LazyOS-gated (they only matter to `PORT=LazyOS`).

| patch | why |
|---|---|
| `tools/jsc/patches/webkit/0001`, `0002` | the JIT's missing include and its AVX check (W2) |
| `0003-platform-lazyos` | the platform macro, above; `PlatformDisplaySkia.cpp` is deliberately kept as it is (its header says why) |
| `0004-cmake-lazyos-port` | adds `LazyOS` to the ports `WebKitCommon.cmake` accepts |
| `0005-jsc-scriptexecutable-include-compilationresult` | a missing include that the unified build used to hide |
| `0006-webcore-gles3-enums-without-epoxy` | `GL_RGBA8`, `GL_R8`, ... come from libepoxy's headers; without it, GLES3 |
| `0007-webcore-openssl3-const-rsa` | `EVP_PKEY_get0_RSA` returns `const RSA*` in OpenSSL 3 |
| `0008-webcore-hardware-acceleration-setting-is-gtk-wpe` | `Page.cpp` asks for a setting that only GTK and WPE define |
| `0009-webkit-c-api-types-for-lazyos` | `WKBase.h` and `WKAPICast.h` pick the PlayStation C API types by `__SCE__`; the port uses `BUILDING_LAZYOS__` (defining `__SCE__` would turn bmalloc and libpas into their console builds) |
| `0010-cmake-symlink-forwarded-headers-on-lazyos` | forwarded headers are symlinks, as on macOS: the C API headers are reached both as `<WebKit/X.h>` and as `"X.h"` and a copy is parsed twice |
| `0011-webkit-layertreehost-playstation-skia-painting-engine` | `SkiaPaintingEngine::create` gained an argument that the (never entered) composited path of the PlayStation layer tree host did not pass |
| `0012-webkit-playstation-page-events-text-modifiers-time` | the C API a UI process sends input through (`WKPageHandleKeyboardEvent`, `...MouseEvent`, `...WheelEvent`) dropped the key's text (so nothing could be typed into a field), the modifiers, the event time (so every click at one spot counted as one more click of a series) and the held button of a drag, and sent a zero horizontal wheel step on every vertical one |
| `0013-webkit-playstation-editing-keyboard-events` | `WebPage::handleEditingKeyboardEvent` is `notImplemented()` (the GTK and WPE ports leave it to a toolkit input method): a page got keydown, keypress and keyup but no text in any field. The hook now runs the editing command of a key down (arrows, Home, End, Delete, Backspace, select all, copy, cut, paste, undo) and inserts the text of a key press |
| `0014-webkit-playstation-view-resize-reaches-the-drawing-area` | `PlayStationWebView::setViewSize` stored the size only: a window that was resized kept laying the page out and painting it at the old size; it now calls `DrawingAreaProxy::setSize` |
| `0015-webkit-playstation-data-directories-in-the-profile` | `WebsiteDataStore`'s PlayStation answers for every data directory the embedder did not set were `/app0`; they now follow `LAZYOS_WEBKIT_PROFILE`, the user's profile directory the UI process names (and keep `/app0` without it) |
| `0016-webkit-playstation-more-cursor-types` | the view client tells the window the I-beam, wait and resize pointers, not three |
| `0017-webkit-networkcache-file-times-without-birthtime` | `NetworkCache::fileTimes()` is `#if HAVE(STAT_BIRTHTIME)`, `USE(SOUP)`, `OS(WINDOWS)` and nothing else, so on this port it had **no body**: clang compiled the fall-off to a trap and the first HTTP-cache *hit* (a later boot) killed a NetworkProcess thread with a general protection fault (`CallableWrapper<Storage::updateFileModificationTime::$_0>::call()` was `push rbp; mov rbp,rsp; int3`). It now reports the modification time as both times (there is no st_birthtime in musl and no GLib xattr emulation) |
| `0018-webkit-c-api-reports-loads-blocked-by-content-rule-lists` | the C API's `contentRuleListNotification` forwarded only `notify` strings; a load a rule list *blocked* (which WebKit already tells the UI process) is now one more entry, `"lazyos:blocked"`, so LazyWeb+'s ad blocker counts blocks without a second rule per block |
| `0019-webkit-network-cache-data-curl-shares-a-mapped-file` | with content extensions on, the UI process needs `NetworkCache::Data::tryCreateSharedMemory()` to hand a compiled rule list to the web processes; the curl backend defines it for Windows only. On Unix-domain-socket platforms the content rule list store maps its files with `mapFileForSharing`, which keeps the file's read-only descriptor with the map, and `tryCreateSharedMemory` wraps both (receivers get a duplicate of the read-only descriptor). Every other map closes its descriptor in `adoptMap` as upstream does: an earlier version kept it in every mapped `Data`, one descriptor per mapped HTTP cache entry in the NetworkProcess (issue #892; `python tools/webkit/cachecheck.py` counts the NetworkProcess's descriptors below the cache during and after two visits of a cache-heavy page). `API::ContentRuleList`'s members are reordered so the compiled list, whose `SharedMemory` wraps the map without owning it, is destroyed before the map |
| `0020-webkit-c-api-says-whether-the-web-process-responds` | `WKPageIsWebProcessResponsive`: the page state client says that the answer to WebKit's 6 s ping changed, not which way; LazyWeb+'s "page not responding" bar needs the value (issue #862) |
| `0021-webkit-the-proxy-of-an-ended-web-process-is-kept` | `WebProcessProxy::processDidTerminateOrFailedToLaunch` keeps a reference to the dead proxy and counts them (`lzw_proxy_kept`, `PROCESS:KEPT`): destroying it when the page is loaded again in a new process aborts the UI process (`crashDueToCheckedPtrToDeadObject`, a thread-safe CheckedPtr count that is already zero), cause not found; not tied to the embedder's calls, to the page being unresponsive, or to timing (issue #862). Covers web processes only: a replaced NetworkProcess's proxy is not kept, and nothing shows that its destruction is safe or not |
| `0022-webkit-ipc-socket-monitor-polls` | the PlayStation path's `SocketMonitor` thread (one per IPC connection, on since `0003`) waited with `select()` on a stack `fd_set`: a descriptor of 1024 or more (a long session with a large HTTP cache: `0019` used to keep one open per mapped entry, issue #892) overflowed the set, and LazyOS's `EINVAL` for `nfds > 1024` made the loop spin without ever seeing the connection close. The loop is now `IPC::LazyOS::monitorSocket` (`port/tree/.../Platform/IPC/unix/SocketMonitorLazyOS.h`): `poll()`, a hangup or error is a close, a descriptor let go of ends the thread, one wait lasts at most 1 s (issue #867) |
| `0023-webcore-curl-stream-scheduler-polls` | the curl port's WebSocket thread (`CurlStreamScheduler::workerThread`) used `select()` with one reused `timeval`, which `select()` counts down to zero, so after the first timeout every wait returned at once, and it never waited at all while a stream had no socket: a NetworkProcess core spun for as long as a page kept a WebSocket. It now `poll()`s the sockets for up to 20 ms every pass. The curl files are fetched (`port/upstream.txt`), so `port_tree.py` fetches before it patches and never fetches a file an applied patch changed |

| `0024-webkit-update-observer-and-in-place-backing-store-scroll` | the classic drawing area told the UI process "the whole view needs display" for any update that scrolled, so every step of a scroll was copied, converted and handed on whole although the web process painted only the exposed rows. `WKPageSetUpdateCallback` (`WKPagePrivatePlayStation.h`) gives the client each update's scrolled rectangle, offset and repainted rectangles instead, and while it is set the view is not reported as needing display for those updates (`paint.cpp` and `xui-app/crates/webkit/src/damage.rs` use it). The Skia `BackingStore::scroll` allocated a bitmap the size of the scrolled area, read into it and wrote it back for every step; a raster surface's pixels are now moved in place (the old copy stays for a surface that cannot be peeked) |
| `0025-webkit-wkpagepaint-stage-counters`, `0026-webkit-wkpagepaint-whole-call-timer` | `WKPageGetPaintStats`: the microseconds `WKPagePaint` spends in making its surface, in `endPrinting`, in the drawing area's paint, and in the snapshot and the `drawImage` inside it, and (0026) the whole call, destructors included. `paint.cpp` prints them as `INFO:WKPAINT` once a second. A call costs about 64 us, which showed that the stage times around it (wall clock on one CPU) include other tasks' time |
| `0027-webkit-background-responsiveness-ping-keeps-the-proxy` | `BackgroundProcessResponsivenessTimer::responsivenessCheckTimerFired` pinged with a reference of its own; when the process was already gone (a second web process that exits cleanly, as one does after a page with a service worker such as The Verge) the ping ended it, the reference was the last, and the `WebProcessProxy` was destroyed inside its own timer callback, which aborts the UI process on this port (`crashDueToCheckedPtrToDeadObject`, the family of `0021`). `tools/webplus/live_run.py` with The Verge, CNN and NASA in a row did it every time (`WEBPLUS:CRASH:signal=6`, then `INIT:RESTART`). One reference is leaked per ping (the interval doubles from 20 s). The timer is kept on purpose: with no input to wait for, it is the only thing that notices a page that spins (`tools/webplus/hang_run.py` fails without it; an attempt to switch it off failed that harness) |
| `0028-webkit-scroll-offset-callback` | `WKPageSetScrollOffsetCallback`: `WebPageProxy::pageDidScroll` (the page's process says the main frame scrolled, just before the update that repaints for it) reported with its offset, so the window can show its picture ahead of the page (`docs/lazywebplus-performance-plan.md`, "Scrolling ahead of the page"). |
| `0029-webkit-backing-store-scroll-checks-in-64-bits` | `BackingStore::scroll` (patch 0024's in-place move) tests its rectangle with 64-bit sums: the rectangle and offset come from the web process. |
| `0030-webkit-overscan-bands-painted-ahead-of-a-scroll` | the classic drawing area paints rows above and below the view (`LAZYOS_WEBKIT_OVERSCAN`, view heights, 0 to 3; the UI process sets 1) in chunks of 512K pixels after the view's own update, as updates of their own (`UpdateInfo::documentRect`) that the backing store does not take; `WKPageSetBandCallback` hands them to the client (`paint.cpp`, `lzw_ui_band`), so the window has the rows a scroll uncovers before the page answers (`xui-app/crates/webkit/src/bands.rs`). A band is repainted at most every 500 ms while the page changes under it |
| `0031-webkit-scroll-repaints-painted-fixed-layers-and-a-display-rate` | `scrollContentsFastPath` repainted the whole rect of every fixed layer, and a page's full-view fixed containers that paint nothing (an overlay, a consent dialog's frame) made each scroll step a whole repaint: on LazyOS only the rects of the layers in their non-composited subtree that paint something are repainted. A display that carries no scroll waits for `LAZYOS_WEBKIT_DISPLAY_INTERVAL_MS` (33 ms) after the last one, so an animation runs at most at 30 Hz on one CPU; a scroll is never held back |
| `0032-webcore-cookie-jar-without-fsync` | the curl cookie jar (`CookieJarDB`) synced every write in rollback-journal mode: `document.cookie` is a synchronous message to the NetworkProcess, and on LazyOS an fsync flushes the volume, so a page that sets cookies often stopped for hundreds of milliseconds. `SyncOff` and `journal_mode=TRUNCATE` (the lock stays per transaction: a second window's NetworkProcess opens the same jar); a power cut can lose the last cookies set |
| `0033-webcore-curl-without-alt-svc-file` | curl rewrote its alt-svc cache through a temporary file and a rename after every request, and an ext2 rename flushes the volume twice under the VFS lock; alt-svc only names protocols this curl does not speak (HTTP/3) |

`OptionsLazyOS.cmake` also turns `ENABLE_CONTENT_EXTENSIONS` on (it is off in `WebKitFeatures.cmake` and only the GTK and
iOS ports enable it), which changes `cmakeconfig.h` and so rebuilds every translation unit of the three programs.

## Drawing

CPU only. `acceleratedCompositingEnabled` is false (the PlayStation `WebPreferences::platformInitializeStore`,
`UIProcess/playstation/WebPreferencesPlayStation.cpp`, sets it, `forceCompositingMode` and
`threadedScrollingEnabled` to false), so the PlayStation drawing
area (`DrawingAreaCoordinatedGraphics`, the classic path) paints dirty
rectangles into a `ShareableBitmap` on a CPU Skia surface (`GraphicsContextSkia`,
no GL) and sends them to the UI process, whose `BackingStoreSkia` keeps them;
`WKPagePaint` copies them out. The PlayStation sources also compile the
composited classes (`LayerTreeHost`, `ThreadedCompositor`,
`AcceleratedSurface`) for `USE_COORDINATED_GRAPHICS`; they link against the
EGL and GLESv2 stand-ins and are never reached.

## The engine: one Host, any number of Views

`port/app/`'s **engine** (`engine.cpp`, `clients.cpp`, `policy.cpp`, `uiclient.cpp`,
`notify.cpp`, `blocking.cpp`, `paint.cpp`, `light.cpp`, `profile.cpp`, `fullview.cpp`, `children.cpp`, `log.cpp`, all declared in `engine.h`) is WebKit's
UI process without a window: one `Host` (the `WKContext`, the notification manager, the engine processes)
and any number of `View`s, a page each with its own state (loading, dialogs, downloads, permission
requests, paint state, rule-list controller). Whatever a view says goes to its `ViewSink`, whatever the host
needs of its owner is a `HostEnv`; neither names a C ABI. (`lazyos_ui.h` is only the constants they share; the in-process host that once put a
C ABI over view 1 for LazyWeb+'s window is gone, LazyWeb+ is a client of webd.)

A second owner implements the same two interfaces. The first is `port/views/main.cpp`, `webkit-views`, which is
also the engine's test (docs/webd-plan.md, P1):

```bash
python tools/webkit/port.py --only WebProcess NetworkProcess webkit-views --run-views   # Docker; builds, runs in the container
```

It runs nine modes in the build container (the programs are static musl executables): `light` (one view with
a non-persistent store and no features), `twoeph`, `two` and `share` (two views, with stores of their own,
with a profile directory and without, and of one profile directory; the first, which has every client of a
browser view, is destroyed and the second loads another page), `group` and `groupprof` (two views of one group, without and with a profile directory: one web process, which
outlives the destruction of the first view; `group` also joins a third view, refuses a recover and rebuilds
the group), `groupmix`
(one group number, two profiles: two processes), `cycle` (twelve views made, shown and destroyed
in turn) and `rush` (twelve made and destroyed at once, before their pages are up); every destroyed view's web
process must end. Each view loads a `data:` page of its own colour and title and must paint that colour and
never the other view's, and the page's updates must reach it (they find their view by the page). Findings it
made:

* **A view gets a WebProcess of its own unless it joins a group**, even two views of one profile directory
  (`share`: two web processes, one network process): 107 MiB a view. Views with the same non-zero
  `ViewConfig::group` and profile share the first one's data store and, as related pages
  (`WKPageConfigurationSetRelatedPage`), its process (`group`, `groupprof`: one web process for two views).
  A destroyed view's process ends with it (`destroyView` terminates it unless a live view of the group uses it;
  then the page is unloaded to `about:blank` and parked). The sharing is made when the page is: a process
  other views use is not recovered in place (`recover` is refused, `WEBPROCESS:RECOVER:SHARED`: ending it and
  reloading its pages aborted the UI process), the owner destroys the group's views and makes new ones (`group`
  does: the last destroyed view ends the process, a new view starts one). A group is one owner's (related
  pages see each other's storage).
* **A view with nothing to fetch still launches a NetworkProcess** (`VIEWS:LAUNCH:web=1:network=1` for the
  light view's `data:` page): the answer to L1 of the plan is no, a light view cannot avoid it with this
  WebKit. Two views of different data stores share the one NetworkProcess and have a WebProcess each.
* **Three page callbacks are process-wide** (`WKPageSetUpdateCallback`, `...ScrollOffsetCallback`,
  `...BandCallback`, patches `0024`, `0028`, `0030`: static callback and context, whichever page registered
  last). With two views the second registration took the first's updates, which never painted again. The engine
  registers them once and finds the view by the page argument (`paint.cpp`, `viewOfPage`).
* **Destroying a `WebPageProxy` aborts the UI process** (`crashDueToCheckedPtrToDeadObject` in
  `WebPageProxy::didReachLayoutMilestone`, a timer of the run loop: one run in three with a persistent
  profile, the family of patches `0021` and `0027`). `destroyView` therefore ends the page's process
  (`WKPageTerminate`, 107 MB back), clears its clients, frees its bitmaps and pacer thread and parks the hollow
  proxy, never released: a proxy per view that ever was, and the data store and session of one that had a store
  of its own. `WKPageClose` before the release made it worse.
* **A persistent profile lives as long as the process**, for the same reason, and the children take
  `LAZYOS_WEBKIT_PROFILE` and `CURL_COOKIE_JAR_PATH` from the first persistent profile only (a process takes
  its environment when it starts, and one NetworkProcess serves every profile). That suits an owner of one
  profile (the in-process LazyWeb+ was one). An owner of several (webd, whose client LazyWeb+ now is) starts the host with `startHostWithChildren` (`profile.cpp`): the
  children's folder and an in-memory default cookie database are decided then and never changed, and each
  persistent store's NetworkProcess session opens its own `cookieStorageFile` (`NetworkSessionCurl.cpp`,
  `setCookieDatabase`), so every profile's cookies are in its own jar (`webd-abi-test profiles` proves it).

## webd's engine half (`port/webd`): light and full views behind a C ABI

`port/webd/webd_abi.h` is the contract between the engine and webd's service (Rust, `xui-app/crates/webd`,
docs/webd-plan.md, P2 slice 1): `webd_engine_*` calls the service makes from any thread, `webd_ui_*` callbacks
the engine makes on WebKit's main thread. `abi.cpp` implements the first over the engine's `Host` and `View`s
and the second as a `ViewSink` per view and one `HostEnv`: the service chooses every view id (a `Slot` maps it to
the engine's), every call copies its arguments and queues a task for the run loop (so nothing waits for WebKit),
a call about an id the engine does not know does nothing, and a call that arrives before the view exists waits for
it, in order (the first view waits for the network lock, below). A frame is always a whole one; a view that is
not visible is painted no more (its page runs), and `set_visible(1)` or `refresh` paints it. A web process that
ends (`ViewState::crashed`) is `webd_ui_view_lost` (`CRASHED`, or `MEMORY`) and the id is dropped; the engine's
view is destroyed behind it. Mouse `kind` is the header's (0 move, 1 press, 2 release); the engine's own
`View::mouse` takes 0 down, 1 up, 2 move, and `abi.cpp` translates.

A **light view** (`ViewConfig::light`, `light.cpp`) is the engine with a lock on it, all of it enforced in the UI
process and none of it left to the page:

* *No network.* A content rule list (WebKit's content-extension store, compiled once at start into
  `$LAZYOS_WEBD_RULES_DIR`, required, no fallback: the one thing the engine writes for a light view) blocks
  every load but `data:`, `blob:` and `about:` URLs, and a navigation policy loads only the owner's own
  `loadHtml`. The list is what stops subresources: the navigation client sees navigations, not requests.
  Measured with a listener on 127.0.0.1 (`network` below): a page that asks for an image, a style sheet, an
  `@import`, a font, a script, a frame, a video poster, audio, an `<object>`, an `<embed>`, an image input, an SVG
  `<image>`, a `<picture>`, a favicon, `preconnect`, `prefetch`, `dns-prefetch` and a ping link, and (scripts on)
  `fetch`, `XMLHttpRequest`, `WebSocket`, `new Image`, `sendBeacon`, `EventSource`, `import()`, a script and a
  link added by script and a `Worker`, makes **no connection**. The same page in a light view *without* the rule
  list made 26 (the first thirteen distinct, in order: `/img.png /a.css /import.css /s.js /svg.png /poster.png /pic.png
  /input.png /fetch /xhr /jsimg /ws /mod.js`), so the lock is the rule list, not the scripts being off. Not
  observable by that test: a DNS lookup for a *name* (`WKPreferencesSetDNSPrefetchingEnabled` is a no-op in this
  WebKit; the rule list covers the `dns-prefetch` link, but a lookup would not reach a listener either way). A
  light view still launches the NetworkProcess (the plan's L1), idle.
* *Navigation locked.* A main-frame navigation the owner did not ask for is refused and told as
  `ViewSink::linkClicked` (`webd_ui_link_clicked`): a link click, a form submit (GET: with its query), a meta
  refresh, a script's `location`, a `target=_blank` link. A `#fragment` link is the page's own and scrolls.
  Any navigation the page starts, even a refused one, cancels the one it had scheduled (WebKit's
  `NavigationScheduler`): a click before a meta refresh fires loses the refresh. A *relative* URL cannot be made
  absolute against `about:blank` or a `data:` base (WebKit's URL stays `r3`): it is told as it is. A refresh to
  the document's own URL is a reload and is refused without a word. Frames of the page load only `data:`,
  `blob:` and `about:` URLs.
* *Scripts* as `scripts` says (`WKPreferencesSetJavaScriptEnabled`; event handlers too), and with them on: no
  local storage, no storage of any kind, no automatic windows, no beacons. `<noscript>` content is not
  rendered even with scripts off (the parser's scripting flag is not the preference: `script` mode prints the
  colour the page ended with).
* *None of a browser's clients*: `createView` forces `features` to 0 for a light view (no UI client, dialogs,
  downloads, notifications, blocking or policy callbacks to the owner), and a light view groups only with
  light views.
* `loadHtml(html, base_url)`: `base_url` must be `about:blank`, empty (the same) or a `data:` URL; anything else
  (`http:`, `https:`, `file:`, `ftp:`, `blob:`) loads nothing and the next state carries an error. The load is
  `WKPageLoadData`, UTF-8, `text/html`.
* Best-effort fields: `default_font_size` sets WebKit's default *and minimum* font size; `user_style` is
  appended to the document as a `<style>` after the caller's markup (the C API has no user style sheet and a
  light view has no other kind of load; text that could close the element is refused and said
  `CONFIG:IGNORED:user_style`); `color_scheme = 1` is **not implemented**: `prefers-color-scheme` is compiled out
  of this WebKit (`ENABLE_DARK_MODE_CSS` is off in the port, and `WKPageSetUseDarkAppearanceForTesting` changes
  nothing without it), so the engine says `CONFIG:IGNORED:color_scheme` and an owner that wants a dark page
  passes a `user_style`. Copy: `Ctrl+C` reads the selection with script (`getSelection()`; the C API has no
  selected-text call), so `webd_ui_clipboard_write` happens only when scripts are on.

`webd-abi-test` (`port/webd/test`) plays the service: the `webd_ui_*` callbacks record what is told per view id
and a driver thread calls `webd_engine_*` as the Rust thread will, one mode per process. Every mode ends with
`WEBD:<mode>:PASS` or `WEBD:<mode>:FAIL:<why>`, and an engine process left behind is a failure:

```bash
python tools/webkit/port.py --only WebProcess NetworkProcess webd-abi-test --run-webd            # Docker; every mode
python tools/webkit/port.py --only WebProcess NetworkProcess webd-abi-test --run-webd lock lost  # some of them
```

| mode | what it asserts |
|---|---|
| `html` | a light view paints a `LoadHtml` page (colour, title, size, a scroll offset, `finished` rises), a second load replaces it, resize gives frames of the new size, a hidden view gets no frame but its page runs and it is painted when shown, `refresh` sends a frame, nothing is told after `view_destroy` and calls about the dead id do nothing |
| `script` | `scripts=0`: an inline script and an event handler do not run; `scripts=1`: the script does; two views at once |
| `config` | `default_font_size` and `user_style` apply; `color_scheme` is reported ignored |
| `lock` | a click on a link, a form submit, a meta refresh and (scripts on) a script's `location` each become `link_clicked` with the absolute URL and the page stays (title, colour, `finished`, no error); a `#fragment` link is not reported |
| `network` | a listener on 127.0.0.1 is reached by a plain engine view (the control) and by no road of a light view, scripts on or off; five bad base URLs are refused with an error and load nothing, three good ones load |
| `many` | twelve views of one group (scripts alternating on and off, so the process holds both) share ONE web process and each paints its own colour and title; destroyed in another order than made, the process lives until the last view and ends with it |
| `lost` | `SIGKILL` of the web process (pid from `webd_engine_children`) is `view_lost(CRASHED)`, the id is dead, a new view works; the same for both views of a group, which is then rebuilt with one new process |
| `cycle` | twenty views made, painted and destroyed in turn: every web process ends, nothing is told after `view_destroy` |

### Full views (P2 slice 3)

A **full view** (`light = 0`) is a browser page: `abi_full.cpp` reads its configuration and its own calls
(`load`, `reload`, `stop`, `back`, `forward`, `download`, `download_cancel`, `dialog_reply`,
`navigation_answer`, `notify_*`, `blocking_*`; each does nothing to a light view), `sink_full.cpp` is its `ViewSink`: the single-page host's sink (deleted with it, P4) with the view id on every call
(`webd_ui_page_state` instead of `webd_ui_state`). `slots.h` is what the three files share.

* *The host.* Full views need `webd_engine_start_host` with a `children_directory`
  (`startHostWithChildren`): without it a full view is refused (`view_lost(REFUSED)`,
  `FULL:REFUSED:...`), since the first profile would otherwise decide every other's cookie jar. The host also
  prepares the light lock, so both kinds share it. `webd_engine_start` and `_start_host` refuse a second start.
* *Configuration.* `features` are masked to the six view features and `WEBD_FEATURE_POLICY` is always added;
  `profile` must be absolute with no `.`, `..` or empty component (else refused), and is created (a profile that
  cannot be made is refused, not silently kept in memory); `memory_only`, `deltas`, `group` as the header says;
  `scripts = 0` turns script off (preferences are touched only when something is asked: scripts off, a font
  size, a suffix); `user_style` is a user script that appends a `<style>` (scripts on only, else
  `CONFIG:IGNORED:user_style`); `color_scheme = 1` is `CONFIG:IGNORED` as for a light view; `store_kind` (`WEBD_STORE_OWN` 0, `WEBD_STORE_PRIVATE` 1,
  appended to the struct) is part of the group-mate match beside the group, the profile directory, `memory_only`,
  `light` and `no_network`: the client's own in-memory profile and its private windows differ in nothing else, and
  without it the second was a related page of the first and shared its cookies (`webd-abi-test store`).
* *The client's own page.* `load_html` on a full view is `loadFullHtml` (`fullview.cpp`): the bytes as
  `WKPageLoadData`, UTF-8 `text/html`, under a base whose scheme (as WebKit parses it) is `about`, `data`, `http` or
  `https`; anything else (`file:`) loads nothing and the next page state carries an error (`FULL:BASE:REFUSED`).
  Its navigation is asked about (`webd_ui_navigate`, the base URL, kind `other`) like any other, and relative
  addresses in the page resolve against the base.
* *No network* (`no_network = 1`, a client without the network scope): the light views' lock list
  (`lightLockFilter`) goes on the page's own controller before the page exists, so the view waits for the lock
  like a light view and is not made without it. The navigation policy is still the service's
  (`webd_ui_navigate`), but the list also blocks the main frame's document loads: an allowed link or `load` to an
  `http:` address fails with "The URL was blocked by a content blocker" (code 104) and nothing reaches the server
  (`nonet`). With `WEBD_FEATURE_BLOCKING` it is refused (`view_lost(REFUSED)`): `blocking.cpp` rebuilds the
  controller's lists at navigations and would drop the lock. Such a view groups only with views without network
  (related pages script each other).
* *User agent suffix.* At most 64 printable ASCII bytes (else `CONFIG:IGNORED:user_agent_suffix`). With
  site-specific quirks on, the page's process sends PlayStation's fixed agent for every address
  (`WebPagePlayStation.cpp`, `platformUserAgent`), so `WKPageSetApplicationNameForUserAgent` never reached a
  request; and it would append `/<WebKit version>` (`UserAgentPlayStation.cpp`). A view with a suffix has the
  quirks off and WebKit's agent, a space and the suffix as its custom agent.
* *User agent* (`webd_view_config.user_agent`, 2026-10-23). At most 256 printable ASCII bytes (else
  `CONFIG:IGNORED:user_agent`); the service never sets it together with a suffix. The view has the quirks off for
  the same reason and `WKPageSetCustomUserAgent` is the text itself. `webd_engine_set_user_agent` changes it from the
  next load on (`""` restores the one the view was made with) and is ignored, with `CONFIG:IGNORED:set_user_agent`,
  for a view made with neither an agent nor a suffix. `webd_engine_recover_group` ends a web process other views
  share (`WEBPROCESS:RECOVER:GROUP`), marking every view in it as stopped on request. Written without a build
  volume to run it: `webd-abi-test` has no case for either call yet, and `View::recover` records that ending a
  shared process aborted the UI process when its pages were then reloaded; whether termination alone is safe is
  unproven (run the `group` case of `webkit-views` first). WebKit's own agent is read once when the page is made
  (`View::standardAgent`): `WKPageCopyUserAgent` answers the custom one afterwards.
* *Held navigations* (`WEBD_VERDICT_ASK`): `ViewSink::navigateRequest` gets a per-view request id (from 1,
  never reused) and `main_frame` (the main frame's navigation, or a new window's: a `target=_blank` link is
  asked with `main_frame = 1` before `webd_ui_new_window`; `window.open` is not asked at all, WebKit goes
  straight to the new window). The listener is kept, retained, until `navigation_answer` (allow: the rule lists
  and Use, or Download for `<a download>`, as the normal path; deny: Ignore). Any newer main-frame navigation of
  the view (a click, a `load`, Back) refuses every held one before it is asked about, and so does the view's
  end; a late answer is ignored. Stop does not drop them.
* *Painting.* `deltas = 0`: whole frames, as a light view. `deltas = 1`: `paintResize/Full/Rect/Update/Plan`
  go to `webd_paint_*`, `frameDelta` to `webd_ui_frame_delta`, bands to `webd_ui_band`, and `webd_ui_scroll`
  comes before every post (the engine tells the offset before each paint; the sink does not deduplicate it for
  such a view). The engine paints a whole frame on the first paint and after every size change whatever the
  plan says; a plan with a rectangle or a scroll area outside the view is painted whole
  (`PAINT:PLAN:REFUSED`). A hidden view is not asked for a plan.
* *Downloads.* Ids per view from 1. The path `webd_ui_download_start` writes must be absolute and
  NUL-terminated within the buffer, or the download is refused. Two engine bugs this found, fixed for LazyWeb+
  too: WebKit's curl download reports 0 for both totals (`NetworkDataTaskCurl.cpp`), so progress was always 0
  (the engine counts the bytes and takes the total from the response); and a cancel ends only through
  `WKDownloadCancel`'s completion, never `didFailWithError` (`DownloadProxy::cancel`), so the owner never heard
  of it: the engine now reports `download_fail("Cancelled")` from the completion and frees its bookkeeping.
* *Notifications.* `webd_engine_notify_permission(view, origin, answer)` settles that view's pending requests
  only (`notifyPermissionIn`); WebKit's record of the decision is the context's (one for every profile).
  Nothing is remembered context-wide at start (`HostEnv::notifyPermissionAt` is -1). **WebKit refuses the
  permission in a page whose session is ephemeral** (`WebNotificationClient::requestPermission`), before
  anything reaches the UI process: a view without a persistent profile (none, or `memory_only`) can never notify.
  `webd_engine_notify_forget(view, origin)` (P4) is `notifyForget`: the context's record only, host-wide.
* *Recover.* `webd_engine_recover(view, reload)` (P4) is `View::recover`: the view's web process is ended and the
  page reloads in a fresh one, or stays stopped (`LOST_OTHER`); a process other views share is left alone.

```bash
LAZYOS_JSC_VOLUME=<yours> python tools/webkit/port.py --only WebProcess NetworkProcess webd-abi-test --run-webd full profiles ask dialog download popup deltas blocking notify nonet store
```

| mode | what it asserts |
|---|---|
| `full` | the first page state answers `view_create`; a load from the test's HTTP server: url, committed url, title, `finished`, `progress`, no back/forward, the load asked as a main-frame `other` without a gesture, the user agent ends with the suffix; a link click (asked as a link with its gesture), Back, Forward, Reload with `can_back`/`can_forward` and `finished`; a denied navigation requests nothing; a failed load says why and leaves the page; no delta for `deltas = 0`; `load_html` under the server's `http` base (asked about; its relative picture requested from the server; title and state arrive), under `about:blank`, and refused under `file:` (an error, nothing loads) |
| `profiles` | profile A's page sets a cookie by `Set-Cookie` and by `document.cookie`; a second view of A (another process) sends both; profile B and a memory-only view send none, and A does not send the memory-only view's; the children's environment is still the start's; the cookie is in a file below A's `cookies/`, in nothing below B, the children's folder or the working directory, and the memory-only view's is on no disk |
| `ask` | a click to another origin held: nothing requested until `navigation_answer(allow)` loads it; a second held one denied stays; a third dropped by a newer `load`, its late answer does nothing; a view destroyed with one held ends cleanly |
| `dialog` | alert, confirm, prompt answered in turn (`done:true:typed`), Cancel is `false:null`, a stale reply is nothing; 25 alerts of a loop answered in order; an endless loop answered ten times, then its view destroyed with a dialog open, and a new view works |
| `download` | a link to an attachment: name, path under the test's folder, progress, `received`/`total` equal to the file, done, the bytes; the page stays; a slow Save As cancelled is `download_fail("Cancelled")` |
| `popup` | a script's `window.open` is `new_window(gesture 0)`, a click's `new_window(gesture 1)`, a `target=_blank` link is asked first and is `new_window(gesture 1)`; nothing loads |
| `deltas` | a tall page and two wheel turns: a delta that moved the picture arrives, a scroll call before every post, and the picture rebuilt from the frame and the deltas equals the whole frame `refresh` sends, pixel for pixel |
| `blocking` | a rule list compiled into the profile blocks `blocked.png` (the server never sees it, `blocked == 1`); a list never stored is `NOT_STORED`; a page the service excepts requests it; a forgotten list blocks nothing |
| `nonet` | a control full view's page (`@import`, `<img>`, fetch, XHR, a script's picture) reaches the server; the same page in a `no_network` view (scripts on, `about:blank` base) makes no request; a denied link is asked and requests nothing; an allowed link and an allowed `load` are blocked by the lock (error, no request); an inert page still loads; `no_network` with blocking is refused |
| `notify` | two views of one origin ask; one allowed, the other still pending, then dismissed (`denied`); the allowed page's notification is shown with its origin, title and body, and hears shown and clicked |
| `store` | the same client's own in-memory profile (`store_kind` 0) and private window (1), both in one non-zero group with no folder and `memory_only`: a cookie set in the own profile is sent by a second own view (the control) and not by the private window; a private window's cookie is sent by a second private window and not by the own profile |

### The program: `webd-app`

`port/webd/main.cpp` links the engine, `abi.cpp` and the service (`xui-app/webd`, a Rust static library,
`libxui_webd_host.a`) into `webd-app`; `python tools/webkit/webd.py` builds the library on the host and runs
`port.py --only webd-app` in the container (`LAZYOS_WEBD_LIBRARY` makes the CMake target exist), and copies
the result to `target/xui/xui-webd.elf`, the package's program (`xui-app/packages/webd`, docs/webd.md). **The process's first thread runs the service and WebKit's run loop runs on a thread
`main.cpp` makes**: `init` and the shell know an app by its first thread. `host.h` is the C ABI of the Rust half
(`webd_host_init`, `webd_host_main`, `webd_host_signal_quit`, `webd_host_fail`, `webd_host_report`);
`SIGTERM` rings the service's doorbell, the service leaves its loop, `webd_engine_quit` stops the run loop, the
engine processes are ended and waited for, and the program `_exit`s (status 4: the watchdog found WebKit's
main thread hung, and nothing is waited for).

## The desktop app

`os.lazy.lazywebplus` ([`docs/lazywebplus.md`](../docs/lazywebplus.md)) is no longer a WebKit program. It
is a plain xui app (`xui-app/webplus`, `xui-lazywebplus.elf`, built by `python tools/xui/build.py` with the
other apps) that is a client of webd: it asks webd for full views (`xui-app/crates/webd-client`'s `FullWebView`)
and draws the frames webd paints, over the seam `xui-app/crates/webkit`; the engine, its processes and its
sandbox are webd's (`os.lazy.webd`, [`docs/webd.md`](../docs/webd.md)). What used to be the C ABI between the
UI process and the window (`lazyos_ui.h`, one page, `lazywebplus-app`) is gone, and webd's views took over its semantics; `port/app/lazyos_ui.h` keeps only
the constants the engine and webd share, and webd's C ABI over the engine's views is `port/webd/webd_abi.h`.
`LAZYOS_LAZYWEBPLUS=1` ships the core package and needs webd (`LAZYOS_WEBD`, on in every desktop) and the
network stack.

Keys reach the page as X11 key symbols (libwpe's key codes); printable text goes with the key press
(`WKKeyboardEvent.text`, patch `0012`) and patch `0013`'s editing hook inserts it and runs the editing
commands (arrows, Delete, select all, copy, paste, undo).

**One WebProcess for every page.** With WebKit's defaults a cross-site navigation swaps in a fresh WebProcess,
and the second 107 MB program's `exec` failed with `ENOMEM`: the kernel charged the *length* of every `mmap`
to the user's memory quota (`docs/architecture/limits.md`; reservations are charged by resident page now, the
launchers still set 8G). `engine.cpp` turns process swapping, pre-warming and the process cache off.

**The profile directory.** WebKit's data directories default to the PlayStation's `/app0`, which LazyOS
has not. The owner names a profile directory per view (`webd_view_config`); `engine.cpp` gives the page a
website data store whose cache, cookie jar (`cookies/cookie.jar.db`) and storage directories are below it, and
names it in `LAZYOS_WEBKIT_PROFILE` and `CURL_COOKIE_JAR_PATH` (curl opens its cookie database before the data
store's file is applied, so the variable is what keeps `cookie.jar.db` out of the working directory).
`ProcessLauncherLazyOS.cpp` starts the children in it, and patch `0015` makes the directories the C API
cannot set (HSTS, alternative services, ...) follow it instead of `/app0`. With no profile (or a view that
asks for none) the store is ephemeral.

**The sandbox.** The WebProcess and the NetworkProcess run under confined labels
(`app:os.lazy.webd/webprocess`, `.../networkprocess`; the `[processes.*]` tables of
`xui-app/packages/webd/manifest.toml`, the kernel's gates in `docs/architecture/process-sandbox.md`):
the page process reads only fonts and has no network, files beyond them, Messenger interface or way to start
a program; the network process has sockets, the resolver, the CA bundle and the profile and Downloads folders.
The launcher is unchanged (`fork`, `execv`); the label changes at the `execve`. LazyWeb+'s own manifest keeps
one helper table, `[processes.listupdate]` (the ad-block list fetch).

**Quitting.** webd ends the WebProcess and the NetworkProcess itself (SIGTERM, SIGKILL after a grace period),
waits for them and reports `CHILD:<name>:<pid>:GONE` (`port/app/children.cpp`).

**The attack harness.** `python tools/webkit/attack_run.py` (`tools/abi/fixtures/src/sandboxprobe.rs`,
judge `tools/webkit/attackjudge.py`, self-test `test_attack_judge.py`) attacks the sandbox from inside the
two engine processes: a stand-in program (`python tools/webkit/attack_build.py`, built with Rust's musl
target, no WebKit build needed) tries to read the user's files and the system's, write them, open a socket,
connect to a named socket, resolve the display, execute BusyBox and a native program, `fork`, signal its
parent and `ptrace`, while the few things the process needs must still work. Every attack must be refused
with the policy's `EPERM` and a `LABEL:DENY` line, as in `tools/accounts/run.py`. `--kernel` (the default)
boots a console image whose first program loads webd's two process tables as labels the way `pkgd`
does and starts the probe under each; `--browser` builds a desktop image with `LAZYOS_WEBKIT_ATTACK=1`, which
ships the stand-in as `/system/bin/webprocess` and `/system/bin/networkprocess` beside the real webd and
LazyWeb+, so the real webd starts them and the label comes from webd's package tables. `LAZYOS_WEBKIT_ATTACK=1`
is a test switch: never set it for an image anyone uses. Like `LAZYOS_TLS_TEST_CA`
it is harness-only on purpose: `attack_run.py` sets it, and there is no
`run_demo.py` flag or launcher GUI control for it.

## LazyWeb+ (`os.lazy.lazywebplus`)

[`docs/lazywebplus.md`](../docs/lazywebplus.md): LazyWeb's chrome on this engine, through webd. The calls the
chrome needs from an engine that has scripts (navigation policy, downloads, JavaScript dialogs, new windows,
the Notifications API, the engine processes' health, pointer moves merged while the main thread is busy, the
ad blocker's rule lists, cookies and site data, a page's data in memory only) are the engine's `View` methods
and `ViewSink` callbacks (`port/app/engine.h`), which webd exposes per view in `port/webd/webd_abi.h`
(`webd_engine_*` down, `webd_ui_*` up) and the Rust side mirrors in `xui-app/crates/webd`; the numbers they
share are `port/app/lazyos_ui.h`'s. The files behind them: `children.cpp` (the launcher's list, and the
shutdown that kills each process group with retries; `ProcessLauncherLazyOS.cpp` reports through
`lzw_launcher_event`), `notify.cpp` (`WKNotificationProvider`, needs `ENABLE_NOTIFICATIONS`, on in
`tools/webkit/port_cmake.py`), `blocking.cpp` (`WKUserContentExtensionStore` below `<profile>/contentextensions`,
the navigation client's `contentRuleListNotification`, patch `0018`; the chrome half is
`xui-app/webplus/src/blocker/`), `policy.cpp`, `uiclient.cpp`, `paint.cpp`, `profile.cpp`.

`python tools/webplus/run.py` is the harness (`tools/webplus/README.md`).
