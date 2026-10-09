#!/usr/bin/env python3
"""Diagnosis tools for the RTL8111H driver (docs/rtl8168-driver-plan.md R0/R3/R4).

No datasheet is public and the box is the only test bench, so these answer the
plan's "to confirm" items from the machine itself:

  survey   (on the box's Linux, as root) collect everything the plan lists:
           `lspci`, `ethtool -i/-d/-S/--show-eee/-a`, `dmesg` for r8169 and its
           firmware, the BAR 2 registers read straight from the chip, and the
           PHY's MII registers; write them to a directory (default
           docs/compat/kabylake/) with a report and the plan-assumption checks.
  decode   decode a dump: ethtool output, a LazyOS serial log, or the BAR.
  diff     what differs between Linux's registers and LazyOS's (the cold-start
           comparison of plan section 5), volatile registers left out.

Examples:
  sudo tools/net/rtl8168/rtl8168_probe.py survey --iface enp2s0 --bdf 0000:02:00.0
  tools/net/rtl8168/rtl8168_probe.py decode --ethtool docs/compat/kabylake/ethtool-d.txt
  tools/net/rtl8168/rtl8168_probe.py diff --ethtool ethtool-d.txt --serial serial.log
"""

from __future__ import annotations

import argparse
import shutil
import struct
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import regmap  # noqa: E402

SIOCGMIIPHY = 0x8947
SIOCGMIIREG = 0x8948


def read_mii(iface: str) -> dict[int, int]:
    """The PHY's page-0 registers through the kernel's MII ioctls."""
    import fcntl
    import socket

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    name = iface.encode()[:15].ljust(16, b"\0")
    request = bytearray(name + struct.pack("HHHH", 0, 0, 0, 0) + bytes(8))
    fcntl.ioctl(sock, SIOCGMIIPHY, request)
    phy = struct.unpack_from("H", request, 16)[0]
    regs = {}
    for reg in range(32):
        request = bytearray(name + struct.pack("HHHH", phy, reg, 0, 0) + bytes(8))
        fcntl.ioctl(sock, SIOCGMIIREG, request)
        regs[reg] = struct.unpack_from("H", request, 22)[0]
    return regs


MII_NAMES = {0: "BMCR", 1: "BMSR", 2: "PHYID1", 3: "PHYID2", 4: "ANAR", 5: "ANLPAR",
             6: "ANER", 9: "GBCR", 10: "GBSR", 15: "ESTATUS", 31: "PAGE"}


def mii_report(regs: dict[int, int]) -> str:
    lines = ["== PHY (MII page 0 as Linux left it) =="]
    for reg, value in regs.items():
        lines.append(f"{reg:2d} {MII_NAMES.get(reg, ''):<8} {value:#06x}")
    lines.append(f"PHY id {regs[2] << 16 | regs[3]:#010x}")
    return "\n".join(lines)


def run(command: list[str]) -> str:
    if shutil.which(command[0]) is None:
        return f"(skipped: {command[0]} not installed)\n"
    done = subprocess.run(command, capture_output=True, text=True)
    return done.stdout + done.stderr


def survey(args: argparse.Namespace) -> int:
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    files = {
        "lspci.txt": ["lspci", "-nnvvv", "-s", args.bdf.split(":", 1)[1]],
        "ethtool-i.txt": ["ethtool", "-i", args.iface],
        "ethtool-d.txt": ["ethtool", "-d", args.iface],
        "ethtool-S.txt": ["ethtool", "-S", args.iface],
        "ethtool-eee.txt": ["ethtool", "--show-eee", args.iface],
        "ethtool-a.txt": ["ethtool", "-a", args.iface],
        "ethtool-k.txt": ["ethtool", "-k", args.iface],
        "ethtool-link.txt": ["ethtool", args.iface],
    }
    for name, command in files.items():
        (out / name).write_text(run(command))
    dmesg = run(["dmesg"])
    keep = ("r8169", "rtl_nic", "rtl8168", "realtek", "Generic FE-GE", args.iface)
    (out / "dmesg-r8169.txt").write_text(
        "\n".join(line for line in dmesg.splitlines() if any(k.lower() in line.lower() for k in keep)) + "\n"
    )
    parts = []
    dumps = {}
    try:
        dumps["bar2 (sysfs)"] = regmap.read_sysfs(args.bdf)
        (out / "bar2.bin").write_bytes(dumps["bar2 (sysfs)"])
    except OSError as error:
        parts.append(f"BAR 2 not readable: {error} (run as root)")
    try:
        dumps["ethtool -d"] = regmap.parse_ethtool((out / "ethtool-d.txt").read_text())
    except ValueError as error:
        parts.append(f"ethtool -d: {error}")
    for title, dump in dumps.items():
        parts.append(regmap.report(dump, title))
    if len(dumps) == 2:
        same = regmap.diff(*dumps.values(), "bar2", "ethtool")
        parts.append("-- bar2 vs ethtool -d --\n" + ("\n".join(same) or "identical (non-volatile)"))
    try:
        parts.append(mii_report(read_mii(args.iface)))
    except OSError as error:
        parts.append(f"MII registers not readable: {error}")
    parts.append(
        "== questions this survey answers (docs/kabylake-box-plan.md section 4 question 3) ==\n"
        "ethtool -i firmware-version: see ethtool-i.txt\n"
        "PHY firmware patch: see dmesg-r8169.txt (an `rtl_nic/rtl8168h-*.fw` line means Linux loads one);\n"
        "run again with the cable plugged in, the patch loads at link-up."
    )
    report = "\n\n".join(parts) + "\n"
    (out / "report.txt").write_text(report)
    print(report)
    print(f"saved to {out}/", file=sys.stderr)
    return 0


def load(args: argparse.Namespace) -> dict[str, bytes]:
    dumps = {}
    if getattr(args, "ethtool", None):
        dumps["linux"] = regmap.parse_ethtool(Path(args.ethtool).read_text())
    if getattr(args, "bdf", None):
        dumps["linux"] = regmap.read_sysfs(args.bdf)
    if getattr(args, "serial", None):
        dumps.update({f"lazyos:{k}": v for k, v in regmap.parse_serial(Path(args.serial).read_text()).items()})
    return dumps


def decode_cmd(args: argparse.Namespace) -> int:
    dumps = load(args)
    if not dumps:
        print("no dump found", file=sys.stderr)
        return 1
    for title, dump in dumps.items():
        print(regmap.report(dump, title))
        print()
    return 0


def diff_cmd(args: argparse.Namespace) -> int:
    dumps = load(args)
    linux = dumps.pop("linux", None)
    if linux is None or not dumps:
        print("need a Linux dump (--ethtool or --bdf) and LazyOS dumps (--serial)", file=sys.stderr)
        return 1
    for title, dump in dumps.items():
        print(f"== linux vs {title} ==")
        print("\n".join(regmap.diff(linux, dump, "linux", "lazyos")) or "identical (non-volatile)")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    s = sub.add_parser("survey")
    s.add_argument("--iface", default="enp2s0")
    s.add_argument("--bdf", default="0000:02:00.0")
    s.add_argument("--out", default="docs/compat/kabylake")
    s.set_defaults(func=survey)
    for name, func in (("decode", decode_cmd), ("diff", diff_cmd)):
        p = sub.add_parser(name)
        p.add_argument("--ethtool")
        p.add_argument("--bdf")
        p.add_argument("--serial")
        p.set_defaults(func=func)
    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
