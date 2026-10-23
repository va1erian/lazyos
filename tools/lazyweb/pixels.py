"""Pixel evidence of LazyWeb's harness (`judge.py`): the page area of a screenshot, the wheel's
scroll measured on a page of static bands, the maximized page painted to its right edge. Pure
functions over `pngstats`, with their own tests (`test_judge.py`)."""

from __future__ import annotations

import hashlib
import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parent.parent / "screenshot"))
import pngstats  # noqa: E402


def region_stats(path: Path, region: tuple[int, int, int, int]) -> dict:
    """`pngstats` numbers for a rectangle of a screenshot, against a white background."""
    width, height, channels, pixels = pngstats.decode_png(Path(path))
    x, y, w, h = region
    x0, y0, x1, y1 = max(x, 0), max(y, 0), min(x + w, width), min(y + h, height)
    if x1 <= x0 or y1 <= y0:
        raise ValueError(f"region {region} is outside the {width}x{height} screenshot")
    rows = [pixels[(row * width + x0) * channels:(row * width + x1) * channels] for row in range(y0, y1)]
    crop = b"".join(rows)
    stats = pngstats.analyse(x1 - x0, y1 - y0, channels, crop, bg=(255, 255, 255))
    stats["digest"] = hashlib.sha256(crop).hexdigest()
    return stats


def judge_maximized(shot: Path | None, region: tuple[int, int, int, int] | None, strip: int = 120,
                    min_ink: float = 0.5) -> list[str]:
    """The maximized window's page area is painted all the way to its right edge: the page's own
    background (not the window's white) fills the last `strip` pixels, which the page's old size did
    not reach."""
    if shot is None or region is None:
        return ["the maximized screenshot or its page area is missing"]
    x, y, w, h = region
    if w < 1100:
        return [f"the page area is {w} px wide after maximizing"]
    try:
        stats = region_stats(shot, (x + w - strip, y, strip, h))
    except (OSError, ValueError) as error:
        return [str(error)]
    if stats["nonbackground_ratio"] < min_ink:
        return [f"the right {strip} px of the maximized page are blank ({stats['nonbackground_ratio']} ink): "
                "the page was not painted at the new size"]
    return []


#: How far five wheel notches move the page, in pixels: WebKit's line step of 40 px per notch.
SCROLL_PX: int | None = 200
#: The left part of the page area that a scroll is measured on: the scroll bar's thumb moves with
#: the page and the pointer (drawn into the screenshot) rests further right.
SCROLL_COLUMNS = 300


def crop_rows(path: Path, region: tuple[int, int, int, int], columns: int = SCROLL_COLUMNS) -> list[bytes]:
    """The rows of the left `columns` pixels of a rectangle of a screenshot, each as bytes."""
    width, height, channels, pixels = pngstats.decode_png(Path(path))
    x, y, w, h = region
    x0, y0, x1, y1 = max(x, 0), max(y, 0), min(x + min(w, columns), width), min(y + h, height)
    if x1 <= x0 or y1 <= y0:
        raise ValueError(f"region {region} is outside the {width}x{height} screenshot")
    return [bytes(pixels[(row * width + x0) * channels:(row * width + x1) * channels]) for row in range(y0, y1)]


def scroll_offset(before: list[bytes], after: list[bytes], need: float = 0.9) -> int | None:
    """By how many rows the picture moved up between two crops of a page of static content: the shift
    with the most identical rows, when at least `need` of the compared rows match. A page of
    animations or one that did not move gives None (a shift of 0 is not a scroll). The pointer is drawn
    into the screenshot, so a few rows differ: hence 0.9, not all."""
    rows = min(len(before), len(after))
    if sum(1 for y in range(rows) if after[y] == before[y]) >= need * rows:
        return None  # the same picture: static bands look alike a row apart, so say so first
    best, best_same = None, 0
    for shift in range(1, rows - rows // 4):
        compared = rows - shift
        same = sum(1 for y in range(compared) if after[y] == before[y + shift])
        if same > best_same:
            best, best_same = shift, same
    if best is None or best_same < need * (rows - best):
        return None
    return best


def judge_scroll(shots: dict[str, Path | None], region: tuple[int, int, int, int] | None,
                 expected: int | None = None) -> tuple[list[str], int | None]:
    """The wheel moved a page of static bands by a whole number of rows (`expected`, when known): the
    problems, and the shift measured."""
    before, after = shots.get("before"), shots.get("after")
    if before is None or after is None or region is None:
        return ["the scroll shots or the page area are missing"], None
    try:
        shift = scroll_offset(crop_rows(before, region), crop_rows(after, region))
    except (OSError, ValueError) as error:
        return [str(error)], None
    if shift is None:
        return ["the page did not move as one picture after the wheel turned (no row shift fits)"], None
    if expected is not None and shift != expected:
        return [f"the wheel moved the page {shift} px, expected {expected}"], shift
    return [], shift
