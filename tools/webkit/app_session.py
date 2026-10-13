"""The WebKit browser's session script (`app_run.py`, `tools/screenshot/examples/webkit.json`).

The image is a desktop built with `LAZYOS_WEBKIT=1`, the UI probe and `LAZYOS_XUI_AUTOSTART=webkit`,
so the browser's window is the only one and opens at login. The script types into the address
field (Ctrl+L), waits for each page's markers (`appjudge.py` lists them), clicks a link and a text
field by page coordinates (the window prints `UI:WIDGET name=page_origin`, one pixel at the page's
top-left, so `offset` is the page coordinate), types into the form, submits it from the keyboard,
turns the wheel over a tall page of static bands (so the judge can measure how far it moved),
goes Back, loads an address nothing listens on (the window says so) and a page that never
answers in time and presses Stop (not a failure), maximizes the window, and closes it from
its title bar (the UI process ends both children and says so); a screenshot after each. Regenerate the checked-in example with
`python tools/webkit/app_session.py --write`.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import appjudge as aj  # noqa: E402

EXAMPLE = ROOT / "tools" / "screenshot" / "examples" / "webkit.json"
#: The link's centre and the text field's, in page pixels (`fixtures/theoldnet.com/webkit/`).
LINK_AT = (150, 50)
FIELD_AT = (200, 42)
#: The title bar's maximize button, from the middle of the title bar (1004 px wide; `xuid` draws
#: its buttons at the right).
MAXIMIZE_AT = (472, 0)
#: The close button, from the middle of the maximized title bar (1280 px wide).
CLOSE_AT = (630, 0)
#: Where the pointer rests for the wheel: inside the page, away from links.
WHEEL_AT = (600, 300)


def navigate(url: str, loaded: str | None, timeout: float, until: str | None = None) -> list[dict]:
    """Ctrl+L, the address, Enter; then wait for the page's `LOAD` marker (or `until`)."""
    return [
        {"key_down": "ctrl"}, {"key": "l"}, {"key_up": "ctrl"},
        {"wait": 1.0},
        {"type": url, "delay": 0.05},
        {"key": "enter", "until": until or f"WEBKIT:W5:LOAD:{loaded}", "timeout": timeout},
    ]


def page_click(window: str, at: tuple[int, int], timeout: float, until: str | None = None) -> dict:
    """A click at page coordinates of the window titled `window`; with `until`, re-sent
    (twice) until that marker appears."""
    step: dict = {"click_at": {"window": window, "widget": "page_origin", "offset": list(at)}, "timeout": timeout}
    if until:
        step.update(until=until, retries=2)
    return step


