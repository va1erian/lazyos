#!/usr/bin/env python3
"""Judge the trusted prompt scenarios (docs/accounts-plan.md U2, issue #626).

Each scenario is one `attack.sh <name>` command in the attack session; its
part of the serial log runs from that command to the next `attack.sh` one
([`section`]), so the prompts of different scenarios are never confused.

* `prompt_over`: `elevd` asks for a change, so `xuid` shows its prompt; five
  seconds later the session opens a window (the Counter), which takes the
  focus. The harness screenshots the screen before (`prompt_up`) and after
  the window opened (`prompt_window`). BLOCKED when the window really opened
  while the prompt was up (its `XUIAPP:COUNTER:PASS` lies between
  `XUID:PROMPT:UP` and `XUID:PROMPT:DONE`), really lies over the prompt's
  panel (its `UI:RECT` overlaps at least `OVERLAP_MIN` of the panel), and
  the panel is the same in both shots (at least `SAME_MIN` of its pixels).
* `prompt_keys`: the Terminal has the focus when the prompt opens; the
  harness types `inject` and Enter, then Escape. A key that reached the
  Terminal would show as `TERM:CMD:inject` (the Terminal reports every line
  submitted with Enter). BLOCKED when no client got it, the prompt reports
  it took the keys (`XUID:PROMPT:DONE outcome=cancelled keys=<n>`, n >= 8)
  from `inputd` and `elevd` recorded the cancel.
* `input_flood`: the same, typing `flooded`, while three programs keep
  `inputd`'s shared endpoint full (`input_flood.rhai`; their refused sends
  must be counted, `full=<n>` > 0, or the flood was not real). BLOCKED when
  no client got the keys and either the prompt took them, or `xuid` refused
  to show the prompt it could not take the keyboard for
  (`XUID:PROMPT:REFUSED`, `elevd`'s `nokeys`).

    python tools/accounts/prompt_judge.py shots/accounts/attack
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "screenshot"))
from pngstats import decode_png  # noqa: E402

#: The prompt panel's design size (`user/src/bin/xuid/prompt_draw.rs`).
PANEL = (480, 324)
#: The fraction of the panel's pixels that must not change.
SAME_MIN = 0.98
#: The fraction of the panel the window must cover for `prompt_over` to mean
#: anything.
OVERLAP_MIN = 0.05
#: What the session types into the prompt before Enter and Escape.
TYPED = "inject"
#: What it types during the inputd flood.
FLOOD_TYPED = "flooded"
#: The window `prompt_over` opens (`xui-app/src/bin/counter.rs`).
COUNTER_TITLE = "xui counter"
RECT = re.compile(r"UI:RECT x=(-?\d+) y=(-?\d+) w=(\d+) h=(\d+) name=window:(.*)")


def panel_rect(width: int, height: int) -> tuple[int, int, int, int]:
    """The panel's rectangle on a `width` x `height` screen (scale 1, or 2
    from 2560x1440 as `xuid` picks it)."""
    scale = 2 if width >= 2560 else 1
    w, h = PANEL[0] * scale, PANEL[1] * scale
    return (width - w) // 2, (height - h) // 2, w, h


def overlap(a: tuple[int, int, int, int], b: tuple[int, int, int, int]) -> int:
    """The area two (x, y, w, h) rectangles share."""
    w = min(a[0] + a[2], b[0] + b[2]) - max(a[0], b[0])
    h = min(a[1] + a[3], b[1] + b[3]) - max(a[1], b[1])
    return max(w, 0) * max(h, 0)


def same_fraction(first: Path, second: Path) -> float:
    """How much of the panel is identical in two screenshots (0 when the
    shots differ in size)."""
    w1, h1, c1, a = decode_png(first)
    w2, h2, c2, b = decode_png(second)
    if (w1, h1, c1) != (w2, h2, c2):
        return 0.0
    x, y, w, h = panel_rect(w1, h1)
    same = 0
    for row in range(y, y + h):
        start = (row * w1 + x) * c1
        line_a, line_b = a[start:start + w * c1], b[start:start + w * c1]
        same += sum(1 for p in range(0, w * c1, c1) if line_a[p:p + 3] == line_b[p:p + 3])
    return same / (w * h)


def section(log: str, name: str) -> str:
    """The part of the log from the command `attack.sh <name>` to the next
    `attack.sh` command (empty when the scenario never started)."""
    start = log.find(f"TERM:CMD:sh /system/share/accounts/attack.sh {name}")
    if start < 0:
        return ""
    end = log.find("TERM:CMD:sh /system/share/accounts/attack.sh", start + 1)
    return log[start:] if end < 0 else log[start:end]


def window_rect(text: str) -> tuple[int, int, int, int] | None:
    """The last rectangle the Counter's window was reported at."""
    rects = [m for m in RECT.finditer(text) if m.group(5).strip() == COUNTER_TITLE]
    if not rects:
        return None
    last = rects[-1]
    return tuple(int(last.group(i)) for i in range(1, 5))  # type: ignore[return-value]


