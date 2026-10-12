"""The verdict of the Wi-Fi tray and Settings country session
(`tray_run.py`, `tools/screenshot/examples/wifi_tray.json`): what the Network
Status applet, the tray, `elevd`, `wlanmd` and the fake access point printed.

As in `sim_judge.py` the AP's log is the proof of a join; the applet's own
markers say what it showed and did. Pure functions over the serial log and
the AP's lines, so `test_tray_judge.py` can show each check fails when it
should. Each check returns a list of problems (empty: passed).
"""

from __future__ import annotations

import re

from sim_judge import FAIL_MARKERS, OPEN, WPA2, Run, _missing

#: The applet's own evidence prefix.
APP = "os.lazy.netstatus"
#: Lines that mean a package's permissions are incomplete.
DENIED = "LABEL:DENY"


def _after(serial: str, marker: str, start: int = 0) -> int:
    """Index just past the first `marker` at or after `start`, else -1."""
    at = serial.find(marker, start)
    return -1 if at < 0 else at + len(marker)


def _applet_lines(run: Run) -> list[str]:
    return [line[line.index("NETSTATUS:"):] for line in run.serial.splitlines() if "NETSTATUS:" in line]


def check_boot(run: Run) -> list[str]:
    problems = _missing(run, ("WLANMD:RADIO:UP wlan0", "NETSTATUS:UP:PASS", "NETSTATUS:WIFI:UP wlan0"))
    problems += [f"failure marker {marker!r}" for marker in FAIL_MARKERS if marker in run.serial]
    if DENIED in run.serial:
        denied = sorted({line.strip() for line in run.serial.splitlines() if DENIED in line})
        problems.append("a package call was refused: " + "; ".join(denied[:4]))
    return problems


def check_idle(run: Run) -> list[str]:
    """Before anything is joined the applet shows the disconnected icon."""
    problems = _missing(run, ("NETSTATUS:ICON disconnected", "NETSTATUS:STATE Wi-Fi: not connected"))
    icons = [line for line in _applet_lines(run) if line.startswith("NETSTATUS:ICON ")]
    if icons and not icons[0].startswith("NETSTATUS:ICON disconnected"):
        problems.append(f"the first Wi-Fi icon was {icons[0]!r}, not the disconnected one")
    return problems


def check_menu(run: Run) -> list[str]:
    """The menu lists the fake networks and the tray opened it."""
    problems = _missing(run, ("SHELL:TRAY:MENU:OPEN app=os.lazy.netstatus",))
    lists = [line for line in _applet_lines(run) if line.startswith("NETSTATUS:WIFI:NETWORKS")]
    counts = [int(m.group(1)) for line in lists if (m := re.search(r"listed=(\d+)", line))]
    if not counts or max(counts) < 3:
        problems.append(f"the menu never listed the fake networks (listed={counts})")
    for ssid in (OPEN, WPA2):
        if f"name=traymenu:{ssid}  (" not in run.serial:
            problems.append(f"the menu has no row for {ssid}")
    if "Disconnect" not in run.serial or "name=traymenu:Network settings..." not in run.serial:
        problems.append("the menu lacks its Disconnect or Network settings row")
    return problems


def check_open_join(run: Run) -> list[str]:
    """A click on an open, unsaved network saves it, joins it and the icon
    shows bars; the AP saw the station authorized and leased it an address."""
    problems = _missing(
        run, (f"NETSTATUS:WIFI:ACTION join-open ssid={OPEN}", "NETSTATUS:WIFI:JOIN:PASS"),
        (("AP:DHCP:ACK", OPEN),))
    if not any(line.startswith(f"AP:AUTHORIZED ssid={OPEN} ") for line in run.ap):
        problems.append(f"the AP never authorized the station on {OPEN}")
    start = _after(run.serial, f"NETSTATUS:WIFI:ACTION join-open ssid={OPEN}")
    if start >= 0:
        after = run.serial[start:]
        if not re.search(r"NETSTATUS:ICON connecting\d", after):
            problems.append("the icon never showed the joining animation")
        if not re.search(r"NETSTATUS:ICON bars[1-4]", after):
            problems.append("the icon never showed bars once connected")
        if f"NETSTATUS:STATE Wi-Fi: connected to {OPEN}," not in after:
            problems.append("the tooltip never named the network joined")
    return problems


def check_secured(run: Run) -> list[str]:
    """A secured network that is not saved opens the Network app (the tray
    never asks for a passphrase)."""
    problems = _missing(run, ("NETSTATUS:WIFI:ACTION configure", "NETSTATUS:WIFI:OPEN:PASS"))
    if "INIT:LAUNCH:PASS app=os.lazy.network" not in run.serial:
        problems.append("init never launched the Network app")
    return problems


def check_disconnect(run: Run) -> list[str]:
    problems = _missing(run, ("NETSTATUS:WIFI:ACTION disconnect", "NETSTATUS:WIFI:DISCONNECT:PASS"))
    start = _after(run.serial, "NETSTATUS:WIFI:ACTION disconnect")
    if start >= 0 and "NETSTATUS:ICON disconnected" not in run.serial[start:]:
        problems.append("the icon did not go back to disconnected")
    return problems


def check_country(run: Run) -> list[str]:
    """Settings' country change went through elevd's own operation, was
    granted on the administrator's password, and reached the radio."""
    problems = _missing(run, ("SETTINGS:WIFI:COUNTRY:FR", "WLANMD:RADIO:COUNTRY:SET wlan0 FR"))
    granted = re.search(
        r'ELEVD:REQUEST op=net\.wifi\.country .*outcome=granted summary="Set the Wi-Fi country to France \(FR\)',
        run.serial)
    if not granted:
        problems.append("elevd never granted net.wifi.country for France (FR)")
    if re.search(r'op=conf\.set .*sys/net/wifi/country', run.serial):
        problems.append("the country went through conf.set instead of net.wifi.country")
    return problems


CHECKS = {
    "boot": check_boot,
    "disconnected icon": check_idle,
    "menu": check_menu,
    "open join": check_open_join,
    "secured network opens Network": check_secured,
    "disconnect": check_disconnect,
    "country": check_country,
}


def judge(run: Run) -> dict[str, list[str]]:
    return {name: check(run) for name, check in CHECKS.items()}
