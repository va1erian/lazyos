"""The WebKit source tree of the port (W4): the pinned tarball, patched, with the
upstream files it lacks and the port's own files laid over it.

``webkit/`` holds only the port: ``patches/*.patch`` (changes to the tarball),
``port/tree/`` (new files, laid out as in the WebKit tree: the ``Options`` and
``Platform`` CMake files and the LazyOS sources), ``port/upstream.txt`` (files of
WebKit's git that the tarball does not carry, with their blob digests). WebKit
itself is never vendored. The tree is extracted next to (not over) the one the
``jsc`` build uses, so a port rebuild never touches that build.

Idempotent: a stamp holds a digest of everything that goes into the tree; when
it changes the tree is made again from the tarball (patches cannot be undone),
except that a change to ``port/tree`` alone only recopies the files that
differ, which keeps ninja's rebuild incremental.
"""

from __future__ import annotations

import hashlib
import shutil
import urllib.request
from pathlib import Path

import toolenv
import wkpins
from toolenv import BuildError, run

PORT_DIR = wkpins.ROOT / "webkit"
PATCHES = PORT_DIR / "patches"
#: The patches of the `jsc-jit` build, which the port needs as well.
JSC_PATCHES = wkpins.ROOT / "tools" / "jsc" / "patches" / "webkit"
OVERLAY = PORT_DIR / "port" / "tree"
UPSTREAM = PORT_DIR / "port" / "upstream.txt"
TREE = wkpins.jsc_pins.WORK / "webkit-port-src"
COMMIT = "628d6ce1850e17225185a5807a9d0aa375bae2be"
RAW = f"https://raw.githubusercontent.com/WebKit/WebKit/{COMMIT}/"
STAMP = ".lazyos-port-tree"


def patch_files() -> list[Path]:
    """JSC's patches first (they are the proven ones), then the port's; name order."""
    return [*sorted(JSC_PATCHES.glob("*.patch")), *sorted(PATCHES.glob("*.patch"))]


def upstream_files() -> list[tuple[str, str]]:
    """``(git blob sha1, path)`` of every file in ``upstream.txt``."""
    entries = []
    for line in UPSTREAM.read_text().splitlines():
        if line.strip() and not line.startswith("#"):
            sha, path = line.split()
            entries.append((sha, path))
    return entries


def overlay_files() -> list[Path]:
    return sorted(p for p in OVERLAY.rglob("*") if p.is_file())


def patch_stamp() -> list[str]:
    """``name:digest`` of every patch, in application order."""
    return [f"{p.name}:{hashlib.sha256(p.read_bytes()).hexdigest()[:12]}" for p in patch_files()]


def git_blob_sha1(data: bytes) -> str:
    return hashlib.sha1(b"blob %d\0" % len(data) + data).hexdigest()


def patched_paths(patches: list[Path]) -> set[str]:
    """The tree paths the patches change (their ``+++ b/<path>`` lines)."""
    paths = set()
    for patch in patches:
        for line in patch.read_text(errors="replace").splitlines():
            if line.startswith("+++ b/"):
                paths.add(line[len("+++ b/"):].split("\t")[0].strip())
    return paths


def applied_patches(tree: Path) -> list[Path]:
    done = tree / ".lazyos-patched"
    names = set(done.read_text().split()) if done.is_file() else set()
    return [p for p in patch_files() if p.name in names]


def fetch_upstream(tree: Path) -> None:
    """Download the files `upstream.txt` lists into `tree`, each checked by its blob digest.

    Runs before the patches, which may change these files too (patch 0023 changes the curl port's
    WebSocket scheduler): a file that an applied patch changed is kept as it is, not fetched again.
    """
    patched = patched_paths(applied_patches(tree))
    for sha, path in upstream_files():
        target = tree / path
        if target.is_file() and (path in patched or git_blob_sha1(target.read_bytes()) == sha):
            continue
        wkpins.log(f"fetching {path}")
        try:
            with urllib.request.urlopen(RAW + path, timeout=60) as response:
                data = response.read()
        except OSError as error:
            raise BuildError(f"download of {path} failed: {error}") from error
        if git_blob_sha1(data) != sha:
            raise BuildError(f"{path}: git blob digest {git_blob_sha1(data)} is not the pinned {sha}")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)


def lay_overlay(tree: Path) -> int:
    """Copy `port/tree` over `tree`, only what differs; the number of files written."""
    written = 0
    for source in overlay_files():
        target = tree / source.relative_to(OVERLAY)
        data = source.read_bytes()
        if target.is_file() and target.read_bytes() == data:
            continue
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        written += 1
    return written


def apply_patches(tree: Path) -> None:
    done = tree / ".lazyos-patched"
    applied = done.read_text().split() if done.is_file() else []
    for patch in patch_files():
        if patch.name in applied:
            continue
        run(["patch", "-p1", "--forward", "-i", str(patch)], tree, f"patching port {patch.name}")
        applied.append(patch.name)
        done.write_text("\n".join(applied) + "\n")


def prepare() -> Path:
    """The tree, made current; its path."""
    archive = wkpins.fetch_webkit()
    if archive is None:
        raise BuildError("WebKitGTK download unavailable")
    wanted = patch_stamp()
    stamp = TREE / wkpins.jsc_pins.WEBKIT.topdir / STAMP
    if stamp.is_file():
        have = stamp.read_text().split()
        # New patches at the end only need applying; a changed or removed one needs a fresh
        # tree (and a full rebuild: the extraction renews every file's time).
        if wanted[:len(have)] != have:
            wkpins.log("an applied patch changed: extracting the tree again")
            shutil.rmtree(TREE / wkpins.jsc_pins.WEBKIT.topdir, ignore_errors=True)
    tree = toolenv.extract(archive, wkpins.jsc_pins.WEBKIT, TREE)
    fetch_upstream(tree)
    apply_patches(tree)
    written = lay_overlay(tree)
    if written:
        wkpins.log(f"{written} port file(s) written into the tree")
    stamp.write_text("\n".join(wanted) + "\n")
    return tree
