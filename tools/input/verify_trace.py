#!/usr/bin/env python3
"""Check an `inputd` serial trace against the keys `input_keys.json` typed.

`tools/screenshot/examples/input_keys.json` presses a fixed set of *physical*
keys through QEMU (which speaks US key names); this script knows what each
layout must type for them, written out by hand from the layout definitions
(not derived from the implementation), so a regression in the HID table, the
keymaps, the modifier/lock state machine or key repeat fails it.

    python tools/input/verify_trace.py shots/input_us/serial.log --layout us
    python tools/input/verify_trace.py shots/input_fr/serial.log --layout fr

The image must be built with `LAZYOS_SERVICES=1` (a debug build, so `inputd`
runs with `trace=1`); the FR image also needs `LAZYOS_KBD_LAYOUT=fr`.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# The text typed by the physical key sequence in input_keys.json, in order:
# letters/digits/symbols unshifted, Shift, AltGr, Ctrl (no text), Caps Lock
# on then off, keypad 1 and +, then `x` held down (first press only; repeats
# are counted separately).
EXPECTED = {
    "us": (
        "qwazm120-=;',./[]\\` " "Q!<?" "" "" "BB".replace("BB", "Bb") + "1+" + "x"
    ),
    "fr": (
        "azqw,&é" "à" ")=mù;:!^$*² " "A1.§" "~@[`" "" "Bb" + "1+" + "x"
    ),
}

# Modifier bits inputd reports (libs/inputmap): NumLock is on at boot.
NUM_LOCK = 0x40
ALT = 0x04
ALTGR = 0x10

KEY = re.compile(
    r"INPUTD:KEY code=(0x[0-9a-f]+) sym=(0x[0-9a-f]+) mods=(0x[0-9a-f]+) (down|up|repeat)"
)
TEXT = re.compile(r"INPUTD:TEXT u\+([0-9a-f]+)")


def parse(log: str):
    events = []
    for line in log.splitlines():
        # The console echoes typed characters into the same stream, so
        # anchor on the marker rather than on the start of the line.
        line = line[line.find("INPUTD:") :] if "INPUTD:" in line else line
        if match := KEY.search(line):
            code, sym, mods, state = match.groups()
            events.append(("key", int(code, 16), int(sym, 16), int(mods, 16), state))
        elif match := TEXT.search(line):
            events.append(("text", int(match.group(1), 16)))
    return events


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("log", type=Path)
    parser.add_argument("--layout", choices=sorted(EXPECTED), required=True)
    args = parser.parse_args()
    log = args.log.read_text(errors="replace")

    failures: list[str] = []
    if f"INPUTD:READY layout={args.layout}" not in log:
        failures.append(f"missing 'INPUTD:READY layout={args.layout}'")

    events = parse(log)
    texts = "".join(chr(e[1]) for e in events if e[0] == "text")
    # Repeats also produce text; collapse the run of `x` after the first.
    collapsed = re.sub(r"x+$", "x", texts)
    if collapsed != EXPECTED[args.layout]:
        failures.append(f"typed {ascii(collapsed)}, want {ascii(EXPECTED[args.layout])}")

    downs = [e for e in events if e[0] == "key" and e[4] == "down"]
    ups = [e for e in events if e[0] == "key" and e[4] == "up"]
    repeats = [e for e in events if e[0] == "key" and e[4] == "repeat"]
    # Every press is released exactly once (no stuck keys).
    if len(downs) != len(ups):
        failures.append(f"{len(downs)} presses but {len(ups)} releases")
    # `x` (HID 0x1b) held for ~1.5 s: 500 ms delay then ~30 ms repeats.
    if not 8 <= len(repeats) <= 60 or {e[1] for e in repeats} != {0x1B}:
        failures.append(f"{len(repeats)} repeats of {sorted({e[1] for e in repeats})}")
    # Repeats never appear before the key's own press, and are flagged.
    first_x = next((i for i, e in enumerate(events) if e[:2] == ("key", 0x1B)), None)
    if first_x is None or events[first_x][4] != "down":
        failures.append("the held key has no leading Down")
    # Right Alt: AltGr on AZERTY, plain Alt on US.
    right_alt = [e for e in downs if e[1] == 0xE6]
    if len(right_alt) != 1:
        failures.append(f"{len(right_alt)} right-Alt presses")
    else:
        want = (NUM_LOCK | ALTGR) if args.layout == "fr" else (NUM_LOCK | ALT)
        if right_alt[0][3] != want:
            failures.append(f"right Alt mods {right_alt[0][3]:#x}, want {want:#x}")
    # Modifier keys are reported as real events (Shift, Ctrl, AltGr, Caps).
    for code, name in ((0xE1, "Shift"), (0xE0, "Ctrl"), (0x39, "CapsLock")):
        if not any(e[1] == code for e in downs):
            failures.append(f"no {name} key event")

    if failures:
        print("input trace: FAIL")
        for failure in failures:
            print("  -", failure)
        return 1
    print(
        f"input trace: PASS layout={args.layout} "
        f"presses={len(downs)} repeats={len(repeats)} text={ascii(collapsed)}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
