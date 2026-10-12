"""The pinned upstream sources of the JavaScriptCore build, and the paths.

Every archive is pinned by URL and SHA-256, and the digest is cross-checked
against what upstream publishes:

* WebKitGTK 2.54.1 (2026-10-02, a stable release): the ``.sums`` file next to
  the tarball on https://webkitgtk.org/releases/ lists the same SHA-256. The
  tarball is the only way to get JavaScriptCore, WTF and bmalloc without
  cloning WebKit's multi-gigabyte git history;
* ICU 74.2: the GitHub release's published digest for ``icu4c-74_2-src.tgz``
  (68db0822...e08c).

``LAZYOS_JSC_WORK`` moves the scratch tree (the Docker image sets ``/build``,
a volume, so the many thousand small files of a WebKit build never cross a
Windows bind mount); finished programs always land in ``target/jsc/bin`` of
the repository.
"""

from __future__ import annotations

import os
import sys
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "doom"))
import fetch as doom_fetch  # noqa: E402  (tools/doom/fetch.py)

WORK = Path(os.environ.get("LAZYOS_JSC_WORK") or ROOT / "target" / "jsc-work")
SRC = WORK / "src"
#: Finished programs, read by `build_support/jsc_embed.rs` and the runner.
OUT = ROOT / "target" / "jsc" / "bin"
PATCHES = HERE / "patches"
#: Marks a fully extracted tree; a tree without it is partial and redone.
EXTRACTED_MARK = ".lazyos-extracted"


@dataclass(frozen=True)
class Source:
    version: str
    url: str
    sha256: str
    #: The directory the archive unpacks into.
    topdir: str

    @property
    def archive(self) -> str:
        return self.url.rsplit("/", 1)[1]


ICU = Source(
    "74.2",
    "https://github.com/unicode-org/icu/releases/download/release-74-2/icu4c-74_2-src.tgz",
    "68db082212a96d6f53e35d60f47d38b962e9f9d207a74cfac78029ae8ff5e08c",
    "icu",
)
WEBKIT = Source(
    "2.54.1",
    "https://webkitgtk.org/releases/webkitgtk-2.54.1.tar.xz",
    "ea0bbb02dbdbc596874a4e7ad35b66645b3e0a232bd0e4081de5ed92eb0a397d",
    "webkitgtk-2.54.1",
)


def log(message: str) -> None:
    print(f"jsc: {message}", file=sys.stderr, flush=True)


def fetch(source: Source) -> Path | None:
    """The verified archive in the cache, or None (logged)."""
    dest = SRC / source.archive
    return dest if doom_fetch.download(source.url, dest, source.sha256, log) else None
