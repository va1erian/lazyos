"""The hardware-read checks of `tools/dbg/run.py` (docs/dbgd-plan.md,
"Hardware reads"): `pci.config` and `mmio.read` against QEMU's standard VGA
(`1234:1111`), whose registers are known:

* config dword 0 is its vendor and device id (little endian);
* BAR2 + 0x500 is the Bochs VBE id register, `0xB0C0..=0xB0C5`;
* BAR2 + 0x418 is an `0x3DA`-class port (input status 1: reading it
  resets the attribute controller's flip-flop), a hole in the allowlist;
* BAR0 is the framebuffer, in the built-in table.

`exercise` runs against an image built with `LAZYOS_DBGD_MMIO=1`;
`exercise_off` against one without it (config reads still work, MMIO is
refused with the key to set). Both take the client's `call`, so the
self-test (`test_hw_checks.py`) can feed them a fake and see them fail.
"""

from __future__ import annotations

import time
from typing import Callable

from dbgclient import DbgError

DENIED, UNAVAILABLE, BAD_PARAMS, RATE_LIMITED = -32002, -32003, -32602, -32005
VGA = ("1234", "1111")


def find_vga(call: Callable[..., dict]) -> dict | None:
    rows = call("devices.list")["devices"]
    return next((d for d in rows if (d.get("vendor"), d.get("device")) == VGA), None)


def _raises(c, name: str, code: int, fn, needle: str = "") -> None:
    try:
        fn()
    except DbgError as error:
        ok = error.code == code and needle in error.message
        c.check(name, ok, f"(code {error.code}: {error.message})")
    else:
        c.check(name, False, "(no error)")


def _dword(reply: dict) -> int:
    return int.from_bytes(bytes.fromhex(reply["hex"])[:4], "little")


def config_checks(call: Callable[..., dict], c, bdf: str) -> None:
    ident = call("pci.config", bdf=bdf, offset=0, len=4)
    c.check("pci.config dword 0 is the vendor and device id",
            ident["hex"] == "34121111", str(ident))
    c.check("pci.config echoes the address and ids",
            ident["bdf"] == bdf and ident["vendor"] == "1234" and ident["device"] == "1111")
    whole = call("pci.config", bdf=bdf, offset=0, len=256)
    c.check("pci.config reads all 256 bytes", whole["len"] == 256 and len(whole["hex"]) == 512)
    odd = call("pci.config", bdf=bdf, offset=3, len=5)
    c.check("pci.config takes an unaligned span", odd["hex"] == whole["hex"][6:16], str(odd))
    c.check("pci.config defaults to 64 bytes",
            call("pci.config", bdf=bdf)["len"] == 64)
    _raises(c, "pci.config refuses offset 256", BAD_PARAMS,
            lambda: call("pci.config", bdf=bdf, offset=256, len=1))
    _raises(c, "pci.config refuses a span past 256", BAD_PARAMS,
            lambda: call("pci.config", bdf=bdf, offset=200, len=100))
    _raises(c, "pci.config refuses a zero length", BAD_PARAMS,
            lambda: call("pci.config", bdf=bdf, offset=0, len=0))
    _raises(c, "pci.config refuses a malformed address", BAD_PARAMS,
            lambda: call("pci.config", bdf="zz:00.0"))
    _raises(c, "pci.config says no such function", UNAVAILABLE,
            lambda: call("pci.config", bdf="fe:1f.7"))
    _raises(c, "pci.config has no write parameter", BAD_PARAMS,
            lambda: call("pci.config", bdf=bdf, offset=4, len=2, value=0))


