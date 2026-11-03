#!/usr/bin/env python3
"""Judge a Bluetooth transport serial log (docs/bluetooth-plan.md, phase B2).

The verdict is what the guest did, not that a marker printed: `usbd` bound
the fake dongle's interface 0 from the descriptors the device returned (byte
for byte what `tools/bt/fakebt/src/device.rs` serves) and left its isochronous
interface 1 alone; `btd` ran as `_bt` with the one capability it is meant to
hold, attached the rings, and brought the controller up through them: the
version and address in `BT:CTRL:PASS` are the fake controller's answers to
`HCI_Reset`, `Read_Local_Version` and `Read_BD_ADDR`, which only reached `btd`
if a command went out as a class request, the event came back on the
interrupt endpoint and both crossed the rings. The full address must never
appear in the log (plan decision 9: the last two octets only).

    python tools/bt/judge.py shots/bt/serial.log --addr 00:1A:7D:DA:71:13
    python tools/bt/judge.py shots/bt/serial.log --addr 00:1A:7D:DA:71:13 --unplug

`--unplug` judges `run.py --unplug`: after the bring-up the dongle was pulled
out; `usbd` released the function and withdrew the interface, and `btd` saw
the controller go.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

#: The fake's descriptors as `USBD:DESC:*` prints them
#: (`fakebt/src/device.rs`, which pins the same bytes in a test).
DEVICE = "12010002e0010140120a0100000101020001"
CONFIG = (
    "0902470002010080320904000003e00101000705810310000107058202000200070502020002000904"
    "010000e00101000904010102e00101000705830109000107050301090001"
)
VENDOR, PRODUCT = 0x0A12, 0x0001
#: What the stub controller reports (`fakebt/src/stub.rs`).
HCI_VERSION, LMP_VERSION, MANUFACTURER = 9, 9, 0x05F1
#: Its Read_Local_Name answer: a 254-byte event, sixteen packets of the
#: events endpoint, so `usbd`'s reassembly is on the path.
LOCAL_NAME = "LazyOS Fake Controller"
#: `_bt`, and the one capability it holds (`CAP_INPUT_SOURCE`, bit 10).
BT_UID, BT_CAPS = 916, 1 << 10

FATAL = re.compile(r"USBD:(FATAL|PANIC|PORT:FAIL|SLOT:LEAK|BT:FAIL)|BT:(PANIC|CTRL:FAIL|LINK:FAIL)")
BIND = re.compile(
    r"USBD:BT:BIND port=(\S+) slot=(\d+) vendor=0x([0-9a-f]+) product=0x([0-9a-f]+) "
    r"interface=(\d+) intr=(\d+) bulk_in=(\d+) bulk_out=(\d+)"
)
DESC = re.compile(r"USBD:DESC:(DEVICE|CONFIG) port=(\S+) ([0-9a-f]+)")
CRED = re.compile(r"BT:CRED uid=(\d+) caps=0x([0-9a-f]+)")
CTRL = re.compile(
    r"BT:CTRL:PASS:hci=(\d+) lmp=(\d+) vendor=0x([0-9a-f]+) addr=\*\*:\*\*:\*\*:\*\*:([0-9A-F]{2}):([0-9A-F]{2}) name=(.*)$",
    re.MULTILINE,
)
DETACH = re.compile(r"USBD:DETACH port=(\S+) slot=\d+ regions=\d+ functions=(\S*)")


def position(log: str, needle: str | re.Pattern[str]) -> int:
    """Where `needle` first appears in `log`, or -1."""
    if isinstance(needle, str):
        return log.find(needle)
    match = needle.search(log)
    return match.start() if match else -1


def judge(log: str, address: str, unplug: bool = False) -> list[str]:
    """Every reason the log fails; empty means it passes."""
    failures = [f"error: {line.strip()}" for line in log.splitlines() if FATAL.search(line)]
    failures += judge_bind(log)
    failures += judge_btd(log, address)
    failures += judge_order(log, unplug)
    if unplug:
        failures += judge_unplug(log)
    elif "USBD:BT:UNBIND" in log:
        failures.append("the controller was unbound in a run that did not unplug it")
    return failures


def judge_bind(log: str) -> list[str]:
    """`usbd` bound interface 0 of the dongle, from the right descriptors."""
    binds = BIND.findall(log)
    if not binds:
        return ["no USBD:BT:BIND: usbd did not bind the controller"]
    failures = []
    if len(binds) != 1:
        failures.append(f"{len(binds)} USBD:BT:BIND lines: only interface 0 binds, once")
    port, _slot, vendor, product, interface, intr, bulk_in, bulk_out = binds[0]
    if (int(vendor, 16), int(product, 16)) != (VENDOR, PRODUCT):
        failures.append(f"bound {vendor}:{product}, not the fake {VENDOR:04x}:{PRODUCT:04x}")
    if interface != "0":
        failures.append(f"bound interface {interface}: only interface 0 may bind")
    # Endpoints 1 IN (DCI 3), 2 IN (DCI 5) and 2 OUT (DCI 4); the isochronous
    # pair of interface 1 (3 IN, 3 OUT) must not be among them.
    if (intr, bulk_in, bulk_out) != ("3", "5", "4"):
        failures.append(f"endpoints intr={intr} bulk_in={bulk_in} bulk_out={bulk_out}, want DCI 3, 5, 4")
    descriptors = {what: hex_ for what, at, hex_ in DESC.findall(log) if at == port}
    if descriptors.get("DEVICE") != DEVICE:
        failures.append(f"device descriptor {descriptors.get('DEVICE')} != {DEVICE}")
    if descriptors.get("CONFIG") != CONFIG:
        failures.append(f"configuration {descriptors.get('CONFIG')} != {CONFIG}")
    if "USBD:BT:SERVE name=os.lazy.usb.bt" not in log:
        failures.append("usbd never registered os.lazy.usb.bt")
    if not re.search(r"USBD:BT:ATTACH owner=\d+ ring=\d+ slots=64", log):
        failures.append("no USBD:BT:ATTACH: btd's rings were never attached")
    return failures


def judge_btd(log: str, address: str) -> list[str]:
    """`btd` ran with the identity it is meant to hold and brought the
    controller up with what the fake answered."""
    failures = []
    cred = CRED.search(log)
    if cred is None:
        failures.append("no BT:CRED: btd never started")
    elif (int(cred.group(1)), int(cred.group(2), 16)) != (BT_UID, BT_CAPS):
        failures.append(f"btd ran as uid {cred.group(1)} caps {cred.group(2)}, want _bt with only CAP_INPUT_SOURCE")
    ctrl = CTRL.search(log)
    if ctrl is None:
        failures.append("no BT:CTRL:PASS: the controller was not brought up")
        return failures
    hci, lmp, vendor, tail_high, tail_low, name = ctrl.groups()
    if (int(hci), int(lmp), int(vendor, 16)) != (HCI_VERSION, LMP_VERSION, MANUFACTURER):
        failures.append(f"BT:CTRL reports hci={hci} lmp={lmp} vendor=0x{vendor}, not the fake's")
    if name.strip() != LOCAL_NAME:
        failures.append(f"BT:CTRL name {name.strip()!r} is not the fake's {LOCAL_NAME!r}")
    octets = address.upper().split(":")
    if [tail_high, tail_low] != octets[-2:]:
        failures.append(f"BT:CTRL address ends {tail_high}:{tail_low}, the fake's is {':'.join(octets[-2:])}")
    # Decision 9: no address appears in full in a default-build trace.
    lowered = log.lower()
    if address.lower() in lowered or address.lower().replace(":", "") in lowered:
        failures.append("the full Bluetooth address appears in the log")
    return failures


def judge_order(log: str, unplug: bool) -> list[str]:
    """The story in order: bound, attached, brought up (and then, for an
    unplug run, released)."""
    steps = [
        ("USBD:BT:BIND", position(log, BIND)),
        ("USBD:BT:ATTACH", position(log, "USBD:BT:ATTACH")),
        ("BT:CTRL:PASS", position(log, "BT:CTRL:PASS")),
    ]
    if unplug:
        steps += [("USBD:BT:UNBIND", position(log, "USBD:BT:UNBIND")),
                  ("BT:CTRL:GONE", position(log, "BT:CTRL:GONE"))]
    failures = []
    seen = [(name, at) for name, at in steps if at >= 0]
    for (name, at), (later, after) in zip(seen, seen[1:]):
        if after < at:
            failures.append(f"{later} came before {name}")
    return failures


def judge_unplug(log: str) -> list[str]:
    """The dongle was pulled out: `usbd` released the function, withdrew the
    name and detached the device; `btd` noticed."""
    failures = []
    if "USBD:BT:UNBIND" not in log:
        failures.append("no USBD:BT:UNBIND: usbd did not release the controller")
    if "USBD:BT:WITHDRAW name=os.lazy.usb.bt" not in log:
        failures.append("usbd never withdrew os.lazy.usb.bt")
    detached = [kinds.split(",") for _port, kinds in DETACH.findall(log)]
    if not any("bt" in kinds for kinds in detached):
        failures.append("no USBD:DETACH of a device with a bt function")
    if "BT:CTRL:GONE" not in log:
        failures.append("no BT:CTRL:GONE: btd never saw the controller go")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("log", type=Path)
    parser.add_argument("--addr", required=True, help="the address the fake controller reports")
    parser.add_argument("--unplug", action="store_true", help="judge run.py --unplug")
    args = parser.parse_args()
    log = args.log.read_text(errors="replace")
    failures = judge(log, args.addr, args.unplug)
    for failure in failures:
        print(f"BT:JUDGE:FAIL {failure}")
    print("BT:JUDGE:" + ("FAIL" if failures else "PASS") + (" unplug" if args.unplug else ""))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
