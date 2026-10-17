# WebKit on LazyOS (`webkit/`)

Stages W4 and W5 of [`docs/webkit-port-plan.md`](../docs/webkit-port-plan.md): the port
layer of a `PORT=LazyOS` build of WebCore and the WebKit (WebKit2) layer, and the desktop
browser on top of it. WebKit
itself is never vendored: `tools/webkit/port.py` takes the pinned WebKitGTK
2.54.1 tarball (the one the `jsc` build uses), patches it, adds the files below
and builds `WebProcess`, `NetworkProcess` and a headless UI process,
`webkit-smoke`, as static-PIE musl programs with zig, in the `jsc` build's Docker
container, against the W3 dependency bundle (`tools/webkit/README.md`).
`tools/webkit/app.py` builds the desktop UI process, `webkit-app` (W5, below).

```bash
python tools/webkit/deps.py          # the bundle, libwpe and the EGL stand-in first
python tools/webkit/port.py          # -> target/webkit/bin/{WebProcess,NetworkProcess,webkit-smoke}
python tools/webkit/port.py --configure   # CMake only
python tools/webkit/port_run.py      # console image with LAZYOS_WEBKIT_PORT=1, boot, judge (QEMU)
python tools/webkit/test_port_judge.py
python tools/run_demo.py --webkit-port   # an interactive console proof image (networking, big-app profile)
python tools/webkit/app.py           # W5: the window library (host) and webkit-app (Docker)
python tools/xui/build.py            # ... which packages it as os.lazy.webkit
python tools/webkit/app_run.py       # W5: desktop image, browse, judge (QEMU, shots/webkit)
python tools/webkit/test_app_judge.py
python tools/run_demo.py --webkit    # the desktop with the browser (networking, big-app profile, 1 GiB volume)
```

**Resources.** The build needs Docker with about 15 GiB of RAM (a WebCore translation unit can take
a gigabyte; `-j` is chosen by memory), about 10 minutes from a fresh container state (an
incremental rebuild after one port file takes seconds), and about 7 GiB of disk in the
`lazyos-jsc-work` volume (sources, the dependency bundle, build trees) plus three programs of
about 107 MiB each in `target/webkit/bin`. An image with them needs a 1 GiB OS volume (the
build, `run_demo.py` and the launcher size it; `tools/lazygui/osvolume.py`), and the guest 4 GiB
of RAM (`port_run.py` passes it).

Two checkouts that build WebKit at once must not share the Docker work volume (each would
re-extract the tree and rebuild everything): `LAZYOS_JSC_VOLUME=<name>` gives a checkout its own
(copy `src`, `webkit-deps`, `prefix`, `wrap` and `zig-cache` from `lazyos-jsc-work` to keep the caches).

In the image the programs are `/system/bin/webprocess`, `/system/bin/networkprocess` and, with
`LAZYOS_WEBKIT_PORT=1`, `/system/bin/webkit-smoke` (program names there are lowercase, `libs/fhs`);
the launcher's default paths are those. `LAZYOS_WEBKIT=1` is the browser: the two processes
beside the app's core package (below).

`port.py` also runs `webkit-smoke` once on the build host (the programs are static
musl executables), in a scratch directory of the work volume, and keeps its output in
`target/webkit/smoke.host.txt`; `port_run.py` requires that file and the guest's painted bitmap
(checksum and ink) to equal it. A host run that fails or times out fails the build, and a boot
that painted nothing, or has no host run to compare with, fails the judge. `webkit-smoke` names no
profile directory, so its NetworkProcess keeps its cookie jar (`cookie.jar.db`) in the working directory
it inherits (the scratch directory there, the launcher's directory on LazyOS); the browser (`webkit-app`)
gives its children a profile directory under `$HOME/.apps` (below). `webkit-smoke` honours `WEBKIT_SMOKE_TIMEOUT` (seconds, default 90; `port_run.py` raises it under
`--accel none`). Both runs set `WEBKIT_SMOKE_FDS=1100` (`port/smoke/monitor.cpp`, issue #867): the program
holds that many descriptors first, so the UI process's IPC sockets are above 1024, and so is each engine
process's connection to the UI process (inherited at the same number; the WebProcess-NetworkProcess
connection is made in the engines and stays low), and tests the IPC socket monitor's loop on such a socket (`WEBKIT:W4:FDS:...`, `WEBKIT:W4:MONITOR:PASS:...`,
both required by the judge).

