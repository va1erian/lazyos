# JavaScriptCore on LazyOS (`jsc`)

Stages W1 and W2 of [`docs/webkit-port-plan.md`](../../docs/webkit-port-plan.md):
a C++ toolchain proof and WebKit's `JSCOnly` port (the C-loop interpreter
`jsc` and the JIT `jsc-jit`), all static `x86_64-linux-musl` programs built by
zig, run on LazyOS through the Linux ABI.

```bash
python tools/jsc/build.py                 # ICU, cxxproof, jsc -> target/jsc/bin/ (Docker)
python tools/jsc/build.py --jit           # also jsc-jit (a second WebKit build, about 7 minutes)
python tools/jsc/stress.py                # the JSTests/stress subset -> target/jsc/jsc-tests/
python tools/jsc/build.py --only cxxproof # ICU and the proof program only
python tools/jsc/build.py --no-docker     # the same steps on a Linux host with the tools
python tools/jsc/build.py --require       # no Docker / no download is an error
python tools/jsc/run.py                   # build the fixture and image, boot, judge (QEMU)
python tools/jsc/run.py --stage w1        # judge only the C++/ICU proof
python tools/jsc/test_judge.py            # the judge fails when it should
python tools/run_demo.py --jsc            # an interactive image with jsc in /system/bin
```

## What gets built

| output (`target/jsc/bin/`) | what | notes |
|---|---|---|
| `cxxproof` | `cxxproof/main.cpp`: libc++, 8 threads, mutex/condvar, exceptions (nested, across threads), RTTI, TLS with destructors, `std::filesystem`, ICU (`uloc`, `ucnv`, `Normalizer2`, `BreakIterator`) | prints `JSC:W1:CHECK:<name>:OK`, `JSC:W1:PASS:cxx`, `JSC:W1:PASS:icu`, `JSC:W1:PASS` |
| `jsc` | WebKitGTK 2.54.1, `PORT=JSCOnly`, `ENABLE_STATIC_JSC=ON`, `ENABLE_JIT=OFF`, `ENABLE_C_LOOP=ON`, `ENABLE_WEBASSEMBLY=OFF`, `USE_SYSTEM_MALLOC=ON`, `DEVELOPER_MODE=OFF`, Release, `-O2` | the C-loop interpreter; ICU 74.2 with its data inside |
| `jsc-jit` (`--jit`) | the same with `ENABLE_JIT=ON`, `ENABLE_C_LOOP=OFF`, `ENABLE_FTL_JIT=OFF` | LLInt in assembly, baseline and DFG; 52 MiB; its own build tree (`webkit-build-jit`) over the same source tree |

ICU 74.2 is built twice, as the cross-build recipe wants: a host build for its
tools (`icupkg`, `genrb`, `pkgdata`), then a static musl build with
`--with-data-packaging=static`, which makes `libicudata.a` carry the 30 MB
data file as an object (there is no file-backed `mmap` to load it from).
`--disable-tools` must not be used: it leaves a stub `libicudata.a` of 1 KB.

## Docker, and why

The build needs cmake, ninja, perl, python, ruby, gperf and bison, and a
WebKit tree is thousands of small files. `build.py` therefore runs itself
inside `Dockerfile` (Debian 12 pinned by digest; zig 0.16.0 from pip, the
version the rest of the repo pins) with the repository mounted at `/work` and
a named volume `lazyos-jsc-work` at `/build` for sources, build trees and
zig's cache, so none of it crosses a Windows bind mount. Only
`target/jsc/bin/*` comes back through `/work`. The container's command is
`build.py` itself, so `--no-docker` on Linux (CI, WSL) runs the same steps;
set `LAZYOS_JSC_WORK` to move the scratch tree there. To start over:
`docker volume rm lazyos-jsc-work`.

## Sources and patches

