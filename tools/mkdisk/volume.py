"""Create, reset and inspect the persistent data volume on the host.

The launcher scripts (``run_demo.py``, the screenshot tools, the GUI) go
through here so they agree on the default location and size, and so nothing
overwrites an existing volume unless the caller says so explicitly.
"""

from __future__ import annotations

import os
import re
from dataclasses import dataclass
from pathlib import Path

from . import ext2
from . import layout as layouts
from .geometry import DEFAULT_BLOCK_SIZE, plan

ROOT = Path(__file__).resolve().parent.parent.parent
DEFAULT_PATH = ROOT / "target" / "data.img"
DEFAULT_SIZE = 64 * 1024 * 1024
DEFAULT_LABEL = "lazyos-data"

_UNITS = {"": 1, "K": 1 << 10, "M": 1 << 20, "G": 1 << 30}


def parse_size(text: str) -> int:
    """Parse ``64M`` / ``512K`` / ``1G`` / plain bytes into a byte count."""
    match = re.fullmatch(r"\s*(\d+)\s*([KMG]?)(?:I?B)?\s*", text.upper())
    if not match:
        raise ValueError(f"cannot parse size {text!r} (try 64M, 512K, 1G)")
    return int(match.group(1)) * _UNITS[match.group(2)]


def format_size(size: int) -> str:
    """A short human size such as ``64 MiB``."""
    for unit, name in ((1 << 30, "GiB"), (1 << 20, "MiB"), (1 << 10, "KiB")):
        if size >= unit and size % unit == 0:
            return f"{size // unit} {name}"
    return f"{size} bytes"


def format_image(path: Path, size: int = DEFAULT_SIZE, label: str = DEFAULT_LABEL,
                 block_size: int = DEFAULT_BLOCK_SIZE,
                 layout: layouts.Layout | None = None) -> int:
    """Write a fresh ext2 volume to ``path`` (replacing any file there).

    ``layout`` says which directories exist and who owns them; the default is
    the seeded demo layout (:func:`mkdisk.layout.seeded`), pass
    :data:`mkdisk.layout.EMPTY` for a bare volume.

    The file is built beside the target and renamed into place, so a failure
    (or a QEMU still holding the old file open) never leaves a half-written
    volume behind. Returns the volume size in bytes.
    """
    geometry = plan(size, block_size)
    seed = layouts.seeded() if layout is None else layout
    extents = ext2.build_extents(geometry, label, layout=seed)
    length = geometry.blocks_count * geometry.block_size
    path.parent.mkdir(parents=True, exist_ok=True)
    partial = path.with_name(path.name + ".partial")
    try:
        with open(partial, "wb") as out:
            out.truncate(length)  # zero-filled: unwritten blocks are already free
            for offset, data in extents:
                out.seek(offset)
                out.write(data)
        os.replace(partial, path)
    finally:
        partial.unlink(missing_ok=True)
    return length


def ensure_volume(path: Path, size: int = DEFAULT_SIZE, label: str = DEFAULT_LABEL) -> bool:
    """Create the volume if it is missing; never touch an existing one.

    Returns ``True`` when a new volume was written.
    """
    if path.exists():
        return False
    format_image(path, size, label)
    return True


@dataclass(frozen=True)
class VolumeStatus:
    """What the launcher shows about the data volume."""

    path: Path
    exists: bool
    size: int

    def describe(self) -> str:
        """A one-line summary for a label or a log."""
        if not self.exists:
            return f"{self.path} (not created yet)"
        return f"{self.path} ({format_size(self.size)})"


def status(path: Path) -> VolumeStatus:
    """Existence and size of the volume at ``path``."""
    if path.is_file():
        return VolumeStatus(path, True, path.stat().st_size)
    return VolumeStatus(path, False, 0)
