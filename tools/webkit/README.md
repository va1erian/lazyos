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
python tools/webkit/licenses.py           # the licence table and register (LGPL-2.1 allowlist)
# the unit tests, one file each (they share module names with tools/jsc, so no `unittest discover`):
# test_judge.py test_buildkit.py test_registry.py test_recipes.py test_paths.py test_licenses.py
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
| Fontconfig, libpsl | autoconf, configured for the guest's paths (fonts, configuration and cache directories, from `libs/fhs`, see below) |
| OpenSSL 3.5 (LTS) | `Configure linux-x86_64 no-asm no-shared`, libraries and headers only; **Apache-2.0: never linked into LazyWeb**, see Licences |
| SQLite | one translation unit, the amalgamation, with FTS3 and R-trees |
| ICU 74.2 | the build of `tools/jsc/icu.py`, copied into the prefix |
| Skia | `skiabuild/CMakeLists.txt` builds the Skia inside the WebKit tarball (`Source/ThirdParty/skia`) with WebKit's own CMake, a few macros stood in for; no GN, no depot_tools |
| `depsproof` | `depsproof/*.cpp`, linked from `pkg-config --static`; run once on the build host (`target/webkit/depsproof.host.txt`) |
| libwpe 1.16.2, `glstub` | W4 (`recipes_port.py`): libwpe's CMake, static, no libxkbcommon; `libEGL.a`/`libGLESv2.a` in which every entry point of the Khronos headers does nothing (`webkit/port/glstub/gen_stub.py`) |

Nothing in any tarball is edited (`patches/<name>/*.patch` is supported by
`buildkit.apply_patches` and is empty).

## Guest paths come from `libs/fhs`

Fontconfig (font, configuration and cache directories), OpenSSL
(`--openssldir`) and curl (CA bundle) compile their search paths into the
static archives, and `depsproof` uses two of them. They are not written out in
the recipes: `fhspaths.py` reads the `pub const` strings of the `fhs` crate
(`share::FONTS`, `system::SYSTEM_ETC`, `state::FONTCONFIG_CACHE`,
`etc::LINUX_SSL`, `etc::LINUX_CA_BUNDLE`, `mount::TMP`), the recipes take them
from there and `proof.py` hands the C++ the same values as `-D` macros
(`proof.h` refuses to compile without them). `test_paths.py` fails when a
recipe or a `depsproof` source spells such a path out again.
`python tools/fhs/check_literals.py` covers Rust sources only.

## Licences

`Dep.license` in `wkpins.py` is an SPDX expression; `python
tools/webkit/licenses.py` prints the table and fails on a licence an LGPL-2.1
binary cannot absorb, on a missing `THIRD_PARTY.md` row, or when `deps.json`
disagrees with the pins. Two entries are called out:

- **OpenSSL is Apache-2.0, which is GPLv2-incompatible.** LGPL-2.1 WebKit may
  link it (its curl port and SubtleCrypto call the OpenSSL API directly) and the
  GPLv3 image may ship it, but it must **never be linked into LazyWeb**
  (GPL-2.0-only, `docs/lazyweb.md`, `AGENTS.md`) or the `nettls` stack.
  `wkpins.NEVER_IN_LAZYWEB` names it and `licenses.py` fails if LazyWeb's
  sources reference this bundle or depend on OpenSSL.
- **libpsl is MIT, but the public-suffix list built into it is MPL-2.0**
  (`MIT AND MPL-2.0`).

## Pins

Every archive is pinned by URL and SHA-256 and cross-checked against a second
publisher (`wkpins.py` says which). libpng is the project's SourceForge release
tarball. brotli has no source release (its GitHub release holds Windows builds
and test data), so it is GitHub's tag archive, which GitHub does not promise to
keep byte-identical; Fedora's source cache (addressed by the SHA-512) holds the
same bytes and is its fallback URL (`Dep.mirrors`).

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
a rebuild skips what matches. A library never installs into the prefix
directly: its install step writes under `DESTDIR` (`buildkit.stage()`) and
leaving the recipe copies the staged files into the prefix, so the file list in
the stamp, and in `deps.json`, is what that install wrote even when the prefix
already held the library. A rebuild first deletes the files of the library's
previous install (and its stamp), so nothing of an old version stays and a
failed rebuild is not mistaken for a finished one. To start over: `docker volume rm
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
WEBKIT:W3:BIN:<sha256> (right after WEBKIT:W3:UP: the program's own digest)
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
the one the server holds, and the server to have seen a completed handshake
with the SNI and at least one request for `/` from `depsproof`.

The build runs the same program once on the Linux build host and keeps its output
as `target/webkit/depsproof.host.txt`. Both runs print `WEBKIT:W3:BIN`, the
SHA-256 of their own executable (read through `/proc/self/exe`), and the judge
fails, naming the cause, unless the guest's equals the host reference's and the
reference's equals that of `target/webkit/bin/depsproof` now: an image built
from an older `depsproof`, or a `deps.py --only X` since the reference was
written, can no longer produce a comparison between unrelated builds. A value
that says nothing (empty, all zeros, a Skia surface with no inked pixel) never
counts as agreement.

## Font files without file-backed `mmap`

LazyOS cannot map a regular file (`mmap` of one is `ENODEV`,
`kernel/src/process/linux/mmapfile.rs`), and Skia maps font files with no
fallback. `skiabuild/lazyos_mmap.c` replaces `mmap` in the one Skia source that
does it: it maps for real when the kernel can, and copies the file into an
anonymous mapping only when the error is `ENODEV` or `ENOSYS`; any other error
reaches the caller. Two limits to remember: the copy path never runs on the
build host (where `mmap` works), so the host-versus-guest comparison does not
cover it, only the guest's own run does; and a copy holds the whole file in RAM
for as long as it is mapped, so large fonts cost their size each time against
the 2 GiB profile.
