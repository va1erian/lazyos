#!/usr/bin/env python3
"""Fetch and build the pinned static-musl BusyBox the image ships as its shell.

BusyBox is the system shell (issue #254), so it is a *build artifact*, never a
committed blob. `ensure_busybox()` returns a path to a static
`x86_64-unknown-linux-musl` binary, preferring, in order:

1. `tools/abi/busybox` — a binary dropped by hand (or by CI) for an offline run;
2. `target/abi/busybox/busybox` — a cached build from a previous run;
3. a fresh build from the pinned source tarball, when the host is Linux with
   `musl-gcc` (and the Debian/Ubuntu `linux-libc-dev` headers) available.

The build is `defconfig` + `CONFIG_STATIC=y`, with the `tc` applet disabled (its
kernel headers conflict on modern distros). If any step is unavailable the
function returns `None` and the bench marks BusyBox `unavailable`; it never
raises, so a Windows or minimal host still builds the OS, which then boots
without a console shell (`build.rs` logs a warning).
"""

from __future__ import annotations

import hashlib
import os
import shutil
import subprocess
import sys
import tarfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
VERSION = "1.36.1"
URL = f"https://busybox.net/downloads/busybox-{VERSION}.tar.bz2"
# Pinned digest of the release tarball: the build runs `make` on this source,
# so the bytes must match the release, not merely come over HTTPS.
SHA256 = "b8cc24c9574d809e7279c3be349795c5d5ceb6fdf19ca709f80cde50e47de314"
BUILD_ROOT = ROOT / "target" / "abi" / "busybox"
SOURCE = BUILD_ROOT / f"busybox-{VERSION}"
OUTPUT = BUILD_ROOT / "busybox"
MANUAL = ROOT / "tools" / "abi" / "busybox"

# The host's kernel headers are not on musl's include path. These two Debian
# locations supply `linux/*.h` and the `asm/*.h` it includes; a host without
# them simply fails the build and reports BusyBox unavailable.
EXTRA_CFLAGS = "-idirafter /usr/include -idirafter /usr/include/x86_64-linux-gnu"


def _cached() -> Path | None:
    for candidate in (MANUAL, OUTPUT):
        if candidate.is_file():
            return candidate
    return None


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def _download(archive: Path) -> bool:
    """Fetch the tarball to a temporary name and move it into place only once
    it is complete and matches the pinned digest, so an interrupted or tampered
    download can never be mistaken for a cached archive."""
    partial = archive.with_suffix(archive.suffix + ".part")
    print(f"busybox: downloading {URL}", file=sys.stderr)
    try:
        urllib.request.urlretrieve(URL, partial)
        if _sha256(partial) != SHA256:
            raise ValueError("SHA-256 does not match the pinned release digest")
        partial.replace(archive)
    except Exception as error:  # noqa: BLE001 - any failure is "unavailable"
        partial.unlink(missing_ok=True)
        print(f"busybox: download failed: {error}", file=sys.stderr)
        return False
    return True


def _fetch() -> bool:
    if SOURCE.is_dir():
        return True
    BUILD_ROOT.mkdir(parents=True, exist_ok=True)
    archive = BUILD_ROOT / f"busybox-{VERSION}.tar.bz2"
    # A cached archive is re-verified too: it may predate the pin or be corrupt.
    if archive.is_file() and _sha256(archive) != SHA256:
        print("busybox: cached archive fails the digest check; discarding", file=sys.stderr)
        archive.unlink()
    if not archive.is_file() and not _download(archive):
        return False
    try:
        with tarfile.open(archive, "r:bz2") as tar:
            _safe_extract(tar, BUILD_ROOT)
    except Exception as error:  # noqa: BLE001 - a bad archive is "unavailable"
        print(f"busybox: extract failed: {error}", file=sys.stderr)
        # Never leave a half-extracted tree or a bad archive to poison a retry.
        shutil.rmtree(SOURCE, ignore_errors=True)
        archive.unlink(missing_ok=True)
        return False
    return SOURCE.is_dir()


def _safe_extract(tar: tarfile.TarFile, dest: Path) -> None:
    """Extract a remote tarball without path traversal or link tricks.

    Members must stay inside `dest`, and link members are refused outright on
    Pythons whose `tarfile` has no extraction filter (a later member could
    otherwise write through an earlier link). Where the `data` filter exists
    (3.12+, backported to security releases) it is applied as well.
    """
    root = dest.resolve()
    has_filter = hasattr(tarfile, "data_filter")
    for member in tar.getmembers():
        target = (root / member.name).resolve()
        if root not in target.parents and target != root:
            raise ValueError(f"refusing path outside the build dir: {member.name}")
        if not has_filter and (member.issym() or member.islnk()):
            raise ValueError(f"refusing link member without a tar filter: {member.name}")
    if has_filter:
        tar.extractall(dest, filter="data")
    else:
        tar.extractall(dest)


def _configure() -> bool:
    subprocess.run(["make", "defconfig"], cwd=SOURCE, capture_output=True, text=True, check=True)
    config = SOURCE / ".config"
    lines = config.read_text(encoding="utf-8").splitlines()
    patched = []
    for line in lines:
        if line == "# CONFIG_STATIC is not set":
            patched.append("CONFIG_STATIC=y")
        elif line == "CONFIG_TC=y":
            # The `tc` applet needs `struct tc_cbq_*`, dropped from modern
            # kernel headers; it is irrelevant to a shell image.
            patched.append("# CONFIG_TC is not set")
        else:
            patched.append(line)
    config.write_text("\n".join(patched) + "\n", encoding="utf-8")
    return True


def _compile() -> bool:
    jobs = str(max(1, (os.cpu_count() or 1)))
    cmd = [
        "make",
        f"-j{jobs}",
        "CC=musl-gcc",
        "HOSTCC=gcc",
        f"EXTRA_CFLAGS={EXTRA_CFLAGS}",
    ]
    result = subprocess.run(cmd, cwd=SOURCE, capture_output=True, text=True)
    if result.returncode != 0:
        print("busybox: build failed", file=sys.stderr)
        print(result.stderr[-2000:], file=sys.stderr)
        return False
    return (SOURCE / "busybox").is_file()


def ensure_busybox() -> Path | None:
    """Return a path to a static BusyBox, building it if the host allows."""
    cached = _cached()
    if cached is not None:
        return cached
    if sys.platform != "linux":
        return None
    if shutil.which("musl-gcc") is None:
        print("busybox: musl-gcc not found; reporting unavailable", file=sys.stderr)
        return None
    try:
        if not _fetch() or not _configure() or not _compile():
            return None
    except Exception as error:  # noqa: BLE001 - any tool failure is "unavailable"
        print(f"busybox: build unavailable: {error}", file=sys.stderr)
        return None
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(SOURCE / "busybox", OUTPUT)
    print(f"busybox: built {OUTPUT}", file=sys.stderr)
    return OUTPUT


if __name__ == "__main__":
    path = ensure_busybox()
    if path is None:
        print("unavailable")
        raise SystemExit(1)
    print(path)
