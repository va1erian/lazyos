"""LazyWeb's session scripts (`run.py`, `tools/screenshot/examples/lazyweb*.json`).

The image is a desktop built with `LAZYOS_LAZYWEB=1`, the UI probe and
`LAZYOS_XUI_AUTOSTART=lazyweb`, so the browser's window is the only one and opens at login on
its start page. The browser is a client of the resident daemon `webd`, which owns the engine and
keeps the cookies and the HTTP cache in its `browser` profile (`$HOME/.apps/os.lazy.webd/profiles/browser`);
the history and the downloads stay LazyWeb's (`$HOME/.apps/os.lazy.lazyweb`). Two boots of the same
image (both profiles must be found again):

* **first boot** (`first`): the start page, example.com over HTTP, theoldnet.com over HTTPS with
  its pictures, a page that sets a cookie, a page whose script opens an `alert`, a `confirm` and a
  `prompt` (answered from the keyboard) and then tries to start another program, run one of the
  browser's commands and open a window with no click (all three refused and reported), a page of
  links (a `mailto:` link, `target=_blank`, `window.open` from a click, an attachment, a slow
  attachment whose progress is on screen), `about:history` and `about:downloads` by key, the wheel
  over a tall page, a form typed into, Back, an address nothing answers, a page that never
  answers and Stop, Maximize, and a close from the title bar (webd destroys the view and the page's
  process ends);
* **second boot** (`second`): theoldnet.com again, which the HTTP cache on the profile serves (the
  first boot's copy: no request reaches the server), the history of the first is in `about:history`, the
  cookie rides on the request, and the Clear history button of the history page (a click on a link of one of the
  browser's own pages: the one `x-lazyweb:` command a person may give) empties it.

Clicks are at page coordinates through the window's `page_origin` probe control.
Regenerate the checked-in examples with `python tools/lazyweb/session.py --write`.
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

EXAMPLES = ROOT / "tools" / "screenshot" / "examples"
EXAMPLE_FIRST = EXAMPLES / "lazyweb.json"
EXAMPLE_SECOND = EXAMPLES / "lazyweb_second.json"
#: The ad blocker's two boots (`blocker.py`, issue #845).
EXAMPLE_BLOCKER = EXAMPLES / "lazyweb_blocker.json"
EXAMPLE_PERSISTED = EXAMPLES / "lazyweb_persisted.json"

#: The title bar's maximize button, from the middle of the title bar, and the close button from
#: the middle of the maximized one (1280 px wide).
MAXIMIZE_AT = (472, 0)
CLOSE_AT = (630, 0)
#: The close button of the window at its first size (1004 px wide, as the second boot has it).
CLOSE_AT_NORMAL = (491, 0)
#: Where the pointer rests for the wheel: inside the page, away from links.
WHEEL_AT = (600, 300)
#: The links page's boxes, in page pixels (`fixtures/theoldnet.com/lazyweb/links.html`): 360 x 70,
#: a row every 90 px.
BOX = {"mail": (180, 35), "blank": (180, 125), "open": (180, 215), "download": (180, 305),
       "slow": (180, 395)}
#: The form's text field and the first page's link (`fixtures/theoldnet.com/webkit/`).
FIELD_AT = (200, 42)
LINK_AT = (150, 50)
#: The history page's "Clear history" button, in page pixels: the note line under the title
#: (`pages.history`: 24 px of margin, a 22 px title) ends with it.
CLEAR_AT = (212, 63)


def navigate(url: str, until: str, timeout: float) -> list[dict]:
    """Ctrl+L, the address, Enter; then wait for `until` (the page's `LOAD` marker)."""
    return [
        {"key_down": "ctrl"}, {"key": "l"}, {"key_up": "ctrl"},
        {"wait": 1.0},
        {"type": url, "delay": 0.05},
        {"key": "enter", "until": until, "timeout": timeout},
    ]


def page_click(window: str, at: tuple[int, int], timeout: float, until: str | None = None) -> dict:
    """A click at page coordinates of the window titled `window`; with `until`, re-sent (twice)
    until that marker appears."""
    step: dict = {"click_at": {"window": window, "widget": "page_origin", "offset": list(at)},
                  "timeout": timeout}
    if until:
        step.update(until=until, retries=2)
    return step


def shortcut(key: str, until: str, timeout: float) -> list[dict]:
    """Ctrl+`key`, until `until` is on serial."""
    return [{"key_down": "ctrl"}, {"key": key, "until": until, "timeout": timeout}, {"key_up": "ctrl"}]


def back_to(window: str, until: str, timeout: float) -> list[dict]:
    """Alt+Left from `window`'s page, until `until` (the page it goes back to)."""
    return [{"key_down": "alt"}, {"key": "left", "until": until, "timeout": timeout}, {"key_up": "alt"}]


def boot_gates(timeout: float) -> list[dict]:
    wait = lambda marker, secs=timeout: {"wait_for": marker, "timeout": secs}  # noqa: E731
    return [wait("PKGD:PROVISION:DONE", 600), wait(f"{j.P}:UP:PASS"),
            wait("NETD:RESOLV wrote"), {"wait": 2.0}]


def first(timeout: float = 300.0) -> list[dict]:
    wait = lambda marker, secs=timeout: {"wait_for": marker, "timeout": secs}  # noqa: E731
    loaded = lambda url: f"{j.P}:LOAD:{url}"  # noqa: E731
    titled = lambda title: f"{j.P}:TITLE:{title}"  # noqa: E731
    return [
        *boot_gates(timeout),
        wait(loaded(j.START)),
        {"wait": 1.0},
        {"shot": "00_start"},
        # example.com over plain HTTP.
        *navigate(j.EXAMPLE_URL, loaded(j.EXAMPLE_URL), timeout),
        wait(titled(j.EXAMPLE_TITLE)),
        {"wait": 2.0},
        {"shot": "01_example"},
        # theoldnet.com over HTTPS, with PNG, JPEG and GIF pictures.
        *navigate(j.OLDNET_URL, loaded(j.OLDNET_URL), timeout),
        wait(titled(j.OLDNET_TITLE)),
        {"wait": 5.0},
        {"shot": "02_theoldnet"},
        # A cookie, set by the server: none came with the request.
        *navigate(j.COOKIE_URL, loaded(j.COOKIE_URL), timeout),
        wait(titled(j.COOKIE_NONE)),
        {"wait": 1.0},
        {"shot": "03_cookie"},
        # Script: three dialogs, answered from the keyboard, and what the page tries on its own.
        {"key_down": "ctrl"}, {"key": "l"}, {"key_up": "ctrl"}, {"wait": 1.0},
        {"type": j.SCRIPT_URL, "delay": 0.05},
        {"key": "enter", "until": f"{j.P}:DIALOG:alert", "timeout": timeout},
        {"wait": 1.5},
        {"shot": "04_alert"},
        {"key": "enter", "until": f"{j.P}:DIALOG:confirm", "timeout": timeout},
        {"wait": 1.0},
        {"shot": "05_confirm"},
        {"key": "enter", "until": f"{j.P}:DIALOG:prompt", "timeout": timeout},
        {"wait": 1.0},
        {"type": j.PROMPT_TEXT, "delay": 0.08},
        {"shot": "06_prompt"},
        {"key": "enter", "until": titled(j.SCRIPT_TITLE), "timeout": timeout},
        wait(f"{j.P}:LAUNCH:{j.EVIL_MAILTO}:BLOCKED"),
        wait(f"{j.P}:LAUNCH:{j.COMMAND_URL}:BLOCKED"),
        {"wait": 3.0},
        {"shot": "07_script"},
        # Links: another app, a new window, an attachment.
        *navigate(j.LINKS_URL, loaded(j.LINKS_URL), timeout),
        wait(titled(j.LINKS_TITLE)),
        {"wait": 2.0},
        {"shot": "08_links"},
        page_click(j.caption(j.LINKS_TITLE), BOX["mail"], timeout, f"{j.P}:LAUNCH:{j.MAIL_URL}:"),
        {"wait": 2.0},
        {"shot": "09_mail"},
        page_click(j.caption(j.LINKS_TITLE), BOX["blank"], timeout, f"{j.P}:POPUP:{j.BLANK_URL}"),
        wait(titled(j.BLANK_TITLE)),
        {"wait": 2.0},
        {"shot": "10_blank"},
        *back_to(j.caption(j.BLANK_TITLE), loaded(j.LINKS_URL), timeout),
        {"wait": 2.0},
        page_click(j.caption(j.LINKS_TITLE), BOX["open"], timeout, f"{j.P}:POPUP:{j.OPENED_URL}"),
        wait(titled(j.OPENED_TITLE)),
        {"wait": 2.0},
        {"shot": "11_opened"},
        *back_to(j.caption(j.OPENED_TITLE), loaded(j.LINKS_URL), timeout),
        {"wait": 2.0},
        page_click(j.caption(j.LINKS_TITLE), BOX["download"], timeout, f"{j.P}:DOWNLOAD:START:{j.DOWNLOAD_NAME}"),
        wait(f"{j.P}:DOWNLOAD:DONE:{j.DOWNLOAD_NAME}:{j.DOWNLOAD_BYTES}"),
        {"wait": 2.0},
        {"shot": "12_downloaded"},
        # A slow attachment: its progress is in the status bar while it runs.
        page_click(j.caption(j.LINKS_TITLE), BOX["slow"], timeout, f"{j.P}:DOWNLOAD:START:{j.SLOW_NAME}"),
        {"wait": 4.0},
        {"shot": "13_downloading"},
        wait(f"{j.P}:DOWNLOAD:DONE:{j.SLOW_NAME}:{j.SLOW_BYTES}"),
        # The history and the downloads, by key.
        *shortcut("h", loaded("about:history"), timeout),
        {"wait": 3.0},
        {"shot": "14_history"},
        *shortcut("j", loaded("about:downloads"), timeout),
        {"wait": 3.0},
        {"shot": "15_downloads"},
        # The wheel: a page of static bands moves under the pointer by a measured number of rows.
        *navigate(j.TALL_URL, loaded(j.TALL_URL), timeout),
        wait(titled(j.TALL_TITLE)),
        {"wait": 3.0},
        {"shot": "16_tall"},
        {"move_to": {"window": j.caption(j.TALL_TITLE), "widget": "page_origin", "offset": list(WHEEL_AT)},
         "timeout": 60},
        {"wait": 0.5},
        {"mouse_scroll": -5},
        {"wait": 3.0},
        {"shot": "17_scrolled"},
        # A form, typed into, submitted from the keyboard; Back.
        *navigate(j.LINKS_FORM_URL, loaded(j.LINKS_FORM_URL), timeout),
        wait(titled(j.LINKS_FORM_TITLE)),
        {"wait": 3.0},
        page_click(j.caption(j.LINKS_FORM_TITLE), LINK_AT, timeout, loaded(j.FORM_URL)),
        wait(titled(j.FORM_TITLE)),
        {"wait": 3.0},
        page_click(j.caption(j.FORM_TITLE), FIELD_AT, 120),
        {"wait": 1.0},
        {"type": j.INPUT_TEXT, "delay": 0.08},
        {"wait": 2.0},
        {"shot": "18_typed"},
        {"key": "enter", "until": titled(j.ECHO_TITLE), "timeout": timeout},
        {"wait": 3.0},
        {"shot": "19_echo"},
        {"click_at": {"window": j.caption(j.ECHO_TITLE), "widget": "back"}, "timeout": timeout,
         "until": loaded(j.FORM_URL), "retries": 1},
        {"wait": 2.0},
        # An address nothing listens on: the load fails and the window says so.
        *navigate(j.FAIL_URL, f"{j.P}:FAIL", timeout),
        {"wait": 1.0},
        {"shot": "20_failed"},
        # A page that does not answer in time; Stop ends the load, and that is not a failure.
        *navigate(j.SLOW_URL, f"{j.P}:NAV:{j.SLOW_URL}", timeout),
        {"wait": 2.0},
        {"click_at": {"window": j.caption(j.FORM_TITLE), "widget": "reload"}, "timeout": timeout,
         "until": f"{j.P}:STOPPED", "retries": 2},
        {"wait": 2.0},
        {"shot": "21_stopped"},
        # Maximize: the page is laid out again at the new size.
        {"click_at": {"target": f"title:{j.caption(j.FORM_TITLE)}", "offset": list(MAXIMIZE_AT)},
         "timeout": 60, "until": f"{j.P}:INFO:RESIZE:", "retries": 2},
        {"wait": 4.0},
        {"shot": "22_maximized"},
        # Close the window from its title bar: both children are ended and reaped.
        {"click_at": {"target": f"title:{j.caption(j.FORM_TITLE)}", "offset": list(CLOSE_AT)},
         "timeout": 60, "until": f"{j.P}:QUIT:", "retries": 2},
        # The profile is written by a flusher that runs every 5 s: wait it out before the stop.
        {"wait": 9.0},
        {"quit": True},
    ]


def second(timeout: float = 300.0) -> list[dict]:
    wait = lambda marker, secs=timeout: {"wait_for": marker, "timeout": secs}  # noqa: E731
    loaded = lambda url: f"{j.P}:LOAD:{url}"  # noqa: E731
    return [
        *boot_gates(timeout),
        wait(loaded(j.START)),
        # A page the first run fetched (max-age=604800): the HTTP cache on the profile serves it, and
        # a hit refreshes the record's modification time on the network process's background queue.
        *navigate(j.OLDNET_URL, loaded(j.OLDNET_URL), timeout),
        wait(f"{j.P}:TITLE:{j.OLDNET_TITLE}"),
        {"wait": 3.0},
        {"shot": "29_cached"},
        # The cookie of the first run rides on the request.
        *navigate(j.COOKIE_URL, loaded(j.COOKIE_URL), timeout),
        wait(f"{j.P}:TITLE:{j.COOKIE_KEPT}"),
        {"wait": 1.0},
        {"shot": "30_cookie_kept"},
        # The history of the first run, from the profile.
        *shortcut("h", loaded("about:history"), timeout),
        {"wait": 3.0},
        {"shot": "31_history_kept"},
        # Clear history: a click on a link of the browser's own page, the one command a person may give.
        page_click(j.caption("History"), CLEAR_AT, timeout, f"{j.P}:LOAD:about:history"),
        {"wait": 3.0},
        {"shot": "32_history_cleared"},
        {"click_at": {"target": f"title:{j.caption('History')}", "offset": list(CLOSE_AT_NORMAL)},
         "timeout": 60, "until": f"{j.P}:QUIT:", "retries": 2},
        {"wait": 9.0},
        {"quit": True},
    ]


def render(items: list[dict]) -> str:
    return json.dumps(items, indent=1) + "\n"


def examples() -> dict[Path, str]:
    """The checked-in sessions and what they render to."""
    import blocker
    return {EXAMPLE_FIRST: render(first()), EXAMPLE_SECOND: render(second()),
            EXAMPLE_BLOCKER: render(blocker.blocker_boot()),
            EXAMPLE_PERSISTED: render(blocker.persisted_boot())}


def main_check() -> int:
    """0 when the checked-in examples are what the sessions render, else 1."""
    for path, text in examples().items():
        if not path.is_file() or path.read_text(encoding="utf-8") != text:
            print(f"{path} is stale: run python tools/lazyweb/session.py --write", file=sys.stderr)
            return 1
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--write", action="store_true", help="write the checked-in examples")
    parser.add_argument("--check", action="store_true", help="fail when the checked-in examples are stale")
    args = parser.parse_args()
    wanted = examples()
    for path, text in wanted.items():
        if args.write:
            path.write_text(text, encoding="utf-8", newline="\n")
            print(f"wrote {path}")
        if args.check and (not path.is_file() or path.read_text(encoding="utf-8") != text):
            print(f"{path} is stale: run python tools/lazyweb/session.py --write", file=sys.stderr)
            return 1
    if not args.write and not args.check:
        print(wanted[EXAMPLE_FIRST], end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