LazyOS has no file-backed `mmap` yet, and the port's Skia compiles W3's
`lazyos_mmap` shim (`tools/webkit/skiabuild/lazyos_mmap.c`, linked from
`WebCore/PlatformLazyOS.cmake`): without it Skia's font manager refuses every font and
all families draw in one face. Remove it with the kernel limitation.

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
| `port/tree/Source/WebKit/UIProcess/Launcher/ProcessLauncherLazyOS.cpp` | the process launcher: `socketpair(SOCK_SEQPACKET)`, `fork`, `execv` of `WebProcess`/`NetworkProcess` with the client end inherited and `<program> <process id> <fd>` as the command line the generic unix `AuxiliaryProcessMain` parses; one reaper thread per child. Replaces the PlayStation launcher (a closed library) and the GLib one (GSubprocess, `posix_spawn`) |
| `port/tree/Source/WebCore/platform/network/lazyos/CurlSSLHandleLazyOS.cpp` | the curl port's CA trust: the system PEM bundle (`/etc/ssl/certs/ca-certificates.crt`, the Mozilla roots of `docs/tls-plan.md`) instead of the console's CertificateStore |
| `port/upstream.txt` | 77 files of WebKit's git that the tarball lacks (libwpe glue, the curl network port), each fetched from `raw.githubusercontent.com` at the pinned commit and checked against its git blob digest (`tools/webkit/port_tree.py`) |
| `port/glstub/gen_stub.py` | writes `libEGL.a`/`libGLESv2.a` from the Khronos headers: every entry point does nothing (`tools/webkit/recipes_port.py`, built with the bundle) |
| `port/tree/Source/WebKit/WebProcess/InjectedBundle/lazyos/InjectedBundleLazyOS.cpp` | injected bundles are shared libraries; the programs are static executables: refused with a message |
| `port/tree/Source/WebKit/UIProcess/API/C/lazyos/WPELoaderLazyOS.cpp` | the `_wpe_loader_interface` that statically linked libwpe asks its backend for: no backend, so no objects (libwpe then uses its in-process pasteboard) |
| `port/app/{main,engine}.cpp`, `lazyos_ui.h` | `webkit-app`, the desktop UI process (W5, below) |
| `port/tree/Source/WebKit/Platform/IPC/unix/SocketMonitorLazyOS.h` | the IPC connection's socket monitor loop (`poll()`, patch `0022`), header-only so `port/smoke/monitor.cpp` tests the same code |
| `port/smoke/main.cpp` | `webkit-smoke`, the headless UI process: C API only, one `WEBKIT:W4:*` line per step (`tools/webkit/portjudge.py` lists them), plus `INFO` lines for the fonts Fontconfig sees, the glyphs as text and timings |
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
| `0019-webkit-network-cache-data-curl-shares-a-mapped-file` | with content extensions on, the UI process needs `NetworkCache::Data::tryCreateSharedMemory()` to hand a compiled rule list to the web processes; the curl backend defines it for Windows only. It now keeps the mapped file's handle (as the GLib backend does) and wraps the map as `SharedMemory` on Unix-domain-socket platforms |
| `0020-webkit-c-api-says-whether-the-web-process-responds` | `WKPageIsWebProcessResponsive`: the page state client says that the answer to WebKit's 6 s ping changed, not which way; LazyWeb+'s "page not responding" bar needs the value (issue #862) |
| `0021-webkit-the-proxy-of-an-ended-web-process-is-kept` | `WebProcessProxy::processDidTerminateOrFailedToLaunch` keeps a reference to the dead proxy and counts them (`lzw_proxy_kept`, `PROCESS:KEPT`): destroying it when the page is loaded again in a new process aborts the UI process (`crashDueToCheckedPtrToDeadObject`, a thread-safe CheckedPtr count that is already zero), cause not found; not tied to the embedder's calls, to the page being unresponsive, or to timing (issue #862). Covers web processes only: a replaced NetworkProcess's proxy is not kept, and nothing shows that its destruction is safe or not |
| `0022-webkit-ipc-socket-monitor-polls` | the PlayStation path's `SocketMonitor` thread (one per IPC connection, on since `0003`) waited with `select()` on a stack `fd_set`: a descriptor of 1024 or more (a long session with a large HTTP cache: `0019` keeps one open per mapped entry) overflowed the set, and LazyOS's `EINVAL` for `nfds > 1024` made the loop spin without ever seeing the connection close. The loop is now `IPC::LazyOS::monitorSocket` (`port/tree/.../Platform/IPC/unix/SocketMonitorLazyOS.h`): `poll()`, a hangup or error is a close, a descriptor let go of ends the thread, one wait lasts at most 1 s (issue #867) |
| `0023-webcore-curl-stream-scheduler-polls` | the curl port's WebSocket thread (`CurlStreamScheduler::workerThread`) used `select()` with one reused `timeval`, which `select()` counts down to zero, so after the first timeout every wait returned at once, and it never waited at all while a stream had no socket: a NetworkProcess core spun for as long as a page kept a WebSocket. It now `poll()`s the sockets for up to 20 ms every pass. The curl files are fetched (`port/upstream.txt`), so `port_tree.py` fetches before it patches and never fetches a file an applied patch changed |

| `0024-webkit-update-observer-and-in-place-backing-store-scroll` | the classic drawing area told the UI process "the whole view needs display" for any update that scrolled, so every step of a scroll was copied, converted and handed on whole although the web process painted only the exposed rows. `WKPageSetUpdateCallback` (`WKPagePrivatePlayStation.h`) gives the client each update's scrolled rectangle, offset and repainted rectangles instead, and while it is set the view is not reported as needing display for those updates (`paint.cpp` and `xui-app/crates/webkit/src/damage.rs` use it). The Skia `BackingStore::scroll` allocated a bitmap the size of the scrolled area, read into it and wrote it back for every step; a raster surface's pixels are now moved in place (the old copy stays for a surface that cannot be peeked) |
| `0025-webkit-wkpagepaint-stage-counters`, `0026-webkit-wkpagepaint-whole-call-timer` | `WKPageGetPaintStats`: the microseconds `WKPagePaint` spends in making its surface, in `endPrinting`, in the drawing area's paint, and in the snapshot and the `drawImage` inside it, and (0026) the whole call, destructors included. `paint.cpp` prints them as `INFO:WKPAINT` once a second. A call costs about 64 us, which showed that the stage times around it (wall clock on one CPU) include other tasks' time |

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

## The desktop app (W5)

`os.lazy.webkit` is one static-PIE program, `webkit-app`, in two halves that meet in a C ABI that began as sixteen
functions (`port/app/lazyos_ui.h`):

* **the UI process, C++** (`port/app/main.cpp`, `engine.cpp`): WebKit's run loop on the process's main
  thread, as in `webkit-smoke`; a context pointed at `/system/bin/webprocess` and `networkprocess`, a
  `WKView`, navigation and page-state clients. It paints the page with `WKPagePaint` into a bitmap
  (Skia's BGRA, made opaque RGBA) and hands it to the window; mouse, wheel and key events go in through
  `WKPageHandle*Event`. It prints the page markers (`WEBKIT:W5:LOAD|TITLE|JS|CLICK|INPUT|...`);
* **the window, Rust** (`xui-app/crates/webkit`, a static library): a toolbar (Back, Forward,
  Reload/Stop, an address field, Go), the page view (one custom-painted xui node that draws the newest
  frame at its own size and forwards pointer, wheel and keys), a status line, the window caption
  ("<title> - WebKit"), over `LazyOSBackend`. It runs on a second thread started by `main`: a Messenger
  handle belongs to the thread that made it, so the window cannot share the run loop's thread. The two
  talk through a `Mailbox` (the newest frame and page state, one wake per burst) and
  `WKRunLoopCallOnMainThread` (every `lzw_engine_*` call only queues work). The window logic is
  portable and host-tested against a recording engine (`cargo test -p xui-webkit`, snapshots in
  `xui-app/target/snapshots/webkit-*.png`).

`tools/webkit/app.py` builds the Rust half on the host (`cargo build -p xui-webkit --lib` for
`x86_64-unknown-linux-musl`: a static library needs no linker) into `target/webkit/lib`, then links
the program in the build container (`port.py --only webkit-app`; CMake adds the target when
`LAZYOS_UI_LIBRARY` names the library, `Source/WebKit/PlatformLazyOS.cmake`) and copies it to
`target/xui/xui-webkit.elf`, where `tools/xui/core_packages.py` packages it
(`xui-app/packages/webkit`). The image build needs the desktop and the network stack for it
(`build_support/webkit_embed.rs`). Keys reach the page as X11 key symbols (libwpe's key codes);
printable text goes with the key press (`WKKeyboardEvent.text`, patch `0012`) and patch `0013`'s editing
hook inserts it and runs the editing commands (arrows, Delete, select all, copy, paste, undo).

**One WebProcess for every page.** With WebKit's defaults a cross-site navigation swaps in a fresh WebProcess,
and the second 107 MB program's `exec` failed with `ENOMEM`: the kernel charges the *length* of every `mmap`
to the user's memory quota (`docs/architecture/limits.md`), and the three processes take what the quota lets
them reserve (2989 of the big-app profile's 3072 MiB after one page; 6957 of 8192 at the `quota_user_memory=8G`
that `run_demo.py --webkit` and the launcher set). `engine.cpp` turns process swapping, pre-warming and the
process cache off.

**The profile directory.** WebKit's data directories default to the PlayStation's `/app0`, which LazyOS has
not. `engine.cpp` makes `$HOME/.apps/os.lazy.webkit` (the app's own per-user folder; mode `0700`), gives the
page a website data store whose cache, cookie jar (`cookies/cookie.jar.db`) and storage directories are below
it, and names it in `LAZYOS_WEBKIT_PROFILE` and `CURL_COOKIE_JAR_PATH` (curl opens its cookie database
before the data store's file is applied, so the variable is what keeps `cookie.jar.db` out of the working
directory). `ProcessLauncherLazyOS.cpp` starts the children in it, and patch `0015` makes the directories
the C API cannot set (HSTS, alternative services, ...) follow it instead of `/app0`. With no `$HOME` the
store is ephemeral. The manifest's `write:$HOME/.apps/os.lazy.webkit` is exactly this directory.

**The sandbox.** The WebProcess and the NetworkProcess run under confined labels
(`app:os.lazy.webkit/webprocess`, `.../networkprocess`; the `[processes.*]` tables of
`xui-app/packages/webkit/manifest.toml`, the kernel's gates in `docs/architecture/process-sandbox.md`): the
page process reads only fonts and has no network, files beyond them, Messenger interface or way to start a
program; the network process has sockets, the resolver, the CA bundle and the profile folder. The launcher
is unchanged (`fork`, `execv`); the label changes at the `execve`. The UI process is an ordinary app label.
`python tools/webkit/attack_run.py` attacks the sandbox from inside it (below).

**Quitting.** When the window closes the UI process ends the WebProcess and the NetworkProcess itself
(SIGTERM, SIGKILL after a grace period), waits for them and prints `WEBKIT:W5:CHILD:<name>:<pid>:GONE` before
`WEBKIT:W5:QUIT:PASS`. The markers that name a page (`LOAD`, `TITLE`, `JS`, `CLICK`, `INPUT`, `NAV`, `START`) print
only in a UI-probe image (`LAZYOS_UI_PROBE=1`).

**The harness.** `python tools/webkit/app_run.py` (docs/webkit-port-plan.md, W5) builds the desktop image
(`LAZYOS_WEBKIT=1`, `LAZYOS_XUI_AUTOSTART=webkit`, the UI probe, a throwaway test CA and `/etc/hosts` for the
stand-in sites), serves LazyWeb's stand-ins from the host (`tools/web/sites.py` and a few pages of ours in
`tools/web/fixtures/theoldnet.com/webkit/`), boots it, drives the window with the session
`tools/screenshot/examples/webkit.json` and judges the markers, the host's request log and the screenshots
(`tools/webkit/appjudge.py`, self-test `test_app_judge.py`). `--label-trace` also lists the package's
`LABEL:DENY` lines; the manifest's permissions were derived that way. Output is in `shots/webkit`
(`serial.log`, `shot_*.png`, `requests.json`, `net.pcap`). The build uses `LAZYOS_RESET_OS=1`, so **every run
recreates `target/lazyos.img`'s OS volume** (installed apps, settings, logs and files on it are erased); the
run also needs the host's ports 80, 443 and 81 free.

**The attack harness.** `python tools/webkit/attack_run.py` (`tools/abi/fixtures/src/sandboxprobe.rs`,
judge `tools/webkit/attackjudge.py`, self-test `test_attack_judge.py`) attacks the sandbox from inside the
two engine processes: a stand-in program (`python tools/webkit/attack_build.py`, built with Rust's musl
target, no WebKit build needed) tries to read the user's files and the system's, write them, open a socket,
connect to a named socket, resolve the display, execute BusyBox and a native program, `fork`, signal its
parent and `ptrace`, while the few things the process needs must still work. Every attack must be refused
with the policy's `EPERM` and a `LABEL:DENY` line, as in `tools/accounts/run.py`. `--kernel` (the default)
boots a console image whose first program loads the browser's two process tables as labels the way `pkgd`
does and starts the probe under each; `--browser` builds a desktop image with `LAZYOS_WEBKIT_ATTACK=1`, which
ships the stand-in as `/system/bin/webprocess` and `/system/bin/networkprocess` beside the real window, so the
real UI process starts them and the label comes from the package's tables. `LAZYOS_WEBKIT_ATTACK=1` is a
test switch: never set it for an image anyone uses. Like `LAZYOS_TLS_TEST_CA`
it is harness-only on purpose: `attack_run.py` sets it, and there is no
`run_demo.py` flag or launcher GUI control for it.

## LazyWeb+ (`os.lazy.lazywebplus`)

[`docs/lazywebplus.md`](../docs/lazywebplus.md): LazyWeb's chrome on this engine. The same C++ UI process
(`port/app/`: `main`, `engine`, `clients`, `policy`, `uiclient`, `paint`, `log`) links a different Rust
window: `tools/webkit/app.py --plus` builds `xui-app/webplus` (`libxui_webplus.a`) and CMake links it
into `lazywebplus-app` (`LAZYOS_PLUS_LIBRARY`, `PlatformLazyOS.cmake`); `webkit-app` still links
`xui-app/crates/webkit`'s own window. What the UI process installs depends on `lzw_ui_features()`
(`LZW_FEATURE_*`), so the W5 window, which asks for nothing, behaves exactly as before. The ABI grew by
what LazyWeb's chrome needs from an engine that has scripts (all in `port/app/lazyos_ui.h`, each mirrored in
`xui-app/crates/webkit/src/{abi,ffi,events}.rs` with host tests, and in `engine.rs`'s recording engine):

| Need | ABI | WebKit side |
|---|---|---|
| Which features the window uses; the marker prefix; the profile directory | `lzw_ui_features`, `lzw_ui_marker_prefix`, `lzw_ui_profile_dir` | read once, before the window thread exists |
| Progress, the committed URL, the padlock, how a load ended | `lzw_ui_state` (`+committed_url`, `secure`, `finished`) | the page-state and navigation clients; `finished` counts `didFinishNavigation`, so a stop, a failure and a download are told apart |
| The link under the pointer | `lzw_ui_hover` | `mouseDidMoveOverElement` |
| The pointer shape | `lzw_ui_cursor` | the view client's `setCursor` (patch `0016` teaches it the I-beam, wait and resize arrows) |
| Navigation policy: `mailto:` and other schemes to `mimed`, `x-lazyweb:` only from the browser's own pages, a user gesture for both | `lzw_ui_navigate(url, kind, user_gesture, committed_url)` | `decidePolicyForNavigationAction` with `WKNavigationActionHasUnconsumedUserGesture`; the decision is `xui-app/webplus/src/policy.rs`, pure and host-tested |
| Downloads | `lzw_ui_download_start` (the window chooses the `.part` path in `$HOME/Downloads`), `_progress`, `_done`, `_fail`, `lzw_engine_download`, `lzw_engine_download_cancel` | `decidePolicyForNavigationResponse` (an attachment or a type the view cannot show) and the `WKDownload` client; the NetworkProcess writes the file |
| JavaScript `alert`, `confirm`, `prompt` | `lzw_ui_dialog` (returns at once), `lzw_engine_dialog_reply` | the UI client's result listeners, retained until the window answers or the page goes away |
| A new window | `lzw_ui_new_window` | `createNewPage` returns null; the window loads the URL in its one view when a gesture started the request |
| The Notifications API (`LZW_FEATURE_NOTIFICATIONS`, `notify.cpp`) | `lzw_ui_notify_show(id, origin, page_url, title, body, tag)`, `_cancel`, `_gone`, `_permission_request(origin, user_gesture)`, `_permission_gone(origin)`, `lzw_ui_notify_permission_at(index, origin, size)`; `lzw_engine_notify_shown`, `_clicked`, `_closed`, `_permission(origin, 1 allow \| 0 block \| -1 dismiss)`, `_forget` | `WKNotificationProvider` (show, cancel, didDestroyNotification, notificationPermissions) on the context's notification manager, the UI client's `decidePolicyForNotificationPermissionRequest` (the request is retained until the window answers), `WKNotificationManagerProviderDidShow/Click/Close`, `...DidUpdateNotificationPolicy`, `...DidRemoveNotificationPolicies`. Needs `ENABLE_NOTIFICATIONS` (on, `tools/webkit/port_cmake.py`; the W5 window installs no provider, so its pages see `Notification.permission === "denied"`) |
| Engine processes: a process came, went, or stopped answering; the window's way out | `lzw_ui_page_responsive(responsive)`, `lzw_engine_recover(reload)`, `lzw_engine_children(&web, &network)`; WebKit's launcher calls `lzw_launcher_event` (weak) for every launch, exit and terminate | `port/app/children.cpp` (the launcher's list, and the shutdown that kills each process group with retries), the context client's `networkProcessDidCrashWithDetails`, the navigation client's `webProcessDidTerminate`, the page state client's `didChangeWebProcessIsResponsive` (patch `0020`), `ProcessLauncherLazyOS.cpp` (`setpgid`, `PR_SET_PDEATHSIG`, the reports); markers in `docs/lazywebplus.md` |
| Calls on the process's first thread | `lzw_engine_run_on_main(task, context)` | `onMain`: a Messenger service names a caller by its task, and the shell's notification service refuses a thread other than the first |
| Cookies and site data | `lzw_ui_profile_dir` (only with `LZW_FEATURE_PROFILE`; the W5 window gets `$HOME/.apps/os.lazy.webkit`) | `createDataStore` in `engine.cpp`: every directory of the store below the profile (`$HOME/.apps/os.lazy.lazywebplus` for LazyWeb+), the cookie jar at `cookies/cookie.jar.db`, the children started in it (the sandbox: `docs/architecture/process-sandbox.md`) |
| The ad blocker (`LZW_FEATURE_BLOCKING`): rule lists in a store, attached per navigation, a count | `lzw_ui_blocking(url)` (a pure decision, asked before every main-frame load), `lzw_engine_blocking_load`, `_compile`, `_forget`, `lzw_ui_blocking_list(id, status)`, `lzw_ui_blocked(count)` | `port/app/blocking.cpp`: `WKUserContentExtensionStore` below `<profile>/contentextensions`, `WKUserContentController` in the page configuration, `decidePolicyForNavigationAction` (attach or take off before the load starts), the navigation client's `contentRuleListNotification` (patch `0018`); the window half is `xui-app/webplus/src/blocker/` |

Markers: the UI process prints them after the window's prefix (`WEBKIT:W5` or `WEBPLUS`: `UP`,
`WEBPROCESS:UP:<pid>`, `INFO:TIME:...`, `CHILD:<name>:<pid>:GONE`, `QUIT:PASS`); with
`LZW_FEATURE_OWN_MARKERS` the window prints the ones that name pages (`LOAD`, `TITLE`, ...).
`python tools/webplus/run.py` is the harness (`tools/webplus/README.md`).
