"""Fetch the SRB2Kart port's two third-party inputs, each pinned by SHA-256.

* the Kart-Public v1.6 engine source (GPL-2.0-or-later), a GitHub tag archive,
  extracted to ``target/srb2kart/Kart-Public-1.6/``;
* the official v1.6 game data, ``AssetsLinuxOnly.zip`` from Kart Krew's release,
  extracted to ``target/srb2kart/data/`` (everything but ``mdls*``, which only
  the OpenGL renderer reads).

The source digest and the data digest both match the ones in Flathub's
``org.srb2.SRB2Kart`` manifest (checked 2026-10-11). Neither input is ever
committed or put in a package: the data has no licence (docs/srb2kart-port-plan.md).
Downloads are untrusted data: only plain files are extracted, never outside the
destination, and nothing from them is executed except the C sources the build
compiles. The download/verify helpers are Doom's (``tools/doom/fetch.py``).
"""

from __future__ import annotations

import importlib.util
import shutil
import sys
import tarfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
CACHE = ROOT / "target" / "srb2kart"

VERSION = "1.6"
SOURCE_SHA256 = "924489e33ebb7e41bae3b84a65be2aae2ca01e2050938bff1e702b39848958d6"
SOURCE_URL = f"https://github.com/STJr/Kart-Public/archive/refs/tags/v{VERSION}.tar.gz"
DATA_SHA256 = "7a384fb99d42f0cf41d12e3ed873754f9a5ba2b993d5e54bde7962bea3accdd1"
DATA_SIZE = 220_991_971
DATA_URL = f"https://github.com/STJr/Kart-Public/releases/download/v{VERSION}/AssetsLinuxOnly.zip"

#: Data files the engine needs (the rest of the zip is documentation, licences
#: and the GL-only `mdls*`).
DATA_REQUIRED = (
    "srb2.srb", "gfx.kart", "textures.kart", "chars.kart", "maps.kart",
    "sounds.kart", "music.kart", "bonuschars.kart",
)

EXTRACTED_MARK = ".lazyos-extracted"


def _doom_fetch():
    spec = importlib.util.spec_from_file_location(
        "doom_fetch", ROOT / "tools" / "doom" / "fetch.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


_doom = _doom_fetch()
sha256 = _doom.sha256
_inside = _doom._inside


def log(message: str) -> None:
    print(f"srb2kart: {message}", file=sys.stderr)


def download(url: str, dest: Path, digest: str) -> bool:
    return _doom.download(url, dest, digest, log=log)


def source() -> Path | None:
    """The engine source tree (the directory holding `src/`)."""
    tree = CACHE / f"Kart-Public-{VERSION}"
    if (tree / EXTRACTED_MARK).is_file():
        return tree
    archive = CACHE / f"Kart-Public-{VERSION}.tar.gz"
    if not download(SOURCE_URL, archive, SOURCE_SHA256):
        return None
    staging = CACHE / ".extract-source"
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
        extracted = staging / f"Kart-Public-{VERSION}"
        if not (extracted / "src" / "d_main.c").is_file():
            raise ValueError("the archive has no src/d_main.c")
        (extracted / EXTRACTED_MARK).write_text("ok\n", encoding="utf-8")
        extracted.rename(tree)
    except Exception as error:  # noqa: BLE001 - a bad archive is "unavailable"
        log(f"extracting the source failed: {error}")
        shutil.rmtree(tree, ignore_errors=True)
        archive.unlink(missing_ok=True)
        return None
    finally:
        shutil.rmtree(staging, ignore_errors=True)
    return tree


def data() -> Path | None:
    """The directory holding the game data files (`srb2.srb`, `*.kart`, ...)."""
    tree = CACHE / "data"
    mark = CACHE / "data.extracted"  # beside the directory: the image embeds its files as they are
    if mark.is_file() and tree.is_dir():
        return tree
    archive = CACHE / "AssetsLinuxOnly.zip"
    if not download(DATA_URL, archive, DATA_SHA256):
        return None
    shutil.rmtree(tree, ignore_errors=True)
    mark.unlink(missing_ok=True)
    try:
        tree.mkdir(parents=True)
        with zipfile.ZipFile(archive) as zf:
            for info in zf.infolist():
                name = info.filename
                # Flat files only; the GL models (`mdls/`, `mdls.dat`) stay out.
                if info.is_dir() or "/" in name or name.startswith("mdls"):
                    continue
                _inside(tree, name)
                with zf.open(info) as src, open(tree / name, "wb") as dst:
                    shutil.copyfileobj(src, dst)
        missing = [n for n in DATA_REQUIRED if not (tree / n).is_file()]
        if missing:
            raise ValueError(f"the zip lacks {', '.join(missing)}")
        mark.write_text("ok\n", encoding="utf-8")
    except Exception as error:  # noqa: BLE001 - a bad archive is "unavailable"
        log(f"extracting the data failed: {error}")
        shutil.rmtree(tree, ignore_errors=True)
        archive.unlink(missing_ok=True)
        return None
    return tree


if __name__ == "__main__":
    print(source(), data())
