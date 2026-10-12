"""The judge of `ui_run.py`: the Network app's Wi-Fi page, from the guest's
serial log, the fake AP's log and the screenshots.

`judge(serial, ap, shots, passphrase)` returns `{check: [problem, ...]}`; a
check passes when its list is empty. Each check names the evidence it needs,
so a run that skipped a step fails that step and nothing else.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "screenshot"))

import pngstats  # noqa: E402

WPA2 = "LazyWPA2"
HIDDEN = "LazyHidden"
WRONG = "LazyWrong"
#: What a good run must show on the guest's serial console, in order.
FLOW = [
    ("app", "NETAPP:UP:PASS"),
    ("radio", "NETAPP:WIFI:UP if=wlan0"),
    ("page", "NETAPP:WIFI:OPEN if=wlan0"),
    ("probe", "NETAPP:WIFI:PROBE:list"),
]


def _ordered(serial: str, needles: list[str]) -> list[str]:
    """The needles that do not appear, each after the one before."""
    problems, at = [], 0
    for needle in needles:
        found = serial.find(needle, at)
        if found < 0:
            problems.append(f"missing `{needle}`" + (" (or out of order)" if needle in serial else ""))
        else:
            at = found + len(needle)
    return problems


def judge(serial: str, ap: str, shots: list[Path], passphrase: str) -> dict[str, list[str]]:
    ap_lines = ap.splitlines()
    verdict: dict[str, list[str]] = {}
    verdict["the page opens"] = _ordered(serial, [needle for _, needle in FLOW])

    heard = [int(n) for n in re.findall(r"NETAPP:WIFI:LIST count=(\d+)", serial)]
    verdict["the list shows the networks heard"] = (
        [] if heard and max(heard) >= 4 else [f"LIST counts {heard}: expected at least 4 (open, WPA2, roam, wrong)"]
    )

    verdict["the password form joins a WPA2 network"] = _ordered(serial, [
        "NETAPP:WIFI:JOIN:OPEN known",
        "NETAPP:WIFI:PROBE:join",
        f"NETAPP:WIFI:CONNECT ssid={WPA2}",
        f"NETAPP:WIFI:CONNECTED ssid={WPA2}",
    ]) + ([] if any(line.startswith(f"AP:AUTHORIZED ssid={WPA2}") for line in ap_lines)
          else [f"the AP never authorized the station on {WPA2}"])

    verdict["Other network saves and joins a hidden network"] = _ordered(serial, [
        "NETAPP:WIFI:JOIN:OPEN other",
        f"NETAPP:WIFI:CONNECT ssid={HIDDEN}",
        f"NETAPP:WIFI:CONNECTED ssid={HIDDEN}",
    ]) + ([] if any(line.startswith(f"AP:AUTHORIZED ssid={HIDDEN}") for line in ap_lines)
          else [f"the AP never authorized the station on {HIDDEN}"])

    verdict["a wrong password is refused with a reason"] = _ordered(serial, [
        f"NETAPP:WIFI:CONNECT ssid={WRONG}",
        f"NETAPP:WIFI:FAILED reason=the password is probably wrong ssid={WRONG}",
    ])

    verdict["forget removes a saved network"] = _ordered(serial, ["NETAPP:WIFI:FORGET ssid="])

    secrets = []
    for name, text in (("the serial log", serial), ("the AP's log", ap)):
        if passphrase and passphrase in text:
            secrets.append(f"the passphrase is in {name}")
    verdict["no password is logged"] = secrets

    denied = [line.strip() for line in serial.splitlines()
              if "LABEL:DENY" in line and re.search(r"network", line, re.I)]
    verdict["no refused call for the app"] = [f"`{line}`" for line in denied[:5]]

    problems: list[str] = []
    if not shots:
        problems.append("no screenshots were taken")
    for shot in shots:
        _, _, failures = pngstats.analyse_file(str(shot), 1280, 720, 0.01, 8, None)
        problems += failures
    verdict["the screenshots are real pictures"] = problems
    return verdict
