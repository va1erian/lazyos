#!/usr/bin/env python3
"""A reproduction bundle for scrolling `about:history` with a long history (webmonkey format 1).

The page is LazyWeb's own and is built from every visit, so its cost grows with the history; a ten-minute
monkey run (about 110 visits) saw 500-700 ms without a drawn frame. This writes the bundle that makes such a
history on its own: `--visits` fast, distinct pages (`example.com/?visit=<n>`), then Ctrl+H and wheel
flings both ways.

    python tools/lazyweb/history_repro.py --out shots/history_repro.json
    python tools/lazyweb/webmonkey.py --repro shots/history_repro.json --attempts 1

`webmonkey.py` prints REPRODUCED (exit 1) when a scroll gap over `--jank-ms` shows; the numbers are the
`LAZYWEB:INFO:SMOOTH` lines of `shots/webmonkey/repro_attempt_1/serial.log` (`gapmax` with `wheel` > 0).
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import webmonkey_repro as repros

#: Where the wheel turns: the middle of a 1280x720 screen, inside the page area.
POINT = [640, 400]


def steps(visits: int, load_wait: float, rounds: int) -> list[dict]:
    """The visits, the history page, then `rounds` of a fling down and a fling up."""
    rows = [{"act": {"a": "navigate", "url": f"https://example.com/?visit={n}"}, "wait": load_wait}
            for n in range(visits)]
    rows.append({"act": {"a": "chrome", "chord": ["ctrl", "h"]}, "wait": 1.0})
    for _ in range(rounds):
        for n in (-5, 5):
            rows.append({"act": {"a": "scroll", "at": POINT, "n": n, "times": 25, "gap": 0.03}, "wait": 0.5})
            rows.append({"act": {"a": "wheel_burst", "at": POINT, "seed": 7, "count": 200}, "wait": 0.5})
    return rows


def bundle(visits: int, load_wait: float, rounds: int) -> dict:
    return {
        "format": repros.FORMAT, "kind": "scroll-gap", "site": "about:history",
        "finding": f"jank: no frame was drawn while scrolling about:history after {visits} visits",
        "page": "about:history", "measured_ms": None, "found_at_s": 0, "found_at_action": 0,
        "replay": {"page": "https://example.com/", "steps": steps(visits, load_wait, rounds)},
        "run": {"commit": "", "dirty": False, "branch": "", "seed": 1, "screen": "1280x720", "memory": None,
                "accel": "auto", "start": "", "duration": 0, "jank_ms": 100.0, "answer_ms": 400.0,
                "build": "python tools/lazyweb/webmonkey.py --build --duration 1"},
        "serial_excerpt": [],
    }


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--out", default="shots/history_repro.json")
    p.add_argument("--visits", type=int, default=120)
    p.add_argument("--load-wait", type=float, default=3.0, help="seconds between two address-bar navigations")
    p.add_argument("--rounds", type=int, default=3)
    args = p.parse_args()
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(bundle(args.visits, args.load_wait, args.rounds), indent=1), encoding="utf-8")
    print(f"{out}: {args.visits} visits, {args.rounds} rounds of flings")


if __name__ == "__main__":
    main()
