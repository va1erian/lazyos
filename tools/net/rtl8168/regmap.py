"""The RTL8168 register map as the LazyOS driver (`libs/rtl8168`) uses it, and
the parsers and decoders for register dumps.

Three sources give the same 256 bytes of register space, and this module reads
all of them:

* Linux's `ethtool -d <iface>` (the r8169 driver dumps the first 256 bytes);
* the BAR itself, mapped read-only through sysfs (`resource2`), which works
  while `r8169` is bound;
* LazyOS's own `NETDRV:REGS <label> <offset>: <32 hex bytes>` serial lines.

Offsets and bit meanings repeat the driver's `regs.rs`; the point of the tools
is to check them against the real chip (docs/rtl8168-driver-plan.md R0) and to
compare what Linux left in the registers with what LazyOS programs.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Callable

DUMP_BYTES = 256
EXPECTED_XID = 0x541


@dataclass(frozen=True)
class Reg:
    name: str
    offset: int
    width: int  # bytes
    # Volatile registers differ between two dumps for no reason worth showing.
    volatile: bool = False
    decode: Callable[[int], str] | None = None


def _bits(value: int, names: dict[int, str]) -> str:
    return " ".join(name for bit, name in sorted(names.items()) if value >> bit & 1) or "-"


def xid(tx_config: int) -> int:
    """The revision field: bits 20..23 and 26..31 of TxConfig, gathered."""
    return (tx_config >> 20) & 0xFCF


def _tx_config(value: int) -> str:
    return f"xid={xid(value):#05x} burst={(value >> 8) & 7} ifg={(value >> 24) & 3}"


def _rx_config(value: int) -> str:
    flags = _bits(
        value,
        {0: "all-phys", 1: "my-phys", 2: "multicast", 3: "broadcast", 4: "runt", 5: "err"},
    )
    return f"{flags} burst={(value >> 8) & 7} fifo-thresh={(value >> 13) & 7}"


def _chip_cmd(value: int) -> str:
    return _bits(value, {4: "RESET", 3: "RX-EN", 2: "TX-EN", 0: "RX-BUF-EMPTY"})


def _intr(value: int) -> str:
    return _bits(
        value,
        {0: "rx-ok", 1: "rx-err", 2: "tx-ok", 3: "tx-err", 4: "rx-unavail", 5: "link-chg",
         6: "rx-fifo-over", 7: "tx-unavail", 8: "sw-int", 14: "pcs-timeout", 15: "sys-err"},
    )


def _phy_status(value: int) -> str:
    speed = [name for bit, name in ((4, "1000"), (3, "100"), (2, "10")) if value >> bit & 1]
    return (
        f"link={'up' if value & 2 else 'down'} speed={'/'.join(speed) or '-'} "
        f"{'full' if value & 1 else 'half'} flow={_bits(value, {5: 'rx', 6: 'tx'})}"
    )


def _cplus(value: int) -> str:
    return _bits(value, {3: "PCI-MUL-RW", 4: "PCI-DAC", 5: "RX-CHECKSUM", 6: "RX-VLAN"})


def _mac(value: int) -> str:
    return ":".join(f"{b:02x}" for b in value.to_bytes(6, "little"))


REGS: list[Reg] = [
    Reg("IDR0-5", 0x00, 6, decode=_mac),
    Reg("MAR0-7", 0x08, 8),
    Reg("DTCCR (tally)", 0x10, 8, volatile=True),
    Reg("TNPDS", 0x20, 8, volatile=True),
    Reg("THPDS", 0x28, 8, volatile=True),
    Reg("ChipCmd", 0x37, 1, decode=_chip_cmd),
    Reg("TxPoll", 0x38, 1, volatile=True),
    Reg("IntrMask", 0x3C, 2, decode=_intr),
    Reg("IntrStatus", 0x3E, 2, volatile=True, decode=_intr),
    Reg("TxConfig", 0x40, 4, decode=_tx_config),
    Reg("RxConfig", 0x44, 4, decode=_rx_config),
    Reg("TCTR (timer)", 0x48, 4, volatile=True),
    Reg("MPC (missed)", 0x4C, 4, volatile=True),
    Reg("Cfg9346", 0x50, 1),
    Reg("Config1", 0x52, 1),
    Reg("Config2", 0x53, 1),
    Reg("Config3", 0x54, 1),
    Reg("Config4", 0x55, 1),
    Reg("Config5", 0x56, 1),
    Reg("PHYAR", 0x60, 4, volatile=True),
    Reg("PHYstatus", 0x6C, 1, decode=_phy_status),
    Reg("RxMaxSize", 0xDA, 2),
    Reg("CPlusCmd", 0xE0, 2, decode=_cplus),
    Reg("RDSAR", 0xE4, 8, volatile=True),
    Reg("MaxTxPacketSize", 0xEC, 1),
]


def value_of(dump: bytes, reg: Reg) -> int:
    return int.from_bytes(dump[reg.offset : reg.offset + reg.width], "little")


_ETHTOOL_LINE = re.compile(r"^\s*(0x[0-9a-fA-F]+)\s*:?\s+((?:[0-9a-fA-F]{2}\s*)+)$")
_SERIAL_LINE = re.compile(r"NETDRV:REGS\s+(\S+)\s+([0-9a-fA-F]{2}):((?:\s+[0-9a-fA-F]{2})+)")


def parse_ethtool(text: str) -> bytes:
    """`ethtool -d` output: `0x0000:  52 54 ...` lines (any bytes per line)."""
    out = bytearray(DUMP_BYTES)
    seen = 0
    for line in text.splitlines():
        match = _ETHTOOL_LINE.match(line)
        if not match:
            continue
        offset = int(match.group(1), 16)
        data = bytes.fromhex("".join(match.group(2).split()))
        for index, byte in enumerate(data):
            if offset + index < DUMP_BYTES:
                out[offset + index] = byte
                seen += 1
    if seen < DUMP_BYTES:
        raise ValueError(f"only {seen} of {DUMP_BYTES} register bytes found")
    return bytes(out)


def parse_serial(text: str) -> dict[str, bytes]:
    """LazyOS `NETDRV:REGS <label> <offset>: <hex bytes>` lines, by label."""
    dumps: dict[str, bytearray] = {}
    seen: dict[str, int] = {}
    for match in _SERIAL_LINE.finditer(text):
        label, offset = match.group(1), int(match.group(2), 16)
        data = bytes.fromhex("".join(match.group(3).split()))
        buf = dumps.setdefault(label, bytearray(DUMP_BYTES))
        buf[offset : offset + len(data)] = data[: DUMP_BYTES - offset]
        seen[label] = seen.get(label, 0) + len(data)
    # A label seen again later (a second link change) overwrites the first,
    # which is the state the chip is in last.
    return {label: bytes(buf) for label, buf in dumps.items() if seen[label] >= DUMP_BYTES}


def read_sysfs(bdf: str) -> bytes:
    """The first 256 bytes of BAR 2, read-only (needs root)."""
    with open(f"/sys/bus/pci/devices/{bdf}/resource2", "rb") as bar:
        return bar.read(DUMP_BYTES)


def decode(dump: bytes) -> list[str]:
    lines = []
    for reg in REGS:
        value = value_of(dump, reg)
        extra = f"  {reg.decode(value)}" if reg.decode else ""
        digits = reg.width * 2
        lines.append(f"{reg.offset:#04x} {reg.name:<16} {value:#0{digits + 2}x}{extra}")
    return lines


def diff(a: bytes, b: bytes, label_a: str = "A", label_b: str = "B") -> list[str]:
    """The registers that differ between two dumps, volatile ones left out."""
    out = []
    for reg in REGS:
        if reg.volatile:
            continue
        va, vb = value_of(a, reg), value_of(b, reg)
        if va != vb:
            digits = reg.width * 2 + 2
            extra = ""
            if reg.decode:
                extra = f"\n      {label_a}: {reg.decode(va)}\n      {label_b}: {reg.decode(vb)}"
            out.append(
                f"{reg.offset:#04x} {reg.name:<16} {label_a}={va:#0{digits}x} "
                f"{label_b}={vb:#0{digits}x}{extra}"
            )
    covered = [False] * DUMP_BYTES
    for reg in REGS:
        for index in range(reg.offset, reg.offset + reg.width):
            covered[index] = True
    unnamed = [i for i in range(DUMP_BYTES) if not covered[i] and a[i] != b[i]]
    if unnamed:
        out.append("unnamed bytes differ at " + ", ".join(f"{i:#04x}" for i in unnamed))
    return out


def check(dump: bytes) -> list[tuple[bool, str]]:
    """What the plan assumed, tested against a dump of the real chip."""
    get = {reg.name: value_of(dump, reg) for reg in REGS}
    tx = get["TxConfig"]
    mac = dump[0:6]
    results = [
        (xid(tx) == EXPECTED_XID, f"XID is {xid(tx):#05x} (the driver drives only {EXPECTED_XID:#05x})"),
        (tx != 0xFFFFFFFF, "TxConfig does not read all ones (the function answers)"),
        (any(mac) and not mac[0] & 1, f"IDR holds a unicast station address ({_mac(get['IDR0-5'])})"),
        (get["PHYstatus"] != 0xFF, "PHYstatus does not read all ones"),
    ]
    return results


def report(dump: bytes, title: str) -> str:
    lines = [f"== {title} =="]
    lines += decode(dump)
    lines.append("-- plan assumptions --")
    lines += [("ok   " if ok else "FAIL ") + text for ok, text in check(dump)]
    return "\n".join(lines)
