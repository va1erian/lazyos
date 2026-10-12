"""Skia, the raster backend of WebKitGTK 2.54.1, as ``libSkia.a``.

At this revision the GTK and WPE ports are Skia only (``USE_CAIRO`` is
``OFF`` in ``OptionsGTK.cmake``) and Skia is not a download: it is
``Source/ThirdParty/skia`` inside the WebKit tarball the ``jsc`` build already
pins, built by WebKit's own CMake as a static library. ``skiabuild/CMakeLists.txt``
builds that directory with the few WebKit macros it uses stood in for, so the
archive is the one WebKit would link, without a GN build (which would need
depot_tools and a different Skia). One shim, ``skiabuild/lazyos_mmap.c``, makes
font files readable without a file-backed ``mmap`` (the only deviation).
"""

from __future__ import annotations

import hashlib
from pathlib import Path

import buildkit
import toolenv
import wkpins
from registry import Lib, register

DEP = wkpins.BUNDLED_BY_NAME["skia"]
WRAPPER = wkpins.HERE / "skiabuild"
OPTIONS = {"CMAKE_CXX_STANDARD": "20"}


def _digest() -> str:
    text = b"".join((WRAPPER / name).read_bytes() for name in ("CMakeLists.txt", "lazyos_mmap.c"))
    return hashlib.sha256(text).hexdigest()[:12]


def webkit_tree() -> Path:
    """The unpacked WebKitGTK tarball (shared with the ``jsc`` build's cache)."""
    archive = wkpins.fetch_webkit()
    if archive is None:
        raise toolenv.BuildError("WebKitGTK download unavailable")
    return toolenv.extract(archive, wkpins.jsc_pins.WEBKIT, wkpins.jsc_pins.WORK / "webkit-src")


def _build(tree: Path, jobs: int) -> None:
    options = dict(OPTIONS, WEBKIT_SOURCE_DIR=str(webkit_tree()))
    buildkit.cmake_build("skia", WRAPPER, options, jobs)


register(Lib("skia", ["freetype", "fontconfig", "libpng", "libjpeg-turbo", "expat", "harfbuzz"],
             {"options": OPTIONS, "wrapper": _digest()}, _build, own_tree=True,
             extra={"version": "WebKitGTK 2.54.1 ThirdParty/skia",
                    "source_sha": wkpins.jsc_pins.WEBKIT.sha256,
                    "info": {"url": DEP.source.url, "license": DEP.license, "why": DEP.why,
                             "sha256": DEP.source.sha256, "archive": DEP.source.archive}}))
