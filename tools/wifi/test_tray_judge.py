#!/usr/bin/env python3
"""The tray judge (`tray_judge.py`) fails when it should: a good run passes
every check, and each check fails when its evidence is removed or
contradicted.

    python tools/wifi/test_tray_judge.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import tray_judge as judge  # noqa: E402
from sim_judge import Run  # noqa: E402

SERIAL = """\
WLANMD:RADIO:UP wlan0 mac=02:57:53:49:4d:00 country=00
NETSTATUS:UP:PASS
NETSTATUS:WIFI:UP wlan0
NETSTATUS:STATE Wi-Fi: not connected
NETSTATUS:ICON disconnected
NETSTATUS:WIFI:NETWORKS count=5 listed=4
SHELL:TRAY:MENU:OPEN app=os.lazy.netstatus rows=10
UI:RECT x=1 y=2 w=3 h=4 name=traymenu:LazyOpen  (open, 4 of 4 bars)
UI:RECT x=1 y=2 w=3 h=4 name=traymenu:LazyWPA2  (locked, 4 of 4 bars)
UI:RECT x=1 y=2 w=3 h=4 name=traymenu:Disconnect
UI:RECT x=1 y=2 w=3 h=4 name=traymenu:Network settings...
NETSTATUS:WIFI:ACTION join-open ssid=LazyOpen
NETSTATUS:WIFI:JOIN:PASS
NETSTATUS:ICON connecting0
NETSTATUS:STATE Wi-Fi: connecting to LazyOpen...
NETSTATUS:ICON connecting2
NETSTATUS:ICON bars4
NETSTATUS:STATE Wi-Fi: connected to LazyOpen, signal excellent (-40 dBm), 2.4 GHz, 192.168.77.100
NETSTATUS:WIFI:ACTION configure
NETSTATUS:WIFI:OPEN:PASS
INIT:LAUNCH:PASS app=os.lazy.network pid=40
NETSTATUS:WIFI:ACTION disconnect
NETSTATUS:WIFI:DISCONNECT:PASS
NETSTATUS:ICON disconnected
SETTINGS:WIFI:COUNTRY:FR
ELEVD:REQUEST op=net.wifi.country uid=1000 admin=admin outcome=granted summary="Set the Wi-Fi country to France (FR), which limits the radio's channels"
WLANMD:RADIO:COUNTRY:SET wlan0 FR
"""
AP = ["AP:AUTHORIZED ssid=LazyOpen sta=02:57:53:49:4d:00 security=open", "AP:DHCP:ACK ip=192.168.77.100 ssid=LazyOpen"]


def run(serial: str = SERIAL, ap: list[str] | None = None) -> Run:
    return Run(serial, AP if ap is None else ap, [])


def without(*lines: str) -> str:
    text = SERIAL
    for line in lines:
        assert line in text, line
        text = text.replace(line + "\n", "")
    return text


class TrayJudge(unittest.TestCase):
    def test_a_good_run_passes_everything(self):
        for name, problems in judge.judge(run()).items():
            self.assertEqual(problems, [], name)

    def test_boot_needs_the_radio_the_applet_and_no_denials(self):
        self.assertTrue(judge.check_boot(run(without("NETSTATUS:WIFI:UP wlan0"))))
        self.assertTrue(judge.check_boot(run(SERIAL + "LABEL:DENY label=os.lazy.netstatus iface=1 method=2\n")))
        self.assertTrue(judge.check_boot(run(SERIAL + "PANIC: x\n")))

    def test_the_first_icon_must_be_the_disconnected_one(self):
        self.assertTrue(judge.check_idle(run(without("NETSTATUS:ICON disconnected", "NETSTATUS:STATE Wi-Fi: not connected"))))
        self.assertTrue(judge.check_idle(run("NETSTATUS:ICON bars3\n" + SERIAL)))

    def test_the_menu_must_list_the_networks_and_its_actions(self):
        self.assertTrue(judge.check_menu(run(without("NETSTATUS:WIFI:NETWORKS count=5 listed=4"))))
        self.assertTrue(judge.check_menu(run(SERIAL.replace("listed=4", "listed=1"))))
        self.assertTrue(judge.check_menu(run(without("UI:RECT x=1 y=2 w=3 h=4 name=traymenu:LazyWPA2  (locked, 4 of 4 bars)"))))
        self.assertTrue(judge.check_menu(run(without("UI:RECT x=1 y=2 w=3 h=4 name=traymenu:Network settings..."))))

    def test_the_join_is_proved_by_the_ap_and_shown_by_the_icon(self):
        self.assertTrue(judge.check_open_join(run(ap=[])))
        self.assertTrue(judge.check_open_join(run(ap=[AP[0]])))
        self.assertTrue(judge.check_open_join(run(without("NETSTATUS:ICON bars4"))))
        self.assertTrue(judge.check_open_join(run(without("NETSTATUS:ICON connecting0", "NETSTATUS:ICON connecting2"))))
        self.assertTrue(judge.check_open_join(run(without(
            "NETSTATUS:STATE Wi-Fi: connected to LazyOpen, signal excellent (-40 dBm), 2.4 GHz, 192.168.77.100"))))
        self.assertTrue(judge.check_open_join(run(without("NETSTATUS:WIFI:JOIN:PASS"))))

    def test_a_secured_network_hands_over_to_the_network_app(self):
        self.assertTrue(judge.check_secured(run(without("NETSTATUS:WIFI:ACTION configure"))))
        self.assertTrue(judge.check_secured(run(without("INIT:LAUNCH:PASS app=os.lazy.network pid=40"))))

    def test_disconnect_must_bring_the_disconnected_icon_back(self):
        self.assertTrue(judge.check_disconnect(run(without("NETSTATUS:WIFI:DISCONNECT:PASS"))))
        # The only disconnected icon is the one before the join.
        early = SERIAL.replace("NETSTATUS:ICON disconnected\nSETTINGS", "SETTINGS")
        self.assertTrue(judge.check_disconnect(run(early)))

    def test_the_country_goes_through_its_own_granted_operation(self):
        self.assertTrue(judge.check_country(run(without("SETTINGS:WIFI:COUNTRY:FR"))))
        self.assertTrue(judge.check_country(run(without("WLANMD:RADIO:COUNTRY:SET wlan0 FR"))))
        denied = SERIAL.replace("outcome=granted", "outcome=denied")
        self.assertTrue(judge.check_country(run(denied)))
        wrong = SERIAL + 'ELEVD:REQUEST op=conf.set admin=admin outcome=granted summary="Set sys/net/wifi/country"\n'
        self.assertTrue(judge.check_country(run(wrong)))


if __name__ == "__main__":
    unittest.main()