`pins.py` pins both tarballs by SHA-256: WebKitGTK 2.54.1 (the `.sums` file
beside it on webkitgtk.org lists the same digest; it is the smallest official
download holding JavaScriptCore, WTF and bmalloc, and the git history is
gigabytes) and ICU 74.2 (the GitHub release's digest). Downloads use
`tools/doom/fetch.py`'s checked `download`. Every change to a tarball is a
file in `patches/<icu|webkit>/*.patch`, applied once in name order by
`toolenv.apply_patches`; nothing is edited by hand. None are needed so far
(`patches/README.md`).

Configuration that needed no patch, only a flag (`webkit.py`):

* `ENABLE_SAMPLING_PROFILER=OFF`: WebKit refuses it with the C-loop.
* `USE_HEADER_MAPS=OFF`: clang's header-map path calls `Tools/Scripts/hmaptool`,
  which the release tarball does not ship.
* `-DSIMDUTF_IMPLEMENTATION_{ICELAKE,HASWELL,WESTMERE}=0`: simdutf's
  runtime-dispatched AVX paths need clang's `evex512` and LazyOS enables no
  AVX state; only the portable implementation is compiled.

The JIT build needs two small source patches (`patches/README.md`): a missing
include, and AVX that must not be used unless the OS enabled it (LazyOS does
not; without it every optimizing tier ends in `SIGILL`). FTL is off: B3 does
not build with WebAssembly off at this revision.

## On LazyOS

`LAZYOS_JSC=1` (`run_demo.py --jsc`, the launcher's Advanced tab) puts `jsc`,
`jsc-jit` and `cxxproof` in `/system/bin` and the stress subset in
`/system/share/jsc-tests` through `build_support/linuxapps_embed.rs`. They are
45, 52 and 31 MB; LazyOS loads an ELF whole into RAM, so the guest wants
`--memory 2G` at least (`--jsc` implies the `--big-app` profile, 4 GiB).
`run.py` builds the `jsc` fixture (`tools/abi/fixtures/src/jsc.rs`) as the
image's `abi-init`. It runs `cxxproof`, then each shell through `jsc -e
'print(1+1)'` and six scripts (`gc`: a heavy allocator with `gc()`/`fullGC()`
and four concurrent marker threads forced on through `JSC_numberOfGCMarkers=4`;
`promise`: classes, Promises, async generators and `Intl`; `hot`: fib(30), a
long loop, a sort, ending with `tier:dfg` for the JIT shell and `tier:none` for
the interpreter; `agents`: four `SharedArrayBuffer` agents allocating and
parking in `Atomics.wait` while the main thread allocates and collects;
`suspend`: the main thread idle on timers while the collector works, which makes
the collector suspend it with a thread-directed `SIGUSR1`), then each file of
`index.txt` in the stress directory with a 10 s limit. It prints, with the prefix
`JSC:` for `jsc` and `JSC:JIT:` for `jsc-jit`:

```
JSC:W1:PASS            JSC:UP:PASS          JSC:START:<ms>
JSC:RUN:PASS:<name>    JSC:TIME:<name>:<ms> JSC:RUN:FAIL:<name>:<reason>
JSC:STRESS:<pass>/<total>  JSC:STRESS:FAIL:<file>:<reason>  JSC:STRESS:TIME:<ms>
JSC:SIGNALS:<name>:<n> (gc, suspend)
JSC:RUN:BEGIN:<name>   JSC:STRESS:RUN:<file>    (they name what the kernel lines after them belong to)
```

`judge.py` reads the serial log; a missing marker is a failure, never a pass,
the stress summary must reach 98% of at least 200 files in each shell and
no file may fail but `get-array-length-reuse.js`, and `JSC:SIGNALS:suspend:<n>`
(the thread-directed signals the process had sent, from
`LazyOS_ThreadSignals` in `/proc/self/status`) must be above zero in both
shells (`run.py` also prints the JIT's speedup per script and every stress failure).
The stress subset is chosen by a fixed rule in `stress.py` (files of
`JSTests/stress` starting `g` or `n`, up to 40 KB, without `//@ skip`,
`requireOptions`, `$vm`, WebAssembly, modules, `load`, `$262` or `$.agent`),
fetched from the WebKit commit the `webkitgtk-2.54.1` tag points to. Its one
known failure, `get-array-length-reuse.js` (`memoryHog`: a 2 GiB typed array),
needs more than the default 2 GiB of guest RAM (`run.py --memory 4G`: 255/255 in both shells).