def steps(timeout: float = 300.0) -> list[dict]:
    wait = lambda marker, secs=timeout: {"wait_for": marker, "timeout": secs}  # noqa: E731
    return [
        wait("PKGD:PROVISION:DONE", 600),
        wait("WEBKIT:W5:UP"),
        wait("WEBKIT:W5:UI:UP"),
        wait("WEBKIT:W5:UI:FRAME:"),
        wait("NETD:RESOLV wrote"),
        {"wait": 2.0},
        {"shot": "00_window"},
        # example.com over plain HTTP.
        *navigate("http://example.com/", aj.EXAMPLE_URL, timeout),
        wait(f"WEBKIT:W5:TITLE:{aj.EXAMPLE_TITLE}"),
        {"wait": 2.0},
        {"shot": "01_example"},
        # theoldnet.com over HTTPS, with PNG, JPEG and GIF pictures.
        *navigate(aj.OLDNET_URL, aj.OLDNET_URL, timeout),
        wait(f"WEBKIT:W5:TITLE:{aj.OLDNET_TITLE}"),
        {"wait": 5.0},
        {"shot": "02_theoldnet"},
        {"wait": 3.0},
        {"shot": "03_theoldnet_later"},
        # The wheel: a page of static bands moves under the pointer by a measured number of rows.
        *navigate(aj.TALL_URL, aj.TALL_URL, timeout),
        wait(f"WEBKIT:W5:TITLE:{aj.TALL_TITLE}"),
        {"wait": 3.0},
        {"shot": "03b_tall"},
        {"move_to": {"window": aj.caption(aj.TALL_TITLE), "widget": "page_origin", "offset": list(WHEEL_AT)},
         "timeout": 60},
        {"wait": 0.5},
        {"mouse_scroll": -5},
        {"wait": 3.0},
        {"shot": "04_scrolled"},
        # Script in the page.
        *navigate(aj.JS_URL, aj.JS_URL, timeout),
        wait(f"WEBKIT:W5:JS:{aj.JS_TITLE}"),
        {"wait": 2.0},
        {"shot": "05_js"},
        # A link, clicked with the mouse.
        *navigate(aj.LINKS_URL, aj.LINKS_URL, timeout),
        wait(f"WEBKIT:W5:TITLE:{aj.LINKS_TITLE}"),
        {"wait": 3.0},
        page_click(aj.caption(aj.LINKS_TITLE), LINK_AT, timeout, f"WEBKIT:W5:CLICK:{aj.FORM_URL}"),
        wait(f"WEBKIT:W5:TITLE:{aj.FORM_TITLE}"),
        {"wait": 3.0},
        {"shot": "06_form"},
        # A text field, typed into, then submitted from the keyboard.
        page_click(aj.caption(aj.FORM_TITLE), FIELD_AT, 120),
        {"wait": 1.0},
        {"type": aj.INPUT_TEXT, "delay": 0.08},
        {"wait": 2.0},
        {"shot": "07_typed"},
        {"key": "enter", "until": f"WEBKIT:W5:INPUT:{aj.ECHO_TITLE}", "timeout": timeout},
        {"wait": 3.0},
        {"shot": "08_echo"},
        # Back to the form, from the toolbar.
        {"click_at": {"window": aj.caption(aj.ECHO_TITLE), "widget": "back"}, "timeout": timeout,
         "until": f"WEBKIT:W5:LOAD:{aj.FORM_URL}", "retries": 1},
        {"wait": 3.0},
        {"shot": "09_back"},
        # An address nothing listens on: the load fails, the window says so, the form stays.
        *navigate(aj.FAIL_URL, None, timeout, until=f"WEBKIT:W5:FAIL:provisional:{aj.FAIL_URL}"),
        wait(f"WEBKIT:W5:UI:ERROR:Could not load {aj.FAIL_URL}"),
        {"wait": 1.0},
        {"shot": "10_failed"},
        # A page that does not answer in time; Stop ends the load, and that is not a failure.
        *navigate(aj.SLOW_URL, None, timeout, until=f"WEBKIT:W5:START:{aj.SLOW_URL}"),
        {"wait": 2.0},
        {"click_at": {"window": aj.caption(aj.FORM_TITLE), "widget": "reload"}, "timeout": timeout,
         "until": "WEBKIT:W5:CANCELLED:", "retries": 2},
        {"wait": 2.0},
        {"shot": "11_stopped"},
        # Maximize: the page is laid out again at the new size and the engine paints it.
        {"click_at": {"target": f"title:{aj.caption(aj.FORM_TITLE)}", "offset": list(MAXIMIZE_AT)},
         "timeout": 60, "until": "WEBKIT:W5:INFO:RESIZE:", "retries": 2},
        {"wait": 4.0},
        {"shot": "12_maximized"},
        # Close the window from its title bar: the run loop ends, both children are ended and reaped.
        {"click_at": {"target": f"title:{aj.caption(aj.FORM_TITLE)}", "offset": list(CLOSE_AT)},
         "timeout": 60, "until": "WEBKIT:W5:QUIT:", "retries": 2},
        {"wait": 3.0},
        {"quit": True},
    ]


def render(items: list[dict]) -> str:
    return json.dumps(items, indent=1) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--write", action="store_true", help=f"write {EXAMPLE.relative_to(ROOT)}")
    parser.add_argument("--check", action="store_true", help="fail when the checked-in example is stale")
    args = parser.parse_args()
    text = render(steps())
    if args.write:
        EXAMPLE.write_text(text, encoding="utf-8", newline="\n")
        print(f"wrote {EXAMPLE}")
    if args.check and (not EXAMPLE.is_file() or EXAMPLE.read_text(encoding="utf-8") != text):
        print(f"{EXAMPLE} is stale: run python tools/webkit/app_session.py --write", file=sys.stderr)
        return 1
    if not args.write and not args.check:
        print(text, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
