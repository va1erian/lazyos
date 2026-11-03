#!/usr/bin/env python3
"""Judge the pairing-interface serial logs (docs/bluetooth-plan.md, phase B5).

The verdict is what the interface did, from both ends: what the client (the
Settings window, or `btctl` in the Terminal) saw and asked, and what `btd` and
`inputd` did about it. The modes (`tools/bt/run.py`, `tools/bt/modes_ui.py`):

* `ui-pair`: Settings -> Bluetooth scans, finds the LE keyboard in pairing
  mode, and pairs it on a click. Nothing pairs before the click (a harness
  build with no pairing window), the passkey the dialog shows comes from `btd`
  (the window never prints it, `btd` only in a harness build), the keyboard is
  bonded with an authenticated key, and its text then reaches `inputd`.
* `btctl`: the same from the shell: `btctl scan` lists the keyboard, `btctl
  pair` prints the passkey `btd` showed and ends paired, `btctl list` shows
  the device paired, `btctl forget` removes it and the next `list` no longer
  does, and `btd` logged the bond gone.

    python tools/bt/judge_ui.py ui-pair shots/bt/serial.log --text pairedui
    python tools/bt/judge_ui.py btctl shots/bt/serial.log --text pairedui
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import judge_hid as hid  # noqa: E402

KBD_TAIL = "01:01"
KBD_FULL = "C0:FF:EE:00:01:01"
MSG = "SETTINGS:MSG:"
PAIRING = MSG + "Bt(Event(Pairing(BtPairing { phase: "
#: What a refused call or a missing permission leaves in the log.
DENIED = re.compile(r"LABEL:DENY.*os\.lazy\.bt|BT:API:DENY")
PASSKEY = re.compile(r"BT:PAIR:PASSKEY value=(\d{6}) addr=xx:xx:xx:xx:" + KBD_TAIL)
TERM_OUT = re.compile(r"TERM:OUT:BTCTL:(\w+):(\d+):(.*)")


def ordered(log: str, steps: list[tuple[str, str]]) -> list[str]:
    """Each needle appears, and after the one before it."""
    failures, last = [], -1
    for name, needle in steps:
        at = hid.position(log, needle, last + 1)
        if at < 0:
            failures.append(f"{name}: {needle!r} not found after the previous step")
            continue
        last = at
    return failures


def common(log: str) -> list[str]:
    """Driver errors, addresses, refusals and the passkey's whereabouts."""
    failures = hid.judge_common(log)
    for bad in ("BT:GATT:FAIL", "BT:PAIR:FAIL", "BT:ENCRYPT:FAIL", "BT:REFUSE"):
        if bad in log:
            failures.append(f"{bad} appears")
    for line in log.splitlines():
        if DENIED.search(line):
            failures.append(f"a call or a permission was denied: {line.strip()}")
    # No silent pairing: nothing connected to the keyboard before a caller
    # asked (`BT:API:PAIR`), and the harness image opened no pairing window.
    asked = hid.position(log, "BT:API:PAIR")
    if asked < 0:
        failures.append("btd never got a Pair call")
    else:
        for early in ("BT:CONN addr=xx:xx:xx:xx:" + KBD_TAIL, "BT:PAIR:START", "BT:PAIR:PASS"):
            at = hid.position(log, early)
            if 0 <= at < asked:
                failures.append(f"{early!r} before the Pair call: a silent pairing")
    return failures


def passkey_leaks(log: str) -> list[str]:
    """The passkey is traced only by `btd`'s harness build, as its own line:
    no client, and no other `btd` line, may carry it."""
    shown = PASSKEY.findall(log)
    if not shown:
        return ["btd never showed a passkey (BT:PAIR:PASSKEY)"]
    value = shown[0]
    failures = []
    for line in log.splitlines():
        if value in line and "BT:PAIR:PASSKEY" not in line and "TERM:OUT" not in line \
                and "BT:REPORT" not in line and "INPUTD:" not in line and "BT:KEY" not in line \
                and not line.startswith("TERM:CMD"):
            failures.append(f"the passkey is in a line that is not its own: {line.strip()[:120]}")
    return failures


