#!/usr/bin/env python3
"""Judge a `usbd` serial log (docs/usb-hid-plan.md U2).

The verdict is what reached `inputd`, not that `usbd` printed a marker: every
key `inputd` decoded must have come through `usbd` (a `USBD:KEY` edge with the
same usage and direction, in order), the mouse steps of the session must have
moved, clicked and scrolled `inputd`'s cursor, and the descriptors the
devices returned must be QEMU's, byte for byte (the same golden bytes
`libs/usbhid` is tested against). `run.py` boots without an i8042, so no key or
pointer record can have come from PS/2.

    python tools/usb/judge.py shots/usb/serial.log [--mouse]
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

#: Descriptors of QEMU's high-speed `usb-kbd` / `usb-mouse` behind
#: `qemu-xhci` (`libs/usbhid/src/tests/golden.rs`).
GOLDEN = {
    "KBD": (
        "120100020000004027060100000001040b01",
        "09022200010108a032090400000103010100092111010001223f0007058103080007",
    ),
    "MOUSE": (
        "120100020000004027060100000001020901",
        "09022200010106a03209040000010301020009210100000122340007058103040007",
    ),
}

#: Where `run.py`'s mouse steps leave the cursor: corner, then +40,+30,
#: a left click there, then two wheel notches up.
MOUSE_CLICK = (40, 30)
MOUSE_WHEEL = 2

FATAL = re.compile(r"USBD:(FATAL|PANIC|PORT:FAIL)")
HID = re.compile(r"USBD:HID:(KBD|MOUSE) port=(\d+)")
DESC = re.compile(r"USBD:DESC:(DEVICE|CONFIG) port=(\d+) ([0-9a-f]+)")
USB_KEY = re.compile(r"USBD:KEY usage=0x([0-9a-f]+) (down|up)")
INPUTD_KEY = re.compile(r"INPUTD:KEY code=0x([0-9a-f]+) sym=\S+ mods=\S+ (down|up)")
POINTER = re.compile(r"INPUTD:POINTER x=(-?\d+) y=(-?\d+) buttons=0x([0-9a-f]+) wheel=(-?\d+),(-?\d+)")


def judge(log: str, mouse: bool) -> list[str]:
    """Every reason the log fails; empty means it passes."""
    failures = [f"driver error: {line}" for line in log.splitlines() if FATAL.search(line)]
    if "USBD:XHCI " not in log:
        failures.append("the controller never came up (no USBD:XHCI)")
    bound = {kind: port for kind, port in HID.findall(log)}
    wanted = ["KBD", "MOUSE"] if mouse else ["KBD"]
    for kind in wanted:
        if kind not in bound:
            failures.append(f"no USBD:HID:{kind}: the device was not configured")
            continue
        descriptors = {what: hex_ for what, port, hex_ in DESC.findall(log) if port == bound[kind]}
        device, config = GOLDEN[kind]
        if descriptors.get("DEVICE") != device:
            failures.append(f"{kind} device descriptor {descriptors.get('DEVICE')} != {device}")
        if descriptors.get("CONFIG") != config:
            failures.append(f"{kind} configuration {descriptors.get('CONFIG')} != {config}")
    failures += judge_keys(log)
    if mouse:
        failures += judge_pointer(log)
    return failures


def judge_keys(log: str) -> list[str]:
    """`inputd`'s key edges must be exactly `usbd`'s, in order."""
    usb = [(int(code, 16), state) for code, state in USB_KEY.findall(log)]
    seen = [(int(code, 16), state) for code, state in INPUTD_KEY.findall(log)]
    if not seen:
        return ["inputd decoded no key at all"]
    if seen != usb:
        for index, (got, want) in enumerate(zip(seen, usb)):
            if got != want:
                return [f"key edge {index}: inputd saw {got}, usbd sent {want}"]
        return [f"inputd saw {len(seen)} key edges, usbd sent {len(usb)}"]
    return []


def judge_pointer(log: str) -> list[str]:
    states = [tuple(int(v, 16) if i == 2 else int(v) for i, v in enumerate(m)) for m in POINTER.findall(log)]
    if not states:
        return ["inputd reported no pointer state at all"]
    failures = []
    if not any(s[0] == 0 and s[1] == 0 for s in states):
        failures.append("the cursor never reached the corner")
    x, y = MOUSE_CLICK
    if not any((s[0], s[1], s[2]) == (x, y, 1) for s in states):
        failures.append(f"no left press at ({x}, {y})")
    if not any(s[3] == MOUSE_WHEEL for s in states):
        failures.append(f"no {MOUSE_WHEEL}-notch wheel event")
    if states[-1][2] != 0:
        failures.append("a button is still held at the end")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("log", type=Path)
    parser.add_argument("--mouse", action="store_true", help="also judge the mouse steps")
    args = parser.parse_args()
    failures = judge(args.log.read_text(errors="replace"), args.mouse)
    for failure in failures:
        print(f"FAIL: {failure}")
    print("usb judge: " + ("FAIL" if failures else "PASS"))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
