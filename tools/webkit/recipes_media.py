"""Recipes: compression and image libraries (zlib, brotli, libpng, libjpeg-turbo, libwebp)."""

from __future__ import annotations

from pathlib import Path

import buildkit
from registry import Lib, register

ZLIB = {"ZLIB_BUILD_TESTING": "OFF", "ZLIB_BUILD_SHARED": "OFF", "ZLIB_BUILD_STATIC": "ON",
        "ZLIB_INSTALL": "ON"}
BROTLI = {"BROTLI_DISABLE_TESTS": "ON", "BROTLI_BUILD_TOOLS": "OFF"}
PNG = {"PNG_SHARED": "OFF", "PNG_STATIC": "ON", "PNG_TESTS": "OFF", "PNG_TOOLS": "OFF",
       "PNG_FRAMEWORK": "OFF", "ZLIB_USE_STATIC_LIBS": "ON"}
# SIMD off: libjpeg-turbo's assembly needs NASM, and LazyOS enables no AVX state.
JPEG = {"ENABLE_SHARED": "OFF", "ENABLE_STATIC": "ON", "WITH_SIMD": "OFF", "WITH_TURBOJPEG": "OFF",
        "WITH_JAVA": "OFF", "WITH_TESTS": "OFF", "WITH_TOOLS": "OFF"}
WEBP = {"WEBP_BUILD_ANIM_UTILS": "OFF", "WEBP_BUILD_CWEBP": "OFF", "WEBP_BUILD_DWEBP": "OFF",
        "WEBP_BUILD_GIF2WEBP": "OFF", "WEBP_BUILD_IMG2WEBP": "OFF", "WEBP_BUILD_VWEBP": "OFF",
        "WEBP_BUILD_WEBPINFO": "OFF", "WEBP_BUILD_WEBPMUX": "OFF", "WEBP_BUILD_EXTRAS": "OFF",
        "WEBP_BUILD_LIBWEBPMUX": "ON", "WEBP_ENABLE_SWAP_16BIT_CSP": "OFF"}


def _cmake(name: str, options: dict):
    def build(tree: Path, jobs: int) -> None:
        buildkit.cmake_build(name, tree, options, jobs)
    return build


register(Lib("zlib", [], ZLIB, _cmake("zlib", ZLIB)))
register(Lib("brotli", [], BROTLI, _cmake("brotli", BROTLI)))
register(Lib("libpng", ["zlib"], PNG, _cmake("libpng", PNG)))
register(Lib("libjpeg-turbo", [], JPEG, _cmake("libjpeg-turbo", JPEG)))
register(Lib("libwebp", [], WEBP, _cmake("libwebp", WEBP)))
