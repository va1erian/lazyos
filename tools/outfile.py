"""Write a build output only when it changed.

A build step that rewrites an output it already made, byte for byte, still
changes its modification time, and every step after it that watches the file
(cargo's `rerun-if-changed`, `make`, the image build) then runs again. A
launcher rebuild with nothing changed spent most of its time that way: the
apps were copied again, packaged again and the OS image rewritten, to end with
the file it started with.

    from outfile import copy_if_changed, write_if_changed

Both return whether the destination was written.

`copy_file` and `copy_tree` are `shutil.copyfile` and `shutil.copytree` for
small files: on Windows those go through `CopyFile2`, which took 60 ms per
file on the launcher's host (a virus scanner hooks it), a hundred times what
reading and writing the same bytes takes, and a package build copies a few
dozen files.
"""

from __future__ import annotations

import os
import shutil
from pathlib import Path

#: What is compared at a time (a 108 MiB program is not read into memory twice).
CHUNK = 1 << 20


def same_content(first: Path, second: Path) -> bool:
    """Whether two files have the same bytes (the sizes are compared first)."""
    try:
        if first.stat().st_size != second.stat().st_size:
            return False
        with first.open("rb") as left, second.open("rb") as right:
            while True:
                a, b = left.read(CHUNK), right.read(CHUNK)
                if a != b:
                    return False
                if not a:
                    return True
    except OSError:
        return False


def copy_file(source: Path, dest: Path) -> None:
    """Copy the bytes of `source` to `dest` (its folder is made when missing)."""
    dest = Path(dest)
    dest.parent.mkdir(parents=True, exist_ok=True)
    with Path(source).open("rb") as reader, dest.open("wb") as writer:
        shutil.copyfileobj(reader, writer, CHUNK)


def copy_tree(source: Path, dest: Path) -> None:
    """Copy the folder `source` to `dest`, which must not exist yet (the
    `shutil.copytree` rule), file by file with [`copy_file`]."""
    source, dest = Path(source), Path(dest)
    dest.mkdir(parents=True)
    for path in sorted(source.rglob("*")):
        target = dest / path.relative_to(source)
        if path.is_dir():
            target.mkdir(parents=True, exist_ok=True)
        else:
            copy_file(path, target)


def copy_if_changed(source: Path, dest: Path) -> bool:
    """Copy `source` to `dest` unless `dest` already holds the same bytes."""
    source, dest = Path(source), Path(dest)
    if dest.is_file() and same_content(source, dest):
        return False
    copy_file(source, dest)
    return True


def write_if_changed(dest: Path, data: bytes) -> bool:
    """Write `data` to `dest` unless `dest` already holds exactly it."""
    dest = Path(dest)
    try:
        if dest.stat().st_size == len(data) and dest.read_bytes() == data:
            return False
    except OSError:
        pass
    dest.parent.mkdir(parents=True, exist_ok=True)
    # Beside it and renamed, so a reader never sees half a file.
    temp = dest.with_name(dest.name + f".{os.getpid()}.tmp")
    temp.write_bytes(data)
    os.replace(temp, dest)
    return True
