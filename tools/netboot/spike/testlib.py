"""Fixtures shared by the spike's tests (not a test module itself)."""

from __future__ import annotations

import struct
from pathlib import Path

import common
import fat16_format


def fake_efi(size: int = 600) -> bytes:
    """The smallest thing preflight's PE check accepts: PE32+, x86-64, subsystem 10."""
    data = bytearray(size)
    data[0:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x40)
    data[0x40:0x44] = b"PE\0\0"
    struct.pack_into("<H", data, 0x44, 0x8664)
    struct.pack_into("<H", data, 0x40 + 24, 0x20B)
    struct.pack_into("<H", data, 0x40 + 24 + 68, 10)
    return bytes(data)


def pattern(n: int, seed: int) -> bytes:
    """`n` deterministic, non-repeating-looking bytes."""
    out = bytearray(n)
    state = seed
    for i in range(n):
        state = (state * 1103515245 + 12345) & 0x7FFFFFFF
        out[i] = (state >> 16) & 0xFF
    return bytes(out)


def make_payload(directory: Path, ramdisk_bytes: int = 200_000) -> dict[str, bytes]:
    """The four payload files, as `extract_payload.py` would leave them."""
    contents = {
        "kernel-x86_64": pattern(70_000, 1),
        "boot.json": b'{"log_level": "Trace"}\n',
        "ramdisk": pattern(ramdisk_bytes, 2),
        common.LOADER_NAME: fake_efi(),
    }
    directory.mkdir(parents=True, exist_ok=True)
    for name, data in contents.items():
        (directory / name).write_bytes(data)
    return contents


def make_stick(path: Path, payload: Path) -> None:
    """A stand-in for the stick image: an MBR + FAT16 volume with the loader's layout."""
    files = {common.LOADER_FAT_PATH: payload / common.LOADER_NAME}
    files.update({name: payload / name for name in common.PAYLOAD_FILES})
    fat16_format.format_image(path, files, label="LAZYOS", part_type=0x0C)
