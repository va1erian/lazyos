"""LazyWeb+'s hung-page session (`hang_run.py`, `tools/screenshot/examples/lazywebplus_hang.json`,
issue #862).

One boot of the image `run.py` builds: the page `https://theoldnet.com/webplus/hang.html`; a click on
its button starts a loop that never ends. The window must say so (a bar), the bar's Wait must put it
away, the toolbar's Reload must end the stuck process and load the page again in a new one, and
after a second hang the bar's Stop must end that process and leave the page stopped. The shots are
for a person to read: the bar over the page (`h01_bar`), after Wait (`h02_waited`), the page again
(`h03_reloaded`), the bar once more (`h04_bar_again`) and the stopped page (`h05_stopped`).

Regenerate the checked-in example with `python tools/webplus/hang_session.py --write`.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import hang_judge as h  # noqa: E402
import judge as j  # noqa: E402
import session as s  # noqa: E402

EXAMPLE = ROOT / "tools" / "screenshot" / "examples" / "lazywebplus_hang.json"
#: The page's one button, in page pixels (`fixtures/theoldnet.com/webplus/hang.html`).
SPIN = (180, 35)
#: Hang-and-Reload cycles after the first, each with the bar's own Reload.
CYCLES = 3


def hang(timeout: float = 300.0) -> list[dict]:
    wait = lambda marker, secs=timeout, **more: {"wait_for": marker, "timeout": secs, **more}  # noqa: E731
    loaded = lambda url: f"{j.P}:LOAD:{url}"  # noqa: E731
    titled = lambda title: f"{j.P}:TITLE:{title}"  # noqa: E731
    ready, spinning = j.caption(h.READY), j.caption(h.SPINNING)
    return [
        *s.boot_gates(timeout),
        wait(loaded(j.START)),
        *s.navigate(h.PAGE, loaded(h.PAGE), timeout),
        wait(titled(h.READY)),
        {"wait": 1.0},
        # The loop starts; WebKit's ping goes unanswered after six seconds and the bar comes up.
        s.page_click(ready, SPIN, timeout, titled(h.SPINNING)),
        wait(f"{j.P}:WEBPROCESS:UNRESPONSIVE:", 120),
        wait(f"{j.P}:HANG:SHOW", 30),
        {"wait": 1.5},
        {"shot": "h01_bar"},
        # Wait puts the bar away and ends nothing.
        {"click_at": {"window": spinning, "widget": "hang_wait"}, "timeout": timeout,
         "until": f"{j.P}:HANG:ANSWER:wait", "retries": 2},
        {"wait": 1.5},
        {"shot": "h02_waited"},
        # The toolbar's Reload is still a way out: the stuck process ends and the page loads in a new one.
        {"click_at": {"window": spinning, "widget": "reload"}, "timeout": timeout,
         "until": f"{j.P}:HANG:ANSWER:reload", "retries": 2},
        wait(f"{j.P}:WEBPROCESS:GONE:", 60),
        wait(titled(h.READY), occurrence=2),
        {"wait": 2.0},
        {"shot": "h03_reloaded"},
        # Hung and recovered again and again, with the bar's own Reload: every cycle ends a process and
        # keeps its proxy (patch 0021), and the judge counts what was kept.
        *[step for cycle in range(CYCLES) for step in (
            s.page_click(ready, SPIN, timeout, titled(h.SPINNING)),
            wait(f"{j.P}:WEBPROCESS:UNRESPONSIVE:", 120, occurrence=2 + cycle),
            wait(f"{j.P}:HANG:SHOW", 30, occurrence=2 + cycle),
            {"wait": 1.0},
            {"click_at": {"window": spinning, "widget": "hang_reload"}, "timeout": timeout,
             "until": f"{j.P}:HANG:ANSWER:reload", "retries": 2},
            wait(titled(h.READY), occurrence=3 + cycle),
            {"wait": 1.5},
        )],
        {"shot": "h04_cycles"},
        # Hung once more: this time the bar's Stop.
        s.page_click(ready, SPIN, timeout, titled(h.SPINNING)),
        wait(f"{j.P}:WEBPROCESS:UNRESPONSIVE:", 120, occurrence=2 + CYCLES),
        wait(f"{j.P}:HANG:SHOW", 30, occurrence=2 + CYCLES),
        {"wait": 1.5},
        {"shot": "h05_bar_again"},
        {"click_at": {"window": spinning, "widget": "hang_stop"}, "timeout": timeout,
         "until": f"{j.P}:HANG:ANSWER:stop", "retries": 2},
        wait(f"{j.P}:FAIL:webprocess:stopped", 60),
        {"wait": 2.0},
        {"shot": "h06_stopped"},
        {"click_at": {"target": f"title:{spinning}", "offset": list(s.CLOSE_AT_NORMAL)},
         "timeout": 60, "until": f"{j.P}:QUIT:", "retries": 2},
        {"wait": 9.0},
        {"quit": True},
    ]


def render(items: list[dict]) -> str:
    return json.dumps(items, indent=1) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--write", action="store_true", help="write the checked-in example")
    parser.add_argument("--check", action="store_true", help="fail when the checked-in example is stale")
    args = parser.parse_args()
    text = render(hang())
    if args.write:
        EXAMPLE.write_text(text, encoding="utf-8", newline="\n")
        print(f"wrote {EXAMPLE}")
    if args.check and (not EXAMPLE.is_file() or EXAMPLE.read_text(encoding="utf-8") != text):
        print(f"{EXAMPLE} is stale: run python tools/webplus/hang_session.py --write", file=sys.stderr)
        return 1
    if not args.write and not args.check:
        print(text, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
