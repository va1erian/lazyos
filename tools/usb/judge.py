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
    python tools/usb/judge.py shots/usb/serial.log --hotplug 200
    python tools/usb/judge.py shots/usb/serial.log --tablet
    python tools/usb/judge.py shots/usb/serial.log --restart

`--restart` (U5) judges `run.py --restart`: the crash-test build of `usbd`
exited holding `x`; `inputd` saw `x` released before the restarted `usbd`
was ready (the kernel released it when the source's owner died); `init`
restarted `usbd` as `_usb`, which reset the controller and bound both
devices again; and the keys typed afterwards arrived, with nothing held.

`--tablet` (U4) judges `run.py --tablet`: the `usb-tablet` was bound as a
tablet from its report descriptor (QEMU's, byte for byte), and each absolute
position the session sent put `inputd`'s cursor on the pixel it maps to on
the default 1280x720 screen, where it then clicked and scrolled.

`--hotplug N` judges `run.py --hotplug N` instead (U3): N keyboard and
N/10 mouse unplug/replug cycles all detached and re-attached, the key and
button held across the first unplug were released, `inputd` saw exactly
`usbd`'s key edges, the keys typed after the last replug arrived, and the
driver's DMA allocations (`regions=`) stayed bounded by its slot count.
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

#: QEMU's `usb-tablet` report descriptors (`hw/usb/dev-hid.c`), as
#: `USBD:DESC:REPORT` hex: current QEMU reports five buttons, older releases
#: (and distribution builds) three with five bits of padding.
TABLET_REPORT = (
    "05010902a1010901a10005091901290515002501950575018102950175038101"
    "050109300931150026ff7f350046ff7f751095028102050109381581257f3500"
    "4500750895018106c0c0"
)
TABLET_REPORT_3 = (
    "05010902a1010901a10005091901290315002501950375018102950175058101"
    "050109300931150026ff7f350046ff7f751095028102050109381581257f3500"
    "4500750895018106c0c0"
)
TABLET_REPORTS = (TABLET_REPORT, TABLET_REPORT_3)
REPORT_DESC = re.compile(r"USBD:DESC:REPORT port=(\d+) ([0-9a-f]+)")
#: `run.py`'s TABLET_POINTS, and `inputd`'s screen when no compositor set one.
TABLET_POINTS = [(0, 0), (0x7FFF, 0x7FFF), (0x4000, 0x2000)]
SCREEN = (1280, 720)

FATAL = re.compile(r"USBD:(FATAL|PANIC|PORT:FAIL|SLOT:LEAK)")
DETACH = re.compile(r"USBD:DETACH port=(\d+) slot=(\d+) regions=(\d+)")
REGIONS = re.compile(r"USBD:HID:\w+ .* regions=(\d+)")
#: `usbd` enables at most this many slots, so it never needs more regions.
MAX_SLOTS = 8
#: Usages `run.py --hotplug` holds across the first unplug, and then types.
HELD_KEY = 0x1B  # x
TYPED_KEYS = [0x04, 0x05, 0x06]  # a, b, c
HID = re.compile(r"USBD:HID:(KBD|MOUSE|TABLET) port=(\d+)")
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


def judge_hotplug(log: str, cycles: int) -> list[str]:
    """Every reason a `run.py --hotplug` log fails; empty means it passes."""
    failures = [f"driver error: {line}" for line in log.splitlines() if FATAL.search(line)]
    mouse_cycles = (cycles + 9) // 10
    detaches = DETACH.findall(log)
    if len(detaches) != cycles + mouse_cycles:
        failures.append(f"{len(detaches)} detaches, want {cycles + mouse_cycles}")
    kinds = [kind for kind, _ in HID.findall(log)]
    if kinds.count("KBD") != cycles + 1:
        failures.append(f"keyboard attached {kinds.count('KBD')} times, want {cycles + 1}")
    if kinds.count("MOUSE") != mouse_cycles + 1:
        failures.append(f"mouse attached {kinds.count('MOUSE')} times, want {mouse_cycles + 1}")
    regions = [int(r) for r in REGIONS.findall(log)] + [int(r) for *_, r in detaches]
    if regions and max(regions) > MAX_SLOTS:
        failures.append(f"{max(regions)} DMA regions allocated: hot-plug leaks memory")
    failures += judge_keys(log)
    edges = [(int(code, 16), state) for code, state in INPUTD_KEY.findall(log)]
    if (HELD_KEY, "down") not in edges:
        failures.append("the key held across the first unplug never went down")
    for code in {code for code, _ in edges}:
        last = [state for c, state in edges if c == code][-1]
        if last != "up":
            failures.append(f"key {code:#x} is still held at the end")
    downs = [code for code, state in edges if state == "down"]
    if downs[-len(TYPED_KEYS):] != TYPED_KEYS:
        failures.append(f"typed after the last replug: {downs[-len(TYPED_KEYS):]}, want {TYPED_KEYS}")
    states = POINTER.findall(log)
    if not any(int(buttons, 16) & 1 for _, _, buttons, _, _ in states):
        failures.append("the button held across the first unplug never went down")
    if states and int(states[-1][2], 16) != 0:
        failures.append("a button is still held at the end")
    return failures


def tablet_pixel(value: int, pixels: int) -> int:
    """Where QMP absolute `value` (0..=0x7fff) lands: usbd scales the tablet's
    0..=0x7fff to 0..=0xffff, inputd scales that onto the screen."""
    normalized = value * 0xFFFF // 0x7FFF
    return normalized * (pixels - 1) // 0xFFFF


def judge_tablet(log: str) -> list[str]:
    """Every reason a `run.py --tablet` log fails; empty means it passes."""
    failures = [f"driver error: {line}" for line in log.splitlines() if FATAL.search(line)]
    bound = {kind: port for kind, port in HID.findall(log)}
    if "TABLET" not in bound:
        return failures + ["no USBD:HID:TABLET: the tablet was not bound"]
    reports = {port: hex_ for port, hex_ in REPORT_DESC.findall(log)}
    if reports.get(bound["TABLET"]) not in TABLET_REPORTS:
        failures.append(f"tablet report descriptor {reports.get(bound['TABLET'])} is not QEMU's")
    states = [tuple(int(v, 16) if i == 2 else int(v) for i, v in enumerate(m)) for m in POINTER.findall(log)]
    width, height = SCREEN
    for x, y in TABLET_POINTS:
        want = (tablet_pixel(x, width), tablet_pixel(y, height))
        if not any(abs(s[0] - want[0]) <= 1 and abs(s[1] - want[1]) <= 1 for s in states):
            failures.append(f"the cursor never reached {want} (sent {x:#x},{y:#x})")
    last = (tablet_pixel(TABLET_POINTS[-1][0], width), tablet_pixel(TABLET_POINTS[-1][1], height))
    if not any(abs(s[0] - last[0]) <= 1 and abs(s[1] - last[1]) <= 1 and s[2] == 1 for s in states):
        failures.append(f"no left press at {last}")
    if not any(s[3] == MOUSE_WHEEL for s in states):
        failures.append(f"no {MOUSE_WHEEL}-notch wheel event")
    if states and states[-1][2] != 0:
        failures.append("a button is still held at the end")
    return failures + judge_keys(log)


#: `_usb` with `CAP_DEV_CLAIM | CAP_INPUT_SOURCE` (`libs/usbpolicy`).
USB_CRED = "USBD:CRED uid=904 caps=0x500"


def judge_restart(log: str) -> list[str]:
    """Every reason a `run.py --restart` log fails; empty means it passes."""
    failures = [f"driver error: {line}" for line in log.splitlines() if re.search(r"USBD:(FATAL|PANIC|PORT:FAIL|SLOT:LEAK)", line)]
    crash = log.find("USBD:CRASH:TEST")
    if crash < 0:
        return failures + ["usbd never crashed (is this the LAZYOS_USB_CRASH_TEST build?)"]
    if "INIT:RESTART:PASS name=usbd" not in log[crash:]:
        failures.append("init did not restart usbd")
    readies = [m.start() for m in re.finditer(r"USBD:READY", log)]
    if len(readies) < 2 or readies[-1] < crash:
        return failures + ["the restarted usbd never became ready"]
    if log.count(USB_CRED) < 2:
        failures.append(f"usbd did not run as _usb both times ({USB_CRED!r})")
    released = re.search(r"INPUTD:KEY code=0x1b \S+ \S+ up", log[crash:])
    if not released or crash + released.start() > readies[-1]:
        failures.append("the key held when usbd died was not released before it came back")
    after = log[readies[-1]:]
    kinds = [kind for kind, _ in HID.findall(log[crash:])]
    if "KBD" not in kinds or "MOUSE" not in kinds:
        failures.append(f"devices bound after the restart: {kinds}, want KBD and MOUSE")
    edges = [(int(code, 16), state) for code, state in INPUTD_KEY.findall(after)]
    downs = [code for code, state in edges if state == "down"]
    if downs[-len(TYPED_KEYS):] != TYPED_KEYS:
        failures.append(f"typed after the restart: {downs[-len(TYPED_KEYS):]}, want {TYPED_KEYS}")
    all_edges = [(int(code, 16), state) for code, state in INPUTD_KEY.findall(log)]
    for code in {code for code, _ in all_edges}:
        if [state for c, state in all_edges if c == code][-1] != "up":
            failures.append(f"key {code:#x} is still held at the end")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("log", type=Path)
    parser.add_argument("--mouse", action="store_true", help="also judge the mouse steps")
    parser.add_argument("--tablet", action="store_true", help="judge a `run.py --tablet` log")
    parser.add_argument("--restart", action="store_true", help="judge a `run.py --restart` log")
    parser.add_argument("--hotplug", type=int, default=0, metavar="N",
                        help="judge a `run.py --hotplug N` log instead")
    args = parser.parse_args()
    log = args.log.read_text(errors="replace")
    if args.restart:
        failures = judge_restart(log)
    elif args.hotplug:
        failures = judge_hotplug(log, args.hotplug)
    elif args.tablet:
        failures = judge_tablet(log)
    else:
        failures = judge(log, args.mouse)
    for failure in failures:
        print(f"FAIL: {failure}")
    print("usb judge: " + ("FAIL" if failures else "PASS"))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
