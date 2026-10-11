#!/usr/bin/env python3
"""The guest session of LazySky's harness (`run.py`), also checked in as
`tools/screenshot/examples/lazysky.json`.

On a desktop image with a Terminal (`LAZYOS_XUI_AUTOSTART=term`,
`LAZYOS_UI_PROBE=1`):

1. the Terminal writes `settings.toml` into LazySky's folder, pointing the
   login at the mock (`init` hands an app one absolute path at most, so the
   server cannot be an argument), and launches the app through `init.Launch`
   from a `rhai` script (as `mail_tls.json` does);
2. the handle is typed, Tab, the app password (`{"type_secret":
   "LAZYSKY_PASSWORD"}`: never written to the script), Tab, Space on the
   Sign in button;
3. once the timeline is up the pointer goes over the window and the wheel
   scrolls 150 notches down and 50 up, with a screenshot at each stage;
4. a click in the middle of the feed (a thread or the viewer, whichever row
   or picture is there), then Alt+Left back.

    python tools/lazysky/session.py --write   # regenerate the example
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
EXAMPLE = ROOT / "tools" / "screenshot" / "examples" / "lazysky.json"
SERVICE = "https://bsky.test:8443"
HANDLE = "me.test"
APP = "os.lazy.lazysky"
#: Wheel notches per step, steps down and steps up.
NOTCH = 5
DOWN_STEPS = 30
UP_STEPS = 10


def typed(line: str, at: float | None = None) -> list[dict]:
    first = {"type": line}
    if at is not None:
        first = {"at": at, **first}
    return [first, {"at": (at or 0) + 0.5, "key": "enter"}]


def steps(timeout: float = 300.0) -> list[dict]:
    out: list[dict] = [
        {"wait_for": "PKGD:PROVISION:DONE", "timeout": 600},
        {"wait_for": "TERM:UP:PASS", "timeout": 600},
        {"wait_for": "NETD:ADDR 10.0.2.15", "timeout": 300},
    ]
    out += typed("mkdir -p ~/.apps/os.lazy.lazysky", 1.0)
    out += typed("cd ~/.apps/os.lazy.lazysky", 2.5)
    out += typed(f"echo 'service = \"{SERVICE}\"' > settings.toml", 4.0)
    out += typed("echo 'let i = msg::connect(\"os.lazy.init.v1\");' > /tmp/l.rhai", 5.5)
    out += typed("echo 'for a in os::args() { print(i.launch(a, \"\", 0)); }' >> /tmp/l.rhai", 7.0)
    out += typed(f"clear; rhai /tmp/l.rhai {APP}", 8.5)
    out += [
        {"wait_for": "SKY:UP:PASS", "timeout": timeout},
        {"wait": 2.0},
        {"shot": "01_login"},
        {"type": HANDLE, "delay": 0.05},
        {"key": "tab"},
        {"type_secret": "LAZYSKY_PASSWORD", "delay": 0.05},
        {"key": "tab"},
        {"key": "space"},
        {"wait_for": f"SKY:LOGIN:PASS:{HANDLE}", "timeout": timeout},
        {"wait_for": "SKY:FEED:ROWS:30", "timeout": timeout},
        {"wait_for": "SKY:IMAGES:", "timeout": timeout},
        {"wait": 2.0},
        {"shot": "02_feed"},
        {"move_to": {"window": "LazySky"}},
    ]
    out += [step for _ in range(DOWN_STEPS) for step in ({"mouse_scroll": -NOTCH}, {"wait": 0.15})]
    out += [{"wait": 3.0}, {"shot": "03_scrolled_down"}]
    out += [step for _ in range(UP_STEPS) for step in ({"mouse_scroll": NOTCH}, {"wait": 0.15})]
    out += [{"wait": 3.0}, {"shot": "04_scrolled_up"}]
    out += [
        {"click_at": {"window": "LazySky"}},
        {"wait": 4.0},
        {"shot": "05_clicked"},
        {"key_down": "alt"},
        {"key": "left"},
        {"key_up": "alt"},
        {"wait": 3.0},
        {"shot": "06_back"},
        {"wait": 3.0},
        {"quit": True},
    ]
    return out


def render(items: list[dict]) -> str:
    return json.dumps(items, indent=1) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--write", action="store_true", help=f"write {EXAMPLE.relative_to(ROOT)}")
    parser.add_argument("--check", action="store_true", help="fail when the example is stale")
    args = parser.parse_args()
    text = render(steps())
    if args.write:
        EXAMPLE.write_text(text, encoding="utf-8")
        print(f"wrote {EXAMPLE.relative_to(ROOT)}")
        return 0
    if args.check:
        current = EXAMPLE.read_text(encoding="utf-8") if EXAMPLE.is_file() else ""
        if current != text:
            print(f"{EXAMPLE.relative_to(ROOT)} is stale: run `python tools/lazysky/session.py --write`")
            return 1
        return 0
    sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
