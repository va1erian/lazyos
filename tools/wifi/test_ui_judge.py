#!/usr/bin/env python3
"""The Wi-Fi page judge (`ui_judge.py`) fails when it should: a good run
passes every check, and each check fails when its evidence is removed or
contradicted.

    python tools/wifi/test_ui_judge.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import ui_judge as judge  # noqa: E402

PASSPHRASE = "lazy-wpa2-pass"
SERIAL = """\
NETAPP:UP:PASS
NETAPP:WIFI:UP if=wlan0
NETAPP:WIFI:OPEN if=wlan0
NETAPP:WIFI:LIST count=5
NETAPP:WIFI:PROBE:list
NETAPP:WIFI:JOIN:OPEN known
NETAPP:WIFI:PROBE:join
NETAPP:WIFI:CONNECT ssid=LazyWPA2
NETAPP:WIFI:STATE Connected ssid=LazyWPA2
NETAPP:WIFI:CONNECTED ssid=LazyWPA2
NETAPP:WIFI:CONNECT ssid=LazyWrong
NETAPP:WIFI:FAILED reason=the password is probably wrong ssid=LazyWrong
NETAPP:WIFI:JOIN:OPEN other
NETAPP:WIFI:CONNECT ssid=LazyHidden
NETAPP:WIFI:CONNECTED ssid=LazyHidden
NETAPP:WIFI:FORGET ssid=LazyHidden
ELEVD:REQUEST op=net.wifi.add uid=1000 label=7 session=1 admin=admin outcome=granted summary="x"
WLANMD:ADD w0123456789ab owner=system
NETAPP:WIFI:CONNECT ssid=LazyRoam
NETAPP:WIFI:CONNECTED ssid=LazyRoam
ELEVD:REQUEST op=net.wifi.forget uid=1000 label=7 session=1 admin=admin outcome=granted summary="y"
NETAPP:WIFI:FORGET ssid=LazyRoam
"""
AP = """\
AP:AUTHORIZED ssid=LazyWPA2 sta=02:57:53:49:4d:00 security=wpa2
AP:AUTHORIZED ssid=LazyHidden sta=02:57:53:49:4d:00 security=open
AP:AUTHORIZED ssid=LazyRoam sta=02:57:53:49:4d:00 security=wpa2
"""


def run(serial: str = SERIAL, ap: str = AP, shots: list | None = None) -> dict[str, list[str]]:
    verdict = judge.judge(serial, ap, shots or [], PASSPHRASE)
    verdict.pop("the screenshots are real pictures")
    return verdict


class UiJudgeTest(unittest.TestCase):
    def failed(self, verdict: dict[str, list[str]]) -> list[str]:
        return [name for name, problems in verdict.items() if problems]

    def test_a_good_run_passes(self) -> None:
        self.assertEqual(self.failed(run()), [])

    def test_each_marker_matters(self) -> None:
        for needle, check in [
            ("NETAPP:WIFI:UP if=wlan0\n", "the page opens"),
            ("NETAPP:WIFI:LIST count=5\n", "the list shows the networks heard"),
            ("NETAPP:WIFI:CONNECTED ssid=LazyWPA2\n", "the password form joins a WPA2 network"),
            ("NETAPP:WIFI:JOIN:OPEN other\n", "Other network saves and joins a hidden network"),
            ("NETAPP:WIFI:FAILED reason=the password is probably wrong ssid=LazyWrong\n",
             "a wrong password is refused with a reason"),
            ("NETAPP:WIFI:FORGET ssid=LazyHidden\n", "forget removes a saved network"),
        ]:
            self.assertIn(check, self.failed(run(SERIAL.replace(needle, ""))), needle)

    def test_the_ap_must_have_authorized_the_station(self) -> None:
        ap = AP.replace("ssid=LazyWPA2", "ssid=LazyNope")
        self.assertEqual(self.failed(run(ap=ap)), ["the password form joins a WPA2 network"])
        ap = AP.replace("ssid=LazyRoam", "ssid=LazyNope")
        self.assertEqual(self.failed(run(ap=ap)), ["an administrator approves a network for all users"])

    def test_a_refused_system_request_fails(self) -> None:
        refused = SERIAL.replace("admin=admin outcome=granted", "admin= outcome=denied", 1)
        self.assertEqual(self.failed(run(refused)), ["an administrator approves a network for all users"])

    def test_a_small_list_fails(self) -> None:
        self.assertEqual(
            self.failed(run(SERIAL.replace("count=5", "count=2"))),
            ["the list shows the networks heard"],
        )

    def test_a_password_in_a_log_fails(self) -> None:
        self.assertEqual(self.failed(run(SERIAL + f"typed {PASSPHRASE}\n")), ["no password is logged"])
        self.assertEqual(self.failed(run(ap=AP + f"x {PASSPHRASE}\n")), ["no password is logged"])

    def test_a_refused_call_for_the_app_fails(self) -> None:
        deny = "LABEL:DENY label=os.lazy.network iface=1234 method=2\n"
        self.assertEqual(self.failed(run(SERIAL + deny)), ["no refused call for the app"])
        other = "LABEL:DENY label=os.lazy.volume iface=1 method=2\n"
        self.assertEqual(self.failed(run(SERIAL + other)), [])

    def test_missing_screenshots_fail(self) -> None:
        verdict = judge.judge(SERIAL, AP, [], PASSPHRASE)
        self.assertEqual(verdict["the screenshots are real pictures"], ["no screenshots were taken"])


if __name__ == "__main__":
    unittest.main()
