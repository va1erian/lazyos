#!/usr/bin/env python3
"""Judge the LE Bluetooth HID serial logs (docs/bluetooth-plan.md, phase B4).

The verdict is what reached `inputd`, as for the BR/EDR modes
(`judge_hid.py`, whose key and pointer checks this reuses), plus the order of
the lines an LE connection must produce: connect, SMP, encryption, only then
the bond and the source (decision 6). The modes (`tools/bt/run.py`):

* `le-pair`: an LE mouse (Just Works, unauthenticated key) and an LE keyboard
  (Passkey Entry, authenticated key) pair over SMP, are discovered over GATT,
  bond, and type and point.
* `le-reboot`: after a pairing run the same image boots again (log 2): the
  bond file is read before the controller is up, nothing pairs again, both
  devices reconnect by themselves (no discovery: the stored handles) and the
  text typed during the boot arrives. The timings are reported.
* `le-rpa`: as `le-reboot`, but the bonded devices advertise under a fresh
  resolvable private address each time (`rpa=1` on every reconnect), and the
  keyboard comes back a second time under yet another.
* `le-refuse`: a Just Works LE keyboard is refused (`BT:REFUSE
  reason=keyboard-unauthenticated`), is never bonded or registered, and
  types nothing; a stranger holding a key the host never issued is never
  even connected.

    python tools/bt/judge_le.py le-pair shots/bt/serial.log --text hello
    python tools/bt/judge_le.py le-reboot shots/bt/boot1/serial.log shots/bt/boot2/serial.log --pair-text hello --text lazy
    python tools/bt/judge_le.py le-rpa shots/bt/boot1/serial.log shots/bt/boot2/serial.log --pair-text hello --text lazygo
    python tools/bt/judge_le.py le-refuse shots/bt/serial.log
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import judge_hid as hid  # noqa: E402

#: The last two octets of the LE peers (`tools/bt/modes_le.py`).
KBD, MOUSE, JW, INTRUDER = "01:01", "01:02", "01:03", "01:09"
CONN = re.compile(r"BT:CONN addr=xx:xx:xx:xx:(\w\w:\w\w) handle=0x[0-9a-f]+ role=le rpa=(\d)")


def story(log: str, tail: str, pairing: bool, kind: str) -> list[str]:
    """One LE device's lines in the order a connection should produce them."""
    mask = f"addr=xx:xx:xx:xx:{tail}"
    steps: list[tuple[str, str | re.Pattern[str]]] = [("BT:CONN role=le", f"BT:CONN {mask}")]
    if pairing:
        steps.append(("BT:PAIR:START", f"BT:PAIR:START {mask}"))
        if kind == "keyboard":
            steps.append(("BT:PAIR:PASSKEY", hid.PASSKEY))
    steps.append(("BT:ENCRYPT", f"BT:ENCRYPT {mask}"))
    if pairing:
        auth, cls = (1, "keyboard") if kind == "keyboard" else (0, "pointer")
        steps.append(("BT:PAIR:PASS", f"BT:PAIR:PASS {mask} auth={auth} class={cls}"))
    line = "BT:HID:KBD" if kind == "keyboard" else "BT:HID:MOUSE"
    steps.append((line, f"{line} {mask}"))
    return hid.ordered(log, steps)


def connections(log: str, tail: str) -> list[int]:
    """The `rpa=` value of each connection of the device `tail`."""
    return [int(rpa) for t, rpa in CONN.findall(log) if t == tail]


def judge_common(log: str) -> list[str]:
    failures = hid.judge_common(log)
    for bad in ("BT:GATT:FAIL", "BT:PAIR:FAIL", "BT:ENCRYPT:FAIL"):
        if bad in log:
            failures.append(f"{bad} appears")
    return failures


def judge_pair(log: str, text: str) -> list[str]:
    """A pairing boot: both devices paired over SMP, bonded, typing and pointing."""
    failures = judge_common(log)
    failures += story(log, KBD, True, "keyboard")
    failures += story(log, MOUSE, True, "mouse")
    start = hid.position(log, "BT:HID:KBD")
    if start >= 0 and "auth=1" not in log[start:].split("\n", 1)[0]:
        failures.append("the keyboard registered without an authenticated key")
    for tail in (KBD, MOUSE):
        if connections(log, tail) != [0]:
            failures.append(f"{tail} should connect once under its own address, saw rpa={connections(log, tail)}")
    loaded = hid.BOND_LOAD.search(log)
    if loaded is None or loaded.group(1) != "0":
        failures.append("the first boot should load an empty bond store")
    saves = log.count("BT:BOND:SAVE bytes=")
    if saves < 2:
        failures.append(f"{saves} bond saves, want one per bonded device")
    failures += hid.judge_typed(log, text)
    failures += hid.judge_pointer(log)
    return failures


