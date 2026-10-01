#!/usr/bin/env python3
"""The USB judge must fail when it should (`python tools/usb/test_judge.py`)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import judge  # noqa: E402

KBD = judge.GOLDEN["KBD"]
MOUSE = judge.GOLDEN["MOUSE"]

GOOD = f"""usbd: USB HID driver
DEV:CROSSCLAIM:usb:PASS uid=904 own=1 refused=6
USBD:XHCI version=0x100 ports=8 slots=8 scratchpads=0 csz64=false
USBD:PORT port=5 speed=High
USBD:DESC:DEVICE port=5 {KBD[0]}
USBD:DESC:CONFIG port=5 {KBD[1]}
USBD:HID:KBD port=5 slot=1 vendor=0x0627 product=0x0001 interface=0 dci=3
USBD:PORT port=6 speed=High
USBD:DESC:DEVICE port=6 {MOUSE[0]}
USBD:DESC:CONFIG port=6 {MOUSE[1]}
USBD:HID:MOUSE port=6 slot=2 vendor=0x0627 product=0x0001 interface=0 dci=3
USBD:READY devices=2
USBD:KEY usage=0x14 down
INPUTD:KEY code=0x14 sym=0x71 mods=0x40 down
USBD:KEY usage=0x14 up
INPUTD:KEY code=0x14 sym=0x71 mods=0x40 repeat
INPUTD:KEY code=0x14 sym=0x71 mods=0x40 up
INPUTD:POINTER x=0 y=0 buttons=0x0 wheel=0,0
INPUTD:POINTER x=40 y=30 buttons=0x0 wheel=0,0
INPUTD:POINTER x=40 y=30 buttons=0x1 wheel=0,0
INPUTD:POINTER x=40 y=30 buttons=0x0 wheel=0,0
INPUTD:POINTER x=40 y=30 buttons=0x0 wheel=2,0
"""


class Judge(unittest.TestCase):
    def test_good_log_passes(self):
        self.assertEqual(judge.judge(GOOD, mouse=True), [])

    def assertFails(self, log, mouse=True):
        self.assertNotEqual(judge.judge(log, mouse), [], "the judge passed a bad log")

    def test_crossclaim(self):
        self.assertFails(GOOD.replace("DEV:CROSSCLAIM:usb:PASS", "DEV:CROSSCLAIM:usb:SKIP"))
        self.assertFails(GOOD.replace(
            "DEV:CROSSCLAIM:usb:PASS uid=904 own=1 refused=6",
            "DEV:CROSSCLAIM:usb:FAIL uid=904 claimed device 3 (net)",
        ))

    def test_missing_controller(self):
        self.assertFails(GOOD.replace("USBD:XHCI ", "USBD:XHCX "))

    def test_driver_errors(self):
        self.assertFails(GOOD + "USBD:PORT:FAIL port=7 timed out\n")
        self.assertFails(GOOD + "USBD:PANIC boom\n")

    def test_missing_device(self):
        self.assertFails(GOOD.replace("USBD:HID:MOUSE", "USBD:HID:OTHER"))
        # Keyboard-only runs do not need the mouse.
        self.assertEqual(judge.judge(GOOD.replace("USBD:HID:MOUSE", "USBD:HID:OTHER"), mouse=False), [])

    def test_wrong_descriptor(self):
        self.assertFails(GOOD.replace(KBD[1], KBD[1][:-2] + "0a"))

    def test_key_not_from_usb(self):
        # A key inputd saw that usbd never sent (it came from somewhere else).
        self.assertFails(GOOD + "INPUTD:KEY code=0x1a sym=0x77 mods=0x40 down\n")
        self.assertFails(GOOD.replace("USBD:KEY usage=0x14 up\n", "USBD:KEY usage=0x15 up\n"))

    def test_no_keys(self):
        self.assertFails("\n".join(line for line in GOOD.splitlines() if "KEY" not in line))

    def test_pointer_steps(self):
        self.assertFails(GOOD.replace("x=40 y=30 buttons=0x1", "x=41 y=30 buttons=0x1"))
        self.assertFails(GOOD.replace("wheel=2,0", "wheel=1,0"))
        self.assertFails(GOOD.replace("INPUTD:POINTER x=0 y=0", "INPUTD:POINTER x=1 y=0"))
        self.assertFails(GOOD + "INPUTD:POINTER x=40 y=30 buttons=0x1 wheel=0,0\n")
        self.assertFails("\n".join(line for line in GOOD.splitlines() if "POINTER" not in line))


def hotplug_log(cycles: int) -> str:
    """A passing `run.py --hotplug` log: x and the left button held across
    the first unplug, every cycle detached and re-attached, a b c typed."""
    lines = [
        "USBD:HID:KBD port=5 slot=1 vendor=0x0627 product=0x0001 interface=0 dci=3 regions=1",
        "USBD:HID:MOUSE port=6 slot=2 vendor=0x0627 product=0x0001 interface=0 dci=3 regions=2",
        "USBD:READY devices=2",
        "USBD:KEY usage=0x1b down",
        "INPUTD:KEY code=0x1b sym=0x78 mods=0x40 down",
        "INPUTD:POINTER x=0 y=0 buttons=0x1 wheel=0,0",
    ]
    for cycle in range(cycles):
        lines.append("USBD:KEY usage=0x1b up" if cycle == 0 else "")
        lines.append("INPUTD:KEY code=0x1b sym=0x78 mods=0x40 up" if cycle == 0 else "")
        lines.append("USBD:DETACH port=5 slot=1 regions=2 (unplugged)")
        lines.append("USBD:HID:KBD port=5 slot=1 vendor=0x0627 product=0x0001 interface=0 dci=3 regions=2")
        if cycle % 10 == 0:
            lines.append("INPUTD:POINTER x=0 y=0 buttons=0x0 wheel=0,0" if cycle == 0 else "")
            lines.append("USBD:DETACH port=6 slot=2 regions=2 (unplugged)")
            lines.append("USBD:HID:MOUSE port=6 slot=2 vendor=0x0627 product=0x0001 interface=0 dci=3 regions=2")
    for usage, sym in ((0x04, 0x61), (0x05, 0x62), (0x06, 0x63)):
        for state in ("down", "up"):
            lines.append(f"USBD:KEY usage={usage:#x} {state}")
            lines.append(f"INPUTD:KEY code={usage:#x} sym={sym:#x} mods=0x40 {state}")
    return "\n".join(line for line in lines if line) + "\n"


class Hotplug(unittest.TestCase):
    CYCLES = 12

    def test_good_log_passes(self):
        self.assertEqual(judge.judge_hotplug(hotplug_log(self.CYCLES), self.CYCLES), [])

    def assertFails(self, log):
        self.assertNotEqual(judge.judge_hotplug(log, self.CYCLES), [], "the judge passed a bad log")

    def test_missing_cycle(self):
        good = hotplug_log(self.CYCLES)
        self.assertFails(good.replace("USBD:DETACH port=5 slot=1 regions=2 (unplugged)\n", "", 1))
        self.assertFails(good.replace("USBD:HID:MOUSE port=6", "USBD:HID:OTHER port=6", 1))

    def test_dma_leak(self):
        self.assertFails(hotplug_log(self.CYCLES) + "USBD:DETACH port=5 slot=1 regions=9 (unplugged)\n")

    def test_driver_errors(self):
        self.assertFails(hotplug_log(self.CYCLES) + "USBD:SLOT:LEAK slot=1 disable failed\n")

    def test_stuck_key_and_button(self):
        good = hotplug_log(self.CYCLES)
        self.assertFails(good.replace("INPUTD:KEY code=0x1b sym=0x78 mods=0x40 up\n", ""))
        self.assertFails(good.replace("INPUTD:POINTER x=0 y=0 buttons=0x0 wheel=0,0\n", ""))

    def test_typing_after_replug(self):
        good = hotplug_log(self.CYCLES)
        self.assertFails(good.replace("code=0x6 sym=0x63 mods=0x40 down", "code=0x7 sym=0x64 mods=0x40 down"))


def tablet_log() -> str:
    """A passing `run.py --tablet` log on the default 1280x720 screen."""
    px = judge.tablet_pixel
    lines = [
        "USBD:XHCI version=0x100 ports=8 slots=8 scratchpads=0 csz64=false",
        f"USBD:DESC:REPORT port=6 {judge.TABLET_REPORT}",
        "USBD:HID:TABLET port=6 slot=2 vendor=0x0627 product=0x0001 interface=0 dci=3 regions=2",
        "USBD:KEY usage=0x14 down",
        "INPUTD:KEY code=0x14 sym=0x71 mods=0x40 down",
        "USBD:KEY usage=0x14 up",
        "INPUTD:KEY code=0x14 sym=0x71 mods=0x40 up",
    ]
    for x, y in judge.TABLET_POINTS:
        lines.append(f"INPUTD:POINTER x={px(x, 1280)} y={px(y, 720)} buttons=0x0 wheel=0,0")
    x, y = px(0x4000, 1280), px(0x2000, 720)
    lines += [
        f"INPUTD:POINTER x={x} y={y} buttons=0x1 wheel=0,0",
        f"INPUTD:POINTER x={x} y={y} buttons=0x0 wheel=0,0",
        f"INPUTD:POINTER x={x} y={y} buttons=0x0 wheel=2,0",
    ]
    return "\n".join(lines) + "\n"


class Tablet(unittest.TestCase):
    def test_good_log_passes(self):
        self.assertEqual(judge.judge_tablet(tablet_log()), [])
        older = tablet_log().replace(judge.TABLET_REPORT, judge.TABLET_REPORT_3)
        self.assertEqual(judge.judge_tablet(older), [], "the three-button QEMU tablet")

    def test_scaling(self):
        self.assertEqual(judge.tablet_pixel(0, 1280), 0)
        self.assertEqual(judge.tablet_pixel(0x7FFF, 1280), 1279)
        self.assertEqual(judge.tablet_pixel(0x7FFF, 720), 719)

    def assertFails(self, log):
        self.assertNotEqual(judge.judge_tablet(log), [], "the judge passed a bad log")

    def test_not_bound_or_wrong_descriptor(self):
        self.assertFails(tablet_log().replace("USBD:HID:TABLET", "USBD:HID:MOUSE"))
        self.assertFails(tablet_log().replace(judge.TABLET_REPORT, judge.TABLET_REPORT[:-2] + "c1"))

    def test_cursor_misplaced(self):
        log = tablet_log()
        self.assertFails(log.replace("x=1279 y=719", "x=1279 y=700"))
        self.assertFails(log.replace("buttons=0x1", "buttons=0x0"))
        self.assertFails(log.replace("wheel=2,0", "wheel=1,0"))
        self.assertFails(log + "INPUTD:POINTER x=640 y=180 buttons=0x1 wheel=0,0\n")


def restart_log() -> str:
    """A passing `run.py --restart` log."""
    bind = [
        judge.USB_CRED,
        "USBD:HID:KBD port=5 slot=1 vendor=0x0627 product=0x0001 interface=0 dci=3 regions=1",
        "USBD:HID:MOUSE port=6 slot=2 vendor=0x0627 product=0x0001 interface=0 dci=3 regions=2",
        "USBD:READY devices=2",
    ]
    lines = bind + [
        "USBD:KEY usage=0x1b down",
        "INPUTD:KEY code=0x1b sym=0x78 mods=0x40 down",
        "USBD:CRASH:TEST exiting with a key held",
        "INPUTD:KEY code=0x1b sym=0x78 mods=0x40 up",
        "INIT:RESTART:PASS name=usbd status=3 attempt=1 delay=10",
    ] + bind
    for usage, sym in ((0x04, 0x61), (0x05, 0x62), (0x06, 0x63)):
        for state in ("down", "up"):
            lines.append(f"USBD:KEY usage={usage:#x} {state}")
            lines.append(f"INPUTD:KEY code={usage:#x} sym={sym:#x} mods=0x40 {state}")
    return "\n".join(lines) + "\n"


class Restart(unittest.TestCase):
    def test_good_log_passes(self):
        self.assertEqual(judge.judge_restart(restart_log()), [])

    def assertFails(self, log):
        self.assertNotEqual(judge.judge_restart(log), [], "the judge passed a bad log")

    def test_no_crash_or_restart(self):
        self.assertFails(restart_log().replace("USBD:CRASH:TEST", "USBD:NOTHING"))
        self.assertFails(restart_log().replace("INIT:RESTART:PASS name=usbd", "INIT:RESTART:PASS name=sndd"))

    def test_held_key_not_released(self):
        self.assertFails(restart_log().replace("INPUTD:KEY code=0x1b sym=0x78 mods=0x40 up\n", ""))

    def test_not_rebound_or_wrong_identity(self):
        log = restart_log()
        self.assertFails(log[: log.rfind("USBD:READY")])
        self.assertFails(log.replace(judge.USB_CRED, "USBD:CRED uid=0 caps=0xffffffff", 1))

    def test_typing_after_restart(self):
        self.assertFails(restart_log().replace("code=0x6 sym=0x63 mods=0x40 down", "code=0x7 sym=0x64 mods=0x40 down"))


if __name__ == "__main__":
    unittest.main()
