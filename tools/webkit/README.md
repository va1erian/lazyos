# The WebKit dependency bundle (`tools/webkit`)

Stage W3 of [`docs/webkit-port-plan.md`](../../docs/webkit-port-plan.md): every
library WebCore and the WebKit layer need at the pinned revision (WebKitGTK
2.54.1), built as static `x86_64-linux-musl` archives by zig from pinned,
hash-checked sources, and a C++ program, `depsproof`, that links all of them
and uses each one on LazyOS.

```bash
python tools/webkit/deps.py               # the bundle and depsproof -> target/webkit/ (Docker)
python tools/webkit/deps.py --only curl   # one library and what it needs
python tools/webkit/deps.py --list        # the recipe names, in build order
python tools/webkit/deps.py --require     # no Docker / no download is an error
python tools/webkit/deps.py --no-docker   # the same steps on a Linux host with the tools
python tools/webkit/run.py                # image + host TLS servers + QEMU, judged
python tools/webkit/test_judge.py         # the judge fails when it should
python tools/run_demo.py --webkit-deps    # an interactive image with depsproof in /system/bin
```

## What gets built

Everything installs into one prefix (`/build/webkit-deps/prefix` in the `jsc`
build's Docker volume), `-O2 -fPIC`, static, with no shared library anywhere.
`target/webkit/deps.json` lists each library's version, archive digest,
licence, installed files and archive size, the build time of each step and the
zig version. The table of what each library is for, and its licence, is in the
plan (§5 W3); the pins and how each digest was cross-checked are in
[`wkpins.py`](wkpins.py).

| step | how |
|---|---|
| zlib, brotli, libpng, libjpeg-turbo (SIMD off), libwebp (+demux, mux), FreeType (zlib, PNG, brotli; no HarfBuzz), expat, HarfBuzz (+FreeType, +ICU glue), libxml2 (ICU, no iconv), nghttp2 (library only), curl (OpenSSL, HTTP/2, brotli, libpsl, threaded resolver, HTTP and HTTPS only) | their own CMake projects, `recipes_media.py`, `recipes_text.py`, `recipes_net.py` |
| Fontconfig, libpsl | autoconf, configured for the guest's paths (fonts in `/system/share/fonts`, configuration in `/system/etc`, cache in `/tmp/fontconfig`) |
| OpenSSL 3.5 (LTS) | `Configure linux-x86_64 no-asm no-shared`, libraries and headers only |
| SQLite | one translation unit, the amalgamation, with FTS3 and R-trees |
| ICU 74.2 | the build of `tools/jsc/icu.py`, copied into the prefix |
| Skia | `skiabuild/CMakeLists.txt` builds the Skia inside the WebKit tarball (`Source/ThirdParty/skia`) with WebKit's own CMake, a few macros stood in for; no GN, no depot_tools |
| `depsproof` | `depsproof/*.cpp`, linked from `pkg-config --static`; run once on the build host (`target/webkit/depsproof.host.txt`) |
| libwpe 1.16.2, `glstub` | W4 (`recipes_port.py`): libwpe's CMake, static, no libxkbcommon; `libEGL.a`/`libGLESv2.a` in which every entry point of the Khronos headers does nothing (`webkit/port/glstub/gen_stub.py`) |

Nothing in any tarball is edited (`patches/<name>/*.patch` is supported by
`buildkit.apply_patches` and is empty).

## The port (W4)

`python tools/webkit/port.py` builds WebCore and WebKit2 as `PORT=LazyOS` against this bundle
(`webkit/README.md`); `port_run.py`, `portjudge.py`, `test_port_judge.py`, `port_tree.py`
(tarball + patches + the files the tarball lacks), `port_cmake.py` and
`gen_platform_patch.py` (regenerates patch 0003) are its tools.

## Docker, stamps, rebuilds

`deps.py` runs itself in the `jsc` build's container (`tools/jsc/Dockerfile`,
the volume `lazyos-jsc-work`), so a Windows host needs only Docker and the
sources are shared with the `jsc` build (the WebKit tarball, ICU).
A stamp per library (`<prefix>/.stamps/<name>.json`) records the recipe key
(source digest, configuration, patches, the keys of the libraries it needs);
a rebuild skips what matches. To start over: `docker volume rm
lazyos-jsc-work` (this also drops the `jsc` build), or delete
`/build/webkit-deps` inside it.

## On LazyOS

`LAZYOS_WEBKIT_DEPS=1` (`run_demo.py --webkit-deps`, the launcher's Advanced
tab) puts `depsproof` in `/system/bin` through
`build_support/linuxapps_embed.rs`; it is 45 MB (30 MB of it ICU's data) and
LazyOS loads an ELF whole, so the switch implies the `--big-app` profile (4 GiB)
and networking. `run.py` builds a console image with the harness's test CA and
`tls.test` mapped to the host (the mechanism of `tools/net/tls_run.py`), starts
the host's TLS servers, types `depsproof https://tls.test:47790/` and judges:

```
WEBKIT:W3:PASS:<lib>   WEBKIT:W3:FAIL:<lib>:<reason>   WEBKIT:W3:INFO:<lib>:<key>=<value>
WEBKIT:W3:PASS         (the last line, when every library passed)
```

for `zlib brotli icu libpng libjpeg-turbo libwebp libxml2 expat sqlite freetype
harfbuzz fontconfig skia libpsl openssl nghttp2 curl`. The pictures are encoded
and decoded in the program (PNG, JPEG, WebP; WebP also demuxed), the font is
Liberation Sans compiled in, fontconfig scans a directory the program writes to
`/tmp`, HarfBuzz shapes with the ICU and FreeType glue, and Skia draws a line and
a line of text into a CPU surface through `SkFontMgr_New_FontConfig` (the
path WebKit's `FontCacheSkia` takes) and checksums it. The judge requires the
decoded pixels, the glyph bitmap, the shaping result and the Skia surface to equal
what the same program printed on the build host, the page curl fetched to be
the one the server holds, and the server to have seen the SNI and one request
for `/`.
