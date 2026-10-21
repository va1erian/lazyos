"""What the Mail harness judges about the reading pane, a light view of webd (docs/mail.md, "Verification"):
pure functions over the serial log, the listener's record and the screenshots, each returning a list of
problems (empty: it passed). `test_judge.py` shows each one failing when it should.

Two messages are opened in the session: esMail's fixture (HTML with an inline `cid:` picture and an
attachment) and `hostile.py`'s message (a page that tries every way to reach the network, and a blue heading).
"""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "screenshot"))
from pngstats import decode_png  # noqa: E402

#: Pixels of the heading's colour (`hostile.HEADING_COLOUR`, `#0000ff`): the heading's text is large, so a
#: painted message has thousands of them; a selection highlight or a link colour is not this blue.
MIN_HEADING_PIXELS = 150


def pane_pixels(shot: Path, box: tuple[int, int, int, int]) -> tuple[bytes, int]:
    """The pixels of `box` (x0, x1, y0, y1) of the PNG, and its channel count."""
    width, _height, channels, pixels = decode_png(shot)
    x0, x1, y0, y1 = box
    rows = b"".join(pixels[(y * width + x0) * channels:(y * width + x1) * channels] for y in range(y0, y1))
    return rows, channels


def colours(pixels: bytes, channels: int) -> int:
    """How many different colours there are."""
    return len({pixels[i:i + channels] for i in range(0, len(pixels), channels)})


def blue_pixels(pixels: bytes, channels: int) -> int:
    """Pixels that are clearly the hostile heading's blue (not a selection or a link)."""
    count = 0
    for i in range(0, len(pixels), channels):
        r, g, b = pixels[i], pixels[i + 1], pixels[i + 2]
        if b > 200 and r < 80 and g < 80:
            count += 1
    return count


def judge_message_shot(shot: Path, box: tuple[int, int, int, int], what: str) -> list[str]:
    """The pane shows a page: text and a header block make many colours, not one."""
    pixels, channels = pane_pixels(shot, box)
    found = colours(pixels, channels)
    if found < 12:
        return [f"{what}: the reading pane is nearly blank ({found} colours)"]
    return []


def judge_hostile_shot(shot: Path, box: tuple[int, int, int, int]) -> list[str]:
    """The hostile message painted its heading (the message itself shows) and the rest of it did nothing."""
    pixels, channels = pane_pixels(shot, box)
    found = blue_pixels(pixels, channels)
    if found < MIN_HEADING_PIXELS:
        return [f"hostile message: the blue heading did not paint ({found} blue pixels, wanted "
                f"{MIN_HEADING_PIXELS}): the message was not shown or was shown without its styles"]
    return []


def judge_requests(webd_judge, record: dict) -> list[str]:
    """No request, and no bare connection, reached the harness's listener except the guest's own control
    request (`webd_judge.judge_requests`: with `control`, a control that never arrived fails the run, since an
    empty record would then prove nothing)."""
    return webd_judge.judge_requests(record, control=True)


def judge_engine(text: str) -> list[str]:
    """webd served the reading pane: it created a view for Mail, nothing failed in the engine, and Mail never
    reported the engine missing."""
    problems = []
    if "WEBD:UP:PASS" not in text:
        problems.append("webd never reported WEBD:UP:PASS (the image has no webd, or it did not start)")
    if "WEBD:VIEW:CREATE client=" not in text:
        problems.append("webd made no view for Mail")
    for line in text.splitlines():
        if line.startswith("WEBD:") and ":FAIL" in line and not line.startswith("WEBD:NOTE:"):
            problems.append(f"webd failure: {line[:160]}")
    return problems[:8]
