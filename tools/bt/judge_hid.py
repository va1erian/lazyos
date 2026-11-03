#!/usr/bin/env python3
"""Judge the Bluetooth HID serial logs (docs/bluetooth-plan.md, phase B3).

The verdict is what reached `inputd`, not that `btd` printed a marker: every
key `inputd` decoded must be one `btd` published (a `BT:KEY` edge with the
same usage and direction, in order) and the text the peer typed must be the
text `inputd` produced (`INPUTD:TEXT`). The modes (`tools/bt/run.py`):

* `pair`: the mouse (Just Works, unauthenticated key) and the keyboard
  (Passkey Entry, authenticated key) pair; the bonds are stored; typing and
  mouse motion arrive.
* `reboot`: after a pairing run, the same image boots again (log 2): the bond
  file is read before the controller is up (`BT:BOND:LOAD n=2`), no pairing
  runs, the peers reconnect by themselves and the text they typed *during
  the boot* arrives. The timings (`BT:READY ms=`, `BT:RECONNECT ms=`) are
  reported.
* `refuse`: a stranger that holds a key but is not bonded pages the host and
  types; it is refused at the connection request and nothing reaches
  `inputd`.
* `kill`: `btd` exits holding a key (crash-test build); the kernel released
  it before `init`'s restarted `btd` came up, and the restarted `btd`
  reattached to the controller and its keyboard types again.

    python tools/bt/judge_hid.py pair shots/bt/serial.log --text hello
    python tools/bt/judge_hid.py reboot shots/bt/boot1/serial.log shots/bt/boot2/serial.log --pair-text hello --text lazy
    python tools/bt/judge_hid.py refuse shots/bt/serial.log
    python tools/bt/judge_hid.py kill shots/bt/serial.log --text zq
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

#: The peers the harness runs (`tools/bt/modes.py`): the last two octets are
#: what a trace may carry.
KBD, MOUSE, INTRUDER = "00:01", "00:02", "00:09"
FULL_ADDRESS = re.compile(r"C0:FF:EE:00:0[01]:0[0-9]", re.IGNORECASE)

#: A bond save is tried twice (`btd` retries a failed one: the kernel's first
#: `fsync` of a new file after boot has answered EINVAL); only the second
#: failure is an error.
FATAL = re.compile(
    r"USBD:(FATAL|PANIC|PORT:FAIL|SLOT:LEAK|BT:FAIL)|BT:(PANIC|CTRL:FAIL|LINK:FAIL|SOURCE:FAIL|BOND:SAVE:FAIL .* attempt=2)"
)
BT_KEY = re.compile(r"BT:KEY usage=0x([0-9a-f]+) (down|up) t=(\d+)")
INPUTD_KEY = re.compile(r"INPUTD:KEY code=0x([0-9a-f]+) sym=\S+ mods=\S+ (down|up)")
INPUTD_TEXT = re.compile(r"INPUTD:TEXT u\+([0-9a-f]+)")
POINTER = re.compile(r"INPUTD:POINTER x=(-?\d+) y=(-?\d+) buttons=0x([0-9a-f]+) wheel=(-?\d+),(-?\d+)")
READY = re.compile(r"BT:READY ms=(\d+)")
RECONNECT = re.compile(r"BT:RECONNECT ms=(\d+) addr=xx:xx:xx:xx:(\w\w:\w\w)")
BOND_LOAD = re.compile(r"BT:BOND:LOAD n=(\d+)")
PASSKEY = re.compile(r"BT:PAIR:PASSKEY value=(\d{6}) addr=xx:xx:xx:xx:(\w\w:\w\w)")


def usage(c: str) -> int:
    """The HID usage `fakebt` presses for a typed character."""
    if "a" <= c <= "z":
        return 0x04 + ord(c) - ord("a")
    if "1" <= c <= "9":
        return 0x1E + ord(c) - ord("1")
    return {"0": 0x27, "\n": 0x28, " ": 0x2C}[c]


def edges(log: str) -> list[tuple[int, str]]:
    return [(int(code, 16), state) for code, state, _ in BT_KEY.findall(log)]


def position(log: str, needle: str | re.Pattern[str], start: int = 0) -> int:
    """Where `needle` first appears in `log` from `start`, or -1."""
    if isinstance(needle, str):
        return log.find(needle, start)
    match = needle.search(log, start)
    return match.start() if match else -1


def judge_common(log: str, crashed: bool = False) -> list[str]:
    """Driver errors, addresses and keys that reached `inputd`. `crashed`: a
    `btd` died holding keys (`BT:CRASH`), which the kernel released."""
    failures = [f"error: {line.strip()}" for line in log.splitlines() if FATAL.search(line)]
    for line in log.splitlines():
        if "BT:" in line and FULL_ADDRESS.search(line):
            failures.append(f"a full Bluetooth address in the log: {line.strip()}")
    failures += judge_keys(log, crashed)
    return failures


def judge_keys(log: str, crashed: bool = False) -> list[str]:
    """`inputd`'s key edges must be exactly `btd`'s, in order. When `btd`
    crashed holding keys, the kernel's releases follow its last edge."""
    sent = edges(log)
    crash = position(log, "BT:CRASH") if crashed else -1
    if crash >= 0:
        before, after = edges(log[:crash]), edges(log[crash:])
        held: list[int] = []
        for code, state in before:
            if state == "down":
                held.append(code)
            elif code in held:
                held.remove(code)
        sent = before + [(code, "up") for code in held] + after
    seen = [(int(code, 16), state) for code, state in INPUTD_KEY.findall(log)]
    if seen == sent:
        return []
    for index, (got, want) in enumerate(zip(seen, sent)):
        if got != want:
            return [f"key edge {index}: inputd saw {got}, btd sent {want}"]
    return [f"inputd saw {len(seen)} key edges, btd sent {len(sent)}"]