def judge_ui_pair(log: str, text: str) -> list[str]:
    failures = common(log)
    failures += ordered(log, [
        ("the window starts", "SETTINGS:UP:PASS"),
        ("the Bluetooth page opens", MSG + "Section(9)"),
        ("the service answers", MSG + "Bt(Event(Snapshot"),
        ("a scan is asked", MSG + "Bt(Scan)"),
        ("a device is found", MSG + "Bt(Event(Device {"),
        ("the device is selected", MSG + "Bt(Select(0))"),
        ("Pair is clicked", MSG + "Bt(Pair)"),
        ("btd got the call", "BT:API:PAIR addr=xx:xx:xx:xx:" + KBD_TAIL),
        ("the connection", f"BT:CONN addr=xx:xx:xx:xx:{KBD_TAIL}"),
        ("SMP starts", f"BT:PAIR:START addr=xx:xx:xx:xx:{KBD_TAIL}"),
        ("the link is encrypted", f"BT:ENCRYPT addr=xx:xx:xx:xx:{KBD_TAIL}"),
        ("the bond is made", f"BT:PAIR:PASS addr=xx:xx:xx:xx:{KBD_TAIL} auth=1 class=keyboard"),
        ("the source opens", f"BT:HID:KBD addr=xx:xx:xx:xx:{KBD_TAIL} auth=1"),
    ])
    failures += passkey_leaks(log)
    # The window's lines and `btd`'s own travel on different paths, so they are
    # not ordered against each other: the passkey is shown after SMP starts and
    # before the bond is made, and the window says paired only after it.
    start = hid.position(log, f"BT:PAIR:START addr=xx:xx:xx:xx:{KBD_TAIL}")
    bond = hid.position(log, f"BT:PAIR:PASS addr=xx:xx:xx:xx:{KBD_TAIL}")
    shown = hid.position(log, PAIRING + "ShowPasskey", start)
    done = hid.position(log, PAIRING + "Paired", bond if bond >= 0 else 0)
    if not 0 <= start <= shown:
        failures.append("the window never showed the passkey after the pairing started")
    if done < 0 or done < shown:
        failures.append("the window never said paired after it showed the passkey and the bond was made")
    start = hid.position(log, "BT:HID:KBD")
    if start >= 0:
        failures += hid.judge_typed(log, text, start)
    return failures


def outputs(log: str) -> dict[str, tuple[int, str]]:
    """What each wrapped `btctl` command printed: `{what: (status, text)}`."""
    return {what: (int(rc), out) for what, rc, out in TERM_OUT.findall(log)}


def judge_btctl(log: str, text: str) -> list[str]:
    failures = common(log)
    out = outputs(log)
    for what in ("LIST1", "SCAN", "PAIR", "LIST2", "FORGET", "LIST3"):
        if what not in out:
            failures.append(f"btctl {what} printed nothing the Terminal reported")
    if failures:
        return failures
    for what in ("LIST1", "SCAN", "PAIR", "LIST2", "FORGET", "LIST3"):
        if out[what][0] != 0:
            failures.append(f"btctl {what} exited {out[what][0]}: {out[what][1]}")
    if KBD_FULL in out["LIST1"][1]:
        failures.append("the first list already shows the keyboard")
    if f"{KBD_FULL}  found" not in out["SCAN"][1]:
        failures.append(f"scan did not find the keyboard: {out['SCAN'][1]!r}")
    shown = PASSKEY.findall(log)
    pair = out["PAIR"][1]
    if not shown or f"passkey: {shown[0]}" not in pair:
        failures.append(f"pair did not print the passkey btd showed: {pair!r}")
    if f"paired: {KBD_FULL}" not in pair:
        failures.append(f"pair did not end paired: {pair!r}")
    if not re.search(rf"{KBD_FULL}  (paired|connected)  le keyboard", out["LIST2"][1]):
        failures.append(f"the paired keyboard is not in the list: {out['LIST2'][1]!r}")
    if "forgotten" not in out["FORGET"][1]:
        failures.append(f"forget printed {out['FORGET'][1]!r}")
    if re.search(rf"{KBD_FULL}  (paired|connected)", out["LIST3"][1]):
        failures.append(f"the keyboard is still paired after forget: {out['LIST3'][1]!r}")
    if "BT:BOND:FORGET" not in log:
        failures.append("btd never logged the bond forgotten")
    failures += passkey_leaks(log)
    start = hid.position(log, "BT:HID:KBD")
    if start < 0:
        failures.append("the keyboard never became a source")
    else:
        failures += hid.judge_typed(log, text, start)
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("mode", choices=("ui-pair", "btctl"))
    parser.add_argument("log", type=Path)
    parser.add_argument("--text", required=True, help="what the keyboard types once paired")
    parser.add_argument("--shots", type=Path, help="ui-pair: the directory the dialog screenshot is in")
    args = parser.parse_args()
    log = args.log.read_text(encoding="utf-8", errors="replace")
    failures = (judge_ui_pair if args.mode == "ui-pair" else judge_btctl)(log, args.text)
    if args.mode == "ui-pair" and args.shots:
        shot = args.shots / "shot_passkey_dialog.png"
        if not shot.is_file() or shot.stat().st_size < 2000:
            failures.append(f"no screenshot of the passkey dialog ({shot})")
    for failure in failures:
        print(f"FAIL: {failure}")
    print(f"bt {args.mode}: " + ("FAIL" if failures else "PASS"))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
