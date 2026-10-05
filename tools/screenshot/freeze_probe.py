"""Tell a frozen guest from one stalled at an I/O port (issue #449).

``monkey.py`` declares a freeze when the display stops changing after mouse
input. A guest caught in ring 0 at a port instruction (an ATA status poll
waiting on a busy drive, each read a VM exit under WHPX) may be in a long but
finite stall rather than a hang. Before calling it a freeze, the monkey asks
the monitor where the CPU is; if it is at port I/O in the kernel, it gives
the guest a grace period and probes again.

Pure helpers here (parsing ``info registers`` and ``x /Nxb`` output, decoding
the opcode) are tested without QEMU: ``python tools/screenshot/test_freeze_probe.py``.
"""

from __future__ import annotations

import re

# in/out with an immediate port or DX, and the string forms ins/outs.
IO_OPCODES = {0xE4, 0xE5, 0xE6, 0xE7, 0xEC, 0xED, 0xEE, 0xEF, 0x6C, 0x6D, 0x6E, 0x6F}
# One-byte forms the CPU may already have retired when the exit is reported,
# leaving RIP just past them.
ONE_BYTE_IO = {0xEC, 0xED, 0xEE, 0xEF, 0x6C, 0x6D, 0x6E, 0x6F}
# Legacy prefixes that may precede them (operand size, rep, segments).
PREFIXES = {0x66, 0x67, 0xF2, 0xF3, 0x26, 0x2E, 0x36, 0x3E, 0x64, 0x65}

_RIP = re.compile(r"\bRIP=([0-9a-fA-F]+)")
_CPL = re.compile(r"\bCPL=(\d)")
_RDX = re.compile(r"\bRDX=([0-9a-fA-F]+)")
_BYTE = re.compile(r"\b0x([0-9a-fA-F]{2})\b")


def parse_registers(text: str) -> dict:
    """``rip``, ``cpl`` and ``rdx`` from monitor ``info registers`` text
    (each ``None`` when absent)."""
    def field(pattern: re.Pattern, base: int):
        match = pattern.search(text)
        return int(match.group(1), base) if match else None

    return {"rip": field(_RIP, 16), "cpl": field(_CPL, 10), "rdx": field(_RDX, 16)}


def parse_bytes(text: str) -> bytes:
    """The bytes of a monitor ``x /Nxb ADDR`` dump (address columns skipped)."""
    out = bytearray()
    for line in text.splitlines():
        _, _, data = line.partition(":")
        out.extend(int(token, 16) for token in _BYTE.findall(data))
    return bytes(out)


def at_port_io(code: bytes, before: int | None = None) -> bool:
    """Whether ``code`` (the bytes at RIP) starts with a port instruction,
    or ``before`` (the byte at RIP-1) is a one-byte port instruction."""
    index = 0
    while index < len(code) and index < 4 and code[index] in PREFIXES:
        index += 1
    if index < len(code) and code[index] in IO_OPCODES:
        return True
    return before is not None and before in ONE_BYTE_IO


def port_io_stall(monitor) -> str | None:
    """Ask the monitor (``monitor(command) -> text``) where the CPU is. A
    description such as ``ring 0 at port I/O, RIP=... RDX=0x1f7`` when the
    guest is in the kernel at a port instruction, else ``None``."""
    try:
        regs = parse_registers(monitor("info registers"))
        if regs["rip"] is None or regs["cpl"] != 0:
            return None
        dump = parse_bytes(monitor(f"x /9xb {regs['rip'] - 1:#x}"))
    except Exception:
        return None
    if len(dump) < 2 or not at_port_io(dump[1:], dump[0]):
        return None
    port = f" RDX={regs['rdx'] & 0xFFFF:#x}" if regs["rdx"] is not None else ""
    return f"ring 0 at port I/O, RIP={regs['rip']:#x}{port}"