def judge_typed(log: str, text: str, start: int = 0) -> list[str]:
    """`text` was published (press and release of each key) and `inputd`
    turned it into the same text."""
    failures = []
    typed = edges(log[start:])
    want = [(usage(c), state) for c in text for state in ("down", "up")]
    if typed != want:
        failures.append(f"btd published {typed}, the peer typed {text!r} = {want}")
    chars = "".join(chr(int(cp, 16)) for cp in INPUTD_TEXT.findall(log[start:]))
    if chars != text:
        failures.append(f"inputd produced {chars!r}, the peer typed {text!r}")
    for code in {code for code, _ in typed}:
        last = [state for c, state in typed if c == code][-1]
        if last != "up":
            failures.append(f"key {code:#x} is still held at the end")
    return failures


def judge_pointer(log: str) -> list[str]:
    """The mouse moved `inputd`'s cursor and pressed and released a button."""
    states = [
        (int(x), int(y), int(buttons, 16)) for x, y, buttons, _, _ in POINTER.findall(log)
    ]
    if not states:
        return ["inputd reported no pointer state at all"]
    failures = []
    if len({(x, y) for x, y, _ in states}) < 2:
        failures.append("the cursor never moved")
    pressed = [i for i, (_, _, b) in enumerate(states) if b & 1]
    if not pressed:
        failures.append("the mouse button never went down")
    elif not any(b == 0 for _, _, b in states[pressed[0]:]):
        failures.append("the mouse button was never released")
    if states[-1][2] != 0:
        failures.append("a button is still held at the end")
    return failures


def ordered(log: str, steps: list[tuple[str, str | re.Pattern[str]]], start: int = 0) -> list[str]:
    """Each step appears, after the one before it."""
    failures = []
    at = start
    for name, needle in steps:
        found = position(log, needle, at)
        if found < 0:
            failures.append(f"{name} is missing (or comes too early)")
            continue
        at = found
    return failures


