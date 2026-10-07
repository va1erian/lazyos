#!/usr/bin/env python3
"""Judge the trusted prompt scenarios (docs/accounts-plan.md U2, issue #626).

The attack session asks `elevd` for a change (`attack.sh prompt_over`), so
`xuid` shows its administrator prompt; five seconds later the session opens a
window (the Counter), which takes the focus. The harness screenshots the
screen before (`prompt_up`) and after the window opened (`prompt_window`),
types `inject` and presses Escape. Two markers come out of that:

* `prompt_over`: nothing drew over the prompt. The window really opened while
  the prompt was up (its `XUIAPP:COUNTER:PASS` lies between `XUID:PROMPT:UP`
  and `XUID:PROMPT:DONE`), and the prompt's panel is the same in both shots
  (at least `SAME_MIN` of its pixels).
* `prompt_keys`: the typing went to the prompt, not to any client: the prompt
  reports it took the keys (`XUID:PROMPT:DONE outcome=cancelled keys=<n>`,
  n >= 7: six letters and Escape), `elevd` recorded the cancel, and no client
  ran a command `inject`.

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
PANEL = (480, 240)
#: The fraction of the panel's pixels that must not change.
SAME_MIN = 0.98
#: What the session types into the prompt before Escape.
TYPED = "inject"


def panel_rect(width: int, height: int) -> tuple[int, int, int, int]:
    """The panel's rectangle on a `width` x `height` screen (scale 1, or 2
    from 2560x1440 as `xuid` picks it)."""
    scale = 2 if width >= 2560 else 1
    w, h = PANEL[0] * scale, PANEL[1] * scale
    return (width - w) // 2, (height - h) // 2, w, h


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


def _index(log: str, needle: str) -> int:
    return log.find(needle)


def over_marker(log: str, shots: Path) -> str:
    """`prompt_over`'s marker (see the module docs)."""
    up, done = _index(log, "XUID:PROMPT:UP"), _index(log, "XUID:PROMPT:DONE")
    window = _index(log, "XUIAPP:COUNTER:PASS")
    if up < 0 or done < 0:
        return "ACCT:ATTACK:prompt_over:ERROR:noprompt"
    if not up < window < done:
        return "ACCT:ATTACK:prompt_over:ERROR:nowindow"
    first, second = shots / "shot_prompt_up.png", shots / "shot_prompt_window.png"
    if not first.is_file() or not second.is_file():
        return "ACCT:ATTACK:prompt_over:ERROR:noshots"
    same = same_fraction(first, second)
    outcome = "BLOCKED" if same >= SAME_MIN else "SUCCEEDED"
    return f"ACCT:ATTACK:prompt_over:{outcome}:panel_same={same:.3f}"


def keys_marker(log: str) -> str:
    """`prompt_keys`'s marker (see the module docs)."""
    done = re.search(r"XUID:PROMPT:DONE outcome=(\w+) keys=(\d+)", log)
    if not done:
        return "ACCT:ATTACK:prompt_keys:ERROR:noprompt"
    outcome, keys = done.group(1), int(done.group(2))
    # elevd audited the cancel, and the asker heard ECANCELED (elev_wait.rhai).
    cancelled = (re.search(r"ELEVD:REQUEST op=time\.set .*outcome=cancelled", log)
                 and "ACCT:PROMPT:CANCELLED" in log)
    leaked = f"TERM:CMD:{TYPED}" in log or f"TERM:OUT:{TYPED}" in log
    if leaked:
        return f"ACCT:ATTACK:prompt_keys:SUCCEEDED:a_client_got_{TYPED}"
    if outcome != "cancelled" or not cancelled or keys < len(TYPED) + 1:
        return f"ACCT:ATTACK:prompt_keys:ERROR:outcome={outcome}_keys={keys}"
    return f"ACCT:ATTACK:prompt_keys:BLOCKED:keys={keys}"


def markers(log: str, shots: Path) -> str:
    """Both markers, one per line, for `attack_judge.judge`."""
    return over_marker(log, shots) + "\n" + keys_marker(log) + "\n"


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
