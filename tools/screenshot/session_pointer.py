"""Pointer targets for qemu_session.py: ``click_at`` and ``move_to`` (issue #538).

A step names where the pointer goes instead of replaying hand-measured
relative moves:

* ``[x, y]``: a screen pixel;
* ``"name"``: an entry of the ``--targets`` JSON file (``{"name": [x, y]}``),
  else a ``UI:RECT`` the guest printed under that name (``"taskbar:start"``);
* ``{"window": "MOD Player", "widget": "play_button"}``: a named control in a
  window, from the guest's ``UI:RECT name=window:<title>`` (``xuid``) and
  ``UI:WIDGET`` (the app) lines; ``{"window": "Terminal"}`` alone is the
  window's content;
* ``{"menu": "Accessories"}``: a start-menu or submenu row (``menu:<label>``);
* ``{"target": "<name>"}``: any ``UI:RECT`` name.

A named rectangle resolves to its centre, plus an optional ``"offset": [dx,
dy]``. The guest prints the ``UI:`` lines only in an image built with
``LAZYOS_UI_PROBE=1`` (``fhs::etc::UI_PROBE``); the newest line for a name
wins, and a step waits up to its ``timeout`` for the line to appear.

With ``--tablet`` the pointer jumps there (the tablet's 0..32767 axes, scaled
from ``--screen``); without it, it is slammed into the top-left corner and
moved there relatively (PS/2), the path the scripts used to spell by hand.
"""

from __future__ import annotations

import json
import re
import time
from pathlib import Path

TABLET_MAX = 32767
# Set by qemu_session.main(): the screen size, the --targets names, and
# whether a usb-tablet is attached.
POINTER: dict = {"screen": (1280, 720), "targets": {}, "tablet": False}

_RECT = re.compile(r"UI:RECT x=(-?\d+) y=(-?\d+) w=(\d+) h=(\d+) name=([^\r\n]*)")
_WIDGET = re.compile(r"UI:WIDGET x=(-?\d+) y=(-?\d+) w=(\d+) h=(\d+) name=(\S+) window=([^\r\n]*)")
_POLL_SECONDS = 0.25
# Relative moves that put a PS/2 pointer in the top-left corner from anywhere.
_CORNER_MOVES = 4


class StepFailed(Exception):
    """A readiness gate timed out, a ``--fail-on`` pattern appeared, or a
    pointer target could not be resolved."""


def parse_screen(text: str) -> tuple[int, int]:
    """``WxH`` as integers, each at least 2 (a pixel maps onto ``size - 1``)."""
    try:
        width, height = (int(n) for n in text.lower().split("x"))
    except ValueError:
        raise ValueError(f"--screen must be WxH, got {text!r}") from None
    if width < 2 or height < 2:
        raise ValueError(f"--screen must be at least 2x2, got {text!r}")
    return width, height


def load_targets(path: str) -> dict:
    """The ``--targets`` file: a JSON object of names (values checked on use)."""
    targets = json.loads(Path(path).read_text(encoding="utf-8"))
    if not isinstance(targets, dict):
        raise ValueError(f"--targets {path} must hold a JSON object of name: [x, y]")
    return targets


def pixel_pair(value, what: str) -> tuple[int, int]:
    """``value`` as an ``(x, y)`` pixel, or :class:`StepFailed`."""
    if (not isinstance(value, (list, tuple)) or len(value) != 2
            or any(isinstance(n, bool) or not isinstance(n, (int, float)) for n in value)):
        raise StepFailed(f"{what}: expected a pixel [x, y], got {value!r}")
    return int(value[0]), int(value[1])


def probe_rect(text: str, name: str) -> tuple[int, int, int, int] | None:
    """The newest ``UI:RECT`` named ``name`` in ``text``."""
    found = None
    for match in _RECT.finditer(text):
        if match.group(5).strip() == name:
            found = tuple(int(n) for n in match.groups()[:4])
    return found


def probe_widget(text: str, window: str, name: str) -> tuple[int, int, int, int] | None:
    """The newest ``UI:WIDGET`` ``name`` of ``window``, window-relative."""
    found = None
    for match in _WIDGET.finditer(text):
        if match.group(5) == name and match.group(6).strip() == window:
            found = tuple(int(n) for n in match.groups()[:4])
    return found