def device_story(log: str, tail: str, pairing: bool, kind: str) -> list[str]:
    """One device's lines, in the order a connection should produce them."""
    mask = f"addr=xx:xx:xx:xx:{tail}"
    steps: list[tuple[str, str | re.Pattern[str]]] = [("BT:CONN", f"BT:CONN {mask}")]
    if pairing:
        steps.append(("BT:PAIR:START", f"BT:PAIR:START {mask}"))
        if kind == "keyboard":
            steps.append(("BT:PAIR:PASSKEY", PASSKEY))
    steps.append(("BT:ENCRYPT", f"BT:ENCRYPT {mask}"))
    if pairing:
        auth = 1 if kind == "keyboard" else 0
        cls = "keyboard" if kind == "keyboard" else "pointer"
        steps.append(("BT:PAIR:PASS", f"BT:PAIR:PASS {mask} auth={auth} class={cls}"))
    hid = "BT:HID:KBD" if kind == "keyboard" else "BT:HID:MOUSE"
    steps.append((hid, f"{hid} {mask}"))
    return ordered(log, steps)


def judge_pair(log: str, text: str) -> list[str]:
    """A pairing boot: both devices paired, bonded, typing and pointing."""
    failures = judge_common(log)
    failures += device_story(log, KBD, True, "keyboard")
    failures += device_story(log, MOUSE, True, "mouse")
    if "BT:HID:KBD" in log and "auth=1" not in log[position(log, "BT:HID:KBD"):].split("\n", 1)[0]:
        failures.append("the keyboard registered without an authenticated key")
    if len(BOND_LOAD.findall(log)) != 1 or BOND_LOAD.search(log).group(1) != "0":
        failures.append("the first boot should load an empty bond store")
    if log.count("BT:BOND:SAVE bytes=") < 4:
        failures.append(f"{log.count('BT:BOND:SAVE bytes=')} bond saves, want one at each key and each record")
    if "BT:PAIR:FAIL" in log:
        failures.append("a pairing failed")
    failures += judge_typed(log, text)
    failures += judge_pointer(log)
    return failures


def numbers(pattern: re.Pattern[str], log: str) -> list[int]:
    return [int(m.group(1)) for m in pattern.finditer(log)]


def judge_reboot(first: str, second: str, pair_text: str, text: str) -> list[str]:
    """Boot 1 pairs; boot 2, the same image, reconnects with no user action."""
    failures = [f"boot 1: {f}" for f in judge_pair(first, pair_text)]
    failures += [f"boot 2: {f}" for f in judge_common(second)]
    loaded = BOND_LOAD.search(second)
    if loaded is None or loaded.group(1) != "2":
        failures.append(f"boot 2 loaded {loaded.group(1) if loaded else 'no'} bonds, want 2 (the bond file was lost)")
    elif position(second, loaded.re) > position(second, READY):
        failures.append("the bond file was read after the host was ready")
    for gone in ("BT:PAIR:START", "BT:PAIR:PASSKEY", "BT:PAIR:PASS", "BT:REFUSE"):
        if gone in second:
            failures.append(f"boot 2 shows {gone}: it paired again or refused a bonded device")
    ready = numbers(READY, second)
    if len(ready) != 1:
        failures.append(f"{len(ready)} BT:READY lines in boot 2, want 1")
    failures += device_story(second, KBD, False, "keyboard")
    failures += device_story(second, MOUSE, False, "mouse")
    reconnects = {tail: int(ms) for ms, tail in RECONNECT.findall(second)}
    for tail in (KBD, MOUSE):
        if tail not in reconnects:
            failures.append(f"no BT:RECONNECT for {tail}")
    failures += judge_typed(second, text)
    failures += judge_pointer(second)
    return failures


def reboot_timings(second: str) -> dict[str, int]:
    """The numbers boot 2 reports: ready since boot, link-up-to-first-report
    per device, all in milliseconds."""
    out = {}
    ready = numbers(READY, second)
    if ready:
        out["ready_ms"] = ready[0]
    for ms, tail in RECONNECT.findall(second):
        out["reconnect_kbd_ms" if tail == KBD else "reconnect_mouse_ms"] = int(ms)
    return out


