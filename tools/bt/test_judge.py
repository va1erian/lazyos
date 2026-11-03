#!/usr/bin/env python3
"""The Bluetooth judge must fail when it should (`python tools/bt/test_judge.py`)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import judge  # noqa: E402

ADDR = "00:1A:7D:DA:71:13"

# A real run's lines (tools/bt/run.py, QEMU with WHPX).
GOOD = f"""init: started btd (pid 5, attempt 1)
BT:CRED uid=916 caps=0x400
BT:WAIT no Bluetooth controller yet
USBD:CRED uid=904 caps=0xd00
USBD:XHCI hc=0 vendor=0x1b36 device=0x000d version=0x100 ports=8 usb2=4 usb3=4 slots=12
USBD:PORT port=0-5 speed=High
USBD:DESC:DEVICE port=0-5 {judge.DEVICE}
USBD:DESC:CONFIG port=0-5 {judge.CONFIG}
USBD:BT:BIND port=0-5 slot=1 vendor=0x0a12 product=0x0001 interface=0 intr=3 bulk_in=5 bulk_out=4 chunk=512
USBD:READY devices=1 controllers=1
USBD:BT:SERVE name=os.lazy.usb.bt
USBD:BT:ATTACH owner=5 ring=1 slots=64
BT:LINK ring=1
BT:CTRL:PASS:hci=9 lmp=9 vendor=0x05f1 addr=**:**:**:**:71:13 name=LazyOS Fake Controller
"""

UNPLUG = GOOD + """USBD:BT:UNBIND port=0-5
USBD:BT:DETACH owner=5 ring=1 controller unplugged
USBD:DETACH port=0-5 slot=1 regions=1 functions=bt (unplugged)
BT:CTRL:GONE the controller was unplugged
BT:WAIT no Bluetooth controller yet
USBD:BT:WITHDRAW name=os.lazy.usb.bt
"""


class Judge(unittest.TestCase):
    def assertFails(self, log, unplug=False, why=""):
        failures = judge.judge(log, ADDR, unplug)
        self.assertNotEqual(failures, [], "the judge passed a bad log")
        if why:
            self.assertTrue(any(why in f for f in failures), f"{why!r} not in {failures}")

    def test_good_logs_pass(self):
        self.assertEqual(judge.judge(GOOD, ADDR), [])
        self.assertEqual(judge.judge(UNPLUG, ADDR, unplug=True), [])

    def test_an_unbind_in_a_run_that_did_not_unplug_fails(self):
        self.assertFails(UNPLUG, why="did not unplug")

    def test_nothing_bound(self):
        self.assertFails(GOOD.replace("USBD:BT:BIND", "USBD:BT:SKIP"), why="did not bind")

    def test_only_interface_zero_binds_once(self):
        bind = next(line for line in GOOD.splitlines() if "USBD:BT:BIND" in line)
        self.assertFails(GOOD + bind + "\n", why="only interface 0")
        self.assertFails(GOOD.replace("interface=0", "interface=1"), why="interface 1")

    def test_wrong_endpoints_or_device(self):
        self.assertFails(GOOD.replace("bulk_in=5", "bulk_in=7"), why="endpoints")
        self.assertFails(GOOD.replace("product=0x0001", "product=0x0002"), why="not the fake")

    def test_the_descriptors_must_be_the_fakes(self):
        self.assertFails(GOOD.replace(judge.DEVICE, judge.DEVICE[:-2] + "ff"), why="device descriptor")
        self.assertFails(GOOD.replace(judge.CONFIG, judge.CONFIG[:-2] + "ff"), why="configuration")

    def test_the_service_and_the_rings_must_have_come_up(self):
        self.assertFails(GOOD.replace("USBD:BT:SERVE", "USBD:BT:NOPE"), why="never registered")
        self.assertFails(GOOD.replace("slots=64", "slots=16"), why="rings were never attached")

    def test_btd_must_run_as_bt_with_one_capability(self):
        self.assertFails(GOOD.replace("uid=916", "uid=0"), why="ran as uid")
        self.assertFails(GOOD.replace("caps=0x400", "caps=0xd00"), why="ran as uid")
        self.assertFails(GOOD.replace("BT:CRED", "BT:CREDX"), why="never started")

    def test_the_controller_must_come_up_with_the_fakes_answers(self):
        self.assertFails(GOOD.replace("BT:CTRL:PASS", "BT:CTRL:FAIL"))
        self.assertFails(GOOD.replace("hci=9", "hci=7"), why="not the fake's")
        self.assertFails(GOOD.replace("vendor=0x05f1", "vendor=0x0002"), why="not the fake's")
        self.assertFails(GOOD.replace("71:13", "00:00"), why="address ends")
        self.assertFails(GOOD.replace("Fake Controller", "Other"), why="name")
        self.assertFails(GOOD.replace(" name=LazyOS Fake Controller", ""), why="no BT:CTRL:PASS")

    def test_the_full_address_must_not_be_logged(self):
        self.assertFails(GOOD + "debug 00:1a:7d:da:71:13\n", why="full Bluetooth address")
        self.assertFails(GOOD + "debug 001A7DDA7113\n", why="full Bluetooth address")

    def test_driver_errors(self):
        self.assertFails(GOOD + "USBD:PANIC boom\n")
        self.assertFails(GOOD + "USBD:BT:FAIL port=0-5 start: timed out\n")
        self.assertFails(GOOD + "BT:PANIC panicked\n")
        self.assertFails(GOOD + "USBD:PORT:FAIL port=0-6 x\n")

    def test_order_matters(self):
        lines = GOOD.splitlines()
        ctrl = next(i for i, line in enumerate(lines) if "BT:CTRL:PASS" in line)
        swapped = lines[:]
        swapped.insert(0, swapped.pop(ctrl))
        self.assertFails("\n".join(swapped) + "\n", why="came before")

    def test_unplug_must_release_everything(self):
        self.assertFails(GOOD, unplug=True, why="UNBIND")
        self.assertFails(UNPLUG.replace("USBD:BT:WITHDRAW", "USBD:BT:XX"), unplug=True, why="withdrew")
        self.assertFails(UNPLUG.replace("functions=bt", "functions=hid"), unplug=True, why="DETACH")
        self.assertFails(UNPLUG.replace("BT:CTRL:GONE", "BT:CTRL:GON"), unplug=True, why="never saw")

    def test_unplug_order(self):
        reordered = GOOD + "BT:CTRL:GONE x\nUSBD:BT:UNBIND port=0-5\nUSBD:BT:WITHDRAW name=os.lazy.usb.bt\n"
        reordered += "USBD:DETACH port=0-5 slot=1 regions=1 functions=bt (unplugged)\n"
        self.assertFails(reordered, unplug=True, why="came before")


if __name__ == "__main__":
    unittest.main()
