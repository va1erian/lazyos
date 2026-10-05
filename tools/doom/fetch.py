"""Fetch the Doom port's two third-party inputs, each pinned by SHA-256.

* the doomgeneric engine source (GPL-2.0-or-later), a GitHub archive of one
  commit, extracted to ``target/doom/doomgeneric-<rev>/``;
* the Freedoom release zip (BSD-3-Clause data), from which ``freedoom1.wad``
  and its ``COPYING.txt`` are extracted to ``target/doom/freedoom-<ver>/``.

Neither is ever committed. A download goes to a temporary name and is moved
into place only once it matches its digest, and a cached file is re-verified,
so a truncated or tampered file is never used (the BusyBox fetcher's rules).
Every failure returns ``None`` with the reason on stderr; the caller decides
whether that is fatal.
"""

from __future__ import annotations

import hashlib
import shutil
import sys
import tarfile
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
CACHE = ROOT / "target" / "doom"

#: doomgeneric at a reviewed commit; bumping it means updating both lines.
DOOMGENERIC_REV = "dcb7a8dbc7a16ce3dda29382ac9aae9d77d21284"
DOOMGENERIC_SHA256 = "1bd3f7f26220494159a38d71f2847ec81b58d6bbd7c7c8d81b08993018001148"
DOOMGENERIC_URL = f"https://github.com/ozkl/doomgeneric/archive/{DOOMGENERIC_REV}.tar.gz"

#: Freedoom's release; the digest is the one in the release's signed CHECKSUM.
FREEDOOM_VERSION = "0.13.0"
FREEDOOM_SHA256 = "3f9b264f3e3ce503b4fb7f6bdcb1f419d93c7b546f4df3e874dd878db9688f59"
FREEDOOM_URL = (
    f"https://github.com/freedoom/freedoom/releases/download/v{FREEDOOM_VERSION}/"
    f"freedoom-{FREEDOOM_VERSION}.zip"
)

#: Marks a fully extracted tree; a tree without it is partial and rebuilt.
EXTRACTED_MARK = ".lazyos-extracted"


def log(message: str) -> None:
    print(f"doom: {message}", file=sys.stderr)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def download(url: str, dest: Path, digest: str, log=log) -> bool:
    """`dest` holding `url`'s bytes with SHA-256 `digest`; cached when valid.

    `log` is the caller's logger, so its messages carry the caller's prefix."""
    if dest.is_file():
        if sha256(dest) == digest:
            return True
        log(f"cached {dest.name} fails the digest check; discarding")
        dest.unlink()
    dest.parent.mkdir(parents=True, exist_ok=True)
    partial = dest.with_suffix(dest.suffix + ".part")
    log(f"downloading {url}")
    try:
        urllib.request.urlretrieve(url, partial)
        if sha256(partial) != digest:
            raise ValueError("SHA-256 does not match the pinned digest")
        partial.replace(dest)
    except Exception as error:  # noqa: BLE001 - any failure is "unavailable"
        partial.unlink(missing_ok=True)
        log(f"download failed: {error}")
        return False
    return True


def _inside(base: Path, name: str) -> Path:
    """`base / name`, refusing a name that would leave `base`."""
    target = (base / name).resolve()
    if base.resolve() not in target.parents and target != base.resolve():
        raise ValueError(f"archive member escapes the destination: {name}")
    return target


def doomgeneric() -> Path | None:
    """The engine's source directory (the one holding `doomgeneric.c`)."""
    tree = CACHE / f"doomgeneric-{DOOMGENERIC_REV}"
    source = tree / "doomgeneric"
    if (tree / EXTRACTED_MARK).is_file():
        return source
    archive = CACHE / f"doomgeneric-{DOOMGENERIC_REV}.tar.gz"
    if not download(DOOMGENERIC_URL, archive, DOOMGENERIC_SHA256):
        return None
    staging = CACHE / ".extract-doomgeneric"
    shutil.rmtree(staging, ignore_errors=True)
    shutil.rmtree(tree, ignore_errors=True)
    try:
        staging.mkdir(parents=True)
        with tarfile.open(archive, "r:gz") as tar:
            for member in tar.getmembers():
                # Plain files and directories only: no links, no devices.
                if not (member.isfile() or member.isdir()):
                    continue
                _inside(staging, member.name)
                tar.extract(member, staging, set_attrs=False)
        extracted = staging / f"doomgeneric-{DOOMGENERIC_REV}"
        if not (extracted / "doomgeneric" / "doomgeneric.c").is_file():
            raise ValueError("the archive has no doomgeneric/doomgeneric.c")
        (extracted / EXTRACTED_MARK).write_text("ok\n", encoding="utf-8")
        extracted.rename(tree)
    except Exception as error:  # noqa: BLE001 - a bad archive is "unavailable"
        log(f"extracting doomgeneric failed: {error}")
        shutil.rmtree(tree, ignore_errors=True)
        archive.unlink(missing_ok=True)
        return None
    finally:
        shutil.rmtree(staging, ignore_errors=True)
    return source


def freedoom() -> Path | None:
    """A directory holding `freedoom1.wad` and `COPYING.txt`."""
    tree = CACHE / f"freedoom-{FREEDOOM_VERSION}"
    if (tree / EXTRACTED_MARK).is_file():
        return tree
    archive = CACHE / f"freedoom-{FREEDOOM_VERSION}.zip"
    if not download(FREEDOOM_URL, archive, FREEDOOM_SHA256):
        return None
    shutil.rmtree(tree, ignore_errors=True)
    try:
        tree.mkdir(parents=True)
        with zipfile.ZipFile(archive) as zf:
            for name in ("freedoom1.wad", "COPYING.txt"):
                member = f"freedoom-{FREEDOOM_VERSION}/{name}"
                with zf.open(member) as src, open(tree / name, "wb") as dst:
                    shutil.copyfileobj(src, dst)
        (tree / EXTRACTED_MARK).write_text("ok\n", encoding="utf-8")
    except Exception as error:  # noqa: BLE001 - a bad archive is "unavailable"
        log(f"extracting Freedoom failed: {error}")
        shutil.rmtree(tree, ignore_errors=True)
        archive.unlink(missing_ok=True)
        return None
    return tree
