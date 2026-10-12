"""The pinned upstream sources of the WebKit dependency bundle (W3), and the paths.

Every archive is pinned by URL and SHA-256 and was cross-checked, in
October 2026, against a second publisher of the same bytes:

* zlib 1.3.2, libjpeg-turbo 3.2.0, expat 2.8.5, HarfBuzz 14.4.0, libpsl 0.23.3,
  curl 8.22.0 and nghttp2 1.70.0: the release asset's SHA-256 that the
  forge's release API reports (computed by the forge at upload, independent of
  this download); nghttp2 also lists it in its ``checksums.txt`` asset;
* OpenSSL 3.5.9 (the 3.5 series is the long-term-support one): the
  ``.tar.gz.sha256`` file next to the tarball in the release;
* fontconfig 2.16.0: the ``.tar.xz.sha256sum`` file on freedesktop.org;
* libxml2 2.15.3: the ``libxml2-2.15.3.sha256sum`` file beside the tarball on
  download.gnome.org;
* SQLite 3.54.0: the SHA3-256 that sqlite.org's download page lists for
  ``sqlite-amalgamation-3540000.zip`` (7b670a62...a2d93; SQLite publishes
  SHA3, not SHA-2, so the pin below is the SHA-256 of the same bytes);
* libwebp 1.6.0, brotli 1.2.0, libpng 1.6.59 and FreeType 2.14.3: the SHA-512
  that Alpine's package recipe (``main/<pkg>/APKBUILD``) pins for the same
  file (libwebp ``5c159d97...``, brotli ``f94542af...``, FreeType
  ``43de86ea...``, libpng ``2d753e76...``). Brotli and libpng are the tags'
  source archives from the project's forge, not a release tarball.

WebKitGTK 2.54.1 (``tools/jsc/pins.py``) holds Skia, which WebKit builds with
its own CMake (``Source/ThirdParty/skia``), so Skia is not a separate pin.
ICU 74.2 is the one the ``jsc`` build made; it is copied into this prefix.

``LAZYOS_JSC_WORK`` moves the scratch tree exactly as for the ``jsc`` build
(the Docker image sets ``/build``, a volume); finished programs land in
``target/webkit/``.
"""

from __future__ import annotations

import sys
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "jsc"))
import pins as jsc_pins  # noqa: E402  (tools/jsc/pins.py)
from pins import Source  # noqa: E402

WORK = jsc_pins.WORK / "webkit-deps"
SRC = jsc_pins.SRC
#: The one prefix every library is installed into (inside the work volume).
PREFIX = WORK / "prefix"
BUILD = WORK / "build"
#: Finished programs and the manifest, read by the image build and the runner.
OUT = ROOT / "target" / "webkit"
BIN = OUT / "bin"
MANIFEST = OUT / "deps.json"


@dataclass(frozen=True)
class Dep:
    """One library: its pinned source and why the bundle has it."""

    name: str
    source: Source
    license: str
    why: str
    #: File name in the source cache (some archives share a generic name).
    cache_name: str = ""

    @property
    def archive(self) -> str:
        return self.cache_name or self.source.archive


H = "https://github.com"

