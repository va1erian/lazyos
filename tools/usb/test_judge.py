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


if __name__ == "__main__":
    unittest.main()
