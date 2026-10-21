"""The session script of the webd harness (`run.py`, `tools/screenshot/examples/webd_light.json`).

One boot of the image `run.py` builds: a desktop that opens the Terminal and the demo client
(`LAZYOS_XUI_AUTOSTART=webddemo,term`: the demo opens first, so the Terminal does not cover it) with `webd` (a resident app, started at login) serving light
views. The demo shows one view of a blue page with a link, a form and a picture that a light view must
never load. The session

1. waits for webd, the demo and the network (so a leak could reach the host), then for the page's first
   frame (`WEBDDEMO:FRAME:PASS colour=blue`) and takes `w01_page`;
2. clicks the page's link by its probe rectangle (`UI:WIDGET name=link`, printed by the demo): the click
   must come back as `WEBDDEMO:LINK:<url>` and load nothing (`w02_link`), then types into the form's
   field and submits it (`w03_form`; the submit comes back as a link too);
3. the loss test (unless `skip_loss`): reads the web process id from the engine's launch line, switches
   to the Terminal with Alt+Tab (the compositor keeps that chord), kills the process, and waits for the
   demo to say `WEBDDEMO:LOST:crashed`, make a new view and show the page again
   (`WEBDDEMO:RECREATE:PASS`); switches back and takes `w04_recreated`;
4. quits the demo (Ctrl+Q; a plain `q` is for the form) and waits for webd to forget its views.

Regenerate the checked-in example with `python tools/webd/session.py --write`; `--check` fails when it
is stale. The shots are for a person to read as well as for `judge.py`.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import judge as j  # noqa: E402

EXAMPLE = ROOT / "tools" / "screenshot" / "examples" / "webd_light.json"
#: The simple session for the app alone (`new_app.py`'s `xui_webddemo.json`).
EXAMPLE_APP = ROOT / "tools" / "screenshot" / "examples" / "xui_webddemo.json"

ALT_TAB = [{"key_down": "alt"}, {"key": "tab"}, {"key_up": "alt"}]


def boot_gates(timeout: float) -> list[dict]:
    wait = lambda marker, secs=timeout: {"wait_for": marker, "timeout": secs}  # noqa: E731
    return [wait("PKGD:PROVISION:DONE", 600), wait("TERM:UP:PASS"), wait(f"{j.W}:UP:PASS"),
            wait(f"{j.D}:UP:PASS"), wait("NETD:RESOLV wrote"), {"wait": 2.0}]


def light(timeout: float = 300.0, skip_loss: bool = False) -> list[dict]:
    """The harness session. `skip_loss` leaves out the kill (no way to end the process from the guest)."""
    wait = lambda marker, secs=timeout, **more: {"wait_for": marker, "timeout": secs, **more}  # noqa: E731
    click = lambda widget, **more: {"click_at": {"window": j.WINDOW, "widget": widget}, "timeout": 60,  # noqa: E731
                                   **more}
    steps: list[dict] = [
        *boot_gates(timeout),
        wait(f"{j.D}:VIEW:PASS id="),
        wait(f"{j.D}:FRAME:PASS colour=blue"),
        # The Terminal opened after the demo and sits over it: a click on the demo's title bar raises it
        # (and gives it the keyboard, which the form below needs).
        {"click_at": {"target": f"title:{j.WINDOW}"}, "timeout": 60},
        {"wait": 1.5},
        {"shot": "w01_page"},
        # The link comes back as an event and loads nothing.
        click("link", until=f"{j.D}:LINK:{j.LINK_URL}", retries=2),
        {"wait": 1.0},
        {"shot": "w02_link"},
        # The form: a click focuses the field, the keys reach the page, Enter submits (a link too).
        click("field"),
        {"wait": 0.5},
        {"type": "ok", "delay": 0.1},
        {"key": "enter"},
        {"wait": 1.5},
        {"shot": "w03_form"},
    ]
    if not skip_loss:
        steps += [
            # The engine says which process it started for the group (`PROCESS:LAUNCH:web:<pid>`).
            wait(r"PROCESS:LAUNCH:web:(\d+)", 60, regex=True, capture="webpid"),
            *ALT_TAB,
            {"wait": 1.0},
            {"type": "kill -9 ${webpid}"},
            {"wait": 0.6},
            {"key": "enter", "until": "TERM:CMD:kill -9 ", "timeout": 30, "retries": 2},
            wait(f"{j.D}:LOST:", 120),
            wait(f"{j.D}:RECREATE:PASS", 240),
            *ALT_TAB,
            {"wait": 2.0},
            {"shot": "w04_recreated"},
        ]
    steps += [
        # Ctrl+Q: the demo quits, its channel closes, webd destroys its views.
        {"key_down": "ctrl"},
        {"key": "q", "until": f"{j.D}:QUIT:PASS", "timeout": 60, "retries": 2},
        {"key_up": "ctrl"},
        {"wait": 5.0},
        {"quit": True},
    ]
    return steps


def app(timeout: float = 240.0) -> list[dict]:
    """The app alone, for a look: the window up and a picture (`xui_webddemo.json`)."""
    return [
        {"wait_for": f"{j.D}:UP:PASS", "timeout": timeout},
        {"wait": 3.0},
        {"shot": "webddemo_initial"},
        {"key_down": "ctrl"},
        {"key": "q", "until": f"{j.D}:QUIT:PASS", "timeout": 60, "retries": 2},
        {"key_up": "ctrl"},
        {"quit": True},
    ]


def render(items: list[dict]) -> str:
    return json.dumps(items, indent=1) + "\n"


def examples() -> dict[Path, str]:
    """The checked-in sessions and what they render to."""
    return {EXAMPLE: render(light()), EXAMPLE_APP: render(app())}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--write", action="store_true", help="write the checked-in examples")
    parser.add_argument("--check", action="store_true", help="fail when a checked-in example is stale")
    args = parser.parse_args()
    wanted = examples()
    if args.write:
        for path, text in wanted.items():
            path.write_text(text, encoding="utf-8", newline="\n")
            print(f"wrote {path}")
    stale = [p for p, text in wanted.items() if not p.is_file() or p.read_text(encoding="utf-8") != text]
    if args.check and stale:
        print("stale: " + ", ".join(str(p) for p in stale) + "; run python tools/webd/session.py --write",
              file=sys.stderr)
        return 1
    if not args.write and not args.check:
        print(render(light()), end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