def judge_refuse(log: str) -> list[str]:
    """A stranger holding a key is refused and types nothing."""
    failures = judge_common(log)
    if f"BT:REFUSE reason=unbonded addr=xx:xx:xx:xx:{INTRUDER}" not in log:
        failures.append("the unbonded device was not refused")
    for forbidden in ("BT:HID:", "BT:ENCRYPT", "BT:PAIR:", "BT:CONN addr", "BT:KEY"):
        if forbidden in log:
            failures.append(f"{forbidden} appears: the stranger got further than the connection request")
    if INPUTD_KEY.search(log) or INPUTD_TEXT.search(log):
        failures.append("inputd saw a key that no bonded device sent")
    if "BT:READY" not in log:
        failures.append("the host never became ready")
    return failures


def judge_kill(log: str, text: str) -> list[str]:
    """`btd` died holding a key: released by the kernel, then back."""
    failures = judge_common(log, crashed=True)
    crash = position(log, "BT:CRASH")
    if crash < 0:
        return failures + ["btd never crashed (is this the crash-test build?)"]
    seen = [(m.start(), int(m.group(1), 16), m.group(2)) for m in INPUTD_KEY.finditer(log)]
    # The keys `btd` had published down and not up when it died (`inputd`
    # prints its lines a moment after `btd` does, so the order of the two
    # streams in the log says nothing about it).
    held: list[int] = []
    for code, state in edges(log[:crash]):
        if state == "down":
            held.append(code)
        elif code in held:
            held.remove(code)
    if not held:
        failures.append("no key was held when btd died")
    restart = [m.start() for m in re.finditer("BT:CRED", log)]
    for code in held:
        downs = [at for at, c, state in seen if c == code and state == "down"]
        if not downs:
            failures.append(f"inputd never saw key {code:#x} go down")
            continue
        released = [at for at, c, state in seen if c == code and state == "up" and at > downs[0]]
        if not released:
            failures.append(f"key {code:#x} was never released after btd died")
        elif len(restart) >= 2 and released[0] > restart[1]:
            failures.append(f"key {code:#x} was released only after btd restarted")
    if log.count("BT:CRED") != 2:
        failures.append(f"{log.count('BT:CRED')} btd starts, want 2 (the original and init's restart)")
    if len(READY.findall(log)) != 2:
        failures.append(f"{len(READY.findall(log))} BT:READY lines, want 2: the restarted btd must reattach")
    # What the restarted btd typed: the text, after the second ready.
    ready2 = [m.start() for m in READY.finditer(log)][1:2]
    if ready2:
        failures += judge_typed(log, text, ready2[0])
    # Nothing is held at the end.
    final: dict[int, str] = {}
    for _, code, state in seen:
        final[code] = state
    for code, state in final.items():
        if state != "up":
            failures.append(f"key {code:#x} is still held at the end")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("mode", choices=["pair", "reboot", "refuse", "kill"])
    parser.add_argument("logs", type=Path, nargs="+")
    parser.add_argument("--text", default="hello", help="what the keyboard typed (in the last boot)")
    parser.add_argument("--pair-text", default="hello", help="reboot: what it typed in the pairing boot")
    args = parser.parse_args()
    logs = [p.read_text(errors="replace") for p in args.logs]
    if args.mode == "reboot":
        failures = judge_reboot(logs[0], logs[1], args.pair_text, args.text)
    elif args.mode == "pair":
        failures = judge_pair(logs[0], args.text)
    elif args.mode == "refuse":
        failures = judge_refuse(logs[0])
    else:
        failures = judge_kill(logs[0], args.text)
    for failure in failures:
        print(f"BT:JUDGE:FAIL {failure}")
    if args.mode == "reboot":
        print("BT:TIMING " + " ".join(f"{k}={v}" for k, v in reboot_timings(logs[1]).items()))
    print(f"BT:JUDGE:{'FAIL' if failures else 'PASS'} {args.mode}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