def mmio_checks(call: Callable[..., dict], c, vga: dict, others: list[dict]) -> None:
    bdf = vga["bdf"]
    first = call("mmio.read", bdf=bdf, bar=2, offset=0x500, len=4)
    ident = _dword(first)
    c.check("mmio.read of the VBE id register", 0xB0C0 <= ident & 0xFFFF <= 0xB0C5,
            f"({ident:#x})")
    c.check("mmio.read is stable for a plain register",
            call("mmio.read", bdf=bdf, bar=2, offset=0x500, len=4)["hex"] == first["hex"])
    fb = call("mmio.read", bdf=bdf, bar=0, offset=0, len=64)
    c.check("mmio.read of the framebuffer BAR", fb["len"] == 64 and len(fb["hex"]) == 128)
    edid = call("mmio.read", bdf=bdf, bar=2, offset=0, len=128)
    c.check("mmio.read across 128 bytes of the EDID window", edid["len"] == 128)
    _raises(c, "mmio.read refuses a register outside the allowlist", DENIED,
            lambda: call("mmio.read", bdf=bdf, bar=2, offset=0x418, len=4), "allowed for BAR2")
    _raises(c, "mmio.read refuses a span that crosses a hole", DENIED,
            lambda: call("mmio.read", bdf=bdf, bar=2, offset=0x3F0, len=0x20))
    _raises(c, "mmio.read refuses an unaligned offset", BAD_PARAMS,
            lambda: call("mmio.read", bdf=bdf, bar=2, offset=0x501, len=4))
    _raises(c, "mmio.read refuses an unaligned length", BAD_PARAMS,
            lambda: call("mmio.read", bdf=bdf, bar=2, offset=0x500, len=6))
    _raises(c, "mmio.read refuses more than 4096 bytes", BAD_PARAMS,
            lambda: call("mmio.read", bdf=bdf, bar=0, offset=0, len=4100))
    _raises(c, "mmio.read refuses BAR 6", BAD_PARAMS,
            lambda: call("mmio.read", bdf=bdf, bar=6, offset=0, len=4))
    _raises(c, "mmio.read has no write parameter", BAD_PARAMS,
            lambda: call("mmio.read", bdf=bdf, bar=2, offset=0x500, len=4, value=0))
    _raises(c, "mmio.read says no such function", UNAVAILABLE,
            lambda: call("mmio.read", bdf="fe:1f.7", bar=0, offset=0, len=4))
    for other in others[:3]:
        _raises(c, f"mmio.read refuses {other['vendor']}:{other['device']} (no range listed)",
                DENIED, lambda other=other: call("mmio.read", bdf=other["bdf"], bar=0,
                                                 offset=0, len=4), "none listed")
    rate_limit_checks(call, c, vga)


def rate_limit_checks(call: Callable[..., dict], c, vga: dict) -> None:
    """Six hundred KiB in a burst exceeds the 512 KiB bucket: some request is
    told to wait, and the same read works once the bucket has refilled."""
    limited = False
    for _ in range(160):
        try:
            call("mmio.read", bdf=vga["bdf"], bar=0, offset=0, len=4096)
        except DbgError as error:
            limited = error.code == RATE_LIMITED
            break
    c.check("a burst of hardware reads is rate limited", limited)
    time.sleep(2.0)
    try:
        ok = call("mmio.read", bdf=vga["bdf"], bar=0, offset=0, len=64)["len"] == 64
    except DbgError:
        ok = False
    c.check("hardware reads work again after the bucket refills", ok)


def exercise(c, call: Callable[..., dict]) -> None:
    """Image built with `LAZYOS_DBGD_MMIO=1`."""
    names = {m["name"] for m in call("methods")["methods"]}
    c.check("methods lists pci.config and mmio.read", {"pci.config", "mmio.read"} <= names)
    vga = find_vga(call)
    if not c.check("devices.list shows the standard VGA with its address",
                   vga is not None and "bdf" in vga, str(vga)):
        return
    config_checks(call, c, vga["bdf"])
    others = [d for d in call("devices.list")["devices"]
              if "bdf" in d and (d["vendor"], d["device"]) != VGA]
    mmio_checks(call, c, vga, others)


def exercise_off(c, call: Callable[..., dict]) -> None:
    """Image built without `LAZYOS_DBGD_MMIO`: config space only."""
    vga = find_vga(call)
    if not c.check("devices.list shows the standard VGA with its address",
                   vga is not None and "bdf" in vga, str(vga)):
        return
    config_checks(call, c, vga["bdf"])
    _raises(c, "mmio.read is refused unless the box turned it on", DENIED,
            lambda: call("mmio.read", bdf=vga["bdf"], bar=2, offset=0x500, len=4),
            "diag.dbg.mmio")
