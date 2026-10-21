#!/usr/bin/env python3
"""Guest check of xui scroll views, driven through the Playground's doc pane.

Four defects of `xui-app`'s backend and the doc pane that no host test of a widget sees, because they
live in how the backend paints and routes pointer events (`xui-app/src/backend/render.rs`,
`pointer.rs`, `wheel.rs`, `coalesce.rs`) and in how fast the page answers a scroll step:

* a node that sticks out of its scroll view (a paragraph scrolled half out of the viewport) was
  painted whole, over the neighbouring controls: the page's text overpainted the button rows above
  and below the pane;
* the second press of a quick pair on the same widget reached it only as `MouseDoubleClick`, which
  `xui-core`'s scroll bar (and its buttons) ignore, so a thumb grabbed right after a click did not
  move;
* the wheel over a label or a paragraph in a scroll view went to that label, which reads no wheel,
  so the page often did not scroll (`backend/wheel.rs` sends it to the scroll view above);
* a thumb drag showed nothing until the button came up: every pointer move re-laid the whole page
  out (35 to 60 ms on a big interface page), the input queue grew faster than it drained, and the
  frame was only presented once the queue was empty (`backend/coalesce.rs` merges a batch's moves,
  `playground/docbody.rs` makes a scroll step a move).

The session (`tools/screenshot/examples/playground_scroll.json`, generated here) opens the
`os.lazy.files.v1` page (long enough to scroll), drags the thumb, clicks it and drags again at once,
and drags again after a pause. The judge reads the thumb's position in each screenshot and compares
the strips above and below the pane before and after scrolling.

    python tools/playground/scroll_run.py --write          # regenerate the session JSON
    python tools/playground/scroll_run.py --check          # fail when the checked-in JSON is stale
    python tools/playground/scroll_run.py                  # rebuild the image (fresh OS volume), boot, judge
    python tools/playground/scroll_run.py --no-build       # boot target/lazyos.img as it is, judge
    python tools/playground/scroll_run.py --out-dir DIR    # judge an earlier run's DIR (no boot)

The image is the one `python tools/playground/run.py` builds (probe on, autostart `playground`,
`LAZYOS_AUTOLOGIN=user`, `LAZYOS_RESET_OS=1`), at 1280x720. The judge measures the doc pane at its
default size, but the Playground stores a dragged layout on the OS volume
(`$HOME/.apps/os.lazy.playground/layout`; run.py's `layout` session leaves a taller doc pane there).
So the default is to rebuild with a reset volume (`run.build` forces the reset), which has no stored layout; `--no-build` reuses
the image and the judge then fails when the serial log shows `PLAYGROUND:LAYOUT:LOADED` (a layout
was loaded: rebuild). Exit status is non-zero on any failure.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "screenshot"))
import pngstats  # noqa: E402

sys.path.insert(0, str(HERE))
import run  # noqa: E402  (its `build` makes the image: reset volume, probe, autostart)

EXAMPLE = ROOT / "tools" / "screenshot" / "examples" / "playground_scroll.json"
SESSION = ROOT / "tools" / "screenshot" / "qemu_session.py"
WINDOW = "Messenger Playground (developer tool)"
#: The scroll bar's centre, from the doc pane's right edge, in pixels at 1x (12 wide).
BAR_FROM_RIGHT = 7
#: Where the press lands, below the doc pane's top (the thumb of the page starts there).
PRESS_BELOW_TOP = 45
#: Rows of the pane's frame, skipped when looking for the thumb.
FRAME = 4
#: Pixels a drag moves the pointer, in two steps.
DRAG = 20
SHOTS = ["scroll_idle", "scroll_drag", "scroll_click_drag", "scroll_pause_drag"]
#: The wheel and mid-drag shots, judged against the thumb of the page scrolled to the top.
WHEEL_SHOTS = ["scroll_top", "scroll_wheel_text", "scroll_wheel_bar"]
#: Taken with the button still held, and after it came up.
LIVE_SHOTS = ["scroll_top2", "scroll_middrag", "scroll_middrag2", "scroll_released"]
#: The least a wheel notch over the page moves the thumb, in pixels (a notch is 48 px of a page that
#: scrolls ~130, so the thumb moves ~25).
WHEEL_MIN = 8


def widget(name: str, **extra) -> dict:
    return {"click_at": {"window": WINDOW, "widget": name, **extra}}


def drag(shot: str) -> list[dict]:
    """Press, two paced moves down, release (a pause after the press: QEMU merges a down and an up
    that the guest has not read between, which is a harness artefact, not a human's drag)."""
    return [
        {"mouse_down": "left"}, {"wait": 0.05},
        {"mouse_move": [0, DRAG // 2]}, {"mouse_move": [0, DRAG // 2]},
        {"wait": 0.05}, {"mouse_up": "left"}, {"wait": 1.0}, {"shot": shot},
    ]


def to_top(shot: str) -> list[dict]:
    """Wheel the page back to the top (the pointer is over the page) and take `shot`."""
    return [{"mouse_scroll": 40}, {"wait": 0.8}, {"shot": shot}]


def steps(doc_width: int = 823, doc_height: int = 215) -> list[dict]:
    """The session. `doc_width`/`doc_height` are the doc pane's probe size at 1x."""
    # The bar's first 90 pixels (`doc_bar_top`): its centre is on the thumb of a page at the top.
    thumb = {"window": WINDOW, "widget": "doc_bar_top"}
    return [
        {"wait_for": "PKGD:PROVISION:DONE", "timeout": 420},
        {"wait_for": "PLAYGROUND:UP:PASS", "timeout": 240},
        {"wait_for": "name=tree window=Messenger Playground", "timeout": 90},
        {"wait": 1.0},
        widget("search"),
        {"key_down": "ctrl"}, {"key": "a"}, {"key_up": "ctrl"},
        {"type": "os.lazy.files"}, {"wait": 1.0},
        widget("tree_row:0"),
        {"wait": 2.0},
        {"move_to": thumb}, {"wait": 0.5},
        {"shot": SHOTS[0]},
        *drag(SHOTS[1]),
        # Press and release without moving, then press again at once and drag: the second press of
        # the pair used to arrive as a double-click only, and the thumb did not move.
        {"mouse_down": "left"}, {"wait": 0.05}, {"mouse_up": "left"}, {"wait": 0.1},
        *drag(SHOTS[2]),
        {"mouse_down": "left"}, {"wait": 0.05}, {"mouse_up": "left"}, {"wait": 0.8},
        *drag(SHOTS[3]),
        # The wheel over the page's text (a label to the backend, which reads no wheel), then over
        # the bar: the page scrolls both times.
        {"move_to": {"window": WINDOW, "widget": "doc", "offset": [-doc_width // 4, -doc_height // 4]}},
        {"wait": 0.3},
        *to_top(WHEEL_SHOTS[0]),
        {"mouse_scroll": -1}, {"wait": 0.8}, {"shot": WHEEL_SHOTS[1]},
        {"move_to": thumb}, {"wait": 0.3},
        {"mouse_scroll": -1}, {"wait": 0.8}, {"shot": WHEEL_SHOTS[2]},
        # The thumb follows the pointer while the button is held, and nothing jumps on release.
        {"move_to": {"window": WINDOW, "widget": "doc", "offset": [-doc_width // 4, -doc_height // 4]}},
        *to_top(LIVE_SHOTS[0]),
        {"move_to": thumb}, {"wait": 0.3},
        {"mouse_down": "left"}, {"wait": 0.2},
        {"mouse_move": [0, 15]}, {"wait": 0.8}, {"shot": LIVE_SHOTS[1]},
        {"mouse_move": [0, 15]}, {"wait": 0.8}, {"shot": LIVE_SHOTS[2]},
        {"mouse_up": "left"}, {"wait": 0.8}, {"shot": LIVE_SHOTS[3]},
        {"quit": True},
    ]


def rendered() -> str:
    return json.dumps(steps(), indent=1) + "\n"


# ---- judging ----------------------------------------------------------------------------------

Probe = dict[str, tuple[int, int, int, int]]


def probes(serial: str) -> Probe:
    """The newest `UI:RECT`/`UI:WIDGET` line of each name: name -> (x, y, w, h)."""
    out: Probe = {}
    for line in serial.splitlines():
        found = re.match(r"UI:(?:RECT|WIDGET) x=(-?\d+) y=(-?\d+) w=(\d+) h=(\d+) name=(.+?)(?: window=.*)?$", line)
        if found:
            x, y, w, h = (int(found.group(i)) for i in range(1, 5))
            out[found.group(5)] = (x, y, w, h)
    return out


def doc_rect(serial: str) -> tuple[int, int, int, int]:
    """The doc pane on the screen: the window's content origin plus the pane's probe rectangle."""
    p = probes(serial)
    window = p.get(f"window:{WINDOW}")
    doc = p.get("doc")
    if window is None or doc is None:
        raise ValueError("the serial log has no window or doc probe line (an image built without LAZYOS_UI_PROBE=1?)")
    return window[0] + doc[0], window[1] + doc[1], doc[2], doc[3]


class Shot:
    """A decoded screenshot with pixel access."""

    def __init__(self, path: Path):
        self.width, self.height, self.channels, self.pixels = pngstats.decode_png(path)

    def pixel(self, x: int, y: int) -> tuple[int, int, int]:
        at = (y * self.width + x) * self.channels
        return (self.pixels[at], self.pixels[at + 1], self.pixels[at + 2])

    def strip(self, left: int, top: int, right: int, bottom: int) -> bytes:
        rows = []
        for y in range(top, bottom):
            at = (y * self.width + left) * self.channels
            rows.append(self.pixels[at:at + (right - left) * self.channels])
        return b"".join(rows)


def near(a: tuple[int, int, int], b: tuple[int, int, int], slack: int = 8) -> bool:
    return all(abs(p - q) <= slack for p, q in zip(a, b))


def thumb_top(shot: Shot, x: int, top: int, bottom: int, colour: tuple[int, int, int]) -> int | None:
    """The first row of the scroll thumb in column `x` between `top` and `bottom`. The pane's frame is
    skipped (in the dark theme its colour is the thumb's); a thumb at the top reads as `top + FRAME`."""
    for y in range(top + FRAME, bottom):
        if near(shot.pixel(x, y), colour):
            return y
    return None


def judge(out: Path) -> list[str]:
    """Every problem found in a run's `serial.log` and screenshots (empty: pass)."""
    problems: list[str] = []
    summary = out / "summary.json"
    if not summary.is_file() or not json.loads(summary.read_text()).get("ok"):
        return [f"the session did not finish: see {summary}"]
    serial = (out / "serial.log").read_text(errors="replace")
    if "PLAYGROUND:LAYOUT:LOADED" in serial:
        return ["a stored layout was loaded (an earlier session left one on the volume): "
                "rebuild with a reset volume (run without --no-build)"]
    try:
        left, top, width, height = doc_rect(serial)
    except ValueError as error:
        return [str(error)]
    shots = {}
    for name in SHOTS:
        path = out / f"shot_{name}.png"
        if not path.is_file():
            return [f"missing screenshot {path.name}"]
        shots[name] = Shot(path)
    right, bottom = left + width, top + height
    x = right - BAR_FROM_RIGHT
    # The thumb's colour, from the first rows of the idle page (the page starts scrolled to the top).
    colour = shots[SHOTS[0]].pixel(x, top + 6)
    track = shots[SHOTS[0]].pixel(x, bottom - 6)
    if near(colour, track, 12):
        return ["no scroll thumb in the idle shot (the page no longer overflows the doc pane?)"]
    tops = [thumb_top(shots[name], x, top, bottom, colour) for name in SHOTS]
    if None in tops:
        return [f"the scroll thumb was not found in {SHOTS[tops.index(None)]} (column {x})"]
    labels = ["a plain drag", "a drag straight after a click (the second press of a quick pair)",
              "a drag after a pause"]
    for before, after, label in zip(tops, tops[1:], labels):
        if after - before < DRAG // 2:
            problems.append(f"{label} moved the thumb {after - before} px (from {before} to {after}), "
                            f"expected about {DRAG}")
    problems += judge_wheel_and_live(out, x, top, bottom, colour)
    # Nothing of the page may be painted outside the pane: the strips above (the button rows) and
    # below (the Call form row) are the same before and after scrolling.
    idle = shots[SHOTS[0]]
    for name in SHOTS[1:]:
        for label, (a, b) in (("above", (top - 24, top - 2)), ("below", (bottom + 2, bottom + 22))):
            if shots[name].strip(left + 4, a, right - 4, b) != idle.strip(left + 4, a, right - 4, b):
                problems.append(f"{name}: the strip {label} the doc pane changed: page text is painted outside it")
    return problems


def judge_wheel_and_live(out: Path, x: int, top: int, bottom: int, colour: tuple[int, int, int]) -> list[str]:
    """The wheel moved the thumb over the text and over the bar; a held drag moved it live."""
    shots = {}
    for name in WHEEL_SHOTS + LIVE_SHOTS:
        path = out / f"shot_{name}.png"
        if not path.is_file():
            return [f"missing screenshot {path.name}"]
        shots[name] = Shot(path)
    t = {name: thumb_top(shot, x, top, bottom, colour) for name, shot in shots.items()}
    missing = [name for name, at in t.items() if at is None]
    if missing:
        return [f"the scroll thumb was not found in {missing[0]} (column {x})"]
    problems = []
    if t["scroll_wheel_text"] - t["scroll_top"] < WHEEL_MIN:
        problems.append(f"one wheel notch over the page's text moved the thumb "
                        f"{t['scroll_wheel_text'] - t['scroll_top']} px: the wheel did not reach the scroll view")
    if t["scroll_wheel_bar"] - t["scroll_wheel_text"] < WHEEL_MIN:
        problems.append(f"one wheel notch over the scroll bar moved the thumb "
                        f"{t['scroll_wheel_bar'] - t['scroll_wheel_text']} px")
    if t["scroll_middrag"] - t["scroll_top2"] < 10:
        problems.append(f"the thumb moved {t['scroll_middrag'] - t['scroll_top2']} px while the button was held "
                        f"(from {t['scroll_top2']} to {t['scroll_middrag']}): the drag is not live")
    if t["scroll_middrag2"] - t["scroll_middrag"] < 10:
        problems.append(f"the thumb did not follow the second move while held "
                        f"({t['scroll_middrag']} to {t['scroll_middrag2']})")
    if abs(t["scroll_released"] - t["scroll_middrag2"]) > 4:
        problems.append(f"the thumb jumped on release: {t['scroll_middrag2']} -> {t['scroll_released']}")
    # The page moved with the thumb: the held shots differ from the one at the top.
    page = (left_of(x), top, x - 4, bottom)
    if shots["scroll_middrag"].strip(*page) == shots["scroll_top2"].strip(*page):
        problems.append("the page did not move while the thumb was held")
    if shots["scroll_released"].strip(*page) != shots["scroll_middrag2"].strip(*page):
        problems.append("the page changed when the button came up (it must already be where the thumb put it)")
    return problems


def left_of(x: int) -> int:
    """The page's left edge for a bar column `x`: a strip of the page's text, not the whole pane."""
    return x - 300


def boot(image: Path, out: Path, args: argparse.Namespace) -> int:
    EXAMPLE.write_text(rendered(), encoding="utf-8", newline="\n")
    command = [sys.executable, str(SESSION), "--image", str(image), "--out", str(out), "--script", str(EXAMPLE),
               "--timeout", str(args.timeout), "--accel", args.accel,
               "--fail-on", r"PLAYGROUND:(BIND|RUN):FAIL", "--fail-on", "LazyOS PANIC"]
    if args.qemu:
        command += ["--qemu", args.qemu]
    return subprocess.call(command, cwd=ROOT)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--write", action="store_true", help="regenerate the session JSON and exit")
    parser.add_argument("--check", action="store_true", help="fail when the checked-in session JSON is stale")
    parser.add_argument("--no-build", action="store_true", help="boot the image as it is (it may hold a stored layout)")
    parser.add_argument("--image", default=str(ROOT / "target" / "lazyos.img"))
    parser.add_argument("--out-dir", help="judge this earlier run's directory instead of booting")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--timeout", type=int, default=900)
    parser.add_argument("--qemu")
    args = parser.parse_args(argv)
    if args.write:
        EXAMPLE.write_text(rendered(), encoding="utf-8", newline="\n")
        print(f"wrote {EXAMPLE.relative_to(ROOT)}")
        return 0
    if args.check:
        current = EXAMPLE.read_text(encoding="utf-8") if EXAMPLE.is_file() else ""
        if current != rendered():
            print(f"{EXAMPLE.relative_to(ROOT)} is stale: run scroll_run.py --write", file=sys.stderr)
            return 1
        return 0
    out = Path(args.out_dir) if args.out_dir else ROOT / "shots" / "playground" / "scroll"
    if not args.out_dir:
        if not args.no_build and not run.build(args):
            print("PLAYGROUND:SCROLL:FAIL: the build failed", file=sys.stderr)
            return 1
        code = boot(Path(args.image), out, args)
        if code != 0:
            print(f"PLAYGROUND:SCROLL:FAIL: the session exited with {code}", file=sys.stderr)
            return 1
    problems = judge(out)
    for problem in problems:
        print(f"PLAYGROUND:SCROLL:FAIL: {problem}", file=sys.stderr)
    if not problems:
        print("PLAYGROUND:SCROLL:PASS")
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