DEPS: list[Dep] = [
    Dep("zlib", Source(
        "1.3.2", f"{H}/madler/zlib/releases/download/v1.3.2/zlib-1.3.2.tar.xz",
        "d7a0654783a4da529d1bb793b7ad9c3318020af77667bcae35f95d0e42a792f3", "zlib-1.3.2"),
        "Zlib", "inflate/deflate everywhere: PNG, HTTP, WOFF, FreeType, curl, OpenSSL"),
    Dep("libpng", Source(
        "1.6.59", f"{H}/pnggroup/libpng/archive/refs/tags/v1.6.59.tar.gz",
        "2540302a1844ad2b2b501977abecfa850f265f97b78f065a712ab4074a89f5b5", "libpng-1.6.59"),
        "libpng-2.0", "PNG decoder (WebCore image decoders, Skia's codec)",
        "libpng-1.6.59.tar.gz"),
    Dep("libjpeg-turbo", Source(
        "3.2.0", f"{H}/libjpeg-turbo/libjpeg-turbo/releases/download/3.2.0/libjpeg-turbo-3.2.0.tar.gz",
        "6f30092cef9fb839779646608f4ee14ae3cbac989c47fa05e841b0841f09878e", "libjpeg-turbo-3.2.0"),
        "IJG, BSD-3-Clause, Zlib", "JPEG decoder (WebCore, Skia)"),
    Dep("libwebp", Source(
        "1.6.0", "https://storage.googleapis.com/downloads.webmproject.org/releases/webp/libwebp-1.6.0.tar.gz",
        "e4ab7009bf0629fd11982d4c2aa83964cf244cffba7347ecd39019a9e38c4564", "libwebp-1.6.0"),
        "BSD-3-Clause", "WebP decoder and demuxer (WebCore requires the demux component)"),
    Dep("brotli", Source(
        "1.2.0", f"{H}/google/brotli/archive/refs/tags/v1.2.0.tar.gz",
        "816c96e8e8f193b40151dad7e8ff37b1221d019dbcb9c35cd3fadbfe6477dfec", "brotli-1.2.0"),
        "MIT", "WOFF2 fonts through FreeType, and Content-Encoding: br through curl",
        "brotli-1.2.0.tar.gz"),
    Dep("freetype", Source(
        "2.14.3", "https://download.savannah.gnu.org/releases/freetype/freetype-2.14.3.tar.xz",
        "36bc4f1cc413335368ee656c42afca65c5a3987e8768cc28cf11ba775e785a5f", "freetype-2.14.3"),
        "FTL or GPL-2.0-or-later", "glyph rasterisation under Skia and the font tables WebCore reads"),
    Dep("expat", Source(
        "2.8.5", f"{H}/libexpat/libexpat/releases/download/R_2_8_5/expat-2.8.5.tar.xz",
        "1e727b8933ec51a77a9a9d9afcf8e688bce45d907c13e36ab7393fe36e703182", "expat-2.8.5"),
        "MIT", "Fontconfig's config parser and Skia's OpenType-SVG glyphs"),
    Dep("harfbuzz", Source(
        "14.4.0", f"{H}/harfbuzz/harfbuzz/releases/download/14.4.0/harfbuzz-14.4.0.tar.xz",
        "2357ed966c6ced7bfa720b0640c0231065af01158fbea215093ffa15aed44371", "harfbuzz-14.4.0"),
        "MIT", "text shaping (WebCore's ComplexTextController, Skia's SVG glyphs), with the ICU glue"),
    Dep("fontconfig", Source(
        "2.16.0", "https://www.freedesktop.org/software/fontconfig/release/fontconfig-2.16.0.tar.xz",
        "6a33dc555cc9ba8b10caf7695878ef134eeb36d0af366041f639b1da9b6ed220", "fontconfig-2.16.0"),
        "HPND-style (MIT-like)", "font matching: Skia's SkFontMgr and WebCore's FontCacheSkia call it"),
    Dep("libxml2", Source(
        "2.15.3", "https://download.gnome.org/sources/libxml2/2.15/libxml2-2.15.3.tar.xz",
        "78262a6e7ac170d6528ebfe2efccdf220191a5af6a6cd61ea4a9a9a5042c7a07", "libxml2-2.15.3"),
        "MIT", "XML, XHTML and SVG parsing (WebCore's XMLDocumentParserLibxml2)"),
    Dep("sqlite", Source(
        "3.54.0", "https://sqlite.org/2026/sqlite-amalgamation-3540000.zip",
        "68e913b0fe8ec6b4e5f391c594a28872884e80d6cd042a1e50c957be46b5aa4d", "sqlite-amalgamation-3540000"),
        "public domain", "localStorage, IndexedDB, the cookie jar and the HSTS/ITP stores"),
    Dep("libpsl", Source(
        "0.23.3", f"{H}/rockdaboot/libpsl/releases/download/0.23.3/libpsl-0.23.3.tar.gz",
        "93941f85a1e7bd593fa94f299233cb5dfc91cd144fd9a78a6ceb75001c5b03be", "libpsl-0.23.3"),
        "MIT", "public-suffix list: which cookie domains are legal (curl port requires it)"),
    Dep("openssl", Source(
        "3.5.9", f"{H}/openssl/openssl/releases/download/openssl-3.5.9/openssl-3.5.9.tar.gz",
        "603f5602e2eef00d77fbd429d34dcd5822bb301757a1bc9cdb24c670f1eb859a", "openssl-3.5.9"),
        "Apache-2.0", "TLS for curl; WebCore's curl port and SubtleCrypto call the OpenSSL API directly"),
    Dep("nghttp2", Source(
        "1.70.0", f"{H}/nghttp2/nghttp2/releases/download/v1.70.0/nghttp2-1.70.0.tar.xz",
        "e05cb1388eaca3830aded4ccf20044b6e1ac1a61411dcca11b0437c4285c8bc2", "nghttp2-1.70.0"),
        "MIT", "HTTP/2 for curl"),
    Dep("curl", Source(
        "8.22.0", f"{H}/curl/curl/releases/download/curl-8_22_0/curl-8.22.0.tar.xz",
        "f7ef3ae8a22e521f289803fe93543eb64c329b58aa73a9e224dfd915a2a5f4f7", "curl-8.22.0"),
        "curl (MIT-style)", "the NetworkProcess HTTP/HTTPS client (WebCore's platform/network/curl)"),
]

BY_NAME = {dep.name: dep for dep in DEPS}


def log(message: str) -> None:
    print(f"webkit: {message}", file=sys.stderr, flush=True)


def fetch(dep: Dep) -> Path | None:
    """The verified archive in the cache, or None (logged)."""
    dest = SRC / dep.archive
    ok = jsc_pins.doom_fetch.download(dep.source.url, dest, dep.source.sha256, log)
    return dest if ok else None


def fetch_webkit() -> Path | None:
    """The WebKitGTK tarball of the ``jsc`` build (it holds Skia), verified."""
    return jsc_pins.fetch(jsc_pins.WEBKIT)
