#!/usr/bin/env python3
"""Self-test of `ui_run.py`'s judge: it passes a good run and fails the bad
ones (a missing marker, a leaked passphrase, a label denial, a forged name).

Run: python tools/wifi/test_ui_judge.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import ui_run  # noqa: E402

GOOD = "\n".join([
    "NETAPP:WIFI:UP if=wlan0",
    "NETAPP:WIFI:SCAN:PASS n=5",
    'NETAPP:WIFI:JOIN:REQUESTED id=wa ssid="LazyOpen" n=1',
    'NETAPP:WIFI:STATE if=wlan0 state=connected ssid="LazyOpen"',
    "NETAPP:WIFI:LEAVE:PASS n=2",
    'NETAPP:WIFI:JOIN:REQUESTED id=wb ssid="LazyWPA2" n=3',
    'NETAPP:WIFI:STATE if=wlan0 state=failed ssid="LazyWPA2"',
    "NETAPP:WIFI:FORGET:PASS id=wb n=4",
    'NETAPP:WIFI:JOIN:REQUESTED id=wc ssid="LazyWPA2" n=7',
    'NETAPP:WIFI:STATE if=wlan0 state=connected ssid="LazyWPA2"',
    "NETAPP:WIFI:JOIN:REFUSED Type the passphrase, or tick Open for a network without one.",
    'NETAPP:WIFI:STATE if=wlan0 state=connected ssid="LazyHidden"',
])
AP = "station associated LazyWPA2"


class JudgeTests(unittest.TestCase):
    def test_a_good_run_passes(self) -> None:
        self.assertEqual(ui_run.judge(GOOD, AP), [])

    def test_a_missing_marker_fails(self) -> None:
        bad = GOOD.replace('state=connected ssid="LazyHidden"', "state=idle")
        self.assertTrue(any("hidden" in p for p in ui_run.judge(bad, AP)))

    def test_a_wrong_row_fails(self) -> None:
        bad = GOOD.replace('ssid="LazyWPA2" n=7', 'ssid="LazyRoam" n=7')
        self.assertTrue(any("joined again" in p for p in ui_run.judge(bad, AP)))

    def test_a_passphrase_on_serial_fails(self) -> None:
        bad = GOOD + "\nNETAPP:DEBUG " + ui_run.WPA2_PASSPHRASE
        self.assertTrue(any("passphrase" in p for p in ui_run.judge(bad, AP)))

    def test_a_label_denial_for_the_tray_applet_fails(self) -> None:
        bad = GOOD + "\nLABEL:DENY label=os.lazy.netstatus iface=1 method=2"
        self.assertTrue(any("label denial" in p for p in ui_run.judge(bad, AP)))

    def test_the_access_point_must_have_seen_the_station(self) -> None:
        self.assertTrue(ui_run.judge(GOOD, ""))


if __name__ == "__main__":
    unittest.main()
