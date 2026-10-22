#!/usr/bin/env python3
"""Drive the Network app's Wi-Fi page against the simulated radio and judge it
(docs/wifi-prerequisites-plan.md WP5, section 3.10).

1. `fake_ap.py` serves the simulated access points (open, WPA2, hidden, a
   roaming pair, one nobody has the passphrase of).
2. A desktop image with the stack, the station manager and the simulated
   radio (`LAZYOS_DESKTOP=1 LAZYOS_NETD=1 LAZYOS_WIFI=1 LAZYOS_WIFI_SIM=1`;
   `LAZYOS_WIFI=1` also embeds the RTL8822CE firmware, so run
   `python tools/firmware/fetch.py --switch wifi` once), the UI probe and the
   label trace, opening the Network app at login.
3. A session switches to the Wi-Fi tab and, by widget name: joins the open
   network, then the WPA2 one with a wrong and with the right passphrase,
   disconnects, forgets it, rejoins from the stored secret with an empty
   passphrase box, and joins the hidden open network by name (an empty
   passphrase alone is refused).
4. The verdict: the app's `NETAPP:WIFI:*` markers (the scan listed the
   networks, each join ended in the state it should, the wrong passphrase
   failed as a bad passphrase), the access points' log (the station really
   associated), no passphrase on the serial line, no `LABEL:DENY` for the app
   and screenshots that are not black.

    python tools/wifi/ui_run.py               # build, boot, judge
    python tools/wifi/ui_run.py --no-build    # reuse target/lazyos.img

Logs, the generated session and the screenshots go to `shots/wifi_ui/`.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import simnet  # noqa: E402

PY = sys.executable
SESSION = ROOT / "tools" / "screenshot" / "qemu_session.py"
PNGSTATS = ROOT / "tools" / "screenshot" / "pngstats.py"
IMAGE = ROOT / "target" / "lazyos.img"
OUT = ROOT / "shots" / "wifi_ui"
WINDOW = "Network"
WPA2_PASSPHRASE = "lazy-wpa2-pass"
WRONG_PASSPHRASE = "definitely-not-it"
#: Where the "Wi-Fi" tab header is, from the centre of the tab control (the
#: window is 640 wide; the header row is the control's top edge).
TAB_OFFSET = [-237, -287]
#: A row of the networks table from the table's centre: the table is the page's
#: fill, so its rows are measured from the top (a header of ~24, rows of ~22).
ROW_HEIGHT = 22
FAIL_ON = (r"panicked", r"user: task [0-9]+ killed by", r"NETAPP:BUILD:FAIL", r"WLANMD:FAIL panic")


def click(widget: str, until: str | None = None, timeout: int = 60, offset=None) -> list[dict]:
    target: dict = {"window": WINDOW, "widget": widget}
    if offset:
        target["offset"] = offset
    step: dict = {"click_at": target, "timeout": 30}
    if until:
        step.update({"until": until, "timeout": timeout, "retries": 1})
    return [step]


def fill(widget: str, text: str) -> list[dict]:
    return click(widget) + [{"key_down": "ctrl"}, {"key": "a"}, {"key_up": "ctrl"}, {"type": text}]


def network_row(index: int, networks_height: int = 326) -> list[dict]:
    """Select the `index`th row of the networks table (0 is the strongest)."""
    top = -networks_height // 2 + 24 + ROW_HEIGHT // 2 + index * ROW_HEIGHT
    return click("wifi_networks", offset=[-100, top])


def session() -> list[dict]:
    state = lambda conn, ssid="": f'NETAPP:WIFI:STATE if=wlan0 state={conn}' + (f' ssid="{ssid}"' if ssid else "")
    action = lambda what, n: {"wait_for": rf"NETAPP:WIFI:{what} .*n={n}(?!\d)", "regex": True, "timeout": 60}
    # A join marker names the network, so a click on the wrong row fails here.
    join = lambda n, ssid: {"wait_for": rf'NETAPP:WIFI:JOIN:REQUESTED id=\S+ ssid="{ssid}" n={n}(?!\d)',
                            "regex": True, "timeout": 60}
    steps: list[dict] = [
        {"wait_for": "PKGD:PROVISION:DONE", "timeout": 420},
        {"wait_for": "NETAPP:UP:PASS", "timeout": 180},
        {"wait_for": "NETAPP:WIFI:UP if=wlan0", "timeout": 60},
        {"wait_for": "name=tabs window=" + WINDOW, "timeout": 60},
        {"click_at": {"window": WINDOW, "widget": "tabs", "offset": TAB_OFFSET}},
        {"wait_for": "name=wifi_join_hidden window=" + WINDOW, "timeout": 60},
        {"wait_for": "NETAPP:WIFI:SCAN:PASS n=5", "timeout": 120},
        {"wait": 2.0},
        {"shot": "01_scan"},
    ]
    # The strongest network is the open one: no passphrase needed. (n=1)
    steps += network_row(0)
    steps += click("wifi_join")
    steps += [join(1, "LazyOpen"),
              {"wait_for": state("connected", "LazyOpen"), "timeout": 120}, {"wait": 2.0}, {"shot": "02_open"}]
    steps += click("wifi_leave") + [action("LEAVE:PASS", 2)]
    # WPA2 with a wrong passphrase: the handshake fails as a bad passphrase.
    steps += network_row(1)
    steps += fill("wifi_passphrase", WRONG_PASSPHRASE)
    steps += click("wifi_join") + [join(3, "LazyWPA2")]
    steps += [{"wait_for": state("failed", "LazyWPA2"), "timeout": 180}, {"wait": 2.0}, {"shot": "03_wrong"}]
    # The profile holds the wrong secret now: forget it and start over.
    steps += click("wifi_forget") + [action("FORGET:PASS", 4)]
    steps += network_row(1)
    steps += fill("wifi_passphrase", WPA2_PASSPHRASE)
    steps += click("wifi_join") + [join(5, "LazyWPA2")]
    steps += [{"wait_for": state("connected", "LazyWPA2"), "timeout": 180}, {"wait": 2.0}, {"shot": "04_wpa2"}]
    # The stored secret, not the typed one: leave, and join again with the
    # passphrase box empty. It must connect from what `keyd` kept.
    steps += click("wifi_leave") + [action("LEAVE:PASS", 6), {"wait": 2.0}]
    steps += network_row(1)
    steps += click("wifi_join") + [join(7, "LazyWPA2")]
    steps += [{"wait_for": rf'NETAPP:WIFI:STATE if=wlan0 state=connected ssid="LazyWPA2"', "timeout": 180}]
    steps += [{"wait": 3.0}, {"shot": "05_rejoined_from_stored"}]
    # A hidden open network by name: the name box and the Open tick (an empty
    # passphrase alone is refused, never saved as open).
    steps += click("wifi_leave") + [action("LEAVE:PASS", 8), {"wait": 2.0}]
    steps += fill("wifi_hidden", "LazyHidden")
    steps += click("wifi_join_hidden") + [{"wait_for": "NETAPP:WIFI:JOIN:REFUSED Type the passphrase", "timeout": 30}]
    steps += click("wifi_open") + click("wifi_join_hidden") + [join(9, "LazyHidden")]
    steps += [{"wait_for": 'NETAPP:WIFI:STATE if=wlan0 state=connected ssid="LazyHidden"', "timeout": 180}]
    steps += [{"wait": 3.0}, {"shot": "05b_hidden"}]
    # `quit` kills QEMU without a sync: let the block cache's flusher (5 s)
    # write `keyd`'s secrets before the host reads the volume.
    steps += [{"wait": 8.0}, {"quit": True}]
    return steps


def second_boot() -> list[dict]:
    """After a reboot (the OS volume kept): the saved networks are still
    there, and the WPA2 one joins from the secret `keyd` kept on disk, with
    nothing typed."""
    action = lambda what, n: {"wait_for": rf"NETAPP:WIFI:{what} .*n={n}(?!\d)", "regex": True, "timeout": 60}
    steps: list[dict] = [
        {"wait_for": "PKGD:PROVISION:DONE", "timeout": 420},
        {"wait_for": "NETAPP:UP:PASS", "timeout": 180},
        {"wait_for": "name=tabs window=" + WINDOW, "timeout": 60},
        {"click_at": {"window": WINDOW, "widget": "tabs", "offset": TAB_OFFSET}},
        {"wait_for": "NETAPP:WIFI:SCAN:PASS n=5", "timeout": 120},
        {"wait": 3.0},
        {"shot": "06_after_reboot"},
    ]
    # The station may have auto-connected to a saved network: leave it first.
    steps += click("wifi_leave") + [action("LEAVE:PASS", 1), {"wait": 2.0}]
    steps += network_row(1)
    steps += click("wifi_join")
    steps += [{"wait_for": rf'NETAPP:WIFI:JOIN:REQUESTED id=\S+ ssid="LazyWPA2" n=2(?!\d)', "regex": True, "timeout": 60}]
    steps += [{"wait_for": 'NETAPP:WIFI:STATE if=wlan0 state=connected ssid="LazyWPA2"', "timeout": 180}]
    steps += [{"wait": 3.0}, {"shot": "07_rejoined_after_reboot"}, {"wait": 8.0}, {"quit": True}]
    return steps


def build() -> str | None:
    if subprocess.call([PY, "tools/xui/build.py"], cwd=ROOT) != 0:
        return "tools/xui/build.py failed"
    env = dict(os.environ, LAZYOS_RESET_OS="1", LAZYOS_DESKTOP="1", LAZYOS_XUI_AUTOSTART="network",
               LAZYOS_NETD_ARGS="demo=0", LAZYOS_WIFI="1", LAZYOS_UI_PROBE="1", LAZYOS_LABEL_TRACE="1", **simnet.build_env())
    print("wifi ui: desktop + stack + simulated radio + UI probe: cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(result.stderr[-4000:])
        return "cargo build failed"
    return None if IMAGE.is_file() else f"{IMAGE} was not written"


#: Markers the session must have produced, in words for a failure.
EXPECT = (
    (r"NETAPP:WIFI:UP if=wlan0", "the Wi-Fi page never found the radio"),
    (r"NETAPP:WIFI:SCAN:PASS n=[1-9]", "the scan listed no network"),
    (r'NETAPP:WIFI:STATE if=wlan0 state=connected ssid="LazyOpen"', "the open network never connected"),
    (r"NETAPP:WIFI:LEAVE:PASS n=2", "Disconnect failed"),
    (r'NETAPP:WIFI:STATE if=wlan0 state=failed ssid="LazyWPA2"', "the wrong passphrase did not fail"),
    (r"NETAPP:WIFI:FORGET:PASS id=\S+ n=4", "Forget failed"),
    (r'NETAPP:WIFI:JOIN:REQUESTED id=\S+ ssid="LazyWPA2" n=7', "the saved network was not joined again"),
    (r'NETAPP:WIFI:JOIN:REFUSED Type the passphrase', "an empty passphrase was not refused for a hidden network"),
    (r'NETAPP:WIFI:STATE if=wlan0 state=connected ssid="LazyHidden"', "the hidden network never connected"),
    (r'NETAPP:WIFI:STATE if=wlan0 state=connected ssid="LazyWPA2"', "WPA2 never connected"),
)


def stored_secret_problems() -> list[str]:
    """Read `keyd`'s secret file off the OS volume: it must exist, name the
    network's profile and hold neither passphrase in the clear (it is sealed
    under the machine key)."""
    result = subprocess.run(
        ["cargo", "run", "-q", "-p", "ext2fs", "--example", "osread", "--", str(IMAGE), "cat",
         "/conf/svc/keyd/secrets"], cwd=ROOT, capture_output=True)
    data = result.stdout
    if result.returncode != 0 or not data:
        return ["keyd's secret file is missing or empty on the OS volume"]
    problems = []
    for secret in (WPA2_PASSPHRASE, WRONG_PASSPHRASE):
        if secret.encode() in data:
            problems.append("a passphrase is stored in the clear in /conf/svc/keyd/secrets")
    print(f"WIFI:UI:SECRETS file={len(data)} bytes, sealed")
    return problems


def judge(serial: str, ap_log: str) -> list[str]:
    problems = [why for pattern, why in EXPECT if not re.search(pattern, serial)]
    for secret in (WPA2_PASSPHRASE, WRONG_PASSPHRASE):
        if secret in serial:
            problems.append("a passphrase reached the serial log")
    problems += [f"unexpected: {m.group(0)}" for pattern in FAIL_ON for m in [re.search(pattern, serial)] if m]
    denied = [line for line in serial.splitlines() if "LABEL:DENY" in line and "os.lazy.net" in line]
    problems += [f"label denial: {line.strip()}" for line in denied[:5]]
    if "LazyWPA2" not in ap_log:
        problems.append("the access point never saw the station on LazyWPA2")
    return problems


def run_session(name: str, steps: list[dict], out: Path, ap_log: Path, pcap: Path, accel: str) -> int:
    """Start the fake access points and run one session against the image."""
    out.mkdir(parents=True, exist_ok=True)
    script = out / name
    script.write_text(json.dumps(steps, indent=1))
    ap_log.unlink(missing_ok=True)
    pcap.unlink(missing_ok=True)
    ap, air = simnet.start(log=ap_log, pcap=pcap)
    try:
        command = [PY, str(SESSION), "--image", str(IMAGE), "--out", str(out), "--script", str(script),
                   "--accel", accel, "--wait-timeout", "300"]
        for pattern in FAIL_ON:
            command += ["--fail-on", pattern]
        for extra in air:
            command.append(f"--extra-arg={extra}")
        return subprocess.call(command, cwd=ROOT)
    finally:
        simnet.stop(ap)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--accel", default="auto")
    args = parser.parse_args()
    if not args.no_build:
        why = build()
        if why:
            print(f"WIFI:UI:FAIL: {why}", file=sys.stderr)
            return 1
    ap_log, pcap = OUT / "ap.log", OUT / "air.pcap"
    for path in (ap_log, pcap):
        path.unlink(missing_ok=True)
    code = run_session("session.json", session(), OUT, ap_log, pcap, args.accel)
    serial = (OUT / "serial.log").read_text(errors="replace") if (OUT / "serial.log").is_file() else ""
    ap_text = ap_log.read_text(errors="replace") if ap_log.is_file() else ""
    problems = judge(serial, ap_text)
    problems += stored_secret_problems()
    if code != 0:
        problems.append(f"the session exited with {code}")
    out2 = OUT / "reboot"
    code2 = run_session("session.json", second_boot(), out2, out2 / "ap.log", out2 / "air.pcap", args.accel)
    serial2 = (out2 / "serial.log").read_text(errors="replace") if (out2 / "serial.log").is_file() else ""
    if code2 != 0:
        problems.append(f"the second boot's session exited with {code2}")
    if not re.search(r'NETAPP:WIFI:STATE if=wlan0 state=connected ssid="LazyWPA2"', serial2):
        problems.append("after a reboot the saved WPA2 network did not join from the stored secret")
    for secret in (WPA2_PASSPHRASE, WRONG_PASSPHRASE):
        if secret in serial2:
            problems.append("a passphrase reached the serial log (second boot)")
    problems += stored_secret_problems()
    shots = sorted(OUT.glob("shot_*.png")) + sorted(out2.glob("shot_*.png"))
    if not shots:
        problems.append("no screenshots")
    elif subprocess.call([PY, str(PNGSTATS), *map(str, shots), "--min-nonblack", "0.01"]) != 0:
        problems.append("a screenshot is black")
    for problem in problems:
        print(f"WIFI:UI:FAIL: {problem}")
    print("WIFI:UI:PASS" if not problems else f"WIFI:UI: {len(problems)} problem(s)")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
