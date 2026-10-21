"""LazyWeb+'s notification session scripts (`notify_run.py`,
`tools/screenshot/examples/lazywebplus_notify*.json`).

The image is the one `run.py` builds: a desktop with `LAZYOS_LAZYWEBPLUS=1`, the UI probe and
`LAZYOS_XUI_AUTOSTART=lazywebplus`. Two boots of it (the profile in
`$HOME/.apps/os.lazy.lazywebplus` must be found again), both on
`https://theoldnet.com/webplus/notify.html` (`tools/webtest/fixtures/theoldnet.com/webplus/`):

* **allow** (`allow`): the page asks for the permission at load with no gesture and learns
  `denied`, with no bar; a click on *Ask* opens the bar; *Allow* is kept; *Post* shows a toast
  titled with the site and, 1.5 s later, the same tag updates it in place; a click on the toast
  runs the page's `click` handler; a burst of six is cut short; the page goes away and a click on
  a toast brings it back;
* **kept** (`kept`): the permission is found again, so *Post* needs no bar; the other origin
  (`www.theoldnet.com`) asks and is blocked, and its post shows nothing; `about:lazyweb` lists both
  decisions and *Forget* removes the blocked one.

Clicks on the page are at page coordinates through the window's `page_origin` probe control; the
bar's buttons and the toast by their probe names. Regenerate the checked-in examples with
`python tools/webplus/notify_session.py --write`.
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
import notify_judge as n  # noqa: E402
import session as s  # noqa: E402

EXAMPLES = ROOT / "tools" / "screenshot" / "examples"
EXAMPLE_ALLOW = EXAMPLES / "lazywebplus_notify.json"
EXAMPLE_KEPT = EXAMPLES / "lazywebplus_notify_kept.json"

#: The page's three boxes, in page pixels: 360 x 70, a row every 90 px
#: (`fixtures/theoldnet.com/webplus/notify.html`).
BOX = {"ask": (180, 35), "post": (180, 125), "burst": (180, 215), "away": (180, 305), "later": (180, 395)}
#: The window's minimize button, from the middle of its title bar (the maximize button is at 472).
MINIMIZE_AT = (454, 0)
#: The first row's *Forget* button on `about:lazyweb`, in page pixels (measured from a screenshot; the newest decision is the
#: first row; the page's table puts the button at the right end of its row).
FORGET_AT = (839, 317)


def caption(title: str) -> str:
    return j.caption(title)


def allow(timeout: float = 300.0) -> list[dict]:
    wait = lambda marker, secs=timeout, **more: {"wait_for": marker, "timeout": secs, **more}  # noqa: E731
    loaded = lambda url: f"{j.P}:LOAD:{url}"  # noqa: E731
    titled = lambda title: f"{j.P}:TITLE:{title}"  # noqa: E731
    note = lambda kind, rest="": f"{j.P}:NOTIFY:{kind}:{rest}"  # noqa: E731
    return [
        *s.boot_gates(timeout),
        wait(loaded(j.START)),
        *s.navigate(n.PAGE, loaded(n.PAGE), timeout),
        # No gesture: the request is refused outright and no bar opens.
        wait(titled(n.READY_DEFAULT)),
        {"wait": 2.0},
        {"shot": "n00_loaded"},
        # A click asks: the browser's own bar, not a toast.
        s.page_click(caption(n.READY_DEFAULT), BOX["ask"], timeout, note("ASK", n.ORIGIN)),
        {"wait": 1.5},
        {"shot": "n01_bar"},
        {"click_at": {"window": caption(n.READY_DEFAULT), "widget": "notify_allow"}, "timeout": timeout,
         "until": note("ALLOW", n.ORIGIN), "retries": 2},
        wait(titled(n.PERM_GRANTED)),
        {"wait": 1.0},
        # A toast, titled with the site; the same tag then updates it in place.
        s.page_click(caption(n.PERM_GRANTED), BOX["post"], timeout, note("SHOW", n.ORIGIN + ":")),
        {"wait": 0.8},
        {"shot": "n02_toast"},
        wait(note("SHOW", n.ORIGIN + ":"), occurrence=2),
        {"wait": 0.6},
        {"shot": "n03_updated"},
        # A click on the toast runs the page's click handler.
        {"click_at": {"target": "toast:body"}, "timeout": timeout, "until": titled(n.CLICKED_SECOND),
         "retries": 2},
        {"wait": 1.0},
        {"shot": "n04_clicked"},
        # The page posts and goes away in one click; a click on its toast brings the page back.
        s.page_click(caption(n.CLICKED_SECOND), BOX["away"], timeout, loaded(j.LINKS_URL)),
        wait(f"{j.P}:TITLE:{j.LINKS_TITLE}"),
        {"shot": "n05_away"},
        {"click_at": {"target": "toast:body"}, "timeout": timeout, "until": note("CLICK", ""),
         "occurrence": 2, "retries": 2},
        wait(loaded(n.PAGE), occurrence=2),
        {"wait": 1.5},
        {"shot": "n06_back"},
        # A post that comes up while the window is minimized: the click brings the window back
        # (the shell raises the app's windows) and runs the page's handler.
        s.page_click(caption(n.READY_GRANTED), BOX["later"], timeout),
        {"click_at": {"target": f"title:{caption(n.READY_GRANTED)}", "offset": list(MINIMIZE_AT)},
         "timeout": 60, "until": "XUID:WM:MINIMIZE", "retries": 2},
        wait(note("SHOW", n.ORIGIN + ":"), occurrence=4),
        {"wait": 0.8},
        {"shot": "n07_minimized"},
        {"click_at": {"target": "toast:body"}, "timeout": timeout, "until": "XUID:WM:UNMINIMIZE",
         "retries": 2},
        wait(titled(n.CLICKED_LATER)),
        {"wait": 1.0},
        {"shot": "n08_raised"},
        # A burst: a few toasts, the rest dropped.
        s.page_click(caption(n.CLICKED_LATER), BOX["burst"], timeout, note("DROP", n.ORIGIN)),
        {"wait": 0.5},
        {"shot": "n09_burst"},
        {"click_at": {"target": f"title:{caption(n.CLICKED_LATER)}", "offset": list(s.CLOSE_AT_NORMAL)},
         "timeout": 60, "until": f"{j.P}:QUIT:", "retries": 2},
        # The profile is written by a flusher that runs every 5 s: wait it out before the stop.
        {"wait": 9.0},
        {"quit": True},
    ]


def kept(timeout: float = 300.0) -> list[dict]:
    wait = lambda marker, secs=timeout, **more: {"wait_for": marker, "timeout": secs, **more}  # noqa: E731
    loaded = lambda url: f"{j.P}:LOAD:{url}"  # noqa: E731
    titled = lambda title: f"{j.P}:TITLE:{title}"  # noqa: E731
    note = lambda kind, rest="": f"{j.P}:NOTIFY:{kind}:{rest}"  # noqa: E731
    return [
        *s.boot_gates(timeout),
        wait(loaded(j.START)),
        *s.navigate(n.PAGE, loaded(n.PAGE), timeout),
        # The permission of the first boot, from the profile: no bar for a post.
        wait(titled(n.READY_GRANTED)),
        {"wait": 1.0},
        s.page_click(caption(n.READY_GRANTED), BOX["post"], timeout, note("SHOW", n.ORIGIN + ":")),
        {"wait": 0.8},
        {"shot": "k00_post_without_asking"},
        wait(note("SHOW", n.ORIGIN + ":"), occurrence=2),
        # Another origin of the same host name: its own decision, asked, and blocked.
        *s.navigate(n.WWW_PAGE, loaded(n.WWW_PAGE), timeout),
        wait(titled(n.READY_DEFAULT)),
        {"wait": 1.0},
        s.page_click(caption(n.READY_DEFAULT), BOX["ask"], timeout, note("ASK", n.WWW_ORIGIN)),
        {"wait": 1.0},
        {"shot": "k01_bar"},
        {"click_at": {"window": caption(n.READY_DEFAULT), "widget": "notify_block"}, "timeout": timeout,
         "until": note("BLOCK", n.WWW_ORIGIN), "retries": 2},
        wait(titled(n.PERM_DENIED)),
        {"wait": 1.0},
        # Posting is refused by the engine itself: nothing is shown.
        s.page_click(caption(n.PERM_DENIED), BOX["post"], timeout),
        {"wait": 3.0},
        {"shot": "k02_blocked"},
        # The decisions are listed in about:lazyweb, and a Forget removes one.
        *s.navigate("about:lazyweb", loaded("about:lazyweb"), timeout),
        {"wait": 2.0},
        {"shot": "k03_about"},
        s.page_click(caption("About LazyWeb+"), FORGET_AT, timeout, note("FORGET", n.WWW_ORIGIN)),
        {"wait": 2.0},
        {"shot": "k04_forgotten"},
        {"click_at": {"target": f"title:{caption('About LazyWeb+')}", "offset": list(s.CLOSE_AT_NORMAL)},
         "timeout": 60, "until": f"{j.P}:QUIT:", "retries": 2},
        {"wait": 9.0},
        {"quit": True},
    ]


def render(items: list[dict]) -> str:
    return json.dumps(items, indent=1) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--write", action="store_true", help="write the checked-in examples")
    parser.add_argument("--check", action="store_true", help="fail when the checked-in examples are stale")
    args = parser.parse_args()
    wanted = {EXAMPLE_ALLOW: render(allow()), EXAMPLE_KEPT: render(kept())}
    for path, text in wanted.items():
        if args.write:
            path.write_text(text, encoding="utf-8", newline="\n")
            print(f"wrote {path}")
        if args.check and (not path.is_file() or path.read_text(encoding="utf-8") != text):
            print(f"{path} is stale: run python tools/webplus/notify_session.py --write", file=sys.stderr)
            return 1
    if not args.write and not args.check:
        print(wanted[EXAMPLE_ALLOW], end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
