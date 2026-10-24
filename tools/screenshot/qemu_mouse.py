"""QMP mouse events: relative moves split into PS/2 packets, buttons, the wheel, absolute moves
(`qemu_qmp.Qmp` sends them; the names are re-exported from `qemu_qmp`)."""

from __future__ import annotations

_BUTTONS = {"left", "middle", "right", "side", "extra", "wheel-up", "wheel-down"}
# The PS/2 button bitmask the monitor's `mouse_button` command takes.
# HMP `mouse_button` state bits, QEMU's MOUSE_EVENT_* values: 1 = left,
# 2 = right, 4 = middle.
_BUTTON_MASKS = {"left": 1, "right": 2, "middle": 4}


# The most a PS/2 packet moves the pointer along one axis.
PS2_MOUSE_STEP = 127


def mouse_move_steps(dx: int, dy: int) -> list[tuple[int, int]]:
    """Split a relative move into steps of at most one PS/2 packet each."""
    dx, dy = int(dx), int(dy)
    steps: list[tuple[int, int]] = []
    while dx or dy:
        step_x = max(-PS2_MOUSE_STEP, min(PS2_MOUSE_STEP, dx))
        step_y = max(-PS2_MOUSE_STEP, min(PS2_MOUSE_STEP, dy))
        steps.append((step_x, step_y))
        dx -= step_x
        dy -= step_y
    return steps


def mouse_move_events(dx: int, dy: int) -> list[dict]:
    events: list[dict] = []
    if dx:
        events.append({"type": "rel", "data": {"axis": "x", "value": int(dx)}})
    if dy:
        events.append({"type": "rel", "data": {"axis": "y", "value": int(dy)}})
    return events


def mouse_button_events(button: str = "left", down: bool = True) -> list[dict]:
    """One button transition, so a caller can hold a drag across moves."""
    if button not in _BUTTONS:
        raise ValueError(f"unknown mouse button {button!r}")
    return [{"type": "btn", "data": {"button": button, "down": bool(down)}}]


def mouse_click_events(button: str = "left") -> list[dict]:
    return mouse_button_events(button, True) + mouse_button_events(button, False)


def mouse_scroll_events(amount: int) -> list[dict]:
    button = "wheel-up" if amount > 0 else "wheel-down"
    events: list[dict] = []
    for _ in range(abs(int(amount))):
        events.append({"type": "btn", "data": {"button": button, "down": True}})
        events.append({"type": "btn", "data": {"button": button, "down": False}})
    return events


def mouse_abs_events(x: int, y: int) -> list[dict]:
    return [
        {"type": "abs", "data": {"axis": "x", "value": int(x)}},
        {"type": "abs", "data": {"axis": "y", "value": int(y)}},
    ]