def judge_reboot(first: str, second: str, pair_text: str, text: str, rpa: bool = False) -> list[str]:
    """Boot 1 pairs; boot 2, the same image, reconnects with no user action."""
    failures = [f"boot 1: {f}" for f in judge_pair(first, pair_text)]
    failures += [f"boot 2: {f}" for f in judge_common(second)]
    loaded = hid.BOND_LOAD.search(second)
    if loaded is None or loaded.group(1) != "2":
        failures.append(f"boot 2 loaded {loaded.group(1) if loaded else 'no'} bonds, want 2 (the bond file was lost)")
    elif hid.position(second, loaded.re) > hid.position(second, hid.READY):
        failures.append("the bond file was read after the host was ready")
    for gone in ("BT:PAIR:START", "BT:PAIR:PASSKEY", "BT:PAIR:PASS", "BT:REFUSE"):
        if gone in second:
            failures.append(f"boot 2 shows {gone}: it paired again or refused a bonded device")
    if len(hid.numbers(hid.READY, second)) != 1:
        failures.append(f"{len(hid.numbers(hid.READY, second))} BT:READY lines in boot 2, want 1")
    failures += story(second, KBD, False, "keyboard")
    failures += story(second, MOUSE, False, "mouse")
    reconnects = {tail for _, tail in hid.RECONNECT.findall(second)}
    for tail in (KBD, MOUSE):
        if tail not in reconnects:
            failures.append(f"no BT:RECONNECT for {tail}")
        seen = connections(second, tail)
        if rpa:
            # The keyboard also disconnects and returns once (a new address).
            count = 2 if tail == KBD else 1
            good = len(seen) == count and all(seen)
            want = f"{count} connection(s), all under a resolvable private address"
        else:
            good = bool(seen) and not any(seen)
            want = "connections under the identity address"
        if not good:
            failures.append(f"{tail} connected with rpa={seen}, want {want}")
    failures += hid.judge_typed(second, text)
    failures += hid.judge_pointer(second)
    return failures


def reboot_timings(second: str) -> dict[str, int]:
    """Ready since boot and link-up-to-first-report per device, milliseconds."""
    out = {}
    ready = hid.numbers(hid.READY, second)
    if ready:
        out["ready_ms"] = ready[0]
    for ms, tail in hid.RECONNECT.findall(second):
        out.setdefault("reconnect_kbd_ms" if tail == KBD else "reconnect_mouse_ms", int(ms))
    return out


def judge_refuse(log: str) -> list[str]:
    """A Just Works keyboard is refused and a stranger never connected."""
    failures = judge_common(log)
    if f"BT:REFUSE reason=keyboard-unauthenticated addr=xx:xx:xx:xx:{JW}" not in log:
        failures.append("the Just Works keyboard was not refused")
    if len(CONN.findall(log)) != 1:
        failures.append(f"{len(CONN.findall(log))} connections, want exactly the Just Works keyboard once")
    for forbidden in ("BT:HID:", "BT:PAIR:PASS", "BT:KEY", "BT:BOND:SAVE bytes="):
        if forbidden in log:
            failures.append(f"{forbidden} appears: the refused keyboard got further than discovery")
    if f"xx:xx:xx:xx:{INTRUDER}" in log:
        failures.append("the stranger appears in a BT line: it was connected")
    if hid.INPUTD_KEY.search(log) or hid.INPUTD_TEXT.search(log):
        failures.append("inputd saw a key that no admitted device sent")
    if "BT:READY" not in log:
        failures.append("the host never became ready")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("mode", choices=["le-pair", "le-reboot", "le-rpa", "le-refuse"])
    parser.add_argument("logs", type=Path, nargs="+")
    parser.add_argument("--text", default="hello", help="what the keyboard typed (in the last boot)")
    parser.add_argument("--pair-text", default="hello", help="what it typed in the pairing boot")
    args = parser.parse_args()
    logs = [p.read_text(errors="replace") for p in args.logs]
    if args.mode in ("le-reboot", "le-rpa"):
        failures = judge_reboot(logs[0], logs[1], args.pair_text, args.text, rpa=args.mode == "le-rpa")
    elif args.mode == "le-pair":
        failures = judge_pair(logs[0], args.text)
    else:
        failures = judge_refuse(logs[0])
    for failure in failures:
        print(f"BT:JUDGE:FAIL {failure}")
    if args.mode in ("le-reboot", "le-rpa"):
        print("BT:TIMING " + " ".join(f"{k}={v}" for k, v in reboot_timings(logs[1]).items()))
    print(f"BT:JUDGE:{'FAIL' if failures else 'PASS'} {args.mode}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
