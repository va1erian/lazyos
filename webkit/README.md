# WebKit on LazyOS (`webkit/`)

Stage W4 of [`docs/webkit-port-plan.md`](../docs/webkit-port-plan.md): the port
layer of a `PORT=LazyOS` build of WebCore and the WebKit (WebKit2) layer. WebKit
itself is never vendored: `tools/webkit/port.py` takes the pinned WebKitGTK
2.54.1 tarball (the one the `jsc` build uses), patches it, adds the files below
and builds `WebProcess`, `NetworkProcess` and a headless UI process,
`webkit-smoke`, as static-PIE musl programs with zig, in the `jsc` build's Docker
container, against the W3 dependency bundle (`tools/webkit/README.md`).

```bash
python tools/webkit/deps.py          # the bundle, libwpe and the EGL stand-in first
python tools/webkit/port.py          # -> target/webkit/bin/{WebProcess,NetworkProcess,webkit-smoke}
python tools/webkit/port.py --configure   # CMake only
python tools/webkit/port_run.py      # image with LAZYOS_WEBKIT=1, boot, judge (QEMU)
python tools/webkit/test_port_judge.py
python tools/run_demo.py --webkit    # an interactive image (networking, big-app profile)
```

**Resources.** The build needs Docker with about 15 GiB of RAM (a WebCore translation unit can take
a gigabyte; `-j` is chosen by memory), about 10 minutes from a fresh container state (an
incremental rebuild after one port file takes seconds), and about 7 GiB of disk in the
`lazyos-jsc-work` volume (sources, the dependency bundle, build trees) plus three programs of
about 107 MiB each in `target/webkit/bin`. An image with them needs a 768 MiB OS volume (the
build, `run_demo.py` and the launcher size it; `tools/lazygui/osvolume.py`), and the guest 4 GiB
of RAM (`port_run.py` passes it).

In the image the programs are `/system/bin/webprocess`, `/system/bin/networkprocess` and
`/system/bin/webkit-smoke` (program names there are lowercase, `libs/fhs`); the launcher's default
paths are those.

`port.py` also runs `webkit-smoke` once on the build host (the programs are static
musl executables), in a scratch directory of the work volume, and keeps its output in
`target/webkit/smoke.host.txt`; `port_run.py` requires that file and the guest's painted bitmap
(checksum and ink) to equal it. A host run that fails or times out fails the build, and a boot
that painted nothing, or has no host run to compare with, fails the judge. The NetworkProcess keeps
its cookie jar (`cookie.jar.db`) in the working directory it inherits (the scratch directory
there, the launcher's directory on LazyOS); W5 needs a per-user data directory instead.
`webkit-smoke` honours `WEBKIT_SMOKE_TIMEOUT` (seconds, default 90; `port_run.py` raises it under
`--accel none`).

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
