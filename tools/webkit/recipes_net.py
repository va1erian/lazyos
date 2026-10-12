"""Recipes: networking (OpenSSL, nghttp2, curl).

OpenSSL is the TLS library because WebKit's curl port is written against it:
``WebCore/platform/network/curl`` (``CurlSSLVerifier``, ``OpenSSLHelper``) and
``crypto/openssl`` (SubtleCrypto) call its API directly, so an alternative
(Mbed TLS, rustls) would need those files rewritten. The 3.5 series is the
long-term-support one; it is Apache-2.0, which LGPL-2.1 WebKit may link.
Built ``no-asm``: portable C is slower but cannot execute an instruction
LazyOS does not enable (it keeps AVX state off, docs/tls-plan.md).
"""

from __future__ import annotations

import shutil
from pathlib import Path

import buildkit
import fhspaths
import toolenv
import wkpins
from buildkit import PREFIX
from registry import Lib, register
from toolenv import run

# Where the guest looks, from libs/fhs (fhspaths.py); both are compiled into the archives.
CA_BUNDLE = fhspaths.CA_BUNDLE
OPENSSL = ["linux-x86_64", "no-shared", "no-asm", "no-tests", "no-docs", "no-apps", "no-dso",
           "no-engine", "no-afalgeng", "no-ui-console", "no-comp", "enable-ec_nistp_64_gcc_128",
           "-fPIC", f"--openssldir={fhspaths.OPENSSL_DIR}", "--libdir=lib"]
NGHTTP2 = {"ENABLE_LIB_ONLY": "ON", "BUILD_STATIC_LIBS": "ON",
           "ENABLE_APP": "OFF", "ENABLE_HPACK_TOOLS": "OFF", "ENABLE_EXAMPLES": "OFF",
           "ENABLE_DOC": "OFF", "BUILD_TESTING": "OFF"}
CURL = {"BUILD_CURL_EXE": "OFF", "BUILD_SHARED_LIBS": "OFF", "BUILD_STATIC_LIBS": "ON",
        "BUILD_TESTING": "OFF", "HTTP_ONLY": "ON", "CURL_USE_OPENSSL": "ON",
        "OPENSSL_ROOT_DIR": str(PREFIX), "OPENSSL_USE_STATIC_LIBS": "ON", "CURL_USE_LIBPSL": "ON",
        "USE_NGHTTP2": "ON", "CURL_BROTLI": "ON", "CURL_ZLIB": "ON", "CURL_ZSTD": "OFF",
        "CURL_USE_LIBSSH2": "OFF", "CURL_USE_LIBSSH": "OFF", "USE_LIBIDN2": "OFF",
        "CURL_USE_GSASL": "OFF", "CURL_USE_LIBUV": "OFF", "ENABLE_ARES": "OFF",
        "ENABLE_THREADED_RESOLVER": "ON", "ENABLE_UNIX_SOCKETS": "ON", "ENABLE_CURL_MANUAL": "OFF",
        "CURL_CA_BUNDLE": CA_BUNDLE, "CURL_CA_PATH": "none", "CURL_DISABLE_INSTALL": "OFF",
        "USE_APPLE_IDN": "OFF", "CURL_USE_PKGCONFIG": "ON"}


def _openssl(tree: Path, jobs: int) -> None:
    build = wkpins.BUILD / "openssl"
    shutil.rmtree(build, ignore_errors=True)
    build.mkdir(parents=True)
    wrap = toolenv.wrappers()
    env = {"CC": wrap["zcc"], "CXX": wrap["zc++"], "AR": wrap["zar"], "RANLIB": wrap["zranlib"]}
    run(["perl", str(tree / "Configure"), *OPENSSL, f"--prefix={PREFIX}", *toolenv.CODEGEN],
        build, "openssl configure", env=env)
    run(["make", f"-j{jobs}", "build_libs"], build, "openssl make", env=env)
    run(["make", "install_dev", f"DESTDIR={buildkit.stage()}"], build, "openssl install", env=env)


def _cmake(name: str, options: dict):
    def build(tree: Path, jobs: int) -> None:
        buildkit.cmake_build(name, tree, options, jobs)
    return build


register(Lib("openssl", ["zlib"], OPENSSL, _openssl))
register(Lib("nghttp2", [], NGHTTP2, _cmake("nghttp2", NGHTTP2)))
register(Lib("curl", ["openssl", "nghttp2", "brotli", "zlib", "libpsl"], CURL, _cmake("curl", CURL)))