def _centre(rect: tuple[int, int, int, int], offset) -> tuple[int, int]:
    x, y, w, h = rect
    dx, dy = pixel_pair(offset, "click_at offset") if offset is not None else (0, 0)
    return x + w // 2 + dx, y + h // 2 + dy


def _named(target: dict, probe: str) -> tuple[int, int, int, int] | None:
    """The screen rectangle a dict target names, or ``None`` while unseen."""
    known = {"window", "widget", "menu", "target", "offset"}
    unknown = set(target) - known
    if unknown or sum(key in target for key in ("window", "menu", "target")) != 1:
        raise StepFailed(
            f"click_at: a named target has exactly one of window/menu/target "
            f"(plus widget, offset), got {target!r}")
    if "widget" in target and "window" not in target:
        raise StepFailed(f"click_at: a widget needs its window: {target!r}")
    if "menu" in target:
        return probe_rect(probe, f"menu:{target['menu']}")
    if "target" in target:
        return probe_rect(probe, str(target["target"]))
    window = probe_rect(probe, f"window:{target['window']}")
    if window is None or "widget" not in target:
        return window
    widget = probe_widget(probe, str(target["window"]), str(target["widget"]))
    if widget is None:
        return None
    return window[0] + widget[0], window[1] + widget[1], widget[2], widget[3]


def describe(target) -> str:
    return json.dumps(target) if not isinstance(target, str) else repr(target)


def resolve_pixel(target, targets: dict, probe: str = "") -> tuple[int, int] | None:
    """The screen pixel ``target`` names; ``None`` when it names a probe
    rectangle the guest has not printed yet. A malformed target raises."""
    if isinstance(target, str):
        if target in targets:
            return pixel_pair(targets[target], f"--targets {target!r}")
        rect = probe_rect(probe, target)
        return _centre(rect, None) if rect else None
    if isinstance(target, dict):
        rect = _named(target, probe)
        return _centre(rect, target.get("offset")) if rect else None
    return pixel_pair(target, "click_at")


def to_tablet(pixel: tuple[int, int], screen: tuple[int, int]) -> tuple[int, int]:
    """A screen pixel on the tablet's axes; off-screen is :class:`StepFailed`."""
    x, y = pixel
    width, height = screen
    if not (0 <= x < width and 0 <= y < height):
        raise StepFailed(f"click_at: ({x}, {y}) is outside the {width}x{height} screen")
    return x * TABLET_MAX // (width - 1), y * TABLET_MAX // (height - 1)


def resolve_click_at(target, screen: tuple[int, int], targets: dict,
                     probe: str = "") -> tuple[int, int]:
    """Tablet axis values (0..32767) for a target that resolves now."""
    pixel = resolve_pixel(target, targets, probe)
    if pixel is None:
        raise StepFailed(
            f"click_at: the guest printed no UI: line for {describe(target)} "
            "(an image built with LAZYOS_UI_PROBE=1 prints them; --targets names pixels)")
    return to_tablet(pixel, screen)


def wait_pixel(target, read_probe, timeout: float) -> tuple[int, int]:
    """Resolve ``target``, waiting up to ``timeout`` for its ``UI:`` line."""
    deadline = time.time() + timeout
    while True:
        pixel = resolve_pixel(target, POINTER["targets"], read_probe())
        if pixel is not None:
            to_tablet(pixel, POINTER["screen"])  # validates the bounds
            return pixel
        if time.time() >= deadline:
            resolve_click_at(target, POINTER["screen"], POINTER["targets"], read_probe())
        time.sleep(_POLL_SECONDS)


def move_pointer(qmp, pixel: tuple[int, int]) -> None:
    """Put the pointer on ``pixel``: absolutely with a tablet, else from the
    top-left corner with relative moves."""
    if POINTER["tablet"]:
        qmp.mouse_abs(*to_tablet(pixel, POINTER["screen"]))
    else:
        for _ in range(_CORNER_MOVES):
            qmp.mouse_move(-300, -300)
            time.sleep(0.2)
        qmp.mouse_move(*pixel)
    time.sleep(0.1)  # let the guest move its cursor before a button


def point(qmp, step: dict, action: str, read_probe, timeout: float) -> tuple[int, int]:
    """Run a ``click_at`` or ``move_to`` step; returns the pixel it went to."""
    pixel = wait_pixel(step[action], read_probe, timeout)
    move_pointer(qmp, pixel)
    if action == "click_at":
        qmp.mouse_click(step.get("button", "left"))
    return pixel