def over_marker(log: str, shots: Path) -> str:
    """`prompt_over`'s marker (see the module docs)."""
    text = section(log, "prompt_over")
    up, done = text.find("XUID:PROMPT:UP"), text.find("XUID:PROMPT:DONE")
    window = text.find("XUIAPP:COUNTER:PASS")
    if up < 0 or done < 0:
        return "ACCT:ATTACK:prompt_over:ERROR:noprompt"
    if not up < window < done:
        return "ACCT:ATTACK:prompt_over:ERROR:nowindow"
    first, second = shots / "shot_prompt_up.png", shots / "shot_prompt_window.png"
    if not first.is_file() or not second.is_file():
        return "ACCT:ATTACK:prompt_over:ERROR:noshots"
    rect = window_rect(text[up:done])
    if rect is None:
        return "ACCT:ATTACK:prompt_over:ERROR:norect"
    width, height, _, _ = decode_png(second)
    panel = panel_rect(width, height)
    covered = overlap(rect, panel) / (panel[2] * panel[3])
    if covered < OVERLAP_MIN:
        return f"ACCT:ATTACK:prompt_over:ERROR:nooverlap={covered:.3f}"
    same = same_fraction(first, second)
    outcome = "BLOCKED" if same >= SAME_MIN else "SUCCEEDED"
    return f"ACCT:ATTACK:prompt_over:{outcome}:panel_same={same:.3f}_covered={covered:.2f}"


def typed_marker(name: str, tag: str, typed: str, text: str) -> str:
    """Where the keys typed at a prompt went: BLOCKED when the prompt took
    them and no client did (see the module docs)."""
    if f"TERM:CMD:{typed}" in text and "XUID:PROMPT:REFUSED" not in text:
        return f"ACCT:ATTACK:{name}:SUCCEEDED:a_client_got_{typed}"
    if "XUID:PROMPT:REFUSED" in text:
        refused = (f"ACCT:PROMPT:{tag}:NOKEYS" in text
                   and re.search(r"ELEVD:REQUEST op=time\.set .*outcome=nokeys", text))
        return (f"ACCT:ATTACK:{name}:BLOCKED:refused" if refused
                else f"ACCT:ATTACK:{name}:ERROR:refused_unreported")
    done = re.search(r"XUID:PROMPT:DONE outcome=(\w+) keys=(\d+)", text)
    if not done:
        return f"ACCT:ATTACK:{name}:ERROR:noprompt"
    outcome, keys = done.group(1), int(done.group(2))
    source = re.search(r"XUID:PROMPT:KEYS source=(\w+)", text)
    cancelled = (re.search(r"ELEVD:REQUEST op=time\.set .*outcome=cancelled", text)
                 and f"ACCT:PROMPT:{tag}:CANCELLED" in text)
    # Six letters, Enter and Escape at least.
    if outcome != "cancelled" or not cancelled or keys < len(typed) + 2:
        return f"ACCT:ATTACK:{name}:ERROR:outcome={outcome}_keys={keys}"
    if not source or source.group(1) != "inputd":
        return f"ACCT:ATTACK:{name}:ERROR:source={source.group(1) if source else 'none'}"
    return f"ACCT:ATTACK:{name}:BLOCKED:keys={keys}"


def keys_marker(log: str) -> str:
    """`prompt_keys`'s marker (see the module docs)."""
    return typed_marker("prompt_keys", "keys", TYPED, section(log, "prompt_keys"))


def flood_marker(log: str) -> str:
    """`input_flood`'s marker (see the module docs)."""
    text = section(log, "input_flood")
    full = re.search(r"ACCT:PROMPT:flood:\w+:full=(\d+)", text)
    if not full:
        return "ACCT:ATTACK:input_flood:ERROR:noreport"
    if int(full.group(1)) == 0:
        return "ACCT:ATTACK:input_flood:ERROR:noflood"
    marker = typed_marker("input_flood", "flood", FLOOD_TYPED, text)
    return marker if ":BLOCKED:" not in marker else f"{marker}_full={full.group(1)}"


def markers(log: str, shots: Path) -> str:
    """Every marker, one per line, for `attack_judge.judge`."""
    return "\n".join([over_marker(log, shots), keys_marker(log), flood_marker(log)]) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("session", type=Path, help="the attack session's output directory")
    args = parser.parse_args()
    log = (args.session / "serial.log").read_text(encoding="utf-8", errors="replace")
    print(markers(log, args.session), end="")
    return 0


if __name__ == "__main__":
    sys.exit(main())
