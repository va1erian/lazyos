"""Judges for the Docs sessions (docs/xui-docs.md): what the serial log and the screenshots must show when Docs
draws its pages in a light view of webd. Pure functions over text and decoded pictures, so
`test_judge.py` can check that each one fails when it should.

Both sessions run on an image with webd (`LAZYOS_WEBD=1`): without it the window says the web engine is not
running and the page never paints, which `judge_markers` reports as a missing `DOCS:RENDER:PASS`.
"""

from __future__ import annotations

import importlib.util
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "screenshot"))
from pngstats import decode_png  # noqa: E402


def _webd_judge():
    """`tools/webd/judge.py` (webd's own label check, with the documented optional refusals of its children),
    loaded under another name: `tools/webd` has other files of the names this directory uses."""
    spec = importlib.util.spec_from_file_location("webd_judge", ROOT / "tools" / "webd" / "judge.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


webd_judge = _webd_judge()

#: The page area of the 900x640 window, below the window frame (x0, x1, y0, y1): everything the wheel
#: moves. The clock may tick in the taskbar, so only this box is compared.
CONTENT = (52, 948, 52, 688)
#: The page below the toolbar, for comparing the welcome page with an opened document.
VIEWPORT = (52, 948, 90, 688)

#: What the wheel session must print, in any order.
WHEEL_MARKERS = ["DOCS:UP:PASS", "DOCS:RENDER:PASS", "WEBD:UP:PASS", "WEBD:VIEW:CREATE client="]
#: What the Open session must print.
OPEN_MARKERS = WHEEL_MARKERS + [
    "DOCS:OPEN:PASS:/system/share/samples/testdoc.md",
    "DOCS:RENDER:PASS:/system/share/samples/testdoc.md",
]

FAILURE = re.compile(r"^(?:DOCS|WEBD):(?!NOTE:)[A-Z_]+(?::[A-Z_]+)*:FAIL.*$", re.M)
DOCS_DENY = re.compile(r"^LABEL:DENY label=app:os\.lazy\.docs(?:/\S+)?\s.*$", re.M)


def judge_markers(text: str, wanted: list[str]) -> list[str]:
    """Every wanted marker is in the log, and nothing failed."""
    problems = []
    for marker in wanted:
        # `DOCS:RENDER:PASS` is the start of `DOCS:RENDER:PASS:<path>`: the welcome page's own line is the whole line.
        exact = marker == "DOCS:RENDER:PASS"
        found = re.search(r"^DOCS:RENDER:PASS\s*$", text, re.M) if exact else marker in text
        if not found:
            problems.append(f"{marker} never appeared")
    problems += [f"failure marker: {line}" for line in FAILURE.findall(text)[:8]]
    if "DOCS:ENGINE:FAIL" in text:
        problems.append("Docs reported the web engine missing or failing")
    return problems


def judge_denies(text: str) -> list[str]:
    """No refused call for Docs, and none for webd or its children beyond the documented optional ones (time
    zone and fontconfig lookups: `tools/webd/judge.py`). The run is built with `LAZYOS_LABEL_TRACE=1`."""
    return ([f"refused: {line}" for line in DOCS_DENY.findall(text)[:12]]
            + webd_judge.judge_denies(text))


def region(path: Path, box: tuple[int, int, int, int]) -> tuple[bytes, int]:
    """The pixels of `box` of the PNG at `path`, and the channel count."""
    width, _height, channels, pixels = decode_png(path)
    x0, x1, y0, y1 = box
    rows = b"".join(pixels[(y * width + x0) * channels:(y * width + x1) * channels] for y in range(y0, y1))
    return rows, channels


def colours(pixels: bytes, channels: int) -> int:
    """How many different colours the pixels hold."""
    return len({pixels[i:i + channels] for i in range(0, len(pixels), channels)})


def judge_wheel(top: Path, down: Path, back: Path) -> list[str]:
    """The wheel moved the page down and the opposite turn returned it exactly, and the page has text."""
    a, channels = region(top, CONTENT)
    b, _ = region(down, CONTENT)
    c, _ = region(back, CONTENT)
    problems = []
    if colours(a, channels) < 8:
        problems.append(f"the page at the top is nearly blank ({colours(a, channels)} colours)")
    if b == a:
        problems.append("the mouse wheel did not scroll the page down")
    if c != a:
        problems.append("wheel-up did not return the page to where it started")
    return problems


def judge_opened(welcome: Path, opened: Path) -> list[str]:
    """Opening the test document changed the page and painted more than a handful of colours."""
    a, channels = region(welcome, VIEWPORT)
    b, _ = region(opened, VIEWPORT)
    problems = []
    if b == a:
        problems.append("opening the test document did not change the page viewport")
    if colours(b, channels) < 8:
        problems.append(f"the opened document's viewport is nearly blank ({colours(b, channels)} colours)")
    return problems


def judge_scrolled(opened: Path, scrolled: Path) -> list[str]:
    """The wheel scrolled the opened document."""
    a, _ = region(opened, VIEWPORT)
    b, _ = region(scrolled, VIEWPORT)
    return [] if a != b else ["the wheel did not scroll the opened document"]
