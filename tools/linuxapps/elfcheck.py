"""Check that a file is a static x86-64 Linux executable LazyOS can load.

Static means no ``PT_INTERP``: LazyOS has no dynamic loader, so a program
that names one cannot run. Both a classic static executable (``ET_EXEC``) and
a static-PIE (``ET_DYN`` without an interpreter) are accepted.
"""

from __future__ import annotations

import struct
from pathlib import Path

ET_EXEC = 2
ET_DYN = 3
EM_X86_64 = 62
PT_INTERP = 3
#: The ELF64 header's size; the program header table follows it elsewhere.
EHDR_SIZE = 64


def problem(data: bytes) -> str | None:
    """Why `data` is not a static ELF64 x86-64 executable, or None if it is."""
    if len(data) < EHDR_SIZE or data[:4] != b"\x7fELF":
        return "not an ELF file"
    if data[4] != 2:
        return "not ELF64"
    if data[5] != 1:
        return "not little-endian"
    e_type, e_machine = struct.unpack_from("<HH", data, 16)
    if e_machine != EM_X86_64:
        return f"machine {e_machine}, not x86-64"
    if e_type not in (ET_EXEC, ET_DYN):
        return f"type {e_type}, not an executable"
    (e_entry,) = struct.unpack_from("<Q", data, 24)
    (e_phoff,) = struct.unpack_from("<Q", data, 32)
    e_phentsize, e_phnum = struct.unpack_from("<HH", data, 54)
    if e_entry == 0:
        return "no entry point"
    if e_phnum == 0 or e_phentsize < 56:
        return "no program headers"
    end = e_phoff + e_phnum * e_phentsize
    if end > len(data):
        return "program headers run past the end of the file"
    for index in range(e_phnum):
        (p_type,) = struct.unpack_from("<I", data, e_phoff + index * e_phentsize)
        if p_type == PT_INTERP:
            return "dynamically linked (has PT_INTERP)"
    return None


def check(path: Path) -> str | None:
    """`problem` for the file at `path` (a missing file is a problem too)."""
    if not path.is_file():
        return "missing"
    return problem(path.read_bytes())
