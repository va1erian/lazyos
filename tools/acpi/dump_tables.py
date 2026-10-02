#!/usr/bin/env python3
"""Dump the ACPI tables QEMU's firmware builds into golden files for `libs/acpi`.

Boots QEMU with no disk under SeaBIOS or OVMF, waits for the firmware to
install its tables, saves guest RAM with QMP `pmemsave`, finds the RSDP
(checksummed `RSD PTR `) and copies every table it reaches (RSDT, XSDT, each
entry, and the FADT's DSDT and FACS) into one file:

    b"ACPIDUMP" | u64 rsdp_phys | { u64 phys | u32 len | bytes }*   (little endian)

`libs/acpi/src/tests` replays such a file as physical memory. Run:

    python tools/acpi/dump_tables.py            # rewrite libs/acpi/golden/
    python tools/acpi/dump_tables.py --only q35-ovmf
"""
from __future__ import annotations

import argparse
import shutil
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "screenshot"))
from qemu_qmp import Qmp, find_qemu, free_port  # noqa: E402

OVMF_CODE = Path("/usr/share/OVMF/OVMF_CODE_4M.fd")
OVMF_VARS = Path("/usr/share/OVMF/OVMF_VARS_4M.fd")
RAM = 128 * 1024 * 1024

# (name, machine, firmware, extra args)
CONFIGS = [
    ("pc-seabios", "pc", "bios", []),
    ("q35-seabios", "q35", "bios", []),
    ("pc-seabios-nohpet", "pc", "bios", ["-no-hpet"]),
    ("q35-ovmf", "q35", "uefi", []),
    ("pc-ovmf", "pc", "uefi", []),
]


def checksum_ok(data: bytes) -> bool:
    return sum(data) & 0xFF == 0


def find_rsdp(ram: bytes) -> int:
    at = 0
    while True:
        at = ram.find(b"RSD PTR ", at)
        if at < 0:
            raise SystemExit("no RSDP in guest RAM")
        if at % 16 == 0 and checksum_ok(ram[at:at + 20]):
            return at
        at += 1


def collect(ram: bytes, rsdp: int) -> list[tuple[int, bytes]]:
    segments: dict[int, bytes] = {}
    revision = ram[rsdp + 15]
    rsdp_len = struct.unpack_from("<I", ram, rsdp + 20)[0] if revision >= 2 else 20
    segments[rsdp] = ram[rsdp:rsdp + rsdp_len]

    def table(phys: int) -> bytes | None:
        if phys == 0 or phys + 36 > len(ram):
            return None
        length = struct.unpack_from("<I", ram, phys + 4)[0]
        if phys + length > len(ram):
            return None
        data = ram[phys:phys + length]
        segments[phys] = data
        return data

    roots = [struct.unpack_from("<I", ram, rsdp + 16)[0]]
    if revision >= 2:
        roots.append(struct.unpack_from("<Q", ram, rsdp + 24)[0])
    for index, root in enumerate(roots):
        data = table(root)
        if data is None:
            continue
        width = 4 if index == 0 else 8
        for off in range(36, len(data), width):
            entry = int.from_bytes(data[off:off + width], "little")
            sub = table(entry)
            if sub is not None and sub[:4] == b"FACP":
                dsdt = struct.unpack_from("<I", sub, 40)[0]
                facs = struct.unpack_from("<I", sub, 36)[0]
                if len(sub) >= 148:
                    dsdt = struct.unpack_from("<Q", sub, 140)[0] or dsdt
                    facs = struct.unpack_from("<Q", sub, 132)[0] or facs
                table(dsdt)
                # The FACS has no checksum; its length is at offset 4 too.
                table(facs)
    return sorted(segments.items())


def dump(qemu: str, name: str, machine: str, firmware: str, extra: list[str], out: Path) -> None:
    port = free_port()
    with tempfile.TemporaryDirectory() as tmp:
        command = [qemu, "-machine", machine, "-m", "128M", "-display", "none",
                   "-nodefaults", "-qmp", f"tcp:127.0.0.1:{port},server=on,wait=off", *extra]
        wait = 4.0
        if firmware == "uefi":
            vars_copy = Path(tmp) / "vars.fd"
            shutil.copy(OVMF_VARS, vars_copy)
            command += ["-drive", f"if=pflash,format=raw,readonly=on,file={OVMF_CODE}",
                        "-drive", f"if=pflash,format=raw,file={vars_copy}"]
            wait = 25.0
        proc = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
        try:
            qmp = Qmp("127.0.0.1", port, 30.0)
            time.sleep(wait)
            qmp.execute("stop")
            ram_file = Path(tmp) / "ram.bin"
            qmp.execute("pmemsave", val=0, size=RAM, filename=str(ram_file))
            try:
                qmp.execute("quit")
            except (RuntimeError, OSError):
                pass  # QEMU may close the socket before answering
        finally:
            proc.wait(timeout=30)
        ram = ram_file.read_bytes()
    rsdp = find_rsdp(ram)
    segments = collect(ram, rsdp)
    blob = bytearray(b"ACPIDUMP" + struct.pack("<Q", rsdp))
    for phys, data in segments:
        blob += struct.pack("<QI", phys, len(data)) + data
    (out / f"{name}.bin").write_bytes(bytes(blob))
    sigs = ", ".join(data[:4].decode("latin1") for _, data in segments[1:])
    print(f"{name}: RSDP at {rsdp:#x}, {len(segments)} segments ({sigs})")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default=str(ROOT / "libs" / "acpi" / "golden"))
    parser.add_argument("--qemu")
    parser.add_argument("--only", help="dump just this configuration")
    args = parser.parse_args()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    qemu = find_qemu(args.qemu)
    for name, machine, firmware, extra in CONFIGS:
        if args.only and args.only != name:
            continue
        dump(qemu, name, machine, firmware, extra, out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
