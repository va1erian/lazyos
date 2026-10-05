"""The pinned upstream sources of the Linux apps, and their safe extraction.

Every archive is pinned by version, URL and SHA-256. The digests were taken
from the downloaded bytes and cross-checked against what upstream publishes:

* Lua 5.4.7: the SHA-256 on https://www.lua.org/ftp/;
* SQLite 3.46.1: upstream publishes SHA3-256 values; the ``sqlite3.c`` inside
  this zip matches the "SHA3-256 for sqlite3.c" of the 3.46.1 release log
  (186a1baa...7dad);
* jq 1.7.1: the ``sha256sum.txt`` of the GitHub release;
* dash 0.5.12: upstream publishes no digest; the tarball's SHA-512 matches
  the one Alpine 3.18's ``main/dash/APKBUILD`` records for the same URL.

ripgrep's own crate is pinned here too (``rg.py`` patches it before
building); its dependencies are pinned by the checksums in its ``Cargo.lock``,
which ``cargo build --locked`` enforces.

The download itself is ``tools/doom/fetch.py``'s ``download`` (temporary name,
digest verified before the file is moved into place, cached copies re-checked).
"""

from __future__ import annotations

import shutil
import stat
import sys
import tarfile
import zipfile
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "doom"))
import fetch as doom_fetch  # noqa: E402  (tools/doom/fetch.py)

WORK = ROOT / "target" / "linuxapps"
CACHE = WORK / "src"
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


PINS: dict[str, Source] = {
    "lua": Source(
        "5.4.7",
        "https://www.lua.org/ftp/lua-5.4.7.tar.gz",
        "9fbf5e28ef86c69858f6d3d34eccc32e911c1a28b4120ff3e84aaa70cfbf1e30",
        "lua-5.4.7",
    ),
    "sqlite3": Source(
        "3.46.1",
        "https://www.sqlite.org/2024/sqlite-amalgamation-3460100.zip",
        "77823cb110929c2bcb0f5d48e4833b5c59a8a6e40cdea3936b99e199dbbe5784",
        "sqlite-amalgamation-3460100",
    ),
    "jq": Source(
        "1.7.1",
        "https://github.com/jqlang/jq/releases/download/jq-1.7.1/jq-1.7.1.tar.gz",
        "478c9ca129fd2e3443fe27314b455e211e0d8c60bc8ff7df703873deeee580c2",
        "jq-1.7.1",
    ),
    "dash": Source(
        "0.5.12",
        "http://gondor.apana.org.au/~herbert/dash/files/dash-0.5.12.tar.gz",
        "6a474ac46e8b0b32916c4c60df694c82058d3297d8b385b74508030ca4a8f28a",
        "dash-0.5.12",
    ),
    # A .crate is a gzipped tarball; the digest is the crates.io index's
    # `cksum` for ripgrep 14.1.1, the same value `cargo` itself checks.
    "rg": Source(
        "14.1.1",
        "https://static.crates.io/crates/ripgrep/ripgrep-14.1.1.crate",
        "f77b8032dc584527975f34aa5a897d0ef5a785573fda778771a614ff9da501d9",
        "ripgrep-14.1.1",
    ),
}


def log(message: str) -> None:
    print(f"linuxapps: {message}", file=sys.stderr)


def _inside(base: Path, name: str) -> Path:
    """`base / name`, refusing a member name that would leave `base`."""
    target = (base / name).resolve()
    root = base.resolve()
    if target != root and root not in target.parents:
        raise ValueError(f"archive member escapes the destination: {name}")
    return target


def _zip_is_link(info: zipfile.ZipInfo) -> bool:
    """A zip member whose Unix mode (the high half of `external_attr`) is a symlink."""
    return stat.S_ISLNK(info.external_attr >> 16)


def _unpack(archive: Path, staging: Path) -> None:
    """Plain files and directories only: no links, no devices."""
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as zf:
            for info in zf.infolist():
                if _zip_is_link(info):
                    continue
                _inside(staging, info.filename)
                zf.extract(info, staging)
        return
    with tarfile.open(archive, "r:*") as tar:
        for member in tar.getmembers():
            if not (member.isfile() or member.isdir()):
                continue
            _inside(staging, member.name)
            tar.extract(member, staging, set_attrs=False)


def fetch(name: str) -> Path | None:
    """The pristine extracted source tree of `name`, or None (reason logged).

    The tree lives in ``target/linuxapps/src/<topdir>``; recipes copy it before
    writing generated files, so a cached tree is never modified.
    """
    pin = PINS[name]
    tree = CACHE / pin.topdir
    if (tree / EXTRACTED_MARK).is_file():
        return tree
    archive = CACHE / pin.archive
    if not doom_fetch.download(pin.url, archive, pin.sha256, log):
        return None
    staging = CACHE / f".extract-{name}"
    shutil.rmtree(staging, ignore_errors=True)
    shutil.rmtree(tree, ignore_errors=True)
    try:
        staging.mkdir(parents=True)
        _unpack(archive, staging)
        extracted = staging / pin.topdir
        if not extracted.is_dir():
            raise ValueError(f"the archive has no {pin.topdir}/")
        (extracted / EXTRACTED_MARK).write_text("ok\n", encoding="utf-8")
        extracted.rename(tree)
    except Exception as error:  # noqa: BLE001 - a bad archive is "unavailable"
        log(f"extracting {pin.archive} failed: {error}")
        shutil.rmtree(tree, ignore_errors=True)
        archive.unlink(missing_ok=True)
        return None
    finally:
        shutil.rmtree(staging, ignore_errors=True)
    return tree


def work_copy(name: str) -> Path | None:
    """A fresh, writable copy of `name`'s source in ``target/linuxapps/build``."""
    tree = fetch(name)
    if tree is None:
        return None
    copy = WORK / "build" / name
    shutil.rmtree(copy, ignore_errors=True)
    shutil.copytree(tree, copy)
    return copy
