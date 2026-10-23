"""The browser monkey's actions (`webmonkey.py`): drawn from a seeded RNG, recorded, replayable.

An action is a small dict (`{"a": "scroll", "at": [x, y], "n": -3, "times": 12}`), so the sequence
that preceded a finding can be read, shortened and sent again (`--replay`). Pointer targets are page
pixels on the screen (the page area is read from the UI probe), so a replay on a layout that moved
still points into the page.
"""

from __future__ import annotations

import random
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "screenshot"))
import session_pointer  # noqa: E402

#: What a person does in a browser, by how often. Reading (scroll, idle) dominates, as it does in real use.
WEIGHTS = {
    "scroll": 26, "idle": 10, "click": 12, "link": 12, "navigate": 8, "keys": 8, "history": 7,
    "reload": 3, "stop": 2, "drag": 3, "wheel_burst": 5, "chrome": 4,
}
SCROLL_KEYS = ["space", "pagedown", "pageup", "down", "up", "end", "home", "down", "down"]
#: Ctrl+W would close the window and end the run; Ctrl+S opens a dialog nothing here answers.
CHROME_KEYS = [("ctrl", "h"), ("ctrl", "j"), ("ctrl", "d"), ("alt", "home"), ("ctrl", "l")]
#: Pages worth visiting when nothing better is known: heavy, long, script-driven, image-rich.
START_URLS = [
    "https://en.wikipedia.org/wiki/Special:Random", "https://news.ycombinator.com/", "https://arstechnica.com/",
    "https://www.theverge.com/", "https://old.reddit.com/", "https://lite.cnn.com/", "https://www.bbc.com/news",
    "https://github.com/trending", "https://www.nytimes.com/", "https://www.wired.com/",
    "https://developer.mozilla.org/en-US/docs/Web/HTML", "https://www.reddit.com/r/programming/",
    "https://stackoverflow.com/questions", "https://example.com/", "https://theoldnet.com/",
]


class Pool:
    """Addresses to visit: the seeds, plus every page the browser reached (a random walk over the web)."""

    def __init__(self, seeds: list[str]):
        self.seeds, self.seen = list(seeds), []

    def note(self, url: str) -> None:
        if url.startswith("http") and url not in self.seen:
            self.seen = (self.seen + [url])[-200:]

    def pick(self, rng: random.Random) -> str:
        pool = self.seeds + self.seen or START_URLS
        return rng.choice(pool)


def page_point(rng: random.Random, region: tuple[int, int, int, int]) -> list[int]:
    x, y, w, h = region
    return [x + rng.randint(w // 20, w - w // 20), y + rng.randint(h // 20, h - h // 20)]


def draw(rng: random.Random, region: tuple[int, int, int, int], pool: Pool) -> dict:
    kind = rng.choices(list(WEIGHTS), list(WEIGHTS.values()))[0]
    at = page_point(rng, region)
    if kind == "scroll":
        return {"a": kind, "at": at, "n": rng.choice([-10, -5, -3, -1, 1, 1, 2, 3, 5, 10]), "times": rng.randint(1, 25),
                "gap": rng.choice([0.0, 0.01, 0.03, 0.1])}
    if kind == "wheel_burst":  # a trackpad fling: hundreds of notches, direction flipping
        return {"a": kind, "at": at, "seed": rng.getrandbits(32), "count": rng.randint(50, 400)}
    if kind == "idle":
        return {"a": kind, "secs": rng.choice([0.5, 1, 2, 4, 8])}
    if kind == "click":
        return {"a": kind, "at": at, "button": rng.choice(["left"] * 6 + ["right"]), "double": rng.random() < 0.1}
    if kind == "link":  # keyboard link picking: focus the nth link from the top, follow it
        return {"a": kind, "tabs": rng.randint(1, 40), "back": rng.random() < 0.2, "follow": rng.random() < 0.85}
    if kind == "navigate":
        return {"a": kind, "url": pool.pick(rng)}
    if kind == "keys":
        return {"a": kind, "at": at, "keys": [rng.choice(SCROLL_KEYS) for _ in range(rng.randint(1, 12))]}
    if kind == "history":
        return {"a": kind, "dir": rng.choice(["left", "left", "right"]), "times": rng.randint(1, 3)}
    if kind == "drag":
        return {"a": kind, "from": at, "to": page_point(rng, region)}
    if kind == "chrome":
        return {"a": kind, "chord": list(rng.choice(CHROME_KEYS))}
    return {"a": kind}  # reload, stop


class Driver:
    """Sends actions over QMP. `region()` is the page area on the screen, read from the probe at the time."""

    def __init__(self, qmp, region):
        self.qmp, self.region = qmp, region

    def _point(self, at) -> None:
        session_pointer.move_pointer(self.qmp, (int(at[0]), int(at[1])))

    def _chord(self, keys: list[str], hold_after: float = 0.0) -> None:
        *mods, last = keys
        for mod in mods:
            self.qmp.key_down(mod)
        try:
            self.qmp.press_key(last)
        finally:
            for mod in reversed(mods):
                self.qmp.key_up(mod)

    def perform(self, act: dict) -> None:
        q, kind = self.qmp, act["a"]
        if kind == "scroll":
            self._point(act["at"])
            for _ in range(act["times"]):
                q.mouse_scroll(act["n"])
                time.sleep(act["gap"])
        elif kind == "wheel_burst":
            rng = random.Random(act["seed"])
            self._point(act["at"])
            direction = rng.choice([-1, 1])
            for _ in range(act["count"]):
                if rng.random() < 0.05:
                    direction = -direction
                q.mouse_scroll(direction * rng.randint(1, 4))
        elif kind == "idle":
            time.sleep(act["secs"])
        elif kind == "click":
            self._point(act["at"])
            q.mouse_click(act["button"])
            if act["double"]:
                q.mouse_click(act["button"])
            if act["button"] == "right":
                q.press_key("esc")  # a context menu would swallow the next input
        elif kind == "link":
            self._point(self.region()[:2])  # page focus first: a click on its top-left corner
            q.mouse_click("left")
            key = "tab"
            if act["back"]:
                q.key_down("shift")
            for _ in range(act["tabs"]):
                q.press_key(key)
            if act["back"]:
                q.key_up("shift")
            if act["follow"]:
                q.press_key("enter")
        elif kind == "navigate":
            self._chord(["ctrl", "l"])
            time.sleep(0.3)
            q.type_text(act["url"], delay=0.01)
            q.press_key("enter")
        elif kind == "keys":
            self._point(act["at"])
            for key in act["keys"]:
                q.press_key(key)
        elif kind == "history":
            for _ in range(act["times"]):
                self._chord(["alt", act["dir"]])
                time.sleep(0.2)
        elif kind == "reload":
            q.press_key("f5")
        elif kind == "stop":
            q.press_key("esc")
        elif kind == "drag":
            self._point(act["from"])
            q.mouse_down("left")
            try:
                self._point(act["to"])
            finally:
                q.mouse_up("left")
        elif kind == "chrome":
            self._chord(act["chord"])
            time.sleep(0.5)
            q.press_key("esc")
        else:
            raise ValueError(f"unknown action {act}")
