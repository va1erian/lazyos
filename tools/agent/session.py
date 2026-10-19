#!/usr/bin/env python3
"""The guest session of LazyAgent's harness (`run.py`), also checked in as
`tools/screenshot/examples/agent.json`.

On a desktop image with a Terminal (`LAZYOS_XUI_AUTOSTART=term`,
`LAZYOS_UI_PROBE=1`) and the mock Fireworks on the host:

1. the Terminal launches the app through `init.Launch` from a `rhai` script
   (as `lazysky.json` does);
2. the consent screen is accepted (Tab to the button, Space);
3. a wrong key is typed into the key page and a message sent: the mock
   answers 401 and the app goes back to the key page;
4. the real key (`{"type_secret": "AGENT_TEST_KEY"}`: never written to the
   script, the serial log or the screenshots) is typed, a message sent and
   the Markdown answer streams in (`AGENT:CHAT:FIRST`, `AGENT:CHAT:DONE`);
5. a long reply is scrolled up (the view stops following), then a reply that
   stalls is stopped with Escape (`AGENT:CHAT:FAIL:stopped`);
6. Ctrl+N starts a new chat and Ctrl+, opens the settings; Escape goes back.

    python tools/agent/session.py --write   # regenerate the example
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
EXAMPLE = ROOT / "tools" / "screenshot" / "examples" / "agent.json"
APP = "os.lazy.agent"
WINDOW = "AI Agent"
#: The wrong key (not a secret: it is what a mistyped one looks like).
WRONG_KEY = "wrong-key-for-the-harness"
#: Wheel notches scrolled up in a long reply.
UP_NOTCHES = 30
NOTCH = 5


def typed(line: str, at: float | None = None) -> list[dict]:
    first = {"type": line}
    if at is not None:
        first = {"at": at, **first}
    return [first, {"at": (at or 0) + 0.5, "key": "enter"}]


def ctrl(key: str) -> list[dict]:
    return [{"key_down": "ctrl"}, {"key": key}, {"key_up": "ctrl"}]


def steps(timeout: float = 300.0, secret: str = "AGENT_TEST_KEY", live: bool = False) -> list[dict]:
    """The session. `live` (the real service) leaves out what only the mock
    can provoke: a refused key, a long reply and a stalled one."""
    out: list[dict] = [
        {"wait_for": "PKGD:PROVISION:DONE", "timeout": 600},
        {"wait_for": "TERM:UP:PASS", "timeout": 600},
        {"wait_for": "NETD:ADDR 10.0.2.15", "timeout": 300},
    ]
    # A fresh run starts from the consent screen, even on a reused image.
    out += typed("rm -rf ~/.apps/os.lazy.agent", 0.5)
    out += typed("echo 'let i = msg::connect(\"os.lazy.init.v1\");' > /tmp/l.rhai", 2.0)
    out += typed("echo 'for a in os::args() { print(i.launch(a, \"\", 0)); }' >> /tmp/l.rhai", 3.5)
    out += typed(f"clear; rhai /tmp/l.rhai {APP}", 5.0)
    out += [
        {"wait_for": "AGENT:UP:PASS", "timeout": timeout},
        {"wait_for": "AGENT:READY:PASS", "timeout": timeout},
        {"wait": 1.5},
        {"shot": "01_consent"},
        # The consent button is the first control: Tab, then Space.
        {"key": "tab"},
        {"key": "space"},
        {"wait_for": "AGENT:CONSENT:PASS", "timeout": 60},
        {"wait": 1.0},
        {"shot": "02_key"},
    ]
    keys_set = 1
    if not live:
        keys_set = 2
        out += [
            # A wrong key: the mock answers 401 and the app asks again.
            {"type": WRONG_KEY, "delay": 0.05},
            {"key": "enter"},
            {"wait_for": "AGENT:KEY:PASS:SET", "timeout": 60, "occurrence": 1},
            {"wait": 1.0},
            {"type": "hello", "delay": 0.05},
            {"key": "enter"},
            {"wait_for": "AGENT:CHAT:FAIL:401", "timeout": timeout},
            {"wait": 1.5},
            {"shot": "03_key_refused"},
        ]
    out += [
        # The real key.
        {"type_secret": secret, "delay": 0.05},
        {"key": "enter"},
        {"wait_for": "AGENT:KEY:PASS:SET", "timeout": 60, "occurrence": keys_set},
        {"wait": 1.0},
        {"type": "What is a DHCP lease?", "delay": 0.05},
        {"key": "enter"},
        {"wait_for": "AGENT:CHAT:FIRST:", "timeout": timeout},
        {"wait": 0.5},
        {"shot": "04_streaming"},
        {"wait_for": "AGENT:CHAT:DONE:", "timeout": timeout, "occurrence": 1},
        {"wait": 2.0},
        {"shot": "05_answer"},
    ]
    if live:
        out += [
            {"type": "In one sentence, what is a subnet mask?", "delay": 0.05},
            {"key": "enter"},
            {"wait_for": "AGENT:CHAT:DONE:", "timeout": timeout, "occurrence": 2},
            {"wait": 2.0},
            {"shot": "06_second"},
        ]
    else:
        out += [
            # A long reply, scrolled up.
            {"type": "LONG answer please", "delay": 0.05},
            {"key": "enter"},
            {"wait_for": "AGENT:CHAT:DONE:", "timeout": timeout, "occurrence": 2},
            {"wait": 2.0},
            {"move_to": {"window": WINDOW}},
        ]
        out += [step for _ in range(UP_NOTCHES // NOTCH) for step in ({"mouse_scroll": NOTCH}, {"wait": 0.2})]
        out += [
            {"wait_for": "AGENT:FOLLOW:0", "timeout": 60},
            {"wait": 1.5},
            {"shot": "06_scrolled"},
            # A reply that stalls, stopped with Escape.
            {"type": "STALL please", "delay": 0.05},
            {"key": "enter"},
            {"wait_for": "AGENT:CHAT:FIRST:", "timeout": timeout, "occurrence": 3},
            {"wait": 0.5},
            {"key": "esc"},
            {"wait_for": "AGENT:CHAT:FAIL:stopped", "timeout": 60},
            {"wait": 1.5},
            {"shot": "07_stopped"},
        ]
    out += ctrl("n")
    out += [{"wait": 1.0}] + ctrl(",")
    out += [
        {"wait": 1.5},
        {"shot": "08_settings"},
        {"key": "esc"},
        {"wait": 1.0},
        {"shot": "09_back"},
        # Close the window: the app says it quit cleanly.
        {"key_down": "alt"},
        {"key": "f4"},
        {"key_up": "alt"},
        {"wait_for": "AGENT:QUIT:PASS", "timeout": 60},
        {"wait": 1.0},
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
            print(f"{EXAMPLE.relative_to(ROOT)} is stale: run `python tools/agent/session.py --write`")
            return 1
        return 0
    sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
