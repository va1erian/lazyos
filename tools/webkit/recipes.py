"""The full recipe list, in build order, and ``ensure`` to install one."""

from __future__ import annotations

import recipes_media  # noqa: F401  (each module registers its libraries)
import recipes_net  # noqa: F401
import recipes_text  # noqa: F401
import skia  # noqa: F401
from registry import LIBS, ensure  # noqa: F401

#: Libraries first, then the raster backend, then the program that uses them.
ORDER = ["zlib", "brotli", "libpng", "libjpeg-turbo", "libwebp", "icu", "freetype", "expat",
         "harfbuzz", "fontconfig", "libxml2", "sqlite", "libpsl", "openssl", "nghttp2", "curl",
         "skia", "depsproof"]
